use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

use crate::sql::backend::Backend;
use crate::sql::ir::{ColumnIr, IndexIr, SchemaIr, TableIr};

/// Migration operation types discovered during schema diffing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaChange {
    CreateTable {
        name: String,
        table: TableIr,
    },
    DropTable {
        name: String,
        table: TableIr,
    },
    RenameTable {
        old_name: String,
        new_name: String,
    },
    AddColumn {
        table_name: String,
        column_name: String,
        column: ColumnIr,
    },
    DropColumn {
        table_name: String,
        column_name: String,
        column: ColumnIr,
    },
    RenameColumn {
        table_name: String,
        old_name: String,
        new_name: String,
    },
    AlterColumnType {
        table_name: String,
        column_name: String,
        old_column: ColumnIr,
        new_column: ColumnIr,
    },
    AlterColumnNullability {
        table_name: String,
        column_name: String,
        old_nullable: bool,
        new_nullable: bool,
        column: ColumnIr,
    },
    CreateIndex {
        table_name: String,
        index: IndexIr,
    },
    DropIndex {
        table_name: String,
        index: IndexIr,
    },
}

/// Errors occurring during migration diffing or plan generation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MigrationError {
    #[error(
        "ambiguous table change: table '{old_table}' removed and '{new_table}' added without explicit .renamed_from(\"{old_table}\") hint"
    )]
    AmbiguousTableRename {
        old_table: String,
        new_table: String,
    },

    #[error(
        "ambiguous column change in table '{table}': column '{old_col}' removed and '{new_col}' added without explicit .renamed_from(\"{old_col}\") hint"
    )]
    AmbiguousColumnRename {
        table: String,
        old_col: String,
        new_col: String,
    },

    #[error("unsupported migration operation for backend '{backend}': {message}")]
    BackendUnsupported { backend: String, message: String },
}

/// Diff an old snapshot schema against a new schema.
pub fn diff_schemas(
    old_schema: &SchemaIr,
    new_schema: &SchemaIr,
) -> Result<Vec<SchemaChange>, MigrationError> {
    let mut changes = Vec::new();

    let mut old_tables: BTreeMap<String, &TableIr> = old_schema
        .tables
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    let new_tables: BTreeMap<String, &TableIr> = new_schema
        .tables
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();

    // Check for explicit table renames via `renamed_from`
    for (new_name, new_table) in &new_tables {
        if let Some(old_name) = &new_table.renamed_from {
            if old_tables.contains_key(old_name) {
                changes.push(SchemaChange::RenameTable {
                    old_name: old_name.clone(),
                    new_name: new_name.clone(),
                });
                old_tables.remove(old_name);
            }
        }
    }

    // Check dropped tables
    for (old_name, old_table) in &old_tables {
        if !new_tables.contains_key(old_name) {
            changes.push(SchemaChange::DropTable {
                name: old_name.clone(),
                table: (*old_table).clone(),
            });
        }
    }

    // Check added tables
    for (new_name, new_table) in &new_tables {
        if !old_schema.tables.contains_key(new_name) && new_table.renamed_from.is_none() {
            changes.push(SchemaChange::CreateTable {
                name: new_name.clone(),
                table: (*new_table).clone(),
            });
        }
    }

    // Check existing tables for column and index changes
    for (table_name, new_table) in &new_schema.tables {
        let old_table_name = new_table.renamed_from.as_ref().unwrap_or(table_name);
        if let Some(old_table) = old_schema.tables.get(old_table_name) {
            let mut old_cols: BTreeMap<String, &ColumnIr> = old_table
                .columns
                .iter()
                .map(|(k, v)| (k.clone(), v))
                .collect();

            // Explicit column renames via `renamed_from`
            for (new_col_name, new_col) in &new_table.columns {
                if let Some(old_col_name) = &new_col.renamed_from {
                    if old_cols.contains_key(old_col_name) {
                        changes.push(SchemaChange::RenameColumn {
                            table_name: table_name.clone(),
                            old_name: old_col_name.clone(),
                            new_name: new_col_name.clone(),
                        });
                        old_cols.remove(old_col_name);
                    }
                }
            }

            // Dropped columns
            for (old_col_name, old_col) in &old_cols {
                if !new_table.columns.contains_key(old_col_name) {
                    changes.push(SchemaChange::DropColumn {
                        table_name: table_name.clone(),
                        column_name: old_col_name.clone(),
                        column: (*old_col).clone(),
                    });
                }
            }

            // Added / modified columns
            for (new_col_name, new_col) in &new_table.columns {
                let old_col_key = new_col.renamed_from.as_ref().unwrap_or(new_col_name);
                if let Some(old_col) = old_table.columns.get(old_col_key) {
                    if old_col.r#type != new_col.r#type {
                        changes.push(SchemaChange::AlterColumnType {
                            table_name: table_name.clone(),
                            column_name: new_col_name.clone(),
                            old_column: (*old_col).clone(),
                            new_column: (*new_col).clone(),
                        });
                    }

                    if old_col.nullable != new_col.nullable {
                        changes.push(SchemaChange::AlterColumnNullability {
                            table_name: table_name.clone(),
                            column_name: new_col_name.clone(),
                            old_nullable: old_col.nullable,
                            new_nullable: new_col.nullable,
                            column: (*new_col).clone(),
                        });
                    }
                } else if new_col.renamed_from.is_none() {
                    changes.push(SchemaChange::AddColumn {
                        table_name: table_name.clone(),
                        column_name: new_col_name.clone(),
                        column: (*new_col).clone(),
                    });
                }
            }

            // Indexes diff
            let old_idx_map: BTreeMap<&str, &IndexIr> = old_table
                .indexes
                .iter()
                .map(|idx| (idx.name.as_str(), idx))
                .collect();
            let new_idx_map: BTreeMap<&str, &IndexIr> = new_table
                .indexes
                .iter()
                .map(|idx| (idx.name.as_str(), idx))
                .collect();

            for (old_idx_name, old_idx) in &old_idx_map {
                if !new_idx_map.contains_key(old_idx_name) {
                    changes.push(SchemaChange::DropIndex {
                        table_name: table_name.clone(),
                        index: (*old_idx).clone(),
                    });
                }
            }

            for (new_idx_name, new_idx) in &new_idx_map {
                if !old_idx_map.contains_key(new_idx_name) {
                    changes.push(SchemaChange::CreateIndex {
                        table_name: table_name.clone(),
                        index: (*new_idx).clone(),
                    });
                }
            }
        }
    }

    Ok(changes)
}

