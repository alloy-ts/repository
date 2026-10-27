use std::path::PathBuf;
use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};
use napi_derive::napi;

use gix::bstr::ByteSlice;

use crate::error::{err, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::repository::Repository;

/// A single reflog entry.
#[napi(object)]
pub struct ReflogEntry {
    /// The previous object id of the reference; the null hash when the ref was created.
    pub previous_oid: String,
    /// The new object id of the reference; the null hash when the ref was deleted.
    pub new_oid: String,
    /// The full SHA the reference now points at (equals `newOid`). Mirrors the
    /// `hash` field of git_spawn's `ReflogEntry`.
    pub hash: String,
    /// Abbreviated SHA (first 7 hex digits of `hash`).
    pub abbreviated_hash: String,
    /// Reflog selector, e.g. `HEAD@{0}` (newest) or `main@{3}`. The index counts
    /// down from the newest entry. Mirrors git_spawn's `ReflogEntry.selector`.
    pub selector: String,
    /// The identity that made the change.
    pub signature: Signature,
    /// The full reflog subject as written by git (e.g. `commit: add foo`).
    pub subject: String,
    /// The action portion of the subject, before the first `": "` (e.g. `commit`,
    /// `checkout`, `reset`). Mirrors git_spawn's `ReflogEntry.action`.
    pub action: String,
    /// The remainder of the subject after the first `": "` (empty when the subject
    /// has no `": "`). Mirrors git_spawn's `ReflogEntry.message`.
    pub message: String,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Read every reflog entry of `name` from `repo`, oldest first (or most recent
/// first when `reverse` is set), reusing gix's file-backed log iterator.
pub(crate) fn read_reflog(
    repo: &Arc<gix::ThreadSafeRepository>,
    name: &str,
    reverse: Option<bool>,
) -> napi::Result<Vec<ReflogEntry>> {
    fn push_entry(out: &mut Vec<ReflogEntry>, line: gix::refs::log::Line) {
        out.push(ReflogEntry {
            previous_oid: line.previous_oid.to_string(),
            new_oid: line.new_oid.to_string(),
            hash: String::new(),
            abbreviated_hash: String::new(),
            selector: String::new(),
            signature: to_signature(
                &line.signature.name,
                &line.signature.email,
                line.signature.time.seconds,
                line.signature.time.offset,
            ),
            subject: lossy(&line.message),
            action: String::new(),
            message: String::new(),
        });
    }
    let r = thread_local(repo);
    // Iterate through the store directly so that refs without a reflog (or refs
    // that no longer exist, e.g. after a log-only deletion) yield empty results.
    let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("ref_name", e))?;
    let mut platform = gix::refs::file::log::iter::Platform {
        store: &r.refs,
        name: full_name.as_ref(),
        buf: Vec::new(),
    };
    let mut out = Vec::new();
    if reverse.unwrap_or(false) {
        if let Some(iter) = platform.rev().map_err(|e| err("reflog.rev", e))? {
            for line in iter {
                push_entry(&mut out, line.map_err(|e| err("reflog line", e))?.into());
            }
        }
    } else if let Some(iter) = platform.all().map_err(|e| err("reflog.all", e))? {
        for line in iter {
            push_entry(&mut out, line.map_err(|e| err("reflog line", e))?.into());
        }
    }
    // Derive the git_spawn-style fields: `selector` (refname@{i}), an
    // abbreviated hash, and the `action`/`message` split of the subject. `i` is
    // the distance from the newest entry (git reflog show's `@{0}` is the newest).
    let n = out.len();
    for (i, e) in out.iter_mut().enumerate() {
        let rev = if reverse.unwrap_or(false) {
            i
        } else {
            n.saturating_sub(i + 1)
        };
        e.hash = e.new_oid.clone();
        e.abbreviated_hash = e.hash.chars().take(7).collect();
        e.selector = format!("{name}@{{{rev}}}");
        let (action, message) = match e.subject.split_once(": ") {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (e.subject.clone(), String::new()),
        };
        e.action = action;
        e.message = message;
    }
    Ok(out)
}

/// The on-disk path of the reflog file for `name` (mirrors `<common_dir>/logs/<name>`).
fn reflog_file_path(r: &gix::Repository, name: &str) -> napi::Result<PathBuf> {
    let full_name = gix::refs::FullName::try_from(name).map_err(|e| err("ref_name", e))?;
    let base = r
        .refs
        .common_dir()
        .unwrap_or_else(|| r.refs.git_dir())
        .to_path_buf();
    Ok(base.join("logs").join(full_name.to_string()))
}

/// A mutable handle to a single reference's reflog.
///
/// Unlike git2's in-memory `git_reflog`, this wraps the live repository and
/// persists changes immediately (gix has no deferred reflog buffer). Read the
/// entries with [`entries`](Self::entries)/[`get`](Self::get); append with
/// [`append`](Self::append).
#[napi]
pub struct Reflog {
    repo: Arc<gix::ThreadSafeRepository>,
    name: String,
}

#[napi]
impl Reflog {
    /// The reference name this reflog belongs to.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// All entries of this reflog, oldest first (or most recent first when
    /// `reverse` is set). Empty when the ref has no reflog.
    #[napi]
    pub fn entries(&self, reverse: Option<bool>) -> napi::Result<Vec<ReflogEntry>> {
        read_reflog(&self.repo, &self.name, reverse)
    }

    /// The entry at `index`, where `index` `0` is the most recent entry (git2's
    /// `reflog_get` semantics). Returns `null` when `index` is out of range.
    #[napi]
    pub fn get(&self, index: u32) -> napi::Result<Option<ReflogEntry>> {
        Ok(self.entries(Some(true))?.into_iter().nth(index as usize))
    }

    /// The number of entries in this reflog.
    #[napi]
    pub fn len(&self) -> napi::Result<u32> {
        Ok(self.entries(None)?.len() as u32)
    }

    /// Whether this reflog has no entries.
    #[napi]
    pub fn is_empty(&self) -> napi::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Append a new entry to this reflog, writing it to disk immediately.
    ///
    /// `newOid` is the object id the reference now points at; the previous oid is
    /// taken from the reference's current tip (the null hash when the reflog is
    /// empty). `committer` supplies the identity and timestamp for the entry.
    /// The reflog is created if it does not yet exist.
    #[napi]
    pub fn append(
        &self,
        new_oid: String,
        committer: Signature,
        message: Option<String>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let oid = parse_oid(&new_oid)?;
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        let committer = signature_from_dto(&committer)?;
        let time_str = committer.time.to_string();
        let committer_ref = gix::actor::SignatureRef {
            name: committer.name.as_bstr(),
            email: committer.email.as_bstr(),
            time: time_str.as_str(),
        };
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            full_name,
            gix::refs::Target::Object(oid),
            gix::refs::transaction::PreviousValue::Any,
            gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::Only,
                force_create_reflog: true,
                message: message.unwrap_or_default().into(),
            },
        );
        r.edit_references_as(vec![edit], Some(committer_ref))
            .map_err(|e| err("append_reflog", e))?;
        Ok(())
    }
}

