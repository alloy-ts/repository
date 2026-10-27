use std::collections::HashMap;
use std::ops::RangeInclusive;

use gix::bstr::BStr;
use gix::diff::Rewrites;
use gix::diff::blob::Algorithm as DiffAlgorithm;
use gix::diff::rewrites::Copies;

use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_actor};
use crate::repository::{Repository, lossy};

/// A single blame hunk mapping a range of lines in the blamed file to the commit that introduced it.
#[derive(Clone)]
#[napi(object)]
pub struct BlameEntry {
    /// 1-based line index where this hunk begins in the blamed file
    /// (matches `git blame` output and the git2 `final_start_line_number`).
    pub start_in_blamed_file: u32,
    /// 1-based line index where this hunk begins in the source file (may differ if the file moved).
    pub start_in_source_file: u32,
    /// Number of lines this hunk spans.
    pub len: u32,
    /// The id of the commit that introduced this hunk.
    pub commit_id: String,
    /// The source file name, if it differs from the blamed file (renamed file).
    pub source_file_name: Option<String>,
    /// Author of the introducing commit, or `null` unless `resolveCommits` was set.
    pub author: Option<Signature>,
    /// Committer of the introducing commit, or `null` unless `resolveCommits` was set.
    pub committer: Option<Signature>,
    /// First paragraph of the introducing commit message, or `null` unless `resolveCommits` was set.
    pub summary: Option<String>,
}

/// Statistics about the work performed while computing a blame.
#[derive(Clone)]
#[napi(object)]
pub struct BlameStatistics {
    /// The amount of commits it traversed until the blame was complete.
    pub commits_traversed: u32,
    /// The amount of trees that were decoded to find the entry of the file to blame.
    pub trees_decoded: u32,
    /// The amount of tree-diffs to see if the filepath was added, deleted or modified.
    pub trees_diffed: u32,
    /// The amount of tree-diffs to see if the file was moved (rewritten).
    pub trees_diffed_with_rewrites: u32,
    /// The amount of blobs there were compared to each other to learn what changed between commits.
    pub blobs_diffed: u32,
}

/// A single step in the blame's history traversal.
///
/// Captures enough information to reconstruct how a blame was performed (the path the history
/// traversal, combined with repeated diffing, has taken). Intended for debugging.
///
/// This is currently always `null` because the high-level [`gix::Repository::blame_file`] wrapper
/// forces `debug_track_path` off; it is exposed for completeness and will be populated if that
/// changes.
#[derive(Clone)]
#[napi(object)]
pub struct BlamePathEntry {
    /// The path to the *Source File* in the blob after the change.
    pub source_file_path: String,
    /// The path to the *Source File* in the blob before the change. Allows detection of renames. `null` for root commits.
    pub previous_source_file_path: Option<String>,
    /// The commit id associated with the state after the change.
    pub commit_id: String,
    /// The blob id associated with the state after the change.
    pub blob_id: String,
    /// The blob id associated with the state before the change.
    pub previous_blob_id: String,
    /// When there is more than one entry for a commit, indicates to which parent commit the change is related.
    pub parent_index: u32,
}

/// The result of blaming a file.
#[derive(Clone)]
#[napi(object)]
pub struct BlameOutcome {
    /// The blame hunks, in file order.
    pub entries: Vec<BlameEntry>,
    /// The full content of the blamed file (as it exists at the start commit).
    pub blob: Vec<u8>,
    /// Additional information about the amount of work performed to produce the blame.
    pub statistics: BlameStatistics,
    /// A log of all changes that affected the outcome of this blame, or `null`.
    pub blame_path: Option<Vec<BlamePathEntry>>,
}

/// A 1-based, inclusive line range to blame (as git uses), e.g. `{ start: 1, end: 10 }` covers lines 1 through 10.
#[derive(Clone)]
#[napi(object)]
pub struct BlameRange {
    /// First line, 1-based and inclusive.
    pub start: u32,
    /// Last line, 1-based and inclusive.
    pub end: u32,
}

/// Options controlling a blame computation.
#[derive(Clone, Default)]
#[napi(object)]
pub struct BlameOptions {
    /// Diff algorithm: `myers` (default), `minimal`, or `histogram`.
    ///
    /// Note: `patience` is not available in the underlying backend.
    pub diff_algorithm: Option<String>,
    /// 1-based, inclusive line ranges to blame. Omit (or pass `null`) to blame the whole file.
    pub ranges: Option<Vec<BlameRange>>,
    /// Only consider commits at or after this unix timestamp (seconds).
    pub since: Option<f64>,
    /// Track renames/rewrites (50% similarity, git default) while blaming.
    pub track_renames: Option<bool>,
    /// Also track copies (beyond renames). Implies rewrite tracking.
    pub track_copies: Option<bool>,
    /// Resolve each hunk's commit to attach `author`, `committer` and `summary`.
    /// Off by default to avoid the per-commit lookup cost when only `commitId` is needed.
    pub resolve_commits: Option<bool>,
}

