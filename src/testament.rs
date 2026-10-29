use std::collections::BTreeMap;

use napi_derive::napi;

use gix::bstr::BString;
use gix::diff::index::Change as TreeIndexChange;
use gix::status::index_worktree::Item as IndexWorktreeItem;
use gix::status::plumbing::index_as_worktree::Change as WorktreeChange;
use gix::status::plumbing::index_as_worktree::EntryStatus as IndexEntryStatus;
use gix::status::{Item as StatusItem, UntrackedFiles};

use crate::error::{err, err_str, thread_local};
use crate::repository::{Repository, lossy};

/// What a [`Testament`] could attest to.
#[napi(string_enum)]
pub enum TestamentCommitKind {
    /// No commit exists yet (`HEAD` is unborn, e.g. right after `git init`).
    Unborn,
    /// The commit is preceded by a tag.
    FromTag,
    /// No tag in the commit's history describes it.
    NoTags,
}

/// How a working tree entry differs from `HEAD`.
#[napi(string_enum)]
pub enum ModificationKind {
    /// Staged or intent-to-add.
    Added,
    /// Removed from the index or the working tree.
    Removed,
    /// Content or mode changed, or the entry is conflicted.
    Modified,
    /// Present on disk but not tracked.
    Untracked,
}

/// One "dirty" entry of the working tree, recorded when the testament was made.
#[napi(object)]
pub struct TestamentModification {
    /// How the entry differs from `HEAD`.
    pub kind: ModificationKind,
    /// Repository-relative path (lossy UTF-8), or the directory path when
    /// untracked entries are collapsed.
    pub path: String,
}

/// Options for [`Repository::testament`].
#[napi(object)]
pub struct TestamentOptions {
    /// Commit-ish to attest; any revision gix understands. Defaults to `HEAD`.
    pub commit: Option<String>,
    /// Which references may supply the tag name: `"annotatedTags"` (default,
    /// like `git describe`), `"allTags"` or `"allRefs"`.
    pub names: Option<String>,
    /// Walk the working tree and return every modification in `modifications`
    /// instead of just reporting `dirty`. Costs a tree/index/worktree walk
    /// (plus a directory walk when `untracked` is on), so leave it off unless
    /// you render the list. Default `false`.
    pub include_modifications: Option<bool>,
    /// Whether untracked files count as modifications. Default `true`. Has no
    /// effect on `dirty`, which never counts untracked files (matching
    /// [`Repository::is_dirty`](crate::status)).
    pub untracked: Option<bool>,
    /// A crate/package version to render. Used for the unborn-`HEAD` fallback
    /// and for the trusted-branch rendering below.
    pub version: Option<String>,
    /// Render a clean tip of this branch as though it carried `version` at the
    /// built commit (`git_testament`'s "trusted branch" escape hatch). Intended
    /// for release pipelines that only tag after artifacts have been built and
    /// tested: for any other input the tag and the version disagree loudly, by
    /// design.
    pub trusted_branch: Option<String>,
}

