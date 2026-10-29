//! Log / history bindings: the flat [`CommitInfo`] DTO returned by `Repository::log`, plus the
//! `log` method itself and the commit-decoding helper it shares with `Commit::info`.

use crate::error::{RepositoryError, err_repo, repo_error_to_napi, thread_local};
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
/// - `Topo` — strict topological order (`git log --topo-order`): a commit is emitted before
///   any of its descendants. Computed by an in-memory topological sort over the full walk, so
///   it materializes the whole history (and is what powers [`Repository::log_graph`]).
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
    /// Alias for `rev` (starting commit/revision).
    pub from: Option<String>,
    /// Maximum number of *matching* commits to return (counted after filters apply).
    pub limit: Option<f64>,
    /// Alias for `limit`.
    pub max_count: Option<f64>,
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

/// One row of a `git log --graph`-style layout. `parent_columns[i]` is the lane of
/// `parents[i]`; when it equals `column`, that parent continues the current lane (the
/// main-line edge). Other parents are merge parents drawn in their own lanes.
#[napi(object)]
pub struct GraphRow {
    /// Commit sha (same value as `CommitInfo.sha`).
    pub commit: String,
    /// Lane/column this commit is drawn in (0 = leftmost / main line).
    pub column: u32,
    /// Lane of each parent, aligned with `parents`.
    pub parent_columns: Vec<u32>,
    /// Parent shas, aligned with `parent_columns`.
    pub parents: Vec<String>,
    /// Whether the commit has more than one (kept) parent — i.e. a merge in the filtered view.
    pub is_merge: bool,
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
) -> Result<bool, RepositoryError> {
    let parents: Vec<gix::Tree> = if parent_ids.is_empty() {
        vec![r.empty_tree()]
    } else {
        let mut v = Vec::with_capacity(parent_ids.len());
        for pid in parent_ids {
            let ct = r
                .find_commit(*pid)
                .map_err(|e| err_repo("find_commit", e))?;
            v.push(ct.tree().map_err(|e| err_repo("parent tree", e))?);
        }
        v
    };
    for pt in &parents {
        let changes = r
            .diff_tree_to_tree(pt, Some(commit_tree), None)
            .map_err(|e| err_repo("diff_tree_to_tree", e))?;
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

/// One walked commit: its decoded summary, its parent object ids, and its own object id.
type GraphItem = (CommitInfo, Vec<gix::ObjectId>, gix::ObjectId);

/// Resolve the walk's start commit from `rev` or `from` (defaults to `HEAD`).
fn resolve_tip(
    r: &gix::Repository,
    rev: &Option<String>,
    from: &Option<String>,
) -> Result<gix::ObjectId, RepositoryError> {
    let spec = rev.as_ref().or(from.as_ref());
    match spec {
        Some(s) => r
            .rev_parse_single(s.as_str())
            .map_err(|e| err_repo("rev_parse_single", e))
            .map(Into::into),
        None => r
            .head_id()
            .map_err(|e| err_repo("head_id", e))
            .map(Into::into),
    }
}

/// Walk the full history from `tip` (honoring `first_parent`) and return every commit that
/// passes the `since`/`until`/`author`/`paths` filters, paired with its parent and own ids.
/// The whole history is materialized because topological ordering and graph layout require the
/// full commit graph in memory.
fn collect_filtered(
    r: &gix::Repository,
    tip: gix::ObjectId,
    first_parent: bool,
    since: Option<i64>,
    until: Option<i64>,
    author: Option<&str>,
    paths: &[String],
    has_paths: bool,
) -> Result<Vec<GraphItem>, RepositoryError> {
    let mut all: Vec<GraphItem> = Vec::new();
    if first_parent {
        let mut current = Some(tip);
        while let Some(id) = current {
            let (entry, parents, tree) = log_summary(r, id)?;
            let next = parents.first().copied();
            if (has_paths && !commit_touches_paths(r, &tree, &parents, paths)?)
                || !commit_matches(&entry, since, until, author)
            {
                current = next;
                continue;
            }
            all.push((entry, parents, id));
            current = next;
        }
    } else {
        let platform = r.rev_walk([tip]);
        for id in platform.all().map_err(|e| err_repo("rev_walk", e))? {
            let id = id.map_err(|e| err_repo("rev_walk item", e))?;
            let (entry, parents, tree) = log_summary(r, id.id)?;
            if (has_paths && !commit_touches_paths(r, &tree, &parents, paths)?)
                || !commit_matches(&entry, since, until, author)
            {
                continue;
            }
            all.push((entry, parents, id.id));
        }
    }
    Ok(all)
}

/// Strict topological order over the filtered commit set: a commit is emitted before any of its
/// parents (child-first), matching `git log --topo-order`. Implemented as iterative post-order
/// DFS from the tips (commits with no kept parent); the `in_stack` guard makes it cycle-safe.
fn topo_sort(items: &[GraphItem]) -> Vec<usize> {
    use std::collections::HashMap;
    let mut by_id: HashMap<gix::ObjectId, usize> = HashMap::new();
    for (i, (_c, _p, oid)) in items.iter().enumerate() {
        by_id.insert(*oid, i);
    }
    // Tips: commits whose parents are all absent from the kept set.
    let mut tips: Vec<usize> = Vec::new();
    for (i, (_c, parents, _oid)) in items.iter().enumerate() {
        if !parents.iter().any(|p| by_id.contains_key(p)) {
            tips.push(i);
        }
    }
    let mut emitted = vec![false; items.len()];
    let mut in_stack = vec![false; items.len()];
    let mut order: Vec<usize> = Vec::with_capacity(items.len());
    for &tip in &tips {
        if emitted[tip] {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(tip, 0)];
        in_stack[tip] = true;
        while let Some(&(node, ci)) = stack.last() {
            let parents = &items[node].1;
            if ci < parents.len() {
                stack.last_mut().unwrap().1 += 1;
                if let Some(&pidx) = by_id.get(&parents[ci]) {
                    if !emitted[pidx] && !in_stack[pidx] {
                        in_stack[pidx] = true;
                        stack.push((pidx, 0));
                    }
                }
            } else {
                if !emitted[node] {
                    emitted[node] = true;
                    order.push(node);
                }
                in_stack[node] = false;
                stack.pop();
            }
        }
    }
    for i in 0..items.len() {
        if !emitted[i] {
            order.push(i);
        }
    }
    order
}

/// Assign graph lanes (columns) to commits that are already ordered child-first. Returns, for
/// each commit, its own column and the column of each of its parents. A parent sharing the
/// commit's column is the main-line continuation; other parents get their own lanes (merge
/// edges). Lanes are reclaimed when a commit takes over an existing lane or a root's lane is
/// freed, keeping the layout compact.
fn assign_columns(items: &[&GraphItem]) -> Vec<(u32, Vec<u32>)> {
    let mut lanes: Vec<Option<gix::ObjectId>> = Vec::new();
    let mut out: Vec<(u32, Vec<u32>)> = Vec::with_capacity(items.len());
    for (_c, parents, oid) in items {
        // Continue the lane already owned by this commit (handed down by one of its children).
        let col = if let Some(l) = lanes.iter().position(|o| *o == Some(*oid)) {
            l as u32
        } else if let Some(l) = lanes.iter().position(|o| o.is_none()) {
            l as u32
        } else {
            lanes.push(Some(*oid));
            (lanes.len() - 1) as u32
        };
        // Free any other lanes that were owned by this commit (e.g. a merge's side lanes).
        for (i, o) in lanes.iter_mut().enumerate() {
            if i != col as usize && *o == Some(*oid) {
                *o = None;
            }
        }
        if (col as usize) >= lanes.len() {
            lanes.push(None);
        }
        lanes[col as usize] = Some(*oid);
        let mut pcols = Vec::with_capacity(parents.len());
        for (i, p) in parents.iter().enumerate() {
            if i == 0 {
                // First parent continues down this lane.
                lanes[col as usize] = Some(*p);
                pcols.push(col);
            } else if let Some(l) = lanes.iter().position(|o| o.is_none()) {
                lanes[l] = Some(*p);
                pcols.push(l as u32);
            } else {
                lanes.push(Some(*p));
                pcols.push((lanes.len() - 1) as u32);
            }
        }
        if parents.is_empty() {
            lanes[col as usize] = None;
        }
        out.push((col, pcols));
    }
    out
}

#[napi]
impl Repository {
    /// Read commit history (a "git log") starting at `options.rev` (or `HEAD`).
    ///
    /// Returns commit summaries as flat [`CommitInfo`] objects, resolved in one call (no
    /// per-commit round-trips). `options` controls the start point, limit, first-parent
    /// traversal, date/author/path filters, sort order and direction. See [`LogOptions`].
    #[napi]
    pub fn log(
        &self,
        env: napi::Env,
        options: Option<LogOptions>,
    ) -> napi::Result<Vec<CommitInfo>> {
        let opts = options.unwrap_or_default();
        let r = thread_local(&self.inner);
        match Self::log_impl(&r, &opts) {
            Ok(v) => Ok(v),
            Err(e) => Err(e.into()),
        }
    }

    /// Body of [`Repository::log`], returning [`RepositoryError`] so the `#[napi]`
    /// boundary can attach `category`/`kind`/`suggestion` to the thrown JS error.
    fn log_impl(
        r: &gix::Repository,
        opts: &LogOptions,
    ) -> Result<Vec<CommitInfo>, RepositoryError> {
        let tip = resolve_tip(r, &opts.rev, &opts.from)?;
        let limit = opts.limit.or(opts.max_count).map(|l| l as usize);
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

        // Topological order requires the full commit graph in memory.
        if matches!(opts.order, Some(LogOrder::Topo)) {
            let mut all = collect_filtered(
                r,
                tip,
                first_parent,
                since,
                until,
                author.as_deref(),
                &paths,
                has_paths,
            )?;
            let order = topo_sort(&all);
            let mut out: Vec<CommitInfo> = order.into_iter().map(|i| all[i].0.clone()).collect();
            if let Some(l) = limit {
                out.truncate(l);
            }
            if reverse {
                out.reverse();
            }
            return Ok(out);
        }

        let mut out: Vec<CommitInfo> = Vec::new();

        if first_parent {
            let mut current = Some(tip);
            while let Some(id) = current {
                let (entry, parents, tree) = log_summary(r, id)?;
                let skip = has_paths && !commit_touches_paths(r, &tree, &parents, &paths)?;
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
            for id in platform.all().map_err(|e| err_repo("rev_walk", e))? {
                let id = id.map_err(|e| err_repo("rev_walk item", e))?;
                let (entry, _parents, tree) = log_summary(r, id.id)?;
                if has_paths && !commit_touches_paths(r, &tree, &_parents, &paths)? {
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

    /// Read commit history and return a structural graph layout suitable for rendering a
    /// `git log --graph`-style visualization. Each row gives the commit, the lane/column it is
    /// drawn in, and the lane of each of its parents (`parent_columns`/`parents` are aligned by
    /// index). A parent sharing the commit's `column` is the main-line continuation; other
    /// parents are merge parents drawn in their own lanes.
    ///
    /// Shares the same `options` filters as [`Repository::log`] (`since`/`until`/`author`/
    /// `paths`/`first_parent`/`limit`/`reverse`). Order is always topological (child before
    /// parent) so the layout is well-formed; the `order` option is accepted but ignored here.
    #[napi]
    pub fn log_graph(
        &self,
        env: napi::Env,
        options: Option<LogOptions>,
    ) -> napi::Result<Vec<GraphRow>> {
        let opts = options.unwrap_or_default();
        let r = thread_local(&self.inner);
        match Self::log_graph_impl(&r, &opts) {
            Ok(v) => Ok(v),
            Err(e) => Err(e.into()),
        }
    }

    /// Body of [`Repository::log_graph`], returning [`RepositoryError`] so the `#[napi]`
    /// boundary can attach `category`/`kind`/`suggestion` to the thrown JS error.
    fn log_graph_impl(
        r: &gix::Repository,
        opts: &LogOptions,
    ) -> Result<Vec<GraphRow>, RepositoryError> {
        let tip = resolve_tip(r, &opts.rev, &opts.from)?;
        let limit = opts.limit.or(opts.max_count).map(|l| l as usize);
        let first_parent = opts.first_parent.unwrap_or(false);
        let reverse = opts.reverse.unwrap_or(false);
        let since = opts.since.as_deref().and_then(parse_date);
        let until = opts.until.as_deref().and_then(parse_date);
        let author = opts.author.as_deref().map(|a| a.to_lowercase());
        let paths = opts.paths.clone().unwrap_or_default();
        let has_paths = !paths.is_empty();

        let mut all = collect_filtered(
            r,
            tip,
            first_parent,
            since,
            until,
            author.as_deref(),
            &paths,
            has_paths,
        )?;
        let present: std::collections::HashSet<gix::ObjectId> =
            all.iter().map(|(_, _, o)| *o).collect();
        let order = topo_sort(&all);
        let ordered: Vec<&GraphItem> = order.iter().map(|&i| &all[i]).collect();
        let columns = assign_columns(&ordered);
        let mut rows: Vec<GraphRow> = ordered
            .iter()
            .zip(columns.iter())
            .map(|((c, parents, _oid), (col, pcols))| {
                let kept: Vec<(String, u32)> = parents
                    .iter()
                    .zip(pcols.iter())
                    .filter(|(p, _)| present.contains(*p))
                    .map(|(p, pc)| (p.to_string(), *pc))
                    .collect();
                GraphRow {
                    commit: c.sha.clone(),
                    column: *col,
                    parent_columns: kept.iter().map(|(_, pc)| *pc).collect(),
                    parents: kept.iter().map(|(p, _)| p.clone()).collect(),
                    is_merge: kept.len() > 1,
                }
            })
            .collect();
        if let Some(l) = limit {
            rows.truncate(l);
        }
        if reverse {
            rows.reverse();
        }
        Ok(rows)
    }
}

/// Decode a commit into owned data for [`CommitInfo`], returning the entry plus its parent ids.
/// Shared by `Repository::log` and `Commit::info`.
pub(crate) fn log_summary(
    r: &gix::Repository,
    id: gix::ObjectId,
) -> Result<(CommitInfo, Vec<gix::ObjectId>, gix::Tree), RepositoryError> {
    let commit = r.find_commit(id).map_err(|e| err_repo("find_commit", e))?;
    let tree = commit.tree().map_err(|e| err_repo("commit tree", e))?;
    let c = commit.decode().map_err(|e| err_repo("decode commit", e))?;
    let author = c.author().map_err(|e| err_repo("author", e))?;
    let committer = c.committer().map_err(|e| err_repo("committer", e))?;
    let at = author.time().map_err(|e| err_repo("author time", e))?;
    let ct = committer
        .time()
        .map_err(|e| err_repo("committer time", e))?;
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
