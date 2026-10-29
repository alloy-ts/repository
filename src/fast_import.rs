//! Git fast-import: stream generation and native application.
//!
//! `git fast-import` reads a mixed command/data stream and writes objects and
//! refs into a repository. It lets a caller create commits, trees, blobs and
//! tags without a worktree or an index, in one pass, from any source: a
//! migration, a synthetic history, or a bundle received over the wire. The
//! mirror image of this module is [`crate::fast_export`], which produces such a
//! stream from a repository.
//!
//! # How this module supports it
//!
//! There is no `gix` implementation of fast-import, so this module provides
//! three layers, all free of a `git` binary:
//!
//! 1. **Stream generation** — [`GitFastImporter`] writes a fast-import stream
//!    to any [`Write`]. This is what a frontend (a converter, a migration
//!    script) produces; [`FastImporter`] is its incremental, JS-facing form.
//! 2. **Native application** — [`compute_fast_import`] parses a stream and
//!    applies it through `gix`: blobs and commits go to the object database as
//!    loose objects, trees are built with `Repository::edit_tree`, and refs are
//!    updated in a transaction with the same fast-forward rule git uses
//!    ([`FastImportOptions::force`] overrides it).
//! 3. **Git passthrough** — [`fast_import_via_git`] spawns the real
//!    `git fast-import` and hands it the stream, for git's exact semantics
//!    (packfiles, `--stats`, `--max-pack-size`) and for the stream dialects
//!    this module's parser rejects.
//!
//! # napi surface
//!
//! - [`FastImporter`] — JS class: build a stream (`blob`, `startCommit`,
//!   `addFile`, `deleteFile`, `deleteAll`, `copyFile`, `renameFile`,
//!   `finishCommit`, `tag`, `reset`, `progress`, `checkpoint`, `feature`,
//!   `done`, `toBuffer`, `importInto`).
//! - [`fast_import`] / [`fast_import_async`] — apply a stream to the repository
//!   at a path.
//! - [`fast_import_via_git`] — pipe a stream through the real `git fast-import`.
//! - `Repository.prototype.fastImport` / `fastImportAsync` — the same
//!   application on an open repository (the export half lives in
//!   [`crate::fast_export`]).
//!
//! # Limitations of the native importer
//!
//! The stream dialect is large; this module implements the subset that real
//! frontends emit and rejects the rest instead of importing it wrongly:
//!
//! - Supported: `blob`, `commit` (with `mark`, `author`, `committer`,
//!   `encoding`, `data`, `from`, `merge`, `M`, `D`, `C`, `R`, `deleteall`),
//!   `tag`, `reset`, `checkpoint`, `progress`, `feature`, `option`, `done`,
//!   plus stream comments and both `data` forms (count and `<<`-delimited).
//! - Rejected: `gpgsig`, `notemodify` (`N`), `alias`, and the interactive
//!   `get-mark`, `cat-blob` and `ls` commands (they need a bidirectional
//!   backend — use [`fast_import_via_git`]).
//! - Paths are matched through `edit_tree`, which takes UTF-8 `&str` paths;
//!   non-UTF-8 paths are converted lossily.
//! - Objects are written loose, so a large import should be followed by
//!   `git gc` / a repack; `--max-pack-size`, `--depth` and `--big-file-threshold`
//!   only exist on the git-binary path.
//! - `C`/`R` resolve their source against the commit's *base* tree, not against
//!   the partially edited tree.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

use chrono::{DateTime, TimeZone};
use gix::bstr::BString;
use gix::objs::WriteTo;
use napi::bindgen_prelude::{AsyncTask, Buffer};
use napi::{Env, Task};
use napi_derive::napi;

use crate::entry::mode_to_entry_kind;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::{Repository, find_git, lossy};

/// One file-level change inside a `commit` command.
#[derive(Debug, Clone)]
pub enum CommitOp {
    /// `M <mode> :<mark> <path>` — content supplied by an earlier `blob`.
    Modify {
        /// Entry mode: `100644`, `100755`, `120000`, `160000`, `040000`.
        mode: String,
        /// Mark returned by [`GitFastImporter::write_blob`].
        mark: usize,
        /// Repository-relative path.
        path: String,
    },
    /// `M <mode> <oid> <path>` — content already in the object database.
    ModifyOid {
        /// Entry mode.
        mode: String,
        /// Full hex object id of an existing blob, tree or commit.
        oid: String,
        /// Repository-relative path.
        path: String,
    },
    /// `D <path>` — remove a file or directory.
    Delete {
        /// Repository-relative path.
        path: String,
    },
    /// `C <source> <destination>`.
    Copy {
        /// Source path.
        source: String,
        /// Destination path.
        destination: String,
    },
    /// `R <source> <destination>`.
    Rename {
        /// Source path.
        source: String,
        /// Destination path.
        destination: String,
    },
    /// `deleteall` — reset the branch to an empty tree before applying the
    /// remaining ops.
    DeleteAll,
}

/// Generates a git fast-import stream.
///
/// Write the stream to any [`Write`] implementor, then pipe it to `git fast-import`.
#[derive(Debug)]
pub struct GitFastImporter<T: Write> {
    output: T,
    current_mark: usize,
    branch: String,
    initial_parent: Option<String>,
    author: String,
}

/// Builder for constructing a single commit.
///
/// Created by [`GitFastImporter::start_commit`]. Add files with [`add_file`](Self::add_file),
/// then call [`finish`](Self::finish) to write the commit. Dropping without calling `finish`
/// discards the commit (but any blobs written remain in the stream).
#[allow(dead_code)] // Rust-side API; the crate's own consumers are its tests.
pub struct CommitBuilder<'a, T: Write> {
    importer: &'a mut GitFastImporter<T>,
    message: String,
    timestamp: String,
    files: Vec<(usize, String)>,
}

impl<'a, T: Write> CommitBuilder<'a, T> {
    /// Adds a file to this commit.
    pub fn add_file(&mut self, path: &str, data: &[u8]) -> io::Result<&mut Self> {
        let mark = self.importer.write_blob(data)?;
        self.files.push((mark, path.to_string()));
        Ok(self)
    }

    /// Writes the commit to the output stream.
    pub fn finish(self) -> io::Result<()> {
        let ops: Vec<CommitOp> = self
            .files
            .iter()
            .map(|(mark, path)| CommitOp::Modify {
                mode: "100644".to_string(),
                mark: *mark,
                path: path.clone(),
            })
            .collect();
        self.importer
            .write_commit(&self.message, &self.timestamp, &ops, None, &[])?;
        Ok(())
    }
}

impl<T: Write> GitFastImporter<T> {
    /// Creates a new importer that writes to the given output.
    ///
    /// # Arguments
    ///
    /// * `output` - The writer to output the fast-import stream to
    /// * `branch` - The branch name to create commits on (e.g., `"main"`)
    /// * `initial_parent` - Optional parent ref for the first commit (e.g., `"refs/heads/main"`)
    /// * `author` - The committer string (e.g., [`GITHUB_BOT_AUTHOR`])
    pub fn new(output: T, branch: String, initial_parent: Option<String>, author: String) -> Self {
        GitFastImporter {
            output,
            current_mark: 0,
            branch,
            initial_parent,
            author,
        }
    }

    /// Starts building a new commit.
    ///
    /// # Arguments
    ///
    /// * `message` - The commit message
    /// * `timestamp` - The commit timestamp (e.g., `Utc::now()`)
    #[allow(dead_code)] // Rust-side API; see [`CommitBuilder`].
    pub fn start_commit(
        &mut self,
        message: &str,
        timestamp: DateTime<impl TimeZone<Offset: std::fmt::Display>>,
    ) -> CommitBuilder<'_, T> {
        CommitBuilder {
            importer: self,
            message: message.to_string(),
            timestamp: timestamp.format("%s %z").to_string(),
            files: Vec::new(),
        }
    }

    /// Writes the `done` command to finalize the stream.
    pub fn finish(&mut self) -> io::Result<()> {
        writeln!(self.output, "done")?;
        Ok(())
    }

    /// Writes a standalone `blob` and returns the mark it was assigned.
    pub fn write_blob(&mut self, data: &[u8]) -> io::Result<usize> {
        self.current_mark += 1;
        let mark = self.current_mark;
        writeln!(self.output, "blob")?;
        writeln!(self.output, "mark :{mark}")?;
        writeln!(self.output, "data {}", data.len())?;
        self.output.write_all(data)?;
        writeln!(self.output)?;
        Ok(mark)
    }

    /// Writes a `commit` and returns the mark it was assigned.
    ///
    /// `from` overrides [`Self::initial_parent`] for this commit; when both are
    /// absent the branch continues from its own tip (or starts a root commit).
    pub fn write_commit(
        &mut self,
        message: &str,
        timestamp: &str,
        ops: &[CommitOp],
        from: Option<&str>,
        merges: &[String],
    ) -> io::Result<usize> {
        self.current_mark += 1;
        let mark = self.current_mark;
        writeln!(self.output, "commit refs/heads/{}", self.branch)?;
        writeln!(self.output, "mark :{mark}")?;
        writeln!(self.output, "{} {}", self.author, timestamp)?;

        writeln!(self.output, "data {}", message.len())?;
        writeln!(self.output, "{}", message)?;

        if let Some(parent) = from
            .map(str::to_string)
            .or_else(|| self.initial_parent.take())
        {
            writeln!(self.output, "from {parent}")?;
        }
        for merge in merges {
            writeln!(self.output, "merge {merge}")?;
        }
        write_ops(&mut self.output, ops)?;
        writeln!(self.output)?;
        Ok(mark)
    }

    /// Writes an annotated `tag` referring to `from`.
    pub fn write_tag(
        &mut self,
        name: &str,
        from: &str,
        tagger: &str,
        message: &str,
    ) -> io::Result<()> {
        writeln!(self.output, "tag {name}")?;
        writeln!(self.output, "from {from}")?;
        writeln!(self.output, "{tagger}")?;
        writeln!(self.output, "data {}", message.len())?;
        writeln!(self.output, "{message}")?;
        Ok(())
    }

    /// Writes a `reset`, which moves a branch or creates a lightweight tag.
    pub fn write_reset(&mut self, reference: &str, from: Option<&str>) -> io::Result<()> {
        writeln!(self.output, "reset {reference}")?;
        if let Some(from) = from {
            writeln!(self.output, "from {from}")?;
        }
        Ok(())
    }

    /// Writes a `progress` command; `git fast-import` echoes it verbatim.
    pub fn write_progress(&mut self, message: &str) -> io::Result<()> {
        writeln!(self.output, "progress {message}")
    }

    /// Writes a `checkpoint`, closing the current packfile and flushing refs.
    pub fn write_checkpoint(&mut self) -> io::Result<()> {
        writeln!(self.output, "checkpoint")
    }

    /// Writes a `feature` command, asserting backend support.
    pub fn write_feature(&mut self, feature: &str) -> io::Result<()> {
        writeln!(self.output, "feature {feature}")
    }

    /// Writes a `option` command, e.g. `option date-format=raw`.
    pub fn write_option(&mut self, option: &str) -> io::Result<()> {
        writeln!(self.output, "option {option}")
    }

    /// Writes a stream comment. Ignored by `git fast-import`; useful when
    /// debugging a frontend.
    pub fn write_comment(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.output, "# {text}")
    }

    /// The bytes written so far. Only meaningful when `T` is a buffer.
    pub fn buffer(&self) -> Option<&[u8]>
    where
        T: AsRef<[u8]>,
    {
        Some(self.output.as_ref())
    }
}

/// Emits file-level ops in the order they were recorded.
fn write_ops(output: &mut impl Write, ops: &[CommitOp]) -> io::Result<()> {
    for op in ops {
        match op {
            CommitOp::Modify { mode, mark, path } => {
                // Empty paths cannot be represented in a tree; skip them so a
                // frontend can pass `""` for "no path" without corrupting the stream.
                if path.is_empty() {
                    continue;
                }
                writeln!(output, "M {mode} :{mark} {}", quote_path(path))?;
            }
            CommitOp::ModifyOid { mode, oid, path } => {
                if path.is_empty() {
                    continue;
                }
                writeln!(output, "M {mode} {oid} {}", quote_path(path))?;
            }
            CommitOp::Delete { path } => writeln!(output, "D {}", quote_path(path))?,
            CommitOp::Copy {
                source,
                destination,
            } => writeln!(
                output,
                "C {} {}",
                quote_path(source),
                quote_path(destination)
            )?,
            CommitOp::Rename {
                source,
                destination,
            } => writeln!(
                output,
                "R {} {}",
                quote_path(source),
                quote_path(destination)
            )?,
            CommitOp::DeleteAll => writeln!(output, "deleteall")?,
        }
    }
    Ok(())
}

/// Render a path the way fast-import wants it: bare bytes when possible,
/// otherwise a C-style quoted string.
///
/// Quoting is required when the path starts with `"`, contains `LF`, or
/// contains a space (copy/rename sources are ambiguous otherwise).
pub(crate) fn quote_path(path: &str) -> String {
    if !path
        .chars()
        .any(|c| matches!(c, '"' | '\\' | ' ' | '\t' | '\n' | '\r'))
    {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for ch in path.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Stream parsing primitives (shared by the importer)
// ---------------------------------------------------------------------------

/// Byte cursor over a fast-import/export stream.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    /// Next line without its trailing `LF`; `None` at end of stream.
    fn take_line(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        match self.data[start..].iter().position(|b| *b == b'\n') {
            Some(offset) => {
                let end = start + offset;
                self.pos = end + 1;
                Some(&self.data[start..end])
            }
            None => {
                self.pos = self.data.len();
                Some(&self.data[start..])
            }
        }
    }

    /// Next line that is not a stream comment.
    fn take_command_line(&mut self) -> Option<&'a [u8]> {
        loop {
            let line = self.take_line()?;
            if line.first() == Some(&b'#') {
                continue;
            }
            return Some(line);
        }
    }

    /// Next non-comment line, without consuming it. `reset` is terminated by
    /// anything other than `from`, so the following command must stay readable.
    fn peek_command_line(&mut self) -> Option<&'a [u8]> {
        let saved = self.pos;
        let line = self.take_command_line();
        self.pos = saved;
        line
    }

    fn take_bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        if self.pos + count > self.data.len() {
            return None;
        }
        let out = &self.data[self.pos..self.pos + count];
        self.pos += count;
        Some(out)
    }

    /// Consume the optional `LF` that may follow a counted `data` payload.
    fn skip_newline(&mut self) {
        if self.data.get(self.pos) == Some(&b'\n') {
            self.pos += 1;
        }
    }
}

/// Split a command line into its command word and the remainder.
fn split_command(line: &[u8]) -> (String, &[u8]) {
    match line.iter().position(|b| *b == b' ') {
        Some(index) => (lossy(&line[..index]), &line[index + 1..]),
        None => (lossy(line), &[]),
    }
}