/// A testament to the state of a repository at a commit.
///
/// Returned by [`Repository::testament`]; `format` is the human-readable
/// rendering that mirrors `git_testament`'s `Display`
/// (`"1.0.0+14 (651af89ed 2019-04-02) dirty 4 modifications"`).
#[napi(object)]
pub struct Testament {
    /// What the commit half could resolve to.
    pub kind: TestamentCommitKind,
    /// Current branch name, or `null` when `HEAD` is detached or unborn.
    pub branch: Option<String>,
    /// Full hex id of the attested commit, or `null` when `HEAD` is unborn.
    pub commit: Option<String>,
    /// The same id abbreviated to 9 hex digits, or `null` when unborn.
    pub abbrev: Option<String>,
    /// Committer time, seconds since the Unix epoch. `0` when unborn.
    pub seconds: i64,
    /// Committer timezone offset, in seconds. `0` when unborn.
    pub offset: i32,
    /// Committer date as `YYYY-MM-DD` UTC, or `null` when unborn. Use
    /// `seconds`/`offset` for anything that needs the time of day or the
    /// original zone.
    pub date: Option<String>,
    /// Closest tag name, or `null` when none describes the commit.
    pub tag: Option<String>,
    /// Commits between `tag` and the commit, or `null` when there is no tag.
    pub distance: Option<u32>,
    /// Tag-derived version string: the tag, or `tag+distance` when the commit
    /// sits `distance` commits past it. `null` when there is no tag.
    pub version: Option<String>,
    /// `true` when the working tree differs from `HEAD` in tracked content.
    pub dirty: bool,
    /// Every modification, or `null` unless `includeModifications` was set.
    pub modifications: Option<Vec<TestamentModification>>,
    /// `true` when the trusted-branch rule was applied to `version`/`format`.
    pub trusted: bool,
    /// The whole testament rendered the way `git_testament` displays it.
    pub format: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Shorten a reference name to its short form (`refs/heads/main` → `main`).
fn shorten(name: &gix::refs::FullNameRef) -> String {
    String::from_utf8_lossy(name.shorten()).into_owned()
}

/// Map the `names` option onto gix's describe candidate selection.
fn select_ref(names: Option<&str>) -> napi::Result<gix::commit::describe::SelectRef> {
    Ok(match names {
        None | Some("annotatedTags") => gix::commit::describe::SelectRef::AnnotatedTags,
        Some("allTags") => gix::commit::describe::SelectRef::AllTags,
        Some("allRefs") => gix::commit::describe::SelectRef::AllRefs,
        Some(other) => {
            return Err(err_str(
                "testament options",
                format!(
                    "invalid names {other:?}, expected \"annotatedTags\", \"allTags\" or \"allRefs\""
                ),
            ));
        }
    })
}

/// Collect working tree modifications relative to `HEAD`.
///
/// Mirrors [`crate::status`]'s walk but keeps only the coarse kind per path,
/// merging the staged and unstaged sides the way `git_testament` does its
/// `GitModification` list. Keyed by path so the result is deterministic.
fn collect_modifications(
    r: &gix::Repository,
    untracked: bool,
) -> napi::Result<Vec<TestamentModification>> {
    let platform = r
        .status(gix::progress::Discard)
        .map_err(|e| err("status", e))?;
    let platform = platform.untracked_files(if untracked {
        UntrackedFiles::Collapsed
    } else {
        UntrackedFiles::None
    });
    let mut iter = platform
        .into_iter(Vec::new())
        .map_err(|e| err("status iter", e))?;
    let mut map: BTreeMap<BString, ModificationKind> = BTreeMap::new();
    for item in iter.by_ref() {
        let item = item.map_err(|e| err("status item", e))?;
        match item {
            StatusItem::TreeIndex(change) => match change {
                TreeIndexChange::Addition { location, .. } => {
                    map.insert(location.into_owned(), ModificationKind::Added);
                }
                TreeIndexChange::Deletion { location, .. } => {
                    map.insert(location.into_owned(), ModificationKind::Removed);
                }
                TreeIndexChange::Modification { location, .. }
                | TreeIndexChange::Rewrite { location, .. } => {
                    map.insert(location.into_owned(), ModificationKind::Modified);
                }
            },
            StatusItem::IndexWorktree(item) => match item {
                IndexWorktreeItem::Modification {
                    rela_path, status, ..
                } => match status {
                    IndexEntryStatus::Change(change) => {
                        let kind = match change {
                            WorktreeChange::Removed => ModificationKind::Removed,
                            _ => ModificationKind::Modified,
                        };
                        map.insert(rela_path, kind);
                    }
                    // An unresolved merge conflict is a modification too.
                    IndexEntryStatus::Conflict { .. } => {
                        map.insert(rela_path, ModificationKind::Modified);
                    }
                    IndexEntryStatus::IntentToAdd => {
                        map.insert(rela_path, ModificationKind::Added);
                    }
                    // Pure stat refresh; nothing changed.
                    IndexEntryStatus::NeedsUpdate(_) => {}
                },
                IndexWorktreeItem::DirectoryContents { entry, .. } => {
                    if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                        map.insert(entry.rela_path, ModificationKind::Untracked);
                    }
                }
                IndexWorktreeItem::Rewrite { dirwalk_entry, .. } => {
                    map.insert(dirwalk_entry.rela_path, ModificationKind::Modified);
                }
            },
        }
    }
    Ok(map
        .into_iter()
        .map(|(path, kind)| TestamentModification {
            kind,
            path: lossy(&path),
        })
        .collect())
}

/// Number of days since the Unix epoch → `(year, month, day)`.
///
/// Howard Hinnant's civil-from-days algorithm; avoids pulling a date library
/// into the addon for one rendering.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

/// Render `seconds` as `YYYY-MM-DD` UTC.
fn iso_date(seconds: i64) -> String {
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Today's UTC date, used as the build-date stand-in for an unborn `HEAD`.
fn today_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_date(seconds)
}

/// Append `" dirty N modification(s)"` to a rendered testament.
fn render_dirty(rendered: &mut String, count: usize) {
    if count > 0 {
        rendered.push_str(&format!(
            " dirty {count} modification{}",
            if count == 1 { "" } else { "s" }
        ));
    }
}

// ---------------------------------------------------------------------------
// NAPI methods
// ---------------------------------------------------------------------------

