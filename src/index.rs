use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::{IndexEntry, IndexEntryInput, Repository};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A git index (staging area), wrapping `gix_index::File`.
///
/// Returned by [`Repository::index()`], [`Repository::open_index()`], [`Repository::try_index()`],
/// [`Repository::index_or_load()`] and [`Repository::index_from_tree()`].
#[napi]
pub struct Index {
    pub(crate) inner: gix::index::File,
}

#[napi]
impl Index {
    /// The number of entries in the index.
    #[napi]
    pub fn len(&self) -> u32 {
        self.inner.entries().len() as u32
    }

    /// `true` if the index contains no entries.
    #[napi]
    pub fn is_empty(&self) -> bool {
        self.inner.entries().is_empty()
    }

    /// The path to the index file on disk (`<git_dir>/index`).
    #[napi]
    pub fn path(&self) -> String {
        self.inner.path().to_string_lossy().into_owned()
    }

    /// The on-disk index format version: `2`, `3`, or `4`.
    #[napi]
    pub fn version(&self) -> u32 {
        match self.inner.version() {
            gix::index::Version::V2 => 2,
            gix::index::Version::V3 => 3,
            gix::index::Version::V4 => 4,
        }
    }

    /// All entries currently in the index.
    #[napi]
    pub fn entries(&self) -> Vec<IndexEntry> {
        self.inner
            .entries()
            .iter()
            .map(|e| IndexEntry {
                path: lossy(e.path(&self.inner)),
                mode: format!("{:?}", e.mode),
                id: e.id.to_string(),
                stage: e.stage() as u32,
            })
            .collect()
    }
}

#[napi]
impl Repository {
    /// Open the shared, auto-updating worktree index; fails if no index exists yet.
    #[napi]
    pub fn index(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index().map_err(|e| err("index", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Open a fresh, fully-decoded copy of the index file; fails if no index exists yet.
    #[napi]
    pub fn open_index(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let file = r.open_index().map_err(|e| err("open_index", e))?;
        Ok(Index { inner: file })
    }

    /// Return the index if present, or `null` if the repository has no index file yet.
    #[napi]
    pub fn try_index(&self) -> Result<Option<Index>> {
        let r = thread_local(&self.inner);
        let opt = r.try_index().map_err(|e| err("try_index", e))?;
        Ok(opt.map(|shared| Index {
            inner: gix::index::File::clone(&shared),
        }))
    }

    #[napi]
    pub fn index_or_empty(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Return the index loaded from `HEAD^{tree}` if no index file exists, or an empty
    /// index if `HEAD` is unborn. Never fails due to a missing index.
    #[napi]
    pub fn index_or_load(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let loaded = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index_or_load", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&loaded),
        })
    }

    /// Build an in-memory index from a tree id (expensive: recursively unpacks the tree).
    #[napi]
    pub fn index_from_tree(&self, tree: String) -> Result<Index> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&tree)?;
        let file = r
            .index_from_tree(&oid)
            .map_err(|e| err("index_from_tree", e))?;
        Ok(Index { inner: file })
    }

    /// The path at which the index file would live (`<git_dir>/index`).
    #[napi]
    pub fn index_path(&self) -> String {
        let r = thread_local(&self.inner);
        r.index_path().to_string_lossy().into_owned()
    }

    /// Read entries from the index (fails if no index exists).
    #[napi]
    pub fn index_entries(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as u32,
            });
        }
        Ok(out)
    }

    /// Read entries from the index, or load them from `HEAD` / an empty index if absent.
    #[napi]
    pub fn index_or_load_from_head_entries(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index_or_load", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as u32,
            });
        }
        Ok(out)
    }

    /// Add or update index entries in the repository index file.
    #[napi]
    pub fn edit_index(&self, entries: Vec<IndexEntryInput>) -> Result<()> {
        let r = thread_local(&self.inner);
        let index_path = r.index_path();
        let mut file =
            match gix::index::File::at(&index_path, r.object_hash(), false, Default::default()) {
                Ok(file) => file,
                Err(_) => gix::index::File::from_state(
                    gix::index::State::new(r.object_hash()),
                    index_path,
                ),
            };

        for e in entries {
            let mode = match e.mode.as_deref().unwrap_or("100644") {
                "100755" => gix::index::entry::Mode::FILE_EXECUTABLE,
                "040000" | "40000" => gix::index::entry::Mode::DIR,
                "120000" => gix::index::entry::Mode::SYMLINK,
                "160000" => gix::index::entry::Mode::COMMIT,
                _ => gix::index::entry::Mode::FILE,
            };
            let oid = parse_oid(&e.id)?;
            let path = gix::bstr::BString::from(e.path);
            file.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                oid,
                gix::index::entry::Flags::empty(),
                mode,
                path.as_ref(),
            );
        }

        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;
        Ok(())
    }
}
