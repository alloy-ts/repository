//! NAPI binding scaffold for the content store: the read path, the re-folding merge, and crash
//! debris recovery, composed behind one JS-facing class.
//!
//! ```text
//! ContentStore  ── owns parts + fold + container ──┐
//!   get / exists / reconstruct / scan_ids / ids     │  crate::read   (read core)
//!   refold                                        ───┤  crate::refold (re-folding merge)
//! debrisReport / removeDebris (free fns)           ─┤  crate::debris (protocol-state debris)
//! ```
//!
//! This is a BINDING SCAFFOLD and a thin marshaling layer only. The three cores
//! (`crate::read`, `crate::refold`, `crate::debris`) and the types they name — `Record`,
//! `PieceHash`, `Part`, `Fold`, `Container`, `FoldCfg`, `OperationControl`, `ReadLimits` — are
//! supplied by the core crate elsewhere, so this module will not compile until those exist.
//! `Record`/`PieceHash` are assumed `#[napi]`-exposed by `crate::types` (shared with `src/wal.rs`).
//!
//! The construction calls in [`ContentStore::open`] show the *shape* the binding expects; align
//! them with the real `Container`/`Manifest` surface before this compiles.

#![allow(dead_code, unused_variables, unreachable_code, unused_imports)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::fold::Fold;
use crate::part::Part;
use crate::read_limits::ReadLimits;
use crate::refold::RefoldStats;
use crate::types::BODY_CONTENT;