#[napi]
impl Repository {
    /// Build a testament to the state of this repository at a commit.
    ///
    /// Combines `git describe` (closest tag + distance), the committer date,
    /// the branch name and the working tree state into one record whose
    /// `format` field mirrors `git_testament`'s rendering. Defaults to `HEAD`;
    /// pass `commit` for any other revision — one call per commit gives you one
    /// testament per commit, which is something the `git_testament!` macro
    /// cannot do (it is frozen at compile time).
    ///
    /// Throws when `commit` cannot be resolved; an unborn `HEAD` yields a
    /// testament of kind `Unborn` rather than an error.
    #[napi]
    pub fn testament(&self, options: Option<TestamentOptions>) -> napi::Result<Testament> {
        let what = "testament";
        let opts = options.as_ref();
        let r = thread_local(&self.inner);

        let branch = match r.head_name() {
            Ok(Some(name)) => Some(shorten(name.as_ref())),
            _ => None,
        };
        let version_opt = opts.and_then(|o| o.version.clone());

        let head = r.head().map_err(|e| err(what, e))?;
        if head.is_unborn() {
            let label = version_opt.as_deref().unwrap_or("unknown");
            let mut rendered = format!("{label} (uncommitted {})", today_utc());
            let modifications = collect(&r, opts)?;
            let dirty = dirty(&r, &modifications)?;
            render_dirty(&mut rendered, modifications.as_ref().map_or(0, Vec::len));
            return Ok(Testament {
                kind: TestamentCommitKind::Unborn,
                branch,
                commit: None,
                abbrev: None,
                seconds: 0,
                offset: 0,
                date: None,
                tag: None,
                distance: None,
                version: version_opt,
                dirty,
                modifications,
                trusted: false,
                format: rendered,
            });
        }

        let spec = opts.and_then(|o| o.commit.as_deref()).unwrap_or("HEAD");
        let id: gix::ObjectId = r
            .rev_parse_single(spec)
            .map_err(|e| err("rev_parse_single", e))?
            .into();
        let full = id.to_string();
        let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        let time = commit.time().map_err(|e| err("time", e))?;
        let abbrev = match full.get(..9) {
            Some(prefix) => Some(prefix.to_string()),
            None => None,
        };
        let date = iso_date(time.seconds);

        let mut platform = commit.describe();
        platform = platform.names(select_ref(opts.and_then(|o| o.names.as_deref()))?);
        let resolution = platform.try_resolve().map_err(|e| err("describe", e))?;

        let modifications = collect(&r, opts)?;
        let dirty = dirty(&r, &modifications)?;

        let (kind, tag, distance) = match &resolution {
            Some(resolution) => {
                // Already a short name (`shorten()` is applied by `describe`).
                let tag = resolution
                    .outcome
                    .name
                    .as_ref()
                    .map(|name| name.to_string());
                let distance = resolution.outcome.depth;
                (TestamentCommitKind::FromTag, tag, Some(distance))
            }
            None => (TestamentCommitKind::NoTags, None, None),
        };

        // Trusted branch: a clean tip of that branch is reported as though the
        // tag had been pushed at it, so a release pipeline can build first and
        // tag later without every testament shouting about a mismatch.
        let trusted = opts
            .and_then(|o| o.trusted_branch.as_deref())
            .zip(version_opt.as_deref())
            .is_some_and(|(trusted_branch, _)| {
                !dirty
                    && matches!(kind, TestamentCommitKind::FromTag)
                    && branch.as_deref() == Some(trusted_branch)
            });
        let (tag, distance) = if trusted {
            // SAFETY: `trusted` is only true when both were `Some`.
            (version_opt.clone(), Some(0))
        } else {
            (tag, distance)
        };

        let version = match (&tag, distance) {
            (Some(tag), Some(distance)) if distance > 0 => Some(format!("{tag}+{distance}")),
            (Some(tag), _) => Some(tag.clone()),
            (None, _) => None,
        };

        let mut rendered = match (&kind, &version, &abbrev) {
            (TestamentCommitKind::NoTags, _, Some(abbrev)) => {
                format!("unknown ({abbrev} {date})")
            }
            (TestamentCommitKind::FromTag, Some(version), Some(abbrev)) => {
                format!("{version} ({abbrev} {date})")
            }
            _ => "unknown".to_string(),
        };
        render_dirty(&mut rendered, modifications.as_ref().map_or(0, Vec::len));

        Ok(Testament {
            kind,
            branch,
            commit: Some(full),
            abbrev,
            seconds: time.seconds,
            offset: time.offset,
            date: Some(date),
            tag,
            distance,
            version,
            dirty,
            modifications,
            trusted,
            format: rendered,
        })
    }
}

/// Optionally walk the working tree, per [`TestamentOptions`].
fn collect(
    r: &gix::Repository,
    opts: Option<&TestamentOptions>,
) -> napi::Result<Option<Vec<TestamentModification>>> {
    if !opts.and_then(|o| o.include_modifications).unwrap_or(false) {
        return Ok(None);
    }
    let untracked = opts.and_then(|o| o.untracked).unwrap_or(true);
    Ok(Some(collect_modifications(r, untracked)?))
}

/// Whether the working tree is dirty: from the modification list when it was
/// collected, otherwise from gix's cheap index/worktree probe (which ignores
/// untracked files, matching [`Repository::is_dirty`](crate::status)).
fn dirty(
    r: &gix::Repository,
    modifications: &Option<Vec<TestamentModification>>,
) -> napi::Result<bool> {
    match modifications {
        Some(modifications) => Ok(modifications
            .iter()
            .any(|m| !matches!(m.kind, ModificationKind::Untracked))),
        None => Ok(r.is_dirty().map_err(|e| err("is_dirty", e))?),
    }
}
