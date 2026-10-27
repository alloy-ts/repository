use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use gix::attrs::{Assignment, State};
use gix::bstr::BStr;
use gix::pathspec::{Defaults, MagicSignature, Pattern, SearchMode};

use crate::error::{err, err_str, thread_local};
use crate::index::entry_to_dto;
use crate::repository::{IndexEntry, Repository};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The active magic signatures of `signature`, in canonical order.
fn magic_names(signature: MagicSignature) -> Vec<String> {
    [
        (MagicSignature::TOP, "top"),
        (MagicSignature::ICASE, "icase"),
        (MagicSignature::EXCLUDE, "exclude"),
        (MagicSignature::MUST_BE_DIR, "mustBeDir"),
    ]
    .into_iter()
    .filter(|(flag, _)| signature.contains(*flag))
    .map(|(_, name)| name.to_string())
    .collect()
}

fn search_mode_name(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Literal => "Literal",
        SearchMode::ShellGlob => "ShellGlob",
        SearchMode::PathAwareGlob => "PathAwareGlob",
    }
}

/// A parsed pathspec pattern with its magic resolved.
#[napi(object)]
pub struct PathspecPattern {
    /// The original pattern text, e.g. `:(glob)src/*.ts`.
    pub source: String,
    /// The pattern path without magic, e.g. `src/*.ts`.
    pub path: String,
    /// The prefix (current-directory) portion of `path`, which is always a directory,
    /// or `""` when there is none.
    pub prefix_directory: String,
    /// Active magic signatures: a subset of `top`, `icase`, `exclude`, `mustBeDir`.
    pub magic: Vec<String>,
    /// How the pattern is matched: `Literal`, `ShellGlob` or `PathAwareGlob`.
    pub search_mode: String,
    /// Whether the pattern excludes matching paths (`:!…` / `:(exclude)…`).
    pub is_excluded: bool,
    /// Whether this is the nil pattern (`:`), which matches everything.
    pub is_nil: bool,
    /// Attributes the pattern requires (`:(attr:…)`), as `name`, `-name`, `name=value` or `!name`.
    pub attributes: Vec<String>,
}

fn pattern_to_dto(p: &Pattern) -> PathspecPattern {
    PathspecPattern {
        source: p.to_bstring().to_string(),
        path: lossy(p.path()),
        prefix_directory: lossy(p.prefix_directory()),
        magic: magic_names(p.signature),
        search_mode: search_mode_name(p.search_mode).to_string(),
        is_excluded: p.is_excluded(),
        is_nil: p.is_nil(),
        attributes: p.attributes.iter().map(assignment_to_string).collect(),
    }
}

/// Render an attribute assignment like git's pathspec syntax (`attr`, `-attr`, `attr=value`).
fn assignment_to_string(a: &Assignment) -> String {
    let name = a.name.as_str();
    match &a.state {
        State::Set => name.to_string(),
        State::Unset => format!("-{name}"),
        State::Value(v) => format!("{name}={}", lossy(v.as_ref().as_bstr())),
        State::Unspecified => format!("!{name}"),
    }
}

/// How a path related to the pathspec set.
#[napi(object)]
pub struct PathspecMatch {
    /// Whether the path is included: matched *and* not excluded.
    pub included: bool,
    /// Whether the path matched a pattern that excludes it.
    pub excluded: bool,
    /// How the pattern matched: `Always` (matches everything), `Prefix` (directory prefix),
    /// `WildcardMatch` (glob) or `Verbatim` (exact path).
    pub kind: String,
    /// The sequence number of the matching pattern, 0-based in the order patterns were given.
    pub pattern_index: u32,
    /// The spec text of the matching pattern.
    pub pattern: String,
}

fn match_to_dto(m: gix::pathspec::search::Match<'_>) -> PathspecMatch {
    let kind = match m.kind {
        gix::pathspec::search::MatchKind::Always => "Always",
        gix::pathspec::search::MatchKind::Prefix => "Prefix",
        gix::pathspec::search::MatchKind::WildcardMatch => "WildcardMatch",
        gix::pathspec::search::MatchKind::Verbatim => "Verbatim",
    };
    PathspecMatch {
        included: !m.is_excluded(),
        excluded: m.is_excluded(),
        kind: kind.to_string(),
        pattern_index: m.sequence_number as u32,
        pattern: m.pattern.to_bstring().to_string(),
    }
}

