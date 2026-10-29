//! NAPI binding scaffold for the write-ahead log.
//!
//! ```text
//! frame:  tag(1) | seq(8) | len(4) | payload(len) | crc32(4)
//! ```
//!
//! The WAL records *carved results* (record id, named content programs, attributes) plus the bytes
//! of any piece that was NOVEL when written, so replay is exact and independent of carve logic. See
//! the module-level note in the original design for the full rationale.
//!
//! This file is a BINDING SCAFFOLD. The surrounding dependencies it names —
//! `crate::part::idcol`, `crate::types` (`Record`/`Content`/`PieceHash`/...), `crate::vfs`,
//! `crate::read_limits`, `crate::sys`, `crate::error` — are supplied by the core crate elsewhere, so
//! this module will not compile until those exist. What it shows is the intended JavaScript/TypeScript
//! surface:
//!
//! - `Wal.open(path: string): Wal`
//! - `wal.frameCount(): bigint`
//! - `wal.byteLength(): bigint`
//! - `wal.appendTomb(seq, id)`
//! - `wal.appendRecord(seq, record, novel)`  — `record: Record` must be a napi-exposed type
//! - `wal.appendBatch(seq, items)`          — `items: WalFramedRecord[]`
//! - `wal.sync()`, `wal.truncate()`
//! - `Wal.replay(path): WalFrame[]`
//!
//! NOTE: `Record` and `PieceHash` are assumed to be `#[napi]`-exposed by `crate::types`; this scaffold
//! does not redefine them. `src/object_store/base.rs` is intentionally left untouched.

#![allow(dead_code, unused_variables, unreachable_code, unused_imports)]

use crate::part::idcol::{get_varint, put_varint};
use crate::types::{AttrValue, Content, ContentHash, ContentOp, PieceHash, Record};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Convert an `anyhow::Error` into an `napi::Error` for return through `#[napi]` methods.
fn to_napi(e: anyhow::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// A record together with the piece bytes that were NOVEL when it was written.
pub type CarvedRecord = (Record, Vec<(PieceHash, Vec<u8>)>);

/// A [`CarvedRecord`] plus whether it is a tombstone — the unit a batch frames.
pub type FramedRecord = (Record, Vec<(PieceHash, Vec<u8>)>, bool);

/// A DELETION. Its payload is the id alone — a tombstone has no body, no attributes and no content.
pub const TOMB_TAG: u8 = 0xD1;
/// A BATCH COMPLETION marker. Payload is the member count as a varint.
pub const BATCH_COMPLETE_TAG: u8 = 0xD2;
/// [`TOMB_TAG`], inside a batch.
pub const BATCH_TOMB_TAG: u8 = 0xD3;
/// A general record.
pub const RECORD_TAG: u8 = 0xD4;
/// [`RECORD_TAG`], inside a batch.
pub const BATCH_RECORD_TAG: u8 = 0xD5;
const HDR: usize = 13; // tag + seq + len
const CRC: usize = 4;

/// A record plus the bytes of any piece that was new when it was written.
pub struct Frame {
    /// True when this frame deletes `record.id`.
    pub tomb: bool,
    pub seq: u64,
    pub record: Record,
    /// `(hash, bytes)` for pieces this frame introduced.
    pub novel: Vec<(PieceHash, Vec<u8>)>,
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn get_bytes<'a>(b: &'a [u8], at: &mut usize) -> Result<&'a [u8]> {
    let n = usize::try_from(get_varint(b, at)?)
        .context("wal: byte-string length exceeds this platform's address space")?;
    if n > b.len() - *at {
        bail!("wal: byte string runs past the frame");
    }
    let s = &b[*at..*at + n];
    *at += n;
    Ok(s)
}

fn take<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    if n > b.len() - *at {
        bail!("wal: field of {n} bytes runs past the frame");
    }
    let s = &b[*at..*at + n];
    *at += n;
    Ok(s)
}

fn put_ops(out: &mut Vec<u8>, ops: &[ContentOp]) {
    put_varint(out, ops.len() as u64);
    for op in ops {
        match op {
            ContentOp::Lit(b) => {
                out.push(0);
                put_bytes(out, b);
            }
            ContentOp::Piece { hash, len } => {
                out.push(1);
                out.extend_from_slice(&hash.0);
                put_varint(out, *len as u64);
            }
        }
    }
}

fn get_ops(b: &[u8], at: &mut usize) -> Result<Vec<ContentOp>> {
    let n_ops = usize::try_from(get_varint(b, at)?)
        .context("wal: content-op count exceeds this platform's address space")?;
    let mut ops = Vec::with_capacity(n_ops.min(b.len()));
    for _ in 0..n_ops {
        let tag = *b
            .get(*at)
            .ok_or_else(|| anyhow::anyhow!("wal: truncated content op"))?;
        *at += 1;
        match tag {
            0 => ops.push(ContentOp::Lit(get_bytes(b, at)?.to_vec())),
            1 => {
                let mut h = [0u8; 32];
                h.copy_from_slice(take(b, at, 32)?);
                let len = u32::try_from(get_varint(b, at)?)
                    .context("wal: piece length exceeds the format's u32 limit")?;
                if len == 0 {
                    bail!("wal: piece length must be non-zero");
                }
                ops.push(ContentOp::Piece {
                    hash: PieceHash(h),
                    len,
                });
            }
            t => bail!("wal: unknown content op tag {t}"),
        }
    }
    Ok(ops)
}

