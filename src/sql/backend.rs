use crate::sql::ir::{ColumnIr, ColumnType, DefaultValueIr};

/// Target database backend abstraction.
pub trait Backend: Send + Sync {
    /// Backend identifier name (e.g. "sqlite", "postgres").
    fn name(&self) -> &'static str;

    /// Render SQL column type declaration.
    fn sql_type(&self, col: &ColumnIr, table_name: &str, col_name: &str) -> String;

    /// Render Rust type string for a column (e.g. "i32", "chrono::DateTime<chrono::Utc>").
    fn rust_type(&self, col: &ColumnIr) -> String;

    /// Render Diesel SQL type string (e.g. "diesel::sql_types::Integer").
    fn diesel_sql_type(&self, col: &ColumnIr, table_name: &str) -> String;

    /// Render NAPI-RS / JS type annotation string.
    fn napi_type(&self, col: &ColumnIr) -> String;

    /// Render default value expression in SQL DDL.
    fn render_default(&self, default: &DefaultValueIr) -> String;

    /// Whether this backend supports native autoincrement primary key syntax.
    fn supports_auto_increment(&self) -> bool {
        true
    }

    /// Whether this backend requires table rebuilds for altering column nullability or drop column.
    fn requires_table_rebuild_for_alter(&self) -> bool;

    /// Check if this backend supports a given column type.
    fn supports_column_type(&self, col_type: &ColumnType) -> Result<(), String>;
}

/// SQLite backend implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct SqliteBackend;

impl Backend for SqliteBackend {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn sql_type(&self, col: &ColumnIr, _table_name: &str, _col_name: &str) -> String {
        match &col.r#type {
            ColumnType::Integer => {
                if col.auto_increment {
                    "INTEGER PRIMARY KEY AUTOINCREMENT".to_string()
                } else {
                    "INTEGER".to_string()
                }
            }
            ColumnType::Bigint => "INTEGER".to_string(),
            ColumnType::Float | ColumnType::Double => "REAL".to_string(),
            ColumnType::Boolean => "INTEGER".to_string(),
            ColumnType::Text => "TEXT".to_string(),
            ColumnType::Timestamp | ColumnType::Timestamptz => "TEXT".to_string(),
            ColumnType::Uuid => "TEXT".to_string(),
            ColumnType::Json => "TEXT".to_string(),
            ColumnType::Enum { .. } => "TEXT".to_string(),
        }
    }

    fn rust_type(&self, col: &ColumnIr) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer => "i32".to_string(),
            ColumnType::Bigint => "i64".to_string(),
            ColumnType::Float => "f32".to_string(),
            ColumnType::Double => "f64".to_string(),
            ColumnType::Boolean => "bool".to_string(),
            ColumnType::Text => "String".to_string(),
            ColumnType::Timestamp => "chrono::NaiveDateTime".to_string(),
            ColumnType::Timestamptz => "chrono::DateTime<chrono::Utc>".to_string(),
            ColumnType::Uuid => "uuid::Uuid".to_string(),
            ColumnType::Json => "serde_json::Value".to_string(),
            ColumnType::Enum { name } => crate::sql::naming::resolve_enum_type_name(name, None),
        };

        if col.nullable {
            format!("Option<{inner}>")
        } else {
            inner
        }
    }

    fn diesel_sql_type(&self, col: &ColumnIr, _table_name: &str) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer => "diesel::sql_types::Integer",
            ColumnType::Bigint => "diesel::sql_types::BigInt",
            ColumnType::Float => "diesel::sql_types::Float",
            ColumnType::Double => "diesel::sql_types::Double",
            ColumnType::Boolean => "diesel::sql_types::Bool",
            ColumnType::Text => "diesel::sql_types::Text",
            ColumnType::Timestamp | ColumnType::Timestamptz => "diesel::sql_types::Text",
            ColumnType::Uuid => "diesel::sql_types::Text",
            ColumnType::Json => "diesel::sql_types::Text",
            ColumnType::Enum { .. } => "diesel::sql_types::Text",
        };

        if col.nullable {
            format!("diesel::sql_types::Nullable<{inner}>")
        } else {
            inner.to_string()
        }
    }

    fn napi_type(&self, col: &ColumnIr) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer | ColumnType::Float | ColumnType::Double => "number".to_string(),
            ColumnType::Bigint => "bigint".to_string(),
            ColumnType::Boolean => "boolean".to_string(),
            ColumnType::Text
            | ColumnType::Timestamp
            | ColumnType::Timestamptz
            | ColumnType::Uuid => "string".to_string(),
            ColumnType::Json => "any".to_string(),
            ColumnType::Enum { name } => crate::sql::naming::resolve_enum_type_name(name, None),
        };

        if col.nullable {
            format!("Option<{inner}>")
        } else {
            inner
        }
    }

    fn render_default(&self, default: &DefaultValueIr) -> String {
        match default {
            DefaultValueIr::Now => "CURRENT_TIMESTAMP".to_string(),
            DefaultValueIr::Null => "NULL".to_string(),
            DefaultValueIr::Literal(serde_json::Value::Bool(b)) => {
                if *b {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            }
            DefaultValueIr::Literal(serde_json::Value::Number(n)) => n.to_string(),
            DefaultValueIr::Literal(serde_json::Value::String(s)) => {
                format!("'{}'", s.replace('\'', "''"))
            }
            DefaultValueIr::Literal(other) => {
                format!("'{}'", other.to_string().replace('\'', "''"))
            }
            DefaultValueIr::Custom(raw) => raw.clone(),
        }
    }

    fn requires_table_rebuild_for_alter(&self) -> bool {
        true
    }

    fn supports_column_type(&self, _col_type: &ColumnType) -> Result<(), String> {
        Ok(())
    }
}

