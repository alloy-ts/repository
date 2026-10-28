#![allow(clippy::empty_docs)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};
use napi_derive::napi;

use gix::bstr::ByteSlice;
use gix::config::tree::{Gpg, User, gpg};
use gix::objs::signature::sign::Options as SignOptions;
use gix::objs::signature::sign::is_literal_ssh_key;
use gix::objs::signature::verify::{Outcome, TrustLevel};
use gix::objs::signature::{self, Format};

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::log::{CommitInfo, log_summary};
use crate::mailmap::Mailmap;
use crate::object::{Object, build_object};
use crate::repository::Repository;
use crate::tree::{Tree, build_tree};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Look up `field` in a raw commit header block. Continuation lines (starting
/// with a space) are unfolded, and repeated headers are joined with newlines,
/// matching libgit2's `git_commit_header_field`.
fn header_field(header: &[u8], field: &str) -> Option<String> {
    let mut values: Vec<String> = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in header.lines() {
        if line.first() == Some(&b' ') {
            if let Some((_, value)) = current.as_mut() {
                value.push('\n');
                value.push_str(&lossy(line.trim_start()));
            }
            continue;
        }
        if let Some((name, value)) = current.take() {
            if name == field {
                values.push(value);
            }
        }
        // Commit headers are `key<space>value`, not `key: value`.
        if let Some(space) = line.find_byte(b' ') {
            current = Some((
                lossy(line[..space].trim_end()),
                lossy(line[space + 1..].trim_start()),
            ));
        }
    }
    if let Some((name, value)) = current {
        if name == field {
            values.push(value);
        }
    }

    if values.is_empty() {
        None
    } else {
        Some(values.join("\n"))
    }
}

/// Resolve `signature` through `mailmap`, preserving its timestamp; returns the
/// original identity when no mailmap entry applies (libgit2 semantics).
fn resolve_with_mailmap(mailmap: &Mailmap, signature: gix::actor::SignatureRef<'_>) -> Signature {
    if let Some(resolved) = mailmap.inner.try_resolve(gix::actor::SignatureRef {
        name: signature.name,
        email: signature.email,
        time: signature.time,
    }) {
        return to_signature(
            &resolved.name,
            &resolved.email,
            resolved.time.seconds,
            resolved.time.offset,
        );
    }
    match signature.time() {
        Ok(time) => to_signature(signature.name, signature.email, time.seconds, time.offset),
        Err(_) => to_signature(signature.name, signature.email, 0, 0),
    }
}

/// Borrow a napi `Signature` as a `gix_actor::SignatureRef`.
///
/// The time is serialized into `time_buf` in git's raw format (`<seconds> <±HHMM>`),
/// because `SignatureRef.time` is a string that gets parsed later — an unparsable time
/// would silently become the unix epoch.
pub(crate) fn signature_ref<'a>(
    s: &'a Signature,
    time_buf: &'a mut String,
) -> gix::actor::SignatureRef<'a> {
    let offset = s.timezone_offset;
    let sign = if offset < 0 { '-' } else { '+' };
    let total = offset.unsigned_abs();
    *time_buf = format!(
        "{} {}{:02}{:02}",
        s.time,
        sign,
        total / 3600,
        (total % 3600) / 60
    );
    gix::actor::SignatureRef {
        name: gix::bstr::BStr::new(s.name.as_bytes()),
        email: gix::bstr::BStr::new(s.email.as_bytes()),
        time: time_buf.as_str(),
    }
}

/// Clean up a commit message, removing extraneous whitespace and ensuring that
/// it ends with a newline: blank lines are collapsed to at most one, leading and
/// trailing blank lines are removed, and trailing whitespace is trimmed from each
/// line. When `commentChar` is set (git's default is `"#"`), lines starting with
/// that character are dropped.
///
/// Mirrors libgit2's `git_message_prettify`.
#[napi]
pub fn message_prettify(message: String, comment_char: Option<String>) -> napi::Result<String> {
    let comment_char = match comment_char {
        None => None,
        Some(char) => {
            let bytes = char.as_bytes();
            if bytes.len() != 1 {
                return Err(err_str(
                    "comment_char",
                    "comment_char must be exactly one character",
                ));
            }
            Some(bytes[0])
        }
    };
    Ok(prettify_message(&message, comment_char))
}

/// Parse the `git interpret-trailers` trailers of an arbitrary message, rather
/// than one attached to a commit. Everything after a `---` cut line is ignored,
/// like git does.
///
/// Mirrors libgit2's `git_message_trailers_strs`. JS strings are always UTF-8, so
/// there is no separate bytes variant (`git_message_trailers_bytes`).
#[napi]
pub fn message_trailers(message: String) -> napi::Result<Vec<Trailer>> {
    let cut = cut_at_line(&message, "---");
    let parsed = gix::objs::commit::MessageRef::from_bytes(cut.as_bytes());
    Ok(match parsed.body() {
        Some(body) => body
            .trailers()
            .map(|trailer| Trailer {
                token: lossy(trailer.token),
                value: lossy(trailer.value.as_ref()),
            })
            .collect(),
        None => Vec::new(),
    })
}