/// The resolved pathspec defaults derived from configuration and environment.
#[napi(object)]
pub struct PathspecDefaults {
    /// Whether patterns match case-insensitively by default (the `ICASE` magic).
    pub ignore_case: bool,
    /// Default search mode (`Literal`, `ShellGlob`, `PathAwareGlob`).
    pub search_mode: String,
    /// Whether pathspecs are taken literally (`GIT_LITERAL_PATHSPECS`).
    pub literal: bool,
    /// The default magic signatures: a subset of `top`, `icase`, `exclude`, `mustBeDir`.
    pub magic: Vec<String>,
}

/// Options for [`parse_pathspec`] standing in for repository-derived defaults.
#[napi(object)]
pub struct PathspecParseOptions {
    /// Search mode used when the pattern carries no glob magic
    /// (`"Literal"`, `"ShellGlob"` (default), `"PathAwareGlob"`).
    pub search_mode: Option<String>,
    /// Match case-insensitively by default, like `:(icase)`.
    pub ignore_case: Option<bool>,
    /// Take the pattern literally, like `GIT_LITERAL_PATHSPECS` (implies `Literal`).
    pub literal: Option<bool>,
}

fn parse_search_mode(mode: &str) -> napi::Result<SearchMode> {
    Ok(match mode {
        "Literal" | "literal" => SearchMode::Literal,
        "ShellGlob" | "shell_glob" | "shellGlob" => SearchMode::ShellGlob,
        "PathAwareGlob" | "path_aware_glob" | "pathAwareGlob" => SearchMode::PathAwareGlob,
        other => {
            return Err(err_str(
                "search_mode",
                format!(
                    "unknown search mode '{other}'; expected Literal, ShellGlob or PathAwareGlob"
                ),
            ));
        }
    })
}

fn parse_options_to_defaults(options: Option<PathspecParseOptions>) -> napi::Result<Defaults> {
    let options = options.unwrap_or(PathspecParseOptions {
        search_mode: None,
        ignore_case: None,
        literal: None,
    });
    let mut signature = MagicSignature::empty();
    if options.ignore_case.unwrap_or(false) {
        signature |= MagicSignature::ICASE;
    }
    let mut search_mode = match options.search_mode.as_deref() {
        Some(mode) => parse_search_mode(mode)?,
        None => SearchMode::default(),
    };
    let literal = options.literal.unwrap_or(false);
    if literal {
        search_mode = SearchMode::Literal;
    }
    Ok(Defaults {
        signature,
        search_mode,
        literal,
    })
}

/// Parse a single pathspec pattern (e.g. `:(glob)src/*.ts`) without a repository, exposing
/// its resolved magic, search mode and attributes.
///
/// `options` provides the defaults a repository would normally contribute; unspecified
/// values behave like a plain git invocation (`ShellGlob`, case-sensitive).
#[napi]
pub fn parse_pathspec(
    pattern: String,
    options: Option<PathspecParseOptions>,
) -> napi::Result<PathspecPattern> {
    let defaults = parse_options_to_defaults(options)?;
    let p = gix::pathspec::Pattern::from_bytes(pattern.as_bytes(), defaults)
        .map_err(|e| err("parse_pathspec", e))?;
    Ok(pattern_to_dto(&p))
}

/// Whether `pattern` is a syntactically valid pathspec (see [`parse_pathspec`]).
#[napi]
pub fn is_pathspec_valid(
    pattern: String,
    options: Option<PathspecParseOptions>,
) -> napi::Result<bool> {
    let defaults = parse_options_to_defaults(options)?;
    Ok(gix::pathspec::Pattern::from_bytes(pattern.as_bytes(), defaults).is_ok())
}

/// A set of pathspecs usable for matching repository paths.
///
/// Build one with [`Repository::pathspec()`], test paths against it with
/// [`is_included()`](Self::is_included), [`matches()`](Self::matches) or
/// [`match()`](Self::match_path), and list index entries with [`match_index()`](Self::match_index).
///
/// Note: patterns that reference attributes (via the `:(attr:…)` magic syntax) are parsed,
/// but matching such patterns requires an attributes source and is not supported.
#[napi]
pub struct Pathspec {
    repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) inner: gix::PathspecDetached,
}

#[napi]
impl Pathspec {
    /// Whether `path` is included by these pathspecs (i.e. matches and is not excluded).
    ///
    /// `is_dir` should be `true` when `path` refers to a directory, or `null` when unknown.
    #[napi]
    pub fn is_included(&mut self, path: String, is_dir: Option<bool>) -> bool {
        self.inner.is_included(BStr::new(path.as_bytes()), is_dir)
    }