/// Generated migration SQL containing up and down scripts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationSql {
    pub up_sql: String,
    pub down_sql: String,
}

/// Emit migration `up.sql` and `down.sql` scripts from changes for a backend.
pub fn emit_migration<B: Backend>(
    changes: &[SchemaChange],
    target_schema: &SchemaIr,
    backend: &B,
) -> Result<MigrationSql, MigrationError> {
    let mut up = String::new();
    let mut down = String::new();

    // Check if SQLite needs table rebuild for any alters
    let sqlite_table_rebuild_needed = backend.name() == "sqlite"
        && changes.iter().any(|c| {
            matches!(
                c,
                SchemaChange::AlterColumnType { .. } | SchemaChange::AlterColumnNullability { .. }
            )
        });

    if sqlite_table_rebuild_needed {
        // Collect all tables requiring rebuild
        let mut affected_tables = BTreeSet::new();
        for change in changes {
            match change {
                SchemaChange::AlterColumnType { table_name, .. }
                | SchemaChange::AlterColumnNullability { table_name, .. } => {
                    affected_tables.insert(table_name.clone());
                }
                _ => {}
            }
        }

        up.push_str("-- SQLite transactional table-rebuild sequence\n");
        up.push_str("PRAGMA foreign_keys = OFF;\nBEGIN TRANSACTION;\n\n");

        for table_name in &affected_tables {
            if let Some(target_table) = target_schema.tables.get(table_name) {
                let temp_name = format!("_{table_name}_new");
                let mut temp_schema = SchemaIr {
                    ir_version: target_schema.ir_version,
                    naming: target_schema.naming.clone(),
                    enums: target_schema.enums.clone(),
                    tables: BTreeMap::new(),
                };
                temp_schema
                    .tables
                    .insert(temp_name.clone(), target_table.clone());

                let create_temp_ddl = crate::sql::emitters::emit_ddl(&temp_schema, backend);
                up.push_str(&create_temp_ddl);

                let cols: Vec<String> = target_table.columns.keys().cloned().collect();
                let col_list = cols.join(", ");
                up.push_str(&format!(
                    "INSERT INTO {temp_name} ({col_list}) SELECT {col_list} FROM {table_name};\n"
                ));
                up.push_str(&format!("DROP TABLE {table_name};\n"));
                up.push_str(&format!(
                    "ALTER TABLE {temp_name} RENAME TO {table_name};\n\n"
                ));
            }
        }

        up.push_str("PRAGMA foreign_key_check;\nCOMMIT;\nPRAGMA foreign_keys = ON;\n");

        let mut down_rebuild = String::new();
        down_rebuild.push_str("-- SQLite transactional table-rebuild sequence (revert)\n");
        down_rebuild.push_str("PRAGMA foreign_keys = OFF;\nBEGIN TRANSACTION;\n\n");
        for table_name in &affected_tables {
            if let Some(target_table) = target_schema.tables.get(table_name) {
                let temp_name = format!("_{table_name}_old");
                let cols: Vec<String> = target_table.columns.keys().cloned().collect();
                let col_list = cols.join(", ");
                down_rebuild.push_str(&format!(
                    "INSERT INTO {temp_name} ({col_list}) SELECT {col_list} FROM {table_name};\n"
                ));
                down_rebuild.push_str(&format!("DROP TABLE {table_name};\n"));
                down_rebuild.push_str(&format!(
                    "ALTER TABLE {temp_name} RENAME TO {table_name};\n\n"
                ));
            }
        }
        down_rebuild.push_str("PRAGMA foreign_key_check;\nCOMMIT;\nPRAGMA foreign_keys = ON;\n");

        return Ok(MigrationSql {
            up_sql: up,
            down_sql: down_rebuild,
        });
    }

    // Standard DDL alterations
    for change in changes {
        match change {
            SchemaChange::CreateTable { name, table } => {
                let mut single_schema = SchemaIr {
                    ir_version: target_schema.ir_version,
                    naming: target_schema.naming.clone(),
                    enums: target_schema.enums.clone(),
                    tables: BTreeMap::new(),
                };
                single_schema.tables.insert(name.clone(), table.clone());

                up.push_str(&crate::sql::emitters::emit_ddl(&single_schema, backend));
                down.push_str(&format!("DROP TABLE {name};\n"));
            }
            SchemaChange::DropTable { name, table } => {
                up.push_str(&format!("DROP TABLE {name};\n"));
                let mut single_schema = SchemaIr {
                    ir_version: target_schema.ir_version,
                    naming: target_schema.naming.clone(),
                    enums: target_schema.enums.clone(),
                    tables: BTreeMap::new(),
                };
                single_schema.tables.insert(name.clone(), table.clone());
                down.push_str(&crate::sql::emitters::emit_ddl(&single_schema, backend));
            }
            SchemaChange::RenameTable { old_name, new_name } => {
                up.push_str(&format!("ALTER TABLE {old_name} RENAME TO {new_name};\n"));
                down.push_str(&format!("ALTER TABLE {new_name} RENAME TO {old_name};\n"));
            }
            SchemaChange::AddColumn {
                table_name,
                column_name,
                column,
            } => {
                let sql_type = backend.sql_type(column, table_name, column_name);
                let null_clause = if column.nullable { "" } else { " NOT NULL" };
                let default_clause = if let Some(d) = &column.default {
                    format!(" DEFAULT {}", backend.render_default(d))
                } else {
                    String::new()
                };

                up.push_str(&format!(
                    "ALTER TABLE {table_name} ADD COLUMN {column_name} {sql_type}{null_clause}{default_clause};\n"
                ));
                down.push_str(&format!(
                    "ALTER TABLE {table_name} DROP COLUMN {column_name};\n"
                ));
            }
            SchemaChange::DropColumn {
                table_name,
                column_name,
                column,
            } => {
                up.push_str(&format!(
                    "ALTER TABLE {table_name} DROP COLUMN {column_name};\n"
                ));
                let sql_type = backend.sql_type(column, table_name, column_name);
                down.push_str(&format!(
                    "ALTER TABLE {table_name} ADD COLUMN {column_name} {sql_type};\n"
                ));
            }
            SchemaChange::RenameColumn {
                table_name,
                old_name,
                new_name,
            } => {
                up.push_str(&format!(
                    "ALTER TABLE {table_name} RENAME COLUMN {old_name} TO {new_name};\n"
                ));
                down.push_str(&format!(
                    "ALTER TABLE {table_name} RENAME COLUMN {new_name} TO {old_name};\n"
                ));
            }
            SchemaChange::AlterColumnNullability {
                table_name,
                column_name,
                new_nullable,
                ..
            } => {
                if backend.name() == "postgres" {
                    if *new_nullable {
                        up.push_str(&format!(
                            "ALTER TABLE {table_name} ALTER COLUMN {column_name} DROP NOT NULL;\n"
                        ));
                        down.push_str(&format!(
                            "ALTER TABLE {table_name} ALTER COLUMN {column_name} SET NOT NULL;\n"
                        ));
                    } else {
                        up.push_str(&format!(
                            "ALTER TABLE {table_name} ALTER COLUMN {column_name} SET NOT NULL;\n"
                        ));
                        down.push_str(&format!(
                            "ALTER TABLE {table_name} ALTER COLUMN {column_name} DROP NOT NULL;\n"
                        ));
                    }
                }
            }
            SchemaChange::CreateIndex { table_name, index } => {
                let unique_kw = if index.unique { "UNIQUE " } else { "" };
                let idx_name = &index.name;
                let cols = index.columns.join(", ");
                up.push_str(&format!(
                    "CREATE {unique_kw}INDEX {idx_name} ON {table_name} ({cols});\n"
                ));
                down.push_str(&format!("DROP INDEX {idx_name};\n"));
            }
            SchemaChange::DropIndex { index, .. } => {
                let idx_name = &index.name;
                up.push_str(&format!("DROP INDEX {idx_name};\n"));
            }
            _ => {}
        }
    }

    Ok(MigrationSql {
        up_sql: up,
        down_sql: down,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::backend::{PostgresBackend, SqliteBackend};
    use crate::sql::ir::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_diff_and_migration_add_table() {
        let old_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: BTreeMap::new(),
        };

        let mut new_tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        new_tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: Vec::new(),
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        let new_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: new_tables,
        };

        let diff = diff_schemas(&old_schema, &new_schema).unwrap();
        assert_eq!(diff.len(), 1);
        assert!(matches!(diff[0], SchemaChange::CreateTable { .. }));

        let migration = emit_migration(&diff, &new_schema, &SqliteBackend).unwrap();
        assert!(migration.up_sql.contains("CREATE TABLE users"));
        assert!(migration.down_sql.contains("DROP TABLE users;"));
    }

    #[test]
    fn test_sqlite_table_rebuild_on_type_change() {
        let mut old_tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let mut age_col = ColumnIr::new(ColumnType::Integer);
        age_col.nullable = true;
        users_cols.insert("age".to_string(), age_col);

        old_tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: Vec::new(),
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );
        let old_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: old_tables,
        };

        let mut new_tables = BTreeMap::new();
        let mut users_cols_new = BTreeMap::new();
        let mut id_col_new = ColumnIr::new(ColumnType::Integer);
        id_col_new.auto_increment = true;
        users_cols_new.insert("id".to_string(), id_col_new);

        let mut age_col_new = ColumnIr::new(ColumnType::Bigint);
        age_col_new.nullable = true;
        users_cols_new.insert("age".to_string(), age_col_new);

        new_tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols_new,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: Vec::new(),
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );
        let new_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: new_tables,
        };

        let diff = diff_schemas(&old_schema, &new_schema).unwrap();
        let migration = emit_migration(&diff, &new_schema, &SqliteBackend).unwrap();

        assert!(migration.up_sql.contains("PRAGMA foreign_keys = OFF;"));
        assert!(migration.up_sql.contains("BEGIN TRANSACTION;"));
        assert!(migration.up_sql.contains("INSERT INTO _users_new"));
        assert!(migration.up_sql.contains("DROP TABLE users;"));
        assert!(
            migration
                .up_sql
                .contains("ALTER TABLE _users_new RENAME TO users;")
        );
        assert!(migration.up_sql.contains("PRAGMA foreign_key_check;"));
    }

    #[test]
    fn test_postgres_migration_alters_and_indexes() {
        let mut old_tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        users_cols.insert("id".to_string(), ColumnIr::new(ColumnType::Integer));
        let mut name_col = ColumnIr::new(ColumnType::Text);
        name_col.nullable = true;
        users_cols.insert("name".to_string(), name_col);

        old_tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: Vec::new(),
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        let old_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: old_tables,
        };

        let mut new_tables = BTreeMap::new();
        let mut users_cols_new = BTreeMap::new();
        users_cols_new.insert("id".to_string(), ColumnIr::new(ColumnType::Integer));
        let mut name_col_new = ColumnIr::new(ColumnType::Text);
        name_col_new.nullable = false; // changed nullable to false
        users_cols_new.insert("name".to_string(), name_col_new);
        users_cols_new.insert("email".to_string(), ColumnIr::new(ColumnType::Text)); // added column

        new_tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols_new,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: vec![IndexIr {
                    name: "users_email_idx".to_string(),
                    columns: vec!["email".to_string()],
                    unique: true,
                }],
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        let new_schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables: new_tables,
        };

        let diff = diff_schemas(&old_schema, &new_schema).unwrap();
        let migration = emit_migration(&diff, &new_schema, &PostgresBackend).unwrap();

        assert!(
            migration
                .up_sql
                .contains("ALTER TABLE users ALTER COLUMN name SET NOT NULL;")
        );
        assert!(
            migration
                .down_sql
                .contains("ALTER TABLE users ALTER COLUMN name DROP NOT NULL;")
        );

        assert!(
            migration
                .up_sql
                .contains("ALTER TABLE users ADD COLUMN email TEXT NOT NULL;")
        );
        assert!(
            migration
                .down_sql
                .contains("ALTER TABLE users DROP COLUMN email;")
        );

        assert!(
            migration
                .up_sql
                .contains("CREATE UNIQUE INDEX users_email_idx ON users (email);")
        );
        assert!(migration.down_sql.contains("DROP INDEX users_email_idx;"));
    }
}