/// git's message cleanup: strip comment lines, trim trailing whitespace from each
/// line, collapse runs of blank lines to one, drop leading/trailing blank lines,
/// and ensure exactly one trailing newline (an empty result stays empty).
fn prettify_message(message: &str, comment_char: Option<u8>) -> String {
    let mut kept: Vec<&str> = Vec::with_capacity(message.lines().count());
    let mut blanks = 0;
    for line in message.lines() {
        if let Some(comment) = comment_char {
            if line.starts_with(comment as char) {
                continue;
            }
        }
        let line = line.trim_end_matches([' ', '\t', '\r']);
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        kept.push(line);
    }
    while kept.first().is_some_and(|line| line.is_empty()) {
        kept.remove(0);
    }
    while kept.last().is_some_and(|line| line.is_empty()) {
        kept.pop();
    }
    if kept.is_empty() {
        return String::new();
    }
    let mut out = kept.join("\n");
    out.push('\n');
    out
}

/// `message` truncated before the first line that is exactly `sep`.
fn cut_at_line<'a>(message: &'a str, separator: &str) -> &'a str {
    let mut offset = 0;
    for chunk in message.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == separator {
            return &message[..offset];
        }
        offset += chunk.len();
    }
    message
}

/// A git commit exposed to TypeScript.
#[napi]
pub struct Commit {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Commit {
    /// The object id of this commit.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// Full commit message, with leading newlines removed (libgit2's
    /// `git_commit_message`). See [`message_raw`](Self::message_raw) for the
    /// message exactly as stored.
    #[napi(getter)]
    pub fn message(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let raw = commit.message_raw().map_err(|e| err("message", e))?;
        let bytes: &[u8] = raw.as_ref();
        let mut start = 0;
        while start < bytes.len() && bytes[start] == b'\n' {
            start += 1;
        }
        Ok(lossy(&bytes[start..]))
    }

