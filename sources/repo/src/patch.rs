use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{InternedInput, UnifiedDiff, diff_with_slider_heuristics};

use crate::diff::{Algorithm, Change};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::{Repository, lossy};
use crate::rev_walk::WalkOptions;

/// How much of a patch to emit when rendering it to text.
///
/// Mirrors the three include flags of `git_patch_size()`; all default to `true`.
#[napi(object)]
pub struct PatchTextOptions {
    /// Include the context lines (`" "` prefixed). When `false` only additions
    /// and removals are emitted.
    pub include_context: Option<bool>,
    /// Include the `@@ -a,b +c,d @@` hunk headers.
    pub include_hunk_headers: Option<bool>,
    /// Include the `diff --git`, `index`, `---` and `+++` file headers.
    pub include_file_headers: Option<bool>,
}

/// Options used when computing a patch.
#[napi(object)]
pub struct PatchOptions {
    /// Lines of context around each hunk (git's `-U<n>`). Defaults to `3`.
    pub context_lines: Option<u32>,
    /// Diff algorithm; defaults to `Algorithm.Histogram`.
    pub algorithm: Option<Algorithm>,
    /// Diff binary content as text (git's `--text`). When omitted, a `NUL`
    /// byte within the first 8 KiB marks a side as binary and no line diff is
    /// produced (the patch then only reports `Binary files … differ`).
    pub text: Option<bool>,
}

/// One line of a [`PatchHunk`], modeled after `git2::DiffLine`.
#[napi(object)]
#[derive(Clone)]
pub struct PatchLine {
    /// One-character prefix: `" "` (context), `"+"` (addition), `"-"` (removal).
    pub origin: String,
    /// Line content without the prefix. A trailing `\n` is preserved when the
    /// source line had one (no trailing newline is signalled by `noNewline`).
    pub content: String,
    /// 1-based line number in the pre-image; omitted for additions.
    pub old_lineno: Option<u32>,
    /// 1-based line number in the post-image; omitted for removals.
    pub new_lineno: Option<u32>,
    /// `true` when the line lacks a trailing newline; rendering emits a
    /// `\ No newline at end of file` marker after it.
    pub no_newline: bool,
}

/// A single hunk of a [`Patch`], modeled after `git2::DiffHunk`.
#[napi(object)]
#[derive(Clone)]
pub struct PatchHunk {
    /// 1-based first line of the hunk in the pre-image (`0` for an empty
    /// pre-image, matching git's `-0,0`).
    pub old_start: u32,
    /// Number of pre-image lines covered by the hunk.
    pub old_lines: u32,
    /// 1-based first line of the hunk in the post-image.
    pub new_start: u32,
    /// Number of post-image lines covered by the hunk.
    pub new_lines: u32,
    /// Rendered header, e.g. `@@ -1,3 +1,4 @@`; git's trailing function
    /// context (`@@ -7,4 +7,4 @@ line6`) is appended when one is found in
    /// the 100 lines preceding the hunk.
    pub header: String,
}

/// Line statistics for a patch (mirrors `git2::Patch::line_stats`).
#[napi(object)]
#[derive(Clone, Default)]
pub struct PatchLineStats {
    /// Number of context lines.
    pub context: u32,
    /// Number of added lines.
    pub additions: u32,
    /// Number of removed lines.
    pub deletions: u32,
}

/// The file-level metadata of a [`Patch`] (a reduced `git2::DiffDelta`).
#[napi(object)]
#[derive(Clone)]
pub struct PatchDelta {
    /// Repo-relative path of the pre-image; for renames/copies this is the
    /// source path.
    pub old_path: String,
    /// Repo-relative path of the post-image (the destination path for
    /// renames/copies).
    pub new_path: String,
    /// Blob id of the pre-image; omitted when the side does not exist or when
    /// the patch was built from raw buffers.
    pub old_id: Option<String>,
    /// Blob id of the post-image (see `oldId` for when it is omitted).
    pub new_id: Option<String>,
    /// Octal git mode of the pre-image (`100644` when unknown).
    pub old_mode: String,
    /// Octal git mode of the post-image (`100644` when unknown).
    pub new_mode: String,
    /// `Added`, `Deleted`, `Modified`, `Renamed`, `Copied` or `Unmodified`.
    pub status: String,
    /// `true` when no line diff was produced because a side is binary.
    pub binary: bool,
}

