use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Version identifier for the IR format to protect against format mismatches.
pub const CURRENT_IR_VERSION: u32 = 1;
pub const SUPPORTED_IR_VERSION: u32 = CURRENT_IR_VERSION;

/// Top-level schema Intermediate Representation (IR).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SchemaIr {
    /// Monotonically increasing schema IR version integer.
    #[serde(alias = "ir_version")]
    pub ir_version: u32,
    /// Schema naming convention preferences.
    #[serde(default)]
    pub naming: NamingConfig,
    /// Registered enums keyed by their logical enum name or vector of definitions.
    #[serde(default, deserialize_with = "deserialize_enums")]
    pub enums: BTreeMap<String, EnumIr>,
    /// Tables keyed by name or list of tables.
    #[serde(default, deserialize_with = "deserialize_tables")]
    pub tables: BTreeMap<String, TableIr>,
}

fn deserialize_enums<'de, D>(deserializer: D) -> Result<BTreeMap<String, EnumIr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (k, v) in map {
                let item: EnumIr = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                out.insert(k, item);
            }
            Ok(out)
        }
        serde_json::Value::Array(list) => {
            let mut out = BTreeMap::new();
            for v in list {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct EnumDefItem {
                    name: String,
                    values: Vec<String>,
                    #[serde(default, alias = "rust_name")]
                    rust_name: Option<String>,
                }
                let item: EnumDefItem = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                out.insert(
                    item.name,
                    EnumIr {
                        values: item.values,
                        rust_name: item.rust_name,
                    },
                );
            }
            Ok(out)
        }
        _ => Err(serde::de::Error::custom("expected object or array for enums")),
    }
}

fn deserialize_tables<'de, D>(deserializer: D) -> Result<BTreeMap<String, TableIr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (k, v) in map {
                let item: TableIr = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                out.insert(k, item);
            }
            Ok(out)
        }
        serde_json::Value::Array(list) => {
            let mut out = BTreeMap::new();
            for v in list {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct TableItem {
                    name: String,
                    #[serde(default, deserialize_with = "deserialize_columns")]
                    columns: BTreeMap<String, ColumnIr>,
                    #[serde(default, alias = "primary_key", deserialize_with = "deserialize_primary_key")]
                    primary_key: Option<PrimaryKeyIr>,
                    #[serde(default, alias = "foreign_keys", alias = "foreignKeys")]
                    foreign_keys: Vec<ForeignKeyIr>,
                    #[serde(default, alias = "uniques", alias = "unique_constraints", alias = "uniqueConstraints")]
                    unique_constraints: Vec<UniqueConstraintIr>,
                    #[serde(default)]
                    indexes: Vec<IndexIr>,
                    #[serde(default, alias = "check_constraints", alias = "checkConstraints")]
                    check_constraints: Vec<CheckConstraintIr>,
                    #[serde(default, alias = "renamed_from")]
                    renamed_from: Option<String>,
                    #[serde(default, alias = "rust_name")]
                    rust_name: Option<String>,
                }
                let item: TableItem = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                out.insert(
                    item.name,
                    TableIr {
                        columns: item.columns,
                        primary_key: item.primary_key,
                        foreign_keys: item.foreign_keys,
                        unique_constraints: item.unique_constraints,
                        indexes: item.indexes,
                        check_constraints: item.check_constraints,
                        renamed_from: item.renamed_from,
                        rust_name: item.rust_name,
                    },
                );
            }
            Ok(out)
        }
        _ => Err(serde::de::Error::custom("expected object or array for tables")),
    }
}

/// Naming strategy configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NamingConfig {
    /// Strategy for database table names (e.g. "snake_case", "preserve").
    #[serde(default = "default_table_strategy", alias = "table_strategy")]
    pub table_strategy: String,
    /// Strategy for column names (e.g. "snake_case", "preserve").
    #[serde(default = "default_column_strategy", alias = "column_strategy")]
    pub column_strategy: String,
    /// Strategy for Rust struct names (e.g. "PascalCase", "singularize").
    #[serde(default = "default_struct_strategy", alias = "struct_strategy")]
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnumIr {
    /// Allowed string variant labels in declaration order.
    pub values: Vec<String>,
    /// Optional explicit Rust enum type name override.
    #[serde(default, alias = "rust_name")]
    pub rust_name: Option<String>,
}

