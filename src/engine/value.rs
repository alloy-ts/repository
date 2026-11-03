use std::sync::Arc;

use napi::bindgen_prelude::{BigInt, JsObjectValue, ToNapiValue, Unknown};
use napi::{Env, JsValue, ValueType};

use crate::engine::errors::EngineError;

/// Safe integer boundary in IEEE 754: 2^53 - 1
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const MIN_SAFE_INTEGER: i64 = -9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq)]
pub enum EngineValue {
    Undef,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<EngineValue>),
}

impl EngineValue {
    pub fn to_fusevm(&self) -> fusevm::Value {
        match self {
            EngineValue::Undef => fusevm::Value::Undef,
            EngineValue::Bool(b) => fusevm::Value::Bool(*b),
            EngineValue::Int(n) => fusevm::Value::Int(*n),
            EngineValue::Float(f) => fusevm::Value::Float(*f),
            EngineValue::Str(s) => fusevm::Value::Str(Arc::new(s.clone())),
            EngineValue::Array(items) => {
                let vals: Vec<fusevm::Value> = items.iter().map(|item| item.to_fusevm()).collect();
                fusevm::Value::Array(Arc::new(vals))
            }
        }
    }

    pub fn from_fusevm(v: &fusevm::Value) -> Self {
        match v {
            fusevm::Value::Undef => EngineValue::Undef,
            fusevm::Value::Bool(b) => EngineValue::Bool(*b),
            fusevm::Value::Int(n) => EngineValue::Int(*n),
            fusevm::Value::Float(f) => EngineValue::Float(*f),
            fusevm::Value::Str(s) => EngineValue::Str(s.as_str().to_string()),
            fusevm::Value::Array(a) => {
                let items = a.iter().map(EngineValue::from_fusevm).collect();
                EngineValue::Array(items)
            }
            _ => EngineValue::Undef,
        }
    }

    pub fn from_js(env: &Env, val: Unknown) -> Result<Self, EngineError> {
        let vtype = val
            .get_type()
            .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;

        match vtype {
            ValueType::Undefined | ValueType::Null => Ok(EngineValue::Undef),
            ValueType::Boolean => {
                let b = val
                    .coerce_to_bool()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                Ok(EngineValue::Bool(b))
            }
            ValueType::Number => {
                let num = val
                    .coerce_to_number()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                let n = num
                    .get_double()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;

                if n.is_finite() && n.fract() == 0.0 && n >= (i64::MIN as f64) && n <= (i64::MAX as f64) {
                    let i = n as i64;
                    // Check for negative zero (-0.0)
                    if n == 0.0 && n.to_bits() == (-0.0f64).to_bits() {
                        Ok(EngineValue::Float(n))
                    } else if (MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) {
                        Ok(EngineValue::Int(i))
                    } else {
                        Ok(EngineValue::Float(n))
                    }
                } else {
                    Ok(EngineValue::Float(n))
                }
            }
            ValueType::BigInt => {
                let bigint = unsafe { val.cast::<BigInt>() }
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                let (i64_val, lossless) = bigint.get_i64();
                if !lossless {
                    return Err(EngineError::range("BigInt out of range for i64"));
                }
                Ok(EngineValue::Int(i64_val))
            }
            ValueType::String => {
                let s = val
                    .coerce_to_string()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?
                    .into_utf8()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?
                    .into_owned()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                Ok(EngineValue::Str(s))
            }
            ValueType::Object => {
                let is_arr = val
                    .is_array()
                    .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                if is_arr {
                    let obj = val
                        .coerce_to_object()
                        .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                    let len = obj
                        .get_array_length()
                        .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                    let mut items = Vec::with_capacity(len as usize);
                    for i in 0..len {
                        let el: Unknown = obj
                            .get_element(i)
                            .map_err(|e: napi::Error| EngineError::runtime(e.to_string()))?;
                        items.push(EngineValue::from_js(env, el)?);
                    }
                    Ok(EngineValue::Array(items))
                } else {
                    Err(EngineError::unsupported_type(
                        "Plain objects are not supported in Engine values",
                    ))
                }
            }
            ValueType::Function => Err(EngineError::unsupported_type(
                "Functions cannot be passed as values",
            )),
            ValueType::Symbol => Err(EngineError::unsupported_type(
                "Symbols are not supported in Engine values",
            )),
            _ => Err(EngineError::unsupported_type("Unsupported JS value type")),
        }
    }

    pub fn to_js<'a>(&self, env: &'a Env) -> Result<Unknown<'a>, napi::Error> {
        match self {
            EngineValue::Undef => {
                let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), ())? };
                Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
            }
            EngineValue::Bool(b) => {
                let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), *b)? };
                Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
            }
            EngineValue::Int(n) => {
                if *n > MAX_SAFE_INTEGER || *n < MIN_SAFE_INTEGER {
                    let bigint = BigInt::from(*n);
                    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), bigint)? };
                    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
                } else {
                    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), *n)? };
                    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
                }
            }
            EngineValue::Float(f) => {
                let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), *f)? };
                Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
            }
            EngineValue::Str(s) => {
                let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), s.as_str())? };
                Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
            }
            EngineValue::Array(arr) => {
                let mut js_arr = env.create_array(arr.len() as u32)?;
                for (i, item) in arr.iter().enumerate() {
                    let js_item = item.to_js(env)?;
                    js_arr.set_element(i as u32, js_item)?;
                }
                let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), js_arr)? };
                Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
            }
        }
    }
}
