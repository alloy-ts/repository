use crate::error::RepositoryResult as Result;
use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};
use napi_derive::napi;
use std::path::Path;
use std::sync::Arc;

use gix::object::tree::EntryKind;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;

/// Where a worktree/index `apply` (not yet implemented) would write the result.
/// Mirrors git2's `git_apply_location_t`. Only meaningful once `Repository::apply`
/// is implemented; `apply_to_tree` ignores it.
#[napi(string_enum)]
pub enum ApplyLocation {
    /// Apply the patch to the workdir.
    WorkDir,
    /// Apply the patch to the index.
    Index,
    /// Apply the patch to both the working directory and the index.
    Both,
}

/// Options controlling patch application, mirroring git2's `ApplyOptions`.
///
/// `check` is accepted everywhere (dry-run) but is currently a no-op for
/// `apply_to_tree` (which always returns the resulting tree) — it becomes
/// meaningful once `Repository::apply` (worktree) is implemented. `location` is
/// used only by the deferred `Repository::apply`.
#[napi(object)]
pub struct ApplyOptions {
    /// Validate the patch applies without writing the result (dry-run).
    pub check: Option<bool>,
    /// Where to apply when using `Repository::apply` (not yet implemented).
    pub location: Option<ApplyLocation>,
}

// ---------------------------------------------------------------------------
// Unified-diff parser + applier (gix has no apply engine in 0.89)
// ---------------------------------------------------------------------------

struct Hunk {
    old_start: usize,
    #[allow(dead_code)]
    old_count: usize,
    #[allow(dead_code)]
    new_start: usize,
    #[allow(dead_code)]
    new_count: usize,
    /// (tag, line-content) where tag is b' ' (context), b'+' (add), b'-' (remove).
    lines: Vec<(u8, Vec<u8>)>,
}

struct FileChange {
    old_path: Option<String>,
    new_path: Option<String>,
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    deleted: bool,
    hunks: Vec<Hunk>,
}

fn strip_file_prefix(s: &str) -> String {
    // git writes "a/path" / "b/path"; "i/" and "w/" also appear. Strip a 2-char
    // "x/" prefix; leave "/dev/null" and quoted paths alone for this draft.
    if s.len() >= 2 && s.as_bytes()[1] == b'/' && (s.starts_with('a') || s.starts_with('b')) {
        s[2..].to_string()
    } else {
        s.to_string()
    }
}

fn parse_mode(s: &str) -> napi::Result<u32> {
    u32::from_str_radix(s.trim(), 8).map_err(|_| err_str("apply", &format!("invalid mode: {s}")))
}

fn parse_range(s: &str) -> napi::Result<(usize, usize)> {
    let s = s.trim_start_matches(['-', '+']);
    if let Some((a, b)) = s.split_once(',') {
        Ok((
            a.parse().map_err(|_| err_str("apply", "bad hunk range"))?,
            b.parse().map_err(|_| err_str("apply", "bad hunk range"))?,
        ))
    } else {
        Ok((
            s.parse().map_err(|_| err_str("apply", "bad hunk range"))?,
            1,
        ))
    }
}