/// A diff of a single file, modeled after `git2::Patch`.
///
/// Instances are produced by the `Repository.patchFrom*` constructors and are
/// immutable snapshots: hunks and lines are fully materialised on construction.
///
/// Deviations from `git2::Patch`:
/// * `toBuf()`/`print()` accept optional [`PatchTextOptions`] so header
///   inclusion can be controlled (git2 gets this from `DiffOptions`).
/// * `print()` returns the text instead of taking a line callback — NAPI
///   callbacks per line would dominate the cost of rendering.
/// * Rename/copy headers (`similarity index`, `rename from/to`) are not
///   rendered; [`PatchDelta::status`] reports the rename instead.
/// * Paths are emitted verbatim (no C-style quoting of unusual characters).
#[napi]
pub struct Patch {
    delta: PatchDelta,
    hunks: Vec<PatchHunk>,
    lines: Vec<Vec<PatchLine>>,
    stats: PatchLineStats,
    /// `true` when the pre-image does not exist (added file).
    old_absent: bool,
    /// `true` when the post-image does not exist (deleted file).
    new_absent: bool,
}

#[derive(Clone, Copy)]
struct TextFlags {
    context: bool,
    hunk_headers: bool,
    file_headers: bool,
}

impl TextFlags {
    const ALL: TextFlags = TextFlags {
        context: true,
        hunk_headers: true,
        file_headers: true,
    };

    fn from_options(options: Option<PatchTextOptions>) -> Self {
        match options {
            Some(o) => TextFlags {
                context: o.include_context.unwrap_or(true),
                hunk_headers: o.include_hunk_headers.unwrap_or(true),
                file_headers: o.include_file_headers.unwrap_or(true),
            },
            None => TextFlags::ALL,
        }
    }

    fn new(context: Option<bool>, hunk_headers: Option<bool>, file_headers: Option<bool>) -> Self {
        TextFlags {
            context: context.unwrap_or(true),
            hunk_headers: hunk_headers.unwrap_or(true),
            file_headers: file_headers.unwrap_or(true),
        }
    }
}

#[napi]
impl Patch {
    /// File-level metadata: paths, blob ids, modes, status and the binary flag.
    #[napi]
    pub fn delta(&self) -> PatchDelta {
        self.delta.clone()
    }

    /// Number of hunks in the patch (`0` for an unchanged or binary file).
    #[napi]
    pub fn num_hunks(&self) -> u32 {
        self.hunks.len() as u32
    }

    /// Number of context, added and removed lines across all hunks.
    #[napi]
    pub fn line_stats(&self) -> PatchLineStats {
        self.stats.clone()
    }

    /// Hunk `index` (0-based). Throws when out of range.
    #[napi]
    pub fn hunk(&self, index: u32) -> napi::Result<PatchHunk> {
        Ok(self
            .hunks
            .get(index as usize)
            .cloned()
            .ok_or_else(|| err_str("patch_hunk", format!("hunk index {index} out of range")))?)
    }

    /// Number of lines in hunk `index`. Throws when out of range.
    #[napi]
    pub fn num_lines_in_hunk(&self, index: u32) -> napi::Result<u32> {
        Ok(self
            .lines
            .get(index as usize)
            .map(|lines| lines.len() as u32)
            .ok_or_else(|| err_str("patch_hunk", format!("hunk index {index} out of range")))?)
    }

    /// Line `line_index` (0-based) of hunk `hunk_index`. Throws when either
    /// index is out of range.
    #[napi]
    pub fn line_in_hunk(&self, hunk_index: u32, line_index: u32) -> napi::Result<PatchLine> {
        Ok(self
            .lines
            .get(hunk_index as usize)
            .and_then(|lines| lines.get(line_index as usize))
            .cloned()
            .ok_or_else(|| {
                err_str(
                    "patch_line",
                    format!("line {line_index} of hunk {hunk_index} out of range"),
                )
            })?)
    }

