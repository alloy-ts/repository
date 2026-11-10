use crate::sql::backend::Backend;
use crate::sql::ir::SchemaIr;
use crate::sql::naming::{
    resolve_enum_type_name, resolve_enum_variant_name, resolve_field_name, resolve_new_struct_name,
    resolve_struct_name,
};

/// Hook for later emitters (Diesel, NAPI-RS) to attach derives and attributes
/// so type mapping and naming live in exactly one place.
pub trait ModelDecorator {
    /// Additional attributes placed on enum definitions.
    fn enum_attrs(&self, _enum_name: &str) -> String {
        String::new()
    }

    /// Additional attributes placed on Row struct definitions.
    fn row_attrs(&self, _table_name: &str) -> String {
        String::new()
    }

    /// Additional attributes placed on NewX insert struct definitions.
    fn new_attrs(&self, _table_name: &str) -> String {
        String::new()
    }
}

/// Default empty decorator.
pub struct NoDecorators;
impl ModelDecorator for NoDecorators {}

/// Options controlling model generation.
#[derive(Clone, Debug, Default)]
pub struct ModelEmitterOptions {
    /// Emit serde Serialize and Deserialize derives.
    pub serde: bool,
    /// Emit Diesel Queryable, Selectable, Insertable derives.
    pub diesel: bool,
    /// Include NAPI #[napi(object)] derives.
    pub napi: bool,
}

impl ModelDecorator for ModelEmitterOptions {
    fn enum_attrs(&self, enum_name: &str) -> String {
        let mut attrs = String::new();
        if self.napi {
            attrs.push_str("#[napi(string_enum)]\n");
        }
        if self.diesel {
            let rust_name = resolve_enum_type_name(enum_name, None);
            attrs.push_str(&format!(
                "#[ExistingTypePath = \"crate::sql_types::{rust_name}\"]\n"
            ));
        }
        attrs
    }

    fn row_attrs(&self, table_name: &str) -> String {
        let mut attrs = String::new();
        if self.diesel {
            attrs.push_str(&format!(
                "#[diesel(table_name = crate::schema::{table_name})]\n"
            ));
        }
        if self.napi {
            attrs.push_str("#[napi(object)]\n");
        }
        attrs
    }

    fn new_attrs(&self, table_name: &str) -> String {
        let mut attrs = String::new();
        if self.diesel {
            attrs.push_str(&format!(
                "#[diesel(table_name = crate::schema::{table_name})]\n"
            ));
        }
        if self.napi {
            attrs.push_str("#[napi(object)]\n");
        }
        attrs
    }
}

