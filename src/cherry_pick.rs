use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, to_signature};
use crate::merge::{
    MergeConflict, MergeOptions, build_tree_options, conflict_to_dto, owned_labels,
};
use crate::repository::Repository;

/// Options for [`Repository::cherry_pick`].
#[napi(object)]
#[derive(Default)]
pub struct CherryPickOptions {
    /// Custom commit message; used verbatim, bypassing the default git-style transformation.
    /// When omitted, the picked commit's message is used with `Change-Id:` trailers stripped and a
    /// `(cherry picked from commit <sha>)` reference appended.
    pub message: Option<String>,
    /// For merge commits, the 1-based parent index treated as the "mainline" (the
    /// side we are on). Required when cherry-picking a commit with more than one parent.
    pub mainline: Option<u32>,
    /// Merge strategy options (conflict labels, favors, rename tracking, …).
    pub merge_options: Option<MergeOptions>,
    /// If `true`, apply the merged tree to the index but do not create a commit; the
    /// conflicts (if any) are returned for resolution. Default `false`.
    pub no_commit: Option<bool>,
    /// Append a `(cherry picked from commit <sha>)` trailer to the default message.
    /// Only affects the auto-generated message (a `message` override is always verbatim).
    /// Defaults to `true`, matching git/git2's default cherry-pick behavior.
    pub record_origin: Option<bool>,
    /// Author identity for the new commit. Defaults to the original commit's author (git behavior).
    pub author: Option<Signature>,
    /// Committer identity for the new commit. Defaults to the repository's configured committer.
    pub committer: Option<Signature>,
    /// If `false` (default), a pick that changes nothing (the merged tree equals `HEAD`'s tree)
    /// stops without creating a commit and reports `empty: true`. If `true`, the empty commit is
    /// still created.
    pub allow_empty: Option<bool>,
}

/// Result of [`Repository::cherry_pick`].
#[napi(object)]
pub struct CherryPickResult {
    /// New commit id when a commit was created (`null` when `noCommit` was set, when a conflict
    /// prevented the commit, or when the pick was empty and `allowEmpty` is `false`).
    pub commit_id: Option<String>,
    /// Whether the merge produced conflicts.
    pub conflicted: bool,
    /// The merged tree id (always present; useful for manual resolution).
    pub tree_id: String,
    /// Per-path conflicts (populated when `conflicted`).
    pub conflicts: Vec<MergeConflict>,
    /// Conflict paths, derived from `conflicts` (populated when `conflicted`).
    pub paths: Vec<String>,
    /// `true` when the picked change was a no-op (the merged tree equals `HEAD`'s tree). The
    /// `commitId` is `null` unless `allowEmpty` was set.
    pub empty: bool,
}