    /// The root tree id of the commit.
    #[napi(getter)]
    pub fn tree_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.tree.to_string())
    }

    /// Parent commit ids.
    #[napi(getter)]
    pub fn parent_ids(&self) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.parents().map(|p| p.to_string()).collect())
    }

    /// The author identity of this commit.
    #[napi]
    pub fn author(&self) -> napi::Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        let sig = commit.author().map_err(|e| err("author", e))?;
        let time = sig.time().map_err(|e| err("author.time", e))?;
        Ok(to_signature(sig.name, sig.email, time.seconds, time.offset))
    }

    /// The committer identity of this commit.
    #[napi]
    pub fn committer(&self) -> napi::Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        let sig = commit.committer().map_err(|e| err("committer", e))?;
        let time = sig.time().map_err(|e| err("committer.time", e))?;
        Ok(to_signature(sig.name, sig.email, time.seconds, time.offset))
    }

    /// The name of the message encoding from the `encoding` header, if present.
    /// UTF-8 should be assumed when this is `null`.
    #[napi(getter)]
    pub fn encoding(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.encoding.map(|e| lossy(e)))
    }

    /// Extra header fields, in the order encountered during parsing. These are all headers
    /// that are not part of the standard set (`tree`, `parent`, `author`, `committer`,
    /// `encoding`, `mergetag`, `gpgsig`, ...), such as custom headers.
    #[napi]
    pub fn extra_headers(&self) -> napi::Result<Vec<CommitHeaderEntry>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit
            .extra_headers
            .iter()
            .map(|(name, value)| CommitHeaderEntry {
                name: lossy(name),
                value: lossy(value),
            })
            .collect())
    }

    /// The raw cryptographic signature (the `gpgsig`/`gpgsig-sha256` header) if the commit is
    /// signed, otherwise `null`. Presence only means the commit *carries* a signature; it does not
    /// prove the signature is valid (see [`Commit::verify`]).
    #[napi(getter)]
    pub fn signature(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let sig = commit.signature().map_err(|e| err("signature", e))?;
        Ok(sig.map(|(s, _)| lossy(&*s)))
    }

    /// Verify this commit's signature.
    ///
    /// Returns `null` if the commit is not signed. Otherwise returns the verification outcome
    /// (`status`, `trustLevel`, `signer`). This shells out to the configured GPG/SSH/X.509 program,
    /// so it can be slow.
    #[napi]
    pub fn verify(&self) -> napi::Result<Option<VerifyOutcome>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let Some((signature, signed_data)) = commit.signature().map_err(|e| err("signature", e))?
        else {
            return Ok(None);
        };

        let config = r.config_snapshot();
        let minimum_trust = config
            .string(Gpg::MIN_TRUST_LEVEL)
            .map(|v| Gpg::MIN_TRUST_LEVEL.try_into_trust_level(v))
            .transpose()
            .map_err(|e| err("gpg.minTrustLevel", e))?
            .unwrap_or(TrustLevel::Undefined);

        let format = Format::from_signature(&*signature)
            .ok_or_else(|| err_str("verify", "unsupported or unrecognized signature format"))?;

        let program = signature_program(&config, format)?;

        let options = match format {
            Format::OpenPgp => signature::verify::Options::OpenPgp {
                program,
                program_arguments: Vec::new(),
                environment: Vec::new(),
                minimum_trust,
            },
            Format::X509 => signature::verify::Options::X509 {
                program,
                program_arguments: Vec::new(),
                environment: Vec::new(),
                minimum_trust,
            },
            Format::Ssh => {
                let allowed_signers = config
                    .trusted_path(gpg::Ssh::ALLOWED_SIGNERS_FILE)
                    .map_err(|e| err("gpg.ssh.allowedSignersFile", e))?
                    .ok_or_else(|| {
                        err_str(
                            "verify",
                            "gpg.ssh.allowedSignersFile must be configured for SSH signature verification",
                        )
                    })?;
                let revocation_file = config
                    .trusted_path(gpg::Ssh::REVOCATION_FILE)
                    .map_err(|e| err("gpg.ssh.revocationFile", e))?
                    .filter(|p| p.exists());
                signature::verify::Options::Ssh {
                    program,
                    program_arguments: Vec::new(),
                    environment: Vec::new(),
                    allowed_signers: resolve_relative_to_repository(&r, allowed_signers),
                    revocation_file: revocation_file.map(|p| resolve_relative_to_repository(&r, p)),
                    verify_time: commit.time().map_err(|e| err("commit.time", e))?,
                    minimum_trust,
                }
            }
        };

        let outcome = signed_data
            .verify(&*signature, options)
            .map_err(|e| err("verify", e))?;
        Ok(Some(VerifyOutcome::from(outcome)))
    }

    /// Sign this commit, producing a *new* commit object that carries the signature, and return it.
    ///
    /// The signing key and program are taken from the repository configuration
    /// (`user.signingKey`, `gpg.format`, `gpg.<format>.program`, etc.), mirroring `git commit -S`.
    #[napi]
    pub fn sign(&self) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let options = signing_options(&r)?;
        let signed = commit
            .decode()
            .map_err(|e| err("decode", e))?
            .sign(options)
            .map_err(|e| err("sign", e))?;
        let id = r
            .write_object(&signed)
            .map_err(|e| err("write_object", e))?;
        let signed_commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        Ok(Commit {
            repo: Arc::clone(&self.repo),
            id: signed_commit.id,
        })
    }

    /// The short "summary" of the message: the first paragraph with whitespace
    /// folded and trimmed, or `null` when there is none.
    #[napi(getter)]
    pub fn summary(&self) -> napi::Result<Option<String>> {
        let prettified = self.prettified_message()?;
        let message = gix::objs::commit::MessageRef::from_bytes(&prettified);
        let summary = message.summary();
        if summary.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lossy(summary.as_ref())))
        }
    }

    /// The long "body" of the message: everything after the first paragraph,
    /// trimmed (trailers included), or `null` when there is none.
    #[napi(getter)]
    pub fn body(&self) -> napi::Result<Option<String>> {
        Ok(crate::object::message_body(&self.prettified_message()?))
    }

    /// The abbreviated object id (default 7 characters).
    #[napi(getter)]
    pub fn short_sha(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        Ok(crate::object::short_sha(&r, &self.id))
    }

    /// The raw first line of the commit message (the subject), without the whitespace folding that
    /// [`summary`](Self::summary) applies.
    #[napi(getter)]
    pub fn subject(&self) -> napi::Result<String> {
        let msg = self.message()?;
        Ok(msg.lines().next().unwrap_or("").to_string())
    }

    /// The author date as an RFC 3339 / ISO 8601 string (e.g. `2024-01-02T03:04:05+00:00`).
    #[napi(getter)]
    pub fn author_date(&self) -> napi::Result<String> {
        let sig = self.author()?;
        Ok(crate::object::rfc3339(sig.time, sig.timezone_offset))
    }

    /// The committer date as an RFC 3339 / ISO 8601 string.
    #[napi(getter)]
    pub fn committer_date(&self) -> napi::Result<String> {
        let sig = self.committer()?;
        Ok(crate::object::rfc3339(sig.time, sig.timezone_offset))
    }

    /// The n-th generation ancestor of this commit, following first-parent history.
    /// `n = 0` returns this commit; `n = 1` its first parent, and so on. Errors when an ancestor
    /// does not exist (e.g. walking past the root commit).
    #[napi]
    pub fn nth_ancestor(&self, n: u32) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let mut id = self.id;
        for _ in 0..n {
            let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
            id = commit
                .parent_ids()
                .next()
                .ok_or_else(|| {
                    err_str(
                        "nth_ancestor",
                        "commit has no parent at the requested depth",
                    )
                })?
                .detach();
        }
        let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        build_commit(&self.repo, commit)
    }

    /// Serialize this commit to the same flat [`CommitInfo`] snapshot that [`Repository::log`] returns,
    /// so a single commit can be rendered with the same shape as history.
    #[napi]
    pub fn info(&self) -> napi::Result<CommitInfo> {
        let r = thread_local(&self.repo);
        Ok(log_summary(&r, self.id)?.0)
    }

    /// The full message exactly as stored, without the leading-newline
    /// prettification that [`message`](Self::message) may apply.
    #[napi(getter)]
    pub fn message_raw(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let raw = commit.message_raw().map_err(|e| err("message_raw", e))?;
        Ok(lossy(raw))
    }

    /// The raw header block: every header line up to (and including the newline
    /// before) the blank line that separates headers from the message
    /// (`git_commit_raw_header`).
    #[napi(getter)]
    pub fn raw_header(&self) -> napi::Result<String> {
        Ok(lossy(&self.raw_header_bytes()?))
    }

    /// The unfolded value of an arbitrary header field (`tree`, `parent`,
    /// `author`, `committer`, `encoding`, `gpgsig`, or any custom header), or
    /// `null` when it is not present. Repeated headers are joined with newlines
    /// (`git_commit_header_field`).
    #[napi]
    pub fn header_field(&self, name: String) -> napi::Result<Option<String>> {
        Ok(header_field(&self.raw_header_bytes()?, &name))
    }

    /// Serialize this commit to its canonical raw git object text — the exact bytes that
    /// `git cat-file commit <oid>` (or `git hash-object -t commit`) would produce: the raw
    /// header block, a blank line, and the message exactly as stored. Useful to confirm what
    /// gets hashed before writing, or to reconstruct an object from its parts.
    #[napi]
    pub fn to_object_text(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let header = self.raw_header_bytes()?;
        let message = commit.message_raw().map_err(|e| err("message_raw", e))?;
        let mut out = Vec::with_capacity(header.len() + message.len() + 1);
        out.extend_from_slice(&header);
        out.push(b'\n'); // completes the blank line separating the header from the message
        out.extend_from_slice(message.as_ref());
        Ok(lossy(&out))
    }

    /// The commit (committer) time as `{ seconds, offset }`, where `offset` is in
    /// **seconds** (consistent with [`Signature.timezoneOffset`]; note git2 uses
    /// minutes).
    #[napi(getter)]
    pub fn time(&self) -> napi::Result<CommitTime> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let time = commit.time().map_err(|e| err("time", e))?;
        Ok(CommitTime {
            seconds: time.seconds,
            offset: time.offset,
        })
    }

    /// The root tree this commit points to.
    #[napi]
    pub fn tree(&self) -> napi::Result<Tree> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let tree = commit.tree().map_err(|e| err("tree", e))?;
        build_tree(&self.repo, tree.id)
    }

    /// The number of parents (0 for the root commit). Cheaper than
    /// `parentIds.length`.
    #[napi]
    pub fn parent_count(&self) -> napi::Result<u32> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        Ok(commit.parent_ids().count() as u32)
    }

    /// The parent commit at `index`, or an error when the index is out of bounds.
    #[napi]
    pub fn parent(&self, index: u32) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let id = commit
            .parent_ids()
            .nth(index as usize)
            .ok_or_else(|| err_str("parent", format!("parent index {index} is out of bounds")))?
            .detach();
        let parent = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        build_commit(&self.repo, parent)
    }

    /// All parent commits, in order.
    #[napi]
    pub fn parents(&self) -> napi::Result<Vec<Commit>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let ids: Vec<gix::ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let parent = r.find_commit(id).map_err(|e| err("find_commit", e))?;
            out.push(build_commit(&self.repo, parent)?);
        }
        Ok(out)
    }

    /// The author identity resolved through `mailmap`, falling back to the
    /// original when no mapping applies.
    #[napi]
    pub fn author_with_mailmap(&self, mailmap: &Mailmap) -> napi::Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let decoded = commit.decode().map_err(|e| err("decode", e))?;
        let author = decoded.author().map_err(|e| err("author", e))?;
        Ok(resolve_with_mailmap(mailmap, author))
    }

    /// The committer identity resolved through `mailmap`, falling back to the
    /// original when no mapping applies.
    #[napi]
    pub fn committer_with_mailmap(&self, mailmap: &Mailmap) -> napi::Result<Signature> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let decoded = commit.decode().map_err(|e| err("decode", e))?;
        let committer = decoded.committer().map_err(|e| err("committer", e))?;
        Ok(resolve_with_mailmap(mailmap, committer))
    }

    /// This commit viewed as a generic object.
    #[napi(ts_return_type = "Object")]
    pub fn as_object(&self) -> napi::Result<Object> {
        build_object(&self.repo, self.id)
    }

    /// Amend this commit, returning the id of the *new* commit object.
    ///
    /// Fields left `null` are carried over from the original commit, which keeps
    /// the same parents. When `updateRef` is set, that reference is moved to the
    /// new commit (a symbolic `HEAD` is dereferenced, like git).
    #[napi]
    pub fn amend(&self, options: AmendOptions) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let decoded = commit.decode().map_err(|e| err("decode", e))?;

        let tree = match options.tree.as_deref() {
            Some(tree) => parse_oid(tree)?,
            None => decoded.tree(),
        };
        let author = match options.author.as_ref() {
            Some(author) => signature_from_dto(author)?,
            None => decoded
                .author()
                .map_err(|e| err("author", e))?
                .to_owned()
                .map_err(|e| err("author", e))?,
        };
        let committer = match options.committer.as_ref() {
            Some(committer) => signature_from_dto(committer)?,
            None => decoded
                .committer()
                .map_err(|e| err("committer", e))?
                .to_owned()
                .map_err(|e| err("committer", e))?,
        };
        let message = match options.message {
            Some(message) => message.into(),
            None => decoded.message.to_owned(),
        };
        let encoding = match options.message_encoding {
            Some(encoding) => Some(encoding.into()),
            None => decoded.encoding.map(|encoding| encoding.to_owned()),
        };

        let amended = gix::objs::Commit {
            tree,
            parents: decoded.parents().collect::<Vec<_>>().into(),
            author,
            committer,
            encoding,
            message,
            extra_headers: decoded
                .extra_headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.as_ref().to_owned()))
                .collect(),
        };

        let id = r
            .write_object(&amended)
            .map_err(|e| err("write_object", e))?;

        if let Some(update_ref) = options.update_ref.as_deref() {
            let full_name =
                gix::refs::FullName::try_from(update_ref).map_err(|e| err("update_ref", e))?;
            let edit = gix::refs::transaction::RefEdit::update_with_log(
                full_name,
                gix::refs::Target::Object(id.detach()),
                gix::refs::transaction::PreviousValue::Any,
                gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: "commit: amend".into(),
                },
            )
            // Like git, `updateRef: "HEAD"` updates the branch HEAD points at
            // rather than detaching it.
            .with_deref(true);
            r.edit_reference(edit).map_err(|e| err("update_ref", e))?;
        }

        Ok(id.to_string())
    }

    /// The message's git-interpret-trailers trailers, e.g. `Signed-off-by` or
    /// `Co-authored-by`, parsed from the trailing paragraph.
    #[napi]
    pub fn trailers(&self) -> napi::Result<Vec<Trailer>> {
        let prettified = self.prettified_message()?;
        let message = gix::objs::commit::MessageRef::from_bytes(&prettified);
        Ok(match message.body() {
            Some(body) => body
                .trailers()
                .map(|trailer| Trailer {
                    token: lossy(trailer.token),
                    value: lossy(trailer.value.as_ref()),
                })
                .collect(),
            None => Vec::new(),
        })
    }

    /// Just the `Co-authored-by` trailers, in the order they appear.
    #[napi]
    pub fn co_authors(&self) -> napi::Result<Vec<Trailer>> {
        Ok(self
            .trailers()?
            .into_iter()
            .filter(|trailer| trailer.token.eq_ignore_ascii_case("co-authored-by"))
            .collect())
    }
}

