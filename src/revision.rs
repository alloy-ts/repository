use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::{Object, build_object};
use crate::repository::Repository;

/// A parsed revision specification (result of [`Repository::rev_parse`]).
#[napi(object)]
pub struct RevSpec {
    /// The kind of specification (`single`, `exclude`, `range`, `merge`, `include-parents`, `exclude-parents`).
    pub kind: String,
    /// The primary resolved object id (the `to` side for ranges, `ours` for merges).
    pub id: Option<String>,
    /// The `from` side for ranges, or `theirs` for merges.
    pub from: Option<String>,
    /// The `to` side for ranges, or `ours` for merges.
    pub to: Option<String>,
}

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

/// The result of [`Repository::rev_parse_ext`].
#[napi(object)]
pub struct RevParseExt {
    /// The resolved object id.
    pub id: String,
    /// The full name of the reference that provided the object — `refs/heads/main` for
    /// `main`, `HEAD` for `HEAD` — or `null` when the spec did not directly name one
    /// (e.g. `HEAD~1`, a hex id). Pass it to `findReference` for a reference handle.
    pub reference_name: Option<String>,
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

fn rev_spec_to_info(spec: &gix::revision::Spec) -> RevSpec {
    use gix::revision::plumbing::Spec as Raw;
    let raw: &Raw = &**spec;
    let (kind, from, to) = match raw {
        Raw::Include(id) => ("single".to_string(), None, Some(id.to_string())),
        Raw::Exclude(id) => ("exclude".to_string(), None, Some(id.to_string())),
        Raw::Range { from, to } => (
            "range".to_string(),
            Some(from.to_string()),
            Some(to.to_string()),
        ),
        Raw::Merge { theirs, ours } => (
            "merge".to_string(),
            Some(theirs.to_string()),
            Some(ours.to_string()),
        ),
        Raw::IncludeOnlyParents(id) => ("include-parents".to_string(), None, Some(id.to_string())),
        Raw::ExcludeParents(id) => ("exclude-parents".to_string(), None, Some(id.to_string())),
    };
    RevSpec {
        kind,
        id: to.clone(),
        from,
        to,
    }
}

#[napi]
impl Repository {
    /// Resolve a single revision specification (e.g. `HEAD~1`, `main`, `<sha>`) to an id.
    #[napi]
    pub fn rev_parse_single(&self, spec: String) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_single", e))?;
        Ok(id.to_string())
    }

    /// Resolve revision specification to hex object ID string.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> napi::Result<String> {
        self.rev_parse_single(spec)
    }

    /// Parse a revision specification and return its kind and resolved id(s).
    ///
    /// Unlike [`rev_parse_single`](Self::rev_parse_single), this distinguishes ranges (`a..b`),
    /// merges (`a...b`) and exclusions (`^a`).
    #[napi]
    pub fn rev_parse(&self, spec: String) -> napi::Result<RevSpec> {
        let r = thread_local(&self.inner);
        let parsed = r
            .rev_parse(spec.as_str())
            .map_err(|e| err("rev_parse", e))?;
        Ok(rev_spec_to_info(&parsed))
    }

    /// Resolve a single revision specification and return its id along with the reference
    /// that provided it, mirroring libgit2's `git_revparse_ext`.
    ///
    /// The reference is looked up by the spec's own name, with short names expanded the way
    /// git does (`main` → `refs/heads/main`, `v1` → `refs/tags/v1`, `origin/main` →
    /// `refs/remotes/origin/main`). Specs that are not a reference name (`HEAD~1`, a hex
    /// id) yield a `null` reference.
    #[napi]
    pub fn rev_parse_ext(&self, spec: String) -> napi::Result<RevParseExt> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_ext", e))?;
        let reference_name = r
            .try_find_reference(spec.as_str())
            .map_err(|e| err("rev_parse_ext", e))?
            .map(|reference| String::from_utf8_lossy(reference.name().as_bstr()).into_owned());
        Ok(RevParseExt {
            id: id.to_string(),
            reference_name,
        })
    }

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
                tip_ids.extend(self.glob_tips(glob)?);
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

    /// Resolve a single revision specification (e.g. `HEAD~1`, `main`, `<sha>`)
    /// directly to an [`Object`], in one step. This is the object-returning
    /// counterpart of [`rev_parse_single`](Self::rev_parse_single) and avoids a
    /// follow-up `findObject` call.
    #[napi]
    pub fn resolve(&self, spec: String) -> napi::Result<Object> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("resolve", e))?;
        build_object(&self.inner, id.detach())
    }

    /// Compute the best merge-base of two revisions (like `git merge-base <one> <two>`).
    ///
    /// Both arguments accept anything [`rev_parse_single`](Self::rev_parse_single) does —
    /// commit ids, `HEAD`, branch or tag names, `~`/`^`/`^{}` expressions. Returns `null`
    /// when the two revisions have no common ancestor.
    /// Resolve a `push_glob`-style reference glob into tip object ids: an implied `refs/`
    /// prefix and `/*` suffix follow git's rules, and matching references contribute their
    /// target ids (tags pointing at other objects are peeled by the walk itself).
    fn glob_tips(&self, glob: &str) -> napi::Result<Vec<gix::ObjectId>> {
        let mut pattern = glob.trim().to_string();
        if !pattern.contains(['?', '*', '[']) {
            pattern.push_str("/*");
        }
        if !pattern.starts_with("refs/") {
            pattern = format!("refs/{pattern}");
        }
        let references = self.references_glob(pattern)?;
        references
            .iter()
            .map(|reference| parse_oid(&reference.target()))
            .collect()
    }
}
