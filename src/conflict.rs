use std::collections::BTreeMap;
use std::sync::Arc;

use napi::Result;
use napi::bindgen_prelude::{AsyncTask, Buffer};
use napi::{Env, Task};
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::{IndexEntryInput, Repository};

/// How a path is conflicted, read from the index conflict stages (1 = ancestor,
/// 2 = ours, 3 = theirs). Mirrors git's seven unmerged states and the
/// `ConflictKind` from the `git_spawn` reference.
///
/// Unlike the reference (which derives this from `git status --porcelain` `XY`
/// codes), we read it straight from the index: a stage-1 entry means the path
/// existed in the common ancestor, while the presence of stages 2/3 reflects
/// which side touched it.
#[derive(Clone, Copy)]
#[napi(string_enum)]
pub enum ConflictKind {
    /// Both sides modified the path (stages 1+2+3 present).
    BothModified,
    /// Both sides added the path (stages 2+3, no stage 1).
    BothAdded,
    /// Both sides deleted the path (only stage 1 present).
    BothDeleted,
    /// We added the path; they left it unmerged (only stage 2).
    AddedByUs,
    /// They added the path; we left it unmerged (only stage 3).
    AddedByThem,
    /// We deleted the path; they modified it (stages 1+3).
    DeletedByUs,
    /// They deleted the path; we modified it (stages 1+2).
    DeletedByThem,
}

impl ConflictKind {
    /// The two-letter porcelain code for this conflict.
    pub fn code(&self) -> &'static str {
        match self {
            ConflictKind::BothDeleted => "DD",
            ConflictKind::AddedByUs => "AU",
            ConflictKind::DeletedByThem => "UD",
            ConflictKind::AddedByThem => "UA",
            ConflictKind::DeletedByUs => "DU",
            ConflictKind::BothAdded => "AA",
            ConflictKind::BothModified => "UU",
        }
    }
}

/// A single conflicted path in an in-progress merge, rebase, or cherry-pick
/// (anything that leaves the index with stage-1/2/3 entries).
#[napi(object)]
pub struct Conflict {
    /// The conflicted path, relative to the repository root.
    pub path: String,
    /// Which side(s) touched the path.
    pub kind: ConflictKind,
}

/// Options for [`Repository::resolve_conflict`].
#[napi(object)]
pub struct ResolveConflictOptions {
    /// Mode for the resolved stage-0 entry. Defaults to `"100644"`; pass
    /// `"100755"` to resolve an executable file. The mode is *not* inferred from
    /// the conflicted sides in this version.
    pub mode: Option<String>,
}

/// Classify a conflict from the presence of the ancestor (stage 1), ours
/// (stage 2), and theirs (stage 3) entries.
fn classify_conflict(ancestor: bool, our: bool, theirs: bool) -> ConflictKind {
    match (ancestor, our, theirs) {
        (true, true, true) => ConflictKind::BothModified,
        (false, true, true) => ConflictKind::BothAdded,
        (true, false, false) => ConflictKind::BothDeleted,
        (false, true, false) => ConflictKind::AddedByUs,
        (false, false, true) => ConflictKind::AddedByThem,
        (true, false, true) => ConflictKind::DeletedByUs,
        (true, true, false) => ConflictKind::DeletedByThem,
        // A path with no stage-1/2/3 entries is not a conflict; this is the
        // degenerate fallback for a path that only has a stage-0 entry.
        _ => ConflictKind::BothModified,
    }
}

/// Read the index and return the typed, path-keyed conflict list. Pure
/// index read — no subprocess, unlike the `git_spawn` reference which shells
/// to `git status --porcelain=v1 -z`.
fn collect_conflicts(file: &gix::index::File) -> Vec<Conflict> {
    let mut map: BTreeMap<String, (bool, bool, bool)> = BTreeMap::new();
    for e in file.entries() {
        let stage = e.stage() as u32;
        if stage == 0 {
            continue;
        }
        let path = String::from_utf8_lossy(&**e.path(file)).into_owned();
        let slot = map.entry(path).or_insert((false, false, false));
        match stage {
            1 => slot.0 = true,
            2 => slot.1 = true,
            3 => slot.2 = true,
            _ => {}
        }
    }
    map.into_iter()
        .map(|(path, (a, o, t))| Conflict {
            path,
            kind: classify_conflict(a, o, t),
        })
        .collect()
}

#[napi]
impl Repository {
    /// List the paths left conflicted by an in-progress merge, rebase, or
    /// cherry-pick, classified into typed [`Conflict`] entries (with
    /// [`ConflictKind`]). A clean index yields an empty list.
    ///
    /// This is the index-native equivalent of the `git_spawn` reference's
    /// `ConflictOps::list()` (which shells to `git status`).
    #[napi]
    pub fn conflicts(&self) -> napi::Result<Vec<Conflict>> {
        // Force a fresh read: a conflict may have just been written to the index
        // file by `apply_merge_conflicts_to_index` / `resolve_conflict`, and we
        // must not return a stale cached copy.
        let index = self.open_index(None)?;
        Ok(collect_conflicts(&index.inner))
    }

