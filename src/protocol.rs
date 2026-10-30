//! NAPI bindings for the git wire protocol (`gix_protocol`).
//!
//! This module hosts the protocol-level types exposed by `gix_protocol` that are useful from
//! JavaScript. The headline type is [`RefPrefixes`], a thin wrapper over the public
//! `gix_protocol::ls_refs::RefPrefixes`, which computes the `ref-prefix` arguments sent to a V2
//! server during the `ls-refs` command. On top of that it exposes helpers for parsing the raw
//! capability advertisement a server sends during the handshake.

use std::collections::{HashSet, HashMap};

use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use crate::error::{err, err_str};
use crate::refspec::{RefSpecOperation, to_refspec_operation};
use crate::remote::HandshakeRef;
use gix::bstr::ByteVec;

/// Actions that a smart transport can ask a subtransport to perform.
#[napi]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Service {
    UploadPackLs,
    UploadPack,
    ReceivePackLs,
    ReceivePack,
}

/// HTTP Response returned by [`HttpTransport`].
#[napi(object)]
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

/// A transport using reqwest for HTTP network communication with Git remotes.
#[napi]
pub struct HttpTransport {
    client: reqwest::Client,
}

#[napi]
impl HttpTransport {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("git/gix-repository")
                .build()
                .unwrap_or_default(),
        }
    }

    /// Perform a smart service request over HTTP(S).
    #[napi]
    pub async fn request_smart_service(
        &self,
        url: String,
        service: Service,
        headers: Option<HashMap<String, String>>,
        body: Option<Vec<u8>>,
    ) -> napi::Result<HttpResponse> {
        let (method, endpoint, accept, content_type) = match service {
            Service::UploadPackLs => (
                reqwest::Method::GET,
                "/info/refs?service=git-upload-pack",
                "application/x-git-upload-pack-advertisement",
                None,
            ),
            Service::UploadPack => (
                reqwest::Method::POST,
                "/git-upload-pack",
                "application/x-git-upload-pack-result",
                Some("application/x-git-upload-pack-request"),
            ),
            Service::ReceivePackLs => (
                reqwest::Method::GET,
                "/info/refs?service=git-receive-pack",
                "application/x-git-receive-pack-advertisement",
                None,
            ),
            Service::ReceivePack => (
                reqwest::Method::POST,
                "/git-receive-pack",
                "application/x-git-receive-pack-result",
                Some("application/x-git-receive-pack-request"),
            ),
        };

        let base_url = url.trim_end_matches('/');
        let (path_part, query_part) = match endpoint.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (endpoint, None),
        };

        let full_url = if base_url.ends_with(path_part) {
            if let Some(q) = query_part {
                if base_url.contains('?') {
                    format!("{base_url}&{q}")
                } else {
                    format!("{base_url}?{q}")
                }
            } else {
                base_url.to_string()
            }
        } else {
            format!("{base_url}{endpoint}")
        };

        let mut req = self.client.request(method, &full_url);
        req = req.header("Accept", accept);

        if let Some(ct) = content_type {
            req = req.header("Content-Type", ct);
        }

        if let Some(hdrs) = headers {
            for (k, v) in hdrs {
                req = req.header(&k, &v);
            }
        }

        if let Some(b) = body {
            req = req.body(b);
        }

        let resp = req.send().await.map_err(|e| err("http_transport.send", e))?;
        let status_code = resp.status().as_u16();

        let mut res_headers = HashMap::new();
        for (k, v) in resp.headers() {
            if let Ok(val) = v.to_str() {
                res_headers.insert(k.as_str().to_string(), val.to_string());
            }
        }

        let resp_body = resp.bytes().await.map_err(|e| err("http_transport.bytes", e))?.to_vec();

        Ok(HttpResponse {
            status_code,
            headers: res_headers,
            body: resp_body,
        })
    }
}

