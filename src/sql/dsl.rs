use std::sync::Arc;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde_json::Value;

use crate::sql::naming::to_snake_case;

#[derive(Clone, Debug, Default)]
pub struct ColumnMeta {
    pub name: Option<String>,
    pub kind: String,
    pub enum_name: Option<String>,
    pub nullable: bool,
    pub auto_increment: bool,
    pub max_length: Option<u32>,
    pub default_val: Option<Value>,
    pub unique: bool,
    pub primary_key: bool,
    pub ref_table: Option<String>,
    pub ref_column: Option<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

#[napi(js_name = "ColumnBuilder")]
#[derive(Clone)]
pub struct JsColumnBuilder {
    pub(crate) meta: Arc<ColumnMeta>,
}

#[napi]
impl JsColumnBuilder {
    pub(crate) fn with_meta(meta: ColumnMeta) -> Self {
        Self {
            meta: Arc::new(meta),
        }
    }

    #[napi]
    pub fn name(&self, name: String) -> Self {
        let mut m = (*self.meta).clone();
        m.name = Some(name);
        Self::with_meta(m)
    }

    #[napi]
    pub fn nullable(&self) -> Self {
        let mut m = (*self.meta).clone();
        m.nullable = true;
        Self::with_meta(m)
    }

    #[napi]
    pub fn primary_key(&self, opts: Option<serde_json::Value>) -> Self {
        let mut m = (*self.meta).clone();
        m.primary_key = true;
        if let Some(opts) = opts {
            if opts.get("autoIncrement").and_then(|v| v.as_bool()).unwrap_or(false) {
                m.auto_increment = true;
            }
        }
        Self::with_meta(m)
    }

    #[napi]
    pub fn unique(&self) -> Self {
        let mut m = (*self.meta).clone();
        m.unique = true;
        Self::with_meta(m)
    }

    #[napi]
    pub fn max_length(&self, len: f64) -> Result<Self> {
        if len <= 0.0 || len.fract() != 0.0 {
            return Err(Error::from_reason("maxLength expects a positive integer"));
        }
        let mut m = (*self.meta).clone();
        m.max_length = Some(len as u32);
        Ok(Self::with_meta(m))
    }

    #[napi]
    pub fn default(&self, val: Option<serde_json::Value>) -> Self {
        let mut m = (*self.meta).clone();
        match val {
            None => {
                m.default_val = Some(serde_json::json!({ "kind": "null" }));
            }
            Some(v) if v.is_null() => {
                m.default_val = Some(serde_json::json!({ "kind": "null" }));
            }
            Some(v) => {
                m.default_val = Some(serde_json::json!({ "kind": "literal", "value": v }));
            }
        }
        Self::with_meta(m)
    }

    #[napi]
    pub fn default_now(&self) -> Self {
        let mut m = (*self.meta).clone();
        m.default_val = Some(serde_json::json!({ "kind": "now" }));
        Self::with_meta(m)
    }

    #[napi]
    pub fn references(&self, target_ref: serde_json::Value, opts: Option<serde_json::Value>) -> Result<Self> {
        let (ref_table, ref_column) = if let Some(obj) = target_ref.as_object() {
            let table = obj
                .get("$tableName")
                .or_else(|| obj.get("tableName"))
                .or_else(|| obj.get("table"))
                .and_then(|t| t.as_str())
                .ok_or_else(|| Error::from_reason("references() target object missing $tableName"))?;
            let col = obj
                .get("$name")
                .or_else(|| obj.get("name"))
                .or_else(|| obj.get("column"))
                .and_then(|c| c.as_str())
                .ok_or_else(|| Error::from_reason("references() target object missing $name"))?;
            (table.to_string(), col.to_string())
        } else if let Some(s) = target_ref.as_str() {
            let (tbl, col) = s
                .split_once('.')
                .ok_or_else(|| Error::from_reason("references() target string must be table.column"))?;
            (tbl.to_string(), col.to_string())
        } else {
            return Err(Error::from_reason("references() expects target column or string 'table.column'"));
        };

        let mut on_del = "no_action".to_string();
        let mut on_upd = "no_action".to_string();

        if let Some(opts) = opts {
            if let Some(d) = opts.get("onDelete").and_then(|v| v.as_str()) {
                on_del = to_snake_case(d);
            }
            if let Some(u) = opts.get("onUpdate").and_then(|v| v.as_str()) {
                on_upd = to_snake_case(u);
            }
        }

        const VALID_ACTIONS: &[&str] = &["no_action", "restrict", "cascade", "set_null", "set_default"];
        if !VALID_ACTIONS.contains(&on_del.as_str()) || !VALID_ACTIONS.contains(&on_upd.as_str()) {
            return Err(Error::from_reason("unknown referential action"));
        }

        let mut m = (*self.meta).clone();
        m.ref_table = Some(ref_table);
        m.ref_column = Some(ref_column);
        m.on_delete = Some(on_del);
        m.on_update = Some(on_upd);
        Ok(Self::with_meta(m))
    }

