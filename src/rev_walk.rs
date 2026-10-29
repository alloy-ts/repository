use std::sync::Arc;

use napi::bindgen_prelude::Function;
use napi_derive::napi;

use crate::error::RepositoryResult as Result;
use crate::error::{err, err_str, thread_local};
use crate::repository::Repository;

/// Options controlling [`Repository::rev_walk`].
#[napi(object)]
pub struct WalkOptions {
    /// Sort order: `breadth-first` (default), `time-newest`, or `time-oldest`. Unknown
    /// values are rejected. (There is no topological order — libgit2's `Sort::TOPOLOGICAL`
    /// has no equivalent in gix's walk.)
    pub sort: Option<String>,
    /// Reverse the resulting commit list.
    pub reverse: Option<bool>,
    /// Traverse only the first parent of each commit.
    pub first_parent: Option<bool>,
    /// Revision specs (`HEAD~10`, `v1.0.0`, `<oid>`, ...) whose commits *and ancestors*
    /// are removed from the walk — libgit2's `hide`/`hide_head`/`hide_ref`.
    pub exclude: Option<Vec<String>>,
    /// A glob of references whose targets are added as walk tips — libgit2's `push_glob`.
    /// An implied `refs/` prefix and `/*` suffix follow git's rules, so `"tags/v"` matches
    /// `refs/tags/*`. Tags pointing at other objects are peeled by the walk.
    pub glob: Option<String>,
    /// A revision range: `to` is pushed and `from` is hidden — libgit2's `push_range`.
    /// `<from>..<to>` walks commits in `to` but not in `from`; the symmetric range
    /// `<from>...<to>` walks commits reachable from either side but not from both
    /// (the merge base of the two is hidden).
    pub range: Option<String>,
    /// Cap the number of commits returned (no limit when omitted).
    pub limit: Option<f64>,
}

/// Sort order for [`Revwalk::set_sorting`]. Mirrors libgit2's `git_sort_t` flags.
///
/// gix has no topological order and no in-walk reverse, so:
/// - `Topological` falls back to `BreadthFirst` (the default).
/// - `Time` selects commit-time ordering.
/// - `Reverse` reverses the collected list (computed after the walk).
#[napi(string_enum)]
pub enum RevwalkSort {
    None,
    Topological,
    Time,
    Reverse,
}

/// Split a revision range into its sides. `a..b` yields `symmetric == false`, `a...b`
/// yields `symmetric == true`; empty sides are rejected.
fn split_range(range: &str, what: &str) -> napi::Result<(String, String, bool)> {
    let invalid = |detail: &str| err_str(what, format!("invalid range {range:?} ({detail})"));
    let (from, to) = range
        .split_once("..")
        .ok_or_else(|| invalid("expected \"<from>..<to>\" or \"<from>...<to>\""))?;
    let (symmetric, to) = match to.strip_prefix('.') {
        Some(rest) => (true, rest),
        None => (false, to),
    };
    if to.starts_with('.') {
        return Err(invalid("too many dots"));
    }
    if from.is_empty() || to.is_empty() {
        return Err(invalid("empty side"));
    }
    Ok((from.to_string(), to.to_string(), symmetric))
}

/// Resolve a `push_glob`-style reference glob into tip object ids: an implied `refs/`
/// prefix and `/*` suffix follow git's rules, and matching references contribute their
/// target ids (tags pointing at other objects are peeled by the walk itself).
fn resolve_glob(
    inner: &Arc<gix::ThreadSafeRepository>,
    glob: &str,
) -> napi::Result<Vec<gix::ObjectId>> {
    let r = thread_local(inner);
    let mut pattern = glob.trim().to_string();
    if !pattern.contains(['?', '*', '[']) {
        pattern.push_str("/*");
    }
    if !pattern.starts_with("refs/") {
        pattern = format!("refs/{pattern}");
    }
    let platform = r.references().map_err(|e| err("revwalk glob", e))?;
    let iter = platform.all().map_err(|e| err("revwalk glob", e))?;
    let pattern_b = gix::bstr::BStr::new(pattern.as_bytes());
    let mode = gix::glob::wildmatch::Mode::empty();
    let mut out = Vec::new();
    for reference in iter {
        let mut reference = reference.map_err(|e| err("revwalk glob ref", e))?;
        let name = reference.name().to_string();
        if gix::glob::wildmatch(pattern_b, gix::bstr::BStr::new(name.as_bytes()), mode) {
            // Peel the reference to a commit (tags pointing at other objects peel to the
            // commit), matching the behaviour of the walk's tip resolution.
            let id = reference
                .peel_to_id()
                .map_err(|e| err("revwalk glob", e))?
                .detach();
            out.push(id);
        }
    }
    Ok(out)
}

