use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix_pack::data::entry::Header as PackHeader;

use crate::error::{err, err_str};

/// A single entry (full or delta object) within a pack data file, mirroring `gix_pack::data::Entry`.
///
/// Decode one with [`PackEntry::from_bytes`], then read its [`PackEntry::header`], sizes and offsets.
#[napi]
pub struct PackEntry {
    inner: gix_pack::data::Entry,
}

/// Map `"sha1"` / `"sha256"` to the corresponding `gix::hash::Kind`.
///
/// Note: `sha256` requires the `sha256` feature on `gix`, which is not enabled in this build, so
/// it is rejected at runtime rather than at compile time.
fn hash_kind(s: &str) -> Result<gix::hash::Kind> {
    match s.to_ascii_lowercase().as_str() {
        "sha1" => Ok(gix::hash::Kind::Sha1),
        "sha256" => Err(err_str(
            "pack_entry_hash",
            "sha256 is not enabled in this build; add the 'sha256' feature to the 'gix' dependency",
        )),
        other => Err(err_str(
            "pack_entry_hash",
            format!("unsupported object hash '{other}', expected 'sha1' or 'sha256'"),
        )),
    }
}

#[napi]
impl PackEntry {
    /// Decode a pack entry from the start of `data` (the bytes of a pack data file, or a slice
    /// beginning at `packOffset`). `packOffset` is the absolute byte offset of this entry within the
    /// pack, and `hash` is `"sha1"` or `"sha256"`.
    #[napi]
    pub fn from_bytes(data: Buffer, pack_offset: f64, hash: String) -> Result<PackEntry> {
        let pack_offset = u64::try_from(pack_offset as i64).map_err(|_| {
            err_str(
                "pack_entry_from_bytes",
                "pack_offset must be a non-negative integer",
            )
        })?;
        let kind = hash_kind(&hash)?;
        let entry = gix_pack::data::Entry::from_bytes(&data, pack_offset, kind)
            .map_err(|e| err("pack_entry_from_bytes", e))?;
        Ok(PackEntry { inner: entry })
    }

    /// The header of this entry, describing the object kind and (for deltas) the base.
    #[napi(getter)]
    pub fn header(&self) -> PackEntryHeader {
        PackEntryHeader {
            inner: self.inner.header,
        }
    }

    /// The decompressed size of the entry in bytes.
    #[napi(getter)]
    pub fn decompressed_size(&self) -> f64 {
        self.inner.decompressed_size as f64
    }

    /// Absolute offset to the compressed object data, just behind the entry's header.
    #[napi(getter)]
    pub fn data_offset(&self) -> f64 {
        self.inner.data_offset as f64
    }

    /// Number of bytes used to encode the entry header in the pack (preserves non-canonical encodings).
    #[napi(getter)]
    pub fn encoded_header_size(&self) -> u32 {
        self.inner.encoded_header_size as u32
    }

    /// The size, in bytes, of the header as actually present in the pack. Unlike `Header`-based
    /// canonical sizing, this preserves overlong (non-canonical) size encodings.
    #[napi]
    pub fn header_size(&self) -> f64 {
        self.inner.header_size() as f64
    }

    /// The absolute offset of the entry's start in the pack, computed from `data_offset` and the
    /// actually-encoded header size.
    #[napi]
    pub fn pack_offset(&self) -> f64 {
        self.inner.pack_offset() as f64
    }

    /// Given an ofs-delta `distance`, return the absolute pack offset of the base entry, or `null`
    /// if `distance` is zero or would point before the pack header.
    #[napi]
    pub fn checked_base_pack_offset(&self, distance: f64) -> Option<f64> {
        let distance = u64::try_from(distance as i64).ok()?;
        self.inner
            .checked_base_pack_offset(distance)
            .map(|o| o as f64)
    }
}

/// The header portion of a pack entry: the kind of stored object, or (for deltas) how the base is
/// identified. Mirrors `gix_pack::data::entry::Header`.
#[napi]
pub struct PackEntryHeader {
    inner: PackHeader,
}

#[napi]
impl PackEntryHeader {
    /// The entry kind: `"blob"`, `"tree"`, `"commit"`, `"tag"`, `"ofs_delta"`, or `"ref_delta"`.
    #[napi(getter)]
    pub fn kind(&self) -> String {
        match self.inner {
            PackHeader::Blob => "blob",
            PackHeader::Tree => "tree",
            PackHeader::Commit => "commit",
            PackHeader::Tag => "tag",
            PackHeader::OfsDelta { .. } => "ofs_delta",
            PackHeader::RefDelta { .. } => "ref_delta",
        }
        .to_string()
    }

    /// `true` if this is a delta object (ofs- or ref-delta).
    #[napi(getter)]
    pub fn is_delta(&self) -> bool {
        self.inner.is_delta()
    }

    /// `true` if this is a base object (not a delta).
    #[napi(getter)]
    pub fn is_base(&self) -> bool {
        self.inner.is_base()
    }

    /// The object kind as a git object type (`"blob"`/`"tree"`/`"commit"`/`"tag"`), or `null` for
    /// delta entries which have no concrete object kind.
    #[napi(getter)]
    pub fn object_kind(&self) -> Option<String> {
        self.inner.as_kind().map(|k| {
            match k {
                gix::object::Kind::Blob => "blob",
                gix::object::Kind::Tree => "tree",
                gix::object::Kind::Commit => "commit",
                gix::object::Kind::Tag => "tag",
            }
            .to_string()
        })
    }

    /// For `ref_delta` entries, the hex object id of the base; `null` otherwise.
    #[napi(getter)]
    pub fn base_id(&self) -> Option<String> {
        match self.inner {
            PackHeader::RefDelta { ref base_id } => Some(base_id.to_string()),
            _ => None,
        }
    }

    /// For `ofs_delta` entries, the base distance (relative to this entry's pack offset); `null` otherwise.
    #[napi(getter)]
    pub fn base_distance(&self) -> Option<i64> {
        match self.inner {
            PackHeader::OfsDelta { base_distance } => Some(base_distance as i64),
            _ => None,
        }
    }

    /// The pack-internal type id byte for this header (e.g. `1` for commit, `3` for blob).
    #[napi(getter)]
    pub fn type_id(&self) -> f64 {
        self.inner.as_type_id() as f64
    }
}
