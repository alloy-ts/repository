use napi_derive::napi;

use crate::error::{err, thread_local};
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
}