#[napi]
impl Repository {
    /// Whether a reflog exists for the given reference name.
    #[napi]
    pub fn reflog_exists(&self, name: String) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        r.refs
            .reflog_exists(name.as_str())
            .map_err(|e| err("reflog_exists", e))
    }

    /// All reflog entries of `name`, oldest first (or most recent first when
    /// `reverse` is set). Empty when the ref has no reflog; a missing ref also
    /// yields an empty list instead of an error. When `name` is omitted it
    /// defaults to `HEAD`, matching `git reflog show`.
    #[napi]
    pub fn reflog(
        &self,
        name: Option<String>,
        reverse: Option<bool>,
    ) -> napi::Result<Vec<ReflogEntry>> {
        let name = name.unwrap_or_else(|| "HEAD".to_string());
        read_reflog(&self.inner, &name, reverse)
    }

    /// Async variant of [`Repository::reflog`].
    #[napi(ts_return_type = "Promise<ReflogEntry[]>")]
    pub fn reflog_async(
        &self,
        name: Option<String>,
        reverse: Option<bool>,
    ) -> napi::Result<AsyncTask<ReflogTask>> {
        let name = name.unwrap_or_else(|| "HEAD".to_string());
        Ok(AsyncTask::new(ReflogTask {
            repo: Arc::clone(&self.inner),
            name,
            reverse,
        }))
    }

    /// Async variant of [`Repository::reflog_exists`].
    #[napi(ts_return_type = "Promise<boolean>")]
    pub fn reflog_exists_async(&self, name: String) -> napi::Result<AsyncTask<ReflogExistsTask>> {
        Ok(AsyncTask::new(ReflogExistsTask {
            repo: Arc::clone(&self.inner),
            name,
        }))
    }

    /// Open a mutable handle to the reflog of `name`. The reflog need not exist
    /// yet; [`append`](Reflog::append) will create it.
    #[napi]
    pub fn open_reflog(&self, name: String) -> Reflog {
        Reflog {
            repo: Arc::clone(&self.inner),
            name,
        }
    }

    /// Best-effort removal of the reflog file for `name`, bypassing gix's
    /// transaction layer (git2's `git_reflog_delete`). Leaves the reference
    /// itself in place. Not safe against concurrent reflog writes.
    #[napi]
    pub fn delete_reflog(&self, name: String) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let path = reflog_file_path(&r, &name)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| err("delete_reflog", e))?;
        }
        Ok(())
    }

    /// Best-effort rename of the reflog file from `oldName` to `newName`,
    /// bypassing gix's transaction layer (git2's `git_reflog_rename`). Does not
    /// move the reference itself. Not safe against concurrent reflog writes.
    #[napi]
    pub fn rename_reflog(&self, old_name: String, new_name: String) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let old_path = reflog_file_path(&r, &old_name)?;
        let new_path = reflog_file_path(&r, &new_name)?;
        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("rename_reflog", e))?;
        }
        std::fs::rename(&old_path, &new_path).map_err(|e| err("rename_reflog", e))?;
        Ok(())
    }

    /// Async variant of [`Repository::delete_reflog`].
    #[napi(ts_return_type = "Promise<void>")]
    pub fn delete_reflog_async(&self, name: String) -> napi::Result<AsyncTask<DeleteReflogTask>> {
        Ok(AsyncTask::new(DeleteReflogTask {
            repo: Arc::clone(&self.inner),
            name,
        }))
    }

    /// Async variant of [`Repository::rename_reflog`].
    #[napi(ts_return_type = "Promise<void>")]
    pub fn rename_reflog_async(
        &self,
        old_name: String,
        new_name: String,
    ) -> napi::Result<AsyncTask<RenameReflogTask>> {
        Ok(AsyncTask::new(RenameReflogTask {
            repo: Arc::clone(&self.inner),
            old_name,
            new_name,
        }))
    }
}