    /// Async variant of [`Repository::conflicts`].
    #[napi(ts_return_type = "Promise<Conflict[]>")]
    pub fn conflicts_async(&self) -> napi::Result<AsyncTask<ConflictsTask>> {
        Ok(AsyncTask::new(ConflictsTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Resolve the conflict at `path` by recording its current, resolved
    /// content as a single stage-0 index entry and dropping the conflict stages
    /// (1–3). Equivalent to `git add <path>` after you have edited the file to
    /// your resolved content, but without touching the worktree — you pass the
    /// resolved bytes directly.
    ///
    /// Removes every index entry for `path` (stages 0–3) before writing the new
    /// stage-0 entry, so any stale stage-0 left behind by a prior state is
    /// replaced too.
    #[napi]
    pub fn resolve_conflict(
        &self,
        path: String,
        content: Buffer,
        options: Option<ResolveConflictOptions>,
    ) -> napi::Result<()> {
        let mut index = self.open_index(None)?;
        let target = path.as_bytes();
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, e) in index.inner.entries().iter().enumerate() {
            if &**e.path(&index.inner) == target {
                to_remove.push(i);
            }
        }
        for i in to_remove.into_iter().rev() {
            index.inner.remove_entry_at_index(i);
        }
        let mode = options
            .and_then(|o| o.mode)
            .unwrap_or_else(|| "100644".to_string());
        self.add_from_buffer_to_index(
            &mut index,
            IndexEntryInput {
                path: path.clone(),
                id: String::new(),
                mode: Some(mode),
            },
            content,
        )?;
        index.write(None)?;
        Ok(())
    }

    /// Async variant of [`Repository::resolve_conflict`].
    #[napi(ts_return_type = "Promise<void>")]
    pub fn resolve_conflict_async(
        &self,
        path: String,
        content: Buffer,
        options: Option<ResolveConflictOptions>,
    ) -> napi::Result<AsyncTask<ResolveConflictTask>> {
        Ok(AsyncTask::new(ResolveConflictTask {
            repo: Arc::clone(&self.inner),
            path,
            content,
            options,
        }))
    }
}

pub struct ConflictsTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}
impl Task for ConflictsTask {
    type Output = Vec<Conflict>;
    type JsValue = Vec<Conflict>;
    fn compute(&mut self) -> napi::Result<Vec<Conflict>> {
        let r = thread_local(&self.repo);
        // Force a fresh read from disk (see `conflicts`).
        let file = gix::index::File::at(r.index_path(), r.object_hash(), false, Default::default())
            .map_err(|e| err("open_index", e))?;
        Ok(collect_conflicts(&file))
    }
    fn resolve(&mut self, _env: Env, output: Vec<Conflict>) -> napi::Result<Vec<Conflict>> {
        Ok(output)
    }
}

pub struct ResolveConflictTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) path: String,
    pub(crate) content: Buffer,
    pub(crate) options: Option<ResolveConflictOptions>,
}
impl Task for ResolveConflictTask {
    type Output = ();
    type JsValue = ();
    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let mut file =
            gix::index::File::at(r.index_path(), r.object_hash(), false, Default::default())
                .map_err(|e| err("open_index", e))?;
        let target = self.path.as_bytes();
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, e) in file.entries().iter().enumerate() {
            if &**e.path(&file) == target {
                to_remove.push(i);
            }
        }
        for i in to_remove.into_iter().rev() {
            file.remove_entry_at_index(i);
        }
        // Write the resolved content as a blob, then add it as a single stage-0
        // entry (mirrors `Index::add_from_buffer_to_index` at the gix level).
        let odb = r
            .objects
            .clone()
            .into_arc()
            .map_err(|e| err("objects", e))?;
        let oid =
            gix::objs::Write::write_buf(&*odb, gix::object::Kind::Blob, self.content.as_ref())
                .map_err(|e| err("write_blob", e))?;
        let mode = self
            .options
            .take()
            .and_then(|o| o.mode)
            .unwrap_or_else(|| "100644".to_string());
        let entry = IndexEntryInput {
            path: self.path.clone(),
            id: oid.to_string(),
            mode: Some(mode),
        };
        crate::index::push_index_entry(&mut file, &entry).map_err(|e| err("push_entry", e))?;
        file.write(gix::index::write::Options {
            skip_hash: false,
            ..Default::default()
        })
        .map_err(|e| err("write_index", e))?;
        Ok(())
    }
    fn resolve(&mut self, _env: Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}