impl Commit {
    /// The message with leading newlines removed — the form libgit2's
    /// `git_commit_message` presents, and the basis for `summary`, `body`, and
    /// `trailers`.
    fn prettified_message(&self) -> napi::Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let raw = commit.message_raw().map_err(|e| err("message_raw", e))?;
        let bytes: &[u8] = raw.as_ref();
        let mut start = 0;
        while start < bytes.len() && bytes[start] == b'\n' {
            start += 1;
        }
        Ok(bytes[start..].to_vec())
    }

    /// The raw header block: object data up to and including the newline that
    /// precedes the blank line separating headers from the message.
    fn raw_header_bytes(&self) -> napi::Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let object = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let data = object.data.as_slice();
        let end = data.find(b"\n\n").map(|pos| pos + 1).unwrap_or(data.len());
        Ok(data[..end].to_vec())
    }
}

/// The result of verifying a commit or annotated-tag signature.
#[napi(object)]
pub struct VerifyOutcome {
    /// The detected signature format (`OpenPgp`, `X509`, or `Ssh`).
    pub format: String,
    /// `Good`, `Bad`, `Error`, `Expired`, `ExpiredKey`, `RevokedKey`, or `Unknown`.
    pub status: String,
    /// `Undefined`, `Never`, `Marginal`, `Fully`, or `Ultimate`.
    pub trust_level: String,
    /// The signer identity, if known.
    pub signer: Option<String>,
    /// The GPG/GPGSM key ID. For SSH this is the same value as `fingerprint`.
    pub key: Option<String>,
    /// The signing-key fingerprint, if reported.
    pub fingerprint: Option<String>,
    /// The GPG/GPGSM primary-key fingerprint, if reported; always `null` for SSH.
    pub primary_key_fingerprint: Option<String>,
    /// Human-readable verifier output (stderr for GPG/GPGSM; stdout+stderr for SSH).
    pub output: String,
    /// The verifier's `--status-fd` machine-readable output.
    pub raw_output: String,
    /// Whether Git would accept the signature with the configured minimum trust level.
    pub valid: bool,
}