/// Table definition within the schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TableIr {
    /// Columns belonging to this table, keyed by column name or mapped in order.
    #[serde(default, deserialize_with = "deserialize_columns")]
    pub columns: BTreeMap<String, ColumnIr>,
    /// Primary key constraint (composite or single).
    #[serde(default, alias = "primary_key", deserialize_with = "deserialize_primary_key")]
    pub primary_key: Option<PrimaryKeyIr>,
    /// Foreign key constraints on this table.
    #[serde(default, alias = "foreign_keys", alias = "foreignKeys")]
    pub foreign_keys: Vec<ForeignKeyIr>,
    /// Unique constraints across one or more columns.
    #[serde(default, alias = "uniques", alias = "unique_constraints", alias = "uniqueConstraints")]
    pub unique_constraints: Vec<UniqueConstraintIr>,
    /// Secondary indexes defined on this table.
    #[serde(default)]
    pub indexes: Vec<IndexIr>,
    /// Check constraints applied to this table.
    #[serde(default, alias = "check_constraints", alias = "checkConstraints")]
    pub check_constraints: Vec<CheckConstraintIr>,
    /// Optional rename hint from a previous migration version.
    #[serde(default, alias = "renamed_from")]
    pub renamed_from: Option<String>,
    /// Optional explicit Rust model struct name override.
    #[serde(default, alias = "rust_name")]
    pub rust_name: Option<String>,
}

fn deserialize_columns<'de, D>(deserializer: D) -> Result<BTreeMap<String, ColumnIr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => {
            let mut out = BTreeMap::new();
            for (k, v) in map {
                let mut item: ColumnIr = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                if item.key.is_empty() {
                    item.key = k.clone();
                }
                out.insert(k, item);
            }
            Ok(out)
        }
        serde_json::Value::Array(list) => {
            let mut out = BTreeMap::new();
            for v in list {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct ColumnItem {
                    #[serde(default)]
                    key: Option<String>,
                    name: String,
                    #[serde(alias = "ty")]
                    r#type: ColumnType,
                    #[serde(default)]
                    nullable: bool,
                    #[serde(default, alias = "auto_increment")]
                    auto_increment: bool,
                    #[serde(default, alias = "max_length")]
                    max_length: Option<usize>,
                    #[serde(default)]
                    default: Option<DefaultValueIr>,
                    #[serde(default, alias = "renamed_from")]
                    renamed_from: Option<String>,
                    #[serde(default, alias = "rust_name")]
                    rust_name: Option<String>,
                }
                let item: ColumnItem = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
                out.insert(
                    item.name.clone(),
                    ColumnIr {
                        key: item.key.unwrap_or_else(|| item.name.clone()),
                        r#type: item.r#type,
                        nullable: item.nullable,
                        auto_increment: item.auto_increment,
                        max_length: item.max_length,
                        default: item.default,
                        renamed_from: item.renamed_from,
                        rust_name: item.rust_name,
                    },
                );
            }
            Ok(out)
        }
        _ => Err(serde::de::Error::custom("expected object or array for columns")),
    }
}

fn deserialize_primary_key<'de, D>(deserializer: D) -> Result<Option<PrimaryKeyIr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(arr)) => {
            let mut cols = Vec::new();
            for c in arr {
                if let serde_json::Value::String(s) = c {
                    cols.push(s);
                }
            }
            if cols.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PrimaryKeyIr {
                    columns: cols,
                    name: None,
                }))
            }
        }
        Some(serde_json::Value::Object(_)) => {
            let obj: PrimaryKeyIr = serde_json::from_value(value.unwrap()).map_err(serde::de::Error::custom)?;
            Ok(Some(obj))
        }
        _ => Err(serde::de::Error::custom("invalid primary key representation")),
    }
}

/// Supported column types in Schema IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Integer,
    Bigint,
    Float,
    Double,
    Boolean,
    Text,
    Timestamp,
    Timestamptz,
    Uuid,
    Json,
    Enum { name: String },
}