/// Read the payload of a `data` command whose header line was already taken.
fn read_data(cursor: &mut Cursor, line: &[u8]) -> napi::Result<Vec<u8>> {
    let spec = &line[5..];
    if let Some(delim) = spec.strip_prefix(b"<<".as_slice()) {
        // Delimited form: every line up to one equal to `delim` is payload,
        // and the `LF` after each line belongs to the payload.
        let mut out = Vec::new();
        loop {
            let next = cursor
                .take_line()
                .ok_or_else(|| err_str("fast_import", "unterminated delimited data"))?;
            if next == delim {
                return Ok(out);
            }
            out.extend_from_slice(next);
            out.push(b'\n');
        }
    }
    let text = lossy(spec).trim().to_string();
    let count: usize = text
        .parse()
        .map_err(|_| err_str("fast_import", format!("invalid data byte count '{text}'")))?;
    let bytes = cursor
        .take_bytes(count)
        .ok_or_else(|| err_str("fast_import", "stream truncated inside data"))?;
    cursor.skip_newline();
    Ok(bytes.to_vec())
}

/// Parse `:idnum` into a mark number.
fn parse_mark(text: &str) -> napi::Result<usize> {
    let digits = text.strip_prefix(':').unwrap_or(text);
    let mark: usize = digits
        .parse()
        .map_err(|_| err_str("fast_import", format!("invalid mark '{text}'")))?;
    if mark == 0 {
        return Err(err_str("fast_import", "mark 0 is reserved"));
    }
    Ok(mark)
}

/// The 40-zero object id, which fast-import reads as "remove this branch".
fn is_null_sha(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b == b'0')
}

/// Parse a quoted or bare path (used by `D`, `C`, `R` and the path of `M`).
fn parse_path(bytes: &[u8]) -> napi::Result<Vec<u8>> {
    if bytes.first() == Some(&b'"') {
        return parse_quoted(bytes);
    }
    Ok(bytes.to_vec())
}

/// Decode a C-style quoted string, including the escape sequences the
/// fast-import format defines.
fn parse_quoted(bytes: &[u8]) -> napi::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok(out),
            b'\\' => {
                if index + 1 >= bytes.len() {
                    break;
                }
                index += 1;
                match bytes[index] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'v' => out.push(0x0b),
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    b'0'..=b'7' => {
                        let digits: Vec<u8> = bytes[index..]
                            .iter()
                            .take_while(|b| matches!(b, b'0'..=b'7'))
                            .take(3)
                            .copied()
                            .collect();
                        let mut value = 0u8;
                        for digit in &digits {
                            value = value * 8 + (digit - b'0');
                        }
                        out.push(value);
                        index += digits.len() - 1;
                    }
                    other => out.push(other),
                }
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    Err(err_str("fast_import", "unterminated quoted path"))
}

/// Split `<path> SP <path>` (the operands of `C` and `R`).
fn parse_two_paths(bytes: &[u8]) -> napi::Result<(Vec<u8>, Vec<u8>)> {
    if bytes.first() == Some(&b'"') {
        let end = quoted_len(bytes)?;
        let rest = &bytes[end..];
        let rest = rest.strip_prefix(b" ".as_slice()).unwrap_or(rest);
        return Ok((parse_quoted(&bytes[..end])?, parse_path(rest)?));
    }
    match bytes.iter().position(|b| *b == b' ') {
        Some(index) => Ok((bytes[..index].to_vec(), parse_path(&bytes[index + 1..])?)),
        None => Err(err_str("fast_import", "path pair is missing a space")),
    }
}

/// Length of the quoted string starting at `bytes[0]`, including both quotes.
fn quoted_len(bytes: &[u8]) -> napi::Result<usize> {
    let mut index = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok(index + 1),
            b'\\' => index += 2,
            _ => index += 1,
        }
    }
    Err(err_str("fast_import", "unterminated quoted path"))
}

/// Normalize the fast-import short modes git accepts (`644`, `755`, `40000`).
fn normalize_mode(mode: &str) -> String {
    match mode {
        "644" => "100644".to_string(),
        "755" => "100755".to_string(),
        "40000" => "040000".to_string(),
        other => other.to_string(),
    }
}

/// How the stream spells the `<when>` of an `author`, `committer` or `tagger`
/// command (`--date-format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateFormat {
    /// `<seconds> <±hhmm>` — git's native format and the default.
    Raw,
    /// Like [`Raw`](Self::Raw), but without the sanity checks git performs on
    /// the epoch and the offset.
    RawPermissive,
    /// `Tue Feb 6 11:22:18 2007 -0500`.
    Rfc2822,
    /// The literal `now`: every identity gets the current time.
    Now,
}

impl DateFormat {
    fn parse(text: &str) -> napi::Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "raw" => Ok(Self::Raw),
            "raw-permissive" => Ok(Self::RawPermissive),
            "rfc2822" => Ok(Self::Rfc2822),
            "now" => Ok(Self::Now),
            other => Err(err_str(
                "dateFormat",
                format!(
                    "unknown date format '{other}' (expected raw, raw-permissive, rfc2822 or now)"
                ),
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::RawPermissive => "raw-permissive",
            Self::Rfc2822 => "rfc2822",
            Self::Now => "now",
        }
    }
}

/// How to treat a signature carried by a `commit` or `tag` command
/// (`--signed-commits` / `--signed-tags`).
///
/// The modes git offers that depend on *verifying* a signature are rejected:
/// the native importer has no crypto, so those streams have to go through
/// [`fast_import_via_git`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignMode {
    /// Import the signature as it is.
    Verbatim,
    /// Import the signature and report a warning.
    WarnVerbatim,
    /// Fail the import.
    Abort,
    /// Drop the signature silently.
    Strip,
    /// Drop the signature and report a warning.
    WarnStrip,
    /// Strip the signature only when it fails verification (git binary only).
    StripIfInvalid,
    /// Replace an invalid signature with a fresh one (git binary only).
    SignIfInvalid(Option<String>),
    /// Fail the import when the signature cannot be verified (git binary only).
    AbortIfInvalid,
}

impl SignMode {
    fn parse(text: &str, what: &str) -> napi::Result<Self> {
        let (name, key) = match text.split_once('=') {
            Some((name, key)) => (name, Some(key.to_string())),
            None => (text, None),
        };
        let mode = match name.to_ascii_lowercase().as_str() {
            "verbatim" => Self::Verbatim,
            "warn-verbatim" => Self::WarnVerbatim,
            "abort" => Self::Abort,
            "strip" => Self::Strip,
            "warn-strip" => Self::WarnStrip,
            "strip-if-invalid" => Self::StripIfInvalid,
            "sign-if-invalid" => Self::SignIfInvalid(key),
            "abort-if-invalid" => Self::AbortIfInvalid,
            other => {
                return Err(err_str(
                    what,
                    format!(
                        "unknown {what} mode '{other}' (expected verbatim, warn-verbatim, abort, strip, warn-strip, strip-if-invalid, sign-if-invalid or abort-if-invalid)"
                    ),
                ));
            }
        };
        Ok(mode)
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Verbatim => "verbatim",
            Self::WarnVerbatim => "warn-verbatim",
            Self::Abort => "abort",
            Self::Strip => "strip",
            Self::WarnStrip => "warn-strip",
            Self::StripIfInvalid => "strip-if-invalid",
            Self::SignIfInvalid(_) => "sign-if-invalid",
            Self::AbortIfInvalid => "abort-if-invalid",
        }
    }

    /// Whether the signature is written to the object.
    fn keeps(&self) -> bool {
        matches!(self, Self::Verbatim | Self::WarnVerbatim)
    }

    /// Whether merely seeing a signature is an error.
    fn aborts(&self) -> bool {
        matches!(self, Self::Abort | Self::AbortIfInvalid)
    }

    /// Whether the mode cannot be honored without verifying the signature.
    fn needs_verification(&self) -> bool {
        matches!(
            self,
            Self::StripIfInvalid | Self::SignIfInvalid(_) | Self::AbortIfInvalid
        )
    }
}

/// Seconds since the epoch, as fast-import's date formats count them.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse the `<when>` of an identity line according to `format`.
fn parse_when(text: &str, format: DateFormat) -> napi::Result<(i64, i32)> {
    match format {
        DateFormat::Now => Ok((now_seconds(), 0)),
        DateFormat::Rfc2822 => {
            let date = chrono::DateTime::parse_from_rfc2822(text.trim()).map_err(|e| {
                err_str("dateFormat", format!("invalid rfc2822 date '{text}': {e}"))
            })?;
            Ok((date.timestamp(), date.offset().local_minus_utc()))
        }
        DateFormat::Raw | DateFormat::RawPermissive => {
            let parts: Vec<&str> = text.split(' ').collect();
            let raw = parts.first().unwrap_or(&"0");
            let seconds: i64 = match raw.parse() {
                Ok(seconds) => seconds,
                // `--date-format=raw-permissive` exists for exactly this: bogus
                // values are imported instead of rejected.
                Err(_) if format == DateFormat::RawPermissive => 0,
                Err(_) => {
                    return Err(err_str(
                        "dateFormat",
                        format!("invalid identity timestamp '{raw}'"),
                    ));
                }
            };
            let offset = match parts.get(1) {
                Some(tz) => match parse_offset(tz) {
                    Ok(offset) => offset,
                    Err(e) if format == DateFormat::Raw => return Err(e),
                    Err(_) => 0,
                },
                None => 0,
            };
            Ok((seconds, offset))
        }
    }
}

/// Parse `Name <email> <when>` into a gix signature.
///
/// `format` decides how `<when>` is read; see [`DateFormat`].
fn parse_ident(line: &str, format: DateFormat) -> napi::Result<gix::actor::Signature> {
    let bytes = line.as_bytes();
    let start = bytes.iter().position(|b| *b == b'<').ok_or_else(|| {
        err_str(
            "fast_import",
            format!("invalid identity '{line}': missing '<'"),
        )
    })?;
    let end = bytes.iter().position(|b| *b == b'>').ok_or_else(|| {
        err_str(
            "fast_import",
            format!("invalid identity '{line}': missing '>'"),
        )
    })?;
    if end < start {
        return Err(err_str("fast_import", format!("invalid identity '{line}'")));
    }
    let name = line[..start].trim();
    let email = &line[start + 1..end];
    let rest = line[end + 1..].trim();
    let (seconds, offset) = parse_when(rest, format)?;
    Ok(gix::actor::Signature {
        name: name.into(),
        email: email.into(),
        time: gix::date::Time { seconds, offset },
    })
}

/// Parse a `±hhmm` timezone into seconds east of UTC.
fn parse_offset(text: &str) -> napi::Result<i32> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 || !matches!(bytes[0], b'+' | b'-') {
        return Err(err_str(
            "fast_import",
            format!("invalid timezone offset '{text}'"),
        ));
    }
    let hours: i32 = lossy(&bytes[1..3])
        .parse()
        .map_err(|_| err_str("fast_import", format!("invalid timezone offset '{text}'")))?;
    let minutes: i32 = lossy(&bytes[3..5])
        .parse()
        .map_err(|_| err_str("fast_import", format!("invalid timezone offset '{text}'")))?;
    let total = hours * 3600 + minutes * 60;
    Ok(if bytes[0] == b'-' { -total } else { total })
}

/// Render a signature in git's raw identity format (`Name <email> <ts> <tz>`).
///
/// Shared with [`crate::fast_export`], which emits the same lines.
pub(crate) fn ident_line(sig: &gix::actor::SignatureRef<'_>) -> napi::Result<String> {
    let time = sig.time().map_err(|e| err("identity time", e))?;
    Ok(format!(
        "{} <{}> {}",
        lossy(sig.name),
        lossy(sig.email),
        time
    ))
}

// ---------------------------------------------------------------------------
// Native import (backend side)
// ---------------------------------------------------------------------------

/// Where the content referenced by a `filemodify` comes from.
#[derive(Debug, Clone)]
enum DataRef {
    /// A mark defined earlier in the stream.
    Mark(usize),
    /// An object that already exists in the repository.
    Oid(gix::ObjectId),
    /// Bytes carried inline by the command.
    Inline(Vec<u8>),
}

/// A parsed `commit` command, before it is applied.
#[derive(Default)]
struct CommitDraft {
    mark: Option<usize>,
    author: Option<String>,
    committer: Option<String>,
    encoding: Option<String>,
    message: Vec<u8>,
    from: Option<String>,
    merges: Vec<String>,
    ops: Vec<FileOp>,
    /// `gpgsig <hash-algo> <format>` plus its payload, when the commit is
    /// signed.
    gpgsig: Option<(String, String, Vec<u8>)>,
}

#[derive(Debug)]
enum FileOp {
    Modify {
        mode: String,
        data: DataRef,
        path: Vec<u8>,
    },
    Delete {
        path: Vec<u8>,
    },
    DeleteAll,
}

/// Counters and results reported back from an import run.
#[derive(Default)]
struct ImportStats {
    blobs: usize,
    commits: usize,
    tags: usize,
    refs_updated: Vec<String>,
    refs_skipped: Vec<String>,
    refs_deleted: Vec<String>,
    progress: Vec<String>,
    marks: Vec<(usize, gix::ObjectId)>,
    /// What the `warn-*` signature modes would have printed.
    warnings: Vec<String>,
}

/// A file-level change whose content has already been resolved to an object id.
///
/// [`ObjectSink::edit_tree`] receives a whole batch of these so a sink that has
/// no `gix::Repository` (a bare object database) can build the tree without
/// borrowing one.
#[derive(Debug)]
pub(crate) enum TreeOp {
    /// `deleteall`: discard the base tree and start from the empty one.
    DeleteAll,
    /// `D <path>`.
    Delete { path: Vec<u8> },
    /// `M <mode> <oid> <path>`.
    Modify {
        mode: String,
        oid: gix::ObjectId,
        path: Vec<u8>,
    },
}