fn put_attrs(out: &mut Vec<u8>, attrs: &[(String, AttrValue)]) {
    put_varint(out, attrs.len() as u64);
    for (k, v) in attrs {
        put_bytes(out, k.as_bytes());
        out.push(v.type_tag());
        match v {
            AttrValue::Str(s) => put_bytes(out, s.as_bytes()),
            AttrValue::Int(i) => out.extend_from_slice(&i.to_le_bytes()),
            AttrValue::Float(f) => out.extend_from_slice(&f.to_bits().to_le_bytes()),
            AttrValue::Bool(b) => out.push(u8::from(*b)),
            AttrValue::UInt(i) => out.extend_from_slice(&i.to_le_bytes()),
            AttrValue::Bytes(bytes) => put_bytes(out, bytes),
            AttrValue::TimestampNs(ns) => out.extend_from_slice(&ns.to_le_bytes()),
            AttrValue::Null => {}
        }
    }
}

fn get_attrs(b: &[u8], at: &mut usize) -> Result<Vec<(String, AttrValue)>> {
    let n_attrs = usize::try_from(get_varint(b, at)?)
        .context("wal: attribute count exceeds this platform's address space")?;
    let mut attrs = Vec::with_capacity(n_attrs.min(b.len()));
    for _ in 0..n_attrs {
        let key = String::from_utf8(get_bytes(b, at)?.to_vec())?;
        if key.is_empty() {
            bail!("wal: attribute name must not be empty");
        }
        let tag = *b
            .get(*at)
            .ok_or_else(|| anyhow::anyhow!("wal: truncated attr"))?;
        *at += 1;
        let v = match tag {
            0 => AttrValue::Str(String::from_utf8(get_bytes(b, at)?.to_vec())?),
            1 => AttrValue::Int(i64::from_le_bytes(take(b, at, 8)?.try_into().unwrap())),
            2 => AttrValue::Float(f64::from_bits(u64::from_le_bytes(
                take(b, at, 8)?.try_into().unwrap(),
            ))),
            3 => match take(b, at, 1)?[0] {
                0 => AttrValue::Bool(false),
                1 => AttrValue::Bool(true),
                other => bail!("wal: invalid boolean byte {other}"),
            },
            4 => AttrValue::UInt(u64::from_le_bytes(take(b, at, 8)?.try_into().unwrap())),
            5 => AttrValue::Bytes(get_bytes(b, at)?.to_vec()),
            6 => AttrValue::TimestampNs(i64::from_le_bytes(take(b, at, 8)?.try_into().unwrap())),
            7 => AttrValue::Null,
            t => bail!("wal: unknown attr type tag {t}"),
        };
        attrs.push((key, v));
    }
    Ok(attrs)
}

fn put_novel(out: &mut Vec<u8>, novel: &[(PieceHash, Vec<u8>)]) {
    put_varint(out, novel.len() as u64);
    for (h, bytes) in novel {
        out.extend_from_slice(&h.0);
        put_bytes(out, bytes);
    }
}

fn get_novel(b: &[u8], at: &mut usize) -> Result<Vec<(PieceHash, Vec<u8>)>> {
    let n_novel = usize::try_from(get_varint(b, at)?)
        .context("wal: novel-piece count exceeds this platform's address space")?;
    let mut novel = Vec::with_capacity(n_novel.min(b.len() / 33 + 1));
    for _ in 0..n_novel {
        let mut h = [0u8; 32];
        h.copy_from_slice(take(b, at, 32)?);
        let bytes = get_bytes(b, at)?.to_vec();
        if bytes.is_empty() {
            bail!("wal: novel piece bytes must be non-empty");
        }
        if PieceHash::of(&bytes).0 != h {
            bail!("wal: novel piece bytes do not match their BLAKE3 identity");
        }
        novel.push((PieceHash(h), bytes));
    }
    Ok(novel)
}

/// Verify every content program whose pieces are wholly carried by this frame.
fn verify_frame_local_identities(record: &Record, novel: &[(PieceHash, Vec<u8>)]) -> Result<()> {
    let mut pieces = HashMap::with_capacity(novel.len());
    for (hash, bytes) in novel {
        if pieces.insert(*hash, bytes.as_slice()).is_some() {
            bail!("wal: duplicate novel piece identity {hash}");
        }
    }
    for content in &record.contents {
        let mut hasher = blake3::Hasher::new();
        let mut complete = true;
        for op in &content.ops {
            match op {
                ContentOp::Lit(bytes) => {
                    hasher.update(bytes);
                }
                ContentOp::Piece { hash, len } => match pieces.get(hash) {
                    Some(bytes) => {
                        if bytes.len() != usize::try_from(*len).unwrap_or(usize::MAX) {
                            bail!(
                                "wal: content {:?} declares piece {hash} length {len}, actual {}",
                                content.name,
                                bytes.len()
                            );
                        }
                        hasher.update(bytes);
                    }
                    None => {
                        complete = false;
                    }
                },
            }
        }
        if complete {
            let declared = content.identity.ok_or_else(|| {
                anyhow::anyhow!("wal: content {:?} has no identity", content.name)
            })?;
            let actual = ContentHash(hasher.finalise().into());
            if actual != declared {
                bail!(
                    "wal: content {:?} identity is {declared}, reconstructed bytes are {actual}",
                    content.name
                );
            }
        }
    }
    Ok(())
}

/// Encode the current general-record payload.
pub fn encode_record(out: &mut Vec<u8>, r: &Record, novel: &[(PieceHash, Vec<u8>)]) -> Result<()> {
    validate_record_for_wal(r, novel)?;
    put_bytes(out, r.id.as_bytes());
    put_varint(out, r.contents.len() as u64);
    let mut contents: Vec<&Content> = r.contents.iter().collect();
    contents.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    for content in contents {
        put_bytes(out, content.name.as_bytes());
        let identity = content.identity.ok_or_else(|| {
            anyhow::anyhow!(
                "content {:?} has no reconstructed-byte identity",
                content.name
            )
        })?;
        out.extend_from_slice(&identity.0);
        put_ops(out, &content.ops);
    }
    put_attrs(out, &r.attrs);
    put_novel(out, novel);
    Ok(())
}