    /// `true` when no line diff was produced because a side is binary.
    #[napi]
    pub fn is_binary(&self) -> bool {
        self.delta.binary
    }

    /// Byte length of the patch text rendered with the given flags — the NAPI
    /// counterpart of `git2::Patch::size`. All flags default to `true`.
    #[napi]
    pub fn size(
        &self,
        include_context: Option<bool>,
        include_hunk_headers: Option<bool>,
        include_file_headers: Option<bool>,
    ) -> u32 {
        let flags = TextFlags::new(include_context, include_hunk_headers, include_file_headers);
        self.render(flags).len() as u32
    }

    /// The patch as unified-diff text (git's `git diff` output for one file).
    #[napi]
    pub fn to_buf(&self, options: Option<PatchTextOptions>) -> String {
        self.render(TextFlags::from_options(options))
    }

    /// Alias of [`to_buf`](Self::to_buf), named after `git2::Patch::print`.
    #[napi]
    pub fn print(&self, options: Option<PatchTextOptions>) -> String {
        self.render(TextFlags::from_options(options))
    }
}

impl Patch {
    fn render(&self, flags: TextFlags) -> String {
        // Identical content: git emits nothing at all, not even file headers.
        if self.delta.status == "Unmodified" {
            return String::new();
        }
        let mut out = String::new();
        if flags.file_headers {
            out.push_str(&self.file_header());
        }
        if self.delta.binary {
            out.push_str(&format!(
                "Binary files {} and {} differ\n",
                self.old_label(),
                self.new_label()
            ));
            return out;
        }
        for (hunk, lines) in self.hunks.iter().zip(self.lines.iter()) {
            if flags.hunk_headers {
                out.push_str(&hunk.header);
                out.push('\n');
            }
            for line in lines {
                if !flags.context && line.origin == " " {
                    continue;
                }
                out.push_str(&line.origin);
                out.push_str(&line.content);
                if line.no_newline {
                    out.push('\n');
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
        out
    }

    fn file_header(&self) -> String {
        // git names both sides after the existing path for additions/deletions.
        let (a, b) = if self.old_absent {
            (self.delta.new_path.as_str(), self.delta.new_path.as_str())
        } else if self.new_absent {
            (self.delta.old_path.as_str(), self.delta.old_path.as_str())
        } else {
            (self.delta.old_path.as_str(), self.delta.new_path.as_str())
        };
        let mut out = format!("diff --git a/{a} b/{b}\n");
        if self.old_absent {
            out.push_str(&format!("new file mode {}\n", self.delta.new_mode));
        } else if self.new_absent {
            out.push_str(&format!("deleted file mode {}\n", self.delta.old_mode));
        }
        match (
            self.old_absent,
            self.new_absent,
            &self.delta.old_id,
            &self.delta.new_id,
        ) {
            (false, false, Some(old), Some(new)) => out.push_str(&format!(
                "index {}..{} {}\n",
                short_oid(old),
                short_oid(new),
                self.delta.new_mode
            )),
            (true, _, _, Some(new)) => {
                out.push_str(&format!("index 0000000..{}\n", short_oid(new)));
            }
            (_, true, Some(old), _) => {
                out.push_str(&format!("index {}..0000000\n", short_oid(old)));
            }
            _ => {}
        }
        // Binary patches carry no pre/post images, so git omits `---`/`+++`.
        if !self.delta.binary {
            out.push_str(&format!("--- {}\n", self.old_label()));
            out.push_str(&format!("+++ {}\n", self.new_label()));
        }
        out
    }

    fn old_label(&self) -> String {
        if self.old_absent {
            "/dev/null".to_string()
        } else {
            format!("a/{}", self.delta.old_path)
        }
    }

    fn new_label(&self) -> String {
        if self.new_absent {
            "/dev/null".to_string()
        } else {
            format!("b/{}", self.delta.new_path)
        }
    }
}

/// First 7 hex characters, git's default abbreviation.
fn short_oid(id: &str) -> &str {
    &id[..7.min(id.len())]
}

/// Collects each hunk and its lines, copying the borrowed tokens into DTOs.
#[derive(Default)]
struct Collector {
    hunks: Vec<PatchHunk>,
    lines: Vec<Vec<PatchLine>>,
}

impl ConsumeHunk for Collector {
    type Out = (Vec<PatchHunk>, Vec<Vec<PatchLine>>);

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        // `before_hunk_start`/`after_hunk_start` are 1-based; a hunk starting
        // at line 1 therefore numbers its first line as 1.
        let mut old_lineno = header.before_hunk_start;
        let mut new_lineno = header.after_hunk_start;
        let mut collected = Vec::with_capacity(lines.len());
        for (kind, content) in lines {
            let (origin, old, new) = match kind {
                DiffLineKind::Context => {
                    let out = (" ", Some(old_lineno), Some(new_lineno));
                    old_lineno += 1;
                    new_lineno += 1;
                    out
                }
                DiffLineKind::Remove => {
                    let out = ("-", Some(old_lineno), None);
                    old_lineno += 1;
                    out
                }
                DiffLineKind::Add => {
                    let out = ("+", None, Some(new_lineno));
                    new_lineno += 1;
                    out
                }
            };
            collected.push(PatchLine {
                origin: origin.to_string(),
                content: lossy(content),
                old_lineno: old,
                new_lineno: new,
                no_newline: !content.ends_with(b"\n"),
            });
        }
        let old_start = empty_adjusted_start(header.before_hunk_start, header.before_hunk_len);
        let new_start = empty_adjusted_start(header.after_hunk_start, header.after_hunk_len);
        self.hunks.push(PatchHunk {
            old_start,
            old_lines: header.before_hunk_len,
            new_start,
            new_lines: header.after_hunk_len,
            header: format_hunk_header(
                old_start,
                header.before_hunk_len,
                new_start,
                header.after_hunk_len,
            ),
        });
        self.lines.push(collected);
        Ok(())
    }

    fn finish(self) -> Self::Out {
        (self.hunks, self.lines)
    }
}

/// git writes an empty range as `<start-1>,0` (e.g. `-0,0` for a new file),
/// while gix reports the 1-based position regardless.
fn empty_adjusted_start(start: u32, len: u32) -> u32 {
    if len == 0 {
        start.saturating_sub(1)
    } else {
        start
    }
}

/// Renders `@@ -a,b +c,d @@`, omitting `,1` like git does and always keeping
/// the count when it is not exactly one.
fn format_hunk_header(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> String {
    fn range(start: u32, len: u32) -> String {
        match len {
            1 => start.to_string(),
            n => format!("{start},{n}"),
        }
    }
    format!(
        "@@ -{} +{} @@",
        range(old_start, old_lines),
        range(new_start, new_lines)
    )
}

fn looks_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|b| *b == 0)
}

fn find_funcname(old: &[u8], hunk_start: u32, hunk_lines: u32) -> Option<String> {
    // Number of pre-image lines strictly before the hunk's first old line. A
    // pure insertion (`hunk_lines == 0`) has an `hunk_start` naming the line the
    // text is inserted after, so that line is still a candidate.
    let before = if hunk_lines == 0 {
        hunk_start as usize
    } else {
        hunk_start.saturating_sub(1) as usize
    };
    let mut window: Vec<&[u8]> = old.split(|b| *b == b'\n').take(before).collect();
    window.reverse();
    window
        .into_iter()
        .take(100)
        .find(|line| {
            matches!(line.first(), Some(first) if first.is_ascii_alphabetic() || *first == b'_' || *first == b'$')
        })
        .map(|line| {
            // Control characters (tabs, CR) never reach the header; git also
            // caps the context at 40 characters.
            lossy(line)
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(40)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
}

fn count_stats(lines: &[Vec<PatchLine>]) -> PatchLineStats {
    let mut stats = PatchLineStats::default();
    for line in lines.iter().flatten() {
        match line.origin.as_str() {
            " " => stats.context += 1,
            "+" => stats.additions += 1,
            _ => stats.deletions += 1,
        }
    }
    stats
}

fn diff_algorithm(algorithm: Option<&Algorithm>) -> gix::diff::blob::Algorithm {
    match algorithm {
        Some(Algorithm::Myers) => gix::diff::blob::Algorithm::Myers,
        Some(Algorithm::MyersMinimal) => gix::diff::blob::Algorithm::MyersMinimal,
        Some(Algorithm::Histogram) | None => gix::diff::blob::Algorithm::Histogram,
    }
}

/// Everything needed to build a [`Patch`] that is not derived from the content.
struct PatchSource {
    old_path: String,
    new_path: String,
    old_id: Option<gix::ObjectId>,
    new_id: Option<gix::ObjectId>,
    old_mode: String,
    new_mode: String,
    status: &'static str,
    old_absent: bool,
    new_absent: bool,
}

const DEFAULT_MODE: &str = "100644";

fn build_patch(
    old: &[u8],
    new: &[u8],
    source: PatchSource,
    options: Option<&PatchOptions>,
) -> napi::Result<Patch> {
    let context_lines = options.and_then(|o| o.context_lines).unwrap_or(3);
    let algorithm = diff_algorithm(options.and_then(|o| o.algorithm.as_ref()));
    let force_text = options.and_then(|o| o.text).unwrap_or(false);
    let binary = !force_text && (looks_binary(old) || looks_binary(new));

    let (mut hunks, lines) = if binary {
        (Vec::new(), Vec::new())
    } else {
        // `&[u8]` is a token source that yields whole lines (newline included),
        // so the slider heuristics can rank hunks by indentation right away.
        let input = InternedInput::new(old, new);
        let diff = diff_with_slider_heuristics(algorithm, &input);
        UnifiedDiff::new(
            &diff,
            &input,
            Collector::default(),
            ContextSize::symmetrical(context_lines),
        )
        .consume()
        .map_err(|e| err("patch", e))?
    };
    if !binary {
        for hunk in hunks.iter_mut() {
            if let Some(funcname) = find_funcname(old, hunk.old_start, hunk.old_lines) {
                hunk.header.push(' ');
                hunk.header.push_str(&funcname);
            }
        }
    }
    let stats = count_stats(&lines);
    Ok(Patch {
        delta: PatchDelta {
            old_path: source.old_path,
            new_path: source.new_path,
            old_id: source.old_id.map(|id| id.to_string()),
            new_id: source.new_id.map(|id| id.to_string()),
            old_mode: source.old_mode,
            new_mode: source.new_mode,
            status: source.status.to_string(),
            binary,
        },
        hunks,
        lines,
        stats,
        old_absent: source.old_absent,
        new_absent: source.new_absent,
    })
}

/// Read a blob's content; `None` yields the empty pre/post-image of an
/// added/deleted file.
fn read_side(
    r: &gix::Repository,
    what: &str,
    id: Option<&str>,
) -> napi::Result<(Vec<u8>, Option<gix::ObjectId>)> {
    match id {
        Some(hex) => {
            let oid = parse_oid(hex)?;
            let blob = r.find_blob(oid).map_err(|e| err(what, e))?;
            Ok((blob.data.to_vec(), Some(oid)))
        }
        None => Ok((Vec::new(), None)),
    }
}

#[napi]
impl Repository {
    /// Build a patch from a single [`Change`] returned by
    /// [`diff_tree_to_tree`](Self::diff_tree_to_tree) — the NAPI counterpart of
    /// `git2::Patch::from_diff`.
    ///
    /// Returns `null` when the change carries no content difference (both
    /// sides missing, or identical ids), mirroring `Patch::from_diff` returning
    /// `None` for unchanged files.
    #[napi]
    pub fn patch_from_change(
        &self,
        change: Change,
        options: Option<PatchOptions>,
    ) -> napi::Result<Option<Patch>> {
        let what = "patch_from_change";
        let old_id = change.old_id.as_deref();
        let new_id = change.new_id.as_deref();
        if old_id == new_id {
            // Same blob (or a metadata-only no-op): nothing to render.
            return Ok(None);
        }
        let r = thread_local(&self.inner);
        let (old, old_oid) = read_side(&r, what, old_id)?;
        let (new, new_oid) = read_side(&r, what, new_id)?;
        let old_path = change
            .source_location
            .clone()
            .unwrap_or_else(|| change.path.clone());
        let source = PatchSource {
            old_path,
            new_path: change.path.clone(),
            old_id: old_oid,
            new_id: new_oid,
            old_mode: change
                .previous_entry_mode
                .clone()
                .unwrap_or_else(|| change.entry_mode.clone()),
            new_mode: if new_oid.is_some() {
                change.entry_mode.clone()
            } else {
                String::new()
            },
            status: kind_str(&change.kind),
            old_absent: old_oid.is_none(),
            new_absent: new_oid.is_none(),
        };
        Ok(Some(build_patch(&old, &new, source, options.as_ref())?))
    }

    /// Build a patch by diffing two blobs — `git2::Patch::from_blobs`.
    ///
    /// Either id may be omitted to represent an added (`oldId: null`) or
    /// deleted (`newId: null`) file. Paths default to `"a"`/`"b"` when omitted.
    #[napi]
    pub fn patch_from_blobs(
        &self,
        old_id: Option<String>,
        old_path: Option<String>,
        new_id: Option<String>,
        new_path: Option<String>,
        options: Option<PatchOptions>,
    ) -> napi::Result<Patch> {
        let what = "patch_from_blobs";
        let r = thread_local(&self.inner);
        let (old, old_oid) = read_side(&r, what, old_id.as_deref())?;
        let (new, new_oid) = read_side(&r, what, new_id.as_deref())?;
        let status = match (&old_oid, &new_oid) {
            (None, None) => "Unmodified",
            (None, Some(_)) => "Added",
            (Some(_), None) => "Deleted",
            (Some(a), Some(b)) if a == b => "Unmodified",
            _ => "Modified",
        };
        let source = PatchSource {
            old_path: old_path.unwrap_or_else(|| "a".to_string()),
            new_path: new_path.unwrap_or_else(|| "b".to_string()),
            old_id: old_oid,
            new_id: new_oid,
            old_mode: if old_oid.is_some() {
                DEFAULT_MODE.to_string()
            } else {
                String::new()
            },
            new_mode: if new_oid.is_some() {
                DEFAULT_MODE.to_string()
            } else {
                String::new()
            },
            status,
            old_absent: old_oid.is_none(),
            new_absent: new_oid.is_none(),
        };
        build_patch(&old, &new, source, options.as_ref())
    }

    /// Build a patch by diffing a blob against an in-memory buffer —
    /// `git2::Patch::from_blob_and_buffer`.
    ///
    /// The buffer has no blob id, so `delta.newId` is omitted and the rendered
    /// text has no `index` line.
    #[napi]
    pub fn patch_from_blob_and_buffer(
        &self,
        old_id: String,
        old_path: Option<String>,
        new_buffer: Buffer,
        new_path: Option<String>,
        options: Option<PatchOptions>,
    ) -> napi::Result<Patch> {
        let what = "patch_from_blob_and_buffer";
        let r = thread_local(&self.inner);
        let (old, old_oid) = read_side(&r, what, Some(&old_id))?;
        let new = new_buffer.to_vec();
        let source = PatchSource {
            old_path: old_path.unwrap_or_else(|| "a".to_string()),
            new_path: new_path.unwrap_or_else(|| "b".to_string()),
            old_id: old_oid,
            new_id: None,
            old_mode: DEFAULT_MODE.to_string(),
            new_mode: DEFAULT_MODE.to_string(),
            status: "Modified",
            old_absent: false,
            new_absent: false,
        };
        build_patch(&old, &new, source, options.as_ref())
    }

    /// Build a patch by diffing two in-memory buffers —
    /// `git2::Patch::from_buffers`.
    ///
    /// No blob ids exist for either side, so `delta.oldId`/`delta.newId` are
    /// `null` and the rendered text omits the `index` line.
    #[napi]
    pub fn patch_from_buffers(
        &self,
        old_buffer: Buffer,
        old_path: Option<String>,
        new_buffer: Buffer,
        new_path: Option<String>,
        options: Option<PatchOptions>,
    ) -> napi::Result<Patch> {
        let old = old_buffer.to_vec();
        let new = new_buffer.to_vec();
        let status = match (old.is_empty(), new.is_empty(), old == new) {
            (_, _, true) => "Unmodified",
            (true, false, false) => "Added",
            (false, true, false) => "Deleted",
            _ => "Modified",
        };
        let source = PatchSource {
            old_path: old_path.unwrap_or_else(|| "a".to_string()),
            new_path: new_path.unwrap_or_else(|| "b".to_string()),
            old_id: None,
            new_id: None,
            old_mode: DEFAULT_MODE.to_string(),
            new_mode: DEFAULT_MODE.to_string(),
            status,
            old_absent: status == "Added",
            new_absent: status == "Deleted",
        };
        build_patch(&old, &new, source, options.as_ref())
    }

    /// Generate `git format-patch`-style mailbox patches for the commits in
    /// `rev_spec`, writing one file per commit into `options.outputDir` (or the
    /// worktree root) and returning their absolute paths in chronological
    /// (oldest-first) order. Reimplemented on gix — no `git` subprocess.
    ///
    /// `apply` / `am` are added in a later increment.
    #[napi]
    pub fn format_patch(
        &self,
        rev_spec: String,
        options: Option<FormatPatchOptions>,
    ) -> napi::Result<Vec<String>> {
        let what = "format_patch";
        let r = thread_local(&self.inner);
        let repo = Repository {
            inner: Arc::clone(&self.inner),
        };
        let mut ids = repo.rev_walk(
            vec![],
            Some(WalkOptions {
                sort: None,
                reverse: None,
                first_parent: None,
                exclude: None,
                glob: None,
                range: Some(rev_spec),
                limit: None,
            }),
        )?;
        // `rev_walk` yields newest-first; `git format-patch` numbers oldest-first.
        ids.reverse();
        let total = ids.len();
        let opts = options.as_ref();
        let root = opts
            .and_then(|o| o.output_dir.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                r.workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| r.path().to_path_buf())
            });
        let numbered = opts.and_then(|o| o.numbered).unwrap_or(false);
        let signoff = opts.and_then(|o| o.signoff).unwrap_or(false);
        let mut paths = Vec::with_capacity(total);
        for (i, id) in ids.iter().enumerate() {
            let (mbox, slug) = format_one_mbox(&r, &repo, id, i, total, numbered, signoff, what)?;
            let full = root.join(format!("{:04}-{}.patch", i + 1, slug));
            std::fs::write(&full, mbox).map_err(|e| err(what, e))?;
            paths.push(full.to_string_lossy().to_string());
        }
        Ok(paths)
    }

    /// Like [`format_patch`](Self::format_patch) but returns the concatenated
    /// mailbox text (`git format-patch --stdout`) instead of writing files.
    #[napi]
    pub fn format_patch_to_string(
        &self,
        rev_spec: String,
        options: Option<FormatPatchOptions>,
    ) -> napi::Result<String> {
        let what = "format_patch";
        let r = thread_local(&self.inner);
        let repo = Repository {
            inner: Arc::clone(&self.inner),
        };
        let mut ids = repo.rev_walk(
            vec![],
            Some(WalkOptions {
                sort: None,
                reverse: None,
                first_parent: None,
                exclude: None,
                glob: None,
                range: Some(rev_spec),
                limit: None,
            }),
        )?;
        ids.reverse();
        let total = ids.len();
        let opts = options.as_ref();
        let numbered = opts.and_then(|o| o.numbered).unwrap_or(false);
        let signoff = opts.and_then(|o| o.signoff).unwrap_or(false);
        let mut out = String::new();
        for (i, id) in ids.iter().enumerate() {
            let (mbox, _slug) = format_one_mbox(&r, &repo, id, i, total, numbered, signoff, what)?;
            out.push_str(&mbox);
        }
        Ok(out)
    }
}

/// Normalise the `Change.kind` strings produced by `diff.rs`, keeping git2's
/// `DiffDelta` vocabulary.
fn kind_str(kind: &str) -> &'static str {
    match kind {
        "Added" => "Added",
        "Deleted" => "Deleted",
        "Renamed" => "Renamed",
        "Copied" => "Copied",
        "Modified" => "Modified",
        _ => "Modified",
    }
}