/// Everything the importer needs from the place it writes to.
///
/// The importer is written against this trait instead of against
/// [`gix::Repository`] so one parser can feed two kinds of destination:
///
/// - [`RepositorySink`] — objects go to the repository's object database and
///   references are updated in place, exactly like `git fast-import`.
/// - a store-only sink (see [`crate::object_store::loose::LooseObjectStore::import_stream`] and
///   [`crate::object_store::memory::MemoryObjectStore::import_stream`])
///   — objects go to an object database that has no references at all. Such a
///   sink records what the stream asked for and lets the caller apply it later
///   with [`crate::ref_store::RefStore::import_refs`].
pub(crate) trait ObjectSink {
    /// Hash kind of the destination (`sha1` or `sha256`).
    fn object_hash(&self) -> gix::hash::Kind;
    /// Write blob content and return its id.
    fn write_blob(&self, data: &[u8]) -> napi::Result<gix::ObjectId>;
    /// Write an already serialized object of `kind`.
    fn write_object(&self, kind: gix::object::Kind, data: &[u8]) -> napi::Result<gix::ObjectId>;
    /// Read an object back: its kind and raw, decompressed content.
    fn find_object(&self, id: gix::ObjectId) -> napi::Result<Option<(gix::object::Kind, Vec<u8>)>>;
    /// Apply `ops` to the tree `base` and write out the resulting tree.
    fn edit_tree(&self, base: gix::ObjectId, ops: &[TreeOp]) -> napi::Result<gix::ObjectId>;
    /// The tree a commit points at.
    fn commit_tree(&self, id: gix::ObjectId) -> napi::Result<gix::ObjectId>;
    /// Peel `id` to the commit it refers to; `None` when it is not a commit-ish.
    fn peel_to_commit(&self, id: gix::ObjectId) -> napi::Result<Option<gix::ObjectId>>;
    /// Resolve a revision expression the stream refers to; `None` when it does
    /// not exist in this destination.
    fn resolve_rev(&self, spec: &str) -> napi::Result<Option<gix::ObjectId>>;
    /// The identity used when a `tag` command carries no `tagger`.
    fn committer(&self) -> Option<gix::actor::Signature>;
    /// Target of a reference, or `None` when the destination has no references.
    fn current_target(&self, name: &str) -> Option<gix::ObjectId>;
    /// Whether `old` is reachable from `new` — the fast-forward test.
    fn is_ancestor(&self, old: gix::ObjectId, new: gix::ObjectId) -> napi::Result<bool>;
    /// Move a reference; called only after the fast-forward check passed.
    fn update_ref(&mut self, name: &str, id: gix::ObjectId, reflog: &str) -> napi::Result<()>;
    /// Delete a reference; `true` when something was deleted.
    fn delete_ref(&mut self, name: &str) -> napi::Result<bool>;
    /// Resolve a marks file path, honoring `--relative-marks`, which places it
    /// in `<git-dir>/info/fast-import`.
    fn marks_path(&self, path: &str, relative: bool) -> napi::Result<std::path::PathBuf>;
}

/// An [`ObjectSink`] backed by a real repository: objects go to its object
/// database and refs are updated in place.
pub(crate) struct RepositorySink<'r> {
    r: &'r gix::Repository,
}

impl<'r> RepositorySink<'r> {
    pub(crate) fn new(r: &'r gix::Repository) -> Self {
        RepositorySink { r }
    }
}

impl<'r> ObjectSink for RepositorySink<'r> {
    fn object_hash(&self) -> gix::hash::Kind {
        self.r.object_hash()
    }

    fn write_blob(&self, data: &[u8]) -> napi::Result<gix::ObjectId> {
        Ok(self
            .r
            .write_blob(data)
            .map_err(|e| err("write_blob", e))?
            .detach())
    }

    fn write_object(&self, kind: gix::object::Kind, data: &[u8]) -> napi::Result<gix::ObjectId> {
        Ok(gix::objs::Write::write_buf(&self.r.objects, kind, data)
            .map_err(|e| err("write_object", e))?)
    }

    fn find_object(&self, id: gix::ObjectId) -> napi::Result<Option<(gix::object::Kind, Vec<u8>)>> {
        let mut buf = Vec::new();
        let data = gix::objs::Find::try_find(&self.r.objects, &id, &mut buf)
            .map_err(|e| err("find_object", e))?;
        Ok(data.map(|data| (data.kind, data.data.to_vec())))
    }

    fn edit_tree(&self, base: gix::ObjectId, ops: &[TreeOp]) -> napi::Result<gix::ObjectId> {
        let empty = gix::ObjectId::empty_tree(self.r.object_hash());
        let mut editor = self.r.edit_tree(base).map_err(|e| err("edit_tree", e))?;
        for op in ops {
            match op {
                TreeOp::DeleteAll => {
                    editor = self.r.edit_tree(empty).map_err(|e| err("edit_tree", e))?;
                }
                TreeOp::Delete { path } => {
                    editor
                        .remove(&lossy(path))
                        .map_err(|e| err("edit_tree.remove", e))?;
                }
                TreeOp::Modify { mode, oid, path } => {
                    let kind = mode_to_entry_kind(&normalize_mode(mode))?;
                    editor
                        .upsert(&lossy(path), kind, *oid)
                        .map_err(|e| err("edit_tree.upsert", e))?;
                }
            }
        }
        Ok(editor
            .write()
            .map_err(|e| err("edit_tree.write", e))?
            .detach())
    }

    fn commit_tree(&self, id: gix::ObjectId) -> napi::Result<gix::ObjectId> {
        Ok(self
            .r
            .find_commit(id)
            .map_err(|e| err("find_commit", e))?
            .tree_id()
            .map_err(|e| err("tree_id", e))?
            .detach())
    }

    fn peel_to_commit(&self, id: gix::ObjectId) -> napi::Result<Option<gix::ObjectId>> {
        peel_to_commit(self.r, id)
    }

    fn resolve_rev(&self, spec: &str) -> napi::Result<Option<gix::ObjectId>> {
        Ok(Some(
            self.r
                .rev_parse_single(spec)
                .map_err(|e| err("rev_parse", e))?
                .detach(),
        ))
    }

    fn committer(&self) -> Option<gix::actor::Signature> {
        self.r.committer()?.ok()?.to_owned().ok()
    }

    fn current_target(&self, name: &str) -> Option<gix::ObjectId> {
        current_target(self.r, name)
    }

    fn is_ancestor(&self, old: gix::ObjectId, new: gix::ObjectId) -> napi::Result<bool> {
        is_ancestor(self.r, old, new)
    }

    fn update_ref(&mut self, name: &str, id: gix::ObjectId, reflog: &str) -> napi::Result<()> {
        let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("ref_name", e))?;
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            full_name,
            gix::refs::Target::Object(id),
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: false,
                message: reflog.into(),
            },
        );
        self.r
            .edit_reference(edit)
            .map(|_| ())
            .map_err(|e| err("edit_reference", e))
    }

    fn delete_ref(&mut self, name: &str) -> napi::Result<bool> {
        let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("ref_name", e))?;
        let edit = gix::refs::transaction::RefEdit::delete_with_log(
            full_name,
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::RefLog::AndReference,
        );
        Ok(self.r.edit_reference(edit).is_ok())
    }

    fn marks_path(&self, path: &str, relative: bool) -> napi::Result<std::path::PathBuf> {
        Ok(crate::fast_export::marks_path(self.r, relative, path))
    }
}

/// Applies a fast-import stream to an [`ObjectSink`].
struct Executor<'a, S: ObjectSink> {
    sink: &'a mut S,
    marks: HashMap<usize, gix::ObjectId>,
    branches: HashMap<String, gix::ObjectId>,
    deleted: Vec<String>,
    stats: ImportStats,
    force: bool,
    reflog_message: String,
    /// `--signed-commits`.
    signed_commits: SignMode,
    /// `--signed-tags`.
    signed_tags: SignMode,
    /// `--date-format`.
    date_format: DateFormat,
    /// `--relative-marks`, switchable mid-stream by `feature`.
    relative_marks: bool,
    /// Marks file written when the stream completes (`--export-marks`, or an
    /// in-stream `feature export-marks`).
    export_marks: Option<std::path::PathBuf>,
    /// Whether in-stream `feature import-marks` is allowed
    /// (`--allow-unsafe-features`).
    allow_unsafe_features: bool,
    /// `--rewrite-submodules-from` + `--rewrite-submodules-to`: gitlink id in
    /// the source repository → id in the destination.
    submodules: HashMap<gix::ObjectId, gix::ObjectId>,
    /// Set by `--done` and by an in-stream `feature done`.
    require_done: bool,
}

impl<'a, S: ObjectSink> Executor<'a, S> {
    fn new(sink: &'a mut S, force: bool, reflog_message: String) -> Self {
        Executor {
            sink,
            marks: HashMap::new(),
            branches: HashMap::new(),
            deleted: Vec::new(),
            stats: ImportStats::default(),
            force,
            reflog_message,
            signed_commits: SignMode::Verbatim,
            signed_tags: SignMode::Verbatim,
            date_format: DateFormat::Raw,
            relative_marks: false,
            export_marks: None,
            allow_unsafe_features: false,
            submodules: HashMap::new(),
            require_done: false,
        }
    }

    fn run(&mut self, stream: &[u8], require_done: bool) -> napi::Result<ImportStats> {
        let mut cursor = Cursor::new(stream);
        let mut saw_done = false;
        while let Some(line) = cursor.take_command_line() {
            let (command, rest) = split_command(line);
            match command.as_str() {
                "blob" => self.command_blob(&mut cursor)?,
                "commit" => self.command_commit(&mut cursor, &lossy(rest).trim().to_string())?,
                "tag" => self.command_tag(&mut cursor, &lossy(rest).trim().to_string())?,
                "reset" => self.command_reset(&mut cursor, &lossy(rest).trim().to_string())?,
                "checkpoint" => {}
                "progress" => self.stats.progress.push(lossy(rest).to_string()),
                "feature" => self.command_feature(lossy(rest).trim())?,
                "option" => self.command_option(lossy(rest).trim())?,
                "alias" => self.command_alias(&mut cursor)?,
                "done" => {
                    saw_done = true;
                    break;
                }
                "gpgsig" => {
                    return Err(err_str(
                        "fast_import",
                        "gpgsig outside a commit or tag is not supported by the native importer; use fastImportViaGit",
                    ));
                }
                "N" => {
                    return Err(err_str(
                        "fast_import",
                        "notemodify is not supported by the native importer",
                    ));
                }
                "get-mark" | "cat-blob" | "ls" => {
                    return Err(err_str(
                        "fast_import",
                        format!("{command} needs an interactive backend; use fastImportViaGit"),
                    ));
                }
                "" => {}
                other => return Err(err_str("fast_import", format!("unknown command '{other}'"))),
            }
        }
        if (require_done || self.require_done) && !saw_done {
            return Err(err_str(
                "fast_import",
                "stream ended without a done command",
            ));
        }
        self.finalize()?;
        Ok(std::mem::take(&mut self.stats))
    }

