use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::RepositoryResult as Result;
use gix::bstr::{BStr, BString};
use gix::dir::entry::Status as DirStatus;
use gix::object::tree::{EntryKind, EntryMode};
use gix::objs::{Kind as ObjectKind, WriteTo};
use gix::refs::transaction::{PreviousValue, RefEdit};
use gix::refs::{FullName, Target};
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::repository::Repository;

/// Flags controlling what `saveStash` captures, mirroring git's stash flags.
/// Use [`flag_bits`] to map a flag to its git stash bit.
///
/// `KeepAll` and `NoRefresh` are accepted for API parity but have no effect on the
/// current gix-backed implementation; `IncludeIgnored` is not yet supported.
#[napi]
pub enum StashFlags {
    /// Capture only tracked changes.
    Default = 0,
    /// Keep the index as-is instead of resetting it after the stash is made.
    KeepIndex = 1,
    /// Also stash untracked files.
    IncludeUntracked = 2,
    /// Also stash ignored files (not yet supported).
    IncludeIgnored = 4,
    /// Keep all changes in the working tree (index + untracked) without resetting.
    KeepAll = 8,
    /// Do not refresh the index before stashing.
    NoRefresh = 16,
}

/// Options for [`Repository::saveStash`].
#[napi(object)]
pub struct StashSaveOptions {
    /// Stash label; surfaced as the reflog message. When absent, git's
    /// `WIP on <branch>: <abbrev>` form is used.
    pub message: Option<String>,
    /// One of [`StashFlags`]; defaults to [`StashFlags::Default`].
    pub flags: Option<StashFlags>,
    /// Restrict the stash to these pathspecs (not yet supported).
    pub pathspecs: Option<Vec<String>>,
}

/// Outcome of a stash apply or pop operation.
#[derive(Clone)]
#[napi(object)]
pub struct StashApplyOutcome {
    /// `true` if the stash applied cleanly without conflicts.
    pub applied: bool,
    /// Conflicted file paths if the stash apply encountered conflicts.
    pub conflicts: Option<Vec<String>>,
}

/// A single stash, as returned by [`Repository::listStashes`].
#[napi(object)]
pub struct StashEntry {
    /// Position in the stash list; `0` is the most recent stash.
    pub index: u32,
    /// Branch the stash was created on, parsed from the reflog message
    /// (e.g. `main`, or `(no branch)` for a stash made on a detached HEAD).
    pub branch: String,
    /// The stash subject: a custom message when one was given, otherwise the
    /// auto-generated `<short-sha> <commit-subject>`.
    pub subject: String,
    /// The full reflog message (e.g. `On main: <label>`).
    pub message: String,
    /// Hex object id of the stash commit.
    pub id: String,
}

/// Map a single [`StashFlags`] value to its git stash bit. Only one flag is
/// honored at a time in this implementation (KeepIndex / IncludeUntracked).
fn flag_bits(f: StashFlags) -> u32 {
    match f {
        StashFlags::Default => 0,
        StashFlags::KeepIndex => 1,
        StashFlags::IncludeUntracked => 2,
        StashFlags::IncludeIgnored => 4,
        StashFlags::KeepAll => 8,
        StashFlags::NoRefresh => 16,
    }
}

/// A tracked (or untracked) file: repo-relative path components, blob id, and mode.
type TreeItem = (Vec<String>, gix::ObjectId, EntryMode);

/// Wrapper that lets us write an arbitrary byte buffer as a tree object.
struct RawTree(Vec<u8>);

impl WriteTo for RawTree {
    fn write_to(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        out.write_all(&self.0)?;
        Ok(())
    }
    fn size(&self) -> u64 {
        self.0.len() as u64
    }
    fn kind(&self) -> ObjectKind {
        ObjectKind::Tree
    }
}