/// The git object id of the empty tree, used as the "before" side when a commit
/// has no parent (a root commit shows every file as added).
/// Options controlling [`Repository::format_patch`] / [`Repository::format_patch_to_string`].
#[napi(object)]
pub struct FormatPatchOptions {
    /// Directory to write patch files into (defaults to the worktree root).
    pub output_dir: Option<String>,
    /// Force `[PATCH n/m]` subject prefixes even for a single patch.
    pub numbered: Option<bool>,
    /// Add a `Signed-off-by` trailer to each generated patch.
    pub signoff: Option<bool>,
}

/// The git object id of the empty tree, used as the "before" side when a commit
/// has no parent (a root commit shows every file as added).
const EMPTY_TREE_ID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Render one commit as a mailbox-style patch, returning the text and a
/// filesystem-safe subject slug for naming the file.
///
/// Reimplements `git format-patch` on gix: the commit's tree is diffed against
/// its first parent (or the empty tree for a root commit), the per-file diffs
/// are rendered with the shared [`Patch`] machinery, and the result is wrapped
/// in mailbox form — no `git` subprocess.
fn format_one_mbox(
    r: &gix::Repository,
    repo: &Repository,
    id: &str,
    index: usize,
    total: usize,
    numbered: bool,
    signoff: bool,
    what: &str,
) -> napi::Result<(String, String)> {
    let oid = parse_oid(id)?;
    let commit = r.find_commit(oid).map_err(|e| err(what, e))?;
    let commit = commit.decode().map_err(|e| err(what, e))?;
    let tree_id = commit.tree().to_string();
    let parent_tree = commit.parents().next().map(|p| p.to_string());
    let old = parent_tree.or_else(|| Some(EMPTY_TREE_ID.to_string()));
    let changes = repo.diff_tree_to_tree(old, Some(tree_id), None)?;
    let mut diff_text = String::new();
    for change in changes {
        if let Some(patch) = repo.patch_from_change(change, None)? {
            diff_text.push_str(&patch.to_buf(None));
        }
    }
    let author = commit.author().map_err(|e| err(what, e))?;
    let time = author.time().map_err(|e| err(what, e))?;
    let name = lossy(author.name);
    let email = lossy(author.email);
    let date = rfc2822(time.seconds, time.offset);
    let msg = commit.message();
    let subject = lossy(msg.title);
    let body = msg.body.map(|b| lossy(b)).unwrap_or_default();
    let prefix = if numbered || total > 1 {
        format!("[PATCH {}/{}] ", index + 1, total)
    } else {
        "[PATCH] ".to_string()
    };
    let mut mbox = String::new();
    mbox.push_str(&format!("From {} {}\n", name, date));
    mbox.push_str(&format!("From: {} <{}>\n", name, email));
    mbox.push_str(&format!("Date: {}\n", date));
    mbox.push_str(&format!("Subject: {}{}\n\n", prefix, subject));
    if signoff {
        mbox.push_str(&format!("Signed-off-by: {} <{}>\n", name, email));
    }
    if !body.is_empty() {
        mbox.push_str(body.trim_end());
        mbox.push('\n');
    }
    mbox.push_str("\n---\n\n");
    mbox.push_str(&diff_text);
    mbox.push_str("\n-- \n2.0\n");
    Ok((mbox, slugify(&subject)))
}

/// Render a git `Time` (seconds + offset in seconds) as an RFC 2822 date used
/// by mailbox `Date:` headers, e.g. `Mon, 01 Jan 2024 12:00:00 +0000`.
fn rfc2822(seconds: i64, offset: i32) -> String {
    match DateTime::<Utc>::from_timestamp(seconds, 0) {
        Some(dt) => {
            let offset =
                FixedOffset::east_opt(offset).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
            dt.with_timezone(&offset).to_rfc2822()
        }
        None => String::new(),
    }
}

/// git-style subject slug: lowercase, runs of non-alphanumerics collapsed to
/// single dashes, trimmed.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}