pub struct ReflogTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
    pub(crate) reverse: Option<bool>,
}
impl Task for ReflogTask {
    type Output = Vec<ReflogEntry>;
    type JsValue = Vec<ReflogEntry>;
    fn compute(&mut self) -> napi::Result<Vec<ReflogEntry>> {
        read_reflog(&self.repo, &self.name, self.reverse).map_err(Into::into)
    }
    fn resolve(&mut self, _env: Env, output: Vec<ReflogEntry>) -> napi::Result<Vec<ReflogEntry>> {
        Ok(output)
    }
}

pub struct ReflogExistsTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
}
impl Task for ReflogExistsTask {
    type Output = bool;
    type JsValue = bool;
    fn compute(&mut self) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        r.refs
            .reflog_exists(self.name.as_str())
            .map_err(|e| err("reflog_exists", e))
            .map_err(Into::into)
    }
    fn resolve(&mut self, _env: Env, output: bool) -> napi::Result<bool> {
        Ok(output)
    }
}

pub struct DeleteReflogTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
}
impl Task for DeleteReflogTask {
    type Output = ();
    type JsValue = ();
    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let path = reflog_file_path(&r, &self.name)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| err("delete_reflog", e))?;
        }
        Ok(())
    }
    fn resolve(&mut self, _env: Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}

pub struct RenameReflogTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) old_name: String,
    pub(crate) new_name: String,
}
impl Task for RenameReflogTask {
    type Output = ();
    type JsValue = ();
    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let old_path = reflog_file_path(&r, &self.old_name)?;
        let new_path = reflog_file_path(&r, &self.new_name)?;
        if let Some(parent) = new_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("rename_reflog", e))?;
        }
        std::fs::rename(&old_path, &new_path).map_err(|e| err("rename_reflog", e))?;
        Ok(())
    }
    fn resolve(&mut self, _env: Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}