#[napi]
impl Repository {
    /// Save the working directory and index state into a new stash commit, recording
    /// it in the `refs/stash` reflog. Returns `null` when there is nothing to stash.
    ///
    /// The stash commit has the structure git uses: its tree is the working-tree
    /// state (or the index when [`StashFlags::KeepIndex`] is set), with the HEAD
    /// commit as the first parent, the staged state as the second, and — when
    /// [`StashFlags::IncludeUntracked`] is set — untracked files as the third.
    #[napi]
    pub fn save_stash(&self, opts: StashSaveOptions) -> napi::Result<Option<String>> {
        let repo = thread_local(&self.inner);
        let workdir = repo
            .workdir()
            .ok_or_else(|| err_str("save_stash", "stash requires a working tree"))?;

        let head = repo.head_commit().map_err(|e| err("head_commit", e))?;
        let head_id = head.id;
        let head_tree_id = head.tree_id().map_err(|e| err("tree_id", e))?.detach();

        let bits = opts.flags.map(flag_bits).unwrap_or(0);
        let keep_index = bits & 1 != 0;
        let include_untracked = bits & 2 != 0;

        let index = repo.index().map_err(|e| err("index", e))?;
        let index_items = index_items(&index)?;
        let index_tree_id = write_tree(&repo, &index_items)?;

        let work_tree_id = if keep_index {
            index_tree_id
        } else {
            let work_items = working_items(&repo, workdir, &index)?;
            write_tree(&repo, &work_items)?
        };

        let untracked_tree_id = if include_untracked {
            let untracked_items = untracked_items(&repo, workdir)?;
            if untracked_items.is_empty() {
                None
            } else {
                Some(write_tree(&repo, &untracked_items)?)
            }
        } else {
            None
        };

        // Nothing to stash: working tree and index both match HEAD and there are
        // no untracked files being captured.
        if untracked_tree_id.is_none()
            && work_tree_id == head_tree_id
            && index_tree_id == head_tree_id
        {
            return Ok(None);
        }

        let branch = head_branch_name(&repo);
        let head_hex = head_id.to_string();
        let label = match opts.message.filter(|m| !m.is_empty()) {
            Some(m) => format!("On {branch}: {m}"),
            None => format!("WIP on {branch}: {}", &head_hex[..7.min(head_hex.len())]),
        };

        let index_commit = repo
            .new_commit(&label, index_tree_id, vec![head_id])
            .map_err(|e| err("commit", e))?;
        let untracked_commit = match untracked_tree_id {
            Some(t) => Some(
                repo.new_commit(&label, t, vec![head_id])
                    .map_err(|e| err("commit", e))?,
            ),
            None => None,
        };

        let mut parents: Vec<gix::ObjectId> = vec![head_id];
        parents.push(index_commit.id);
        if let Some(u) = &untracked_commit {
            parents.push(u.id);
        }
        let stash_commit = repo
            .new_commit(&label, work_tree_id, parents)
            .map_err(|e| err("commit", e))?;
        let stash_id = stash_commit.id;

        update_stash_ref(&repo, stash_id, &label)?;
        Ok(Some(stash_id.to_string()))
    }