#[napi]
impl Repository {
    /// Walk reachable commits from the given tips, with optional sort/reverse/first-parent
    /// control, exclusions, reference globs, and ranges.
    ///
    /// Tips and every spec in `exclude`/`range` accept anything
    /// [`rev_parse_single`](Self::rev_parse_single) does — object ids, `HEAD`, branch or
    /// tag names, `~`/`^`/`^{}` expressions — see [`WalkOptions`] for the rest.
    #[napi]
    pub fn rev_walk(
        &self,
        tips: Vec<String>,
        options: Option<WalkOptions>,
    ) -> napi::Result<Vec<String>> {
        let what = "rev_walk";
        let r = thread_local(&self.inner);
        let resolve = |spec: &str| -> napi::Result<gix::ObjectId> {
            r.rev_parse_single(spec)
                .map(|id| id.detach())
                .map_err(|e| err(what, e))
        };
        let mut tip_ids = tips
            .iter()
            .map(|t| resolve(t))
            .collect::<Result<Vec<_>>>()?;
        let mut hidden: Vec<gix::ObjectId> = Vec::new();
        let mut reverse = false;
        let mut first_parent = false;
        let mut sorting: Option<gix::revision::walk::Sorting> = None;
        let mut limit: Option<usize> = None;

        if let Some(opts) = options {
            for spec in opts.exclude.into_iter().flatten() {
                hidden.push(resolve(&spec)?);
            }
            if let Some(glob) = opts.glob.as_deref() {
                tip_ids.extend(resolve_glob(&self.inner, glob)?);
            }
            if let Some(range) = opts.range.as_deref() {
                let (from, to, symmetric) = split_range(range, what)?;
                let from = resolve(&from)?;
                let to = resolve(&to)?;
                tip_ids.push(from);
                tip_ids.push(to);
                if symmetric {
                    // `a...b`: commits reachable from either side but not from both —
                    // hide the merge base, like libgit2's push_range.
                    match r.merge_base(from, to).map_err(|e| err(what, e))? {
                        Some(base) => hidden.push(base.detach()),
                        None => {
                            return Err(err_str(
                                what,
                                format!("no merge base between {from} and {to}"),
                            ));
                        }
                    }
                } else {
                    hidden.push(from);
                }
            }
            if let Some(sort) = opts.sort.as_deref() {
                sorting = Some(match sort {
                    "breadth-first" => gix::revision::walk::Sorting::BreadthFirst,
                    "time-newest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
                    ),
                    "time-oldest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::OldestFirst,
                    ),
                    other => {
                        return Err(err_str(
                            what,
                            format!(
                                "invalid sort {other:?} (expected \"breadth-first\", \
                                 \"time-newest\" or \"time-oldest\")"
                            ),
                        ));
                    }
                });
            }
            first_parent = opts.first_parent.unwrap_or(false);
            reverse = opts.reverse.unwrap_or(false);
            limit = opts.limit.map(|l| l as usize);
        }

        let mut platform = r.rev_walk(tip_ids);
        if first_parent {
            platform = platform.first_parent_only();
        }
        if let Some(sorting) = sorting {
            platform = platform.sorting(sorting);
        }
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden);
        }
        let mut ids: Vec<String> = platform
            .all()
            .map_err(|e| err(what, e))?
            .map(|id| id.map(|i| i.id.to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| err("rev_walk item", e))?;
        if let Some(n) = limit {
            ids.truncate(n);
        }
        if reverse {
            ids.reverse();
        }
        Ok(ids)
    }

    /// Create a [`Revwalk`] for incremental, configurable traversal of the commit graph.
    ///
    /// Unlike [`rev_walk`](Self::rev_walk), which returns the whole list at once, a
    /// `Revwalk` lets you `push`/`hide` tips, set the sorting, and pull commits one at a
    /// time via [`next`](Revwalk::next) (or all at once via [`to_array`](Revwalk::to_array)).
    /// Backed by gix's revision walk; the libgit2-style hide callback is emulated by
    /// pre-resolving hidden ids when [`with_hide_callback`](Revwalk::with_hide_callback)
    /// is called.
    #[napi]
    pub fn revwalk(&self) -> Revwalk {
        Revwalk::new(Arc::clone(&self.inner))
    }

    /// Resolve a `push_glob`-style reference glob into tip object ids: an implied `refs/`
    /// prefix and `/*` suffix follow git's rules, and matching references contribute their
    /// target ids (tags pointing at other objects are peeled by the walk itself).
    fn glob_tips(&self, glob: &str) -> napi::Result<Vec<gix::ObjectId>> {
        resolve_glob(&self.inner, glob)
    }
}