    fn command_blob(&mut self, cursor: &mut Cursor) -> napi::Result<()> {
        let mut mark = None;
        while let Some(line) = cursor.take_command_line() {
            let (command, rest) = split_command(line);
            match command.as_str() {
                "mark" => mark = Some(parse_mark(lossy(rest).trim())?),
                "original-oid" => {}
                "data" => {
                    let data = read_data(cursor, line)?;
                    let id = self.sink.write_blob(&data)?;
                    self.stats.blobs += 1;
                    if let Some(mark) = mark {
                        self.marks.insert(mark, id);
                    }
                    return Ok(());
                }
                other => {
                    return Err(err_str(
                        "fast_import",
                        format!("unexpected '{other}' inside blob"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Handle a `feature` command: assert support, or take over one of the
    /// options git lets a stream set for itself.
    fn command_feature(&mut self, feature: &str) -> napi::Result<()> {
        let (name, value) = feature.split_once('=').unwrap_or((feature, ""));
        match name {
            // git treats these three as unsafe: they name files on disk.
            "export-marks" | "import-marks" | "import-marks-if-exists" => {
                if !self.allow_unsafe_features {
                    return Err(err_str(
                        "fast_import",
                        format!(
                            "feature {name} is unsafe; pass allowUnsafeFeatures to let the stream set it"
                        ),
                    ));
                }
                if value.is_empty() {
                    return Err(err_str(
                        "fast_import",
                        format!("feature {name} requires a file argument"),
                    ));
                }
                let path = self.sink.marks_path(value, self.relative_marks)?;
                match name {
                    "export-marks" => self.export_marks = Some(path),
                    "import-marks" => load_marks(&mut self.marks, &path)?,
                    _ => {
                        if path.exists() {
                            load_marks(&mut self.marks, &path)?;
                        }
                    }
                }
                Ok(())
            }
            "relative-marks" => {
                self.relative_marks = true;
                Ok(())
            }
            "no-relative-marks" => {
                self.relative_marks = false;
                Ok(())
            }
            "done" => {
                self.require_done = true;
                Ok(())
            }
            _ => require_known_feature(feature),
        }
    }

    /// Handle an `option` command. Only `date-format` changes how the rest of
    /// the stream is read; the remaining options are about the backend's
    /// plumbing and are ignored.
    fn command_option(&mut self, option: &str) -> napi::Result<()> {
        match option.split_once('=') {
            Some(("date-format", format)) => {
                self.date_format = DateFormat::parse(format)?;
                Ok(())
            }
            _ => {
                let name = option.split('=').next().unwrap_or(option);
                if KNOWN_OPTIONS.contains(&name) {
                    Ok(())
                } else {
                    Err(err_str("fast_import", format!("unknown option {name}")))
                }
            }
        }
    }

    /// `alias`: record that a mark refers to an object without creating one.
    ///
    /// Frontends use it to give pruned commits a valid value when resuming an
    /// incremental import from `--import-marks`.
    fn command_alias(&mut self, cursor: &mut Cursor) -> napi::Result<()> {
        let mut mark = None;
        let mut to = None;
        while let Some(line) = cursor.take_command_line() {
            let (command, rest) = split_command(line);
            match command.as_str() {
                "mark" => mark = Some(parse_mark(lossy(rest).trim())?),
                "to" => to = Some(lossy(rest).trim().to_string()),
                "" => break,
                other => {
                    return Err(err_str(
                        "fast_import",
                        format!("unexpected '{other}' inside alias"),
                    ));
                }
            }
        }
        let mark = mark.ok_or_else(|| err_str("fast_import", "alias is missing a mark command"))?;
        let to = to.ok_or_else(|| err_str("fast_import", "alias is missing a to command"))?;
        let id = self
            .resolve_commitish(&to)?
            .ok_or_else(|| err_str("fast_import", format!("alias target '{to}' does not exist")))?;
        self.marks.insert(mark, id);
        Ok(())
    }

    /// Decide what to do with a signature found on a commit or tag, returning
    /// the bytes to store (if any).
    fn signature_for(
        &mut self,
        what: &str,
        mode: &SignMode,
        data: Vec<u8>,
    ) -> napi::Result<Option<Vec<u8>>> {
        if mode.needs_verification() {
            return Err(err_str(
                "fast_import",
                format!(
                    "signed{what} mode '{}' verifies signatures, which the native importer cannot do; use fastImportViaGit",
                    mode.name()
                ),
            ));
        }
        if mode.aborts() {
            return Err(err_str(
                "fast_import",
                format!(
                    "signed {what} encountered and the mode is '{}'",
                    mode.name()
                ),
            ));
        }
        if matches!(mode, SignMode::WarnVerbatim | SignMode::WarnStrip) {
            self.stats
                .warnings
                .push(format!("signed {what} encountered"));
        }
        Ok(if mode.keeps() { Some(data) } else { None })
    }

    fn command_commit(&mut self, cursor: &mut Cursor, reference: &str) -> napi::Result<()> {
        let reference = normalize_ref_name(reference);
        let mut draft = CommitDraft::default();
        while let Some(line) = cursor.take_command_line() {
            let (command, rest) = split_command(line);
            match command.as_str() {
                "mark" => draft.mark = Some(parse_mark(lossy(rest).trim())?),
                "original-oid" => {}
                "author" => draft.author = Some(lossy(rest).to_string()),
                "committer" => draft.committer = Some(lossy(rest).to_string()),
                "encoding" => draft.encoding = Some(lossy(rest).trim().to_string()),
                "data" => draft.message = read_data(cursor, line)?,
                "from" => draft.from = Some(lossy(rest).trim().to_string()),
                "merge" => draft.merges.push(lossy(rest).trim().to_string()),
                "deleteall" => draft.ops.push(FileOp::DeleteAll),
                "M" => {
                    let (mode, dataref, path) = split_filemodify(rest)?;
                    let data = if dataref == "inline" {
                        let data_line = cursor.take_command_line().ok_or_else(|| {
                            err_str("fast_import", "inline filemodify is missing its data")
                        })?;
                        if !data_line.starts_with(b"data ") {
                            return Err(err_str(
                                "fast_import",
                                "inline filemodify must be followed by a data command",
                            ));
                        }
                        DataRef::Inline(read_data(cursor, data_line)?)
                    } else {
                        parse_dataref(&dataref)?
                    };
                    draft.ops.push(FileOp::Modify { mode, data, path });
                }
                "D" => draft.ops.push(FileOp::Delete {
                    path: parse_path(rest)?,
                }),
                "C" => {
                    let (source, destination) = parse_two_paths(rest)?;
                    let (mode, oid) = self
                        .lookup_current(&reference, &source)?
                        .ok_or_else(|| err_str("fast_import", "filecopy source does not exist"))?;
                    draft.ops.push(FileOp::Modify {
                        mode,
                        data: DataRef::Oid(oid),
                        path: destination,
                    });
                }
                "R" => {
                    let (source, destination) = parse_two_paths(rest)?;
                    let (mode, oid) =
                        self.lookup_current(&reference, &source)?.ok_or_else(|| {
                            err_str("fast_import", "filerename source does not exist")
                        })?;
                    draft.ops.push(FileOp::Modify {
                        mode,
                        data: DataRef::Oid(oid),
                        path: destination,
                    });
                    draft.ops.push(FileOp::Delete { path: source });
                }
                // `gpgsig <hash-algo> <format>` followed by a `data` command.
                // The algorithm decides which header git stores the signature
                // under: `gpgsig`, or `gpgsig-sha256`.
                "gpgsig" => {
                    let args = lossy(rest);
                    let mut args = args.split_whitespace();
                    let algo = args.next().unwrap_or("sha1").to_string();
                    let format = args.next().unwrap_or("unknown").to_string();
                    let data_line = cursor.take_command_line().ok_or_else(|| {
                        err_str("fast_import", "gpgsig is missing its data command")
                    })?;
                    if !data_line.starts_with(b"data ") {
                        return Err(err_str(
                            "fast_import",
                            "gpgsig must be followed by a data command",
                        ));
                    }
                    draft.gpgsig = Some((algo, format, read_data(cursor, data_line)?));
                }
                "N" | "ls" | "cat-blob" | "get-mark" => {
                    return Err(err_str(
                        "fast_import",
                        format!(
                            "'{command}' inside commit is not supported by the native importer"
                        ),
                    ));
                }
                "" => break,
                other => {
                    return Err(err_str(
                        "fast_import",
                        format!("unexpected '{other}' inside commit"),
                    ));
                }
            }
        }
        self.apply_commit(reference, draft)
    }

    fn command_tag(&mut self, cursor: &mut Cursor, name: &str) -> napi::Result<()> {
        let mut mark = None;
        let mut from = None;
        let mut tagger = None;
        let mut message = Vec::new();
        let mut gpgsig = None;
        while let Some(line) = cursor.take_command_line() {
            let (command, rest) = split_command(line);
            match command.as_str() {
                "mark" => mark = Some(parse_mark(lossy(rest).trim())?),
                "original-oid" => {}
                "from" => from = Some(lossy(rest).trim().to_string()),
                "tagger" => tagger = Some(lossy(rest).to_string()),
                // A signed tag carries its signature before the message.
                "gpgsig" => {
                    let data_line = cursor.take_command_line().ok_or_else(|| {
                        err_str("fast_import", "gpgsig is missing its data command")
                    })?;
                    if !data_line.starts_with(b"data ") {
                        return Err(err_str(
                            "fast_import",
                            "gpgsig must be followed by a data command",
                        ));
                    }
                    gpgsig = Some(read_data(cursor, data_line)?);
                }
                // `data` is the last command of a tag stanza. The blank line
                // that usually follows it is the optional LF git's
                // `skip_optional_lf` consumes, so stop reading here instead of
                // waiting for it — otherwise the next command is parsed as
                // part of this stanza.
                "data" => {
                    message = read_data(cursor, line)?;
                    break;
                }
                "" => break,
                other => {
                    return Err(err_str(
                        "fast_import",
                        format!("unexpected '{other}' inside tag"),
                    ));
                }
            }
        }
        let from = from.ok_or_else(|| err_str("fast_import", "tag is missing a from command"))?;
        let Some(target) = self.resolve_commitish(&from)? else {
            return Err(err_str("fast_import", "tag target does not exist"));
        };
        let tagger = match tagger {
            Some(line) => Some(parse_ident(&line, self.date_format)?),
            None => self.sink.committer(),
        };
        // A tag may target a commit or another tag (`--mark-tags` exports).
        let target_kind = match self.sink.find_object(target)? {
            Some((kind, _)) => kind,
            None => gix::object::Kind::Commit,
        };
        let mode = self.signed_tags.clone();
        let signature = match gpgsig {
            Some(data) => self.signature_for("tag", &mode, data)?.map(BString::from),
            None => None,
        };
        let tag = gix::objs::Tag {
            target,
            target_kind,
            name: BString::from(short_tag_name(name)),
            tagger,
            message: BString::from(message),
            signature,
        };
        let mut encoded = Vec::new();
        tag.write_to(&mut encoded)
            .map_err(|e| err("write_object", e))?;
        let id = self.sink.write_object(gix::object::Kind::Tag, &encoded)?;
        if let Some(mark) = mark {
            self.marks.insert(mark, id);
        }
        self.branches
            .insert(format!("refs/tags/{}", short_tag_name(name)), id);
        self.stats.tags += 1;
        Ok(())
    }

    fn command_reset(&mut self, cursor: &mut Cursor, reference: &str) -> napi::Result<()> {
        let mut from = None;
        // Only `from` belongs to a reset; anything else starts the next
        // command and must not be consumed here.
        while let Some(line) = cursor.peek_command_line() {
            let (command, rest) = split_command(line);
            if command != "from" {
                break;
            }
            cursor.take_command_line();
            from = Some(lossy(rest).trim().to_string());
        }
        let reference = normalize_ref_name(reference);
        match from {
            Some(spec) => match self.resolve_commitish(&spec)? {
                Some(id) => {
                    self.branches.insert(reference, id);
                }
                None => self.delete_ref(reference),
            },
            // `reset` without `from` removes the branch, matching git.
            None => self.delete_ref(reference),
        }
        Ok(())
    }

    fn apply_commit(&mut self, reference: String, draft: CommitDraft) -> napi::Result<()> {
        let committer_line = draft
            .committer
            .ok_or_else(|| err_str("fast_import", "commit is missing a committer command"))?;
        let committer = parse_ident(&committer_line, self.date_format)?;
        let author = match draft.author.as_deref() {
            Some(line) => parse_ident(line, self.date_format)?,
            None => committer.clone(),
        };

        // `from <null-sha>` removes the branch instead of committing to it.
        if draft.from.as_deref().is_some_and(is_null_sha) {
            self.delete_ref(reference);
            return Ok(());
        }

        let empty = gix::ObjectId::empty_tree(self.sink.object_hash());
        let base = match draft.from.as_deref() {
            Some(spec) => match self.resolve_commitish(spec)? {
                Some(id) => self.tree_of(id)?,
                None => empty,
            },
            None => match self.branches.get(&reference) {
                Some(id) => self.tree_of(*id)?,
                None => empty,
            },
        };

        // Resolve the content first: a sink without a repository cannot hold a
        // tree editor open while blobs are being written.
        let mut ops = Vec::with_capacity(draft.ops.len());
        for op in &draft.ops {
            match op {
                FileOp::DeleteAll => ops.push(TreeOp::DeleteAll),
                FileOp::Delete { path } => ops.push(TreeOp::Delete { path: path.clone() }),
                FileOp::Modify { mode, data, path } => {
                    let oid = self.resolve_dataref(data)?;
                    let oid = self.rewrite_submodule(mode, oid)?;
                    ops.push(TreeOp::Modify {
                        mode: mode.clone(),
                        oid,
                        path: path.clone(),
                    });
                }
            }
        }
        let tree = self.sink.edit_tree(base, &ops)?;

        let mut parents = Vec::new();
        match draft.from.as_deref() {
            Some(spec) => {
                if let Some(id) = self.resolve_commitish(spec)? {
                    parents.push(id);
                }
            }
            // Omitting `from` continues from the branch's current tip, which
            // becomes the first parent (git's rule for existing branches).
            None => {
                if let Some(id) = self.branches.get(&reference).copied() {
                    parents.push(id);
                }
            }
        }
        for merge in &draft.merges {
            if let Some(id) = self.resolve_commitish(merge)? {
                parents.push(id);
            }
        }

        let mode = self.signed_commits.clone();
        let extra_headers = match draft.gpgsig {
            Some((algo, _format, data)) => match self.signature_for("commit", &mode, data)? {
                Some(signature) => {
                    let name = if algo.eq_ignore_ascii_case("sha256") {
                        "gpgsig-sha256"
                    } else {
                        "gpgsig"
                    };
                    vec![(BString::from(name), BString::from(signature))]
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        let commit = gix::objs::Commit {
            tree,
            parents: parents.into(),
            author,
            committer,
            encoding: draft.encoding.map(BString::from),
            message: BString::from(draft.message.clone()),
            extra_headers,
        };
        let mut encoded = Vec::new();
        commit
            .write_to(&mut encoded)
            .map_err(|e| err("write_object", e))?;
        let id = self
            .sink
            .write_object(gix::object::Kind::Commit, &encoded)?;
        if let Some(mark) = draft.mark {
            self.marks.insert(mark, id);
        }
        self.branches.insert(reference, id);
        self.stats.commits += 1;
        Ok(())
    }

    /// Write every pending ref update, honoring the fast-forward rule.
    fn finalize(&mut self) -> napi::Result<()> {
        let mut updates: Vec<(String, gix::ObjectId)> = self
            .branches
            .iter()
            .map(|(name, id)| (name.clone(), *id))
            .collect();
        updates.sort();
        for (name, id) in updates {
            let previous = self.sink.current_target(&name);
            if let Some(old) = previous {
                if old != id && !self.force && !self.sink.is_ancestor(old, id)? {
                    self.stats.refs_skipped.push(name);
                    continue;
                }
            }
            // The sink is borrowed mutably below, so the message is copied out
            // before the update is handed over.
            let reflog = self.reflog_message.clone();
            self.sink.update_ref(&name, id, &reflog)?;
            self.stats.refs_updated.push(name);
        }
        for name in std::mem::take(&mut self.deleted) {
            // Git deletes a ref before recreating a nested tag, so a deletion
            // followed by a later update is not a deletion at all.
            if self.branches.contains_key(&name) {
                continue;
            }
            if self.sink.delete_ref(&name)? {
                self.stats.refs_deleted.push(name);
            }
        }
        let mut marks: Vec<(usize, gix::ObjectId)> =
            self.marks.iter().map(|(m, id)| (*m, *id)).collect();
        marks.sort();
        self.stats.marks = marks;
        Ok(())
    }

    fn delete_ref(&mut self, reference: String) {
        self.branches.remove(&reference);
        self.deleted.push(reference);
    }

    /// Rewrite the object id of a gitlink (`M 160000`) through
    /// `--rewrite-submodules-from` / `--rewrite-submodules-to`.
    ///
    /// Without the option the id is left alone; with it, an unknown id is an
    /// error, exactly as git reports when it cannot rewrite a submodule.
    fn rewrite_submodule(&self, mode: &str, oid: gix::ObjectId) -> napi::Result<gix::ObjectId> {
        if self.submodules.is_empty() || normalize_mode(mode) != "160000" {
            return Ok(oid);
        }
        self.submodules.get(&oid).copied().ok_or_else(|| {
            err_str(
                "fast_import",
                format!(
                    "cannot rewrite submodule {oid}: it is not in the rewriteSubmodulesFrom marks"
                ),
            )
        })
    }

    fn tree_of(&self, commit: gix::ObjectId) -> napi::Result<gix::ObjectId> {
        self.sink.commit_tree(commit)
    }

    /// Resolve a `<commit-ish>`: a mark, an imported branch, or a revision
    /// expression. `None` means "does not exist" (a null sha).
    fn resolve_commitish(&self, spec: &str) -> napi::Result<Option<gix::ObjectId>> {
        if is_null_sha(spec) {
            return Ok(None);
        }
        if let Some(digits) = spec.strip_prefix(':') {
            let mark: usize = digits
                .parse()
                .map_err(|_| err_str("fast_import", format!("invalid mark reference '{spec}'")))?;
            return self
                .marks
                .get(&mark)
                .copied()
                .ok_or_else(|| err_str("fast_import", format!("mark :{mark} is not defined")))
                .map(Some);
        }
        if let Some(id) = self.branches.get(spec) {
            return Ok(Some(*id));
        }
        match self.sink.resolve_rev(spec)? {
            Some(id) => Ok(Some(self.sink.peel_to_commit(id)?.unwrap_or(id))),
            None => Ok(None),
        }
    }

    fn resolve_dataref(&mut self, data: &DataRef) -> napi::Result<gix::ObjectId> {
        match data {
            DataRef::Mark(mark) => self
                .marks
                .get(mark)
                .copied()
                .ok_or_else(|| err_str("fast_import", format!("mark :{mark} is not defined"))),
            DataRef::Oid(id) => Ok(*id),
            DataRef::Inline(bytes) => {
                self.stats.blobs += 1;
                self.sink.write_blob(bytes)
            }
        }
    }

    /// Look up a path in the tree a commit starts from (used by `C` and `R`).
    fn lookup_current(
        &self,
        reference: &str,
        path: &[u8],
    ) -> napi::Result<Option<(String, gix::ObjectId)>> {
        let empty = gix::ObjectId::empty_tree(self.sink.object_hash());
        let base = match self.branches.get(reference) {
            Some(id) => self.tree_of(*id)?,
            None => empty,
        };
        lookup_path(&*self.sink, base, path)
    }
}

/// Split `M <mode> SP <dataref> SP <path>` into its three operands.
fn split_filemodify(rest: &[u8]) -> napi::Result<(String, String, Vec<u8>)> {
    let rest: &[u8] = rest;
    let first = rest
        .iter()
        .position(|b| *b == b' ')
        .ok_or_else(|| err_str("fast_import", "filemodify is missing its dataref"))?;
    let mode = lossy(&rest[..first]).trim().to_string();
    let tail = &rest[first + 1..];
    let second = tail
        .iter()
        .position(|b| *b == b' ')
        .ok_or_else(|| err_str("fast_import", "filemodify is missing its path"))?;
    let dataref = lossy(&tail[..second]).trim().to_string();
    let path = parse_path(&tail[second + 1..])?;
    Ok((mode, dataref, path))
}

/// Parse a `<dataref>`: `:mark`, a full object id, or the `inline` keyword.
fn parse_dataref(text: &str) -> napi::Result<DataRef> {
    if let Some(digits) = text.strip_prefix(':') {
        return Ok(DataRef::Mark(digits.parse().map_err(|_| {
            err_str("fast_import", format!("invalid mark reference '{text}'"))
        })?));
    }
    Ok(DataRef::Oid(parse_oid(text)?))
}

/// `commit <ref>` accepts full refnames; bare branch names are placed under
/// `refs/heads/`.
fn normalize_ref_name(reference: &str) -> String {
    if reference.starts_with("refs/") {
        reference.to_string()
    } else {
        format!("refs/heads/{reference}")
    }
}

/// `tag <name>` is stored under `refs/tags/`; strip an explicit prefix.
pub(crate) fn short_tag_name(name: &str) -> String {
    name.strip_prefix("refs/tags/").unwrap_or(name).to_string()
}

/// Options a stream may set with `option` that do not change how the stream is
/// read, and are therefore accepted and ignored.
const KNOWN_OPTIONS: &[&str] = &[
    "max-pack-size",
    "depth",
    "active-branches",
    "big-file-threshold",
    "export-pack-edges",
    "cat-blob-fd",
];

/// Reject stream features the native importer cannot honor.
fn require_known_feature(feature: &str) -> napi::Result<()> {
    let name = feature.split('=').next().unwrap_or(feature);
    const KNOWN: &[&str] = &[
        "date-format",
        "force",
        "done",
        "notes",
        "relative-marks",
        "no-relative-marks",
    ];
    if KNOWN.contains(&name) {
        return Ok(());
    }
    const UNIMPLEMENTED: &[&str] = &[
        "get-mark",
        "cat-blob",
        "ls",
        "import-marks",
        "import-marks-if-exists",
        "export-marks",
    ];
    if UNIMPLEMENTED.contains(&name) {
        return Err(err_str(
            "fast_import",
            format!("feature {name} requires the git backend; use fastImportViaGit"),
        ));
    }
    Err(err_str("fast_import", format!("unknown feature {name}")))
}

/// Resolve a path inside a tree, returning its mode string and object id.
///
/// Written against [`ObjectSink`] so a bare object database can serve `C` and
/// `R` commands too.
fn lookup_path<S: ObjectSink>(
    sink: &S,
    tree: gix::ObjectId,
    path: &[u8],
) -> napi::Result<Option<(String, gix::ObjectId)>> {
    let components: Vec<&[u8]> = path
        .split(|b| *b == b'/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut current = tree;
    for (index, component) in components.iter().enumerate() {
        let Some((kind, data)) = sink.find_object(current)? else {
            return Ok(None);
        };
        if kind != gix::object::Kind::Tree {
            return Ok(None);
        }
        let decoded = gix::objs::TreeRef::from_bytes(&data, sink.object_hash())
            .map_err(|e| err("decode", e))?;
        let entry = decoded.entries.iter().find(|entry| {
            let name: &[u8] = entry.filename;
            name == *component
        });
        match entry {
            Some(entry) => {
                let last = index + 1 == components.len();
                if last {
                    return Ok(Some((
                        crate::entry::mode_string(entry.mode.into()),
                        entry.oid.to_owned(),
                    )));
                }
                if !entry.mode.is_tree() {
                    return Ok(None);
                }
                current = entry.oid.to_owned();
            }
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Current target of `name`, if the reference exists and is direct.
fn current_target(r: &gix::Repository, name: &str) -> Option<gix::ObjectId> {
    match r.find_reference(name) {
        Ok(reference) => match reference.detach().target {
            gix::refs::Target::Object(id) => Some(id),
            gix::refs::Target::Symbolic(_) => None,
        },
        Err(_) => None,
    }
}

/// Whether `old` is reachable from `new` — the fast-forward test git performs
/// before moving a branch.
fn is_ancestor(r: &gix::Repository, old: gix::ObjectId, new: gix::ObjectId) -> napi::Result<bool> {
    let walk = r
        .rev_walk(vec![old])
        .with_hidden(vec![new])
        .all()
        .map_err(|e| err("rev_walk", e))?;
    let contained = walk.filter_map(|item| item.ok()).next().is_none();
    Ok(contained)
}

/// Peel any object to the commit it refers to.
pub(crate) fn peel_to_commit(
    r: &gix::Repository,
    id: gix::ObjectId,
) -> napi::Result<Option<gix::ObjectId>> {
    let object = r.find_object(id).map_err(|e| err("find_object", e))?;
    match object.peel_to_kind(gix::object::Kind::Commit) {
        Ok(peeled) => Ok(Some(peeled.id)),
        Err(_) => Ok(None),
    }
}

/// Read a marks file (`:markid SP <oid>` per line) into the marks table.
fn load_marks(
    marks: &mut HashMap<usize, gix::ObjectId>,
    path: &std::path::Path,
) -> napi::Result<()> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        err_str(
            "fast_import",
            format!("cannot read marks file '{}': {e}", path.display()),
        )
    })?;
    for line in contents.lines() {
        let mut parts = line.split(' ');
        let mark = parts.next().unwrap_or_default();
        let oid = parts.next().unwrap_or_default();
        if mark.is_empty() || oid.is_empty() {
            continue;
        }
        let mark = parse_mark(mark)?;
        marks.insert(mark, parse_oid(oid)?);
    }
    Ok(())
}

/// Write the marks table in `--export-marks` format.
fn write_marks(marks: &[(usize, gix::ObjectId)], path: &std::path::Path) -> napi::Result<()> {
    let mut contents = String::new();
    for (mark, id) in marks {
        contents.push_str(&format!(":{mark} {id}\n"));
    }
    crate::fast_export::ensure_parent_dir(path)?;
    std::fs::write(path, contents).map_err(|e| {
        err_str(
            "fast_import",
            format!("cannot write marks file '{}': {e}", path.display()),
        )
    })
}

/// Build the gitlink rewrite table from `--rewrite-submodules-from` and
/// `--rewrite-submodules-to`.
///
/// Both options take `<name>:<marks-file>`; the marks file written by
/// `git fast-export` for a submodule maps its commits to the ids
/// `git fast-import` produced for the same commits, which is what makes a
/// conversion between hash algorithms possible.
fn submodule_map(
    from: Option<&Vec<String>>,
    to: Option<&Vec<String>>,
) -> napi::Result<HashMap<gix::ObjectId, gix::ObjectId>> {
    const PAIRS: &str =
        "rewriteSubmodulesFrom and rewriteSubmodulesTo must be given in corresponding pairs";
    let (Some(from), Some(to)) = (from, to) else {
        if from.is_some() != to.is_some() {
            return Err(err_str("fast_import", PAIRS));
        }
        return Ok(HashMap::new());
    };
    if from.len() != to.len() {
        return Err(err_str("fast_import", PAIRS));
    }
    let mut out = HashMap::new();
    for (source, target) in from.iter().zip(to.iter()) {
        let (source_name, source_path) = source.split_once(':').ok_or_else(|| {
            err_str(
                "fast_import",
                format!("rewriteSubmodulesFrom token '{source}' must be <name>:<file>"),
            )
        })?;
        let (target_name, target_path) = target.split_once(':').ok_or_else(|| {
            err_str(
                "fast_import",
                format!("rewriteSubmodulesTo token '{target}' must be <name>:<file>"),
            )
        })?;
        if source_name != target_name {
            return Err(err_str("fast_import", PAIRS));
        }
        let mut source_marks = HashMap::new();
        let mut target_marks = HashMap::new();
        load_marks(&mut source_marks, std::path::Path::new(source_path))?;
        load_marks(&mut target_marks, std::path::Path::new(target_path))?;
        for (mark, id) in source_marks {
            if let Some(rewritten) = target_marks.get(&mark) {
                out.insert(id, *rewritten);
            }
        }
    }
    Ok(out)
}

/// Options for [`Repository::import_repository`] / [`import_repository`].
#[derive(Clone, Default)]
#[napi(object)]
pub struct RepositoryImportOptions {
    /// Options for the fast-import part of the archive.
    pub fast_import: Option<FastImportOptions>,
    /// Restore the reference table that
    /// [`export_repository`](crate::fast_export::export_repository) appended:
    /// remotes, notes, stash and HEAD. Default `true`.
    pub apply_refs: Option<bool>,
    /// Delete references that exist in the repository but not in the archive.
    pub prune: Option<bool>,
    /// Check out HEAD into the worktree afterwards. Uses the `git` binary and
    /// is a no-op for bare repositories.
    pub checkout: Option<bool>,
}

/// Result of [`Repository::import_repository`].
#[napi(object)]
pub struct RepositoryImportStats {
    /// Blobs written by the stream.
    pub blobs: u32,
    /// Commits written by the stream.
    pub commits: u32,
    /// Annotated tags written by the stream.
    pub tags: u32,
    /// References moved by the stream.
    pub refs_updated: Vec<String>,
    /// References the stream refused to move (non fast-forward).
    pub refs_skipped: Vec<String>,
    /// References deleted by the stream.
    pub refs_deleted: Vec<String>,
    /// References restored from the archive's reference table.
    pub refs_restored: Vec<String>,
    /// References removed because they were absent from the archive.
    pub refs_pruned: Vec<String>,
    /// What HEAD points at after the import, when it could be determined.
    pub head: Option<String>,
    /// Whether the worktree was checked out.
    pub checked_out: bool,
}

/// Apply an archive produced by
/// [`compute_repository_export`](crate::fast_export::compute_repository_export).
///
/// The fast-import half recreates history, branches and tags; the reference
/// table after it restores everything else. `prune` makes the result match the
/// source exactly instead of merging into what is already there, and
/// `checkout` recreates the working tree, which no stream can carry.
pub(crate) fn compute_repository_import(
    r: &gix::Repository,
    archive: &[u8],
    options: Option<&RepositoryImportOptions>,
) -> napi::Result<RepositoryImportStats> {
    let options = options.cloned().unwrap_or_default();
    let marker = format!("\n{}\n", crate::fast_export::REPOSITORY_EXPORT_MARKER);
    let (stream, trailer) = match archive
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
    {
        Some(index) => (&archive[..index], &archive[index + marker.len()..]),
        // No reference table: a plain fast-import stream.
        None => (archive, &[][..]),
    };
    let stats = compute_fast_import(r, stream, options.fast_import.as_ref())?;
    let mut restored = Vec::new();
    let mut pruned = Vec::new();
    let mut head = None;

    if options.apply_refs.unwrap_or(true) {
        let mut recorded: Vec<String> = Vec::new();
        for line in lossy(trailer).lines() {
            let mut parts = line.splitn(3, ' ');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("head"), Some(target), _) if target != "-" => {
                    head = Some(target.to_string());
                    set_head(r, target)?;
                }
                (Some("ref"), Some(name), Some(id)) => {
                    apply_recorded_ref(r, name, id, &mut restored)?;
                    recorded.push(name.to_string());
                }
                (Some("sym"), Some(name), Some(target)) => {
                    apply_symbolic_ref(r, name, target, &mut restored)?;
                    recorded.push(name.to_string());
                }
                _ => {}
            }
        }
        if options.prune.unwrap_or(false) {
            pruned = prune_refs(r, &recorded)?;
        }
    }
    if head.is_none() {
        head = r
            .head_ref()
            .ok()
            .flatten()
            .map(|reference| reference.name().to_string());
    }

    let mut checked_out = false;
    if options.checkout.unwrap_or(false) && r.workdir().is_some() {
        crate::checkout::checkout_worktree(r, "HEAD")?;
        checked_out = true;
    }
    Ok(RepositoryImportStats {
        blobs: stats.blobs,
        commits: stats.commits,
        tags: stats.tags,
        refs_updated: stats.refs_updated,
        refs_skipped: stats.refs_skipped,
        refs_deleted: stats.refs_deleted,
        refs_restored: restored,
        refs_pruned: pruned,
        head,
        checked_out,
    })
}

/// Point HEAD at `target` (a branch name), keeping it symbolic.
fn set_head(r: &gix::Repository, target: &str) -> napi::Result<()> {
    let name = gix::refs::FullName::try_from("HEAD").map_err(|e| err("importRepository", e))?;
    let target = gix::refs::FullName::try_from(target).map_err(|e| err("importRepository", e))?;
    let edit = gix::refs::transaction::RefEdit::update_with_log(
        name,
        gix::refs::Target::Symbolic(target),
        gix::refs::transaction::PreviousValue::Any,
        gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: "import-repository".into(),
        },
    );
    r.edit_reference(edit)
        .map(|_| ())
        .map_err(|e| err("edit_reference", e))
}

/// Restore one `ref <name> <id>` entry; a null id deletes the reference.
fn apply_recorded_ref(
    r: &gix::Repository,
    name: &str,
    id: &str,
    restored: &mut Vec<String>,
) -> napi::Result<()> {
    let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("importRepository", e))?;
    let edit = if is_null_sha(id) {
        gix::refs::transaction::RefEdit::delete_with_log(
            full_name,
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::RefLog::AndReference,
        )
    } else {
        gix::refs::transaction::RefEdit::update_with_log(
            full_name,
            gix::refs::Target::Object(parse_oid(id)?),
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: false,
                message: "import-repository".into(),
            },
        )
    };
    if r.edit_reference(edit).is_ok() {
        restored.push(name.to_string());
    }
    Ok(())
}

/// Restore one `sym <name> <target>` entry.
fn apply_symbolic_ref(
    r: &gix::Repository,
    name: &str,
    target: &str,
    restored: &mut Vec<String>,
) -> napi::Result<()> {
    let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("importRepository", e))?;
    let target = gix::refs::Target::Symbolic(
        gix::refs::FullName::try_from(target).map_err(|e| err("importRepository", e))?,
    );
    let edit = gix::refs::transaction::RefEdit::update_with_log(
        full_name,
        target,
        gix::refs::transaction::PreviousValue::Any,
        gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: "import-repository".into(),
        },
    );
    if r.edit_reference(edit).is_ok() {
        restored.push(name.to_string());
    }
    Ok(())
}

/// Delete every reference that the archive did not record.
fn prune_refs(r: &gix::Repository, recorded: &[String]) -> napi::Result<Vec<String>> {
    let platform = r.references().map_err(|e| err("references", e))?;
    let mut deleted = Vec::new();
    for reference in platform.all().map_err(|e| err("references", e))? {
        let reference = reference.map_err(|e| err("reference", e))?;
        let name = reference.name().to_string();
        if name == "HEAD" || recorded.iter().any(|entry| entry == &name) {
            continue;
        }
        let full_name =
            gix::refs::FullName::try_from(name.as_str()).map_err(|e| err("prune", e))?;
        let edit = gix::refs::transaction::RefEdit::delete_with_log(
            full_name,
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::RefLog::AndReference,
        );
        if r.edit_reference(edit).is_ok() {
            deleted.push(name);
        }
    }
    Ok(deleted)
}

/// Parse a fast-import stream and apply it to `r`.
pub(crate) fn compute_fast_import(
    r: &gix::Repository,
    stream: &[u8],
    options: Option<&FastImportOptions>,
) -> napi::Result<FastImportStats> {
    let mut sink = RepositorySink::new(r);
    run_stream(&mut sink, stream, options)
}

/// Parse a fast-import stream and apply it through `sink`.
///
/// This is the generic entry point: [`compute_fast_import`] is this function
/// over a [`RepositorySink`], and
/// [`crate::object_store::memory::MemoryObjectStore::import_stream`] is it over an object
/// database that has no references.
pub(crate) fn run_stream<S: ObjectSink>(
    sink: &mut S,
    stream: &[u8],
    options: Option<&FastImportOptions>,
) -> napi::Result<FastImportStats> {
    let options = options.cloned().unwrap_or_default();
    let relative = options.relative_marks.unwrap_or(false);
    // Marks paths are resolved before the executor borrows the sink.
    let import_paths = options
        .import_marks
        .iter()
        .flatten()
        .map(|path| sink.marks_path(path, relative))
        .collect::<napi::Result<Vec<_>>>()?;
    let import_if_exists = match options.import_marks_if_exists.as_deref() {
        Some(path) => Some(sink.marks_path(path, relative)?),
        None => None,
    };
    let export_marks = match options.export_marks.as_deref() {
        Some(path) => Some(sink.marks_path(path, relative)?),
        None => None,
    };
    let mut executor = Executor::new(
        sink,
        options.force.unwrap_or(false),
        options
            .reflog_message
            .clone()
            .unwrap_or_else(|| "fast-import".to_string()),
    );
    executor.date_format = match options.date_format.as_deref() {
        Some(text) => DateFormat::parse(text)?,
        None => DateFormat::Raw,
    };
    executor.signed_commits = match options.signed_commits.as_deref() {
        Some(mode) => SignMode::parse(mode, "signedCommits")?,
        None => SignMode::Verbatim,
    };
    executor.signed_tags = match options.signed_tags.as_deref() {
        Some(mode) => SignMode::parse(mode, "signedTags")?,
        None => SignMode::Verbatim,
    };
    executor.relative_marks = relative;
    executor.allow_unsafe_features = options.allow_unsafe_features.unwrap_or(false);
    executor.require_done = options.done.unwrap_or(false);
    executor.export_marks = export_marks;
    executor.submodules = submodule_map(
        options.rewrite_submodules_from.as_ref(),
        options.rewrite_submodules_to.as_ref(),
    )?;
    for path in &import_paths {
        load_marks(&mut executor.marks, path)?;
    }
    if let Some(path) = import_if_exists.as_deref() {
        if path.exists() {
            load_marks(&mut executor.marks, path)?;
        }
    }
    let stats = executor.run(stream, options.done.unwrap_or(false))?;
    // `executor.export_marks` starts as the `--export-marks` given on the
    // command line and is replaced when the stream sets one with
    // `feature export-marks`, so the last one wins — git's own order.
    if let Some(path) = executor.export_marks.as_deref() {
        write_marks(&stats.marks, path)?;
    }
    Ok(FastImportStats::from(&stats))
}

/// Wrap an [`io::Error`] from stream writing into a napi error.
pub(crate) fn io_err(e: io::Error) -> napi::Error {
    err("stream write", e)
}

// ---------------------------------------------------------------------------
// napi boundary
// ---------------------------------------------------------------------------

/// Options for [`fast_import`] and [`Repository::fast_import`].
///
/// `force`, `done`, `importMarks`, `exportMarks` and `reflogMessage` are
/// honored by the native importer; the remaining fields are passed through to
/// the real `git fast-import` by [`fast_import_via_git`].
#[derive(Clone, Default)]
#[napi(object)]
pub struct FastImportOptions {
    /// Update branches even when the update is not a fast-forward (`--force`).
    pub force: Option<bool>,
    /// Error out when the stream does not end with a `done` command (`--done`).
    pub done: Option<bool>,
    /// Read marks from these files before processing the stream
    /// (`--import-marks`, repeatable).
    pub import_marks: Option<Vec<String>>,
    /// Write the resulting marks table here when the import completes
    /// (`--export-marks`).
    pub export_marks: Option<String>,
    /// Read marks from this file first, if it exists
    /// (`--import-marks-if-exists`). Unlike `importMarks`, a missing file is
    /// not an error.
    pub import_marks_if_exists: Option<String>,
    /// Resolve marks paths relative to `<git-dir>/info/fast-import`
    /// (`--relative-marks`). `false` spells `--no-relative-marks`.
    pub relative_marks: Option<bool>,
    /// How to treat signed commits: `"verbatim"` (default), `"warn-verbatim"`,
    /// `"strip"`, `"warn-strip"` or `"abort"` (`--signed-commits`). The
    /// verification-dependent modes git offers (`strip-if-invalid`,
    /// `sign-if-invalid[=<keyid>]`, `abort-if-invalid`) need the git backend.
    pub signed_commits: Option<String>,
    /// How to treat signed tags (`--signed-tags`); same modes as
    /// `signedCommits`.
    pub signed_tags: Option<String>,
    /// Rewrite submodule (gitlink) object ids: `<name>:<marks-file>` produced
    /// by `git fast-export` for the source repository
    /// (`--rewrite-submodules-from`, repeatable). Must be paired with
    /// `rewriteSubmodulesTo`.
    pub rewrite_submodules_from: Option<Vec<String>>,
    /// `<name>:<marks-file>` produced by `git fast-import` for the same
    /// submodule (`--rewrite-submodules-to`, repeatable).
    pub rewrite_submodules_to: Option<Vec<String>>,
    /// Reflog message recorded for every ref update made by the native importer.
    pub reflog_message: Option<String>,
    /// Git binary only: print statistics after the import (`--stats`).
    pub stats: Option<bool>,
    /// Git binary only: suppress statistics (`--quiet`).
    pub quiet: Option<bool>,
    /// Git binary only: permit unsafe in-stream features
    /// (`--allow-unsafe-features`).
    pub allow_unsafe_features: Option<bool>,
    /// Date format used by the stream's identity lines (`--date-format`):
    /// `"raw"` (default), `"raw-permissive"`, `"rfc2822"` or `"now"`.
    pub date_format: Option<String>,
    /// Git binary only: file descriptor that receives the answers to
    /// `get-mark`, `cat-blob` and `ls` (`--cat-blob-fd`).
    pub cat_blob_fd: Option<u32>,
    /// Git binary only: maximum number of branches kept active at once
    /// (`--active-branches`).
    pub active_branches: Option<u32>,
    /// Git binary only: largest blob that is considered for deltas, in bytes
    /// (`--big-file-threshold`).
    pub big_file_threshold: Option<f64>,
    /// Git binary only: maximum delta depth (`--depth`).
    pub depth: Option<u32>,
    /// Git binary only: write the packfile name and the last commit of each
    /// branch here (`--export-pack-edges`).
    pub export_pack_edges: Option<String>,
    /// Git binary only: maximum size of one output packfile, in bytes
    /// (`--max-pack-size`).
    pub max_pack_size: Option<f64>,
}

/// One entry of the marks table produced by an import.
#[napi(object)]
pub struct FastImportMark {
    /// Mark number, as assigned by the stream.
    pub mark: u32,
    /// Hex object id the mark refers to.
    pub id: String,
}

/// Result of an import: what was written, and what was refused.
#[napi(object)]
pub struct FastImportStats {
    /// Number of blobs written (including inline file contents).
    pub blobs: u32,
    /// Number of commits written.
    pub commits: u32,
    /// Number of annotated tags written.
    pub tags: u32,
    /// References moved to a new commit.
    pub refs_updated: Vec<String>,
    /// References left untouched because the update was not a fast-forward.
    pub refs_skipped: Vec<String>,
    /// References deleted by `reset` or a null-sha `from`.
    pub refs_deleted: Vec<String>,
    /// `progress` messages carried by the stream.
    pub progress: Vec<String>,
    /// Warnings raised by the `warn-*` signature modes.
    pub warnings: Vec<String>,
    /// Marks defined by the stream, mapped to object ids.
    pub marks: Vec<FastImportMark>,
}

impl From<&ImportStats> for FastImportStats {
    fn from(s: &ImportStats) -> Self {
        FastImportStats {
            blobs: s.blobs as u32,
            commits: s.commits as u32,
            tags: s.tags as u32,
            refs_updated: s.refs_updated.clone(),
            refs_skipped: s.refs_skipped.clone(),
            refs_deleted: s.refs_deleted.clone(),
            progress: s.progress.clone(),
            warnings: s.warnings.clone(),
            marks: s
                .marks
                .iter()
                .map(|(mark, id)| FastImportMark {
                    mark: *mark as u32,
                    id: id.to_string(),
                })
                .collect(),
        }
    }
}

/// Result of [`fast_import_via_git`].
#[napi(object)]
pub struct GitFastImportOutput {
    /// Whether `git fast-import` exited successfully.
    pub success: bool,
    /// Process exit code; `-1` when no code is available.
    pub exit_code: i32,
    /// Captured standard output (statistics, unless `--quiet`).
    pub stdout: Buffer,
    /// Captured standard error.
    pub stderr: Buffer,
}

/// Builds a git fast-import stream in memory.
///
/// ```ts
/// const importer = new FastImporter("main", "committer Bot <bot@example.com>");
/// importer.startCommit("Import", Date.now() / 1000);
/// importer.addFile("a.txt", Buffer.from("hello"));
/// importer.finishCommit();
/// importer.done();
/// const stats = importer.importInto("/path/to/repo");
/// ```
#[napi]
pub struct FastImporter {
    inner: GitFastImporter<Vec<u8>>,
    pending: Option<PendingCommit>,
}

/// State of a commit being assembled across several JS calls.
struct PendingCommit {
    message: String,
    timestamp: String,
    ops: Vec<CommitOp>,
    from: Option<String>,
    merges: Vec<String>,
}

#[napi]
impl FastImporter {
    /// Create a stream that commits to `branch` as `author`.
    ///
    /// `author` is a raw identity line, e.g.
    /// `"committer Bot <bot@example.com>"` — the timestamp is appended by
    /// [`start_commit`](Self::start_commit).
    #[napi(constructor)]
    pub fn new(branch: String, author: String, initial_parent: Option<String>) -> Self {
        FastImporter {
            inner: GitFastImporter::new(Vec::new(), branch, initial_parent, author),
            pending: None,
        }
    }

    /// Write a standalone `blob` and return its mark.
    #[napi]
    pub fn blob(&mut self, data: Buffer) -> napi::Result<u32> {
        let mark = self.inner.write_blob(&data).map_err(io_err)?;
        Ok(mark as u32)
    }

    /// Begin a commit. `timestamp` is seconds since the epoch; omit it to use
    /// the current time.
    #[napi]
    pub fn start_commit(&mut self, message: String, timestamp: Option<f64>) -> napi::Result<()> {
        if self.pending.is_some() {
            return Err(err_str(
                "startCommit",
                "the previous commit was not finished",
            ));
        }
        self.pending = Some(PendingCommit {
            message,
            timestamp: timestamp_string(timestamp),
            ops: Vec::new(),
            from: None,
            merges: Vec::new(),
        });
        Ok(())
    }

    /// Add a file to the pending commit and return the blob's mark.
    #[napi]
    pub fn add_file(&mut self, path: String, data: Buffer) -> napi::Result<u32> {
        self.ensure_pending("addFile")?;
        // Blobs must appear in the stream before the commit that uses them,
        // so the payload is written immediately.
        let mark = self.inner.write_blob(&data).map_err(io_err)?;
        self.push_op(CommitOp::Modify {
            mode: "100644".to_string(),
            mark,
            path,
        });
        Ok(mark as u32)
    }

    /// Add a file with an explicit mode (`100644`, `100755`, `120000`,
    /// `160000`, `040000`).
    #[napi]
    pub fn add_file_with_mode(
        &mut self,
        mode: String,
        path: String,
        data: Buffer,
    ) -> napi::Result<u32> {
        crate::entry::mode_to_entry_kind(&normalize_mode(&mode))?;
        self.ensure_pending("addFileWithMode")?;
        let mark = self.inner.write_blob(&data).map_err(io_err)?;
        self.push_op(CommitOp::Modify {
            mode: normalize_mode(&mode),
            mark,
            path,
        });
        Ok(mark as u32)
    }

    /// Reference an object that already exists in the target repository.
    #[napi]
    pub fn add_existing(&mut self, mode: String, oid: String, path: String) -> napi::Result<()> {
        crate::entry::mode_to_entry_kind(&normalize_mode(&mode))?;
        parse_oid(&oid)?;
        self.ensure_pending("addExisting")?;
        self.push_op(CommitOp::ModifyOid {
            mode: normalize_mode(&mode),
            oid,
            path,
        });
        Ok(())
    }

    /// Remove a file or directory from the pending commit.
    #[napi]
    pub fn delete_file(&mut self, path: String) -> napi::Result<()> {
        self.ensure_pending("deleteFile")?;
        self.push_op(CommitOp::Delete { path });
        Ok(())
    }

    /// Remove every file before applying the remaining ops.
    #[napi]
    pub fn delete_all(&mut self) -> napi::Result<()> {
        self.ensure_pending("deleteAll")?;
        self.push_op(CommitOp::DeleteAll);
        Ok(())
    }

    /// Copy `source` to `destination` within the pending commit.
    #[napi]
    pub fn copy_file(&mut self, source: String, destination: String) -> napi::Result<()> {
        self.ensure_pending("copyFile")?;
        self.push_op(CommitOp::Copy {
            source,
            destination,
        });
        Ok(())
    }

    /// Rename `source` to `destination` within the pending commit.
    #[napi]
    pub fn rename_file(&mut self, source: String, destination: String) -> napi::Result<()> {
        self.ensure_pending("renameFile")?;
        self.push_op(CommitOp::Rename {
            source,
            destination,
        });
        Ok(())
    }

    /// Set the `from` of the pending commit (defaults to the branch tip).
    #[napi]
    pub fn set_from(&mut self, commit_ish: String) -> napi::Result<()> {
        let pending = self.ensure_pending_mut("setFrom")?;
        pending.from = Some(commit_ish);
        Ok(())
    }

    /// Add a `merge` parent to the pending commit.
    #[napi]
    pub fn add_merge(&mut self, commit_ish: String) -> napi::Result<()> {
        let pending = self.ensure_pending_mut("addMerge")?;
        pending.merges.push(commit_ish);
        Ok(())
    }

    /// Write the pending commit and return its mark.
    #[napi]
    pub fn finish_commit(&mut self) -> napi::Result<u32> {
        let pending = self
            .pending
            .take()
            .ok_or_else(|| err_str("finishCommit", "there is no pending commit"))?;
        let mark = self
            .inner
            .write_commit(
                &pending.message,
                &pending.timestamp,
                &pending.ops,
                pending.from.as_deref(),
                &pending.merges,
            )
            .map_err(io_err)?;
        Ok(mark as u32)
    }

    /// Write an annotated tag. `tagger` is a full identity line, e.g.
    /// `"tagger Bot <bot@example.com> 1700000000 +0000"`.
    #[napi]
    pub fn tag(
        &mut self,
        name: String,
        from: String,
        tagger: String,
        message: String,
    ) -> napi::Result<()> {
        self.inner
            .write_tag(&name, &from, &tagger, &message)
            .map_err(io_err)
    }

    /// Move a branch, or create a lightweight tag, without committing.
    #[napi]
    pub fn reset(&mut self, reference: String, from: Option<String>) -> napi::Result<()> {
        self.inner
            .write_reset(&reference, from.as_deref())
            .map_err(io_err)
    }

    /// Emit a `progress` line.
    #[napi]
    pub fn progress(&mut self, message: String) -> napi::Result<()> {
        self.inner.write_progress(&message).map_err(io_err)
    }

    /// Emit a `checkpoint`.
    #[napi]
    pub fn checkpoint(&mut self) -> napi::Result<()> {
        self.inner.write_checkpoint().map_err(io_err)
    }

    /// Assert that the backend supports `feature`.
    #[napi]
    pub fn feature(&mut self, feature: String) -> napi::Result<()> {
        self.inner.write_feature(&feature).map_err(io_err)
    }

    /// Emit an `option` line, e.g. `date-format=raw`.
    #[napi]
    pub fn option(&mut self, option: String) -> napi::Result<()> {
        self.inner.write_option(&option).map_err(io_err)
    }

    /// Emit a comment. Ignored by fast-import; useful when debugging.
    #[napi]
    pub fn comment(&mut self, text: String) -> napi::Result<()> {
        self.inner.write_comment(&text).map_err(io_err)
    }

    /// Terminate the stream with `done`.
    #[napi]
    pub fn done(&mut self) -> napi::Result<()> {
        self.inner.finish().map_err(io_err)
    }

    /// The stream built so far.
    #[napi]
    pub fn to_buffer(&self) -> Buffer {
        Buffer::from(self.inner.output.clone())
    }

    /// The stream built so far, as a UTF-8 string (useful in assertions and
    /// crash reports; binary blob payloads are replaced as-is).
    #[napi]
    pub fn to_stream(&self) -> String {
        String::from_utf8_lossy(&self.inner.output).into_owned()
    }

    /// Apply the stream built so far to the repository at `dir`, leaving the
    /// stream intact so it can be reused or inspected.
    #[napi]
    pub fn import_into(
        &mut self,
        dir: String,
        options: Option<FastImportOptions>,
    ) -> napi::Result<FastImportStats> {
        let stream = self.inner.output.clone();
        fast_import(dir, Buffer::from(stream), options)
    }
}

impl FastImporter {
    /// Fail unless a commit is being assembled.
    fn ensure_pending(&self, what: &str) -> napi::Result<()> {
        match self.pending {
            Some(_) => Ok(()),
            None => Err(err_str(what, "call startCommit first")),
        }
    }

    /// Borrow the commit being assembled.
    fn ensure_pending_mut(&mut self, what: &str) -> napi::Result<&mut PendingCommit> {
        self.pending
            .as_mut()
            .ok_or_else(|| err_str(what, "call startCommit first"))
    }

    /// Record an op on the pending commit.
    fn push_op(&mut self, op: CommitOp) {
        if let Some(pending) = self.pending.as_mut() {
            pending.ops.push(op);
        }
    }
}

/// Render a JS timestamp as fast-import's raw date format.
fn timestamp_string(timestamp: Option<f64>) -> String {
    let seconds = timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as f64)
            .unwrap_or(0.0)
    });
    format!("{} +0000", seconds.max(0.0) as i64)
}

/// Open the repository at `dir` for a one-shot operation.
fn open_repository(dir: &str) -> napi::Result<gix::Repository> {
    let thread_safe = Arc::new(gix::ThreadSafeRepository::open(dir).map_err(|e| err("open", e))?);
    Ok(thread_local(&thread_safe))
}

/// Parse a fast-import stream and apply it to the repository at `dir`.
///
/// Objects are written loose and refs are updated with a fast-forward check;
/// pass `{ force: true }` to overwrite branches whose history would be lost.
#[napi]
pub fn fast_import(
    dir: String,
    stream: Buffer,
    options: Option<FastImportOptions>,
) -> napi::Result<FastImportStats> {
    let r = open_repository(&dir)?;
    compute_fast_import(&r, &stream, options.as_ref())
}

/// [`fast_import`] on the libuv thread pool.
#[napi(ts_return_type = "Promise<FastImportStats>")]
pub fn fast_import_async(
    dir: String,
    stream: Buffer,
    options: Option<FastImportOptions>,
) -> napi::Result<AsyncTask<FastImportTask>> {
    Ok(AsyncTask::new(FastImportTask {
        dir,
        stream: stream.to_vec(),
        options,
    }))
}

/// Pipe a stream through the real `git fast-import`.
///
/// Use this for stream dialects the native importer rejects (`gpgsig`,
/// `notemodify`, `cat-blob`, `ls`, `get-mark`) and for packfile-oriented
/// options (`--max-pack-size`, `--depth`, `--export-pack-edges`).
#[napi]
pub fn fast_import_via_git(
    dir: String,
    stream: Buffer,
    options: Option<FastImportOptions>,
) -> napi::Result<GitFastImportOutput> {
    let git = find_git().ok_or_else(|| err_str("fast-import", "git executable not found"))?;
    let options = options.unwrap_or_default();
    let mut args: Vec<String> = vec!["fast-import".to_string()];
    if options.force.unwrap_or(false) {
        args.push("--force".to_string());
    }
    if options.quiet.unwrap_or(false) {
        args.push("--quiet".to_string());
    } else if options.stats.unwrap_or(false) {
        args.push("--stats".to_string());
    }
    if options.done.unwrap_or(false) {
        args.push("--done".to_string());
    }
    if options.allow_unsafe_features.unwrap_or(false) {
        args.push("--allow-unsafe-features".to_string());
    }
    if let Some(format) = options.date_format.as_deref() {
        args.push(format!("--date-format={format}"));
    }
    // `--relative-marks` applies to every marks path that follows it, so it
    // has to be emitted before them.
    match options.relative_marks {
        Some(true) => args.push("--relative-marks".to_string()),
        Some(false) => args.push("--no-relative-marks".to_string()),
        None => {}
    }
    if let Some(path) = options.export_marks.as_deref() {
        args.push(format!("--export-marks={path}"));
    }
    for path in options.import_marks.iter().flatten() {
        args.push(format!("--import-marks={path}"));
    }
    if let Some(path) = options.import_marks_if_exists.as_deref() {
        args.push(format!("--import-marks-if-exists={path}"));
    }
    if let Some(mode) = options.signed_tags.as_deref() {
        args.push(format!("--signed-tags={mode}"));
    }
    if let Some(mode) = options.signed_commits.as_deref() {
        args.push(format!("--signed-commits={mode}"));
    }
    for entry in options.rewrite_submodules_from.iter().flatten() {
        args.push(format!("--rewrite-submodules-from={entry}"));
    }
    for entry in options.rewrite_submodules_to.iter().flatten() {
        args.push(format!("--rewrite-submodules-to={entry}"));
    }
    if let Some(fd) = options.cat_blob_fd {
        args.push(format!("--cat-blob-fd={fd}"));
    }
    if let Some(n) = options.active_branches {
        args.push(format!("--active-branches={n}"));
    }
    if let Some(n) = options.big_file_threshold {
        args.push(format!("--big-file-threshold={}", n as u64));
    }
    if let Some(n) = options.depth {
        args.push(format!("--depth={n}"));
    }
    if let Some(path) = options.export_pack_edges.as_deref() {
        args.push(format!("--export-pack-edges={path}"));
    }
    if let Some(n) = options.max_pack_size {
        args.push(format!("--max-pack-size={}", n as u64));
    }
    let mut child = std::process::Command::new(&git)
        .args(&args)
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| err("spawn", e))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| err_str("fast-import", "failed to open stdin"))?;
        stdin.write_all(&stream).map_err(|e| err("stdin", e))?;
    }
    let output = child.wait_with_output().map_err(|e| err("wait", e))?;
    Ok(GitFastImportOutput {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: Buffer::from(output.stdout),
        stderr: Buffer::from(output.stderr),
    })
}