    /// List all stashes, newest first. Returns an empty array when there are none.
    #[napi]
    pub fn list_stashes(&self) -> napi::Result<Vec<StashEntry>> {
        let repo = thread_local(&self.inner);
        let git_dir = repo.git_dir();
        let log_path = Path::new(&git_dir).join("logs").join("refs").join("stash");
        if !log_path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&log_path).map_err(|e| err("read", e))?;
        let mut out = Vec::new();
        for (i, line) in content.lines().filter(|l| !l.is_empty()).rev().enumerate() {
            let meta = match line.find('\t') {
                Some(t) => &line[..t],
                None => line,
            };
            let new_oid = meta.split_whitespace().nth(1).unwrap_or("");
            let message = match line.find('\t') {
                Some(t) => &line[t + 1..],
                None => "",
            };
            let (branch, subject) = match message.split_once(": ") {
                Some((prefix, rest)) => {
                    let branch = prefix
                        .strip_prefix("WIP on ")
                        .or_else(|| prefix.strip_prefix("On "))
                        .unwrap_or(prefix);
                    (branch.to_string(), rest.to_string())
                }
                None => (String::new(), message.to_string()),
            };
            out.push(StashEntry {
                index: i as u32,
                branch,
                subject,
                message: message.to_string(),
                id: new_oid.to_string(),
            });
        }
        Ok(out)
    }

    /// Remove the stash at `index` (0 = most recent) from the `refs/stash` reflog.
    /// Removing the last remaining stash also deletes the `refs/stash` reference.
    #[napi]
    pub fn drop_stash(&self, index: u32) -> napi::Result<()> {
        let repo = thread_local(&self.inner);
        let git_dir = repo.git_dir();
        let log_path = Path::new(&git_dir).join("logs").join("refs").join("stash");
        if !log_path.exists() {
            return Err(err_str("drop_stash", format!("no stash at index {index}")).into());
        }
        let content = std::fs::read_to_string(&log_path).map_err(|e| err("read", e))?;
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        let idx = index as usize;
        if idx >= lines.len() {
            return Err(err_str("drop_stash", format!("no stash at index {index}")).into());
        }
        // Reflog file order is oldest-first, so index 0 (newest) is the last line.
        let remove_pos = lines.len() - 1 - idx;
        let remaining: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != remove_pos)
            .map(|(_, l)| *l)
            .collect();

        if remaining.is_empty() {
            let _ = std::fs::remove_file(&log_path);
            let _ = std::fs::remove_file(Path::new(&git_dir).join("refs").join("stash"));
        } else {
            std::fs::write(&log_path, format!("{}\n", remaining.join("\n")))
                .map_err(|e| err("write", e))?;
            // Dropping the newest stash moves the ref to the new top entry.
            if idx == 0 {
                let new_target = remaining
                    .last()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .ok_or_else(|| err_str("drop_stash", "corrupt refs/stash reflog"))?;
                std::fs::write(
                    Path::new(&git_dir).join("refs").join("stash"),
                    format!("{new_target}\n"),
                )
                .map_err(|e| err("write", e))?;
            }
        }
        Ok(())
    }

    /// Restore the stash at `index` (0 = most recent) into the working tree and the index.
    ///
    /// The stash commit is `stash^0` (the working-tree state at stash time), with parents
    /// `stash^1` (HEAD, the base), `stash^2` (the staged/index state at stash time), and
    /// optionally `stash^3` (untracked files). This restores the working tree from `stash^0`
    /// (and `stash^3` for untracked files) and the index from `stash^2`.
    ///
    /// This is a tree-walk restore, not a three-way merge. Before overwriting, each path is
    /// checked against its base (`stash^1`) version: if the current working-tree file differs
    /// from the base, the operation fails with a conflict error instead of clobbering local
    /// work. Deletion of files present locally but absent from the stash, and `KeepIndex`
    /// interactions on apply, are not yet handled beyond this guard.
    #[napi]
    pub fn apply_stash(&self, index: u32) -> napi::Result<()> {
        let repo = thread_local(&self.inner);
        let workdir = repo
            .workdir()
            .ok_or_else(|| err_str("apply_stash", "apply requires a working tree"))?;

        let stash_id = stash_commit_id_at(&repo, index)?;
        let commit = repo
            .find_commit(stash_id)
            .map_err(|e| err("find_commit", e))?;
        let work_tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?.detach();
        let parents: Vec<gix::ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();

        // stash^1 = base (HEAD at stash time); stash^2 = index tree; stash^3 = untracked tree.
        let base_tree_id = parents.first().copied();
        let index_tree_id = parents.get(1).copied();
        let untracked_tree_id = parents.get(2).copied();

        // Conflict-protective guard: refuse to overwrite local modifications.
        if let Some(base) = base_tree_id {
            let base_map: HashMap<Vec<String>, gix::ObjectId> = tree_entries(&repo, base)?
                .into_iter()
                .map(|(p, o, _)| (p, o))
                .collect();
            for (path, stash0_oid, _) in tree_entries(&repo, work_tree_id)? {
                let Some(&base_oid) = base_map.get(&path) else {
                    // New in the stash: cannot clobber a base path.
                    continue;
                };
                if stash0_oid == base_oid {
                    // The stash did not change this path relative to the base.
                    continue;
                }
                let base_bytes = read_blob(&repo, base_oid)?;
                let current = worktree_file_bytes(workdir, &path);
                if current.as_deref() != Some(&base_bytes[..]) {
                    return Err(err_str(
                        "apply_stash",
                        format!(
                            "conflict at '{}': local changes would be overwritten by the stash",
                            path.join("/")
                        ),
                    )
                    .into());
                }
            }
        }

        // Restore the working tree from the stash's worktree tree (stash^0).
        restore_tree_to_worktree(&repo, workdir, work_tree_id)?;
        // Untracked files (stash^3) go to the working tree only.
        if let Some(ut) = untracked_tree_id {
            restore_tree_to_worktree(&repo, workdir, ut)?;
        }
        // Restore the index from the stash's index tree (stash^2).
        if let Some(it) = index_tree_id {
            restore_index_from_tree(&repo, it)?;
        }
        Ok(())
    }

    /// Apply stash at `index`, returning a structured outcome indicating whether it applied or conflicted.
    #[napi]
    pub fn apply_stash_with_outcome(
        &self,
        index: u32,
        _restore_index: Option<bool>,
    ) -> napi::Result<StashApplyOutcome> {
        match self.apply_stash(index) {
            Ok(()) => Ok(StashApplyOutcome {
                applied: true,
                conflicts: None,
            }),
            Err(e) => {
                let err_msg = e.to_string();
                if let Some(start) = err_msg.find("conflict at '") {
                    let rest = &err_msg[start + 13..];
                    if let Some(end) = rest.find('\'') {
                        let path = &rest[..end];
                        return Ok(StashApplyOutcome {
                            applied: false,
                            conflicts: Some(vec![path.to_string()]),
                        });
                    }
                }
                Err(e)
            }
        }
    }

    /// Apply stash at `index` and drop it if applied cleanly.
    #[napi]
    pub fn pop_stash_with_outcome(
        &self,
        index: u32,
        _restore_index: Option<bool>,
    ) -> napi::Result<StashApplyOutcome> {
        let outcome = self.apply_stash_with_outcome(index, _restore_index)?;
        if outcome.applied {
            self.drop_stash(index)?;
        }
        Ok(outcome)
    }

    /// Apply the stash at `index` and then remove it from the stash list (equivalent to
    /// `git stash pop`).
    #[napi]
    pub fn pop_stash(&self, index: u32) -> napi::Result<()> {
        self.apply_stash(index)?;
        self.drop_stash(index)?;
        Ok(())
    }

    /// Remove every stash, deleting the `refs/stash` reference entirely. Equivalent to
    /// `git stash clear`. No-op when the stash list is already empty.
    #[napi]
    pub fn clear_stash(&self) -> napi::Result<()> {
        let repo = thread_local(&self.inner);
        let git_dir = repo.git_dir();
        let log_path = Path::new(&git_dir).join("logs").join("refs").join("stash");
        if log_path.exists() {
            let _ = std::fs::remove_file(&log_path);
        }
        let _ = std::fs::remove_file(Path::new(&git_dir).join("refs").join("stash"));
        Ok(())
    }
}

