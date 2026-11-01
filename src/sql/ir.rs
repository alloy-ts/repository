use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};

/// Version identifier for the IR format to protect against format mismatches.
pub const CURRENT_IR_VERSION: u32 = 1;

/// Top-level schema Intermediate Representation (IR).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaIr {
    /// Monotonically increasing schema IR version integer.
    pub ir_version: u32,
    /// Schema naming convention preferences.
    #[serde(default)]
    pub naming: NamingConfig,
    /// Registered enums keyed by their logical enum name.
    #[serde(default)]
    pub enums: BTreeMap<String, EnumIr>,
    /// Tables keyed by name (`tables: { users: table({...}) }`), ensuring duplicate names cannot exist.
    #[serde(default)]
    pub tables: BTreeMap<String, TableIr>,
}

/// Naming strategy configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingConfig {
    /// Strategy for database table names (e.g. "snake_case", "preserve").
    #[serde(default = "default_table_strategy")]
    pub table_strategy: String,
    /// Strategy for column names (e.g. "snake_case", "preserve").
    #[serde(default = "default_column_strategy")]
    pub column_strategy: String,
    /// Strategy for Rust struct names (e.g. "PascalCase", "singularize").
    #[serde(default = "default_struct_strategy")]
    pub struct_strategy: String,
}

fn default_table_strategy() -> String {
    "snake_case".to_string()
}

fn default_column_strategy() -> String {
    "snake_case".to_string()
}

fn default_struct_strategy() -> String {
    "PascalCase".to_string()
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            table_strategy: default_table_strategy(),
            column_strategy: default_column_strategy(),
            struct_strategy: default_struct_strategy(),
        }
    }
}

/// Enum definition within the schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumIr {
    /// Allowed string variant labels in declaration order.
    pub values: Vec<String>,
    /// Optional explicit Rust enum type name override.
    #[serde(default)]
    pub rust_name: Option<String>,
}

/// Table definition within the schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableIr {
    /// Columns belonging to this table, keyed by column name or mapped in order.
    #[serde(default)]
    pub columns: BTreeMap<String, ColumnIr>,
    /// Primary key constraint (composite or single).
    #[serde(default)]
    pub primary_key: Option<PrimaryKeyIr>,
    /// Foreign key constraints on this table.
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyIr>,
    /// Unique constraints across one or more columns.
    #[serde(default)]
    pub unique_constraints: Vec<UniqueConstraintIr>,
    /// Secondary indexes defined on this table.
    #[serde(default)]
    pub indexes: Vec<IndexIr>,
    /// Check constraints applied to this table.
    #[serde(default)]
    pub check_constraints: Vec<CheckConstraintIr>,
    /// Optional rename hint from a previous migration version.
    #[serde(default)]
    pub renamed_from: Option<String>,
    /// Optional explicit Rust model struct name override.
    #[serde(default)]
    pub rust_name: Option<String>,
}

/// Supported column types in Schema IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    Integer,
    Bigint,
    Float,
    Double,
    Boolean,
    Text,
    Timestamptz,
    Uuid,
    Json,
    #[serde(rename = "enum")]
    Enum { name: String },
}

/// Column definition within a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnIr {
    /// Logical column data type.
    pub r#type: ColumnType,
    /// Whether column allows NULL (NOT NULL by default).
    #[serde(default)]
    pub nullable: bool,
    /// Whether column is an autoincrementing integer.
    #[serde(default)]
    pub auto_increment: bool,
    /// Maximum character length constraint for Text columns.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Default value for new records.
    #[serde(default)]
    pub default: Option<DefaultValueIr>,
    /// Optional rename hint from a previous migration version.
    #[serde(default)]
    pub renamed_from: Option<String>,
    /// Optional explicit Rust field name override.
    #[serde(default)]
    pub rust_name: Option<String>,
}

/// Representation of column default values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum DefaultValueIr {
    /// Literal value (string, number, boolean, etc.).
    Literal(serde_json::Value),
    /// Function invocation like current timestamp NOW() / CURRENT_TIMESTAMP.
    Now,
    /// Null default.
    Null,
    /// Raw SQL expression for backends.
    Custom(String),
}

/// Primary key specification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryKeyIr {
    /// Column names that make up the primary key (ordered).
    pub columns: Vec<String>,
    /// Optional explicit constraint name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Referential integrity action for foreign keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReferentialAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl ReferentialAction {
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }
}

/// Foreign key constraint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignKeyIr {
    /// Local column names.
    pub columns: Vec<String>,
    /// Referenced target table name.
    pub target_table: String,
    /// Referenced target column names.
    pub target_columns: Vec<String>,
    /// Action on parent row deletion.
    #[serde(default)]
    pub on_delete: ReferentialAction,
    /// Action on parent row update.
    #[serde(default)]
    pub on_update: ReferentialAction,
    /// Optional explicit constraint name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Unique constraint over a list of columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniqueConstraintIr {
    /// Column names covered by the unique constraint.
    pub columns: Vec<String>,
    /// Optional constraint name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Table index definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexIr {
    /// Explicit or computed index name.
    pub name: String,
    /// Columns participating in the index.
    pub columns: Vec<String>,
    /// Whether the index enforces uniqueness.
    #[serde(default)]
    pub unique: bool,
}

/// Check constraint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckConstraintIr {
    /// Optional constraint name.
    #[serde(default)]
    pub name: Option<String>,
    /// SQL boolean expression.
    pub expression: String,
}
