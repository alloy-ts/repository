use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::index::IndexEntryMode;
use crate::repository::Repository;

/// A single attribute assignment for a path, as resolved by git's attribute machinery.
#[derive(Clone)]
#[napi(object)]
pub struct AttributeEntry {
    /// The attribute name, e.g. `text`, `eol`, `merge`.
    pub name: String,
    /// The resolved state, rendered with `Debug`: `Set`, `Unspecified`, or `Value("…")`.
    ///
    /// Note: this is the raw git `State` representation. Use it as a tri-state rather than
    /// assuming a particular string format.
    pub value: String,
}

/// Where to read `.gitattributes` data from.
///
/// Mirrors `gix::worktree::stack::state::attributes::Source`. Defaults to
/// [`WorktreeThenIdMapping`](AttributeSource::WorktreeThenIdMapping) when not supplied, matching
/// the previous hardcoded behavior of `attributes` / `attributes_only`.
#[napi]
pub enum AttributeSource {
    /// No worktree checkout available (bare repo, during clone, or blobs from other history).
    IdMapping,
    /// Read from id mappings, falling back to the worktree (used when *checking out* files).
    IdMappingThenWorktree,
    /// Read from the worktree, falling back to id mappings (used when *checking in* files).
    WorktreeThenIdMapping,
}

/// Aggregate counts describing where the matched attributes came from.
///
/// Mirrors `gix::worktree::stack::state::attributes::Statistics`, which lives inside the
/// worktree `Stack` cache and is exposed here after a match is computed.
#[napi(object)]
pub struct AttributeStatistics {
    /// Number of patterns buffers read from the index (id mappings).
    pub patterns_buffers: u32,
    /// Number of `.gitattributes` files read from disk.
    pub pattern_files: u32,
    /// Number of `.gitattributes` files we attempted to find on disk.
    pub tried_pattern_files: u32,
}

/// The result of attributing a path, paired with the statistics gathered while doing so.
#[napi(object)]
pub struct AttributeResult {
    /// The resolved attribute assignments.
    pub entries: Vec<AttributeEntry>,
    /// Aggregate counts describing where the matched attributes came from.
    pub statistics: AttributeStatistics,
}

fn to_gix_source(source: AttributeSource) -> gix::worktree::stack::state::attributes::Source {
    match source {
        AttributeSource::IdMapping => gix::worktree::stack::state::attributes::Source::IdMapping,
        AttributeSource::IdMappingThenWorktree => {
            gix::worktree::stack::state::attributes::Source::IdMappingThenWorktree
        }
        AttributeSource::WorktreeThenIdMapping => {
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping
        }
    }
}

/// Resolve the optional source to a gix `Source`, defaulting to `WorktreeThenIdMapping`
/// (the previous hardcoded choice for these methods).
fn attribute_source(
    source: Option<AttributeSource>,
) -> gix::worktree::stack::state::attributes::Source {
    source
        .map(to_gix_source)
        .unwrap_or(gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping)
}

/// Resolve the optional source to a gix ignore `Source`. Unlike `attributes::Source`, the
/// `ignore::Source` enum only has `IdMapping` and `WorktreeThenIdMappingIfNotSkipped`, so the
/// worktree-based `AttributeSource` variants all collapse to the default.
fn ignore_source(source: Option<AttributeSource>) -> gix::worktree::stack::state::ignore::Source {
    match source {
        Some(AttributeSource::IdMapping) => gix::worktree::stack::state::ignore::Source::IdMapping,
        _ => gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    }
}

/// Run attribute matching for `relative_path` on `platform`, returning the resolved entries.
fn resolve_attributes(platform: &gix::worktree::stack::Platform<'_>) -> Vec<AttributeEntry> {
    let mut selected = gix::attrs::search::Outcome::default();
    platform.matching_attributes(&mut selected);
    selected
        .iter()
        .map(|m| AttributeEntry {
            name: m.assignment.name.as_str().to_string(),
            value: format!("{:?}", m.assignment.state),
        })
        .collect()
}