// --- helpers -----------------------------------------------------------------

fn head_branch_name(repo: &gix::Repository) -> String {
    match repo.head_name() {
        Ok(Some(name)) => {
            let s = String::from_utf8_lossy(name.as_bstr());
            s.strip_prefix("refs/heads/").unwrap_or(&s).to_string()
        }
        _ => "HEAD".to_string(),
    }
}

fn update_stash_ref(
    repo: &gix::Repository,
    stash_id: gix::ObjectId,
    label: &str,
) -> napi::Result<()> {
    let name: FullName = "refs/stash".try_into().map_err(|e| err("ref_name", e))?;
    let edit = match repo.find_reference(&name) {
        Ok(_) => RefEdit::update(
            name,
            Target::Object(stash_id),
            PreviousValue::MustExist,
            label,
        ),
        Err(_) => RefEdit::update(
            name,
            Target::Object(stash_id),
            PreviousValue::MustNotExist,
            label,
        ),
    };
    repo.edit_reference(edit)
        .map_err(|e| err("edit_reference", e))?;
    Ok(())
}

fn path_components(path: &BStr) -> Vec<String> {
    path.split(|b| *b == b'/')
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Read the index entries and return them as tree items (staged state).
fn index_items(index: &gix::worktree::Index) -> napi::Result<Vec<TreeItem>> {
    let mut out = Vec::new();
    for e in index.entries() {
        let path = e.path(index);
        let mode = e
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| err_str("index_items", "unsupported index entry mode"))?;
        out.push((path_components(path), e.id, mode));
    }
    Ok(out)
}

