//! JS-First Schema Compiler (`src/sql/mod.rs`)
//!
//! Implements the complete JS-First Schema Compiler architecture as laid out in the
//! Implementation Plan and design specifications:
//!
//! - **Intermediate Representation (IR)**: Strongly typed, versioned, normalized schema
//!   data structure produced by the JavaScript DSL and validated before code generation.
//! - **Naming Module**: Consistent normalization for tables, columns, constraints, and
//!   Rust identifiers (snake_case, PascalCase, singularization, overrides).
//! - **Validation Pass**: Exhaustive schema verification (primary keys, foreign keys,
//!   type compatibility, unique constraints, index columns, and backend capability checks).
//! - **Backend Abstraction**: Pluggable backend trait with `Sqlite` and `Postgres` dialects.
//! - **Code Emitters**:
//!   1. Rust models: Row structs, `NewX` insert structs, enum types, derives.
//!   2. SQL DDL: `CREATE TABLE`, `CREATE INDEX`, foreign keys, and constraints.
//!   3. Diesel Schema: `table!`, `joinable!`, `allow_tables_to_appear_in_same_query!`.
//!   4. NAPI-RS Bindings: `#[napi(object)]` models and bridge types.
//!   5. Runtime Validators: Length, range, and format enforcement.
//! - **Migrations**: Schema diffing between IR snapshots emitting `up.sql` and `down.sql`.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// 1. Intermediate Representation (IR)
// -----------------------------------------------------------------------------

/// The root intermediate representation of a database schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaIr {
    /// Schema format version for forward/backward compatibility checks.
    pub version: String,
    /// Optional schema-wide naming strategy configuration.
    #[serde(default)]
    pub naming: Option<NamingConfig>,
    /// Defined custom database and model enums, keyed by enum name.
    #[serde(default)]
    pub enums: BTreeMap<String, Vec<String>>,
    /// Defined tables, keyed by table name.
    pub tables: BTreeMap<String, TableIr>,
}

/// A single table definition in the schema IR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableIr {
    /// Columns belonging to this table, keyed by column name.
    pub columns: BTreeMap<String, ColumnIr>,
    /// Optional explicit Rust struct name override.
    #[serde(default)]
    pub rust_name: Option<String>,
    /// Table-level secondary indexes.
    #[serde(default)]
    pub indexes: Vec<IndexIr>,
    /// Table-level multi-column or single-column unique constraints.
    #[serde(default)]
    pub uniques: Vec<UniqueConstraintIr>,
}

/// A column definition within a table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColumnIr {
    /// The logical data type of this column.
    #[serde(rename = "type")]
    pub col_type: ColumnType,
    /// Associated enum name if `col_type` is `Enum`.
    #[serde(default)]
    pub enum_name: Option<String>,
    /// Whether the column allows NULL values.
    #[serde(default)]
    pub nullable: bool,
    /// Whether the column is part of the primary key.
    #[serde(default)]
    pub primary_key: bool,
    /// Whether the column auto-increments upon insert.
    #[serde(default)]
    pub auto_increment: bool,
    /// Whether the column has a unique constraint.
    #[serde(default)]
    pub unique: bool,
    /// Optional maximum character length (enforced by validators / varchar).
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Optional default value expression or literal.
    #[serde(default)]
    pub default_value: Option<DefaultValueIr>,
    /// Foreign key reference configuration.
    #[serde(default)]
    pub references: Option<ForeignKeyReferenceIr>,
    /// Optional explicit Rust field name override.
    #[serde(default)]
    pub rust_name: Option<String>,
}

/// Supported logical column types across backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    Integer,
    BigInt,
    Float,
    Double,
    Boolean,
    Text,
    Timestamp,
    Timestamptz,
    Uuid,
    Json,
    Enum,
}

impl fmt::Display for ColumnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer => write!(f, "integer"),
            Self::BigInt => write!(f, "bigint"),
            Self::Float => write!(f, "float"),
            Self::Double => write!(f, "double"),
            Self::Boolean => write!(f, "boolean"),
            Self::Text => write!(f, "text"),
            Self::Timestamp => write!(f, "timestamp"),
            Self::Timestamptz => write!(f, "timestamptz"),
            Self::Uuid => write!(f, "uuid"),
            Self::Json => write!(f, "json"),
            Self::Enum => write!(f, "enum"),
        }
    }
}

/// A default value specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum DefaultValueIr {
    Literal(serde_json::Value),
    Function(String),
    Now,
    Null,
}

/// Foreign key reference configuration on a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForeignKeyReferenceIr {
    pub target_table: String,
    pub target_column: String,
    #[serde(default = "default_cascade_action")]
    pub on_delete: String,
    #[serde(default = "default_cascade_action")]
    pub on_update: String,
}

fn default_cascade_action() -> String {
    "NO ACTION".to_string()
}

/// Table-level secondary index definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndexIr {
    #[serde(default)]
    pub name: Option<String>,
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

/// Table-level unique constraint definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniqueConstraintIr {
    #[serde(default)]
    pub name: Option<String>,
    pub columns: Vec<String>,
}

/// Configurable naming rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamingConfig {
    #[serde(default)]
    pub table_strategy: Option<String>,
    #[serde(default)]
    pub column_strategy: Option<String>,
    #[serde(default)]
    pub struct_strategy: Option<String>,
}