fn attribute_statistics(
    stats: gix::worktree::stack::state::attributes::Statistics,
) -> AttributeStatistics {
    AttributeStatistics {
        patterns_buffers: stats.patterns_buffers as u32,
        pattern_files: stats.pattern_files as u32,
        tried_pattern_files: stats.tried_pattern_files as u32,
    }
}

/// A reusable attribute/exclude cache for a repository, backed by an owned
/// [`gix_worktree::Stack`]. Reusing one instance across many path queries avoids
/// re-reading `.gitattributes`/`.gitignore` files for every call.
///
/// Build it with [`Repository::attributes_stack`], [`Repository::attributes_only_stack`], or
/// [`Repository::excludes_stack`]. The underlying [`gix::AttributeStack`] is *detached* into an
/// owned stack so it can outlive a single FFI call — the borrowed [`gix::AttributeStack`] and its
/// `Platform` (returned by `at_path`/`at_entry`) cannot cross the FFI boundary, which is why these
/// query methods run the whole `at_path` + `matching_attributes`/`is_excluded` sequence internally.
#[napi]
pub struct AttributeStack {
    inner: gix::worktree::Stack,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl AttributeStack {
    /// Resolve attributes for `relative_path`, reusing this stack's cache. Equivalent to
    /// [`Repository::attributes`](crate::Repository::attributes) but without rebuilding the cache.
    /// `mode` describes the entry kind at that path; omit it to treat `relative_path` as a file.
    #[napi]
    pub fn attributes(
        &mut self,
        relative_path: String,
        mode: Option<u32>,
    ) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.repo);
        let platform = self
            .inner
            .at_path(
                relative_path.as_str(),
                mode.and_then(|b| IndexEntryMode::from_bits(b).map(|m| m.inner)),
                &r.objects,
            )
            .map_err(|e| err("at_path", e))?;
        Ok(resolve_attributes(&platform))
    }

    /// Like [`attributes`](Self::attributes) but also returns the gathered [`AttributeStatistics`].
    #[napi]
    pub fn attributes_with_statistics(
        &mut self,
        relative_path: String,
        mode: Option<u32>,
    ) -> Result<AttributeResult> {
        let r = thread_local(&self.repo);
        let platform = self
            .inner
            .at_path(
                relative_path.as_str(),
                mode.and_then(|b| IndexEntryMode::from_bits(b).map(|m| m.inner)),
                &r.objects,
            )
            .map_err(|e| err("at_path", e))?;
        let entries = resolve_attributes(&platform);
        let statistics = attribute_statistics(self.inner.statistics().attributes);
        Ok(AttributeResult {
            entries,
            statistics,
        })
    }

    /// Whether `relative_path` is excluded by `.gitignore`/exclude files, reusing this stack's cache.
    /// Equivalent to [`Repository::is_path_excluded`](crate::Repository::is_path_excluded).
    #[napi]
    pub fn is_excluded(&mut self, relative_path: String) -> Result<bool> {
        let r = thread_local(&self.repo);
        let platform = self
            .inner
            .at_path(
                relative_path.as_str(),
                Some(gix::objs::tree::EntryKind::Blob.into()),
                &r.objects,
            )
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    /// Statistics gathered since this stack was created (or the last
    /// [`take_statistics`](Self::take_statistics)).
    #[napi]
    pub fn statistics(&self) -> AttributeStatistics {
        attribute_statistics(self.inner.statistics().attributes)
    }

    /// Consume the stack's statistics and return them, resetting the counters for the next batch.
    #[napi]
    pub fn take_statistics(&mut self) -> AttributeStatistics {
        attribute_statistics(self.inner.take_statistics().attributes)
    }
}