/// Map a user-supplied diff algorithm name to the backend enum.
fn parse_diff_algorithm(s: &str) -> napi::Result<DiffAlgorithm> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "myers" => DiffAlgorithm::Myers,
        "minimal" => DiffAlgorithm::MyersMinimal,
        "histogram" => DiffAlgorithm::Histogram,
        other => {
            return Err(napi::Error::from_reason(format!(
                "unknown diff algorithm: {other} (expected myers, minimal, or histogram)"
            )));
        }
    })
}

/// Build a `gix_blame` range specification from napi options.
fn build_ranges(ranges: Option<&[BlameRange]>) -> napi::Result<gix::blame::BlameRanges> {
    match ranges {
        None => Ok(gix::blame::BlameRanges::default()),
        Some(rs) => {
            let inclusive: Vec<RangeInclusive<u32>> = rs.iter().map(|r| r.start..=r.end).collect();
            gix::blame::BlameRanges::from_one_based_inclusive_ranges(inclusive)
                .map_err(|e| err("blame_ranges", e))
        }
    }
}

/// Resolve a commit to its author, committer and message summary.
///
/// Returns `None` when the commit cannot be found or decoded (the corresponding
/// blame entries simply omit the commit details rather than failing the whole blame).
fn resolve_commit(
    r: &gix::Repository,
    oid: &gix::ObjectId,
) -> (Signature, Signature, Option<String>) {
    let none = || {
        (
            Signature {
                name: String::new(),
                email: String::new(),
                time: 0,
                timezone_offset: 0,
            },
            Signature {
                name: String::new(),
                email: String::new(),
                time: 0,
                timezone_offset: 0,
            },
            None,
        )
    };

    let commit_handle = match r.find_commit(*oid).ok() {
        Some(c) => c,
        None => return none(),
    };
    let commit = match commit_handle.decode().ok() {
        Some(c) => c,
        None => return none(),
    };
    let author = match commit.author().ok() {
        Some(s) => signature_from_actor(&s),
        None => return none(),
    };
    let committer = match commit.committer().ok() {
        Some(s) => signature_from_actor(&s),
        None => return none(),
    };
    let summary = {
        let message = gix::objs::commit::MessageRef::from_bytes(commit.message.as_ref());
        let s = message.summary();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    };
    (author, committer, summary)
}

/// Convert a `gix_blame::Outcome` into the napi `BlameOutcome`.
///
/// When `resolve` is set, each unique introducing commit is looked up once to
/// attach its author, committer and message summary to the matching entries.
fn map_outcome(r: &gix::Repository, outcome: gix::blame::Outcome, resolve: bool) -> BlameOutcome {
    // O(unique commits) lookups; empty when resolution is disabled.
    let resolved: HashMap<gix::ObjectId, Option<(Signature, Signature, Option<String>)>> =
        if resolve {
            let mut m = HashMap::with_capacity(outcome.entries.len());
            for e in &outcome.entries {
                m.entry(e.commit_id)
                    .or_insert_with(|| Some(resolve_commit(r, &e.commit_id)));
            }
            m
        } else {
            HashMap::new()
        };

    let entries = outcome
        .entries
        .iter()
        .map(|e| {
            let (author, committer, summary) = resolved
                .get(&e.commit_id)
                .and_then(|o| o.as_ref())
                .map(|t| (Some(t.0.clone()), Some(t.1.clone()), t.2.clone()))
                .unwrap_or((None, None, None));
            BlameEntry {
                // gix reports 0-based start lines; surface 1-based to match git and git2.
                start_in_blamed_file: e.start_in_blamed_file + 1,
                start_in_source_file: e.start_in_source_file + 1,
                len: e.len.get(),
                commit_id: e.commit_id.to_string(),
                source_file_name: e.source_file_name.as_ref().map(|n| lossy(n)),
                author,
                committer,
                summary,
            }
        })
        .collect();

    let statistics = BlameStatistics {
        commits_traversed: outcome.statistics.commits_traversed as u32,
        trees_decoded: outcome.statistics.trees_decoded as u32,
        trees_diffed: outcome.statistics.trees_diffed as u32,
        trees_diffed_with_rewrites: outcome.statistics.trees_diffed_with_rewrites as u32,
        blobs_diffed: outcome.statistics.blobs_diffed as u32,
    };

    let blame_path = outcome.blame_path.map(|entries| {
        entries
            .into_iter()
            .map(|e| BlamePathEntry {
                source_file_path: lossy(&e.source_file_path),
                previous_source_file_path: e.previous_source_file_path.as_ref().map(|p| lossy(p)),
                commit_id: e.commit_id.to_string(),
                blob_id: e.blob_id.to_string(),
                previous_blob_id: e.previous_blob_id.to_string(),
                parent_index: e.parent_index as u32,
            })
            .collect()
    });

    BlameOutcome {
        entries,
        blob: outcome.blob,
        statistics,
        blame_path,
    }
}

