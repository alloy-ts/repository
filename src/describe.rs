use std::sync::Arc;

use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::Object;
use crate::repository::Repository;

/// Options controlling [`Repository::describe`].
#[napi(object)]
pub struct DescribeOptions {
    /// Which references contribute candidate names:
    /// `annotatedTags` (default, like `git describe`), `allTags` or `allRefs`.
    pub names: Option<String>,
    /// Traverse only the first parent of merge commits (faster, less accurate).
    pub first_parent: Option<bool>,
    /// Maximum number of candidate names to track (default 10, max 32).
    pub max_candidates: Option<u32>,
    /// Always produce a result even when no name is found (falls back to the id).
    pub id_as_fallback: Option<bool>,
    /// Append this suffix to the output when the working tree is dirty
    /// (git2's `dirty_suffix` / `git describe --dirty`). gix only appends it when
    /// it detects a dirty worktree, matching `git describe`; it is ignored when
    /// clean. The `pattern` / custom abbreviation / forced-long-format options
    /// from git2 are not expressible through gix and are intentionally omitted.
    pub dirty_suffix: Option<String>,
}

/// The result of a `git describe`-style operation.
#[napi(object)]
pub struct DescribeOutcome {
    /// The name of the closest tag or branch, or `null` if none was found
    /// (only possible with `idAsFallback`).
    pub name: Option<String>,
    /// The described commit id.
    pub id: String,
    /// Number of commits between the named reference and the commit.
    pub depth: u32,
    /// `true` if the commit lies directly on the named reference.
    pub exact_match: bool,
    /// The typical `git describe` rendering (`v1.0.0`, `v1.0.0-3-gabcdef0` or
    /// the abbreviated hash as fallback).
    pub format: String,
    /// Number of commits traversed during the operation.
    pub commits_seen: u32,
}

#[napi]
impl Repository {
    /// Describe a commit the way `git describe` does: find the closest tag or
    /// branch that contains it and count the commits in between.
    ///
    /// Returns `null` when no candidate name exists and `idAsFallback` is off.
    #[napi]
    pub fn describe(
        &self,
        commit: String,
        options: Option<DescribeOptions>,
    ) -> napi::Result<Option<DescribeOutcome>> {
        let what = "describe";
        let r = thread_local(&self.inner);
        let oid = parse_oid(&commit)?;
        let commit_object = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let commit = commit_object.try_into_commit().map_err(|e| err(what, e))?;
        let mut platform = commit.describe();
        if let Some(o) = &options {
            if let Some(names) = o.names.as_deref() {
                let select = match names {
                    "annotatedTags" => gix::commit::describe::SelectRef::AnnotatedTags,
                    "allTags" => gix::commit::describe::SelectRef::AllTags,
                    "allRefs" => gix::commit::describe::SelectRef::AllRefs,
                    other => {
                        return Err(err_str(
                            what,
                            format!(
                                "invalid names {other:?} (expected \"annotatedTags\", \"allTags\" or \"allRefs\")"
                            ),
                        ));
                    }
                };
                platform = platform.names(select);
            }
            if let Some(first_parent) = o.first_parent {
                platform = platform.traverse_first_parent(first_parent);
            }
            if let Some(max_candidates) = o.max_candidates {
                platform = platform.max_candidates(max_candidates as usize);
            }
            if let Some(id_as_fallback) = o.id_as_fallback {
                platform = platform.id_as_fallback(id_as_fallback);
            }
        }
        let resolution = match platform.try_resolve().map_err(|e| err(what, e))? {
            Some(resolution) => resolution,
            None => return Ok(None),
        };
        let name = resolution
            .outcome
            .name
            .as_ref()
            .map(|name| name.to_string());
        let depth = resolution.outcome.depth;
        let id = resolution.outcome.id.to_string();
        let commits_seen = resolution.outcome.commits_seen;
        let dirty_suffix = options.as_ref().and_then(|o| o.dirty_suffix.clone());
        let format = if let Some(suffix) = dirty_suffix {
            resolution
                .format_with_dirty_suffix(suffix)
                .map_err(|e| err(what, e))?
        } else {
            resolution.format().map_err(|e| err(what, e))?
        };
        Ok(Some(DescribeOutcome {
            name,
            id,
            depth,
            exact_match: format.is_exact_match(),
            format: format.to_string(),
            commits_seen,
        }))
    }
}

#[napi]
impl Object {
    /// Describe this object the way `git describe` does: find the closest tag or
    /// branch that contains it and count the commits in between. Returns `null`
    /// when no candidate name exists and `idAsFallback` is off.
    ///
    /// This is [`Repository::describe`] with this object's id supplied; throws
    /// when the object is not a commit.
    #[napi]
    pub fn describe(
        &self,
        options: Option<DescribeOptions>,
    ) -> napi::Result<Option<DescribeOutcome>> {
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };
        repo.describe(self.id.to_string(), options)
    }
}