#[napi]
impl Repository {
    /// Apply the changes introduced by commit `sha` onto the current `HEAD` (a cherry-pick).
    ///
    /// Performs a 3-way merge with the commit's parent as the ancestor and the commit as "theirs",
    /// then — unless `noCommit` is set — commits the result onto the current branch (or moves a
    /// detached `HEAD`). On conflict the commit is skipped and the conflict list is returned for
    /// resolution rather than throwing. When no `message` is supplied, the picked commit's message
    /// is used with `Change-Id:` trailers removed and a `(cherry picked from commit <sha>)` reference
    /// appended (a `message` override bypasses this transformation). The author defaults to the
    /// original commit's author and the committer to the repository's configured identity; both can
    /// be overridden via `author` / `committer`.
    #[napi]
    pub fn cherry_pick(
        &self,
        sha: String,
        options: Option<CherryPickOptions>,
    ) -> napi::Result<CherryPickResult> {
        let options = options.unwrap_or_default();
        let r = thread_local(&self.inner);
        let ours_id = r
            .head_commit()
            .map_err(|e| err("head_commit", e))?
            .id
            .to_string();
        let (tree_id, conflicted, conflicts) = compute_cherry_pick(
            &r,
            &ours_id,
            &sha,
            options.mainline,
            options.merge_options.as_ref(),
        )?;
        let tree_str = tree_id.to_string();
        let paths: Vec<String> = conflicts.iter().map(|c| c.ours.location.clone()).collect();

        // noCommit: apply the merged tree to the index and return, leaving resolution to JS.
        if options.no_commit.unwrap_or(false) {
            let mut index = r
                .index_from_tree(&tree_id)
                .map_err(|e| err("index_from_tree", e))?;
            index
                .write(gix::index::write::Options::default())
                .map_err(|e| err("index_write", e))?;
            return Ok(CherryPickResult {
                commit_id: None,
                conflicted,
                tree_id: tree_str,
                conflicts,
                paths,
                empty: false,
            });
        }

        // Conflict: skip the commit and return the conflicts for resolution.
        if conflicted {
            return Ok(CherryPickResult {
                commit_id: None,
                conflicted,
                tree_id: tree_str,
                conflicts,
                paths,
                empty: false,
            });
        }

        // Detect a no-op pick (the merged tree equals HEAD's tree).
        let head_tree = r
            .head_commit()
            .map_err(|e| err("head_commit", e))?
            .tree()
            .map_err(|e| err("commit_tree", e))?
            .id;
        let is_empty = tree_id == head_tree;
        if is_empty && !options.allow_empty.unwrap_or(false) {
            return Ok(CherryPickResult {
                commit_id: None,
                conflicted: false,
                tree_id: tree_str,
                conflicts: vec![],
                paths: vec![],
                empty: true,
            });
        }

        // Build the message: verbatim override, else git-style transform.
        let pick_oid = parse_oid(&sha)?;
        let pick = r.find_commit(pick_oid).map_err(|e| err("find_commit", e))?;
        let full_sha = pick_oid.to_string();
        let message = match options.message {
            Some(m) => m,
            None => {
                let message_ref = pick.message().map_err(|e| err("message", e))?;
                let title = String::from_utf8_lossy(message_ref.title).into_owned();
                let full_msg = match message_ref.body {
                    Some(body) => {
                        format!("{title}\n\n{}", String::from_utf8_lossy(body).into_owned())
                    }
                    None => title,
                };
                format_cherry_pick_message(
                    &full_msg,
                    &full_sha,
                    options.record_origin.unwrap_or(true),
                )
            }
        };

        // Author: explicit override, else the original commit's author (git behavior).
        let author_ref = pick.author().map_err(|e| err("author", e))?;
        let author_time = author_ref.time().map_err(|e| err("author.time", e))?;
        let author = options.author.clone().unwrap_or_else(|| {
            to_signature(
                &author_ref.name[..],
                &author_ref.email[..],
                author_time.seconds,
                author_time.offset,
            )
        });

        // Committer: explicit override, else the repository's configured committer.
        let committer = match options.committer.clone() {
            Some(c) => c,
            None => match r.committer().and_then(|res| res.ok()) {
                Some(cs) => {
                    let ct = cs.time().ok();
                    to_signature(
                        &cs.name[..],
                        &cs.email[..],
                        ct.map(|t| t.seconds).unwrap_or(0),
                        ct.map(|t| t.offset).unwrap_or(0),
                    )
                }
                None => {
                    return Err(err_str(
                        "cherry_pick",
                        "no committer identity configured; pass `committer` explicitly",
                    ));
                }
            },
        };

        let head = r.head().map_err(|e| err("head", e))?;
        let head_id = r.head_commit().map_err(|e| err("head_commit", e))?.id;
        let parent_str = head_id.to_string();
        let new_id = match head.referent_name() {
            Some(name) => self.commit_as(
                committer,
                author,
                name.to_string(),
                message.clone(),
                tree_str.clone(),
                vec![parent_str],
            )?,
            None => {
                let id = self.new_commit_as(
                    committer,
                    author,
                    message.clone(),
                    tree_str.clone(),
                    vec![parent_str],
                )?;
                self.set_head(id.clone())?;
                id
            }
        };

        Ok(CherryPickResult {
            commit_id: Some(new_id),
            conflicted: false,
            tree_id: tree_str,
            conflicts: vec![],
            paths: vec![],
            empty: is_empty,
        })
    }
}

