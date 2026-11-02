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
                .and_then(|t| t.as_str())
                .ok_or_else(|| Error::from_reason("references() target object missing $tableName"))?;
            let col = obj
                .get("$name")
                .and_then(|c| c.as_str())
                .ok_or_else(|| Error::from_reason("references() target object missing $name"))?;
            (table.to_string(), col.to_string())
        } else if let Some(s) = target_ref.as_str() {
            let (tbl, col) = s
                .split_once('.')
                .ok_or_else(|| Error::from_reason("references() target string must be table.column"))?;
            (tbl.to_string(), col.to_string())
        } else {
            return Err(Error::from_reason("references() expects target column or thunk result"));
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
        "$isEnumFactory": true,
        "name": name,
        "values": values
    }))
}

/// NAPI-compatible table helper that compiles JS column configurations into Table IR JSON
#[napi(js_name = "table")]
pub fn table_builder(
    env: Env,
    table_name: String,
    columns_obj: Object,
    _extra_fn: Option<Unknown>,
) -> Result<serde_json::Value> {
    let prop_names = columns_obj.get_property_names()?;
    let len = prop_names.get_array_length()?;
    let mut cols_val = serde_json::Map::new();

    for i in 0..len {
        let key: String = prop_names.get_element(i)?;
        if key.starts_with('$') {
            return Err(Error::from_reason(format!("reserved key \"{key}\"")));
        }
        let val: Unknown = columns_obj.get_named_property(&key)?;
        if val.get_type()? == ValueType::Object {
            let obj = val.coerce_to_object()?;
            if let Ok(meta_val) = obj.get_named_property::<Unknown>("meta") {
                let json_val: serde_json::Value = env.from_js_value(meta_val).unwrap_or(serde_json::Value::Null);
                cols_val.insert(key, json_val);
                continue;
            }
        }
        let json_val: serde_json::Value = env.from_js_value(val).unwrap_or(serde_json::Value::Null);
        cols_val.insert(key, json_val);
    }

    Ok(serde_json::json!({
        "$tableName": table_name,
        "$columnsObj": cols_val,
        "$extraFn": serde_json::Value::Null,
    }))
}

/// NAPI-compatible schema definition evaluator that produces a validated IR JSON Value
#[napi(js_name = "defineSchema")]
pub fn define_schema(config: serde_json::Value) -> Result<serde_json::Value> {
    let enums_arr = config
        .get("enums")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    let tables_arr = config
        .get("tables")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let mut enums_out = Vec::new();
    for e in enums_arr {
        let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let values = e.get("values").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        enums_out.push(serde_json::json!({
            "name": name,
            "values": values
        }));
    }

    let mut table_names = std::collections::HashSet::new();
    let mut tables_out = Vec::new();

    for t in tables_arr {
        let table_name = t
            .get("$tableName")
            .or_else(|| t.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        if !table_names.insert(table_name.clone()) {
            return Err(Error::from_reason(format!("duplicate table names: \"{table_name}\"")));
        }

        let mut columns_out = Vec::new();
        let mut primary_key_cols = Vec::new();
        let mut uniques_out = Vec::new();
        let mut foreign_keys_out = Vec::new();

        if let Some(cols_obj) = t.get("$columnsObj").and_then(|c| c.as_object()) {
            for (col_key, col_val) in cols_obj {
                let name = col_val
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| to_snake_case(col_key));
                let kind = col_val
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("text");
                let nullable = col_val
                    .get("nullable")
                    .and_then(|n| n.as_bool())
                    .unwrap_or(false);
                let auto_increment = col_val
                    .get("autoIncrement")
                    .and_then(|a| a.as_bool())
                    .unwrap_or(false);
                let max_length = col_val.get("maxLength").and_then(|m| m.as_u64());
                let default_val = col_val.get("default").cloned();
                let is_pk = col_val
                    .get("primaryKey")
                    .and_then(|p| p.as_bool())
                    .unwrap_or(false);
                let is_unq = col_val
                    .get("unique")
                    .and_then(|u| u.as_bool())
                    .unwrap_or(false);

                if is_pk {
                    primary_key_cols.push(name.clone());
                }
                if is_unq {
                    uniques_out.push(serde_json::json!({
                        "name": format!("{table_name}_{name}_key"),
                        "columns": [name.clone()]
                    }));
                }

                if let Some(ref_table) = col_val.get("refTable").and_then(|r| r.as_str()) {
                    let ref_column = col_val
                        .get("refColumn")
                        .and_then(|r| r.as_str())
                        .unwrap_or("id");
                    let on_delete = col_val
                        .get("onDelete")
                        .and_then(|r| r.as_str())
                        .unwrap_or("no_action");
                    let on_update = col_val
                        .get("onUpdate")
                        .and_then(|r| r.as_str())
                        .unwrap_or("no_action");

                    foreign_keys_out.push(serde_json::json!({
                        "name": format!("{table_name}_{name}_fkey"),
                        "columns": [name.clone()],
                        "refTable": ref_table,
                        "refColumns": [ref_column],
                        "onDelete": on_delete,
                        "onUpdate": on_update
                    }));
                }

                let mut col_item = serde_json::json!({
                    "key": col_key,
                    "name": name,
                    "type": { "kind": kind },
                    "nullable": nullable,
                    "autoIncrement": auto_increment,
                });

                if let Some(m) = max_length {
                    col_item["maxLength"] = serde_json::json!(m);
                }
                if let Some(d) = default_val {
                    if !d.is_null() {
                        col_item["default"] = d;
                    }
                }
                columns_out.push(col_item);
            }
        } else if let Some(existing_cols) = t.get("columns").and_then(|c| c.as_array()) {
            columns_out = existing_cols.clone();
            if let Some(pk) = t.get("primaryKey").and_then(|p| p.as_array()) {
                primary_key_cols = pk.iter().filter_map(|s| s.as_str().map(|str| str.to_string())).collect();
            }
        }

        tables_out.push(serde_json::json!({
            "name": table_name,
            "columns": columns_out,
            "primaryKey": primary_key_cols,
            "uniques": uniques_out,
            "indexes": [],
            "foreignKeys": foreign_keys_out
        }));
    }

    Ok(serde_json::json!({
        "irVersion": 1,
        "enums": enums_out,
        "tables": tables_out,
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
    fn test_define_schema_builders() {
        let t = serde_json::json!({
            "$tableName": "users",
            "columns": {
                "id": { "kind": "integer", "nullable": false, "autoIncrement": true },
                "name": { "kind": "text", "nullable": false }
            }
        });
        let schema = define_schema(serde_json::json!({
            "tables": [t]
        }))
        .unwrap();
        assert_eq!(schema["irVersion"], 1);
    }
}