impl From<Outcome> for VerifyOutcome {
    fn from(o: Outcome) -> Self {
        VerifyOutcome {
            format: format!("{:?}", o.format),
            status: format!("{:?}", o.status),
            trust_level: format!("{:?}", o.trust_level),
            signer: o.signer.as_ref().map(|s| lossy(s.as_slice())),
            key: o.key.as_ref().map(|s| lossy(s.as_slice())),
            fingerprint: o.fingerprint.as_ref().map(|s| lossy(s.as_slice())),
            primary_key_fingerprint: o
                .primary_key_fingerprint
                .as_ref()
                .map(|s| lossy(s.as_slice())),
            output: lossy(&o.output),
            raw_output: lossy(&o.raw_output),
            valid: o.is_valid(),
        }
    }
}

/// A single extra header entry of a commit.
#[derive(Clone)]
#[napi(object)]
pub struct CommitHeaderEntry {
    /// The header field name (without trailing colon).
    pub name: String,
    /// The unfolded header value.
    pub value: String,
}

/// A commit timestamp.
#[napi(object)]
pub struct CommitTime {
    /// Seconds since the Unix epoch.
    pub seconds: i64,
    /// Offset from UTC in **seconds** (consistent with `Signature.timezoneOffset`).
    /// Note that git2/libgit2 reports this in minutes.
    pub offset: i32,
}