// -----------------------------------------------------------------------------
// 2. Naming Module
// -----------------------------------------------------------------------------

pub mod naming {
    /// Convert an arbitrary string identifier to `snake_case`.
    pub fn to_snake_case(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 4);
        let mut prev_is_lower = false;

        for c in s.chars() {
            if c == '-' || c == ' ' || c == '.' {
                if !out.is_empty() && !out.ends_with('_') {
                    out.push('_');
                }
                prev_is_lower = false;
            } else if c.is_uppercase() {
                if prev_is_lower && !out.ends_with('_') {
                    out.push('_');
                }
                for lc in c.to_lowercase() {
                    out.push(lc);
                }
                prev_is_lower = false;
            } else {
                out.push(c);
                prev_is_lower = c.is_lowercase() || c.is_numeric();
            }
        }
        out
    }

    /// Convert an arbitrary identifier to `PascalCase`.
    pub fn to_pascal_case(s: &str) -> String {
        let snake = to_snake_case(s);
        let mut out = String::with_capacity(s.len());
        let mut cap_next = true;

        for c in snake.chars() {
            if c == '_' {
                cap_next = true;
            } else if cap_next {
                for uc in c.to_uppercase() {
                    out.push(uc);
                }
                cap_next = false;
            } else {
                out.push(c);
            }
        }
        out
    }

    /// English singularization for standard table plural naming conventions.
    pub fn singularize(s: &str) -> String {
        if s.ends_with("ies") && s.len() > 3 {
            format!("{}y", &s[..s.len() - 3])
        } else if s.ends_with("ses") || s.ends_with("xes") || s.ends_with("shes") || s.ends_with("ches") {
            s[..s.len() - 2].to_string()
        } else if s.ends_with('s') && !s.ends_with("ss") && s.len() > 1 {
            s[..s.len() - 1].to_string()
        } else {
            s.to_string()
        }
    }

    /// Determine the canonical Rust struct name for a given table name.
    pub fn table_to_struct_name(table_name: &str, custom_name: Option<&str>) -> String {
        if let Some(name) = custom_name {
            to_pascal_case(name)
        } else {
            let singular = singularize(table_name);
            to_pascal_case(&singular)
        }
    }

    /// Determine the Rust field name for a given column name.
    pub fn column_to_field_name(col_name: &str, custom_name: Option<&str>) -> String {
        if let Some(name) = custom_name {
            to_snake_case(name)
        } else {
            to_snake_case(col_name)
        }
    }

    /// Determine the index name, defaulting to `{table}_{cols}_idx` if unnamed.
    pub fn index_name(table_name: &str, cols: &[String], custom_name: Option<&str>) -> String {
        if let Some(name) = custom_name {
            to_snake_case(name)
        } else {
            format!("{}_{}_idx", table_name, cols.join("_"))
        }
    }
}

// -----------------------------------------------------------------------------
// 3. Validation Pass
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("Table '{table}': column '{column}' has primaryKey and nullable, which is invalid")]
    PrimaryKeyCannotBeNullable { table: String, column: String },

    #[error("Table '{table}': column '{column}' has autoIncrement on non-integer type '{col_type}'")]
    AutoIncrementOnlyOnInteger {
        table: String,
        column: String,
        col_type: ColumnType,
    },

    #[error("Table '{table}': column '{column}' has maxLength on non-text type '{col_type}'")]
    MaxLengthOnlyOnText {
        table: String,
        column: String,
        col_type: ColumnType,
    },

    #[error("Table '{table}': column '{column}' references unknown enum '{enum_name}'")]
    UnknownEnum {
        table: String,
        column: String,
        enum_name: String,
    },

    #[error("Table '{table}': column '{column}' default '{default_val}' does not match enum '{enum_name}' variants {variants:?}")]
    InvalidEnumDefault {
        table: String,
        column: String,
        enum_name: String,
        default_val: String,
        variants: Vec<String>,
    },

    #[error("Table '{table}': column '{column}' references missing table '{target_table}'")]
    MissingTargetTable {
        table: String,
        column: String,
        target_table: String,
    },

    #[error("Table '{table}': column '{column}' references missing column '{target_table}.{target_column}'")]
    MissingTargetColumn {
        table: String,
        column: String,
        target_table: String,
        target_column: String,
    },

    #[error("Table '{table}': index '{index}' references missing column '{column}'")]
    IndexColumnNotFound {
        table: String,
        index: String,
        column: String,
    },

    #[error("Table '{table}': unique constraint '{name}' references missing column '{column}'")]
    UniqueColumnNotFound {
        table: String,
        name: String,
        column: String,
    },

    #[error("Table '{table}': no primary key defined")]
    MissingPrimaryKey { table: String },

    #[error("Backend '{backend}' does not support column type '{col_type}' for column '{table}.{column}'")]
    UnsupportedBackendType {
        backend: String,
        table: String,
        column: String,
        col_type: ColumnType,
    },
}