/// PostgreSQL backend implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct PostgresBackend;

impl Backend for PostgresBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn sql_type(&self, col: &ColumnIr, _table_name: &str, _col_name: &str) -> String {
        if col.auto_increment {
            match &col.r#type {
                ColumnType::Integer => return "SERIAL".to_string(),
                ColumnType::Bigint => return "BIGSERIAL".to_string(),
                _ => {}
            }
        }

        match &col.r#type {
            ColumnType::Integer => "INTEGER".to_string(),
            ColumnType::Bigint => "BIGINT".to_string(),
            ColumnType::Float => "REAL".to_string(),
            ColumnType::Double => "DOUBLE PRECISION".to_string(),
            ColumnType::Boolean => "BOOLEAN".to_string(),
            ColumnType::Text => "TEXT".to_string(),
            ColumnType::Timestamp => "TIMESTAMP".to_string(),
            ColumnType::Timestamptz => "TIMESTAMPTZ".to_string(),
            ColumnType::Uuid => "UUID".to_string(),
            ColumnType::Json => "JSONB".to_string(),
            ColumnType::Enum { name } => name.clone(),
        }
    }

    fn rust_type(&self, col: &ColumnIr) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer => "i32".to_string(),
            ColumnType::Bigint => "i64".to_string(),
            ColumnType::Float => "f32".to_string(),
            ColumnType::Double => "f64".to_string(),
            ColumnType::Boolean => "bool".to_string(),
            ColumnType::Text => "String".to_string(),
            ColumnType::Timestamp => "chrono::NaiveDateTime".to_string(),
            ColumnType::Timestamptz => "chrono::DateTime<chrono::Utc>".to_string(),
            ColumnType::Uuid => "uuid::Uuid".to_string(),
            ColumnType::Json => "serde_json::Value".to_string(),
            ColumnType::Enum { name } => crate::sql::naming::resolve_enum_type_name(name, None),
        };

        if col.nullable {
            format!("Option<{inner}>")
        } else {
            inner
        }
    }

    fn diesel_sql_type(&self, col: &ColumnIr, _table_name: &str) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer => "diesel::sql_types::Integer".to_string(),
            ColumnType::Bigint => "diesel::sql_types::BigInt".to_string(),
            ColumnType::Float => "diesel::sql_types::Float".to_string(),
            ColumnType::Double => "diesel::sql_types::Double".to_string(),
            ColumnType::Boolean => "diesel::sql_types::Bool".to_string(),
            ColumnType::Text => "diesel::sql_types::Text".to_string(),
            ColumnType::Timestamp => "diesel::sql_types::Timestamp".to_string(),
            ColumnType::Timestamptz => "diesel::sql_types::Timestamptz".to_string(),
            ColumnType::Uuid => "diesel::sql_types::Uuid".to_string(),
            ColumnType::Json => "diesel::sql_types::Jsonb".to_string(),
            ColumnType::Enum { name } => format!(
                "crate::sql_types::{}",
                crate::sql::naming::resolve_enum_type_name(name, None)
            ),
        };

        if col.nullable {
            format!("diesel::sql_types::Nullable<{inner}>")
        } else {
            inner
        }
    }

    fn napi_type(&self, col: &ColumnIr) -> String {
        let inner = match &col.r#type {
            ColumnType::Integer | ColumnType::Float | ColumnType::Double => "number".to_string(),
            ColumnType::Bigint => "bigint".to_string(),
            ColumnType::Boolean => "boolean".to_string(),
            ColumnType::Text
            | ColumnType::Timestamp
            | ColumnType::Timestamptz
            | ColumnType::Uuid => "string".to_string(),
            ColumnType::Json => "any".to_string(),
            ColumnType::Enum { name } => crate::sql::naming::resolve_enum_type_name(name, None),
        };

        if col.nullable {
            format!("Option<{inner}>")
        } else {
            inner
        }
    }

    fn render_default(&self, default: &DefaultValueIr) -> String {
        match default {
            DefaultValueIr::Now => "NOW()".to_string(),
            DefaultValueIr::Null => "NULL".to_string(),
            DefaultValueIr::Literal(serde_json::Value::Bool(b)) => {
                if *b {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            DefaultValueIr::Literal(serde_json::Value::Number(n)) => n.to_string(),
            DefaultValueIr::Literal(serde_json::Value::String(s)) => {
                format!("'{}'", s.replace('\'', "''"))
            }
            DefaultValueIr::Literal(other) => {
                format!("'{}'", other.to_string().replace('\'', "''"))
            }
            DefaultValueIr::Custom(raw) => raw.clone(),
        }
    }

    fn requires_table_rebuild_for_alter(&self) -> bool {
        false
    }

    fn supports_column_type(&self, _col_type: &ColumnType) -> Result<(), String> {
        Ok(())
    }
}