#[napi]
impl Repository {
    /// Check attributes for a relative `relative_path`.
    ///
    /// Consults both `.gitattributes` and the exclude/ignore sources (`.gitignore`,
    /// `.git/info/exclude`), so the returned set is what git would resolve for that path.
    ///
    /// `source` selects where to read `.gitattributes` from; omit it to use the default
    /// (`WorktreeThenIdMapping`).
    #[napi]
    pub fn attributes(
        &self,
        relative_path: String,
        source: Option<AttributeSource>,
    ) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes(
                &index,
                attribute_source(source),
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                None,
            )
            .map_err(|e| err("attributes", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        Ok(resolve_attributes(&platform))
    }

    /// Like [`attributes`](Self::attributes) but also returns the [`AttributeStatistics`]
    /// gathered while attributing the path.
    #[napi]
    pub fn attributes_with_statistics(
        &self,
        relative_path: String,
        source: Option<AttributeSource>,
    ) -> Result<AttributeResult> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes(
                &index,
                attribute_source(source),
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                None,
            )
            .map_err(|e| err("attributes", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let entries = resolve_attributes(&platform);
        let statistics = attribute_statistics(cache.statistics().attributes);
        Ok(AttributeResult {
            entries,
            statistics,
        })
    }

    /// Check attributes for a relative `relative_path`, without applying exclude/ignore information.
    ///
    /// Unlike [`attributes`](Self::attributes), `.gitignore`/`.git/info/exclude` files are
    /// not consulted, so this is the appropriate query when only `.gitattributes` matches
    /// are relevant.
    ///
    /// `source` selects where to read `.gitattributes` from; omit it to use the default
    /// (`WorktreeThenIdMapping`).
    #[napi]
    pub fn attributes_only(
        &self,
        relative_path: String,
        source: Option<AttributeSource>,
    ) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes_only(&index, attribute_source(source))
            .map_err(|e| err("attributes_only", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        Ok(resolve_attributes(&platform))
    }

    /// Like [`attributes_only`](Self::attributes_only) but also returns the
    /// [`AttributeStatistics`] gathered while attributing the path.
    #[napi]
    pub fn attributes_only_with_statistics(
        &self,
        relative_path: String,
        source: Option<AttributeSource>,
    ) -> Result<AttributeResult> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes_only(&index, attribute_source(source))
            .map_err(|e| err("attributes_only", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let entries = resolve_attributes(&platform);
        let statistics = attribute_statistics(cache.statistics().attributes);
        Ok(AttributeResult {
            entries,
            statistics,
        })
    }

    /// Check whether a relative `relative_path` is excluded by gitignore / exclude files.
    #[napi]
    pub fn is_path_excluded(&self, relative_path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .map_err(|e| err("excludes", e))?;
        let platform = cache
            .at_path(
                relative_path.as_str(),
                Some(gix::objs::tree::EntryKind::Blob.into()),
            )
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    /// Build a reusable attribute *and* exclude cache for [`AttributeStack`] queries.
    ///
    /// Prefer this over repeatedly calling [`attributes`](Self::attributes) when you need to query
    /// many paths against the same worktree/attributes state. `source` selects where `.gitattributes`
    /// are read from; omit it to use the default (`WorktreeThenIdMapping`).
    #[napi]
    pub fn attributes_stack(&self, source: Option<AttributeSource>) -> Result<AttributeStack> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let stack = r
            .attributes(
                &index,
                attribute_source(source),
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                None,
            )
            .map_err(|e| err("attributes", e))?
            .detach();
        Ok(AttributeStack {
            inner: stack,
            repo: self.inner.clone(),
        })
    }

    /// Build a reusable attribute-only cache (no exclude/ignore information consulted).
    #[napi]
    pub fn attributes_only_stack(&self, source: Option<AttributeSource>) -> Result<AttributeStack> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let stack = r
            .attributes_only(&index, attribute_source(source))
            .map_err(|e| err("attributes_only", e))?
            .detach();
        Ok(AttributeStack {
            inner: stack,
            repo: self.inner.clone(),
        })
    }

    /// Build a reusable exclude-only cache for [`AttributeStack::is_excluded`] queries.
    #[napi]
    pub fn excludes_stack(&self, source: Option<AttributeSource>) -> Result<AttributeStack> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let stack = r
            .excludes(&index, None, ignore_source(source))
            .map_err(|e| err("excludes", e))?
            .detach();
        Ok(AttributeStack {
            inner: stack,
            repo: self.inner.clone(),
        })
    }
}
