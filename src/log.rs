//! Log / history bindings: the flat [`CommitInfo`] DTO returned by `Repository::log`, plus the
//! `log` method itself and the commit-decoding helper it shares with `Commit::info`.

use crate::error::{err, thread_local};
use crate::object::{lossy, message_body, rfc3339, short_sha};
use crate::repository::Repository;

use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use napi_derive::napi;

/// A flat, JSON-serializable snapshot of a commit, as returned by [`Repository::log`]. Mirrors the
/// fields of a typical `git log` entry so callers can render history without opening each commit.
#[derive(Clone)]
#[napi(object)]
pub struct CommitInfo {
    /// Full object id (SHA-1 / SHA-256).
    pub sha: String,
    /// Abbreviated object id: the shortest unambiguous prefix (never shorter than 7 characters).
    pub short_sha: String,
    /// Full commit message.
    pub message: String,
    /// First line (subject) of the message.
    pub subject: String,
    /// Message body: everything after the first paragraph, trimmed; empty when there is none.
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    /// Authoring time, unix seconds.
    pub author_time: i64,
    /// Authoring time as an RFC 3339 / ISO 8601 string.
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    /// Committing time, unix seconds.
    pub committer_time: i64,
    /// Committing time as an RFC 3339 / ISO 8601 string.
    pub committer_date: String,
    pub parent_ids: Vec<String>,
    pub tree_id: String,
    /// Raw `gpgsig` / `gpgsig-sha256` header when the commit is signed, else `null`.
    pub signature: Option<String>,
}

/// Sort order for [`Repository::log`].
///
/// - `Default` — gix's topological breadth-first order (a commit is emitted before its
///   parents; this is what `git log` does by default).
/// - `Date` — commits ordered by commit time, newest first (`git log --date-order`).
/// - `Topo` — strict topological order (`git log --topo-order`). gix's walk has no native
///   `--topo-order`, so this currently falls back to `Default` (topological breadth-first);
///   the stricter layout arrives with the graph API.
#[napi(string_enum)]
pub enum LogOrder {
    Default,
    Date,
    Topo,
}

/// Options for [`Repository::log`], replacing the older positional arguments with a single,
/// extensible, JS-friendly object.
#[derive(Default)]
#[napi(object)]
pub struct LogOptions {
    /// Start of the walk (branch, tag, `HEAD`, `<oid>`, `~`/`^` expressions). Defaults to `HEAD`.
    pub rev: Option<String>,
    /// Maximum number of *matching* commits to return (counted after filters apply).
    pub limit: Option<f64>,
    /// Follow only the first parent of each merge commit.
    pub first_parent: Option<bool>,
    /// Include only commits whose committer date is at or after this time. Accepts a Unix
    /// timestamp (seconds) or an RFC 3339 / `YYYY-MM-DD` string.
    pub since: Option<String>,
    /// Include only commits whose committer date is at or before this time (same formats as `since`).
    pub until: Option<String>,
    /// Include only commits whose author name or email contains this substring (case-insensitive).
    pub author: Option<String>,
    /// Sort order; see [`LogOrder`].
    pub order: Option<LogOrder>,
    /// Return commits oldest-first instead of newest-first.
    pub reverse: Option<bool>,
    /// Include only commits that changed (added/modified/deleted/renamed) at least one of
    /// these paths or paths under these directories. A directory matches recursively
    /// (`"src"` matches `"src/a.rs"`). This diffs each candidate commit's tree against its
    /// parent(s), so it is more expensive than the other filters.
    pub paths: Option<Vec<String>>,
}

/// Parse a date filter value: Unix timestamp, RFC 3339, or `YYYY-MM-DD` (midnight UTC).
fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(ts) = s.parse::<i64>() {
        return Some(ts);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(t) = chrono::NaiveTime::from_hms_opt(0, 0, 0) {
            return Some(d.and_time(t).and_utc().timestamp());
        }
    }
    None
}

/// Apply the `since`/`until`/`author` filters to a decoded commit.
fn commit_matches(
    c: &CommitInfo,
    since: Option<i64>,
    until: Option<i64>,
    author: Option<&str>,
) -> bool {
    if let Some(s) = since {
        if c.committer_time < s {
            return false;
        }
    }
    if let Some(u) = until {
        if c.committer_time > u {
            return false;
        }
    }
    if let Some(a) = author {
        let name = c.author_name.to_lowercase();
        let email = c.author_email.to_lowercase();
        if !name.contains(a) && !email.contains(a) {
            return false;
        }
    }
    true
}