/// [`fast_import`] as a [`Task`].
pub struct FastImportTask {
    dir: String,
    stream: Vec<u8>,
    options: Option<FastImportOptions>,
}

impl Task for FastImportTask {
    type Output = FastImportStats;
    type JsValue = FastImportStats;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = open_repository(&self.dir)?;
        compute_fast_import(&r, &self.stream, self.options.as_ref())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// [`Repository::fast_import_async`] as a [`Task`].
pub struct RepositoryFastImportTask {
    repo: Arc<gix::ThreadSafeRepository>,
    stream: Vec<u8>,
    options: Option<FastImportOptions>,
}

impl Task for RepositoryFastImportTask {
    type Output = FastImportStats;
    type JsValue = FastImportStats;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = thread_local(&self.repo);
        compute_fast_import(&r, &self.stream, self.options.as_ref())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

#[napi]
impl Repository {
    /// Apply a fast-import stream to this repository.
    #[napi]
    pub fn fast_import(
        &self,
        stream: Buffer,
        options: Option<FastImportOptions>,
    ) -> napi::Result<FastImportStats> {
        let r = thread_local(&self.inner);
        compute_fast_import(&r, &stream, options.as_ref())
    }

    /// [`fast_import`](Self::fast_import) on the libuv thread pool.
    #[napi(ts_return_type = "Promise<FastImportStats>")]
    pub fn fast_import_async(
        &self,
        stream: Buffer,
        options: Option<FastImportOptions>,
    ) -> napi::Result<AsyncTask<RepositoryFastImportTask>> {
        Ok(AsyncTask::new(RepositoryFastImportTask {
            repo: Arc::clone(&self.inner),
            stream: stream.to_vec(),
            options,
        }))
    }

