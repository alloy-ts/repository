//! NAPI bindings for the git wire protocol (`gix_protocol`).
//!
//! This module hosts the protocol-level types exposed by `gix_protocol` that are useful from
//! JavaScript. The headline type is [`RefPrefixes`], a thin wrapper over the public
//! `gix_protocol::ls_refs::RefPrefixes`, which computes the `ref-prefix` arguments sent to a V2
//! server during the `ls-refs` command. On top of that it exposes helpers for parsing the raw
//! capability advertisement a server sends during the handshake.
//!
//! Note on scope: `gix_protocol` re-exports a deliberately small surface. The `ls-refs`
//! `LsRefsCommand` struct is public, but its `peel`/`symrefs`/`ref-in-want` command arguments are
//! private, and `gix::remote`'s `ref_map` does not forward them — so there is no public way to
//! request peeled/symbolic-ref ref lists from JS without re-implementing gitoxide's
//! handshake→ls-refs→parse pipeline. Invoking `ls-refs` (with the options gitoxide *does* support,
//! e.g. `unborn` when the server advertises it) is therefore done through the higher-level
//! [`Remote::ls_refs`](crate::remote::Remote::ls_refs); this module provides the building blocks
//! (prefix computation, capability parsing) around it.
//!
//! `gix_protocol::ls_refs::RefPrefixes` keeps its `prefixes` field and `into_args` method private,
//! so we mirror the computed prefixes locally to serialize them (the wire form prepends
//! `ref-prefix `).

use std::collections::HashSet;

use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use crate::error::{err, err_str};
use crate::refspec::{RefSpecOperation, to_refspec_operation};
use crate::remote::HandshakeRef;
use gix::bstr::ByteVec;
use gix::config::AsBStr;

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
    ) -> napi::Result<RefPrefixes> {
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
            .collect::<napi::Result<Vec<_>>>()?;

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
) -> napi::Result<Vec<String>> {
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

/// Parse a git protocol capability string into the list of capability names the server
/// advertised.
///
/// `input` is the first handshake packet line, a space-separated list such as
/// `"agent=git/2.34.1 ls-refs=shallow concurrent-push fetch=..."`. This mirrors
/// `gix_transport::client::Capabilities::from_bytes` and is useful for inspecting what a remote
/// supports at the wire level (`agent`, `ls-refs`, `fetch`, `ref-in-want`, `unborn`, …).
#[napi]
pub fn parse_capabilities(input: String) -> napi::Result<Vec<String>> {
    let (caps, _) = gix_protocol::transport::client::Capabilities::from_bytes(&with_nul(input))
        .map_err(|e| err("parse_capabilities", e))?;
    Ok(caps.iter().map(|c| c.name().to_string()).collect())
}

/// Whether the server advertises `name` as a capability, given the same capability string as
/// [`parse_capabilities`].
///
/// Returns `null` when the capability is absent, `false` when it is explicitly negated
/// (`-name`), and `true` when present — matching gitoxide's capability-negotiation semantics.
#[napi]
pub fn capabilities_supports(input: String, name: String) -> napi::Result<Option<bool>> {
    let (caps, _) = gix_protocol::transport::client::Capabilities::from_bytes(&with_nul(input))
        .map_err(|e| err("parse_capabilities", e))?;
    if caps.capability(name.as_str()).is_some() {
        return Ok(Some(true));
    }
    // `capability()` ignores the negated `-name` form, so detect it explicitly.
    let negated = format!("-{name}");
    let negated: &[u8] = negated.as_bytes();
    let negated = caps.iter().any(|c| &*c.name() == negated);
    Ok(if negated { Some(false) } else { None })
}

/// `Capabilities::from_bytes` parses a packet-line capability section where a leading `version N`
/// token is NUL-terminated and the real capabilities follow that delimiter. A plain
/// space-separated capability string from JS has neither, so synthesize `version 2` before the
/// NUL and place the capabilities after it.
fn with_nul(input: String) -> Vec<u8> {
    let mut buf = String::from("version 2\0");
    buf.push_str(&input);
    buf.into_bytes()
}

/// V2 protocol commands — the gitoxide analog of git2-curl's `Service`
/// (UploadPackLs / UploadPack / ReceivePackLs / ReceivePack). gix exposes the read side
/// (`ls-refs`, `fetch`, `object-info`); push has no V2 command binding, so it is omitted.
#[napi]
pub enum ProtocolCommand {
    /// `ls-refs` — advertise refs (git-upload-pack advertisement).
    LsRefs,
    /// `fetch` — negotiate a pack (git-upload-pack).
    Fetch,
    /// `object-info` — query object metadata.
    ObjectInfo,
}

/// Parse a single git protocol V2 `ls-refs` line into the same [`HandshakeRef`] shape returned by
/// `Remote.ls_refs`.
///
/// The grammar mirrors gitoxide's `Ref::from_v2_refs` parser: `"<oid> <refname>"`,
/// `"<oid> <refname> peeled:<oid>"`, `"<oid> <refname> symref-target:<target>"`, the combination of
/// both, and `"unborn <refname> symref-target:<target>"`.
#[napi]
pub fn parse_ref_line(line: String) -> napi::Result<HandshakeRef> {
    let r = parse_single_ref_line(&line)?;
    Ok(crate::remote::convert_ref(r))
}

/// Parse a full ref advertisement (newline-separated `ls-refs` lines) into the same
/// [`HandshakeRef`] shape returned by `Remote.ls_refs`. Empty lines are ignored.
#[napi]
pub fn parse_ref_advertisement(text: String) -> napi::Result<Vec<HandshakeRef>> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let r = parse_single_ref_line(line)?;
        out.push(crate::remote::convert_ref(r));
    }
    Ok(out)
}

