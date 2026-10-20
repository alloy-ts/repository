use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// A set of pathspecs usable for matching repository paths.
///
/// Build one with [`Repository::pathspec()`] and then test paths against it with
/// [`is_included()`](Self::is_included) or [`matches()`](Self::matches).
///
/// Note: patterns that reference attributes (via the `:(attr:…)` magic syntax) are parsed,
/// but matching such patterns requires an attributes source and is not supported.
#[napi]
pub struct Pathspec {
    pub(crate) inner: gix::PathspecDetached,
}

/// The resolved pathspec defaults derived from configuration and environment.
#[napi(object)]
pub struct PathspecDefaults {
    /// Default magic signature bitflags (e.g. `ICASE`), Debug-formatted.
    pub signature: String,
    /// Default search mode (`Literal`, `ShellGlob`, `PathAwareGlob`).
    pub search_mode: String,
    /// Whether pathspecs are taken literally (`GIT_LITERAL_PATHSPECS`).
    pub literal: bool,
}

#[napi]
impl Pathspec {
    /// Whether `path` is included by these pathspecs (i.e. matches and is not excluded).
    ///
    /// `is_dir` should be `true` when `path` refers to a directory, or `null` when unknown.
    #[napi]
    pub fn is_included(&mut self, path: String, is_dir: Option<bool>) -> bool {
        self.inner
            .is_included(gix::bstr::BStr::new(path.as_bytes()), is_dir)
    }

    /// Whether `path` matches these pathspecs at all (included *or* explicitly excluded).
    ///
    /// `is_dir` should be `true` when `path` refers to a directory, or `null` when unknown.
    #[napi]
    pub fn matches(&mut self, path: String, is_dir: Option<bool>) -> bool {
        self.inner
            .pattern_matching_relative_path(gix::bstr::BStr::new(path.as_bytes()), is_dir)
            .is_some()
    }
}

#[napi]
impl Repository {
    /// Create a pathspec from `patterns` that can match repository paths.
    ///
    /// `empty_patterns_match_prefix` confines empty patterns to the current prefix instead of
    /// letting them match everything (defaults to `false`). `inherit_ignore_case` makes patterns
    /// case-insensitive on case-insensitive filesystems, which is appropriate when matching
    /// files on disk (defaults to `true`).
    ///
    /// Patterns support git's pathspec magic, e.g. `src/**`, `:!build`, `:(glob)docs/*`.
    #[napi]
    pub fn pathspec(
        &self,
        patterns: Vec<String>,
        empty_patterns_match_prefix: Option<bool>,
        inherit_ignore_case: Option<bool>,
    ) -> Result<Pathspec> {
        let r = thread_local(&self.inner);
        // An index is only required when patterns reference attributes (`:(attr:…)`), but the
        // API needs one, so load it or synthesize it from HEAD / empty.
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let state: &gix::index::State = &index;
        let ps = r
            .pathspec(
                empty_patterns_match_prefix.unwrap_or(false),
                patterns.iter().map(|p| gix::bstr::BStr::new(p.as_bytes())),
                inherit_ignore_case.unwrap_or(true),
                state,
                Default::default(),
            )
            .map_err(|e| err("pathspec", e))?;
        let detached = ps.detach().map_err(|e| err("pathspec.detach", e))?;
        Ok(Pathspec { inner: detached })
    }

    /// The pathspec defaults resolved from configuration and environment.
    #[napi]
    pub fn pathspec_defaults(&self) -> Result<PathspecDefaults> {
        let r = thread_local(&self.inner);
        let d = r
            .pathspec_defaults()
            .map_err(|e| err("pathspec_defaults", e))?;
        Ok(PathspecDefaults {
            signature: format!("{:?}", d.signature),
            search_mode: format!("{:?}", d.search_mode),
            literal: d.literal,
        })
    }

    /// Like `pathspecDefaults`, but with control over case-insensitivity inheritance.
    #[napi]
    pub fn pathspec_defaults_inherit_ignore_case(
        &self,
        inherit_icase: bool,
    ) -> Result<PathspecDefaults> {
        let r = thread_local(&self.inner);
        let d = r
            .pathspec_defaults_inherit_ignore_case(inherit_icase)
            .map_err(|e| err("pathspec_defaults_inherit_ignore_case", e))?;
        Ok(PathspecDefaults {
            signature: format!("{:?}", d.signature),
            search_mode: format!("{:?}", d.search_mode),
            literal: d.literal,
        })
    }
}