fn validate_record_for_wal(r: &Record, novel: &[(PieceHash, Vec<u8>)]) -> Result<()> {
    crate::types::validate_contents(&r.contents)?;
    if r.id.is_empty() {
        bail!("record id must not be empty");
    }
    if r.attrs.iter().any(|(key, _)| key.is_empty()) {
        bail!("attribute name must not be empty");
    }
    let mut identities = std::collections::HashSet::with_capacity(novel.len());
    for (hash, bytes) in novel {
        if bytes.is_empty() {
            bail!("novel piece bytes must be non-empty");
        }
        if PieceHash::of(bytes) != *hash {
            bail!("novel piece bytes do not match their BLAKE3 identity");
        }
        if !identities.insert(*hash) {
            bail!("duplicate novel piece identity {hash}");
        }
    }
    verify_frame_local_identities(r, novel)?;
    Ok(())
}

/// Decode a record payload.
pub fn decode_record(b: &[u8]) -> Result<CarvedRecord> {
    let mut at = 0usize;
    let id = String::from_utf8(get_bytes(b, &mut at)?.to_vec())?;
    let n_contents = usize::try_from(get_varint(b, &mut at)?)
        .context("wal: content count exceeds this platform's address space")?;
    let mut contents = Vec::with_capacity(n_contents.min(b.len()));
    let mut previous_name: Option<Vec<u8>> = None;
    for _ in 0..n_contents {
        let name_bytes = get_bytes(b, &mut at)?.to_vec();
        if previous_name
            .as_deref()
            .is_some_and(|previous| previous >= name_bytes.as_slice())
        {
            bail!("wal: content names are duplicated or out of canonical UTF-8 order");
        }
        previous_name = Some(name_bytes.clone());
        let name = String::from_utf8(name_bytes)?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(take(b, &mut at, 32)?);
        let identity = Some(ContentHash(digest));
        let mut content = Content::new(name, get_ops(b, &mut at)?);
        content.identity = identity;
        contents.push(content);
    }
    let attrs = get_attrs(b, &mut at)?;
    let novel = get_novel(b, &mut at)?;
    if at != b.len() {
        bail!("wal: record has {} trailing bytes", b.len() - at);
    }
    let record = Record::new(id, contents, attrs)?;
    verify_frame_local_identities(&record, &novel)?;
    Ok((record, novel))
}

/// Buffered, explicitly-flushed WAL writer positioned through the [`crate::vfs`] seam.
pub struct Wal {
    f: File,
    path: std::path::PathBuf,
    /// Bytes durably ordered in the FILE (not necessarily fsynced).
    file_len: u64,
    /// Frames appended but not yet written to the file.
    buf: Vec<u8>,
    scratch: Vec<u8>,
    read_limits: crate::read_limits::ReadLimits,
    frame_count: u64,
}

pub(crate) struct ReplayState {
    pub frames: Vec<Frame>,
    pub physical_frames: u64,
    pub valid_bytes: u64,
}

/// Flush the buffer once it holds this much.
const BUF_FLUSH: usize = 1 << 20;

impl Wal {
    /// Open (recovering) the WAL at `path`. Recovery truncates the file back to the last
    /// fully-valid byte boundary.
    pub(crate) fn open_path(path: &Path) -> Result<Wal> {
        let read_limits = crate::read_limits::ReadLimits::default();
        let replay = Self::replay_state_with_limits(path, read_limits)?;
        Self::open_recovered(
            path,
            read_limits,
            replay.physical_frames,
            replay.valid_bytes,
        )
    }

    pub(crate) fn open_recovered(
        path: &Path,
        read_limits: crate::read_limits::ReadLimits,
        frame_count: u64,
        valid_bytes: u64,
    ) -> Result<Wal> {
        let read_limits = read_limits.validate()?;
        read_limits.admit_wal_frames(frame_count)?;
        let (f, created) = crate::vfs::open_or_create(path)
            .with_context(|| format!("open wal {}", path.display()))?;
        if created {
            if let Some(parent) = path.parent() {
                crate::vfs::sync_dir(parent).with_context(|| {
                    format!("fsync {} after creating the wal", parent.display())
                })?;
            }
        }
        let file_len = f.metadata()?.len();
        if valid_bytes > file_len {
            bail!("WAL replay boundary {valid_bytes} runs past the file's {file_len} bytes");
        }
        if valid_bytes < file_len {
            crate::vfs::set_len(&f, path, valid_bytes)?;
            crate::vfs::sync_file(&f, path)?;
        }
        Ok(Wal {
            f,
            path: path.to_path_buf(),
            file_len: valid_bytes,
            buf: Vec::new(),
            scratch: Vec::new(),
            read_limits,
            frame_count,
        })
    }