/// A revwalk allows traversal of the commit graph defined by including one or more
/// leaves (via `push`/`push_head`/...) and excluding one or more roots (via
/// `hide`/`hide_head`/...).
///
/// This is a gix-backed mirror of libgit2's `git_revwalk`. Configuration is accumulated
/// (push/hide/sort) and the walk is materialised lazily on the first call to
/// [`next`](Self::next) or [`to_array`](Self::to_array).
#[napi]
pub struct Revwalk {
    inner: Arc<gix::ThreadSafeRepository>,
    tips: Vec<gix::ObjectId>,
    hidden: Vec<gix::ObjectId>,
    sorting: Option<gix::revision::walk::Sorting>,
    first_parent: bool,
    reverse: bool,
    /// Materialised walk results, filled lazily and invalidated on reconfiguration.
    buffer: Option<Vec<String>>,
    pos: usize,
}

#[napi]
impl Revwalk {
    fn new(inner: Arc<gix::ThreadSafeRepository>) -> Self {
        Revwalk {
            inner,
            tips: Vec::new(),
            hidden: Vec::new(),
            sorting: None,
            first_parent: false,
            reverse: false,
            buffer: None,
            pos: 0,
        }
    }

    /// Resolve a revision spec (oid, `HEAD`, branch/tag name, `~`/`^`/`^{}`) to an object id.
    fn resolve(&self, spec: &str) -> napi::Result<gix::ObjectId> {
        thread_local(&self.inner)
            .rev_parse_single(spec)
            .map(|id| id.detach())
            .map_err(|e| err("revwalk", e))
    }

    /// Drop any materialised walk so the next access rebuilds it from the current config.
    fn invalidate(&mut self) {
        self.buffer = None;
        self.pos = 0;
    }

