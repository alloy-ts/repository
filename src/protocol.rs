//! NAPI bindings for the git wire protocol (`gix_protocol`).
//!
//! This module hosts the protocol-level types exposed by `gix_protocol` that are useful from
//! JavaScript. The headline type is [`RefPrefixes`], a thin wrapper over the public
//! `gix_protocol::ls_refs::RefPrefixes`, which computes the `ref-prefix` arguments sent to a V2
//! server during the `ls-refs` command.
//!
//! Note on scope: `gix_protocol` only re-exports a small surface. The `ls-refs` `LsRefsCommand`
//! lives in a `pub(crate)` module of `gix_protocol`, so it cannot be wrapped directly from this
//! crate. Invoking `ls-refs` is therefore done through the higher-level
//! [`Remote::ls_refs`](crate::remote::Remote::ls_refs), which internally runs that very command;
//! this module provides the building blocks (prefix computation) around it.
//!
//! `gix_protocol::ls_refs::RefPrefixes` keeps its `prefixes` field and `into_args` method private,
//! so we mirror the computed prefixes locally to serialize them (the wire form prepends
//! `ref-prefix `).

use std::collections::HashSet;

use napi::Result;
use napi_derive::napi;

use crate::error::err;
use crate::refspec::{RefSpecOperation, to_refspec_operation};
use gix::bstr::ByteVec;

/// `ref-prefix` arguments sent to a V2 server during `ls-refs`.
///
/// Wraps `gix_protocol::ls_refs::RefPrefixes`. Build it from refspecs with
/// [`RefPrefixes::from_refspecs`] (the same expansion gix's `ls-refs` command uses internally),
/// or add prefixes manually with [`RefPrefixes::extend`]. Call [`RefPrefixes::into_args`] for the
/// wire-form strings that would actually be transmitted.
#[napi]
pub struct RefPrefixes {
    /// Upstream prefix set (used for its authoritative `from_refspecs`/`extend` logic).
    inner: gix_protocol::ls_refs::RefPrefixes,
    /// Mirror of the prefixes, kept because `inner`'s `prefixes`/`into_args` are private upstream.
    raw: Vec<gix::bstr::BString>,
}

#[napi]
impl RefPrefixes {
    /// Create an empty set of `ref-prefix` arguments.
    #[napi(constructor)]
    pub fn new() -> Self {
        RefPrefixes {
            inner: gix_protocol::ls_refs::RefPrefixes::new(),
            raw: Vec::new(),
        }
    }

    /// Build a prefix set from refspecs, expanding each into its two-component `ref-prefix`
    /// candidates (e.g. `refs/heads/`, `refs/tags/`, `HEAD`).
    ///
    /// Non-fully-qualified refs (like `main` or `HEAD`) are expanded DWIM-style, yielding
    /// candidates such as `refs/heads/main`. `operation` defaults to
    /// [`Fetch`](RefSpecOperation::Fetch).
    #[napi]
    pub fn from_refspecs(
        specs: Vec<String>,
        operation: Option<RefSpecOperation>,
    ) -> Result<RefPrefixes> {
        let op = operation
            .map(to_refspec_operation)
            .unwrap_or(gix::refspec::parse::Operation::Fetch);
        let parsed: Vec<gix::refspec::RefSpec> = specs
            .iter()
            .map(|spec| {
                gix::refspec::parse(gix::bstr::BStr::new(spec.as_bytes()), op)
                    .map_err(|e| err("refspec.parse", e))
                    .map(|r| r.to_owned())
            })
            .collect::<Result<Vec<_>>>()?;

        let mut seen = HashSet::new();
        let mut raw = Vec::new();
        for spec in &parsed {
            let spec_ref = spec.to_ref();
            if seen.insert(spec_ref.instruction()) {
                spec_ref.expand_prefixes(&mut raw);
            }
        }

        Ok(RefPrefixes {
            inner: gix_protocol::ls_refs::RefPrefixes::from_refspecs(&parsed),
            raw,
        })
    }

    /// Add additional prefixes (e.g. `refs/heads/`, `HEAD`). Duplicates are ignored and first-seen
    /// order is preserved.
    #[napi]
    pub fn extend(&mut self, prefixes: Vec<String>) {
        for p in prefixes {
            let b = gix::bstr::BString::from(p.into_bytes());
            if !self.raw.iter().any(|e| e == &b) {
                self.raw.push(b.clone());
            }
            self.inner.extend(std::iter::once(b));
        }
    }

    /// Return the wire-form arguments: each prefix prefixed with `ref-prefix `.
    ///
    /// Consumes the prefix set (it cannot be extended afterwards).
    #[napi]
    pub fn into_args(&mut self) -> Vec<String> {
        let raw = std::mem::take(&mut self.raw);
        raw.into_iter()
            .map(|mut prefix| {
                prefix.insert_str(0, "ref-prefix ");
                prefix.to_string()
            })
            .collect()
    }
}

/// Compute the raw `ref-prefix` patterns (without the `ref-prefix ` wire prefix) that the git
/// protocol V2 `ls-refs` command would send for the given refspecs.
///
/// This mirrors `gix_protocol::ls_refs::RefPrefixes::from_refspecs` but returns the bare prefixes
/// (e.g. `refs/heads/`, `HEAD`) rather than a command object. `operation` defaults to
/// [`Fetch`](RefSpecOperation::Fetch).
#[napi]
pub fn ref_prefixes_from_specs(
    specs: Vec<String>,
    operation: Option<RefSpecOperation>,
) -> Result<Vec<String>> {
    let op = operation
        .map(to_refspec_operation)
        .unwrap_or(gix::refspec::parse::Operation::Fetch);
    let mut prefixes: Vec<gix::bstr::BString> = Vec::new();
    for spec in &specs {
        let parsed = gix::refspec::parse(gix::bstr::BStr::new(spec.as_bytes()), op)
            .map_err(|e| err("refspec.parse", e))?;
        let mut out = Vec::new();
        parsed.expand_prefixes(&mut out);
        for prefix in out {
            if !prefixes.iter().any(|existing| existing == &prefix) {
                prefixes.push(prefix);
            }
        }
    }
    Ok(prefixes.into_iter().map(|p| p.to_string()).collect())
}