/// Read the working-tree files (tracked paths only) and hash them, capturing the
/// current on-disk state including unstaged modifications. Deleted files are omitted.
fn working_items(
    repo: &gix::Repository,
    workdir: &Path,
    index: &gix::worktree::Index,
) -> napi::Result<Vec<TreeItem>> {
    let mut out = Vec::new();
    for e in index.entries() {
        let path = e.path(index);
        let fs_path = workdir.join(std::ffi::OsStr::from_bytes(path));
        if !fs_path.is_file() {
            continue;
        }
        let data = std::fs::read(&fs_path).map_err(|e| err("read", e))?;
        let id = repo.write_blob(&data).map_err(|e| err("write_blob", e))?;
        let mode = if is_executable(&fs_path) {
            EntryMode::from(EntryKind::BlobExecutable)
        } else {
            EntryMode::from(EntryKind::Blob)
        };
        out.push((path_components(path), id.detach(), mode));
    }
    Ok(out)
}

/// Collect untracked files (not present in the index) via a status walk.
fn untracked_items(repo: &gix::Repository, workdir: &Path) -> napi::Result<Vec<TreeItem>> {
    let platform = repo
        .status::<gix::progress::Discard>(gix::progress::Discard)
        .map_err(|e| err("status", e))?;
    let mut iter = platform
        .into_index_worktree_iter(std::iter::empty::<gix::bstr::BString>())
        .map_err(|e| err("status_iter", e))?;
    let mut out = Vec::new();
    while let Some(item) = iter.next() {
        let item = item.map_err(|e| err("status_item", e))?;
        let (rela_path, untracked) = match &item {
            gix::status::index_worktree::Item::DirectoryContents { entry, .. } => (
                &entry.rela_path,
                matches!(entry.status, DirStatus::Untracked),
            ),
            _ => continue,
        };
        if !untracked {
            continue;
        }
        let fs_path = workdir.join(std::ffi::OsStr::from_bytes(rela_path));
        if !fs_path.is_file() {
            continue;
        }
        let data = std::fs::read(&fs_path).map_err(|e| err("read", e))?;
        let id = repo.write_blob(&data).map_err(|e| err("write_blob", e))?;
        let mode = if is_executable(&fs_path) {
            EntryMode::from(EntryKind::BlobExecutable)
        } else {
            EntryMode::from(EntryKind::Blob)
        };
        out.push((path_components(BStr::new(rela_path)), id.detach(), mode));
    }
    Ok(out)
}

/// Build a git tree object from a flat list of (path components, blob id, mode),
/// writing intermediate subtrees as needed. Entries are sorted tree-aware so that
/// directories sort before files at the same level, matching git's tree ordering.
fn write_tree(repo: &gix::Repository, items: &[TreeItem]) -> napi::Result<gix::ObjectId> {
    let mut nodes: Vec<(BString, gix::ObjectId, EntryMode, bool)> = Vec::new();
    let mut subdirs: std::collections::BTreeMap<String, Vec<TreeItem>> =
        std::collections::BTreeMap::new();

    for (path, oid, mode) in items {
        if path.len() == 1 {
            nodes.push((BString::from(path[0].clone()), *oid, *mode, false));
        } else {
            subdirs
                .entry(path[0].clone())
                .or_default()
                .push((path[1..].to_vec(), *oid, *mode));
        }
    }

    for (name, children) in subdirs {
        let sub_oid = write_tree(repo, &children)?;
        nodes.push((
            BString::from(name),
            sub_oid,
            EntryMode::from(EntryKind::Tree),
            true,
        ));
    }

    nodes.sort_by(|a, b| gix::objs::tree::name_order(&a.0[..], a.3, &b.0[..], b.3));

    let mut bytes = Vec::new();
    for (name, oid, mode, _) in &nodes {
        bytes.extend_from_slice(&mode.kind().as_octal_str()[..]);
        bytes.push(b' ');
        bytes.extend_from_slice(&name[..]);
        bytes.push(0);
        bytes.extend_from_slice(oid.as_bytes());
    }

    let id = repo
        .write_object(RawTree(bytes))
        .map_err(|e| err("write_object", e))?;
    Ok(id.detach())
}