impl Serialize for ColumnType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Integer => serializer.serialize_str("integer"),
            Self::Bigint => serializer.serialize_str("bigint"),
            Self::Float => serializer.serialize_str("float"),
            Self::Double => serializer.serialize_str("double"),
            Self::Boolean => serializer.serialize_str("boolean"),
            Self::Text => serializer.serialize_str("text"),
            Self::Timestamp => serializer.serialize_str("timestamp"),
            Self::Timestamptz => serializer.serialize_str("timestamptz"),
            Self::Uuid => serializer.serialize_str("uuid"),
            Self::Json => serializer.serialize_str("json"),
            Self::Enum { name } => {
                #[derive(Serialize)]
                struct EnumWrap<'a> {
                    name: &'a str,
                }
                #[derive(Serialize)]
                struct TypeWrap<'a> {
                    r#enum: EnumWrap<'a>,
                }
                TypeWrap {
                    r#enum: EnumWrap { name },
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ColumnType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::String(s) => match s.to_lowercase().as_str() {
                "integer" => Ok(Self::Integer),
                "bigint" => Ok(Self::Bigint),
                "float" => Ok(Self::Float),
                "double" => Ok(Self::Double),
                "boolean" => Ok(Self::Boolean),
                "text" | "string" => Ok(Self::Text),
                "timestamp" => Ok(Self::Timestamp),
                "timestamptz" => Ok(Self::Timestamptz),
                "uuid" => Ok(Self::Uuid),
                "json" => Ok(Self::Json),
                other => Ok(Self::Enum {
                    name: other.to_string(),
                }),
            },
            serde_json::Value::Object(map) => {
                if let Some(enum_val) = map.get("enum") {
                    if let Some(name_str) = enum_val.get("name").and_then(|n| n.as_str()) {
                        return Ok(Self::Enum {
                            name: name_str.to_string(),
                        });
                    }
                    if let Some(s) = enum_val.as_str() {
                        return Ok(Self::Enum {
                            name: s.to_string(),
                        });
                    }
                }
                if let Some(name_val) = map.get("name").and_then(|n| n.as_str()) {
                    return Ok(Self::Enum {
                        name: name_val.to_string(),
                    });
                }
                if let Some(kind_val) = map.get("kind").and_then(|k| k.as_str()) {
                    match kind_val.to_lowercase().as_str() {
                        "integer" => return Ok(Self::Integer),
                        "bigint" => return Ok(Self::Bigint),
                        "float" => return Ok(Self::Float),
                        "double" => return Ok(Self::Double),
                        "boolean" => return Ok(Self::Boolean),
                        "text" => return Ok(Self::Text),
                        "timestamp" => return Ok(Self::Timestamp),
                        "timestamptz" => return Ok(Self::Timestamptz),
                        "uuid" => return Ok(Self::Uuid),
                        "json" => return Ok(Self::Json),
                        "enum" => {
                            let name = map
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("enum")
                                .to_string();
                            return Ok(Self::Enum { name });
                        }
                        _ => {}
                    }
                }
                Err(serde::de::Error::custom(format!(
                    "unrecognized column type object: {:?}",
                    map
                )))
            }
            _ => Err(serde::de::Error::custom("expected string or object for column type")),
        }
    }
}

impl ColumnType {
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Integer | Self::Bigint)
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text)
    }

    pub fn is_timestamp(&self) -> bool {
        matches!(self, Self::Timestamp | Self::Timestamptz)
    }

    pub fn enum_name(&self) -> Option<&str> {
        match self {
            Self::Enum { name } => Some(name),
            _ => None,
        }
    }
}

/// Column definition within a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ColumnIr {
    /// JS key in the schema object, defaults to column name.
    #[serde(default)]
    pub key: String,
    /// Logical column data type.
    #[serde(alias = "ty")]
    pub r#type: ColumnType,
    /// Whether column allows NULL (NOT NULL by default).
    #[serde(default)]
    pub nullable: bool,
    /// Whether column is an autoincrementing integer.
    #[serde(default, alias = "auto_increment")]
    pub auto_increment: bool,
    /// Maximum character length constraint for Text columns.
    #[serde(default, alias = "max_length")]
    pub max_length: Option<usize>,
    /// Default value for new records.
    #[serde(default)]
    pub default: Option<DefaultValueIr>,
    /// Optional rename hint from a previous migration version.
    #[serde(default, alias = "renamed_from")]
    pub renamed_from: Option<String>,
    /// Optional explicit Rust field name override.
    #[serde(default, alias = "rust_name")]
    pub rust_name: Option<String>,
}

impl ColumnIr {
    pub fn new(ty: ColumnType) -> Self {
        Self {
            key: String::new(),
            r#type: ty,
            nullable: false,
            auto_increment: false,
            max_length: None,
            default: None,
            renamed_from: None,
            rust_name: None,
        }
    }
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ForeignKeyIr {
    /// Local column names.
    pub columns: Vec<String>,
    /// Referenced target table name.
    #[serde(alias = "target_table", alias = "ref_table", alias = "targetTable", alias = "refTable")]
    pub target_table: String,
    /// Referenced target column names.
    #[serde(alias = "target_columns", alias = "ref_columns", alias = "targetColumns", alias = "refColumns")]
    pub target_columns: Vec<String>,
    /// Action on parent row deletion.
    #[serde(default, alias = "on_delete")]
    pub on_delete: ReferentialAction,
    /// Action on parent row update.
    #[serde(default, alias = "on_update")]
    pub on_update: ReferentialAction,
    /// Optional explicit constraint name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Unique constraint over a list of columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UniqueConstraintIr {
    /// Column names covered by the unique constraint.
    pub columns: Vec<String>,
    /// Optional constraint name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Table index definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CheckConstraintIr {
    /// Optional constraint name.
    #[serde(default)]
    pub name: Option<String>,
    /// SQL boolean expression.
    pub expression: String,
}