/// A single `git interpret-trailers` trailer from a commit message.
#[napi(object)]
pub struct Trailer {
    /// The trailer token, e.g. `Co-authored-by`.
    pub token: String,
    /// The trailer value, with leading and trailing whitespace trimmed and
    /// multi-line values unfolded.
    pub value: String,
}

/// Render a single trailer as `token<separator>value`, mirroring the pasted
/// `Trailer::display`. The default separator is `": "`.
#[napi]
pub fn format_trailer(trailer: Trailer, separator: Option<String>) -> String {
    let sep = separator.unwrap_or_else(|| ": ".to_string());
    format!("{}{}{}", trailer.token, sep, trailer.value)
}

/// Render each trailer as `token<separator>value`, one per line (no trailing
/// newline), mirroring the pasted `Trailers::to_string`. The default separator
/// is `": "`.
#[napi]
pub fn format_trailers(trailers: Vec<Trailer>, separator: Option<String>) -> String {
    let sep = separator.unwrap_or_else(|| ": ".to_string());
    trailers
        .iter()
        .map(|t| format!("{}{}{}", t.token, sep, t.value))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fields to replace when amending a commit; `null` keeps the original value.
#[napi(object)]
pub struct AmendOptions {
    /// A reference to move to the new commit, e.g. `HEAD`. A symbolic `HEAD` is
    /// dereferenced so the branch it points at is updated, like git.
    pub update_ref: Option<String>,
    /// Replacement author identity (name, email **and** time).
    pub author: Option<Signature>,
    /// Replacement committer identity (name, email **and** time).
    pub committer: Option<Signature>,
    /// Replacement `encoding` header value.
    pub message_encoding: Option<String>,
    /// Replacement commit message.
    pub message: Option<String>,
    /// Replacement root tree id.
    pub tree: Option<String>,
}

/// Construct a [`Commit`] wrapper from a gix commit (used by `Repository` / `GitObject`).
pub fn build_commit(
    repo: &Arc<gix::ThreadSafeRepository>,
    commit: gix::Commit,
) -> napi::Result<Commit> {
    Ok(Commit {
        repo: Arc::clone(repo),
        id: commit.id,
    })
}

/// Resolve the signing/verification program for the given signature format, falling back to the
/// format's configured default (`gpg` / `gpgsm` / `ssh-keygen`) when no explicit program is set.
pub(crate) fn signature_program(
    config: &gix::config::Snapshot,
    format: Format,
) -> napi::Result<OsString> {
    let (configured, default) = match format {
        Format::OpenPgp => (config.string(gpg::OpenPgp::PROGRAM), "gpg"),
        Format::X509 => (config.string(gpg::X509::PROGRAM), "gpgsm"),
        Format::Ssh => (config.string(gpg::Ssh::PROGRAM), "ssh-keygen"),
    };
    match configured {
        Some(program) => Ok(OsString::from(lossy(program.as_slice()))),
        None => Ok(OsString::from(default)),
    }
}

/// Compute the [`SignOptions`] for signing, resolving the signing key and program from the
/// repository configuration — equivalent to `git commit -S`.
pub(crate) fn signing_options(repo: &gix::Repository) -> napi::Result<SignOptions> {
    let config = repo.config_snapshot();
    let value = config.string(Gpg::FORMAT);
    let format = Gpg::FORMAT
        .try_into_signature_format(value.as_ref().map(|v| v.as_bstr()))
        .map_err(|e| err("gpg.format", e))?;
    let program = signature_program(&config, format)?;

    let signing_key = match config.string(User::SIGNING_KEY) {
        Some(key) if !key.is_empty() => gix::path::from_bstring(key)
            .map(|p| p.into_os_string())
            .map_err(|e| err("user.signingKey", e))?,
        _ if format == Format::Ssh => default_ssh_key(&config)?.ok_or_else(|| {
            err_str(
                "user.signingKey",
                "user.signingKey or gpg.ssh.defaultKeyCommand must provide an SSH signing key",
            )
        })?,
        _ => {
            let committer = repo
                .committer()
                .ok_or_else(|| {
                    err_str(
                        "committer",
                        "Committer identity is not configured and user.signingKey is unset",
                    )
                })?
                .map_err(|e| err("committer", e))?;
            let mut identity = committer.name.to_vec();
            identity.extend_from_slice(b" <");
            identity.extend_from_slice(committer.email);
            identity.extend_from_slice(b">");
            gix::path::from_bstring(identity)
                .map(|p| p.into_os_string())
                .map_err(|e| err("committer", e))?
        }
    };

    Ok(SignOptions {
        format,
        program,
        program_arguments: Vec::new(),
        signing_key,
        environment: Vec::new(),
    })
}

/// Resolve `gpg.ssh.defaultKeyCommand` to an SSH signing key when `user.signingKey` is unset.
fn default_ssh_key(config: &gix::config::Snapshot) -> napi::Result<Option<OsString>> {
    let Some(program) = config.string(gpg::Ssh::DEFAULT_KEY_COMMAND) else {
        return Ok(None);
    };
    let program = lossy(program.as_slice());
    let output = std::process::Command::new(&program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| err("gpg.ssh.defaultKeyCommand", e))?;
    if !output.status.success() {
        return Err(err_str(
            "gpg.ssh.defaultKeyCommand",
            "command exited with failure",
        ));
    }
    let key = output
        .stdout
        .as_bstr()
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    if is_literal_ssh_key(key).is_none() {
        return Err(err_str(
            "gpg.ssh.defaultKeyCommand",
            "returned an invalid (non-literal) SSH key",
        ));
    }
    Ok(Some(OsString::from(lossy(key.as_ref()))))
}

/// If `path` is relative, resolve it against the repository's working tree (or git dir).
pub(crate) fn resolve_relative_to_repository(repo: &gix::Repository, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        repo.workdir().unwrap_or_else(|| repo.git_dir()).join(path)
    }
}