    #[napi(getter)]
    pub fn meta(&self) -> serde_json::Value {
        serde_json::json!({
            "isColumnBuilder": true,
            "name": self.meta.name,
            "kind": self.meta.kind,
            "enumName": self.meta.enum_name,
            "nullable": self.meta.nullable,
            "autoIncrement": self.meta.auto_increment,
            "maxLength": self.meta.max_length,
            "default": self.meta.default_val,
            "unique": self.meta.unique,
            "primaryKey": self.meta.primary_key,
            "refTable": self.meta.ref_table,
            "refColumn": self.meta.ref_column,
            "onDelete": self.meta.on_delete,
            "onUpdate": self.meta.on_update,
        })
    }
}

// Column type factory functions exported to JavaScript
#[napi]
pub fn integer() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "integer".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn bigint() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "bigint".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn float() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "float".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn double() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "double".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn text() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "text".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn boolean() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "boolean".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn timestamp() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "timestamp".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn timestamptz() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "timestamptz".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn uuid() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "uuid".to_string(),
        ..Default::default()
    })
}

#[napi]
pub fn json() -> JsColumnBuilder {
    JsColumnBuilder::with_meta(ColumnMeta {
        kind: "json".to_string(),
        ..Default::default()
    })
}

#[napi(js_name = "enumType")]
pub fn enum_type(name: String, values: Vec<String>) -> Result<serde_json::Value> {
    let mut seen = std::collections::HashSet::new();
    for v in &values {
        if !seen.insert(v) {
            return Err(Error::from_reason(format!(
                "enumType(\"{name}\") duplicate values: \"{v}\""
            )));
        }
    }

    Ok(serde_json::json!({
        "name": name,
        "values": values
    }))
}

/// NAPI-compatible table helper that compiles JS column configurations into Table IR JSON
#[napi(js_name = "table", ts_args_type = "tableName: string, columnsObj: Record<string, any>, extraFn?: any")]
pub fn table_builder(
    env: Env,
    table_name: String,
    columns_obj: Object,
    extra_fn: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let keys = Object::keys(&columns_obj)?;
    let mut cols_map = serde_json::Map::new();

    for k in keys {
        if k.starts_with('$') {
            return Err(Error::from_reason(format!("reserved key \"{k}\"")));
        }
        let col_val: Object = columns_obj.get(&k)?.ok_or_else(|| {
            Error::from_reason(format!("missing value for column {k}"))
        })?;

        // Extract metadata whether it's a JsColumnBuilder instance (has .meta property) or a plain object
        let meta_json: serde_json::Value = if let Ok(Some(meta_val)) = col_val.get::<serde_json::Value>("meta") {
            meta_val
        } else if let Ok(v) = env.from_js_value::<serde_json::Value, _>(col_val) {
            v
        } else {
            return Err(Error::from_reason(format!("expected a column builder for key \"{k}\"")));
        };

        cols_map.insert(k, meta_json);
    }

    Ok(serde_json::json!({
        "$tableName": table_name,
        "$columnsObj": serde_json::Value::Object(cols_map),
        "$extraFn": extra_fn,
    }))
}