/// Convert an `anyhow::Error` into an `napi::Error` for return through `#[napi]` methods.
fn to_napi(e: anyhow::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

// ----------------------------------------------------------------------------
// ContentStore — the composed read + refold surface
// ----------------------------------------------------------------------------

/// A content store: a committed manifest revision's parts plus the fold those parts reference.
///
/// Reads answer purely from `parts`/`fold`; [`ContentStore::refold`] atomically swaps in a new
/// generation and reloads them.
#[napi]
pub struct ContentStore {
    parts: Vec<Arc<Part>>,
    fold: Fold,
    container: Arc<Mutex<crate::container::Container>>,
    /// Optional write-ahead log. Present only when the store was opened in WAL mode; the
    /// `append*`/`sync` methods route here and the core crate's commit-to-parts step follows.
    wal: Option<crate::wal::Wal>,
}

#[napi]
impl ContentStore {
    /// Open the store at `path`. When `wal` is true (or `null` → false), a write-ahead log is
    /// opened beside the store and the `append*`/`sync` methods become available.
    #[napi]
    pub fn open(path: String, wal: Option<bool>) -> napi::Result<ContentStore> {
        let container = Arc::new(Mutex::new(
            crate::container::Container::open(Path::new(&path)).map_err(to_napi)?,
        ));
        // The manifest/parts/fold loading calls below are the *shape* this binding expects. The real
        // API (Container / Manifest surface) is provided by the core crate elsewhere; align these
        // before the module compiles.
        let guard = container
            .lock()
            .map_err(|_| napi::Error::from_reason("container lock poisoned"))?;
        let parts = guard.committed_parts().map_err(to_napi)?; // -> Vec<Arc<Part>>
        let fold = guard.committed_fold().map_err(to_napi)?; // -> Fold
        drop(guard);
        // WAL sits beside the store under `<path>-wal`; the core crate owns the exact path rule.
        let wal = if wal.unwrap_or(false) {
            Some(crate::wal::Wal::open(format!("{path}-wal"))?)
        } else {
            None
        };
        Ok(ContentStore {
            parts,
            fold,
            container,
            wal,
        })
    }

    /// The newest committed version of `id`, or `null` if absent or deleted.
    #[napi]
    pub fn get(&self, id: String) -> napi::Result<Option<crate::record::Record>> {
        Ok(crate::read::get(&self.parts, &id)
            .map_err(to_napi)?
            .map(Into::into))
    }

    /// Whether `id` is present — cheaper than [`ContentStore::get`], no record is decoded.
    #[napi]
    pub fn exists(&self, id: String) -> napi::Result<bool> {
        crate::read::exists(&self.parts, &id).map_err(to_napi)
    }

    /// Byte-exact content for `id`. `name` defaults to the body content; `null` when the record or
    /// the named value is absent.
    #[napi]
    pub fn reconstruct(&self, id: String, name: Option<String>) -> napi::Result<Option<Buffer>> {
        let content = name.as_deref().unwrap_or(BODY_CONTENT);
        let bytes = crate::read::reconstruct_content(&self.parts, &self.fold, &id, content)
            .map_err(to_napi)?;
        Ok(bytes.map(Buffer::from))
    }

    /// Distinct live committed ids in `[from, to)`, sorted (or reversed), at most `limit`.
    #[napi]
    pub fn scan_ids(
        &self,
        from: Option<String>,
        to: Option<String>,
        limit: u32,
        reverse: bool,
    ) -> napi::Result<Vec<String>> {
        crate::read::scan_ids(
            &self.parts,
            from.as_deref(),
            to.as_deref(),
            limit as usize,
            reverse,
        )
        .map_err(to_napi)
    }

    /// Every distinct live committed id, sorted.
    #[napi]
    pub fn ids(&self) -> napi::Result<Vec<String>> {
        crate::read::ids(&self.parts).map_err(to_napi)
    }

    // --- WAL mode (only available when opened with `wal: true`) -------------------------------

    /// Append a general record through the WAL, with the bytes of any piece that was NOVEL when
    /// written. The store's commit-to-parts step (core crate) follows; this only makes the write
    /// durable and ordered. Requires WAL mode at [`ContentStore::open`].
    #[napi]
    pub fn append(
        &mut self,
        seq: u64,
        record: crate::record::Record,
        novel: Vec<crate::wal::WalNovelPiece>,
    ) -> napi::Result<()> {
        let wal = self
            .wal
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("store not opened in WAL mode"))?;
        let novel: Vec<(crate::types::PieceHash, Vec<u8>)> = novel
            .into_iter()
            .map(|p| {
                let arr: [u8; 32] =
                    p.hash.as_slice().try_into().map_err(|_| {
                        napi::Error::from_reason("novel piece hash must be 32 bytes")
                    })?;
                Ok((crate::types::PieceHash(arr), p.bytes.to_vec()))
            })
            .collect::<napi::Result<_>>()?;
        wal.append(seq, &record.into(), &novel).map_err(to_napi)
    }

    /// Append a tombstone (delete) for `id` through the WAL. Requires WAL mode.
    #[napi]
    pub fn append_tomb(&mut self, seq: u64, id: String) -> napi::Result<()> {
        let wal = self
            .wal
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("store not opened in WAL mode"))?;
        wal.append_tomb(seq, &id).map_err(to_napi)
    }

    /// Flush and fsync the WAL. No-op when the store has no WAL.
    #[napi]
    pub fn sync(&mut self) -> napi::Result<()> {
        if let Some(wal) = self.wal.as_mut() {
            wal.sync().map_err(to_napi)?;
        }
        Ok(())
    }

    /// The re-folding merge: reclaim bytes unreachable under current authority and write a new fold
    /// generation. Atomically reloads `parts`/`fold` on success.
    #[napi]
    pub fn refold(&mut self) -> napi::Result<RefoldReport> {
        // Per-part sequence ranges and the current fold generation. Invented core accessors; align
        // with the real `Part`/`Fold` surface before compiling.
        let seqs: Vec<(u64, u64)> = self
            .parts
            .iter()
            .map(|p| (p.seq_lo(), p.seq_hi()))
            .collect();
        let old_gen = self.fold.generation();
        let cfg = crate::fold::FoldCfg::default();
        let control = crate::control::OperationControl::default();
        let read_limits = ReadLimits::default();
        let (_new_gen, _members, st, new_fold) =
            crate::refold::refold_into_container_with_control_and_limits(
                Arc::clone(&self.container),
                &self.parts,
                &seqs,
                &self.fold,
                old_gen,
                cfg,
                &control,
                read_limits,
            )
            .map_err(to_napi)?;
        // Reload the committed view from the (now swapped) container state.
        let guard = self
            .container
            .lock()
            .map_err(|_| napi::Error::from_reason("container lock poisoned"))?;
        self.parts = guard.committed_parts().map_err(to_napi)?;
        drop(guard);
        self.fold = new_fold;
        Ok(st.into())
    }
}