/// Input for writing a loose commit object.
#[derive(Clone)]
#[napi(object)]
pub struct CommitInput {
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
    pub author: Option<Signature>,
    pub committer: Option<Signature>,
    /// `encoding` header value (e.g. `UTF-8`); `null` omits the header.
    pub encoding: Option<String>,
    /// Extra header fields (custom headers such as `gpgsig`, `Reviewed-by`, …).
    /// Each entry's `value` is unfolded as stored.
    pub extra_headers: Option<Vec<CommitHeaderEntry>>,
}

#[napi]
impl Repository {
    /// Find and decode a commit by id.
    #[napi]
    pub fn find_commit(&self, id: String) -> napi::Result<Commit> {
        let r = thread_local(&self.inner);
        let commit = r
            .find_commit(parse_oid(&id)?)
            .map_err(|e| err("find_commit", e))?;
        build_commit(&self.inner, commit)
    }

    /// Write a loose commit object directly to the object database without moving references.
    #[napi]
    pub fn write_commit(&self, input: CommitInput) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let tree = parse_oid(&input.tree)?;
        let parents = input
            .parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<napi::Result<Vec<_>>>()?;

        let default_author = r.author().and_then(|res| res.ok());
        let default_committer = r.committer().and_then(|res| res.ok());

        let author = match input.author {
            Some(s) => signature_from_dto(&s)?,
            None => default_author
                .map(|s| s.into())
                .ok_or_else(|| err_str("author", "no author configured"))?,
        };
        let committer = match input.committer {
            Some(s) => signature_from_dto(&s)?,
            None => default_committer
                .map(|s| s.into())
                .ok_or_else(|| err_str("committer", "no committer configured"))?,
        };

        let encoding = input.encoding.as_deref().map(|e| e.into());
        let extra_headers = input
            .extra_headers
            .unwrap_or_default()
            .into_iter()
            .map(|h| (h.name.into(), h.value.into()))
            .collect::<Vec<_>>();

        let commit = gix::objs::Commit {
            tree,
            parents: parents.into(),
            author,
            committer,
            encoding,
            message: input.message.into(),
            extra_headers,
        };

        let oid = r
            .write_object(&commit)
            .map_err(|e| err("write_commit", e))?;
        Ok(oid.to_string())
    }

    /// Create a commit on `reference` and return its id.
    #[napi]
    pub fn commit(
        &self,
        reference: String,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<napi::Result<Vec<_>>>()?;
        let id = r
            .commit(reference, message.as_str(), parse_oid(&tree)?, parent_ids)
            .map_err(|e| err("commit", e))?;
        Ok(id.to_string())
    }

    /// Like `commit`, but with explicitly specified `committer` and `author` identities,
    /// which also fixes the author and commit times by hand.
    #[napi]
    pub fn commit_as(
        &self,
        committer: Signature,
        author: Signature,
        reference: String,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<napi::Result<Vec<_>>>()?;
        let mut committer_buf = String::new();
        let mut author_buf = String::new();
        let id = r
            .commit_as(
                signature_ref(&committer, &mut committer_buf),
                signature_ref(&author, &mut author_buf),
                reference,
                message.as_str(),
                parse_oid(&tree)?,
                parent_ids,
            )
            .map_err(|e| err("commit_as", e))?;
        Ok(id.to_string())
    }

    /// Write a commit object with `message`, `tree` and `parents` using the configured
    /// author/committer identities, without updating any reference. Returns its id.
    #[napi]
    pub fn new_commit(
        &self,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<napi::Result<Vec<_>>>()?;
        let commit = r
            .new_commit(message.as_str(), parse_oid(&tree)?, parent_ids)
            .map_err(|e| err("new_commit", e))?;
        Ok(commit.id.to_string())
    }

    /// Like `newCommit`, but with explicitly specified `committer` and `author`
    /// identities, which also fixes the author and commit times by hand.
    #[napi]
    pub fn new_commit_as(
        &self,
        committer: Signature,
        author: Signature,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<napi::Result<Vec<_>>>()?;
        let mut committer_buf = String::new();
        let mut author_buf = String::new();
        let commit = r
            .new_commit_as(
                signature_ref(&committer, &mut committer_buf),
                signature_ref(&author, &mut author_buf),
                message.as_str(),
                parse_oid(&tree)?,
                parent_ids,
            )
            .map_err(|e| err("new_commit_as", e))?;
        Ok(commit.id.to_string())
    }
}