    /// Apply an archive produced by
    /// [`export_repository`](crate::fast_export::export_repository): history
    /// from the fast-import half, then every recorded reference.
    ///
    /// `checkout: true` additionally recreates the working tree with
    /// `git reset --hard HEAD`, which is the only way to restore files — a
    /// fast-import stream carries objects and refs, not the worktree.
    #[napi]
    pub fn import_repository(
        &self,
        archive: Buffer,
        options: Option<RepositoryImportOptions>,
    ) -> napi::Result<RepositoryImportStats> {
        let r = thread_local(&self.inner);
        compute_repository_import(&r, &archive, options.as_ref())
    }
}

/// Apply an archive produced by [`crate::fast_export::export_repository`] to
/// the repository at `dir`. See
/// [`Repository::import_repository`](crate::fast_import::Repository::import_repository).
#[napi]
pub fn import_repository(
    dir: String,
    archive: Buffer,
    options: Option<RepositoryImportOptions>,
) -> napi::Result<RepositoryImportStats> {
    let r = open_repository(&dir)?;
    compute_repository_import(&r, &archive, options.as_ref())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fast_export::{FastExportOptions, compute_fast_export};
    use chrono::Utc;

    fn ts() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    fn create_importer(output: &mut Vec<u8>) -> GitFastImporter<&mut Vec<u8>> {
        GitFastImporter::new(
            output,
            "main".to_string(),
            None,
            "committer Test <test@example.com>".to_string(),
        )
    }

    #[test]
    fn test_single_commit_with_file() {
        let mut output = Vec::new();
        let mut importer = create_importer(&mut output);

        let mut commit = importer.start_commit("Add test file", ts());
        commit.add_file("test.txt", b"hello world").unwrap();
        commit.finish().unwrap();
        importer.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 11
hello world
commit refs/heads/main
mark :2
committer Test <test@example.com> 0 +0000
data 13
Add test file
M 100644 :1 test.txt

done
"
        );
    }