/// SSH Response returned by [`SshTransport`].
#[napi(object)]
#[derive(Clone, Debug)]
pub struct SshResponse {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A transport for executing Git commands over SSH.
#[napi]
pub struct SshTransport {
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
}

#[napi]
impl SshTransport {
    #[napi(constructor)]
    pub fn new(user: Option<String>, port: Option<u16>, identity_file: Option<String>) -> Self {
        Self {
            user,
            port,
            identity_file,
        }
    }

    /// Execute a remote git command via SSH.
    #[napi]
    pub async fn exec_command(
        &self,
        host: String,
        command: String,
        stdin: Option<Vec<u8>>,
    ) -> napi::Result<SshResponse> {
        let mut cmd = std::process::Command::new("ssh");
        if let Some(port) = self.port {
            cmd.arg("-p").arg(port.to_string());
        }
        if let Some(ref id) = self.identity_file {
            cmd.arg("-i").arg(id);
        }
        let destination = if let Some(ref user) = self.user {
            format!("{user}@{host}")
        } else {
            host
        };
        cmd.arg(destination).arg(command);

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| err("ssh_transport.spawn", e))?;

        if let Some(input) = stdin {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin_pipe.write_all(&input);
            }
        }

        let output = child.wait_with_output().map_err(|e| err("ssh_transport.wait", e))?;

        Ok(SshResponse {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// Information parsed from remote progress messages sent by a git server.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct ParsedRemoteProgress {
    pub action: String,
    pub percent: Option<u32>,
    pub step: Option<u32>,
    pub max: Option<u32>,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct RemoteProgress<'a> {
    pub action: &'a gix::bstr::BStr,
    pub percent: Option<u32>,
    pub step: Option<usize>,
    pub max: Option<usize>,
}

fn parse_number(i: &mut &[u8]) -> Option<usize> {
    let len = i.iter().take_while(|b| b.is_ascii_digit()).count();
    if len == 0 {
        return None;
    }
    let (number, rest) = i.split_at(len);
    *i = rest;
    std::str::from_utf8(number).ok()?.parse::<usize>().ok()
}

fn skip_until_digit_or_to_end(i: &mut &[u8]) {
    let pos = i.iter().position(u8::is_ascii_digit).unwrap_or(i.len());
    *i = &i[pos..];
}

fn next_optional_percentage(i: &mut &[u8]) -> Option<u32> {
    let before = *i;
    skip_until_digit_or_to_end(i);
    let number = parse_number(i)?;
    if let Some(rest) = i.strip_prefix(b"%") {
        *i = rest;
        u32::try_from(number).ok()
    } else {
        *i = before;
        None
    }
}

fn next_optional_number(i: &mut &[u8]) -> Option<usize> {
    skip_until_digit_or_to_end(i);
    parse_number(i)
}

fn parse_progress<'i>(line: &mut &'i [u8]) -> std::result::Result<RemoteProgress<'i>, ()> {
    let action_end = line.iter().position(|b| *b == b':').ok_or(())?;
    if action_end == 0 {
        return Err(());
    }
    let action = &line[..action_end];
    *line = &line[action_end..];
    let percent = next_optional_percentage(line);
    let step = next_optional_number(line);
    let max = next_optional_number(line);
    Ok(RemoteProgress {
        action: gix::bstr::BStr::new(action),
        percent,
        step,
        max,
    })
}

impl RemoteProgress<'_> {
    pub fn from_bytes(mut line: &[u8]) -> Option<RemoteProgress<'_>> {
        parse_progress(&mut line)
            .ok()
            .filter(|&r| !(r.percent.is_none() && r.step.is_none() && r.max.is_none()))
    }
}

/// Parse a remote progress line sent by a git server during fetch/clone/push.
#[napi]
pub fn parse_remote_progress(line: String) -> Option<ParsedRemoteProgress> {
    let rp = RemoteProgress::from_bytes(line.as_bytes())?;
    Some(ParsedRemoteProgress {
        action: rp.action.to_string(),
        percent: rp.percent,
        step: rp.step.map(|s| s as u32),
        max: rp.max.map(|m| m as u32),
    })
}

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