/// Options for [`Repository::commit_all`].
#[napi(object)]
pub struct CommitAllOptions {
    /// The commit message (required).
    pub message: String,
    /// Author identity override. When omitted, git's configured author is used.
    pub author: Option<Signature>,
    /// Committer identity override. When omitted, git's configured committer is used.
    pub committer: Option<Signature>,
    /// Target ref. Defaults to the current branch (or `HEAD` when detached),
    /// matching `git commit`.
    pub reference: Option<String>,
    /// Stage all worktree changes (`git add -A`) before committing. Default `true`.
    pub all: Option<bool>,
    /// Reserved for signing; not yet wired (requires the crypto signing backend).
    pub sign: Option<bool>,
}

#[napi]
impl Repository {
    /// Create a commit on the current branch (or `reference`) that includes every
    /// change in the worktree — the high-level convenience equivalent of
    /// `git commit -a -m` / git_spawn's `Commit::commit`.
    ///
    /// Unlike the lower-level [`Repository::commit`], this derives the tree from
    /// the staged index (staging everything first when `all` is set, via
    /// [`Repository::add_all`]), takes the parent from the target ref's current
    /// tip (none when the ref is unborn), and resolves the ref for you. Returns
    /// the new commit id.
    #[napi]
    pub fn commit_all(&self, options: CommitAllOptions) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let reference = match &options.reference {
            Some(name) => name.clone(),
            None => commit_reference_default(&r),
        };
        let mut index = self.index_or_load()?;
        if options.all.unwrap_or(true) {
            self.add_all(&mut index)?;
        }
        let tree = self.write_tree_from_index(&index)?;
        let parents: Vec<String> = commit_parent_id(&r, &reference)
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        let message = options.message;
        let id = match (options.committer, options.author) {
            (Some(c), Some(a)) => self.commit_as(
                c,
                a,
                reference.clone(),
                message,
                tree.clone(),
                parents.clone(),
            )?,
            (Some(c), None) => self.commit_as(
                c.clone(),
                c,
                reference.clone(),
                message,
                tree.clone(),
                parents.clone(),
            )?,
            (None, Some(a)) => self.commit_as(
                a.clone(),
                a,
                reference.clone(),
                message,
                tree.clone(),
                parents.clone(),
            )?,
            (None, None) => {
                self.commit(reference.clone(), message, tree.clone(), parents.clone())?
            }
        };
        Ok(id)
    }

    /// Async variant of [`Repository::commit_all`].
    #[napi(ts_return_type = "Promise<string>")]
    pub fn commit_all_async(
        &self,
        options: CommitAllOptions,
    ) -> napi::Result<AsyncTask<CommitAllTask>> {
        Ok(AsyncTask::new(CommitAllTask {
            repo: Arc::clone(&self.inner),
            options: Some(options),
        }))
    }
}

/// Resolve the ref to commit on when the caller didn't specify one: the symbolic
/// target of `HEAD` (a branch) when it points at one, otherwise `HEAD` itself
/// (gix writes through to the underlying branch for symbolic `HEAD`).
fn commit_reference_default(r: &gix::Repository) -> String {
    match r.head() {
        Ok(head) => head
            .referent_name()
            .map(|n| n.as_bstr().to_string())
            .unwrap_or_else(|| "HEAD".to_string()),
        Err(_) => "HEAD".to_string(),
    }
}

/// The object id the `reference` (or `HEAD`) currently points at, or `None` when
/// it is unborn — used as the single parent of the new commit.
fn commit_parent_id(r: &gix::Repository, reference: &str) -> Option<gix::ObjectId> {
    r.find_reference(reference)
        .ok()
        .and_then(|mut rf| rf.peel_to_id_in_place().ok().map(|id| id.detach()))
}

pub struct CommitAllTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) options: Option<CommitAllOptions>,
}
impl Task for CommitAllTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> napi::Result<String> {
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };
        let options = self
            .options
            .take()
            .expect("commit_all task options must be set");
        repo.commit_all(options).map_err(Into::into)
    }
    fn resolve(&mut self, _env: Env, output: String) -> napi::Result<String> {
        Ok(output)
    }
}