/// NAPI-compatible schema definition evaluator that produces a validated canonical IR JSON Value
#[napi(js_name = "defineSchema")]
pub fn define_schema(config: serde_json::Value) -> Result<serde_json::Value> {
    let raw_enums = config
        .get("enums")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let raw_tables = config
        .get("tables")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    // 1. Process enums
    let mut enums = Vec::new();
    for e in raw_enums {
        let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let values = e
            .get("values")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        enums.push(serde_json::json!({
            "name": name,
            "values": values
        }));
    }

    // 2. Register table names & detect duplicates
    let mut registered_tables = std::collections::HashSet::new();
    for t in &raw_tables {
        let tbl_name = t
            .get("$tableName")
            .or_else(|| t.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("");
        if !registered_tables.insert(tbl_name.to_string()) {
            return Err(Error::from_reason(format!("duplicate table names: \"{tbl_name}\"")));
        }
    }

    // 3. Transform tables into canonical TableIr JSON
    let mut tables = Vec::new();
    for t in raw_tables {
        let tbl_name = t
            .get("$tableName")
            .or_else(|| t.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        let cols_map = t
            .get("$columnsObj")
            .or_else(|| t.get("columns"))
            .and_then(|c| c.as_object())
            .cloned()
            .unwrap_or_default();

        let mut columns = Vec::new();
        let mut col_pk_names = Vec::new();
        let mut uniques = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut indexes = Vec::new();
        let mut key_to_col_name = std::collections::HashMap::new();

        for (key, col_val) in &cols_map {
            let meta = if let Some(m) = col_val.get("meta") {
                m.clone()
            } else {
                col_val.clone()
            };

            let col_name = meta
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| to_snake_case(key));

            key_to_col_name.insert(key.clone(), col_name.clone());

            let kind = meta
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("text");

            let col_type = if kind == "enum" {
                let enum_name = meta
                    .get("enumName")
                    .or_else(|| meta.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("enum");
                serde_json::json!({
                    "enum": {
                        "name": enum_name
                    }
                })
            } else {
                serde_json::Value::String(kind.to_string())
            };

            let nullable = meta.get("nullable").and_then(|v| v.as_bool()).unwrap_or(false);
            let auto_increment = meta.get("autoIncrement").and_then(|v| v.as_bool()).unwrap_or(false);
            let max_length = meta.get("maxLength").and_then(|v| v.as_u64());

            if meta.get("primaryKey").and_then(|v| v.as_bool()).unwrap_or(false) {
                col_pk_names.push(col_name.clone());
            }

            if meta.get("unique").and_then(|v| v.as_bool()).unwrap_or(false) {
                uniques.push(serde_json::json!({
                    "name": format!("{tbl_name}_{col_name}_key"),
                    "columns": [col_name.clone()]
                }));
            }

            if let Some(ref_tbl) = meta.get("refTable").and_then(|v| v.as_str()) {
                let ref_col = meta.get("refColumn").and_then(|v| v.as_str()).unwrap_or("id");
                let on_del = meta.get("onDelete").and_then(|v| v.as_str()).unwrap_or("no_action");
                let on_upd = meta.get("onUpdate").and_then(|v| v.as_str()).unwrap_or("no_action");

                foreign_keys.push(serde_json::json!({
                    "name": format!("{tbl_name}_{col_name}_fkey"),
                    "columns": [col_name.clone()],
                    "targetTable": ref_tbl,
                    "targetColumns": [ref_col],
                    "onDelete": on_del.to_uppercase(),
                    "onUpdate": on_upd.to_uppercase(),
                }));
            }

            let mut col_json = serde_json::json!({
                "key": key,
                "name": col_name,
                "type": col_type,
                "nullable": nullable,
                "autoIncrement": auto_increment,
            });

            if let Some(ml) = max_length {
                col_json["maxLength"] = serde_json::json!(ml);
            }

            if let Some(def) = meta.get("default") {
                if !def.is_null() {
                    col_json["default"] = def.clone();
                }
            }

            columns.push(col_json);
        }

        // Handle extraFn table-level definitions if present as an array
        if let Some(extra_arr) = t.get("$extraFn").and_then(|e| e.as_array()) {
            for item in extra_arr {
                let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let cols = item
                    .get("columns")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .map(|k| key_to_col_name.get(&k).cloned().unwrap_or(k))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                if kind == "index" {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("{tbl_name}_{}_idx", cols.join("_")));
                    indexes.push(serde_json::json!({
                        "name": name,
                        "columns": cols
                    }));
                } else if kind == "unique" {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("{tbl_name}_{}_key", cols.join("_")));
                    uniques.push(serde_json::json!({
                        "name": name,
                        "columns": cols
                    }));
                }
            }
        }

        tables.push(serde_json::json!({
            "name": tbl_name,
            "columns": columns,
            "primaryKey": col_pk_names,
            "uniques": uniques,
            "indexes": indexes,
            "foreignKeys": foreign_keys
        }));
    }

    Ok(serde_json::json!({
        "irVersion": 1,
        "enums": enums,
        "tables": tables,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_builders_immutability() {
        let a = integer();
        let b = a.nullable();
        assert!(!a.meta.nullable);
        assert!(b.meta.nullable);
    }

    #[test]
    fn test_column_builders_methods() {
        let col = text().max_length(100.0).unwrap().unique();
        assert_eq!(col.meta.max_length, Some(100));
        assert!(col.meta.unique);
    }

    #[test]
    fn test_max_length_validation() {
        assert!(text().max_length(-1.0).is_err());
        assert!(text().max_length(0.0).is_err());
        assert!(text().max_length(1.5).is_err());
        assert!(text().max_length(10.0).is_ok());
    }

    #[test]
    fn test_table_and_define_schema_builders() {
        let cols = serde_json::json!({
            "id": { "kind": "integer", "nullable": false, "autoIncrement": true, "primaryKey": true },
            "name": { "kind": "text", "nullable": false }
        });
        let t = serde_json::json!({
            "$tableName": "users",
            "$columnsObj": cols,
            "$extraFn": null
        });

        let schema = define_schema(serde_json::json!({
            "tables": [t]
        }))
        .unwrap();
        assert_eq!(schema["irVersion"], 1);
        assert_eq!(schema["tables"][0]["name"], "users");
        assert_eq!(schema["tables"][0]["primaryKey"][0], "id");
    }
}