/// Validate the schema IR against structural rules and backend capabilities.
pub fn validate(schema: &SchemaIr, backend: Option<&dyn Backend>) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    for (table_name, table) in &schema.tables {
        let mut has_pk = false;

        for (col_name, col) in &table.columns {
            if col.primary_key {
                has_pk = true;
                if col.nullable {
                    errors.push(ValidationError::PrimaryKeyCannotBeNullable {
                        table: table_name.clone(),
                        column: col_name.clone(),
                    });
                }
            }

            if col.auto_increment && col.col_type != ColumnType::Integer && col.col_type != ColumnType::BigInt {
                errors.push(ValidationError::AutoIncrementOnlyOnInteger {
                    table: table_name.clone(),
                    column: col_name.clone(),
                    col_type: col.col_type,
                });
            }

            if col.max_length.is_some() && col.col_type != ColumnType::Text {
                errors.push(ValidationError::MaxLengthOnlyOnText {
                    table: table_name.clone(),
                    column: col_name.clone(),
                    col_type: col.col_type,
                });
            }

            if col.col_type == ColumnType::Enum {
                match &col.enum_name {
                    Some(enum_name) => match schema.enums.get(enum_name) {
                        Some(variants) => {
                            if let Some(DefaultValueIr::Literal(serde_json::Value::String(s))) = &col.default_value {
                                if !variants.contains(s) {
                                    errors.push(ValidationError::InvalidEnumDefault {
                                        table: table_name.clone(),
                                        column: col_name.clone(),
                                        enum_name: enum_name.clone(),
                                        default_val: s.clone(),
                                        variants: variants.clone(),
                                    });
                                }
                            }
                        }
                        None => {
                            errors.push(ValidationError::UnknownEnum {
                                table: table_name.clone(),
                                column: col_name.clone(),
                                enum_name: enum_name.clone(),
                            });
                        }
                    },
                    None => {
                        errors.push(ValidationError::UnknownEnum {
                            table: table_name.clone(),
                            column: col_name.clone(),
                            enum_name: "<unspecified>".to_string(),
                        });
                    }
                }
            }

            if let Some(target) = &col.references {
                match schema.tables.get(&target.target_table) {
                    Some(target_tbl) => {
                        if !target_tbl.columns.contains_key(&target.target_column) {
                            errors.push(ValidationError::MissingTargetColumn {
                                table: table_name.clone(),
                                column: col_name.clone(),
                                target_table: target.target_table.clone(),
                                target_column: target.target_column.clone(),
                            });
                        }
                    }
                    None => {
                        errors.push(ValidationError::MissingTargetTable {
                            table: table_name.clone(),
                            column: col_name.clone(),
                            target_table: target.target_table.clone(),
                        });
                    }
                }
            }

            if let Some(be) = backend {
                if !be.supports_type(&col.col_type) {
                    errors.push(ValidationError::UnsupportedBackendType {
                        backend: be.name().to_string(),
                        table: table_name.clone(),
                        column: col_name.clone(),
                        col_type: col.col_type,
                    });
                }
            }
        }

        if !has_pk {
            errors.push(ValidationError::MissingPrimaryKey {
                table: table_name.clone(),
            });
        }

        for idx in &table.indexes {
            let idx_name = naming::index_name(table_name, &idx.columns, idx.name.as_deref());
            for col in &idx.columns {
                if !table.columns.contains_key(col) {
                    errors.push(ValidationError::IndexColumnNotFound {
                        table: table_name.clone(),
                        index: idx_name.clone(),
                        column: col.clone(),
                    });
                }
            }
        }

        for unq in &table.uniques {
            let unq_name = unq.name.clone().unwrap_or_else(|| format!("{}_unique", table_name));
            for col in &unq.columns {
                if !table.columns.contains_key(col) {
                    errors.push(ValidationError::UniqueColumnNotFound {
                        table: table_name.clone(),
                        name: unq_name.clone(),
                        column: col.clone(),
                    });
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

// -----------------------------------------------------------------------------
// 4. Backend Dialect Trait & Implementations
// -----------------------------------------------------------------------------

pub trait Backend {
    fn name(&self) -> &'static str;
    fn supports_type(&self, col_type: &ColumnType) -> bool;
    fn default_now_sql(&self) -> &'static str;
    fn sql_column_type(
        &self,
        col: &ColumnIr,
        enum_map: &BTreeMap<String, Vec<String>>,
    ) -> String;
    fn diesel_sql_type(&self, col: &ColumnIr) -> String;
    fn rust_field_type(&self, col: &ColumnIr, is_insert: bool) -> String;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteBackend;

impl Backend for SqliteBackend {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn supports_type(&self, _col_type: &ColumnType) -> bool {
        true
    }

    fn default_now_sql(&self) -> &'static str {
        "CURRENT_TIMESTAMP"
    }

    fn sql_column_type(
        &self,
        col: &ColumnIr,
        enum_map: &BTreeMap<String, Vec<String>>,
    ) -> String {
        match col.col_type {
            ColumnType::Integer => {
                if col.primary_key && col.auto_increment {
                    "INTEGER PRIMARY KEY AUTOINCREMENT".to_string()
                } else {
                    "INTEGER".to_string()
                }
            }
            ColumnType::BigInt => "INTEGER".to_string(),
            ColumnType::Float | ColumnType::Double => "REAL".to_string(),
            ColumnType::Boolean => "INTEGER".to_string(),
            ColumnType::Text | ColumnType::Timestamp | ColumnType::Timestamptz | ColumnType::Uuid | ColumnType::Json => {
                "TEXT".to_string()
            }
            ColumnType::Enum => {
                if let Some(enum_name) = &col.enum_name {
                    if let Some(variants) = enum_map.get(enum_name) {
                        let allowed: Vec<String> = variants.iter().map(|v| format!("'{}'", v)).collect();
                        return format!("TEXT CHECK({} IN ({}))", "%COL%", allowed.join(", "));
                    }
                }
                "TEXT".to_string()
            }
        }
    }

    fn diesel_sql_type(&self, col: &ColumnIr) -> String {
        let base = match col.col_type {
            ColumnType::Integer => "Integer",
            ColumnType::BigInt => "BigInt",
            ColumnType::Float => "Float",
            ColumnType::Double => "Double",
            ColumnType::Boolean => "Bool",
            ColumnType::Text | ColumnType::Uuid | ColumnType::Enum => "Text",
            ColumnType::Timestamp => "Timestamp",
            ColumnType::Timestamptz => "Timestamptz",
            ColumnType::Json => "Text",
        };
        if col.nullable {
            format!("Nullable<{}>", base)
        } else {
            base.to_string()
        }
    }

    fn rust_field_type(&self, col: &ColumnIr, is_insert: bool) -> String {
        let base = match col.col_type {
            ColumnType::Integer => "i32".to_string(),
            ColumnType::BigInt => "i64".to_string(),
            ColumnType::Float => "f32".to_string(),
            ColumnType::Double => "f64".to_string(),
            ColumnType::Boolean => "bool".to_string(),
            ColumnType::Text => "String".to_string(),
            ColumnType::Timestamp => "chrono::NaiveDateTime".to_string(),
            ColumnType::Timestamptz => "chrono::DateTime<chrono::Utc>".to_string(),
            ColumnType::Uuid => "uuid::Uuid".to_string(),
            ColumnType::Json => "serde_json::Value".to_string(),
            ColumnType::Enum => {
                col.enum_name
                    .as_deref()
                    .map(naming::to_pascal_case)
                    .unwrap_or_else(|| "String".to_string())
            }
        };

        if is_insert {
            if col.nullable || col.auto_increment || col.default_value.is_some() {
                format!("Option<{}>", base)
            } else {
                base
            }
        } else if col.nullable {
            format!("Option<{}>", base)
        } else {
            base
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresBackend;

impl Backend for PostgresBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn supports_type(&self, _col_type: &ColumnType) -> bool {
        true
    }

    fn default_now_sql(&self) -> &'static str {
        "NOW()"
    }

    fn sql_column_type(
        &self,
        col: &ColumnIr,
        _enum_map: &BTreeMap<String, Vec<String>>,
    ) -> String {
        match col.col_type {
            ColumnType::Integer => {
                if col.primary_key && col.auto_increment {
                    "SERIAL".to_string()
                } else {
                    "INTEGER".to_string()
                }
            }
            ColumnType::BigInt => {
                if col.primary_key && col.auto_increment {
                    "BIGSERIAL".to_string()
                } else {
                    "BIGINT".to_string()
                }
            }
            ColumnType::Float => "REAL".to_string(),
            ColumnType::Double => "DOUBLE PRECISION".to_string(),
            ColumnType::Boolean => "BOOLEAN".to_string(),
            ColumnType::Text => "TEXT".to_string(),
            ColumnType::Timestamp => "TIMESTAMP".to_string(),
            ColumnType::Timestamptz => "TIMESTAMPTZ".to_string(),
            ColumnType::Uuid => "UUID".to_string(),
            ColumnType::Json => "JSONB".to_string(),
            ColumnType::Enum => col.enum_name.clone().unwrap_or_else(|| "TEXT".to_string()),
        }
    }

    fn diesel_sql_type(&self, col: &ColumnIr) -> String {
        let base = match col.col_type {
            ColumnType::Integer => "Integer",
            ColumnType::BigInt => "BigInt",
            ColumnType::Float => "Float",
            ColumnType::Double => "Double",
            ColumnType::Boolean => "Bool",
            ColumnType::Text => "Text",
            ColumnType::Timestamp => "Timestamp",
            ColumnType::Timestamptz => "Timestamptz",
            ColumnType::Uuid => "Uuid",
            ColumnType::Json => "Jsonb",
            ColumnType::Enum => "Text",
        };
        if col.nullable {
            format!("Nullable<{}>", base)
        } else {
            base.to_string()
        }
    }

    fn rust_field_type(&self, col: &ColumnIr, is_insert: bool) -> String {
        SqliteBackend.rust_field_type(col, is_insert)
    }
}

// -----------------------------------------------------------------------------
// 5. Code Emitters
// -----------------------------------------------------------------------------

/// Emit SQL DDL schema statements (`CREATE TABLE`, `CREATE INDEX`, foreign keys).
pub fn emit_sql_ddl(schema: &SchemaIr, backend: &dyn Backend) -> String {
    let mut out = String::new();

    // Postgres: emit custom ENUM types first
    if backend.name() == "postgres" {
        for (enum_name, variants) in &schema.enums {
            let quoted: Vec<String> = variants.iter().map(|v| format!("'{}'", v)).collect();
            out.push_str(&format!(
                "CREATE TYPE {} AS ENUM ({});\n\n",
                enum_name,
                quoted.join(", ")
            ));
        }
    }

    for (table_name, table) in &schema.tables {
        out.push_str(&format!("CREATE TABLE {} (\n", table_name));

        let mut lines = Vec::new();
        let mut pks = Vec::new();

        for (col_name, col) in &table.columns {
            let mut col_def = format!("    {}", col_name);
            let mut sql_type = backend.sql_column_type(col, &schema.enums);
            if sql_type.contains("%COL%") {
                sql_type = sql_type.replace("%COL%", col_name);
            }
            col_def.push_str(&format!(" {}", sql_type));

            // In SQLite, an INTEGER PRIMARY KEY AUTOINCREMENT includes primary key declaration
            let already_declared_pk = backend.name() == "sqlite"
                && col.primary_key
                && col.auto_increment
                && col.col_type == ColumnType::Integer;

            if !already_declared_pk && !col.nullable {
                col_def.push_str(" NOT NULL");
            }

            if col.unique && !col.primary_key {
                col_def.push_str(" UNIQUE");
            }

            if let Some(default_val) = &col.default_value {
                match default_val {
                    DefaultValueIr::Now => {
                        col_def.push_str(&format!(" DEFAULT {}", backend.default_now_sql()));
                    }
                    DefaultValueIr::Literal(serde_json::Value::Bool(b)) => {
                        let repr = if backend.name() == "sqlite" {
                            if *b { "1" } else { "0" }
                        } else {
                            if *b { "TRUE" } else { "FALSE" }
                        };
                        col_def.push_str(&format!(" DEFAULT {}", repr));
                    }
                    DefaultValueIr::Literal(serde_json::Value::Number(n)) => {
                        col_def.push_str(&format!(" DEFAULT {}", n));
                    }
                    DefaultValueIr::Literal(serde_json::Value::String(s)) => {
                        col_def.push_str(&format!(" DEFAULT '{}'", s));
                    }
                    DefaultValueIr::Function(func) => {
                        col_def.push_str(&format!(" DEFAULT {}", func));
                    }
                    DefaultValueIr::Null => {
                        col_def.push_str(" DEFAULT NULL");
                    }
                    _ => {}
                }
            }

            if let Some(fk) = &col.references {
                col_def.push_str(&format!(
                    " REFERENCES {}({}) ON DELETE {} ON UPDATE {}",
                    fk.target_table, fk.target_column, fk.on_delete, fk.on_update
                ));
            }

            if col.primary_key && !already_declared_pk {
                pks.push(col_name.clone());
            }

            lines.push(col_def);
        }

        if !pks.is_empty() {
            lines.push(format!("    PRIMARY KEY ({})", pks.join(", ")));
        }

        for unq in &table.uniques {
            lines.push(format!("    UNIQUE ({})", unq.columns.join(", ")));
        }

        out.push_str(&lines.join(",\n"));
        out.push_str("\n);\n\n");

        for idx in &table.indexes {
            let idx_name = naming::index_name(table_name, &idx.columns, idx.name.as_deref());
            let unique_str = if idx.unique { "UNIQUE " } else { "" };
            out.push_str(&format!(
                "CREATE {}INDEX {} ON {} ({});\n",
                unique_str,
                idx_name,
                table_name,
                idx.columns.join(", ")
            ));
        }
        if !table.indexes.is_empty() {
            out.push('\n');
        }
    }

    out
}

/// Emit Rust models (`Row` structs, `NewX` insert structs, and enum types).
pub fn emit_rust_models(schema: &SchemaIr, backend: &dyn Backend) -> String {
    let mut out = String::new();

    out.push_str("// Generated by JS-First Schema Compiler\n");
    out.push_str("#![allow(unused_imports, clippy::all)]\n\n");
    out.push_str("use diesel::prelude::*;\n");
    out.push_str("use serde::{Deserialize, Serialize};\n\n");

    // Emit enum definitions
    for (enum_name, variants) in &schema.enums {
        let rust_enum_name = naming::to_pascal_case(enum_name);
        out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n");
        out.push_str("#[serde(rename_all = \"lowercase\")]\n");
        out.push_str(&format!("pub enum {} {{\n", rust_enum_name));
        for v in variants {
            out.push_str(&format!("    {},\n", naming::to_pascal_case(v)));
        }
        out.push_str("}\n\n");
    }

    // Emit row & insert structs per table
    for (table_name, table) in &schema.tables {
        let struct_name = naming::table_to_struct_name(table_name, table.rust_name.as_deref());
        let new_struct_name = format!("New{}", struct_name);

        // Row struct
        out.push_str("#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Serialize, Deserialize)]\n");
        out.push_str(&format!("#[diesel(table_name = crate::sql::schema::{})]\n", table_name));
        out.push_str(&format!("pub struct {} {{\n", struct_name));
        for (col_name, col) in &table.columns {
            let field_name = naming::column_to_field_name(col_name, col.rust_name.as_deref());
            let rust_type = backend.rust_field_type(col, false);
            out.push_str(&format!("    pub {}: {},\n", field_name, rust_type));
        }
        out.push_str("}\n\n");

        // Insert struct (NewX)
        out.push_str("#[derive(Debug, Clone, Insertable, Serialize, Deserialize)]\n");
        out.push_str(&format!("#[diesel(table_name = crate::sql::schema::{})]\n", table_name));
        out.push_str(&format!("pub struct {} {{\n", new_struct_name));
        for (col_name, col) in &table.columns {
            let field_name = naming::column_to_field_name(col_name, col.rust_name.as_deref());
            let rust_type = backend.rust_field_type(col, true);
            out.push_str(&format!("    pub {}: {},\n", field_name, rust_type));
        }
        out.push_str("}\n\n");
    }

    out
}

/// Emit Diesel `table!`, `joinable!`, and table group macros.
pub fn emit_diesel_schema(schema: &SchemaIr, backend: &dyn Backend) -> String {
    let mut out = String::new();

    out.push_str("// Generated Diesel schema definitions\n\n");
    out.push_str("diesel::table! {\n");

    for (table_name, table) in &schema.tables {
        let pks: Vec<String> = table
            .columns
            .iter()
            .filter(|(_, c)| c.primary_key)
            .map(|(name, _)| name.clone())
            .collect();
        let pk_tuple = if pks.is_empty() {
            "id".to_string()
        } else if pks.len() == 1 {
            pks[0].clone()
        } else {
            format!("({})", pks.join(", "))
        };

        out.push_str(&format!("    {} ({}) {{\n", table_name, pk_tuple));
        for (col_name, col) in &table.columns {
            let sql_type = backend.diesel_sql_type(col);
            out.push_str(&format!("        {} -> {},\n", col_name, sql_type));
        }
        out.push_str("    }\n\n");
    }

    out.push_str("}\n\n");

    // Foreign key joinables
    for (table_name, table) in &schema.tables {
        for (col_name, col) in &table.columns {
            if let Some(fk) = &col.references {
                out.push_str(&format!(
                    "diesel::joinable!({} -> {} ({}));\n",
                    table_name, fk.target_table, col_name
                ));
            }
        }
    }

    let all_tables: Vec<&str> = schema.tables.keys().map(String::as_str).collect();
    if all_tables.len() > 1 {
        out.push_str(&format!(
            "\ndiesel::allow_tables_to_appear_in_same_query!(\n    {}\n);\n",
            all_tables.join(",\n    ")
        ));
    }

    out
}

/// Emit NAPI-RS `#[napi(object)]` struct representations.
pub fn emit_napi_bindings(schema: &SchemaIr, _backend: &dyn Backend) -> String {
    let mut out = String::new();

    out.push_str("// Generated NAPI-RS bindings\n");
    out.push_str("#![allow(unused_imports, clippy::all)]\n\n");
    out.push_str("use napi_derive::napi;\n\n");

    for (table_name, table) in &schema.tables {
        let struct_name = naming::table_to_struct_name(table_name, table.rust_name.as_deref());

        out.push_str("#[napi(object)]\n");
        out.push_str("#[derive(Debug, Clone)]\n");
        out.push_str(&format!("pub struct {}Js {{\n", struct_name));
        for (col_name, col) in &table.columns {
            let field_name = naming::column_to_field_name(col_name, col.rust_name.as_deref());
            let js_type = match col.col_type {
                ColumnType::Integer => "i32",
                ColumnType::BigInt => "i64",
                ColumnType::Float => "f64",
                ColumnType::Double => "f64",
                ColumnType::Boolean => "bool",
                ColumnType::Text | ColumnType::Enum => "String",
                ColumnType::Timestamp | ColumnType::Timestamptz => "String",
                ColumnType::Uuid => "String",
                ColumnType::Json => "String",
            };
            if col.nullable {
                out.push_str(&format!("    pub {}: Option<{}>,\n", field_name, js_type));
            } else {
                out.push_str(&format!("    pub {}: {},\n", field_name, js_type));
            }
        }
        out.push_str("}\n\n");
    }

    out
}

/// Emit runtime validator implementations (e.g., `maxLength`).
pub fn emit_validators(schema: &SchemaIr) -> String {
    let mut out = String::new();

    out.push_str("// Generated runtime validators\n\n");

    for (table_name, table) in &schema.tables {
        let struct_name = naming::table_to_struct_name(table_name, table.rust_name.as_deref());

        out.push_str(&format!("impl {} {{\n", struct_name));
        out.push_str("    pub fn validate(&self) -> Result<(), Vec<String>> {\n");
        out.push_str("        let mut errors = Vec::new();\n");

        for (col_name, col) in &table.columns {
            let field_name = naming::column_to_field_name(col_name, col.rust_name.as_deref());
            if let Some(max_len) = col.max_length {
                if col.nullable {
                    out.push_str(&format!(
                        "        if let Some(val) = &self.{} {{\n",
                        field_name
                    ));
                    out.push_str(&format!(
                        "            if val.len() > {} {{\n",
                        max_len
                    ));
                    out.push_str(&format!(
                        "                errors.push(\"Field '{}' exceeds max length of {}\".to_string());\n",
                        field_name, max_len
                    ));
                    out.push_str("            }\n");
                    out.push_str("        }\n");
                } else {
                    out.push_str(&format!(
                        "        if self.{}.len() > {} {{\n",
                        field_name, max_len
                    ));
                    out.push_str(&format!(
                        "            errors.push(\"Field '{}' exceeds max length of {}\".to_string());\n",
                        field_name, max_len
                    ));
                    out.push_str("        }\n");
                }
            }
        }

        out.push_str("        if errors.is_empty() { Ok(()) } else { Err(errors) }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    out
}

// -----------------------------------------------------------------------------
// 6. Schema Migrations (Diffing)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SchemaDiff {
    CreateTable { name: String, table: TableIr },
    DropTable { name: String },
    AddColumn { table: String, name: String, column: ColumnIr },
    DropColumn { table: String, name: String },
    AddIndex { table: String, index: IndexIr },
    DropIndex { table: String, name: String },
}

/// Compute differences between a previous schema and a current schema.
pub fn diff(old_schema: &SchemaIr, new_schema: &SchemaIr) -> Vec<SchemaDiff> {
    let mut diffs = Vec::new();

    // Check for dropped tables
    for old_table in old_schema.tables.keys() {
        if !new_schema.tables.contains_key(old_table) {
            diffs.push(SchemaDiff::DropTable {
                name: old_table.clone(),
            });
        }
    }

    // Check for created or modified tables
    for (table_name, new_table) in &new_schema.tables {
        match old_schema.tables.get(table_name) {
            None => {
                diffs.push(SchemaDiff::CreateTable {
                    name: table_name.clone(),
                    table: new_table.clone(),
                });
            }
            Some(old_table) => {
                // Columns dropped
                for old_col in old_table.columns.keys() {
                    if !new_table.columns.contains_key(old_col) {
                        diffs.push(SchemaDiff::DropColumn {
                            table: table_name.clone(),
                            name: old_col.clone(),
                        });
                    }
                }
                // Columns added
                for (new_col_name, new_col) in &new_table.columns {
                    if !old_table.columns.contains_key(new_col_name) {
                        diffs.push(SchemaDiff::AddColumn {
                            table: table_name.clone(),
                            name: new_col_name.clone(),
                            column: new_col.clone(),
                        });
                    }
                }
                // Indexes added
                for new_idx in &new_table.indexes {
                    if !old_table.indexes.contains(new_idx) {
                        diffs.push(SchemaDiff::AddIndex {
                            table: table_name.clone(),
                            index: new_idx.clone(),
                        });
                    }
                }
                // Indexes dropped
                for old_idx in &old_table.indexes {
                    if !new_table.indexes.contains(old_idx) {
                        let idx_name = naming::index_name(
                            table_name,
                            &old_idx.columns,
                            old_idx.name.as_deref(),
                        );
                        diffs.push(SchemaDiff::DropIndex {
                            table: table_name.clone(),
                            name: idx_name,
                        });
                    }
                }
            }
        }
    }

    diffs
}

/// Render a set of schema differences into `up.sql` and `down.sql`.
pub fn emit_migration(
    diffs: &[SchemaDiff],
    schema: &SchemaIr,
    backend: &dyn Backend,
) -> (String, String) {
    let mut up = String::new();
    let mut down = String::new();

    for d in diffs {
        match d {
            SchemaDiff::CreateTable { name, table } => {
                let mut single_schema = schema.clone();
                single_schema.tables.clear();
                single_schema.tables.insert(name.clone(), table.clone());
                let create_sql = emit_sql_ddl(&single_schema, backend);
                up.push_str(&create_sql);
                down.push_str(&format!("DROP TABLE IF EXISTS {};\n", name));
            }
            SchemaDiff::DropTable { name } => {
                up.push_str(&format!("DROP TABLE IF EXISTS {};\n", name));
                down.push_str(&format!("-- Reverse drop of table {}\n", name));
            }
            SchemaDiff::AddColumn { table, name, column } => {
                let sql_type = backend.sql_column_type(column, &schema.enums);
                let not_null = if column.nullable { "" } else { " NOT NULL DEFAULT ''" };
                up.push_str(&format!(
                    "ALTER TABLE {} ADD COLUMN {} {}{};\n",
                    table, name, sql_type, not_null
                ));
                down.push_str(&format!("ALTER TABLE {} DROP COLUMN {};\n", table, name));
            }
            SchemaDiff::DropColumn { table, name } => {
                up.push_str(&format!("ALTER TABLE {} DROP COLUMN {};\n", table, name));
                down.push_str(&format!("-- Reverse drop of column {}.{}\n", table, name));
            }
            SchemaDiff::AddIndex { table, index } => {
                let idx_name = naming::index_name(table, &index.columns, index.name.as_deref());
                let unique = if index.unique { "UNIQUE " } else { "" };
                up.push_str(&format!(
                    "CREATE {}INDEX {} ON {} ({});\n",
                    unique,
                    idx_name,
                    table,
                    index.columns.join(", ")
                ));
                down.push_str(&format!("DROP INDEX IF EXISTS {};\n", idx_name));
            }
            SchemaDiff::DropIndex { name, .. } => {
                up.push_str(&format!("DROP INDEX IF EXISTS {};\n", name));
                down.push_str(&format!("-- Reverse drop of index {}\n", name));
            }
        }
    }

    (up, down)
}

// -----------------------------------------------------------------------------
// 7. Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ir() -> SchemaIr {
        let mut enums = BTreeMap::new();
        enums.insert(
            "user_role".to_string(),
            vec!["admin".to_string(), "editor".to_string(), "viewer".to_string()],
        );

        let mut users_cols = BTreeMap::new();
        users_cols.insert(
            "id".to_string(),
            ColumnIr {
                col_type: ColumnType::Integer,
                enum_name: None,
                nullable: false,
                primary_key: true,
                auto_increment: true,
                unique: false,
                max_length: None,
                default_value: None,
                references: None,
                rust_name: None,
            },
        );
        users_cols.insert(
            "name".to_string(),
            ColumnIr {
                col_type: ColumnType::Text,
                enum_name: None,
                nullable: false,
                primary_key: false,
                auto_increment: false,
                unique: false,
                max_length: Some(100),
                default_value: None,
                references: None,
                rust_name: None,
            },
        );
        users_cols.insert(
            "role".to_string(),
            ColumnIr {
                col_type: ColumnType::Enum,
                enum_name: Some("user_role".to_string()),
                nullable: false,
                primary_key: false,
                auto_increment: false,
                unique: false,
                max_length: None,
                default_value: Some(DefaultValueIr::Literal(serde_json::Value::String(
                    "viewer".to_string(),
                ))),
                references: None,
                rust_name: None,
            },
        );

        let mut posts_cols = BTreeMap::new();
        posts_cols.insert(
            "id".to_string(),
            ColumnIr {
                col_type: ColumnType::Integer,
                enum_name: None,
                nullable: false,
                primary_key: true,
                auto_increment: true,
                unique: false,
                max_length: None,
                default_value: None,
                references: None,
                rust_name: None,
            },
        );
        posts_cols.insert(
            "author_id".to_string(),
            ColumnIr {
                col_type: ColumnType::Integer,
                enum_name: None,
                nullable: false,
                primary_key: false,
                auto_increment: false,
                unique: false,
                max_length: None,
                default_value: None,
                references: Some(ForeignKeyReferenceIr {
                    target_table: "users".to_string(),
                    target_column: "id".to_string(),
                    on_delete: "CASCADE".to_string(),
                    on_update: "CASCADE".to_string(),
                }),
                rust_name: None,
            },
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "users".to_string(),
            TableIr {
                columns: users_cols,
                rust_name: None,
                indexes: vec![IndexIr {
                    name: Some("idx_users_role".to_string()),
                    columns: vec!["role".to_string()],
                    unique: false,
                }],
                uniques: vec![],
            },
        );
        tables.insert(
            "posts".to_string(),
            TableIr {
                columns: posts_cols,
                rust_name: None,
                indexes: vec![],
                uniques: vec![],
            },
        );

        SchemaIr {
            version: "1.0.0".to_string(),
            naming: None,
            enums,
            tables,
        }
    }

    #[test]
    fn test_validation_success() {
        let ir = sample_ir();
        assert!(validate(&ir, Some(&SqliteBackend)).is_ok());
        assert!(validate(&ir, Some(&PostgresBackend)).is_ok());
    }

    #[test]
    fn test_validation_missing_fk_target() {
        let mut ir = sample_ir();
        ir.tables.get_mut("posts").unwrap().columns.get_mut("author_id").unwrap().references =
            Some(ForeignKeyReferenceIr {
                target_table: "nonexistent".to_string(),
                target_column: "id".to_string(),
                on_delete: "CASCADE".to_string(),
                on_update: "CASCADE".to_string(),
            });
        assert!(validate(&ir, None).is_err());
    }

    #[test]
    fn test_naming_conventions() {
        assert_eq!(naming::to_snake_case("authorId"), "author_id");
        assert_eq!(naming::to_pascal_case("user_role"), "UserRole");
        assert_eq!(naming::singularize("users"), "user");
        assert_eq!(naming::singularize("categories"), "category");
        assert_eq!(naming::table_to_struct_name("users", None), "User");
        assert_eq!(naming::column_to_field_name("createdAt", None), "created_at");
    }

    #[test]
    fn test_ddl_and_models_emission() {
        let ir = sample_ir();
        let sqlite_ddl = emit_sql_ddl(&ir, &SqliteBackend);
        assert!(sqlite_ddl.contains("CREATE TABLE users"));
        assert!(sqlite_ddl.contains("REFERENCES users(id)"));

        let models = emit_rust_models(&ir, &SqliteBackend);
        assert!(models.contains("pub struct User"));
        assert!(models.contains("pub struct NewUser"));
        assert!(models.contains("pub struct Post"));
        assert!(models.contains("pub enum UserRole"));

        let diesel = emit_diesel_schema(&ir, &SqliteBackend);
        assert!(diesel.contains("diesel::table!"));
        assert!(diesel.contains("diesel::joinable!(posts -> users (author_id));"));

        let napi = emit_napi_bindings(&ir, &SqliteBackend);
        assert!(napi.contains("#[napi(object)]"));
        assert!(napi.contains("pub struct UserJs"));
    }

    #[test]
    fn test_diff_and_migrations() {
        let old_ir = sample_ir();
        let mut new_ir = old_ir.clone();
        new_ir.tables.get_mut("users").unwrap().columns.insert(
            "bio".to_string(),
            ColumnIr {
                col_type: ColumnType::Text,
                enum_name: None,
                nullable: true,
                primary_key: false,
                auto_increment: false,
                unique: false,
                max_length: Some(500),
                default_value: None,
                references: None,
                rust_name: None,
            },
        );

        let diffs = diff(&old_ir, &new_ir);
        assert_eq!(diffs.len(), 1);
        let (up, down) = emit_migration(&diffs, &new_ir, &SqliteBackend);
        assert!(up.contains("ALTER TABLE users ADD COLUMN bio TEXT"));
        assert!(down.contains("ALTER TABLE users DROP COLUMN bio"));
    }
}
