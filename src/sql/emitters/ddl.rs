use crate::sql::backend::Backend;
use crate::sql::ir::SchemaIr;

/// Emit full SQL DDL (CREATE TABLE, enums, indexes, constraints) for a target backend.
pub fn emit_ddl<B: Backend>(schema: &SchemaIr, backend: &B) -> String {
    let mut out = String::new();

    // SQLite PRAGMA foreign keys helper reminder
    if backend.name() == "sqlite" {
        out.push_str("-- SQLite: enable foreign key support\n");
        out.push_str("PRAGMA foreign_keys = ON;\n\n");
    }

    // Postgres Enum type creations
    if backend.name() == "postgres" {
        for (enum_name, enum_def) in &schema.enums {
            let values = enum_def
                .values
                .iter()
                .map(|v| format!("'{}'", v.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("CREATE TYPE {enum_name} AS ENUM ({values});\n\n"));
        }
    }

    // CREATE TABLE statements
    for (table_name, table) in &schema.tables {
        out.push_str(&format!("CREATE TABLE {table_name} (\n"));
        let mut defs = Vec::new();

        // Check if single integer primary key with autoincrement in SQLite
        let sqlite_single_auto_pk = backend.name() == "sqlite"
            && table.primary_key.as_ref().map_or(false, |pk| {
                pk.columns.len() == 1
                    && table
                        .columns
                        .get(&pk.columns[0])
                        .map_or(false, |c| c.auto_increment && c.r#type.is_integer())
            });

        for (col_name, col) in &table.columns {
            let mut line = String::new();
            line.push_str(&format!("    {col_name} "));

            if sqlite_single_auto_pk && table.primary_key.as_ref().unwrap().columns[0] == *col_name
            {
                line.push_str("INTEGER PRIMARY KEY AUTOINCREMENT");
            } else {
                let sql_type = backend.sql_type(col, table_name, col_name);
                line.push_str(&sql_type);

                if !col.nullable && !sql_type.contains("PRIMARY KEY") {
                    line.push_str(" NOT NULL");
                }
            }

            if let Some(default) = &col.default {
                line.push_str(&format!(" DEFAULT {}", backend.render_default(default)));
            }

            // SQLite enum check constraint emulation
            if backend.name() == "sqlite" {
                if let Some(enum_name) = col.r#type.enum_name() {
                    if let Some(enum_def) = schema.enums.get(enum_name) {
                        let allowed = enum_def
                            .values
                            .iter()
                            .map(|v| format!("'{}'", v.replace('\'', "''")))
                            .collect::<Vec<_>>()
                            .join(", ");
                        line.push_str(&format!(" CHECK ({col_name} IN ({allowed}))"));
                    }
                }
            }

            defs.push(line);
        }

        // Primary key constraint (if not already handled inline in SQLite)
        if !sqlite_single_auto_pk {
            if let Some(pk) = &table.primary_key {
                let cols = pk.columns.join(", ");
                if let Some(name) = &pk.name {
                    defs.push(format!("    CONSTRAINT {name} PRIMARY KEY ({cols})"));
                } else {
                    defs.push(format!("    PRIMARY KEY ({cols})"));
                }
            }
        }

        // Unique constraints
        for unq in &table.unique_constraints {
            let cols = unq.columns.join(", ");
            if let Some(name) = &unq.name {
                defs.push(format!("    CONSTRAINT {name} UNIQUE ({cols})"));
            } else {
                defs.push(format!("    UNIQUE ({cols})"));
            }
        }

        // Foreign keys
        for fk in &table.foreign_keys {
            let local_cols = fk.columns.join(", ");
            let target_cols = fk.target_columns.join(", ");
            let target_table = &fk.target_table;
            let on_delete = fk.on_delete.as_sql();
            let on_update = fk.on_update.as_sql();

            let mut fk_def = format!(
                "    FOREIGN KEY ({local_cols}) REFERENCES {target_table} ({target_cols}) ON DELETE {on_delete} ON UPDATE {on_update}"
            );
            if let Some(name) = &fk.name {
                fk_def = format!("    CONSTRAINT {name}{}", &fk_def[3..]);
            }
            defs.push(fk_def);
        }

        // Check constraints
        for chk in &table.check_constraints {
            let expr = &chk.expression;
            if let Some(name) = &chk.name {
                defs.push(format!("    CONSTRAINT {name} CHECK ({expr})"));
            } else {
                defs.push(format!("    CHECK ({expr})"));
            }
        }

        out.push_str(&defs.join(",\n"));
        out.push_str("\n);\n\n");

        // Indexes
        for idx in &table.indexes {
            let unique_kw = if idx.unique { "UNIQUE " } else { "" };
            let idx_name = &idx.name;
            let cols = idx.columns.join(", ");
            out.push_str(&format!(
                "CREATE {unique_kw}INDEX {idx_name} ON {table_name} ({cols});\n"
            ));
        }
        if !table.indexes.is_empty() {
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::backend::{PostgresBackend, SqliteBackend};
    use crate::sql::ir::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_emit_ddl_sqlite() {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let mut name_col = ColumnIr::new(ColumnType::Text);
        name_col.max_length = Some(100);
        users_cols.insert("name".to_string(), name_col);

        let mut role_col = ColumnIr::new(ColumnType::Enum {
            name: "user_role".to_string(),
        });
        role_col.default = Some(DefaultValueIr::Literal(serde_json::Value::String(
            "viewer".to_string(),
        )));
        users_cols.insert("role".to_string(), role_col);

        tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                primary_key: Some(PrimaryKeyIr {
                    columns: vec!["id".to_string()],
                    name: None,
                }),
                foreign_keys: Vec::new(),
                unique_constraints: Vec::new(),
                indexes: vec![IndexIr {
                    name: "users_name_idx".to_string(),
                    columns: vec!["name".to_string()],
                    unique: false,
                }],
                check_constraints: Vec::new(),
                renamed_from: None,
                rust_name: None,
            },
        );

        let mut enums = BTreeMap::new();
        enums.insert(
            "user_role".to_string(),
            EnumIr {
                values: vec![
                    "admin".to_string(),
                    "editor".to_string(),
                    "viewer".to_string(),
                ],
                rust_name: None,
            },
        );

        let schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums,
            tables,
        };

        let ddl = emit_ddl(&schema, &SqliteBackend);
        assert!(ddl.contains("PRAGMA foreign_keys = ON;"));
        assert!(ddl.contains("id INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(ddl.contains("CHECK (role IN ('admin', 'editor', 'viewer'))"));
        assert!(ddl.contains("CREATE INDEX users_name_idx ON users (name);"));
    }

    #[test]
    fn test_emit_ddl_postgres() {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        tables.insert(
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

        let mut enums = BTreeMap::new();
        enums.insert(
            "user_role".to_string(),
            EnumIr {
                values: vec!["admin".to_string(), "viewer".to_string()],
                rust_name: None,
            },
        );

        let schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums,
            tables,
        };

        let ddl = emit_ddl(&schema, &PostgresBackend);
        assert!(ddl.contains("CREATE TYPE user_role AS ENUM ('admin', 'viewer');"));
        assert!(ddl.contains("id SERIAL NOT NULL"));
    }
}