/// Value of a server capability, given the same capability string as [`parse_capabilities`].
/// For a capability like `fetch=wait-for-done`, this returns `wait-for-done`. Returns `null` when
/// the capability is absent or present without a value.
#[napi]
pub fn capability_value(text: String, name: String) -> napi::Result<Option<String>> {
    let (caps, _) = gix_protocol::transport::client::Capabilities::from_bytes(&with_nul(text))
        .map_err(|e| err("parse_capabilities", e))?;
    Ok(caps
        .capability(name.as_str())
        .and_then(|c| c.value())
        .map(|v| v.to_string()))
}

/// Parse one V2 `ls-refs` line into a `gix_protocol::handshake::Ref`, replicating gitoxide's
/// (private) `Ref::from_v2_refs` parser so JS can parse raw protocol data without a live connection.
fn parse_single_ref_line(line: &str) -> napi::Result<gix_protocol::handshake::Ref> {
    let trimmed = line.trim_end();
    let mut tokens = trimmed.splitn(4, ' ');
    match (tokens.next(), tokens.next()) {
        (Some(hex_hash), Some(path)) => {
            let id = if hex_hash == "unborn" {
                None
            } else {
                Some(
                    gix::ObjectId::from_hex(hex_hash.as_bytes())
                        .map_err(|e| err("ref.parse_oid", e))?,
                )
            };
            if path.is_empty() {
                return Err(err_str("ref.parse", "ref line has empty path"));
            }
            let mut symref_target = None;
            let mut peeled = None;
            for attribute in tokens.by_ref().take(2) {
                let mut at = attribute.splitn(2, ':');
                match (at.next(), at.next()) {
                    (Some(attribute), Some(value)) => {
                        if value.is_empty() {
                            return Err(err_str("ref.parse", "ref attribute has empty value"));
                        }
                        match attribute {
                            "peeled" => {
                                peeled = Some(
                                    gix::ObjectId::from_hex(value.as_bytes())
                                        .map_err(|e| err("ref.parse_peeled", e))?,
                                );
                            }
                            "symref-target" => symref_target = Some(value),
                            _ => {
                                return Err(err_str(
                                    "ref.parse",
                                    format!("unknown ref attribute: {attribute}"),
                                ));
                            }
                        }
                    }
                    _ => return Err(err_str("ref.parse", "malformed ref attribute")),
                }
            }
            if tokens.next().is_some() {
                return Err(err_str("ref.parse", "too many tokens in ref line"));
            }
            Ok(match (symref_target, peeled) {
                (Some(target_name), peeled) => match target_name {
                    "(null)" => match peeled {
                        None => gix_protocol::handshake::Ref::Direct {
                            full_ref_name: gix::bstr::BString::from(path),
                            object: id.ok_or_else(|| err_str("ref.parse", "unborn as direct"))?,
                        },
                        Some(peeled) => gix_protocol::handshake::Ref::Peeled {
                            full_ref_name: gix::bstr::BString::from(path),
                            object: peeled,
                            tag: id.ok_or_else(|| err_str("ref.parse", "unborn as tag"))?,
                        },
                    },
                    name => match id {
                        Some(id) => gix_protocol::handshake::Ref::Symbolic {
                            full_ref_name: gix::bstr::BString::from(path),
                            tag: peeled.map(|_| id),
                            object: peeled.unwrap_or(id),
                            target: gix::bstr::BString::from(name),
                        },
                        None => gix_protocol::handshake::Ref::Unborn {
                            full_ref_name: gix::bstr::BString::from(path),
                            target: gix::bstr::BString::from(name),
                        },
                    },
                },
                (None, Some(peeled)) => gix_protocol::handshake::Ref::Peeled {
                    full_ref_name: gix::bstr::BString::from(path),
                    object: peeled,
                    tag: id.ok_or_else(|| err_str("ref.parse", "unborn as peeled tag"))?,
                },
                (None, None) => gix_protocol::handshake::Ref::Direct {
                    object: id.ok_or_else(|| err_str("ref.parse", "unborn as direct object"))?,
                    full_ref_name: gix::bstr::BString::from(path),
                },
            })
        }
        _ => Err(err_str("ref.parse", "ref line must be '<oid> <path>'")),
    }
}
