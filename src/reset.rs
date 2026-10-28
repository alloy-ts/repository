use napi_derive::napi;
use napi::bindgen_prelude::Either;

use crate::error::{err, err_str, thread_local};
use crate::repository::{Repository, find_git};

use gix;

/// Reset mode, mirroring `git reset --soft | --mixed | --hard`.
#[napi]
pub enum ResetMode {
    /// `--soft`: only HEAD (or its branch) moves; the index and work tree are kept.
    Soft,
    /// `--mixed` (Git's default): the index is set to the commit's tree; the work
    /// tree is kept, so the changes appear unstaged.
    Mixed,
    /// `--hard`: the index and tracked files are set to the commit; untracked
    /// files are kept (unlike the old `remove_dir_all` wipe, which destroyed them).
    Hard,
}

#[napi]
impl Repository {
    /// Move HEAD to `revision` (any rev-parse spec; tags/objects peeled to a
    /// commit) like `git reset --soft/--mixed/--hard <revision>`, and return the
    /// new HEAD SHA.
    ///
    /// `revision` is resolved to a commit, so `HEAD~1`, branch names and tags all
    /// work — unlike `reset`/`resetDefault`, which require a raw object id.
    /// `Soft` keeps the index and work tree; `Mixed` also resets the index to the
    /// commit's tree; `Hard` also resets tracked working-tree files while
    /// preserving untracked files.
    #[napi]
    pub fn reset_to(&self, revision: String, mode: ResetMode) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let (oid, tree_id) = resolve_commit(&r, &revision)?;

        // `git reset --hard` is the canonical, safe hard reset: it rewrites HEAD,
        // the index and tracked working-tree files while preserving untracked
        // files. Delegating here avoids the old `remove_dir_all` wipe, which
        // destroyed untracked files (and the `.git` directory). For Soft/Mixed we
        // use gix directly (HEAD move + index rewrite from the tree).
        if matches!(mode, ResetMode::Hard) {
            let git = find_git()
                .ok_or_else(|| err_str("reset_to", "no system git binary found"))?;
            let mut cmd = std::process::Command::new(&git);
            cmd.arg("reset").arg("--hard").arg(oid.to_string());
            if let Some(wd) = r.workdir() {
                cmd.current_dir(wd);
            }
            return match cmd.output() {
                Ok(out) if out.status.success() => Ok(oid.to_string()),
                Ok(out) => Err(err_str(
                    "reset_to",
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                )),
                Err(e) => Err(err("reset_to", e)),
            };
        }

        // Move HEAD first — this alone satisfies the Soft case.
        let label = format!("reset: moving to {revision}");
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("reset_head", e))?;

        // Mixed rewrites the index to the commit's tree.
        if matches!(mode, ResetMode::Mixed) {
            let mut file = r
                .index_from_tree(&tree_id)
                .map_err(|e| err("index_from_tree", e))?;
            file.write(Default::default())
                .map_err(|e| err("write_index", e))?;
        }

        Ok(oid.to_string())
    }

    /// Stage/unstage specific paths to a commit's content, like
    /// `git reset <revision> -- <paths>`: each path (a file, or a directory for
    /// everything under it) is staged as it is in the commit, or removed from the
    /// index when the commit does not have it. HEAD and the work tree are not
    /// changed. An empty `paths` or a `"."` entry means every path.
    #[napi]
    pub fn reset_paths(
        &self,
        revision: String,
        paths: Either<String, Vec<String>>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let (oid, _tree_id) = resolve_commit(&r, &revision)?;

        let git = find_git()
            .ok_or_else(|| err_str("reset_paths", "no system git binary found"))?;
        let specs: Vec<String> = match paths {
            Either::A(s) => vec![s],
            Either::B(v) => v,
        };
        let all = specs.is_empty() || specs.iter().any(|s| s == ".");

        let mut cmd = std::process::Command::new(&git);
        cmd.arg("reset").arg(oid.to_string()).arg("--");
        if all {
            cmd.arg(".");
        } else {
            for s in &specs {
                cmd.arg(s);
            }
        }
        if let Some(wd) = r.workdir() {
            cmd.current_dir(wd);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(err_str(
                "reset_paths",
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )),
            Err(e) => Err(err("reset_paths", e)),
        }
    }

    /// Reset HEAD, index, and optionally working tree to `target` (a raw object
    /// id). Kept for backward compatibility; prefer [`reset_to`](Self::reset_to),
    /// which accepts revisions and a mode and applies the safe hard-reset
    /// semantics.
    #[napi]
    pub fn reset(&self, target: String, hard: Option<bool>) -> napi::Result<()> {
        let mode = if hard.unwrap_or(false) {
            ResetMode::Hard
        } else {
            ResetMode::Mixed
        };
        self.reset_to(target, mode)?;
        Ok(())
    }

    /// Perform a mixed reset (HEAD + index, work tree untouched). Equivalent to
    /// `reset(target, false)`; kept for backward compatibility.
    #[napi]
    pub fn reset_default(&self, target: String) -> napi::Result<()> {
        self.reset_to(target, ResetMode::Mixed)?;
        Ok(())
    }

    /// Reset index and tracked working tree files to HEAD (hard reset).
    #[napi]
    pub fn reset_to_head(&self) -> napi::Result<String> {
        self.reset_to("HEAD".to_string(), ResetMode::Hard)
    }

    /// Hard reset tracked working tree files and index to HEAD.
    #[napi]
    pub fn reset_hard(&self) -> napi::Result<String> {
        self.reset_to_head()
    }
}

/// Resolve `spec` to a commit, returning `(commit id, tree id)`. The spec is
/// rev-parsed; annotated tags / other objects are peeled to a commit.
fn resolve_commit(
    r: &gix::Repository,
    spec: &str,
) -> napi::Result<(gix::ObjectId, gix::ObjectId)> {
    let id: gix::ObjectId = r
        .rev_parse_single(spec)
        .map_err(|e| err("rev_parse_single", e))?
        .into();
    let commit_id = match r.find_commit(id) {
        Ok(c) => c.id,
        Err(_) => {
            let obj = r.find_object(id).map_err(|e| err("find_object", e))?;
            obj.peel_to_kind(gix::object::Kind::Commit)
                .map_err(|_| err_str("reset", format!("{spec} does not point to a commit")))?
                .id
        }
    };
    let commit = r.find_commit(commit_id).map_err(|e| err("find_commit", e))?;
    let tree_id: gix::ObjectId = commit.tree_id().map_err(|e| err("tree_id", e))?.into();
    Ok((commit_id, tree_id))
}
