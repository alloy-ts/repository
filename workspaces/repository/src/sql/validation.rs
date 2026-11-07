use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

use crate::sql::backend::Backend;
use crate::sql::ir::{ColumnIr, ColumnType, DefaultValueIr, SchemaIr, TableIr};

/// Errors produced during schema IR validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("IR version mismatch: schema has {found}, expected {expected}")]
    VersionMismatch { expected: u32, found: u32 },

    #[error("table '{table}' has no columns defined")]
    EmptyTable { table: String },

    #[error("table '{table}': no primary key")]
    NoPrimaryKey { table: String },

    #[error("table '{table}': primary key column \"{column}\" missing")]
    PrimaryKeyColumnNotFound { table: String, column: String },

    #[error("{table}.{column}: primary key cannot be nullable")]
    PrimaryKeyCannotBeNullable { table: String, column: String },

    #[error("{table}.{column}: autoIncrement needs an integer/bigint primary key")]
    AutoIncrementNonInteger { table: String, column: String },

    #[error("{table}.{column}: maxLength only applies to text")]
    MaxLengthNonText { table: String, column: String },

    #[error("{table}.{column}: defaultNow() only applies to timestamps")]
    DefaultNowNonTimestamp { table: String, column: String },

    #[error("{table}.{column}: enum \"{enum_name}\" not declared in enums[]")]
    UndefinedEnum { table: String, column: String, enum_name: String },

    #[error("{table}.{column}: default {default_val} is not a member of enum {enum_name}")]
    InvalidEnumDefault {
        table: String,
        column: String,
        enum_name: String,
        default_val: String,
        allowed: Vec<String>,
    },

    #[error("{table}: foreign key targets missing table \"{target_table}\"")]
    ForeignKeyTargetTableNotFound { table: String, target_table: String },

    #[error("{table}: foreign key column count mismatch")]
    ForeignKeyColumnCountMismatch {
        table: String,
        col_count: usize,
        target_table: String,
        target_col_count: usize,
    },

    #[error("{table}: foreign key references missing column")]
    ForeignKeyLocalColumnNotFound { table: String, column: String },

    #[error("{table}: foreign key target '{target_table}.{target_column}' does not exist")]
    ForeignKeyTargetColumnNotFound { table: String, target_table: String, target_column: String },

    #[error("{table}: foreign key type mismatch for '{column}'")]
    ForeignKeyTypeMismatch { table: String, column: String, target_table: String, target_column: String },

    #[error("{table}: foreign key target {target_table}.{target_column} is not unique")]
    ForeignKeyTargetNotUnique { table: String, target_table: String, target_column: String },

    #[error("{table}: unique constraint references missing column \"{column}\"")]
    UniqueConstraintColumnNotFound { table: String, column: String },

    #[error("{table}: index '{index}' references missing column \"{column}\"")]
    IndexColumnNotFound { table: String, index: String, column: String },

    #[error("index name \"{index}\" is used twice (global in Postgres)")]
    DuplicateIndexName { index: String },

    #[error("backend '{backend}' capability violation: {message}")]
    BackendUnsupported { backend: String, message: String },
}