fn parse_hunk_header(line: &str) -> napi::Result<Hunk> {
    let inner = line
        .trim()
        .trim_start_matches("@@")
        .trim_end_matches("@@")
        .trim();
    let mut parts = inner.split(' ');
    let old = parts
        .next()
        .ok_or_else(|| err_str("apply", "bad hunk header"))?;
    let new = parts
        .next()
        .ok_or_else(|| err_str("apply", "bad hunk header"))?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Ok(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_patch(patch: &str) -> napi::Result<Vec<FileChange>> {
    let lines: Vec<&str> = patch.split('\n').collect();
    let mut changes = Vec::new();
    let mut cur: Option<FileChange> = None;
    let mut cur_hunk: Option<Hunk> = None;

    let flush = |cur: &mut Option<FileChange>,
                 cur_hunk: &mut Option<Hunk>,
                 changes: &mut Vec<FileChange>| {
        if let Some(mut c) = cur.take() {
            if let Some(h) = cur_hunk.take() {
                c.hunks.push(h);
            }
            changes.push(c);
        }
    };

    for line in lines {
        if line.starts_with("diff --git ") {
            flush(&mut cur, &mut cur_hunk, &mut changes);
            let rest = &line["diff --git ".len()..];
            let mut it = rest.split(' ');
            let a = it.next().unwrap_or("");
            let b = it.next().unwrap_or("");
            let a = strip_file_prefix(a);
            let b = strip_file_prefix(b);
            cur = Some(FileChange {
                old_path: Some(a),
                new_path: Some(b),
                old_mode: None,
                new_mode: None,
                deleted: false,
                hunks: Vec::new(),
            });
        } else if let Some(c) = cur.as_mut() {
            if line.starts_with("@@ ") {
                if let Some(h) = cur_hunk.take() {
                    c.hunks.push(h);
                }
                cur_hunk = Some(parse_hunk_header(line)?);
            } else if line.starts_with("--- ") {
                let p = strip_file_prefix(&line[4..]);
                c.old_path = if p == "/dev/null" { None } else { Some(p) };
            } else if line.starts_with("+++ ") {
                let p = strip_file_prefix(&line[4..]);
                c.new_path = if p == "/dev/null" { None } else { Some(p) };
            } else if line.starts_with("new file mode ") {
                c.new_mode = Some(parse_mode(&line[15..])?);
            } else if line.starts_with("deleted file mode ") {
                c.deleted = true;
                c.old_mode = Some(parse_mode(&line[19..])?);
            } else if line.starts_with("old mode ") {
                c.old_mode = Some(parse_mode(&line[9..])?);
            } else if line.starts_with("new mode ") {
                c.new_mode = Some(parse_mode(&line[9..])?);
            } else if line.starts_with("rename from ") {
                c.old_path = Some(line[12..].to_string());
            } else if line.starts_with("rename to ") {
                c.new_path = Some(line[10..].to_string());
            } else if line.starts_with("copy from ") {
                c.old_path = Some(line[10..].to_string());
            } else if line.starts_with("copy to ") {
                c.new_path = Some(line[8..].to_string());
            } else if line.starts_with("GIT binary patch") || line.starts_with("Binary files") {
                return Err(err_str("apply", "binary patches are not supported yet"));
            } else if line.starts_with('\\') {
                // "\ No newline at end of file" — ignored for this draft.
            } else if let Some(h) = cur_hunk.as_mut() {
                if line.is_empty() {
                    continue;
                }
                let bytes = line.as_bytes();
                let tag = bytes[0];
                if tag == b' ' || tag == b'+' || tag == b'-' {
                    h.lines.push((tag, bytes[1..].to_vec()));
                }
            }
        }
    }
    flush(&mut cur, &mut cur_hunk, &mut changes);
    Ok(changes)
}

/// Split bytes into lines without trailing-newline artifacts.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&[u8]> = data.split(|&c| c == b'\n').collect();
    if data.ends_with(&[b'\n']) {
        v.pop();
    }
    v
}

/// Apply a file's hunks onto `old` content, returning the new content.
fn apply_hunks(old: &[u8], hunks: &[Hunk]) -> napi::Result<Vec<u8>> {
    let old_lines = split_lines(old);
    let had_trailing_newline = old.ends_with(&[b'\n']) || old.is_empty();
    let mut out: Vec<u8> = Vec::new();
    let mut old_pos: usize = 0;

    for h in hunks {
        old_pos = if h.old_start == 0 { 0 } else { h.old_start - 1 };
        for (tag, content) in &h.lines {
            match tag {
                b' ' => {
                    if old_pos >= old_lines.len() || old_lines[old_pos] != content.as_slice() {
                        return Err(err_str("apply", "context mismatch while applying patch"));
                    }
                    out.extend_from_slice(content);
                    out.push(b'\n');
                    old_pos += 1;
                }
                b'-' => {
                    if old_pos >= old_lines.len() || old_lines[old_pos] != content.as_slice() {
                        return Err(err_str(
                            "apply",
                            "removed line mismatch while applying patch",
                        ));
                    }
                    old_pos += 1;
                }
                b'+' => {
                    out.extend_from_slice(content);
                    out.push(b'\n');
                }
                _ => {}
            }
        }
    }
    while old_pos < old_lines.len() {
        out.extend_from_slice(old_lines[old_pos]);
        out.push(b'\n');
        old_pos += 1;
    }
    if !had_trailing_newline && out.last() == Some(&b'\n') {
        out.pop();
    }
    Ok(out)
}

fn read_path_blob(
    r: &gix::Repository,
    base_oid: gix::ObjectId,
    path: &str,
) -> napi::Result<Vec<u8>> {
    let tree = r.find_tree(base_oid).map_err(|e| err("find_tree", e))?;
    let entry = tree
        .lookup_entry_by_path(Path::new(path))
        .map_err(|e| err("apply_lookup", e))?
        .ok_or_else(|| err_str("apply", &format!("path {path} not found in base tree")))?;
    let blob = r
        .find_blob(entry.object_id())
        .map_err(|e| err("find_blob", e))?;
    Ok(blob.data.to_vec())
}

fn apply_patch_to_tree(
    r: &gix::Repository,
    base_tree_id: &str,
    patch: &str,
) -> napi::Result<String> {
    let base_oid = parse_oid(base_tree_id)?;
    let changes = parse_patch(patch)?;
    let mut editor = r.edit_tree(base_oid).map_err(|e| err("edit_tree", e))?;

    for ch in &changes {
        let old_path = ch.old_path.clone();
        let new_path = ch.new_path.clone();
        let old_data = match &old_path {
            Some(p) => read_path_blob(r, base_oid, p.as_str())?,
            None => Vec::new(),
        };
        let new_blob: Option<Vec<u8>> = if ch.deleted {
            None
        } else if ch.hunks.is_empty() {
            // Rename/copy or mode-only change: content is unchanged.
            if old_path.is_some() {
                Some(old_data.clone())
            } else {
                None
            }
        } else {
            Some(apply_hunks(&old_data, &ch.hunks)?)
        };

        let mode = ch.new_mode.or(ch.old_mode).unwrap_or(0o100644);
        let kind: EntryKind = crate::entry::mode_to_entry_kind(&format!("{mode:o}"))
            .map_err(|e| err("mode_to_entry_kind", e))?;

        match (new_path, new_blob, ch.deleted) {
            (None, _, true) => {
                let p = old_path.ok_or_else(|| err_str("apply", "deleted file without a path"))?;
                editor
                    .remove(p.as_str())
                    .map_err(|e| err("apply_remove", e))?;
            }
            (Some(np), Some(data), _) => {
                let blob_id = r.write_blob(&data).map_err(|e| err("write_blob", e))?;
                editor
                    .upsert(np.as_str(), kind, blob_id.detach())
                    .map_err(|e| err("apply_upsert", e))?;
                if let Some(op) = &old_path {
                    if op != &np {
                        editor
                            .remove(op.as_str())
                            .map_err(|e| err("apply_remove", e))?;
                    }
                }
            }
            (Some(np), None, _) => {
                // Rename/copy without content change.
                let blob_id = r.write_blob(&old_data).map_err(|e| err("write_blob", e))?;
                editor
                    .upsert(np.as_str(), kind, blob_id.detach())
                    .map_err(|e| err("apply_upsert", e))?;
                if let Some(op) = &old_path {
                    if op != &np {
                        editor
                            .remove(op.as_str())
                            .map_err(|e| err("apply_remove", e))?;
                    }
                }
            }
            _ => {
                return Err(err_str(
                    "apply",
                    "patch hunk produced neither a target path nor a deletion",
                ));
            }
        }
    }

    let new_id = editor.write().map_err(|e| err("apply_write", e))?;
    Ok(new_id.detach().to_string())
}

#[napi]
impl Repository {
    /// Apply a unified-diff `patch` onto `baseTreeId` and return the resulting
    /// tree id. Mirrors git2's `Repository::apply_to_tree` (without the `Diff`
    /// object, which gix/this binding doesn't surface — the patch string is the
    /// input instead).
    ///
    /// Supports the common patch forms: context/added/removed lines, new files,
    /// deleted files, mode-only changes, and rename/copy without content edits.
    /// Binary patches are rejected. `options.check` is accepted but ignored here.
    #[napi]
    pub fn apply_to_tree(
        &self,
        patch: String,
        base_tree_id: String,
        options: Option<ApplyOptions>,
    ) -> napi::Result<String> {
        let _ = options;
        let r = thread_local(&self.inner);
        apply_patch_to_tree(&r, &base_tree_id, &patch)
    }

    /// Async variant of [`Repository::apply_to_tree`].
    #[napi(ts_return_type = "Promise<string>")]
    pub fn apply_to_tree_async(
        &self,
        patch: String,
        base_tree_id: String,
        options: Option<ApplyOptions>,
    ) -> napi::Result<AsyncTask<ApplyToTreeTask>> {
        Ok(AsyncTask::new(ApplyToTreeTask {
            repo: Arc::clone(&self.inner),
            patch,
            base_tree_id,
            options,
        }))
    }

    /// Apply a patch to the worktree and/or index (git2's `Repository::apply`).
    ///
    /// **Not yet implemented**: gix 0.89 has no apply engine, and this binding
    /// has no `Diff` object to feed it. Use [`Repository::apply_to_tree`] for a
    /// pure, worktree-free result, or shell out to `git apply` directly.
    #[napi]
    pub fn apply(&self, _patch: String, _options: Option<ApplyOptions>) -> napi::Result<()> {
        Err(err_str(
            "apply",
            "worktree/index apply is not yet implemented: gix 0.89 has no apply engine. \
             Use applyToTree for a pure tree result, or apply via `git` directly.",
        ))
    }
}

pub struct ApplyToTreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) patch: String,
    pub(crate) base_tree_id: String,
    pub(crate) options: Option<ApplyOptions>,
}
impl Task for ApplyToTreeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> napi::Result<String> {
        let _ = &self.options;
        let r = thread_local(&self.repo);
        apply_patch_to_tree(&r, &self.base_tree_id, &self.patch)
    }
    fn resolve(&mut self, _env: Env, output: String) -> napi::Result<String> {
        Ok(output)
    }
}