    pub(crate) fn admit_additional_frames(&self, additional: u64) -> Result<()> {
        let proposed = self
            .frame_count
            .checked_add(additional)
            .ok_or_else(|| anyhow::anyhow!("WAL frame count overflow"))?;
        self.read_limits.admit_wal_frames(proposed)?;
        Ok(())
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn flush_buf(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        crate::vfs::write_all_at(&self.f, &self.path, &self.buf, self.file_len)?;
        self.file_len = self
            .file_len
            .checked_add(self.buf.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("WAL byte length overflow"))?;
        self.buf.clear();
        Ok(())
    }

    fn append_frame(&mut self, tag: u8, seq: u64, payload: &[u8]) -> Result<()> {
        if payload.len() as u64 > u32::MAX as u64 {
            bail!(
                "wal frame payload of {} bytes exceeds the u32 length field",
                payload.len()
            );
        }
        self.admit_additional_frames(1)?;
        let frame_len = HDR
            .checked_add(payload.len())
            .and_then(|len| len.checked_add(CRC))
            .ok_or_else(|| anyhow::anyhow!("WAL frame allocation length overflow"))?;
        let projected = self
            .buf
            .len()
            .checked_add(frame_len)
            .ok_or_else(|| anyhow::anyhow!("WAL buffer length overflow"))?;

        if !self.buf.is_empty() && projected >= BUF_FLUSH {
            self.flush_buf()?;
        }

        let mut hdr = [0u8; HDR];
        hdr[0] = tag;
        hdr[1..9].copy_from_slice(&seq.to_le_bytes());
        hdr[9..13].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(&hdr);
        crc.update(payload);
        let c = crc.finalise();
        self.buf
            .try_reserve(frame_len)
            .map_err(|error| anyhow::anyhow!("reserve WAL frame buffer: {error}"))?;
        self.buf.extend_from_slice(&hdr);
        self.buf.extend_from_slice(payload);
        self.buf.extend_from_slice(&c.to_le_bytes());
        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("WAL frame count overflow"))?;
        Ok(())
    }