    /// Materialise the walk into `buffer` if not already done.
    fn ensure_buffer(&mut self) -> napi::Result<()> {
        if self.buffer.is_some() {
            return Ok(());
        }
        let ids = {
            let r = thread_local(&self.inner);
            let mut platform = r.rev_walk(self.tips.clone());
            if self.first_parent {
                platform = platform.first_parent_only();
            }
            if let Some(sorting) = self.sorting {
                platform = platform.sorting(sorting);
            }
            if !self.hidden.is_empty() {
                platform = platform.with_hidden(self.hidden.clone());
            }
            let iter = platform.all().map_err(|e| err("revwalk", e))?;
            let mut ids: Vec<String> = iter
                .map(|id| id.map(|i| i.id.to_string()))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| err("revwalk item", e))?;
            if self.reverse {
                ids.reverse();
            }
            ids
        };
        self.buffer = Some(ids);
        self.pos = 0;
        Ok(())
    }

    /// Reset a revwalk to allow re-configuring it. The accumulated tips/hidden/sorting are
    /// preserved so that the next iteration re-runs the same walk; call `push`/`hide` again
    /// to extend it.
    #[napi]
    pub fn reset(&mut self) -> napi::Result<()> {
        self.buffer = None;
        self.pos = 0;
        Ok(())
    }

    /// Set the order in which commits are visited. Accepts a list of [`RevwalkSort`] flags
    /// (e.g. `["Time", "Reverse"]`). Replaces the previous sorting.
    #[napi]
    pub fn set_sorting(&mut self, sort: Vec<RevwalkSort>) -> napi::Result<()> {
        let mut sorting = None;
        let mut reverse = false;
        for s in &sort {
            match s {
                RevwalkSort::None => {}
                RevwalkSort::Topological => {
                    sorting = Some(gix::revision::walk::Sorting::BreadthFirst)
                }
                RevwalkSort::Time => {
                    sorting = Some(gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
                    ))
                }
                RevwalkSort::Reverse => reverse = true,
            }
        }
        self.sorting = sorting;
        self.reverse = reverse;
        self.invalidate();
        Ok(())
    }

    /// Simplify the history by first-parent: no parents other than the first for each commit
    /// will be enqueued.
    #[napi]
    pub fn simplify_first_parent(&mut self) -> napi::Result<()> {
        self.first_parent = true;
        self.invalidate();
        Ok(())
    }

    /// Mark a commit to start traversal from. The spec must resolve to a commitish.
    #[napi]
    pub fn push(&mut self, spec: String) -> napi::Result<()> {
        let oid = self.resolve(&spec)?;
        self.tips.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Push the repository's `HEAD`.
    #[napi]
    pub fn push_head(&mut self) -> napi::Result<()> {
        let oid = self.resolve("HEAD")?;
        self.tips.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Push the OIDs pointed to by references matching the given glob (implied `refs/`
    /// prefix and `/*` suffix follow git's rules).
    #[napi]
    pub fn push_glob(&mut self, glob: String) -> napi::Result<()> {
        let ids = resolve_glob(&self.inner, &glob)?;
        self.tips.extend(ids);
        self.invalidate();
        Ok(())
    }

    /// Push and hide the respective endpoints of the given range. `<from>..<to>` pushes both
    /// and hides `from`; the symmetric `<from>...<to>` pushes both and hides their merge
    /// base. The left-hand commit is thus hidden and the right-hand commit pushed.
    #[napi]
    pub fn push_range(&mut self, range: String) -> napi::Result<()> {
        self.apply_range(&range)
    }

    /// Push the OID pointed to by a reference (e.g. `refs/heads/main`, `HEAD`, `main`).
    #[napi]
    pub fn push_ref(&mut self, reference: String) -> napi::Result<()> {
        let oid = self.resolve(&reference)?;
        self.tips.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Mark a commit as not of interest to this revwalk.
    #[napi]
    pub fn hide(&mut self, spec: String) -> napi::Result<()> {
        let oid = self.resolve(&spec)?;
        self.hidden.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Hide all commits for which the callback returns `true` from the walk.
    ///
    /// gix has no runtime hide callback, so this is emulated: the current tips are walked
    /// once and the callback is consulted per commit to pre-resolve the hidden ids (which
    /// are then merged into the `hide` set). Call this after pushing your tips.
    #[napi]
    pub fn with_hide_callback(&mut self, callback: Function<'_, String, bool>) -> napi::Result<()> {
        let hidden = {
            let r = thread_local(&self.inner);
            let mut platform = r.rev_walk(self.tips.clone());
            if self.first_parent {
                platform = platform.first_parent_only();
            }
            if let Some(sorting) = self.sorting {
                platform = platform.sorting(sorting);
            }
            let iter = platform.all().map_err(|e| err("revwalk hide_cb", e))?;
            let mut hidden = Vec::new();
            for info in iter {
                let info = info.map_err(|e| err("revwalk hide_cb item", e))?;
                let oid = info.id.to_string();
                if callback.call(oid)? {
                    hidden.push(info.id);
                }
            }
            hidden
        };
        self.hidden.extend(hidden);
        self.invalidate();
        Ok(())
    }

    /// Hide the repository's `HEAD`.
    #[napi]
    pub fn hide_head(&mut self) -> napi::Result<()> {
        let oid = self.resolve("HEAD")?;
        self.hidden.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Hide references matching the given glob (implied `refs/` prefix and `/*` suffix follow
    /// git's rules); their ancestors are removed from the output.
    #[napi]
    pub fn hide_glob(&mut self, glob: String) -> napi::Result<()> {
        let ids = resolve_glob(&self.inner, &glob)?;
        self.hidden.extend(ids);
        self.invalidate();
        Ok(())
    }

    /// Hide the OID pointed to by a reference (e.g. `refs/heads/main`, `HEAD`, `main`).
    #[napi]
    pub fn hide_ref(&mut self, reference: String) -> napi::Result<()> {
        let oid = self.resolve(&reference)?;
        self.hidden.push(oid);
        self.invalidate();
        Ok(())
    }

    /// Return the next commit id in the walk, or `null` when the walk is exhausted.
    #[napi]
    pub fn next(&mut self) -> napi::Result<Option<String>> {
        self.ensure_buffer()?;
        let buf = self.buffer.as_ref().unwrap();
        if self.pos < buf.len() {
            let id = buf[self.pos].clone();
            self.pos += 1;
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Return all remaining commit ids in the walk (does not advance a subsequent `next`).
    #[napi]
    pub fn to_array(&mut self) -> napi::Result<Vec<String>> {
        self.ensure_buffer()?;
        Ok(self.buffer.clone().unwrap())
    }

    /// Push and hide the endpoints of a range spec, per [`push_range`](Self::push_range).
    fn apply_range(&mut self, range: &str) -> napi::Result<()> {
        let (from_s, to_s, symmetric) = split_range(range, "revwalk")?;
        let from = self.resolve(&from_s)?;
        let to = self.resolve(&to_s)?;
        self.tips.push(from);
        self.tips.push(to);
        if symmetric {
            let base = {
                let r = thread_local(&self.inner);
                r.merge_base(from, to)
                    .map_err(|e| err("revwalk", e))?
                    .map(|b| b.detach())
            };
            match base {
                Some(b) => self.hidden.push(b),
                None => {
                    return Err(err_str(
                        "revwalk",
                        format!("no merge base between {from} and {to}"),
                    ));
                }
            }
        } else {
            self.hidden.push(from);
        }
        self.invalidate();
        Ok(())
    }
}
