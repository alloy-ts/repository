use std::ops::RangeInclusive;

use gix::bstr::BStr;
use gix::diff::Rewrites;
use gix::diff::blob::Algorithm as DiffAlgorithm;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::{Repository, lossy};

/// A single blame hunk mapping a range of lines in the blamed file to the commit that introduced it.
#[derive(Clone)]
#[napi(object)]
pub struct BlameEntry {
    /// 0-based line index where this hunk begins in the blamed file.
    pub start_in_blamed_file: u32,
    /// 0-based line index where this hunk begins in the source file (may differ if the file moved).
    pub start_in_source_file: u32,
    /// Number of lines this hunk spans.
    pub len: u32,
    /// The id of the commit that introduced this hunk.
    pub commit_id: String,
    /// The source file name, if it differs from the blamed file (renamed file).
    pub source_file_name: Option<String>,
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
}

/// Map a user-supplied diff algorithm name to the backend enum.
fn parse_diff_algorithm(s: &str) -> Result<DiffAlgorithm> {
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
fn build_ranges(ranges: Option<&[BlameRange]>) -> Result<gix::blame::BlameRanges> {
    match ranges {
        None => Ok(gix::blame::BlameRanges::default()),
        Some(rs) => {
            let inclusive: Vec<RangeInclusive<u32>> = rs.iter().map(|r| r.start..=r.end).collect();
            gix::blame::BlameRanges::from_one_based_inclusive_ranges(inclusive)
                .map_err(|e| err("blame_ranges", e))
        }
    }
}

/// Convert a `gix_blame::Outcome` into the napi `BlameOutcome`.
fn map_outcome(outcome: gix::blame::Outcome) -> BlameOutcome {
    let entries = outcome
        .entries
        .iter()
        .map(|e| BlameEntry {
            start_in_blamed_file: e.start_in_blamed_file,
            start_in_source_file: e.start_in_source_file,
            len: e.len.get(),
            commit_id: e.commit_id.to_string(),
            source_file_name: e.source_file_name.as_ref().map(|n| lossy(n)),
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
    pub fn blame(&self, file_path: String, commit_id: Option<String>) -> Result<BlameOutcome> {
        self.blame_with_options(file_path, commit_id, None)
    }

    /// Compute a blame with fine-grained control over the diff algorithm, line ranges, the `since`
    /// cutoff, and rename tracking.
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
    ) -> Result<BlameOutcome> {
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
        let rewrites = opts
            .track_renames
            .filter(|v| *v)
            .map(|_| Rewrites::default());

        let gix_opts = gix::repository::blame_file::Options {
            diff_algorithm,
            ranges,
            since,
            rewrites,
        };

        let outcome = r
            .blame_file(BStr::new(file_path.as_str()), suspect, gix_opts)
            .map_err(|e| err("blame_file", e))?;

        Ok(map_outcome(outcome))
    }
}
