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
}