#[napi]
impl Repository {
    /// Compute a blame for `file_path` (relative to the repository root), starting from
    /// `commit_id` (or `HEAD` if omitted). Returns the per-hunk blame entries plus the full
    /// content of the blamed file.
    ///
    /// This is a convenience wrapper around
    /// [`blame_with_options`](Self::blame_with_options) that uses default options (whole file,
    /// the repository's configured diff algorithm, no rename tracking, no `since` filter).
    #[napi]
    pub fn blame(
        &self,
        file_path: String,
        commit_id: Option<String>,
    ) -> napi::Result<BlameOutcome> {
        self.blame_with_options(file_path, commit_id, None)
    }

    /// Compute a blame with fine-grained control over the diff algorithm, line ranges, the `since`
    /// cutoff, rename/copy tracking, and optional per-commit resolution.
    ///
    /// * `file_path` — slash-separated, repository-relative path to the file to blame.
    /// * `commit_id` — the commit to start from, or `HEAD` if omitted.
    /// * `options` — optional [`BlameOptions`]; `null` is equivalent to [`blame`](Self::blame).
    #[napi]
    pub fn blame_with_options(
        &self,
        file_path: String,
        commit_id: Option<String>,
        options: Option<BlameOptions>,
    ) -> napi::Result<BlameOutcome> {
        let r = thread_local(&self.inner);
        let suspect = match commit_id {
            Some(c) => parse_oid(&c)?,
            None => r.head_commit().map_err(|e| err("head_commit", e))?.id,
        };

        let opts = options.unwrap_or_default();
        let diff_algorithm = match opts.diff_algorithm {
            Some(ref s) => Some(parse_diff_algorithm(s)?),
            None => None,
        };
        let ranges = build_ranges(opts.ranges.as_deref())?;
        let since = opts.since.map(|s| gix::date::Time {
            seconds: s as i64,
            offset: 0,
        });
        let track_copies = opts.track_copies.unwrap_or(false);
        let rewrites = if opts.track_renames.unwrap_or(false) || track_copies {
            let mut rw = Rewrites::default();
            if track_copies {
                rw.copies = Some(Copies::default());
            }
            Some(rw)
        } else {
            None
        };

        let gix_opts = gix::repository::blame_file::Options {
            diff_algorithm,
            ranges,
            since,
            rewrites,
        };

        let outcome = r
            .blame_file(BStr::new(file_path.as_str()), suspect, gix_opts)
            .map_err(|e| err("blame_file", e))?;

        Ok(map_outcome(
            &r,
            outcome,
            opts.resolve_commits.unwrap_or(false),
        ))
    }

    /// Compute a blame and return only the hunk covering `line` (1-based), or `null`
    /// when the line is outside the file. Equivalent to calling
    /// [`blame_with_options`](Self::blame_with_options) and binary-searching `entries`,
    /// but exposed for ergonomics.
    ///
    /// * `file_path` — slash-separated, repository-relative path to the file to blame.
    /// * `line` — 1-based line number in the blamed file.
    /// * `commit_id` — the commit to start from, or `HEAD` if omitted.
    /// * `options` — optional [`BlameOptions`]; honors `resolveCommits` like the others.
    #[napi]
    pub fn blame_line(
        &self,
        file_path: String,
        line: u32,
        commit_id: Option<String>,
        options: Option<BlameOptions>,
    ) -> napi::Result<Option<BlameEntry>> {
        let outcome = self.blame_with_options(file_path, commit_id, options)?;
        Ok(outcome
            .entries
            .into_iter()
            .find(|e| line >= e.start_in_blamed_file && line < e.start_in_blamed_file + e.len))
    }
}