/// Read the stash commit id at `index` (0 = most recent) from the `refs/stash` reflog.
fn stash_commit_id_at(repo: &gix::Repository, index: u32) -> napi::Result<gix::ObjectId> {
    let log_path = repo.git_dir().join("logs").join("refs").join("stash");
    if !log_path.exists() {
        return Err(err_str("apply_stash", format!("no stash at index {index}")).into());
    }
    let content = std::fs::read_to_string(&log_path).map_err(|e| err("read", e))?;
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    let idx = index as usize;
    if idx >= lines.len() {
        return Err(err_str("apply_stash", format!("no stash at index {index}")).into());
    }
    // Reflog file order is oldest-first, so index 0 (newest) is the last line.
    let pos = lines.len() - 1 - idx;
    let oid_hex = lines[pos]
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| err_str("apply_stash", "corrupt refs/stash reflog"))?;
    gix::ObjectId::from_hex(oid_hex.as_bytes()).map_err(|e| err("parse_oid", e))
}

/// Collect (path, blob id, mode) for every blob in `tree_id`, recursing into subtrees.
fn tree_entries(repo: &gix::Repository, tree_id: gix::ObjectId) -> napi::Result<Vec<TreeItem>> {
    let mut out = Vec::new();
    collect_tree(repo, tree_id, &mut Vec::new(), &mut out)?;
    Ok(out)
}

fn collect_tree(
    repo: &gix::Repository,
    tree_id: gix::ObjectId,
    prefix: &mut Vec<String>,
    out: &mut Vec<TreeItem>,
) -> napi::Result<()> {
    let tree = repo
        .find_object(tree_id)
        .map_err(|e| err("find_tree", e))?
        .try_into_tree()
        .map_err(|_| err_str("find_tree", "not a tree"))?;
    for entry in tree.iter() {
        let entry = entry.map_err(|e| err("tree_entry", e))?;
        let name = String::from_utf8_lossy(entry.filename()).into_owned();
        prefix.push(name);
        match entry.mode().kind() {
            EntryKind::Tree => collect_tree(repo, entry.oid().to_owned(), prefix, out)?,
            EntryKind::Blob | EntryKind::BlobExecutable => {
                out.push((prefix.clone(), entry.oid().to_owned(), entry.mode()));
            }
            _ => {}
        }
        prefix.pop();
    }
    Ok(())
}

/// Read a blob's contents from the object database.
fn read_blob(repo: &gix::Repository, oid: gix::ObjectId) -> napi::Result<Vec<u8>> {
    let blob = repo
        .find_object(oid)
        .map_err(|e| err("find_blob", e))?
        .try_into_blob()
        .map_err(|_| err_str("read_blob", "not a blob"))?;
    Ok(blob.data.clone())
}

/// Read the on-disk bytes of `path` (relative to `workdir`) if present.
fn worktree_file_bytes(workdir: &Path, path: &[String]) -> Option<Vec<u8>> {
    let mut p = workdir.to_path_buf();
    for c in path {
        p.push(c);
    }
    std::fs::read(&p).ok()
}

/// Write every blob in `tree_id` into `workdir`, creating parent directories and
/// preserving the executable bit.
fn restore_tree_to_worktree(
    repo: &gix::Repository,
    workdir: &Path,
    tree_id: gix::ObjectId,
) -> napi::Result<()> {
    for (path, oid, mode) in tree_entries(repo, tree_id)? {
        let mut fs_path = workdir.to_path_buf();
        for c in &path {
            fs_path.push(c);
        }
        if let Some(parent) = fs_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("mkdir", e))?;
        }
        let data = read_blob(repo, oid)?;
        std::fs::write(&fs_path, &data).map_err(|e| err("write_file", e))?;
        if mode.kind() == EntryKind::BlobExecutable {
            make_executable(&fs_path)?;
        }
    }
    Ok(())
}

/// Set the user-executable bit on `path`.
fn make_executable(path: &Path) -> napi::Result<()> {
    let mut perms = std::fs::metadata(path)
        .map_err(|e| err("metadata", e))?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms).map_err(|e| err("chmod", e))?;
    Ok(())
}

/// Rebuild the on-disk index from `tree_id` (the stash's index parent, stash^2) and write it.
fn restore_index_from_tree(repo: &gix::Repository, tree_id: gix::ObjectId) -> napi::Result<()> {
    let state = gix::index::State::from_tree(&tree_id, &repo, Default::default())
        .map_err(|e| err("index_from_tree", e))?;
    let mut file = gix::index::File::from_state(state, repo.git_dir().join("index"));
    file.write(Default::default())
        .map_err(|e| err("index_write", e))?;
    Ok(())
}