/// Whether `commit_tree` differs from any of its parents (or from the empty tree, for a
/// root commit) on at least one of `paths`. Mirrors `git log -- <paths>`: a path matches if
/// it equals a changed path or is a parent directory of one.
fn commit_touches_paths(
    r: &gix::Repository,
    commit_tree: &gix::Tree,
    parent_ids: &[gix::ObjectId],
    paths: &[String],
) -> napi::Result<bool> {
    let parents: Vec<gix::Tree> = if parent_ids.is_empty() {
        vec![r.empty_tree()]
    } else {
        let mut v = Vec::with_capacity(parent_ids.len());
        for pid in parent_ids {
            let ct = r.find_commit(*pid).map_err(|e| err("find_commit", e))?;
            v.push(ct.tree().map_err(|e| err("parent tree", e))?);
        }
        v
    };
    for pt in &parents {
        let changes = r
            .diff_tree_to_tree(pt, Some(commit_tree), None)
            .map_err(|e| err("diff_tree_to_tree", e))?;
        for change in changes {
            if path_matches(change.location(), paths) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Directory-aware path match: `loc` matches if it equals `p` or lives under `p/`; a
/// trailing slash on `p` is ignored; an empty `p` matches everything.
fn path_matches(loc: &[u8], paths: &[String]) -> bool {
    for p in paths {
        let mut p = p.as_bytes();
        while p.last() == Some(&b'/') {
            p = &p[..p.len() - 1];
        }
        if p.is_empty() {
            return true;
        }
        if loc == p {
            return true;
        }
        if loc.starts_with(p) && loc.get(p.len()) == Some(&b'/') {
            return true;
        }
    }
    false
}

#[napi]
impl Repository {
    /// Read commit history (a "git log") starting at `options.rev` (or `HEAD`).
    ///
    /// Returns commit summaries as flat [`CommitInfo`] objects, resolved in one call (no
    /// per-commit round-trips). `options` controls the start point, limit, first-parent
    /// traversal, date/author/path filters, sort order and direction. See [`LogOptions`].
    #[napi]
    pub fn log(&self, options: Option<LogOptions>) -> napi::Result<Vec<CommitInfo>> {
        let opts = options.unwrap_or_default();
        let r = thread_local(&self.inner);
        let tip: gix::ObjectId = match &opts.rev {
            Some(s) => r
                .rev_parse_single(s.as_str())
                .map_err(|e| err("rev_parse_single", e))?
                .into(),
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        let limit = opts.limit.map(|l| l as usize);
        let first_parent = opts.first_parent.unwrap_or(false);
        let reverse = opts.reverse.unwrap_or(false);
        let since = opts.since.as_deref().and_then(parse_date);
        let until = opts.until.as_deref().and_then(parse_date);
        let author = opts.author.as_deref().map(|a| a.to_lowercase());
        let paths = opts.paths.clone().unwrap_or_default();
        let has_paths = !paths.is_empty();

        let sorting = match opts.order {
            Some(LogOrder::Date) => Some(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst)),
            _ => None,
        };

        let mut out: Vec<CommitInfo> = Vec::new();

        if first_parent {
            let mut current = Some(tip);
            while let Some(id) = current {
                let (entry, parents, tree) = log_summary(&r, id)?;
                let skip = has_paths && !commit_touches_paths(&r, &tree, &parents, &paths)?;
                let next = parents.into_iter().next();
                if skip {
                    current = next;
                    continue;
                }
                if commit_matches(&entry, since, until, author.as_deref()) {
                    out.push(entry);
                    if let Some(l) = limit {
                        if out.len() >= l {
                            break;
                        }
                    }
                }
                current = next;
            }
        } else {
            let mut platform = r.rev_walk([tip]);
            if let Some(s) = sorting {
                platform = platform.sorting(s);
            }
            for id in platform.all().map_err(|e| err("rev_walk", e))? {
                let id = id.map_err(|e| err("rev_walk item", e))?;
                let (entry, _parents, tree) = log_summary(&r, id.id)?;
                if has_paths && !commit_touches_paths(&r, &tree, &_parents, &paths)? {
                    continue;
                }
                if commit_matches(&entry, since, until, author.as_deref()) {
                    out.push(entry);
                    if let Some(l) = limit {
                        if out.len() >= l {
                            break;
                        }
                    }
                }
            }
        }

        if reverse {
            out.reverse();
        }
        Ok(out)
    }
}

/// Decode a commit into owned data for [`CommitInfo`], returning the entry plus its parent ids.
/// Shared by `Repository::log` and `Commit::info`.
pub(crate) fn log_summary(
    r: &gix::Repository,
    id: gix::ObjectId,
) -> napi::Result<(CommitInfo, Vec<gix::ObjectId>, gix::Tree)> {
    let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
    let tree = commit.tree().map_err(|e| err("commit tree", e))?;
    let c = commit.decode().map_err(|e| err("decode commit", e))?;
    let author = c.author().map_err(|e| err("author", e))?;
    let committer = c.committer().map_err(|e| err("committer", e))?;
    let at = author.time().map_err(|e| err("author time", e))?;
    let ct = committer.time().map_err(|e| err("committer time", e))?;
    let full = lossy(c.message);
    let subject = full.split('\n').next().unwrap_or("").to_string();
    let start = full.bytes().take_while(|&b| b == b'\n').count();
    let body = message_body(&full.as_bytes()[start..]).unwrap_or_default();
    let parent_ids: Vec<String> = c.parents().map(|p| p.to_string()).collect();
    let parent_oids: Vec<gix::ObjectId> = c.parents().collect();
    let signature = c
        .extra_headers()
        .find("gpgsig")
        .or_else(|| c.extra_headers().find("gpgsig-sha256"))
        .map(|v| lossy(v));
    let entry = CommitInfo {
        sha: id.to_string(),
        short_sha: short_sha(r, &id),
        message: full,
        subject,
        body,
        author_name: lossy(author.name),
        author_email: lossy(author.email),
        author_time: at.seconds,
        author_date: rfc3339(at.seconds, at.offset),
        committer_name: lossy(committer.name),
        committer_email: lossy(committer.email),
        committer_time: ct.seconds,
        committer_date: rfc3339(ct.seconds, ct.offset),
        parent_ids: parent_ids.clone(),
        tree_id: c.tree().to_string(),
        signature,
    };
    Ok((entry, parent_oids, tree))
}