    /// Whether `path` matches these pathspecs at all (included *or* explicitly excluded).
    ///
    /// `is_dir` should be `true` when `path` refers to a directory, or `null` when unknown.
    #[napi]
    pub fn matches(&mut self, path: String, is_dir: Option<bool>) -> bool {
        self.inner
            .pattern_matching_relative_path(BStr::new(path.as_bytes()), is_dir)
            .is_some()
    }

    /// Describe how `path` relates to these pathspecs, or `null` when no pattern matches
    /// at all.
    ///
    /// `is_dir` should be `true` when `path` refers to a directory, or `null` when unknown.
    #[napi(js_name = "match")]
    pub fn match_path(&mut self, path: String, is_dir: Option<bool>) -> Option<PathspecMatch> {
        self.inner
            .pattern_matching_relative_path(BStr::new(path.as_bytes()), is_dir)
            .map(match_to_dto)
    }

    /// The parsed patterns that participate in matching, in the order they were given.
    #[napi]
    pub fn patterns(&self) -> Vec<PathspecPattern> {
        self.inner.search.patterns().map(pattern_to_dto).collect()
    }

    /// The portion of the pattern path shared by all positive (non-excluded) patterns, which
    /// is always matched case-sensitively. Useful to skip over large portions of input.
    #[napi]
    pub fn common_prefix(&self) -> String {
        lossy(self.inner.search.common_prefix())
    }

    /// The longest common directory of all patterns, or `null` if there is none. Useful for
    /// scoping directory walks; path separators are always `/`.
    #[napi]
    pub fn longest_common_directory(&self) -> napi::Result<Option<String>> {
        match self.inner.search.longest_common_directory() {
            Ok(Some(dir)) => Ok(Some(dir.to_string_lossy().replace('\\', "/"))),
            Ok(None) => Ok(None),
            Err(e) => Err(err("longest_common_directory", e)),
        }
    }

    /// All index entries whose paths are included by these pathspecs (like
    /// `git ls-files -- <pathspec>`).
    #[napi]
    pub fn match_index(&mut self) -> napi::Result<Vec<IndexEntry>> {
        let r = thread_local(&self.repo);
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        // Entries outside the common prefix of the positive patterns cannot match.
        let prefix = self.inner.search.common_prefix().to_vec();
        let Some(entries) = index.prefixed_entries(BStr::new(&prefix)) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries {
            let path = entry.path(&index);
            if self.inner.is_included(path, Some(false)) {
                out.push(entry_to_dto(&index, entry));
            }
        }
        Ok(out)
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
    ) -> napi::Result<Pathspec> {
        let r = thread_local(&self.inner);
        // An index is only required when patterns reference attributes (`:(attr:…)`), but the
        // API needs one, so load it or synthesize it from HEAD / empty.
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let state: &gix::index::State = &index;
        let ps = r
            .pathspec(
                empty_patterns_match_prefix.unwrap_or(false),
                patterns.iter().map(|p| BStr::new(p.as_bytes())),
                inherit_ignore_case.unwrap_or(true),
                state,
                Default::default(),
            )
            .map_err(|e| err("pathspec", e))?;
        let inner = ps.detach().map_err(|e| err("pathspec.detach", e))?;
        Ok(Pathspec {
            repo: self.inner.clone(),
            inner,
        })
    }

    /// The pathspec defaults resolved from configuration and environment.
    #[napi]
    pub fn pathspec_defaults(&self) -> napi::Result<PathspecDefaults> {
        let r = thread_local(&self.inner);
        let d = r
            .pathspec_defaults()
            .map_err(|e| err("pathspec_defaults", e))?;
        Ok(PathspecDefaults {
            ignore_case: d.signature.contains(MagicSignature::ICASE),
            search_mode: search_mode_name(d.search_mode).to_string(),
            literal: d.literal,
            magic: magic_names(d.signature),
        })
    }

    /// Like `pathspecDefaults`, but with control over case-insensitivity inheritance.
    #[napi]
    pub fn pathspec_defaults_inherit_ignore_case(
        &self,
        inherit_icase: bool,
    ) -> napi::Result<PathspecDefaults> {
        let r = thread_local(&self.inner);
        let d = r
            .pathspec_defaults_inherit_ignore_case(inherit_icase)
            .map_err(|e| err("pathspec_defaults_inherit_ignore_case", e))?;
        Ok(PathspecDefaults {
            ignore_case: d.signature.contains(MagicSignature::ICASE),
            search_mode: search_mode_name(d.search_mode).to_string(),
            literal: d.literal,
            magic: magic_names(d.signature),
        })
    }
}
