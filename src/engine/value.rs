use fusevm::Value;
use napi::bindgen_prelude::*;

#[derive(Debug, Clone)]
pub enum EngineValue {
    Undef,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl EngineValue {
    pub fn from_fusevm(v: &Value) -> Self {
        match v {
            Value::Undef => EngineValue::Undef,
            Value::Bool(b) => EngineValue::Bool(*b),
            Value::Int(n) => EngineValue::Int(*n),
            Value::Float(f) => EngineValue::Float(*f),
            Value::Str(s) => EngineValue::String(s.to_string()),
            other => EngineValue::String(other.to_str()),
        }
    }

    pub fn to_fusevm(&self) -> Value {
        match self {
            EngineValue::Undef => Value::Undef,
            EngineValue::Bool(b) => Value::Bool(*b),
            EngineValue::Int(n) => Value::Int(*n),
            EngineValue::Float(f) => Value::Float(*f),
            EngineValue::String(s) => Value::str(s),
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            EngineValue::Undef => "undefined".to_string(),
            EngineValue::Bool(b) => b.to_string(),
            EngineValue::Int(n) => n.to_string(),
            EngineValue::Float(f) => f.to_string(),
            EngineValue::String(s) => s.clone(),
        }
    }
}
