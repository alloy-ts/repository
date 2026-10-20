use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
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
    /// Sort order: `breadth-first` (default), `time-newest`, or `time-oldest`.
    pub sort: Option<String>,
    /// Reverse the resulting commit list.
    pub reverse: Option<bool>,
    /// Traverse only the first parent of each commit.
    pub first_parent: Option<bool>,
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
    pub fn rev_parse_single(&self, spec: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_single", e))?;
        Ok(id.to_string())
    }

    /// Resolve revision specification to hex object ID string.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
    }

    /// Parse a revision specification and return its kind and resolved id(s).
    ///
    /// Unlike [`rev_parse_single`](Self::rev_parse_single), this distinguishes ranges (`a..b`),
    /// merges (`a...b`) and exclusions (`^a`).
    #[napi]
    pub fn rev_parse(&self, spec: String) -> Result<RevSpec> {
        let r = thread_local(&self.inner);
        let parsed = r
            .rev_parse(spec.as_str())
            .map_err(|e| err("rev_parse", e))?;
        Ok(rev_spec_to_info(&parsed))
    }

    /// Walk reachable commits from the given tip ids, with optional sort/reverse/first-parent control.
    #[napi]
    pub fn rev_walk(&self, tips: Vec<String>, options: Option<WalkOptions>) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let tip_ids: Vec<gix::ObjectId> = tips
            .iter()
            .map(|t| parse_oid(t))
            .collect::<Result<Vec<_>>>()?;
        let mut platform = r.rev_walk(tip_ids);
        let mut reverse = false;
        if let Some(opts) = options {
            if opts.first_parent.unwrap_or(false) {
                platform = platform.first_parent_only();
            }
            if let Some(sort) = opts.sort.as_deref() {
                let sorting = match sort {
                    "time-newest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
                    ),
                    "time-oldest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::OldestFirst,
                    ),
                    _ => gix::revision::walk::Sorting::BreadthFirst,
                };
                platform = platform.sorting(sorting);
            }
            reverse = opts.reverse.unwrap_or(false);
        }
        let mut ids: Vec<String> = platform
            .all()
            .map_err(|e| err("rev_walk", e))?
            .map(|id| id.map(|i| i.id.to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| err("rev_walk item", e))?;
        if reverse {
            ids.reverse();
        }
        Ok(ids)
    }
}
