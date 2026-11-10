//! NAPI binding scaffold for the domain `Record` and its parts.
//!
//! This is a BINDING SCAFFOLD. The real `Record`/`Content`/`ContentOp`/`AttrValue`/`PieceHash`/
//! `ContentHash` live in the core crate as `crate::types::*` ("provided elsewhere"). This module
//! defines the JS-facing `#[napi]` representations and the `From` bridges both ways, so the store
//! and WAL bindings can hand `Record` across the FFI boundary.

#![allow(dead_code, unused_variables, unreachable_code, unused_imports)]

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::types::{AttrValue, Content, ContentHash, ContentOp, PieceHash};

fn to_napi(e: anyhow::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

fn piece_hash_from_bytes(b: &[u8]) -> PieceHash {
    let mut a = [0u8; 32];
    a.copy_from_slice(b);
    PieceHash(a)
}

fn content_hash_from_bytes(b: &[u8]) -> ContentHash {
    let mut a = [0u8; 32];
    a.copy_from_slice(b);
    ContentHash(a)
}

// ----------------------------------------------------------------------------
// NAPI object types
// ----------------------------------------------------------------------------

/// A content program operation, as seen from JavaScript.
#[napi(object)]
pub struct ContentOpJs {
    /// `"lit"` (inline bytes) or `"piece"` (a referenced, hashed piece).
    pub kind: String,
    pub bytes: Option<Buffer>,
    pub hash: Option<Vec<u8>>,
    pub len: Option<u32>,
}

/// A named content program, as seen from JavaScript.
#[napi(object)]
pub struct ContentJs {
    pub name: String,
    pub identity: Option<Buffer>,
    pub ops: Vec<ContentOpJs>,
}

/// A named attribute value, as seen from JavaScript.
#[napi(object)]
#[derive(Default)]
pub struct AttrValueJs {
    pub kind: String,
    pub str: Option<String>,
    pub int: Option<i64>,
    pub float: Option<f64>,
    pub bool: Option<bool>,
    pub uint: Option<u64>,
    pub bytes: Option<Buffer>,
    pub timestamp_ns: Option<i64>,
}

/// A `(key, value)` attribute pair.
#[napi(object)]
pub struct AttrKvJs {
    pub key: String,
    pub value: AttrValueJs,
}

/// A record — the unit the WAL logs and the store reconstructs.
#[napi(object)]
pub struct Record {
    pub id: String,
    pub contents: Vec<ContentJs>,
    pub attrs: Vec<AttrKvJs>,
}

#[napi(object)]
pub struct PieceHashJs {
    pub bytes: Vec<u8>,
}

#[napi(object)]
pub struct ContentHashJs {
    pub bytes: Vec<u8>,
}

// ----------------------------------------------------------------------------
// Core -> JS
// ----------------------------------------------------------------------------

impl From<ContentOp> for ContentOpJs {
    fn from(op: ContentOp) -> Self {
        match op {
            ContentOp::Lit(b) => ContentOpJs {
                kind: "lit".into(),
                bytes: Some(b.into()),
                hash: None,
                len: None,
            },
            ContentOp::Piece { hash, len } => ContentOpJs {
                kind: "piece".into(),
                bytes: None,
                hash: Some(hash.0.to_vec()),
                len: Some(len),
            },
        }
    }
}

impl From<Content> for ContentJs {
    fn from(c: Content) -> Self {
        ContentJs {
            name: c.name,
            identity: c.identity.map(|h| h.0.to_vec().into()),
            ops: c.ops.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<AttrValue> for AttrValueJs {
    fn from(v: AttrValue) -> Self {
        match v {
            AttrValue::Str(s) => AttrValueJs {
                kind: "str".into(),
                str: Some(s),
                ..default()
            },
            AttrValue::Int(i) => AttrValueJs {
                kind: "int".into(),
                int: Some(i),
                ..default()
            },
            AttrValue::Float(f) => AttrValueJs {
                kind: "float".into(),
                float: Some(f),
                ..default()
            },
            AttrValue::Bool(b) => AttrValueJs {
                kind: "bool".into(),
                bool: Some(b),
                ..default()
            },
            AttrValue::UInt(i) => AttrValueJs {
                kind: "uint".into(),
                uint: Some(i),
                ..default()
            },
            AttrValue::Bytes(b) => AttrValueJs {
                kind: "bytes".into(),
                bytes: Some(b.into()),
                ..default()
            },
            AttrValue::TimestampNs(ns) => AttrValueJs {
                kind: "timestamp_ns".into(),
                timestamp_ns: Some(ns),
                ..default()
            },
            AttrValue::Null => AttrValueJs {
                kind: "null".into(),
                ..default()
            },
        }
    }
}

impl From<crate::types::Record> for Record {
    fn from(r: crate::types::Record) -> Self {
        Record {
            id: r.id,
            contents: r.contents.into_iter().map(Into::into).collect(),
            attrs: r
                .attrs
                .into_iter()
                .map(|(key, value)| AttrKvJs {
                    key,
                    value: value.into(),
                })
                .collect(),
        }
    }
}

// ----------------------------------------------------------------------------
// JS -> core
// ----------------------------------------------------------------------------

impl From<ContentOpJs> for ContentOp {
    fn from(j: ContentOpJs) -> Self {
        match j.kind.as_str() {
            "lit" => ContentOp::Lit(j.bytes.map(|b| b.to_vec()).unwrap_or_default()),
            "piece" => ContentOp::Piece {
                hash: piece_hash_from_bytes(&j.hash.unwrap_or_default()),
                len: j.len.unwrap_or(0),
            },
            _ => ContentOp::Lit(Vec::new()),
        }
    }
}

impl From<ContentJs> for Content {
    fn from(j: ContentJs) -> Self {
        let mut c = Content::new(j.name, j.ops.into_iter().map(Into::into).collect());
        c.identity = j.identity.map(|b| content_hash_from_bytes(&b.to_vec()));
        c
    }
}

impl From<AttrValueJs> for AttrValue {
    fn from(j: AttrValueJs) -> Self {
        match j.kind.as_str() {
            "str" => AttrValue::Str(j.str.unwrap_or_default()),
            "int" => AttrValue::Int(j.int.unwrap_or(0)),
            "float" => AttrValue::Float(j.float.unwrap_or(0.0)),
            "bool" => AttrValue::Bool(j.bool.unwrap_or(false)),
            "uint" => AttrValue::UInt(j.uint.unwrap_or(0)),
            "bytes" => AttrValue::Bytes(j.bytes.map(|b| b.to_vec()).unwrap_or_default()),
            "timestamp_ns" => AttrValue::TimestampNs(j.timestamp_ns.unwrap_or(0)),
            _ => AttrValue::Null,
        }
    }
}

impl From<Record> for crate::types::Record {
    fn from(j: Record) -> crate::types::Record {
        crate::types::Record::new(
            j.id,
            j.contents.into_iter().map(Into::into).collect(),
            j.attrs
                .into_iter()
                .map(|kv| (kv.key, kv.value.into()))
                .collect(),
        )
        .expect("record from JS must satisfy core validation")
    }
}

impl From<PieceHashJs> for PieceHash {
    fn from(j: PieceHashJs) -> Self {
        piece_hash_from_bytes(&j.bytes)
    }
}

impl From<ContentHashJs> for ContentHash {
    fn from(j: ContentHashJs) -> Self {
        content_hash_from_bytes(&j.bytes)
    }
}