/// Validate a complete schema IR against logical integrity rules and target backend capabilities.
pub fn validate_schema<B: Backend>(schema: &SchemaIr, backend: &B) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if schema.ir_version != crate::sql::ir::CURRENT_IR_VERSION {
        errors.push(ValidationError::VersionMismatch {
            expected: crate::sql::ir::CURRENT_IR_VERSION,
            found: schema.ir_version,
        });
    }

    let mut global_index_names = BTreeSet::new();

    for (table_name, table) in &schema.tables {
        if table.columns.is_empty() {
            errors.push(ValidationError::EmptyTable {
                table: table_name.clone(),
            });
            continue;
        }

        let pk_cols = table
            .primary_key
            .as_ref()
            .map(|pk| pk.columns.clone())
            .unwrap_or_default();

        if pk_cols.is_empty() {
            errors.push(ValidationError::NoPrimaryKey {
                table: table_name.clone(),
            });
        }

        // Validate columns
        for (col_name, col) in &table.columns {
            let is_pk = pk_cols.contains(col_name);

            // Backend capability check
            if let Err(msg) = backend.supports_column_type(&col.r#type) {
                errors.push(ValidationError::BackendUnsupported {
                    backend: backend.name().to_string(),
                    message: format!("table '{table_name}' column '{col_name}': {msg}"),
                });
            }

            // Primary key nullability
            if is_pk && col.nullable {
                errors.push(ValidationError::PrimaryKeyCannotBeNullable {
                    table: table_name.clone(),
                    column: col_name.clone(),
                });
            }

            // auto_increment only valid on Integer / Bigint primary keys
            if col.auto_increment && !(is_pk && col.r#type.is_integer()) {
                errors.push(ValidationError::AutoIncrementNonInteger {
                    table: table_name.clone(),
                    column: col_name.clone(),
                });
            }

            // max_length only valid on Text
            if col.max_length.is_some() && !col.r#type.is_text() {
                errors.push(ValidationError::MaxLengthNonText {
                    table: table_name.clone(),
                    column: col_name.clone(),
                });
            }

            // defaultNow only valid on Timestamps
            if matches!(col.default, Some(DefaultValueIr::Now)) && !col.r#type.is_timestamp() {
                errors.push(ValidationError::DefaultNowNonTimestamp {
                    table: table_name.clone(),
                    column: col_name.clone(),
                });
            }

            // Enum reference check
            if let Some(enum_name) = col.r#type.enum_name() {
                if let Some(enum_def) = schema.enums.get(enum_name) {
                    if let Some(DefaultValueIr::Literal(serde_json::Value::String(s))) = &col.default {
                        if !enum_def.values.contains(s) {
                            errors.push(ValidationError::InvalidEnumDefault {
                                table: table_name.clone(),
                                column: col_name.clone(),
                                enum_name: enum_name.to_string(),
                                default_val: s.clone(),
                                allowed: enum_def.values.clone(),
                            });
                        }
                    }
                } else {
                    errors.push(ValidationError::UndefinedEnum {
                        table: table_name.clone(),
                        column: col_name.clone(),
                        enum_name: enum_name.to_string(),
                    });
                }
            }
        }

        // Primary key columns existence
        for pk_col in &pk_cols {
            if !table.columns.contains_key(pk_col) {
                errors.push(ValidationError::PrimaryKeyColumnNotFound {
                    table: table_name.clone(),
                    column: pk_col.clone(),
                });
            }
        }

        // Unique constraints validation
        for unq in &table.unique_constraints {
            for col in &unq.columns {
                if !table.columns.contains_key(col) {
                    errors.push(ValidationError::UniqueConstraintColumnNotFound {
                        table: table_name.clone(),
                        column: col.clone(),
                    });
                }
            }
        }

        // Indexes validation
        for idx in &table.indexes {
            if !global_index_names.insert(&idx.name) {
                errors.push(ValidationError::DuplicateIndexName {
                    index: idx.name.clone(),
                });
            }

            for col in &idx.columns {
                if !table.columns.contains_key(col) {
                    errors.push(ValidationError::IndexColumnNotFound {
                        table: table_name.clone(),
                        index: idx.name.clone(),
                        column: col.clone(),
                    });
                }
            }
        }

        // Foreign keys validation
        for fk in &table.foreign_keys {
            if fk.columns.len() != fk.target_columns.len() {
                errors.push(ValidationError::ForeignKeyColumnCountMismatch {
                    table: table_name.clone(),
                    col_count: fk.columns.len(),
                    target_table: fk.target_table.clone(),
                    target_col_count: fk.target_columns.len(),
                });
                continue;
            }

            let Some(target_table) = schema.tables.get(&fk.target_table) else {
                errors.push(ValidationError::ForeignKeyTargetTableNotFound {
                    table: table_name.clone(),
                    target_table: fk.target_table.clone(),
                });
                continue;
            };

            for (local_col_name, target_col_name) in fk.columns.iter().zip(&fk.target_columns) {
                let local_col = table.columns.get(local_col_name);
                let target_col = target_table.columns.get(target_col_name);

                match (local_col, target_col) {
                    (None, _) => {
                        errors.push(ValidationError::ForeignKeyLocalColumnNotFound {
                            table: table_name.clone(),
                            column: local_col_name.clone(),
                        });
                    }
                    (_, None) => {
                        errors.push(ValidationError::ForeignKeyTargetColumnNotFound {
                            table: table_name.clone(),
                            target_table: fk.target_table.clone(),
                            target_column: target_col_name.clone(),
                        });
                    }
                    (Some(l), Some(t)) => {
                        if l.r#type != t.r#type {
                            errors.push(ValidationError::ForeignKeyTypeMismatch {
                                table: table_name.clone(),
                                column: local_col_name.clone(),
                                target_table: fk.target_table.clone(),
                                target_column: target_col_name.clone(),
                            });
                        }

                        // Check target uniqueness (either single PK or in a unique constraint)
                        let target_pk = target_table
                            .primary_key
                            .as_ref()
                            .map(|pk| pk.columns.clone())
                            .unwrap_or_default();
                        let is_target_unique = target_pk == vec![target_col_name.clone()]
                            || target_table
                                .unique_constraints
                                .iter()
                                .any(|u| u.columns == vec![target_col_name.clone()]);

                        if !is_target_unique {
                            errors.push(ValidationError::ForeignKeyTargetNotUnique {
                                table: table_name.clone(),
                                target_table: fk.target_table.clone(),
                                target_column: target_col_name.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::backend::SqliteBackend;
    use crate::sql::ir::*;

    fn sample_valid_schema() -> SchemaIr {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let mut name_col = ColumnIr::new(ColumnType::Text);
        name_col.max_length = Some(100);
        users_cols.insert("name".to_string(), name_col);

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

        SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables,
        }
    }

    #[test]
    fn test_valid_schema_passes() {
        let schema = sample_valid_schema();
        let backend = SqliteBackend;
        assert!(validate_schema(&schema, &backend).is_ok());
    }

    #[test]
    fn test_primary_key_nullable_fails() {
        let mut schema = sample_valid_schema();
        schema
            .tables
            .get_mut("users")
            .unwrap()
            .columns
            .get_mut("id")
            .unwrap()
            .nullable = true;

        let res = validate_schema(&schema, &SqliteBackend);
        assert!(res.is_err());
        let errs = res.unwrap_err();
        assert!(matches!(
            errs[0],
            ValidationError::PrimaryKeyCannotBeNullable { .. }
        ));
    }

    #[test]
    fn test_auto_increment_non_integer_fails() {
        let mut schema = sample_valid_schema();
        schema
            .tables
            .get_mut("users")
            .unwrap()
            .columns
            .get_mut("name")
            .unwrap()
            .auto_increment = true;

        let res = validate_schema(&schema, &SqliteBackend);
        assert!(res.is_err());
        let errs = res.unwrap_err();
        assert!(matches!(
            errs[0],
            ValidationError::AutoIncrementNonInteger { .. }
        ));
    }

    #[test]
    fn test_max_length_non_text_fails() {
        let mut schema = sample_valid_schema();
        schema
            .tables
            .get_mut("users")
            .unwrap()
            .columns
            .get_mut("id")
            .unwrap()
            .max_length = Some(10);

        let res = validate_schema(&schema, &SqliteBackend);
        assert!(res.is_err());
        let errs = res.unwrap_err();
        assert!(matches!(
            errs[0],
            ValidationError::MaxLengthNonText { .. }
        ));
    }

    #[test]
    fn test_fk_target_table_missing_fails() {
        let mut schema = sample_valid_schema();
        schema
            .tables
            .get_mut("users")
            .unwrap()
            .foreign_keys
            .push(ForeignKeyIr {
                columns: vec!["id".to_string()],
                target_table: "missing_table".to_string(),
                target_columns: vec!["id".to_string()],
                on_delete: ReferentialAction::Cascade,
                on_update: ReferentialAction::Cascade,
                name: None,
            });

        let res = validate_schema(&schema, &SqliteBackend);
        assert!(res.is_err());
        let errs = res.unwrap_err();
        assert!(matches!(
            errs[0],
            ValidationError::ForeignKeyTargetTableNotFound { .. }
        ));
    }
}
