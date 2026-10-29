//! Buffered, tokio-compatible async I/O over an `object_store` backend, exposed to JS.
//!
//! This wraps the upstream [`object_store::buffered`] module — `BufReader` (a lazily
//! range-fetching, tokio `AsyncRead`/`AsyncSeek` reader) and `BufWriter` (an adaptive
//! put/multipart `AsyncWrite` writer). We wrap the public types; the implementation is
//! the `object_store` crate's own (MIT), not vendored here.
//!
//! Each napi method is *synchronous* from JS's point of view but drives its future on the
//! shared tokio runtime via [`runtime`](crate::object_store::base::runtime)`().block_on(..)`,
//! matching the rest of this module (`GenericStore::get`/`put`/`list` in `base.rs`).

use std::io::SeekFrom;

use ::object_store::ObjectStoreExt;
use ::object_store::path::Path as OsPath;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::{err, err_str};
use crate::object_store::base::{GenericObjectStore, runtime};

/// Default in-memory buffer size for [`ObjectStoreBufReader`] (1 MiB), matching upstream.
pub const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024;
/// Default capacity for [`ObjectStoreBufWriter`] (10 MiB), matching upstream.
pub const DEFAULT_WRITE_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// Options for constructing an [`ObjectStoreBufReader`].
#[napi(object)]
pub struct BufReaderOptions {
    /// Internal read-ahead buffer size in bytes. `null` uses the 1 MiB default.
    pub capacity: Option<u32>,
}

/// Options for constructing an [`ObjectStoreBufWriter`].
#[napi(object)]
pub struct BufWriterOptions {
    /// Bytes buffered before switching from `put` to multipart upload. `null` = 10 MiB.
    pub capacity: Option<u32>,
    /// Max in-flight multipart requests. `null` = 8.
    pub max_concurrency: Option<u32>,
}

/// Where a [`ObjectStoreBufReader::seek`] is relative to.
#[napi(string_enum)]
pub enum SeekWhence {
    /// Relative to the start of the object.
    Start,
    /// Relative to the current cursor.
    Current,
    /// Relative to the end of the object.
    End,
}

/// A lazily range-fetching, seekable buffered reader over a single object.
///
/// Unlike [`ObjectStoreReader`](crate::object_store::base::ObjectStoreReader) (which loads the
/// whole object up front), this fetches only the ranges demanded by [`read`](Self::read), so it
/// suits large or remote objects scanned sequentially or seeked into.
#[napi]
pub struct ObjectStoreBufReader {
    inner: ::object_store::buffered::BufReader,
    size: u64,
}

#[napi]
impl ObjectStoreBufReader {
    /// Open a buffered reader for `path`. The object's size is resolved via `head` at construction.
    #[napi(constructor)]
    pub fn new(
        store: &GenericObjectStore,
        path: String,
        options: Option<BufReaderOptions>,
    ) -> napi::Result<Self> {
        let backend = store.backend();
        let loc = OsPath::parse(&path).map_err(|e| err("buf_reader", e))?;
        let meta = runtime()
            .block_on(backend.head(&loc))
            .map_err(|e| err("buf_reader", e))?;
        let capacity = options
            .map(|o| o.capacity)
            .flatten()
            .map(|c| c as usize)
            .unwrap_or(DEFAULT_BUFFER_SIZE);
        let inner = ::object_store::buffered::BufReader::with_capacity(backend, &meta, capacity);
        Ok(Self {
            inner,
            size: meta.size as u64,
        })
    }

    /// Total size of the object in bytes.
    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.size as f64
    }

    /// Read up to `max_bytes` bytes, returning a `Buffer`. Returns an empty `Buffer` at EOF.
    #[napi]
    pub fn read(&mut self, max_bytes: f64) -> napi::Result<Buffer> {
        let max = if max_bytes <= 0.0 {
            0
        } else {
            max_bytes as usize
        };
        if max == 0 {
            return Ok(Buffer::from(Vec::new()));
        }
        let mut buf = vec![0u8; max];
        let n = runtime()
            .block_on(self.inner.read(&mut buf))
            .map_err(|e| err("buf_reader_read", e))?;
        buf.truncate(n);
        Ok(Buffer::from(buf))
    }

    /// Seek to a position, returning the new absolute offset. `whence` selects the reference
    /// point (start / current / end); negative `offset` with `Current`/`End` is allowed.
    #[napi]
    pub fn seek(&mut self, offset: f64, whence: SeekWhence) -> napi::Result<f64> {
        let from = match whence {
            SeekWhence::Start => SeekFrom::Start(offset.max(0.0) as u64),
            SeekWhence::Current => SeekFrom::Current(offset as i64),
            SeekWhence::End => SeekFrom::End(offset as i64),
        };
        let pos = runtime()
            .block_on(self.inner.seek(from))
            .map_err(|_| err_str("buf_reader_seek", "seek would overflow the object bounds"))?;
        Ok(pos as f64)
    }
}

/// An adaptive buffered writer: writes up to `capacity` bytes via `put`, then transparently
/// switches to a multipart upload for larger or streaming writes.
#[napi]
pub struct ObjectStoreBufWriter {
    inner: ::object_store::buffered::BufWriter,
}

#[napi]
impl ObjectStoreBufWriter {
    /// Open a buffered writer for `path`.
    #[napi(constructor)]
    pub fn new(
        store: &GenericObjectStore,
        path: String,
        options: Option<BufWriterOptions>,
    ) -> napi::Result<Self> {
        let backend = store.backend();
        let loc = OsPath::parse(&path).map_err(|e| err("buf_writer", e))?;
        let (capacity, max_concurrency) = match options {
            Some(o) => (o.capacity, o.max_concurrency),
            None => (None, None),
        };
        let capacity = capacity
            .map(|c| c as usize)
            .unwrap_or(DEFAULT_WRITE_BUFFER_SIZE);
        let max_concurrency = max_concurrency.map(|c| c as usize).unwrap_or(8);
        let inner = ::object_store::buffered::BufWriter::with_capacity(backend, loc, capacity)
            .with_max_concurrency(max_concurrency);
        Ok(Self { inner })
    }

    /// Append `data`, returning the number of bytes written (always `data.length`).
    #[napi]
    pub fn write(&mut self, data: Buffer) -> napi::Result<u32> {
        runtime()
            .block_on(self.inner.write_all(&data))
            .map_err(|e| err("buf_writer_write", e))?;
        Ok(data.len() as u32)
    }

    /// Flush buffered data (a no-op once a multipart upload has begun).
    #[napi]
    pub fn flush(&mut self) -> napi::Result<()> {
        runtime()
            .block_on(self.inner.flush())
            .map_err(|e| err("buf_writer_flush", e))
    }

    /// Finalize the upload (a single `put`, or `finish` the multipart upload).
    ///
    /// Must be called exactly once; subsequent `write` calls will panic in the underlying
    /// `object_store` writer, so callers should treat the writer as consumed after this.
    #[napi]
    pub fn shutdown(&mut self) -> napi::Result<()> {
        runtime()
            .block_on(self.inner.shutdown())
            .map_err(|e| err("buf_writer_shutdown", e))
    }

    /// Abort any in-progress multipart upload and discard buffered state.
    #[napi]
    pub fn abort(&mut self) -> napi::Result<()> {
        runtime()
            .block_on(self.inner.abort())
            .map_err(|e| err("buf_writer_abort", e))
    }
}