/// Merge `pick_id`'s parent..`pick_id` change onto `ours_id` and return the resulting tree id,
/// whether the merge was conflicted, and the per-path conflicts.
fn compute_cherry_pick(
    r: &gix::Repository,
    ours_id: &str,
    pick_id: &str,
    mainline: Option<u32>,
    merge: Option<&MergeOptions>,
) -> napi::Result<(gix::ObjectId, bool, Vec<MergeConflict>)> {
    let ours_oid = parse_oid(ours_id)?;
    let pick_oid = parse_oid(pick_id)?;
    let pick_commit = r.find_commit(pick_oid).map_err(|e| err("find_commit", e))?;
    let parents: Vec<gix::ObjectId> = pick_commit.parent_ids().map(|p| p.detach()).collect();
    // Choose the merge ancestor. A merge commit has more than one parent, so the caller must
    // say which one is the "mainline" (the side we are on) via `mainline`.
    let parent_oid = if parents.is_empty() {
        return Err(err_str(
            "cherry_pick",
            "cannot cherry-pick a commit without parents",
        ));
    } else if parents.len() == 1 {
        parents[0]
    } else {
        let idx = mainline.ok_or_else(|| {
            err_str(
                "cherry_pick",
                "cherry-picking a merge commit requires the 'mainline' option (1-based parent index)",
            )
        })? as usize;
        if idx == 0 || idx > parents.len() {
            return Err(err_str(
                "cherry_pick",
                format!(
                    "mainline {} is out of range (commit has {} parents)",
                    idx,
                    parents.len()
                ),
            ));
        }
        parents[idx - 1]
    };
    let parent_commit = r
        .find_commit(parent_oid)
        .map_err(|e| err("find_commit", e))?;

    let ours_tree = r
        .find_commit(ours_oid)
        .map_err(|e| err("find_commit", e))?
        .tree()
        .map_err(|e| err("commit_tree", e))?
        .id;
    let ancestor_tree = parent_commit.tree().map_err(|e| err("commit_tree", e))?.id;
    let their_tree = pick_commit.tree().map_err(|e| err("commit_tree", e))?.id;

    let owned = owned_labels(merge);
    let tree_options = build_tree_options("cherry-pick", merge)?;
    let mut outcome = r
        .merge_trees(
            ancestor_tree,
            ours_tree,
            their_tree,
            owned.borrowed(),
            tree_options,
        )
        .map_err(|e| err("cherry_pick_merge", e))?;
    let how = gix::merge::tree::TreatAsUnresolved::git();
    let conflicts = outcome
        .conflicts
        .iter()
        .map(|c| conflict_to_dto(r, c, how))
        .collect();
    let tree_id = outcome
        .tree
        .write()
        .map_err(|e| err("cherry_pick_tree_write", e))?;
    let conflicted = !outcome.conflicts.is_empty();
    Ok((tree_id.into(), conflicted, conflicts))
}

/// Strip `Change-Id:` trailer lines, trim trailing blank lines, and (when `record_origin`) append a
/// `(cherry picked from commit <sha>)` reference — mirroring git/git2's default cherry-pick message.
/// `Change-Id:` stripping always happens; only the trailer is gated by `record_origin`.
fn format_cherry_pick_message(old_msg: &str, sha: &str, record_origin: bool) -> String {
    let mut lines: Vec<&str> = old_msg
        .lines()
        .filter(|line| !line.trim().starts_with("Change-Id:"))
        .collect();
    while let Some(last) = lines.last() {
        if last.trim().is_empty() {
            lines.pop();
        } else {
            break;
        }
    }
    let mut msg = lines.join("\n");
    if !msg.is_empty() {
        msg.push('\n');
    }
    if record_origin {
        msg.push_str(&format!("(cherry picked from commit {sha})"));
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_cherry_pick_message_strips_change_id() {
        let msg = "Hello world\n\nChange-Id: I123\n\nSigned-off-by: Foo\n";
        let result = format_cherry_pick_message(msg, "abc123", true);
        assert!(!result.contains("Change-Id:"));
        assert!(result.contains("Hello world"));
        assert!(result.contains("(cherry picked from commit abc123)"));
    }

    #[test]
    fn format_cherry_pick_message_no_change_id() {
        let msg = "Hello world\n\nSigned-off-by: Foo\n";
        let result = format_cherry_pick_message(msg, "abc123", true);
        assert!(result.contains("Hello world"));
        assert!(result.contains("(cherry picked from commit abc123)"));
    }

    #[test]
    fn format_cherry_pick_message_suppresses_trailer() {
        let msg = "Hello world\n\nSigned-off-by: Foo\n";
        let result = format_cherry_pick_message(msg, "abc123", false);
        assert!(!result.contains("(cherry picked from commit"));
        assert!(result.contains("Hello world"));
    }
}