/// Emit Rust models using a pluggable decorator hook.
pub fn emit_models_with_decorator<B: Backend, D: ModelDecorator>(
    schema: &SchemaIr,
    backend: &B,
    opts: &ModelEmitterOptions,
    decorator: &D,
) -> String {
    let mut out = String::new();
    out.push_str("//! Auto-generated database models from validated Schema IR.\n\n");

    // Enums
    for (enum_name, enum_def) in &schema.enums {
        let rust_enum_name = resolve_enum_type_name(enum_name, enum_def.rust_name.as_deref());
        out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash");
        if opts.serde {
            out.push_str(", serde::Serialize, serde::Deserialize");
        }
        if opts.diesel && backend.name() == "postgres" {
            out.push_str(", diesel_derive_enum::DbEnum");
        }
        out.push_str(")]\n");

        let custom_attrs = decorator.enum_attrs(enum_name);
        if !custom_attrs.is_empty() {
            out.push_str(&custom_attrs);
        }

        out.push_str(&format!("pub enum {rust_enum_name} {{\n"));
        for val in &enum_def.values {
            let variant = resolve_enum_variant_name(val);
            if opts.serde {
                out.push_str(&format!("    #[serde(rename = \"{val}\")]\n"));
            }
            out.push_str(&format!("    {variant},\n"));
        }
        out.push_str("}\n\n");

        // Display and as_str / FromStr implementations
        out.push_str(&format!("impl {rust_enum_name} {{\n"));
        out.push_str("    /// The database / JSON representation of this variant.\n");
        out.push_str("    pub const fn as_str(&self) -> &'static str {\n");
        out.push_str("        match self {\n");
        for val in &enum_def.values {
            let variant = resolve_enum_variant_name(val);
            out.push_str(&format!("            Self::{variant} => \"{val}\",\n"));
        }
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");

        out.push_str(&format!("impl std::fmt::Display for {rust_enum_name} {{\n"));
        out.push_str("    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n");
        out.push_str("        write!(f, \"{}\", self.as_str())\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    // Tables
    for (table_name, table) in &schema.tables {
        let struct_name = resolve_struct_name(table_name, table.rust_name.as_deref());
        let new_struct_name = resolve_new_struct_name(table_name, table.rust_name.as_deref());

        // Row struct
        out.push_str(&format!("/// Row of table `{table_name}`.\n"));
        out.push_str("#[derive(Debug, Clone, PartialEq");
        if opts.serde {
            out.push_str(", serde::Serialize, serde::Deserialize");
        }
        if opts.diesel {
            out.push_str(", diesel::Queryable, diesel::Selectable");
        }
        out.push_str(")]\n");

        let row_attrs = decorator.row_attrs(table_name);
        if !row_attrs.is_empty() {
            out.push_str(&row_attrs);
        }

        out.push_str(&format!("pub struct {struct_name} {{\n"));
        for (col_name, col) in &table.columns {
            let field_name = resolve_field_name(col_name, col.rust_name.as_deref());
            let rust_type = backend.rust_type(col);
            if opts.serde && col_name != &field_name {
                out.push_str(&format!("    #[serde(rename = \"{col_name}\")]\n"));
            }
            out.push_str(&format!("    pub {field_name}: {rust_type},\n"));
        }
        out.push_str("}\n\n");

        // Insert struct (NewX)
        out.push_str(&format!("/// Values for inserting into `{table_name}`; omitted optional fields use database defaults.\n"));
        out.push_str("#[derive(Debug, Clone, PartialEq");
        if opts.serde {
            out.push_str(", serde::Serialize, serde::Deserialize");
        }
        if opts.diesel {
            out.push_str(", diesel::Insertable");
        }
        out.push_str(")]\n");

        let new_attrs = decorator.new_attrs(table_name);
        if !new_attrs.is_empty() {
            out.push_str(&new_attrs);
        }

        out.push_str(&format!("pub struct {new_struct_name} {{\n"));
        for (col_name, col) in &table.columns {
            let field_name = resolve_field_name(col_name, col.rust_name.as_deref());
            let base_type = backend.rust_type(col);

            // Columns with default or auto-increment become Option in NewX
            let has_default_or_autoincrement = col.auto_increment || col.default.is_some();
            let insert_type = if has_default_or_autoincrement && !col.nullable {
                format!("Option<{base_type}>")
            } else {
                base_type
            };

            if opts.serde {
                if col_name != &field_name {
                    out.push_str(&format!("    #[serde(rename = \"{col_name}\")]\n"));
                }
                if has_default_or_autoincrement || col.nullable {
                    out.push_str(
                        "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n",
                    );
                }
            }
            out.push_str(&format!("    pub {field_name}: {insert_type},\n"));
        }
        out.push_str("}\n\n");
    }

    out
}

/// Emit Rust models (Row struct, NewX insert struct, and enums).
pub fn emit_models<B: Backend>(
    schema: &SchemaIr,
    backend: &B,
    opts: &ModelEmitterOptions,
) -> String {
    emit_models_with_decorator(schema, backend, opts, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::backend::SqliteBackend;
    use crate::sql::ir::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_emit_models_sqlite() {
        let mut tables = BTreeMap::new();
        let mut users_cols = BTreeMap::new();
        let mut id_col = ColumnIr::new(ColumnType::Integer);
        id_col.auto_increment = true;
        users_cols.insert("id".to_string(), id_col);

        let mut name_col = ColumnIr::new(ColumnType::Text);
        name_col.max_length = Some(100);
        users_cols.insert("name".to_string(), name_col);

        let mut age_col = ColumnIr::new(ColumnType::Integer);
        age_col.nullable = true;
        users_cols.insert("age".to_string(), age_col);

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

        let schema = SchemaIr {
            ir_version: 1,
            naming: NamingConfig::default(),
            enums: BTreeMap::new(),
            tables,
        };

        let generated = emit_models(
            &schema,
            &SqliteBackend,
            &ModelEmitterOptions {
                serde: true,
                diesel: true,
                napi: true,
            },
        );

        assert!(generated.contains("pub struct User {"));
        assert!(generated.contains("pub id: i32,"));
        assert!(generated.contains("pub name: String,"));
        assert!(generated.contains("pub age: Option<i32>,"));
        assert!(generated.contains("pub struct NewUser {"));
        assert!(generated.contains("pub id: Option<i32>,")); // auto_increment -> Option in NewUser
        assert!(generated.contains("skip_serializing_if = \"Option::is_none\""));
    }
}