    #[test]
    fn test_multiple_files_in_commit() {
        let mut output = Vec::new();
        let mut importer = create_importer(&mut output);

        let mut commit = importer.start_commit("Add multiple files", ts());
        commit.add_file("a.txt", b"aaa").unwrap();
        commit.add_file("b.txt", b"bbb").unwrap();
        commit.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 3
aaa
blob
mark :2
data 3
bbb
commit refs/heads/main
mark :3
committer Test <test@example.com> 0 +0000
data 18
Add multiple files
M 100644 :1 a.txt
M 100644 :2 b.txt

"
        );
    }

    #[test]
    fn test_multiple_commits() {
        let mut output = Vec::new();
        let mut importer = create_importer(&mut output);

        let mut commit1 = importer.start_commit("First commit", ts());
        commit1.add_file("first.txt", b"first").unwrap();
        commit1.finish().unwrap();

        let mut commit2 = importer.start_commit("Second commit", ts());
        commit2.add_file("second.txt", b"second").unwrap();
        commit2.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 5
first
commit refs/heads/main
mark :2
committer Test <test@example.com> 0 +0000
data 12
First commit
M 100644 :1 first.txt

blob
mark :3
data 6
second
commit refs/heads/main
mark :4
committer Test <test@example.com> 0 +0000
data 13
Second commit
M 100644 :3 second.txt

"
        );
    }

    #[test]
    fn test_initial_parent() {
        let mut output = Vec::new();
        let mut importer = GitFastImporter::new(
            &mut output,
            "main".to_string(),
            Some("refs/heads/existing".to_string()),
            "committer Test <test@example.com>".to_string(),
        );

        let mut commit = importer.start_commit("Child commit", ts());
        commit.add_file("file.txt", b"data").unwrap();
        commit.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 4
data
commit refs/heads/main
mark :2
committer Test <test@example.com> 0 +0000
data 12
Child commit
from refs/heads/existing
M 100644 :1 file.txt

"
        );
    }

    #[test]
    fn test_initial_parent_only_on_first_commit() {
        let mut output = Vec::new();
        let mut importer = GitFastImporter::new(
            &mut output,
            "main".to_string(),
            Some("refs/heads/existing".to_string()),
            "committer Test <test@example.com>".to_string(),
        );

        let mut commit1 = importer.start_commit("First", ts());
        commit1.add_file("a.txt", b"a").unwrap();
        commit1.finish().unwrap();

        let mut commit2 = importer.start_commit("Second", ts());
        commit2.add_file("b.txt", b"b").unwrap();
        commit2.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 1
a
commit refs/heads/main
mark :2
committer Test <test@example.com> 0 +0000
data 5
First
from refs/heads/existing
M 100644 :1 a.txt

blob
mark :3
data 1
b
commit refs/heads/main
mark :4
committer Test <test@example.com> 0 +0000
data 6
Second
M 100644 :3 b.txt

"
        );
    }

    #[test]
    fn test_empty_path_skipped() {
        let mut output = Vec::new();
        let mut importer = create_importer(&mut output);

        let mut commit = importer.start_commit("Test", ts());
        commit.add_file("", b"data").unwrap();
        commit.add_file("real.txt", b"data").unwrap();
        commit.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 4
data
blob
mark :2
data 4
data
commit refs/heads/main
mark :3
committer Test <test@example.com> 0 +0000
data 4
Test
M 100644 :2 real.txt

"
        );
    }

    #[test]
    fn test_dropped_commit_builder_no_commit() {
        let mut output = Vec::new();
        let mut importer = create_importer(&mut output);

        {
            let mut builder = importer.start_commit("Dropped commit", ts());
            builder.add_file("file.txt", b"data").unwrap();
            // Drop without calling finish()
        }

        importer.finish().unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\
blob
mark :1
data 4
data
done
"
        );
    }

    #[test]
    fn test_quoted_paths() {
        assert_eq!(quote_path("a/b.txt"), "a/b.txt");
        assert_eq!(quote_path("a b.txt"), "\"a b.txt\"");
        assert_eq!(quote_path("a\nb"), "\"a\\nb\"");
        assert_eq!(quote_path("\"quoted\""), "\"\\\"quoted\\\"\"");
    }

    #[test]
    fn test_parse_ident() {
        let sig = parse_ident(
            "A U Thor <thor@example.com> 1700000000 +0200",
            DateFormat::Raw,
        )
        .unwrap();
        assert_eq!(sig.name, "A U Thor".as_bytes());
        assert_eq!(sig.email, "thor@example.com".as_bytes());
        assert_eq!(sig.time.seconds, 1_700_000_000);
        assert_eq!(sig.time.offset, 7200);
    }

    /// `--date-format=rfc2822` reads the whole `<when>` as one RFC 2822 date.
    #[test]
    fn test_parse_ident_rfc2822() {
        let sig = parse_ident(
            "A U Thor <thor@example.com> Tue, 6 Feb 2007 11:22:18 -0500",
            DateFormat::Rfc2822,
        )
        .unwrap();
        // 2007-02-06T11:22:18-05:00 == 1170778938 UTC.
        assert_eq!(sig.time.seconds, 1_170_778_938);
        assert_eq!(sig.time.offset, -5 * 3600);
    }

    /// `now` stamps the identity with the current time.
    #[test]
    fn test_parse_ident_now() {
        let before = now_seconds();
        let sig = parse_ident("A U Thor <thor@example.com> now", DateFormat::Now).unwrap();
        assert!(sig.time.seconds >= before);
        assert_eq!(sig.time.offset, 0);
    }

    /// `raw-permissive` imports bogus timestamps instead of rejecting them.
    #[test]
    fn test_parse_ident_raw_permissive() {
        let sig = parse_ident(
            "A U Thor <thor@example.com> not-a-number +0000",
            DateFormat::RawPermissive,
        )
        .unwrap();
        assert_eq!(sig.time.seconds, 0);
        assert_eq!(sig.time.offset, 0);
        assert!(
            parse_ident(
                "A U Thor <thor@example.com> not-a-number +0000",
                DateFormat::Raw
            )
            .is_err()
        );
    }

    /// `gpgsig` is stored under `gpgsig`, or `gpgsig-sha256` for SHA-256.
    #[test]
    fn test_signed_commit_is_imported_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let stream = signed_stream("sha1", "openpgp");
        let stats = compute_fast_import(
            &r,
            &stream,
            Some(&FastImportOptions {
                signed_commits: Some("verbatim".to_string()),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        assert_eq!(stats.commits, 1);
        let tip = current_target(&r, "refs/heads/main").unwrap();
        let commit = r.find_commit(tip).unwrap();
        let decoded = commit.decode().unwrap();
        let header = decoded
            .extra_headers()
            .find("gpgsig")
            .expect("the signature is stored");
        assert_eq!(lossy(header).trim(), "-----BEGIN PGP SIGNATURE-----");
    }

    /// `--signed-commits=strip` drops the signature; `abort` refuses it.
    #[test]
    fn test_signed_commit_modes() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let stats = compute_fast_import(
            &r,
            &signed_stream("sha1", "openpgp"),
            Some(&FastImportOptions {
                signed_commits: Some("strip".to_string()),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        let tip = current_target(&r, "refs/heads/main").unwrap();
        let commit = r.find_commit(tip).unwrap();
        let decoded = commit.decode().unwrap();
        assert!(decoded.extra_headers().find("gpgsig").is_none());

        let other = tempfile::tempdir().unwrap();
        let other = init_repo(other.path());
        let error = compute_fast_import(
            &other,
            &signed_stream("sha1", "openpgp"),
            Some(&FastImportOptions {
                signed_commits: Some("abort".to_string()),
                ..FastImportOptions::default()
            }),
        )
        .err()
        .unwrap();
        assert!(
            format!("{error}").contains("signed commit"),
            "unexpected error: {error}"
        );
        assert_eq!(stats.commits, 1);
    }

    /// `alias` records a mark without writing an object.
    #[test]
    fn test_alias_marks_an_existing_object() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let head = sample_stream();
        compute_fast_import(&r, &head, None).unwrap();
        let tip = current_target(&r, "refs/heads/main").unwrap();
        let stream = format!(
            "alias\nmark :7\nto {tip}\n\ncommit refs/heads/next\ncommitter Test <test@example.com> 0 +0000\ndata 5\nalias\nfrom :7\n\ndone\n"
        );
        let stats = compute_fast_import(&r, stream.as_bytes(), None).unwrap();
        assert_eq!(stats.commits, 1);
        let next = current_target(&r, "refs/heads/next").unwrap();
        let commit = r.find_commit(next).unwrap();
        let decoded = commit.decode().unwrap();
        let parent = lossy(&decoded.parents[0]);
        assert_eq!(parent, tip.to_string());
    }

    /// `--relative-marks` puts the marks file under `<git-dir>/info/fast-import`.
    #[test]
    fn test_relative_marks_path() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let marks = tempfile::NamedTempFile::new().unwrap();
        let stats = compute_fast_import(
            &r,
            &sample_stream(),
            Some(&FastImportOptions {
                export_marks: Some("./marks".to_string()),
                relative_marks: Some(true),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        assert!(!stats.marks.is_empty());
        let written = r
            .common_dir()
            .join("info")
            .join("fast-import")
            .join("marks");
        let contents = std::fs::read_to_string(&written).unwrap();
        assert!(contents.starts_with(":1 "), "{contents}");
        // An absolute path is unaffected by the flag.
        let absolute = marks.path().to_path_buf();
        compute_fast_import(
            &r,
            &sample_stream(),
            Some(&FastImportOptions {
                export_marks: Some(absolute.to_string_lossy().to_string()),
                relative_marks: Some(true),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        assert!(absolute.exists());
    }

    /// `feature export-marks` is refused without `allowUnsafeFeatures`.
    #[test]
    fn test_in_stream_export_marks_needs_allow_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        // `feature` has to come before the `done` that ends the stream.
        let mut stream = sample_stream();
        assert!(stream.ends_with(b"done\n"));
        stream.truncate(stream.len() - "done\n".len());
        stream.extend_from_slice(b"feature export-marks=./marks\ndone\n");
        let error = compute_fast_import(&r, &stream, None).err().unwrap();
        assert!(
            format!("{error}").contains("unsafe"),
            "unexpected error: {error}"
        );

        let allowed = compute_fast_import(
            &r,
            &stream,
            Some(&FastImportOptions {
                allow_unsafe_features: Some(true),
                relative_marks: Some(true),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        assert_eq!(allowed.commits, 2);
        assert!(
            r.common_dir()
                .join("info")
                .join("fast-import")
                .join("marks")
                .exists()
        );
    }

    /// A submodule gitlink is rewritten through the from/to marks.
    #[test]
    fn test_rewrite_submodules() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let from = tempfile::NamedTempFile::new().unwrap();
        let to = tempfile::NamedTempFile::new().unwrap();
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        std::fs::write(from.path(), format!(":1 {old}\n")).unwrap();
        std::fs::write(to.path(), format!(":1 {new}\n")).unwrap();
        let stream = format!(
            "commit refs/heads/main\ncommitter Test <test@example.com> 0 +0000\ndata 5\nfirst\nM 160000 {old} sub\n\ndone\n"
        );
        compute_fast_import(
            &r,
            stream.as_bytes(),
            Some(&FastImportOptions {
                rewrite_submodules_from: Some(vec![format!("sub:{}", from.path().display())]),
                rewrite_submodules_to: Some(vec![format!("sub:{}", to.path().display())]),
                ..FastImportOptions::default()
            }),
        )
        .unwrap();
        let tip = current_target(&r, "refs/heads/main").unwrap();
        let tree = r.find_commit(tip).unwrap().tree_id().unwrap().detach();
        let (mode, oid) = lookup_path(&RepositorySink::new(&r), tree, b"sub")
            .unwrap()
            .unwrap();
        assert_eq!(mode, "160000");
        assert_eq!(oid.to_string(), new);
    }

    #[test]
    fn test_delimited_data() {
        let stream = b"data <<EOF\nline one\nline two\nEOF\n";
        let mut cursor = Cursor::new(stream);
        let line = cursor.take_command_line().unwrap();
        let data = read_data(&mut cursor, line).unwrap();
        assert_eq!(String::from_utf8(data).unwrap(), "line one\nline two\n");
    }

    #[test]
    fn test_counted_data() {
        let stream = b"data 5\nhello\n\n";
        let mut cursor = Cursor::new(stream);
        let line = cursor.take_command_line().unwrap();
        let data = read_data(&mut cursor, line).unwrap();
        assert_eq!(String::from_utf8(data).unwrap(), "hello");
        // The LF after the payload is consumed, so the next line is empty.
        assert_eq!(cursor.take_line(), Some(&b""[..]));
    }

    /// Build a two-commit stream: `a.txt` in the first commit, `a.txt` edited
    /// and `b.txt` added in the second.
    fn sample_stream() -> Vec<u8> {
        let mut out = Vec::new();
        let mut importer = GitFastImporter::new(
            &mut out,
            "main".to_string(),
            None,
            "committer Test <test@example.com>".to_string(),
        );
        let mut first = importer.start_commit("first", ts());
        first.add_file("a.txt", b"one\n").unwrap();
        first.finish().unwrap();
        let mut second = importer.start_commit("second", ts());
        second.add_file("a.txt", b"two\n").unwrap();
        second.add_file("nested/b.txt", b"bee\n").unwrap();
        second.finish().unwrap();
        importer.finish().unwrap();
        out
    }

    /// A one-commit stream whose commit carries a `gpgsig`.
    fn signed_stream(algo: &str, format: &str) -> Vec<u8> {
        let signature = "-----BEGIN PGP SIGNATURE-----\n";
        format!(
            "commit refs/heads/main\ncommitter Test <test@example.com> 0 +0000\ndata 5\nfirst\ngpgsig {algo} {format}\ndata {}\n{signature}\n\ndone\n",
            signature.len()
        )
        .into_bytes()
    }

    /// Initialize a repository at `path` and return a thread-local view of it.
    fn init_repo(path: &std::path::Path) -> gix::Repository {
        let repo: gix::ThreadSafeRepository = gix::init(path).unwrap().into();
        thread_local(&Arc::new(repo))
    }

    #[test]
    fn test_import_creates_commits_and_refs() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let stream = sample_stream();
        let stats = compute_fast_import(&r, &stream, None).unwrap();

        assert_eq!(stats.commits, 2);
        assert_eq!(stats.blobs, 3);
        assert_eq!(stats.refs_updated, vec!["refs/heads/main".to_string()]);

        let tip = current_target(&r, "refs/heads/main").unwrap();
        let commit = r.find_commit(tip).unwrap();
        let decoded = commit.decode().unwrap();
        assert_eq!(lossy(decoded.message).trim(), "second");
        let tree = parse_oid(&lossy(decoded.tree)).unwrap();
        let (mode, oid) = lookup_path(&RepositorySink::new(&r), tree, b"a.txt")
            .unwrap()
            .unwrap();
        assert_eq!(mode, "100644");
        let blob = r.find_object(oid).unwrap();
        assert_eq!(String::from_utf8_lossy(&blob.data), "two\n");
    }

    #[test]
    fn test_export_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let stream = sample_stream();
        compute_fast_import(&r, &stream, None).unwrap();

        let exported = compute_fast_export(&r, None).unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(text.contains("commit refs/heads/main"), "{text}");
        assert!(text.contains("committer Test <test@example.com>"));
        assert!(text.contains("M 100644 "), "{text}");
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("nested/b.txt"), "{text}");
    }

    #[test]
    fn test_export_then_reimport() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = init_repo(source_dir.path());
        compute_fast_import(&source, &sample_stream(), None).unwrap();
        let exported = compute_fast_export(
            &source,
            Some(&FastExportOptions {
                full_tree: Some(true),
                ..FastExportOptions::default()
            }),
        )
        .unwrap();

        let target_dir = tempfile::tempdir().unwrap();
        let target = init_repo(target_dir.path());
        let stats = compute_fast_import(&target, &exported, None).unwrap();
        assert_eq!(stats.commits, 2);
        assert_eq!(stats.refs_updated, vec!["refs/heads/main".to_string()]);
    }

    #[test]
    fn test_repository_export_import_round_trip() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = init_repo(source_dir.path());
        compute_fast_import(&source, &sample_stream(), None).unwrap();
        // A remote ref and a symbolic HEAD: fast-export walks neither.
        let main = current_target(&source, "refs/heads/main").unwrap();
        let full = gix::refs::FullName::try_from("refs/remotes/origin/main").unwrap();
        source
            .edit_reference(gix::refs::transaction::RefEdit::update_with_log(
                full,
                gix::refs::Target::Object(main),
                gix::refs::transaction::PreviousValue::Any,
                gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: "test".into(),
                },
            ))
            .unwrap();

        let archive = crate::fast_export::compute_repository_export(&source, None).unwrap();
        let text = String::from_utf8_lossy(&archive).to_string();
        assert!(text.contains("done\n"), "{text}");
        assert!(text.contains("# repository-export v1"), "{text}");
        assert!(text.contains("ref refs/remotes/origin/main "), "{text}");

        // A plain fast-import of the same archive reproduces the history.
        let bare_dir = tempfile::tempdir().unwrap();
        let bare = init_repo(bare_dir.path());
        let stats = compute_fast_import(&bare, &archive, None).unwrap();
        assert_eq!(stats.commits, 2);
        assert!(current_target(&bare, "refs/remotes/origin/main").is_none());

        // The repository importer also restores the references it records.
        let target_dir = tempfile::tempdir().unwrap();
        let target = init_repo(target_dir.path());
        let stats = compute_repository_import(&target, &archive, None).unwrap();
        assert_eq!(stats.commits, 2);
        assert_eq!(
            current_target(&target, "refs/remotes/origin/main"),
            Some(main)
        );
        assert!(
            stats
                .refs_restored
                .iter()
                .any(|name| name == "refs/remotes/origin/main")
        );
        assert!(!stats.checked_out);
    }

    #[test]
    fn test_unsupported_interactive_command_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let r = init_repo(dir.path());
        let error = compute_fast_import(&r, b"get-mark :1\n", None)
            .err()
            .unwrap();
        assert!(error.to_string().contains("interactive backend"));
    }
}