// ----------------------------------------------------------------------------
// Crash-debris recovery (free functions)
// ----------------------------------------------------------------------------

/// List crash-debris beside the store at `path` (interrupted-protocol transient names).
#[napi]
pub fn debris_report(path: String) -> napi::Result<DebrisReport> {
    let report = crate::debris::debris_report(Path::new(&path)).map_err(to_napi)?;
    Ok(report.into())
}

/// Remove recognized debris beside a present store at `path`; returns the number of items removed.
#[napi]
pub fn remove_debris(path: String) -> napi::Result<u64> {
    let removed =
        crate::debris::remove_beside_present_store(Path::new(&path), ReadLimits::default())
            .map_err(to_napi)?;
    Ok(removed)
}

// ----------------------------------------------------------------------------
// NAPI output types
// ----------------------------------------------------------------------------

/// Re-folding merge statistics, as surfaced to JavaScript.
#[napi(object)]
pub struct RefoldReport {
    pub parts_in: u32,
    pub parts_out: u32,
    pub records_kept: u32,
    pub records_dropped: u32,
    pub tombstones_dropped: u32,
    pub pieces_kept: u32,
    pub pieces_dropped: u32,
    pub fold_bytes_before: u64,
    pub fold_bytes_after: u64,
    /// Bytes reclaimed — valid ONLY when `stale_generation_left` is false.
    pub bytes_reclaimed: u64,
    pub stale_generation_left: bool,
}

impl From<RefoldStats> for RefoldReport {
    fn from(s: RefoldStats) -> Self {
        RefoldReport {
            parts_in: s.parts_in as u32,
            parts_out: s.parts_out as u32,
            records_kept: s.records_kept as u32,
            records_dropped: s.records_dropped as u32,
            tombstones_dropped: s.tombstones_dropped as u32,
            pieces_kept: s.pieces_kept as u32,
            pieces_dropped: s.pieces_dropped as u32,
            fold_bytes_before: s.fold_bytes_before,
            fold_bytes_after: s.fold_bytes_after,
            bytes_reclaimed: s.bytes_reclaimed(),
            stale_generation_left: s.stale_generation_left,
        }
    }
}

/// A kind of crash debris transient name.
#[napi(string_enum)]
pub enum DebrisKind {
    PendingPublish,
    CreationStaging,
    ReclaimStaging,
    ReclaimAnchor,
    ReclaimCandidate,
    MergeScratch,
    ArtifactStaging,
}

impl From<crate::debris::DebrisKind> for DebrisKind {
    fn from(k: crate::debris::DebrisKind) -> Self {
        use crate::debris::DebrisKind as Core;
        match k {
            Core::PendingPublish => DebrisKind::PendingPublish,
            Core::CreationStaging => DebrisKind::CreationStaging,
            Core::ReclaimStaging => DebrisKind::ReclaimStaging,
            Core::ReclaimAnchor => DebrisKind::ReclaimAnchor,
            Core::ReclaimCandidate => DebrisKind::ReclaimCandidate,
            Core::MergeScratch => DebrisKind::MergeScratch,
            Core::ArtifactStaging => DebrisKind::ArtifactStaging,
        }
    }
}

/// One recognized debris item.
#[napi(object)]
pub struct DebrisEntry {
    pub path: String,
    pub kind: DebrisKind,
}

impl From<crate::debris::DebrisEntry> for DebrisEntry {
    fn from(e: crate::debris::DebrisEntry) -> Self {
        DebrisEntry {
            path: e.path.to_string_lossy().into_owned(),
            kind: e.kind.into(),
        }
    }
}

/// A debris scan result.
#[napi(object)]
pub struct DebrisReport {
    pub entries: Vec<DebrisEntry>,
}

impl From<crate::debris::DebrisReport> for DebrisReport {
    fn from(r: crate::debris::DebrisReport) -> Self {
        DebrisReport {
            entries: r.entries.into_iter().map(Into::into).collect(),
        }
    }
}
