use std::collections::HashMap;
use fusevm::Value;
use napi::bindgen_prelude::*;
use napi::Env;

/// Typed Engine values: decoupling pure language values from NAPI and FuseVM internals.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EngineValue {
    Undef,
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<EngineValue>),
    Object(HashMap<String, EngineValue>),
}

impl EngineValue {
    pub fn to_fusevm(&self) -> Value {
        match self {
            EngineValue::Undef => Value::Undef,
            EngineValue::Null => Value::Undef,
            EngineValue::Bool(b) => Value::bool(*b),
            EngineValue::Int(i) => Value::int(*i),
            EngineValue::Float(f) => Value::float(*f),
            EngineValue::String(s) => Value::str(s.clone()),
            EngineValue::Array(items) => {
                let vm_items: Vec<Value> = items.iter().map(|item| item.to_fusevm()).collect();
                Value::array(vm_items)
            }
            EngineValue::Object(map) => {
                let mut hash = HashMap::new();
                for (k, v) in map {
                    hash.insert(k.clone(), v.to_fusevm());
                }
                Value::Hash(hash)
            }
        }
    }

    pub fn from_fusevm(v: &Value) -> Self {
        match v {
            Value::Undef => EngineValue::Undef,
            Value::Bool(b) => EngineValue::Bool(*b),
            Value::Int(i) => EngineValue::Int(*i),
            Value::Float(f) => EngineValue::Float(*f),
            Value::Str(s) => EngineValue::String((**s).clone()),
            Value::Array(arr) => {
                let items = arr.iter().map(Self::from_fusevm).collect();
                EngineValue::Array(items)
            }
            Value::Hash(h) => {
                let mut map = HashMap::new();
                for (k, val) in h {
                    map.insert(k.clone(), Self::from_fusevm(val));
                }
                EngineValue::Object(map)
            }
            Value::Status(s) => EngineValue::Int(*s as i64),
            Value::Ref(r) => Self::from_fusevm(r),
            Value::NativeFn(_) => EngineValue::Undef,
            Value::Obj(id) => EngineValue::Int(*id as i64),
        }
    }

    pub fn to_js<'env>(&self, env: &'env Env) -> Result<Unknown<'env>> {
        match self {
            EngineValue::Undef => ToNapiValue::into_unknown((), env),
            EngineValue::Null => ToNapiValue::into_unknown(Null, env),
            EngineValue::Bool(b) => ToNapiValue::into_unknown(*b, env),
            EngineValue::Int(i) => {
                const MAX_SAFE_INTEGER: i64 = 9007199254740991; // 2^53 - 1
                const MIN_SAFE_INTEGER: i64 = -9007199254740991;
                if *i >= MIN_SAFE_INTEGER && *i <= MAX_SAFE_INTEGER {
                    ToNapiValue::into_unknown(*i, env)
                } else {
                    let bi = if *i >= 0 {
                        BigInt {
                            sign_bit: false,
                            words: vec![*i as u64],
                        }
                    } else {
                        let abs_val = (*i as i128).unsigned_abs() as u64;
                        BigInt {
                            sign_bit: true,
                            words: vec![abs_val],
                        }
                    };
                    ToNapiValue::into_unknown(bi, env)
                }
            }
            EngineValue::Float(f) => ToNapiValue::into_unknown(*f, env),
            EngineValue::String(s) => ToNapiValue::into_unknown(s.clone(), env),
            EngineValue::Array(items) => {
                let mut js_arr = Vec::with_capacity(items.len());
                for item in items {
                    js_arr.push(item.to_js(env)?);
                }
                ToNapiValue::into_unknown(js_arr, env)
            }
            EngineValue::Object(map) => {
                let mut obj = Object::new(env)?;
                for (k, v) in map {
                    let js_v = v.to_js(env)?;
                    obj.set(k, js_v)?;
                }
                ToNapiValue::into_unknown(obj, env)
            }
        }
    }

    pub fn from_js<'env>(env: &'env Env, val: Unknown<'env>) -> Result<Self> {
        let mut seen = Vec::new();
        Self::from_js_inner(env, val, &mut seen)
    }

    fn from_js_inner<'env>(
        env: &'env Env,
        val: Unknown<'env>,
        seen: &mut Vec<sys::napi_value>,
    ) -> Result<Self> {
        let vtype = val.get_type()?;
        match vtype {
            ValueType::Undefined => Ok(EngineValue::Undef),
            ValueType::Null => Ok(EngineValue::Null),
            ValueType::Boolean => {
                let b = val.coerce_to_bool()?;
                Ok(EngineValue::Bool(b))
            }
            ValueType::Number => {
                let num = val.coerce_to_number()?.get_double()?;
                if num == 0.0 && num.is_sign_negative() {
                    Ok(EngineValue::Float(num))
                } else if num.is_finite()
                    && num.fract() == 0.0
                    && num.abs() <= 9007199254740991.0
                {
                    Ok(EngineValue::Int(num as i64))
                } else {
                    Ok(EngineValue::Float(num))
                }
            }
            ValueType::String => {
                let s = val.coerce_to_string()?.into_utf8()?.into_owned()?;
                Ok(EngineValue::String(s))
            }
            ValueType::Object => {
                let raw_val = val.raw();
                for &seen_val in seen.iter() {
                    let mut is_equal = false;
                    unsafe {
                        napi::sys::napi_strict_equals(env.raw(), raw_val, seen_val, &mut is_equal);
                    }
                    if is_equal {
                        return Err(napi::Error::new(
                            Status::InvalidArg,
                            "Cyclic object references are not supported",
                        ));
                    }
                }
                seen.push(raw_val);

                let obj = unsafe { val.cast::<Object>() }?;
                let res = if obj.is_array()? {
                    let arr = unsafe { val.cast::<Array>() }?;
                    let len = arr.len();
                    let mut vec = Vec::with_capacity(len as usize);
                    for i in 0..len {
                        if let Some(elem) = arr.get::<Unknown>(i)? {
                            vec.push(Self::from_js_inner(env, elem, seen)?);
                        } else {
                            vec.push(EngineValue::Undef);
                        }
                    }
                    Ok(EngineValue::Array(vec))
                } else {
                    let keys = Object::keys(&obj)?;
                    let mut map = HashMap::new();
                    for key in keys {
                        if let Some(elem) = obj.get::<Unknown>(&key)? {
                            map.insert(key, Self::from_js_inner(env, elem, seen)?);
                        }
                    }
                    Ok(EngineValue::Object(map))
                };
                seen.pop();
                res
            }
            ValueType::Function => Err(napi::Error::new(
                Status::InvalidArg,
                "Functions cannot be passed directly as values",
            )),
            ValueType::Symbol => Err(napi::Error::new(
                Status::InvalidArg,
                "Symbols are not supported as values",
            )),
            _ => {
                if let Ok(bigint) = unsafe { val.cast::<BigInt>() } {
                    let (val, lossless) = bigint.get_i64();
                    if !lossless {
                        return Err(napi::Error::new(
                            Status::InvalidArg,
                            "BigInt value exceeds 64-bit integer range",
                        ));
                    }
                    return Ok(EngineValue::Int(val));
                }
                Ok(EngineValue::Undef)
            }
        }
    }
}