    /// Log a deletion.
    pub fn append_tomb(&mut self, seq: u64, id: &str) -> Result<()> {
        if id.is_empty() {
            bail!("record id must not be empty");
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.extend_from_slice(id.as_bytes());
        let r = self.append_frame(TOMB_TAG, seq, &scratch);
        self.scratch = scratch;
        r
    }

    pub fn append(&mut self, seq: u64, r: &Record, novel: &[(PieceHash, Vec<u8>)]) -> Result<()> {
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        let encoded = encode_record(&mut scratch, r, novel);
        let res = encoded.and_then(|()| self.append_frame(RECORD_TAG, seq, &scratch));
        self.scratch = scratch;
        res
    }

    /// Append records and tombstones as ONE atomic unit.
    pub fn append_batch(&mut self, seq: u64, items: &[FramedRecord]) -> Result<()> {
        if items.is_empty() {
            bail!("a WAL batch must contain at least one mutation");
        }
        for (record, novel, tombstone) in items {
            if record.id.is_empty() {
                bail!("record id must not be empty");
            }
            if *tombstone {
                if !record.contents.is_empty() || !record.attrs.is_empty() || !novel.is_empty() {
                    bail!("a WAL tombstone may carry only its record id");
                }
            } else {
                validate_record_for_wal(record, novel)?;
            }
        }
        let batch_frames = u64::try_from(items.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| anyhow::anyhow!("WAL batch frame count overflow"))?;
        self.admit_additional_frames(batch_frames)?;
        for (r, novel, tomb) in items {
            let mut scratch = std::mem::take(&mut self.scratch);
            scratch.clear();
            let tag = if *tomb {
                scratch.extend_from_slice(r.id.as_bytes());
                BATCH_TOMB_TAG
            } else {
                encode_record(&mut scratch, r, novel)?;
                BATCH_RECORD_TAG
            };
            let res = self.append_frame(tag, seq, &scratch);
            self.scratch = scratch;
            res?;
        }
        let mut count = Vec::with_capacity(4);
        put_varint(&mut count, items.len() as u64);
        self.append_frame(BATCH_COMPLETE_TAG, seq, &count)
    }

    /// The ACK point.
    pub fn sync(&mut self) -> Result<()> {
        self.flush_buf()?;
        crate::vfs::sync_file(&self.f, &self.path).context("fsync wal")?;
        Ok(())
    }

    /// Drop every frame — called once the records they cover are committed in a part.
    pub fn truncate(&mut self) -> Result<()> {
        self.buf.clear();
        crate::vfs::set_len(&self.f, &self.path, 0)?;
        self.file_len = 0;
        self.frame_count = 0;
        crate::vfs::sync_file(&self.f, &self.path)?;
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.file_len + self.buf.len() as u64
    }

    /// Replay every intact, COMMITTED frame, in order.
    pub fn replay_records(path: &Path) -> Result<Vec<Frame>> {
        let state =
            Self::replay_state_with_limits(path, crate::read_limits::ReadLimits::default())?;
        Ok(state.frames)
    }

    pub fn replay_with_limits(
        path: &Path,
        read_limits: crate::read_limits::ReadLimits,
    ) -> Result<ReplayState> {
        Self::replay_state_with_limits(path, read_limits)
    }

    fn replay_state_with_limits(
        path: &Path,
        read_limits: crate::read_limits::ReadLimits,
    ) -> Result<ReplayState> {
        let f = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReplayState {
                    frames: Vec::new(),
                    physical_frames: 0,
                    valid_bytes: 0,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len();
        let mut out: Vec<Frame> = Vec::new();
        let mut pending: Vec<Frame> = Vec::new();
        let mut pending_start: Option<(u64, u64)> = None;
        let mut physical_frames: u64 = 0;
        let mut off: u64 = 0;

        let known = |tag: u8| {
            matches!(
                tag,
                TOMB_TAG | BATCH_TOMB_TAG | BATCH_COMPLETE_TAG | RECORD_TAG | BATCH_RECORD_TAG
            )
        };

        while off + (HDR as u64) + (CRC as u64) <= len {
            let mut hdr = [0u8; HDR];
            crate::sys::read_exact_at(&f, &mut hdr, off)
                .with_context(|| format!("read WAL frame header at byte {off}"))?;
            let tag = hdr[0];
            let seq = u64::from_le_bytes(hdr[1..9].try_into().unwrap());
            let plen = u32::from_le_bytes(hdr[9..13].try_into().unwrap()) as usize;
            let end = off
                .checked_add(HDR as u64)
                .and_then(|end| end.checked_add(plen as u64))
                .and_then(|end| end.checked_add(CRC as u64))
                .ok_or_else(|| anyhow::anyhow!("WAL frame end overflows"))?;
            if end > len {
                break; // structurally short tail
            }
            read_limits
                .admit_wal_frames(physical_frames.saturating_add(1))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            read_limits
                .admit("WAL frame", plen as u64, plen as u64)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut payload = vec![0u8; plen];
            crate::sys::read_exact_at(&f, &mut payload, off + HDR as u64)
                .with_context(|| format!("read WAL frame payload at byte {off}"))?;
            let mut cb = [0u8; CRC];
            crate::sys::read_exact_at(&f, &mut cb, off + HDR as u64 + plen as u64)
                .with_context(|| format!("read WAL frame checksum at byte {off}"))?;
            let mut crc = crc32fast::Hasher::new();
            crc.update(&hdr);
            crc.update(&payload);
            if crc.finalise() != u32::from_le_bytes(cb) {
                if end == len {
                    break; // only the final complete frame may be a torn write
                }
                bail!(
                    "wal frame at byte {off} fails its checksum with {} later bytes present",
                    len - end
                );
            }
            if !known(tag) {
                bail!(
                    "wal frame tag {:#04x} is not a type this build knows, and it checksums — \
                     refusing rather than discarding committed records",
                    tag
                );
            }
            match tag {
                BATCH_COMPLETE_TAG => {
                    let mut at = 0usize;
                    let n = get_varint(&payload, &mut at)
                        .context("wal batch completion has a malformed member count")?;
                    if at != payload.len() {
                        bail!(
                            "wal batch completion has {} trailing bytes",
                            payload.len() - at
                        );
                    }
                    let n = usize::try_from(n)
                        .context("wal batch member count exceeds this platform's address space")?;
                    if n == 0 {
                        bail!("wal batch completion marker cannot name zero members");
                    }
                    if n > pending.len() {
                        bail!(
                            "wal batch completion names {n} frames but only {} precede it",
                            pending.len()
                        );
                    }
                    let start = pending.len() - n;
                    if pending[start..].iter().any(|member| member.seq != seq) {
                        bail!(
                            "wal batch completion sequence {seq} differs from one or more member sequences"
                        );
                    }
                    out.extend(pending.drain(..).skip(start));
                    pending_start = None;
                }
                TOMB_TAG | BATCH_TOMB_TAG => {
                    let id = String::from_utf8(payload).context("wal tombstone id is not UTF-8")?;
                    if id.is_empty() {
                        bail!("wal tombstone id is empty");
                    }
                    let fr = Frame {
                        seq,
                        tomb: true,
                        record: Record {
                            id,
                            contents: Vec::new(),
                            attrs: Vec::new(),
                        },
                        novel: Vec::new(),
                    };
                    if tag == BATCH_TOMB_TAG {
                        if pending.is_empty() {
                            pending_start = Some((off, physical_frames));
                        }
                        pending.push(fr);
                    } else {
                        pending.clear();
                        pending_start = None;
                        out.push(fr);
                    }
                }
                RECORD_TAG | BATCH_RECORD_TAG => {
                    let (record, novel) = decode_record(&payload)
                        .context("wal record frame carries an invalid current-format payload")?;
                    let fr = Frame {
                        seq,
                        tomb: false,
                        record,
                        novel,
                    };
                    if tag == BATCH_RECORD_TAG {
                        if pending.is_empty() {
                            pending_start = Some((off, physical_frames));
                        }
                        pending.push(fr);
                    } else {
                        pending.clear();
                        pending_start = None;
                        out.push(fr);
                    }
                }
                _ => unreachable!("known WAL tag was not handled"),
            }
            physical_frames += 1;
            off = end;
        }
        let (valid_bytes, physical_frames) = pending_start.unwrap_or((off, physical_frames));
        Ok(ReplayState {
            frames: out,
            physical_frames,
            valid_bytes,
        })
    }
}

// ----------------------------------------------------------------------------
// NAPI binding surface
// ----------------------------------------------------------------------------

/// A novel piece as seen from JavaScript: a 32-byte BLAKE3 `hash` plus its `bytes`.
#[napi(object)]
pub struct WalNovelPiece {
    pub hash: Vec<u8>,
    pub bytes: Buffer,
}

/// A framed record for batching, as seen from JavaScript.
#[napi(object)]
pub struct WalFramedRecord {
    pub record: crate::record::Record,
    pub novel: Vec<WalNovelPiece>,
    pub tombstone: bool,
}

/// A committed frame as returned by `Wal.replay`, as seen from JavaScript.
#[napi(object)]
pub struct WalFrame {
    pub seq: u64,
    pub tomb: bool,
    pub record: crate::record::Record,
    pub novel: Vec<WalNovelPiece>,
}

impl From<Frame> for WalFrame {
    fn from(f: Frame) -> Self {
        WalFrame {
            seq: f.seq,
            tomb: f.tomb,
            record: f.record.into(),
            novel: f
                .novel
                .into_iter()
                .map(|(h, b)| WalNovelPiece {
                    hash: h.0.to_vec(),
                    bytes: b.into(),
                })
                .collect(),
        }
    }
}

fn novel_from_js(pieces: Vec<WalNovelPiece>) -> napi::Result<Vec<(PieceHash, Vec<u8>)>> {
    pieces
        .into_iter()
        .map(|p| {
            let arr: [u8; 32] = p
                .hash
                .as_slice()
                .try_into()
                .map_err(|_| napi::Error::from_reason("novel piece hash must be 32 bytes"))?;
            Ok((PieceHash(arr), p.bytes.to_vec()))
        })
        .collect()
}

#[napi]
impl Wal {
    /// Open (and recover) the WAL at `path`.
    #[napi]
    pub fn open(path: String) -> napi::Result<Wal> {
        let read_limits = crate::read_limits::ReadLimits::default();
        let replay =
            Self::replay_state_with_limits(Path::new(&path), read_limits).map_err(to_napi)?;
        let wal = Self::open_recovered(
            Path::new(&path),
            read_limits,
            replay.physical_frames,
            replay.valid_bytes,
        )
        .map_err(to_napi)?;
        Ok(wal)
    }

    /// Number of frames appended in this WAL's lifetime (committed + pending).
    #[napi]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Total bytes held by the WAL: durable file bytes plus the unflushed buffer.
    #[napi]
    pub fn byte_length(&self) -> u64 {
        self.bytes()
    }

    /// Log a deletion of `id`.
    #[napi]
    pub fn append_tomb(&mut self, seq: u64, id: String) -> napi::Result<()> {
        self.append_tomb(seq, &id).map_err(to_napi)
    }

    /// Append a single general record with its novel pieces.
    #[napi]
    pub fn append_record(
        &mut self,
        seq: u64,
        record: crate::record::Record,
        novel: Vec<WalNovelPiece>,
    ) -> napi::Result<()> {
        let novel = novel_from_js(novel)?;
        let record: crate::types::Record = record.into();
        self.append(seq, &record, &novel).map_err(to_napi)
    }

    /// Append a batch of records/tombstones as one atomic, all-or-nothing unit.
    #[napi]
    pub fn append_batch_records(
        &mut self,
        seq: u64,
        items: Vec<WalFramedRecord>,
    ) -> napi::Result<()> {
        let items: Vec<FramedRecord> = items
            .into_iter()
            .map(|it| {
                let novel = novel_from_js(it.novel)?;
                let record: crate::types::Record = it.record.into();
                Ok((record, novel, it.tombstone))
            })
            .collect::<napi::Result<Vec<_>>>()?;
        self.append_batch(seq, &items).map_err(to_napi)
    }

    /// The ACK point: flush and fsync.
    #[napi]
    pub fn sync(&mut self) -> napi::Result<()> {
        self.sync().map_err(to_napi)
    }

    /// Drop every frame after the covered records have been committed.
    #[napi]
    pub fn truncate(&mut self) -> napi::Result<()> {
        self.truncate().map_err(to_napi)
    }

    /// Replay every intact, committed frame from the WAL at `path`.
    #[napi]
    pub fn replay(path: String) -> napi::Result<Vec<WalFrame>> {
        let state = Self::replay_state_with_limits(
            Path::new(&path),
            crate::read_limits::ReadLimits::default(),
        )
        .map_err(to_napi)?;
        Ok(state.frames.into_iter().map(WalFrame::from).collect())
    }
}

// ----------------------------------------------------------------------------
// Tests (require the core dependencies; kept for parity with the design).
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BODY_CONTENT;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn rec() -> (Record, Vec<(PieceHash, Vec<u8>)>) {
        let bytes = b"a novel piece".to_vec();
        let h = PieceHash::of(&bytes);
        (
            Record {
                id: "genai:aé#in".into(),
                contents: vec![Content::identified(
                    BODY_CONTENT,
                    vec![
                        ContentOp::Lit(b"[".to_vec()),
                        ContentOp::Piece {
                            hash: h,
                            len: bytes.len() as u32,
                        },
                    ],
                    ContentHash::of(&[b"[".as_slice(), bytes.as_slice()].concat()),
                )],
                attrs: vec![
                    ("k".into(), AttrValue::Str("v".into())),
                    ("k".into(), AttrValue::Int(-5)),
                    (
                        "f".into(),
                        AttrValue::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
                    ),
                    ("z".into(), AttrValue::Float(-0.0)),
                    ("b".into(), AttrValue::Bool(true)),
                    ("u".into(), AttrValue::UInt(u64::MAX)),
                    ("raw".into(), AttrValue::Bytes(vec![0, 0xff, 1])),
                    ("at".into(), AttrValue::TimestampNs(i64::MIN)),
                    ("none".into(), AttrValue::Null),
                ],
            },
            vec![(h, bytes)],
        )
    }

    #[test]
    fn replay_admits_a_complete_frame_before_payload_allocation() {
        let p = std::env::temp_dir().join(format!(
            "turndb-wal-read-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut wal = Wal::open_path(&p).unwrap();
        wal.append_tomb(1, &"x".repeat(128)).unwrap();
        wal.sync().unwrap();
        drop(wal);

        let error = match Wal::replay_with_limits(
            &p,
            crate::read_limits::ReadLimits {
                max_stored_frame_bytes: 64,
                max_decoded_frame_bytes: 64,
                ..crate::read_limits::ReadLimits::default()
            },
        ) {
            Ok(_) => panic!("strict replay must refuse the complete larger frame"),
            Err(error) => error,
        };
        assert_eq!(
            crate::error::classify(&error),
            crate::error::ErrorClass::ResourceExhausted
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn record_wire_roundtrips_exactly() {
        let (r, novel) = rec();
        let mut b = Vec::new();
        encode_record(&mut b, &r, &novel).unwrap();
        let (got, gn) = decode_record(&b).unwrap();
        assert_eq!(
            got, r,
            "record must replay exactly, attr order and dupes included"
        );
        assert_eq!(gn.len(), 1);
        assert_eq!(gn[0].1, novel[0].1);
        match (&got.attrs[2].1, &r.attrs[2].1) {
            (AttrValue::Float(a), AttrValue::Float(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            _ => panic!(),
        }
    }

    #[test]
    fn record_wire_refuses_noncanonical_content_order() {
        let mut bytes = Vec::new();
        put_bytes(&mut bytes, b"record");
        put_varint(&mut bytes, 2);
        let identity = ContentHash::of(b"");
        for name in [b"z".as_slice(), b"a".as_slice()] {
            put_bytes(&mut bytes, name);
            bytes.extend_from_slice(&identity.0);
            put_ops(&mut bytes, &[]);
        }
        put_attrs(&mut bytes, &[]);
        put_novel(&mut bytes, &[]);
        assert!(
            decode_record(&bytes)
                .unwrap_err()
                .to_string()
                .contains("canonical"),
            "the reader must not sort hostile WAL bytes into a valid semantic record"
        );
    }

    #[test]
    fn record_wire_refuses_false_content_identity_and_empty_attribute_name() {
        let mut false_identity = Vec::new();
        put_bytes(&mut false_identity, b"record");
        put_varint(&mut false_identity, 1);
        put_bytes(&mut false_identity, b"body");
        false_identity.extend_from_slice(&ContentHash::of(b"different").0);
        put_ops(&mut false_identity, &[ContentOp::Lit(b"actual".to_vec())]);
        put_attrs(&mut false_identity, &[]);
        put_novel(&mut false_identity, &[]);
        assert!(decode_record(&false_identity).is_err());

        let mut empty_attr = Vec::new();
        put_bytes(&mut empty_attr, b"record");
        put_varint(&mut empty_attr, 0);
        put_attrs(&mut empty_attr, &[(String::new(), AttrValue::Null)]);
        put_novel(&mut empty_attr, &[]);
        assert!(decode_record(&empty_attr).is_err());

        let mut zero_piece = Vec::new();
        put_bytes(&mut zero_piece, b"record");
        put_varint(&mut zero_piece, 1);
        put_bytes(&mut zero_piece, b"body");
        zero_piece.extend_from_slice(&ContentHash::of(b"").0);
        put_ops(
            &mut zero_piece,
            &[ContentOp::Piece {
                hash: PieceHash::of(b""),
                len: 0,
            }],
        );
        put_attrs(&mut zero_piece, &[]);
        put_novel(&mut zero_piece, &[]);
        assert!(
            decode_record(&zero_piece).is_err(),
            "a zero-length piece op must refuse"
        );

        let mut zero_novel = Vec::new();
        put_bytes(&mut zero_novel, b"record");
        put_varint(&mut zero_novel, 0);
        put_attrs(&mut zero_novel, &[]);
        put_novel(&mut zero_novel, &[(PieceHash::of(b""), Vec::new())]);
        assert!(
            decode_record(&zero_novel).is_err(),
            "empty novel piece bytes must refuse"
        );
    }

    #[test]
    fn public_wal_writers_refuse_unreplayable_records_before_buffering_frames() {
        let d =
            std::env::temp_dir().join(format!("turndb-wal-writer-shape-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let mut wal = Wal::open_path(&p).unwrap();

        assert!(wal.append_batch(1, &[]).is_err());
        assert_eq!(
            wal.bytes(),
            0,
            "an empty batch must not write a marker the reader refuses"
        );

        assert!(wal.append_tomb(1, "").is_err());
        assert_eq!(wal.bytes(), 0);

        let invalid = Record {
            id: "invalid".into(),
            contents: Vec::new(),
            attrs: vec![(String::new(), AttrValue::Null)],
        };
        assert!(wal.append(1, &invalid, &[]).is_err());
        assert_eq!(wal.bytes(), 0);

        let zero_piece = Record {
            id: "zero-piece".into(),
            contents: vec![Content::identified(
                "body",
                vec![ContentOp::Piece {
                    hash: PieceHash::of(b""),
                    len: 0,
                }],
                ContentHash::of(b""),
            )],
            attrs: Vec::new(),
        };
        assert!(wal.append(1, &zero_piece, &[]).is_err());
        assert_eq!(
            wal.bytes(),
            0,
            "zero-length piece refusal must precede WAL buffering"
        );

        let empty_novel = vec![(PieceHash::of(b""), Vec::new())];
        let (valid, _) = rec();
        assert!(wal.append(1, &valid, &empty_novel).is_err());
        assert_eq!(
            wal.bytes(),
            0,
            "empty novel-piece refusal must precede WAL buffering"
        );

        let (valid, novel) = rec();
        assert!(
            wal.append_batch(1, &[(valid, novel, false), (invalid, Vec::new(), false)])
                .is_err()
        );
        assert_eq!(
            wal.bytes(),
            0,
            "a late invalid batch member must precede every frame"
        );

        let tomb_with_payload = Record {
            id: "deleted".into(),
            contents: Vec::new(),
            attrs: vec![("hidden".into(), AttrValue::Bool(true))],
        };
        assert!(
            wal.append_batch(1, &[(tomb_with_payload, Vec::new(), true)])
                .is_err()
        );
        assert_eq!(wal.bytes(), 0);
        drop(wal);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn log_replays_and_stops_at_a_torn_tail() {
        let d = std::env::temp_dir().join(format!("turndb-wal-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let (r, novel) = rec();
        {
            let mut w = Wal::open_path(&p).unwrap();
            for s in 1..=5 {
                w.append(s, &r, &novel).unwrap();
            }
            w.sync().unwrap();
        }
        assert_eq!(Wal::replay_records(&p).unwrap().len(), 5);

        {
            let mut f = OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(&[RECORD_TAG, 9, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0])
                .unwrap();
            f.write_all(b"short").unwrap();
            f.sync_all().unwrap();
        }
        let frames = Wal::replay_records(&p).unwrap();
        assert_eq!(
            frames.len(),
            5,
            "a torn tail is the end of the log, not an error"
        );
        assert_eq!(frames[4].seq, 5);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_checksum_failure_before_later_frames_is_corruption_not_a_torn_tail() {
        let d = std::env::temp_dir().join(format!("turndb-wal-middle-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let (r, novel) = rec();
        let mut writer = Wal::open_path(&p).unwrap();
        writer.append(1, &r, &novel).unwrap();
        writer.append(1, &r, &novel).unwrap();
        writer.sync().unwrap();
        drop(writer);

        let mut bytes = std::fs::read(&p).unwrap();
        let first_payload = HDR;
        bytes[first_payload] ^= 1;
        std::fs::write(&p, bytes).unwrap();
        let error = Wal::replay_records(&p)
            .err()
            .expect("middle checksum damage must refuse")
            .to_string();
        assert!(error.contains("later bytes"), "unexpected refusal: {error}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_batch_replays_all_or_nothing() {
        let d = std::env::temp_dir().join(format!("turndb-walbatch-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let mk = |id: &str| {
            let (mut r, n) = rec();
            r.id = id.into();
            (r, n)
        };
        let (a, na) = mk("a");
        let (b, nb) = mk("b");
        {
            let mut w = Wal::open_path(&p).unwrap();
            w.append(1, &a, &na).unwrap();
            let tomb = Record {
                id: "z".into(),
                contents: Vec::new(),
                attrs: Vec::new(),
            };
            w.append_batch(
                2,
                &[
                    (a.clone(), na.clone(), false),
                    (tomb, Vec::new(), true),
                    (b.clone(), nb.clone(), false),
                ],
            )
            .unwrap();
            w.sync().unwrap();
        }
        let f = Wal::replay_records(&p).unwrap();
        assert_eq!(f.len(), 4, "standalone + three committed members");
        assert!(f[2].tomb && f[2].record.id == "z");
        assert_eq!(f[3].record.id, "b");

        let flen = std::fs::metadata(&p).unwrap().len();
        let fh = OpenOptions::new().write(true).open(&p).unwrap();
        fh.set_len(flen - (HDR as u64 + 1 + CRC as u64)).unwrap();
        let f = Wal::replay_records(&p).unwrap();
        assert_eq!(
            f.len(),
            1,
            "an uncommitted batch must not replay a prefix of itself"
        );

        {
            let mut w = Wal::open_path(&p).unwrap();
            w.append_batch(3, &[(b.clone(), nb.clone(), false)])
                .unwrap();
            w.sync().unwrap();
        }
        let f = Wal::replay_records(&p).unwrap();
        assert_eq!(
            f.len(),
            2,
            "abandoned members stay discarded: {:?}",
            f.iter().map(|x| &x.record.id).collect::<Vec<_>>()
        );
        assert_eq!(f[1].record.id, "b");

        let p2 = d.join("wal2");
        {
            let mut w = Wal::open_path(&p2).unwrap();
            let mut count = Vec::new();
            put_varint(&mut count, 5);
            w.append_frame(BATCH_COMPLETE_TAG, 1, &count).unwrap();
            w.sync().unwrap();
        }
        assert!(
            Wal::replay_records(&p2).is_err(),
            "a marker naming absent frames must refuse the log"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_batch_marker_must_carry_its_members_sequence() {
        let d = std::env::temp_dir().join(format!("turndb-wal-batch-seq-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let (r, novel) = rec();
        let mut writer = Wal::open_path(&p).unwrap();
        writer.append_batch(7, &[(r, novel, false)]).unwrap();
        writer.sync().unwrap();
        drop(writer);

        let mut bytes = std::fs::read(&p).unwrap();
        let first_len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let marker = HDR + first_len + CRC;
        assert_eq!(bytes[marker], BATCH_COMPLETE_TAG);
        bytes[marker + 1..marker + 9].copy_from_slice(&8u64.to_le_bytes());
        let marker_len =
            u32::from_le_bytes(bytes[marker + 9..marker + 13].try_into().unwrap()) as usize;
        let checksum_at = marker + HDR + marker_len;
        let checksum = crc32fast::hash(&bytes[marker..checksum_at]);
        bytes[checksum_at..checksum_at + CRC].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&p, bytes).unwrap();

        let error = Wal::replay_records(&p)
            .err()
            .expect("marker sequence mismatch must refuse")
            .to_string();
        assert!(error.contains("differs"), "unexpected refusal: {error}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn checksum_valid_malformed_current_frames_are_refused() {
        let d = std::env::temp_dir().join(format!(
            "turndb-wal-semantic-corruption-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();

        let cases: &[(u8, &[u8])] = &[
            (BATCH_COMPLETE_TAG, &[0x80]),
            (BATCH_COMPLETE_TAG, &[0, 0]),
            (TOMB_TAG, &[0xff]),
            (TOMB_TAG, b""),
            (RECORD_TAG, &[0]),
        ];
        for (index, &(tag, payload)) in cases.iter().enumerate() {
            let path = d.join(format!("case-{index}.wal"));
            {
                let mut wal = Wal::open_path(&path).unwrap();
                wal.append_frame(tag, 1, payload).unwrap();
                wal.sync().unwrap();
            }
            assert!(
                Wal::replay_records(&path).is_err(),
                "checksum-valid malformed frame case {index} must be corruption, not a torn tail"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn truncate_empties_the_log() {
        let d = std::env::temp_dir().join(format!("turndb-wal2-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("wal");
        let (r, novel) = rec();
        let mut w = Wal::open_path(&p).unwrap();
        w.append(1, &r, &novel).unwrap();
        w.sync().unwrap();
        assert!(w.bytes() > 0);
        w.truncate().unwrap();
        assert_eq!(w.bytes(), 0);
        assert!(Wal::replay_records(&p).unwrap().is_empty());
        std::fs::remove_dir_all(&d).ok();
    }
}
