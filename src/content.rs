use std::collections::HashMap;
use std::fs::{self, DirEntry, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use sha2::{Digest, Sha256};

/// Content identifier - a 32-byte SHA-256 hash.
///
/// Vendored from the tree crate's `cid` module. The only additions are
/// [`Cid::to_hex`] / [`Cid::from_hex`], which the NAPI bindings need to carry a
/// CID across the JS boundary, and the [`std::fmt::Display`] impl.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Cid(pub [u8; 32]);

impl Cid {
    /// Compute CID from bytes using SHA-256.
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = Sha256::digest(data);
        Cid(hash.into())
    }

    /// Get raw bytes of the CID.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Lowercase hex encoding: the string form used by the NAPI bindings and by
    /// the sharded blob directory names.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(hex_char(byte >> 4));
            out.push(hex_char(byte & 0x0f));
        }
        out
    }

    /// Parse a 64-character lowercase hex content id.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }

        let mut bytes = [0u8; 32];
        for (idx, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            bytes[idx] = (high << 4) | low;
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Display for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

const VALUE_REF_MAGIC: &[u8; 4] = b"PLVB";
const VALUE_REF_VERSION: u8 = 1;
const VALUE_REF_INLINE: u8 = 0;
const VALUE_REF_BLOB: u8 = 1;
const VALUE_REF_HEADER_LEN: usize = 6;
const U64_LEN: usize = 8;

/// Default maximum value size stored directly in leaf nodes by large-value
/// helpers.
pub const DEFAULT_INLINE_VALUE_THRESHOLD: usize = 64 * 1024;

/// Content-addressed reference to an offloaded blob.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlobRef {
    /// Content ID of the blob bytes.
    pub cid: Cid,
    /// Blob length in bytes.
    pub len: u64,
}

impl BlobRef {
    /// Create a blob reference from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            cid: Cid::from_bytes(bytes),
            len: bytes.len() as u64,
        }
    }

    /// Validate that bytes match this content-addressed reference.
    pub fn validate_bytes(&self, bytes: &[u8]) -> Result<(), ContentError> {
        validate_blob_reference(self, bytes)
    }
}

/// Value stored in a leaf by the large-value helper layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueRef {
    /// Inline value bytes.
    Inline(Vec<u8>),
    /// Content-addressed blob reference.
    Blob(BlobRef),
}

impl ValueRef {
    /// Encode this reference as a deterministic leaf value envelope.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Inline(value) => {
                let mut out = Vec::with_capacity(VALUE_REF_HEADER_LEN + U64_LEN + value.len());
                write_header(&mut out, VALUE_REF_INLINE);
                out.extend_from_slice(&(value.len() as u64).to_be_bytes());
                out.extend_from_slice(value);
                out
            }
            Self::Blob(reference) => {
                let mut out = Vec::with_capacity(VALUE_REF_HEADER_LEN + 32 + U64_LEN);
                write_header(&mut out, VALUE_REF_BLOB);
                out.extend_from_slice(reference.cid.as_bytes());
                out.extend_from_slice(&reference.len.to_be_bytes());
                out
            }
        }
    }

    /// Decode a value reference envelope.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ContentError> {
        if !bytes.starts_with(VALUE_REF_MAGIC) {
            return Err(ContentError::Deserialize(
                "value reference missing PLVB magic".to_string(),
            ));
        }
        decode_value_ref(bytes)
    }

    /// Decode a stored value, treating non-envelope bytes as inline values.
    pub fn from_stored_bytes(bytes: &[u8]) -> Result<Self, ContentError> {
        if bytes.starts_with(VALUE_REF_MAGIC) {
            decode_value_ref(bytes)
        } else {
            Ok(Self::Inline(bytes.to_vec()))
        }
    }

    /// Whether raw inline bytes must be escaped to avoid being interpreted as a
    /// value reference envelope.
    pub fn inline_requires_escape(value: &[u8]) -> bool {
        value.starts_with(VALUE_REF_MAGIC)
    }
}

/// Large-value offload policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LargeValueConfig {
    /// Values larger than this many bytes are written to the blob store.
    pub inline_threshold: usize,
}

impl LargeValueConfig {
    /// Create a new policy with an explicit inline threshold.
    pub fn new(inline_threshold: usize) -> Self {
        Self { inline_threshold }
    }

    /// Configure the maximum value size stored directly in leaf nodes.
    pub fn with_inline_threshold(mut self, inline_threshold: usize) -> Self {
        self.inline_threshold = inline_threshold;
        self
    }
}

impl Default for LargeValueConfig {
    fn default() -> Self {
        Self {
            inline_threshold: DEFAULT_INLINE_VALUE_THRESHOLD,
        }
    }
}

/// Errors produced by the content layer.
#[derive(Debug)]
pub enum ContentError {
    /// A value reference envelope or blob bytes could not be decoded.
    Deserialize(String),
    /// A referenced blob is absent from the blob store.
    NotFound(Cid),
    /// A blob store backend failed.
    Store(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deserialize(message) => write!(f, "content deserialize error: {message}"),
            Self::NotFound(cid) => write!(f, "blob not found: {cid}"),
            Self::Store(err) => write!(f, "blob store error: {err}"),
        }
    }
}

impl std::error::Error for ContentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Deserialize(_) | Self::NotFound(_) => None,
            Self::Store(err) => Some(err.as_ref()),
        }
    }
}

// ----------------------------------------------------------------------------
// Core — blob storage
// ----------------------------------------------------------------------------

/// Content-addressed blob storage used by large-value helpers.
pub trait BlobStore: Send + Sync {
    /// Error type for blob storage operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Load a blob by reference.
    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Store bytes and return their content-addressed reference.
    fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef, Self::Error>;

    /// Delete a blob. Deleting a missing blob is not an error.
    fn delete_blob(&self, reference: &BlobRef) -> Result<(), Self::Error>;
}

impl<T: BlobStore> BlobStore for Arc<T> {
    type Error = T::Error;

    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, Self::Error> {
        (**self).get_blob(reference)
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef, Self::Error> {
        (**self).put_blob(bytes)
    }

    fn delete_blob(&self, reference: &BlobRef) -> Result<(), Self::Error> {
        (**self).delete_blob(reference)
    }
}

impl<T: BlobStore + ?Sized> BlobStore for &T {
    type Error = T::Error;

    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, Self::Error> {
        (**self).get_blob(reference)
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef, Self::Error> {
        (**self).put_blob(bytes)
    }

    fn delete_blob(&self, reference: &BlobRef) -> Result<(), Self::Error> {
        (**self).delete_blob(reference)
    }
}

/// Blob stores that can enumerate known blob references.
///
/// This trait is separate from [`BlobStore`] so simple point-read blob stores
/// do not need to expose backend-wide scans. Implementations should return
/// content-addressed blob references, not temporary files or unrelated metadata.
pub trait BlobStoreScan: BlobStore {
    /// List all known blob references.
    ///
    /// Returned references should be sorted by raw CID bytes for deterministic
    /// garbage-collection planning.
    fn list_blob_refs(&self) -> Result<Vec<BlobRef>, Self::Error>;
}

impl<T: BlobStoreScan> BlobStoreScan for Arc<T> {
    fn list_blob_refs(&self) -> Result<Vec<BlobRef>, Self::Error> {
        (**self).list_blob_refs()
    }
}

/// In-memory blob store for tests and lightweight applications.
#[derive(Debug, Default)]
pub struct MemBlobStore {
    data: RwLock<HashMap<Cid, Vec<u8>>>,
}

impl MemBlobStore {
    /// Create an empty in-memory blob store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored blobs.
    pub fn len(&self) -> Result<usize, MemBlobStoreError> {
        self.data
            .read()
            .map(|data| data.len())
            .map_err(|err| MemBlobStoreError(format!("lock poisoned: {err}")))
    }

    /// Whether the store contains no blobs.
    pub fn is_empty(&self) -> Result<bool, MemBlobStoreError> {
        self.len().map(|len| len == 0)
    }
}

/// Error type for [`MemBlobStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemBlobStoreError(String);

impl std::fmt::Display for MemBlobStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemBlobStore error: {}", self.0)
    }
}

impl std::error::Error for MemBlobStoreError {}

impl BlobStore for MemBlobStore {
    type Error = MemBlobStoreError;

    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, Self::Error> {
        let data = self
            .data
            .read()
            .map_err(|err| MemBlobStoreError(format!("lock poisoned: {err}")))?;
        Ok(data.get(&reference.cid).cloned())
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef, Self::Error> {
        let reference = BlobRef::from_bytes(bytes);
        let mut data = self
            .data
            .write()
            .map_err(|err| MemBlobStoreError(format!("lock poisoned: {err}")))?;
        data.entry(reference.cid.clone())
            .or_insert_with(|| bytes.to_vec());
        Ok(reference)
    }

    fn delete_blob(&self, reference: &BlobRef) -> Result<(), Self::Error> {
        let mut data = self
            .data
            .write()
            .map_err(|err| MemBlobStoreError(format!("lock poisoned: {err}")))?;
        data.remove(&reference.cid);
        Ok(())
    }
}

impl BlobStoreScan for MemBlobStore {
    fn list_blob_refs(&self) -> Result<Vec<BlobRef>, Self::Error> {
        let data = self
            .data
            .read()
            .map_err(|err| MemBlobStoreError(format!("lock poisoned: {err}")))?;
        let mut refs = data
            .iter()
            .map(|(cid, bytes)| BlobRef {
                cid: cid.clone(),
                len: bytes.len() as u64,
            })
            .collect::<Vec<_>>();
        sort_blob_refs(&mut refs);
        Ok(refs)
    }
}

/// Durable filesystem-backed blob store.
///
/// Blobs are stored under `blobs/sha256/aa/bb/<cid-hex>` below the configured
/// root directory. Writes are content-addressed and idempotent; a completed
/// write is published with an atomic rename inside the target directory.
#[derive(Clone, Debug)]
pub struct FileBlobStore {
    root: PathBuf,
}

impl FileBlobStore {
    /// Open or create a filesystem blob store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, FileBlobStoreError> {
        let root = root.into();
        let store = Self { root };
        store.ensure_namespace_dir()?;
        Ok(store)
    }

    /// Return the root directory for this blob store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the namespace directory that contains sharded blob files.
    pub fn namespace_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    /// Return the filesystem path for a blob reference.
    pub fn path_for_ref(&self, reference: &BlobRef) -> PathBuf {
        self.blob_path(&reference.cid)
    }

    fn ensure_namespace_dir(&self) -> Result<(), FileBlobStoreError> {
        let path = self.namespace_dir();
        fs::create_dir_all(&path).map_err(|source| FileBlobStoreError::Io { path, source })
    }

    fn blob_path(&self, cid: &Cid) -> PathBuf {
        let hex = cid.to_hex();
        self.namespace_dir()
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex)
    }

    fn temp_path_for(&self, path: &Path) -> PathBuf {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("blob");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nanos))
    }

    fn read_blob_path(
        &self,
        reference: &BlobRef,
        path: &Path,
    ) -> Result<Option<Vec<u8>>, FileBlobStoreError> {
        match fs::read(path) {
            Ok(bytes) => {
                validate_file_blob(reference, path, &bytes)?;
                Ok(Some(bytes))
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(FileBlobStoreError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

impl BlobStore for FileBlobStore {
    type Error = FileBlobStoreError;

    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, Self::Error> {
        self.read_blob_path(reference, &self.blob_path(&reference.cid))
    }

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobRef, Self::Error> {
        let reference = BlobRef::from_bytes(bytes);
        let path = self.blob_path(&reference.cid);

        if let Some(existing) = self.read_blob_path(&reference, &path)? {
            if existing.len() as u64 == reference.len {
                return Ok(reference);
            }
        }

        let Some(parent) = path.parent() else {
            return Err(FileBlobStoreError::InvalidPath {
                path,
                message: "blob path has no parent directory".to_string(),
            });
        };
        fs::create_dir_all(parent).map_err(|source| FileBlobStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let mut temp_path = self.temp_path_for(&path);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                temp_path = self.temp_path_for(&path);
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp_path)
                    .map_err(|source| FileBlobStoreError::Io {
                        path: temp_path.clone(),
                        source,
                    })?
            }
            Err(source) => {
                return Err(FileBlobStoreError::Io {
                    path: temp_path,
                    source,
                });
            }
        };

        if let Err(source) = file.write_all(bytes) {
            let _ = fs::remove_file(&temp_path);
            return Err(FileBlobStoreError::Io {
                path: temp_path,
                source,
            });
        }
        if let Err(source) = file.sync_all() {
            let _ = fs::remove_file(&temp_path);
            return Err(FileBlobStoreError::Io {
                path: temp_path,
                source,
            });
        }
        drop(file);

        if let Err(source) = fs::rename(&temp_path, &path) {
            let _ = fs::remove_file(&temp_path);
            return Err(FileBlobStoreError::Io { path, source });
        }

        Ok(reference)
    }

    fn delete_blob(&self, reference: &BlobRef) -> Result<(), Self::Error> {
        let path = self.blob_path(&reference.cid);
        match fs::remove_file(&path) {
            Ok(()) => {
                remove_empty_blob_dirs(&path, &self.namespace_dir());
                Ok(())
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(FileBlobStoreError::Io { path, source }),
        }
    }
}

impl BlobStoreScan for FileBlobStore {
    fn list_blob_refs(&self) -> Result<Vec<BlobRef>, Self::Error> {
        let namespace = self.namespace_dir();
        if !namespace.exists() {
            return Ok(Vec::new());
        }

        let mut refs = Vec::new();
        for first in read_visible_dir_entries(&namespace)? {
            ensure_shard_dir(&first, 2)?;
            for second in read_visible_dir_entries(&first.path())? {
                ensure_shard_dir(&second, 2)?;
                for blob in read_visible_dir_entries(&second.path())? {
                    let path = blob.path();
                    let file_type = blob.file_type().map_err(|source| FileBlobStoreError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    if !file_type.is_file() {
                        return Err(FileBlobStoreError::InvalidPath {
                            path,
                            message: "blob namespace entry is not a file".to_string(),
                        });
                    }

                    let name = entry_name(&blob)?;
                    let cid =
                        Cid::from_hex(&name).ok_or_else(|| FileBlobStoreError::InvalidPath {
                            path: path.clone(),
                            message: "blob filename is not a 64-byte hex CID".to_string(),
                        })?;
                    if name[0..2] != entry_name(&first)? || name[2..4] != entry_name(&second)? {
                        return Err(FileBlobStoreError::InvalidPath {
                            path,
                            message: "blob CID does not match shard directories".to_string(),
                        });
                    }

                    let len = blob
                        .metadata()
                        .map_err(|source| FileBlobStoreError::Io {
                            path: blob.path(),
                            source,
                        })?
                        .len();
                    refs.push(BlobRef { cid, len });
                }
            }
        }

        sort_blob_refs(&mut refs);
        Ok(refs)
    }
}

/// Error type for [`FileBlobStore`].
#[derive(Debug)]
pub enum FileBlobStoreError {
    /// Filesystem I/O failed for the given path.
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A file's bytes do not match its content-addressed blob reference.
    InvalidBlob {
        /// Path containing the invalid blob bytes.
        path: PathBuf,
        /// Validation failure description.
        message: String,
    },
    /// The blob store namespace contains an invalid path or filename.
    InvalidPath {
        /// Invalid path.
        path: PathBuf,
        /// Validation failure description.
        message: String,
    },
}

impl std::fmt::Display for FileBlobStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    f,
                    "file blob store I/O error at {}: {source}",
                    path.display()
                )
            }
            Self::InvalidBlob { path, message } => {
                write!(f, "invalid file blob at {}: {message}", path.display())
            }
            Self::InvalidPath { path, message } => {
                write!(f, "invalid file blob path {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for FileBlobStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidBlob { .. } | Self::InvalidPath { .. } => None,
        }
    }
}

// ----------------------------------------------------------------------------
// Core — encode / resolve
// ----------------------------------------------------------------------------

/// Encode `value` as a leaf value: offload it to `blob_store` when it exceeds
/// `config.inline_threshold`, otherwise store it inline (escaping inline values
/// that would otherwise look like a reference envelope).
pub fn encode_stored_value<B>(
    blob_store: &B,
    value: Vec<u8>,
    config: &LargeValueConfig,
) -> Result<Vec<u8>, ContentError>
where
    B: BlobStore,
{
    if value.len() > config.inline_threshold {
        let reference = blob_store
            .put_blob(&value)
            .map_err(|err| ContentError::Store(Box::new(err)))?;
        validate_blob_reference(&reference, &value)?;
        return Ok(ValueRef::Blob(reference).to_bytes());
    }

    if ValueRef::inline_requires_escape(&value) {
        Ok(ValueRef::Inline(value).to_bytes())
    } else {
        Ok(value)
    }
}

/// Resolve a stored leaf value, loading offloaded bytes from `blob_store`.
pub fn resolve_stored_value<B>(blob_store: &B, stored: &[u8]) -> Result<Vec<u8>, ContentError>
where
    B: BlobStore,
{
    match ValueRef::from_stored_bytes(stored)? {
        ValueRef::Inline(value) => Ok(value),
        ValueRef::Blob(reference) => {
            let bytes = blob_store
                .get_blob(&reference)
                .map_err(|err| ContentError::Store(Box::new(err)))?
                .ok_or_else(|| ContentError::NotFound(reference.cid.clone()))?;
            validate_blob_reference(&reference, &bytes)?;
            Ok(bytes)
        }
    }
}

fn write_header(out: &mut Vec<u8>, tag: u8) {
    out.extend_from_slice(VALUE_REF_MAGIC);
    out.push(VALUE_REF_VERSION);
    out.push(tag);
}

fn decode_value_ref(bytes: &[u8]) -> Result<ValueRef, ContentError> {
    if bytes.len() < VALUE_REF_HEADER_LEN {
        return Err(value_ref_error("value reference header is truncated"));
    }
    if bytes[..VALUE_REF_MAGIC.len()] != VALUE_REF_MAGIC[..] {
        return Err(value_ref_error("value reference missing PLVB magic"));
    }
    if bytes[4] != VALUE_REF_VERSION {
        return Err(value_ref_error(format!(
            "unsupported value reference version {}",
            bytes[4]
        )));
    }

    match bytes[5] {
        VALUE_REF_INLINE => decode_inline_ref(bytes),
        VALUE_REF_BLOB => decode_blob_ref(bytes),
        tag => Err(value_ref_error(format!(
            "unknown value reference tag {tag}"
        ))),
    }
}

fn decode_inline_ref(bytes: &[u8]) -> Result<ValueRef, ContentError> {
    let mut offset = VALUE_REF_HEADER_LEN;
    let len = usize::try_from(read_u64(bytes, &mut offset, "inline value length")?)
        .map_err(|_| value_ref_error("inline value length exceeds platform limits"))?;
    if bytes.len().saturating_sub(offset) != len {
        return Err(value_ref_error(
            "inline value length does not match payload",
        ));
    }
    Ok(ValueRef::Inline(bytes[offset..].to_vec()))
}

fn decode_blob_ref(bytes: &[u8]) -> Result<ValueRef, ContentError> {
    let expected_len = VALUE_REF_HEADER_LEN + 32 + U64_LEN;
    if bytes.len() != expected_len {
        return Err(value_ref_error("blob reference length is invalid"));
    }

    let mut cid_bytes = [0u8; 32];
    cid_bytes.copy_from_slice(&bytes[VALUE_REF_HEADER_LEN..VALUE_REF_HEADER_LEN + 32]);
    let mut offset = VALUE_REF_HEADER_LEN + 32;
    let len = read_u64(bytes, &mut offset, "blob length")?;
    Ok(ValueRef::Blob(BlobRef {
        cid: Cid(cid_bytes),
        len,
    }))
}

fn read_u64(bytes: &[u8], offset: &mut usize, context: &str) -> Result<u64, ContentError> {
    let end = offset.saturating_add(U64_LEN);
    let Some(raw) = bytes.get(*offset..end) else {
        return Err(value_ref_error(format!("{context} is truncated")));
    };
    *offset = end;
    Ok(u64::from_be_bytes(raw.try_into().map_err(|_| {
        value_ref_error(format!("{context} has invalid length"))
    })?))
}

fn validate_blob_reference(reference: &BlobRef, bytes: &[u8]) -> Result<(), ContentError> {
    if reference.len != bytes.len() as u64 {
        return Err(value_ref_error("blob length does not match reference"));
    }

    let actual = Cid::from_bytes(bytes);
    if actual != reference.cid {
        return Err(value_ref_error("blob CID does not match reference"));
    }

    Ok(())
}

fn value_ref_error(message: impl Into<String>) -> ContentError {
    ContentError::Deserialize(format!("invalid value reference: {}", message.into()))
}

fn validate_file_blob(
    reference: &BlobRef,
    path: &Path,
    bytes: &[u8],
) -> Result<(), FileBlobStoreError> {
    reference
        .validate_bytes(bytes)
        .map_err(|err| FileBlobStoreError::InvalidBlob {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
}

fn read_visible_dir_entries(path: &Path) -> Result<Vec<DirEntry>, FileBlobStoreError> {
    let mut entries = Vec::new();
    let dir = fs::read_dir(path).map_err(|source| FileBlobStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    for entry in dir {
        let entry = entry.map_err(|source| FileBlobStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let name = entry_name(&entry)?;
        if name.starts_with('.') {
            continue;
        }
        entries.push(entry);
    }

    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn ensure_shard_dir(entry: &DirEntry, len: usize) -> Result<(), FileBlobStoreError> {
    let path = entry.path();
    let file_type = entry.file_type().map_err(|source| FileBlobStoreError::Io {
        path: path.clone(),
        source,
    })?;
    if !file_type.is_dir() {
        return Err(FileBlobStoreError::InvalidPath {
            path,
            message: "blob shard entry is not a directory".to_string(),
        });
    }

    let name = entry_name(entry)?;
    if name.len() != len || !name.as_bytes().iter().all(|byte| is_lower_hex_byte(*byte)) {
        return Err(FileBlobStoreError::InvalidPath {
            path,
            message: "blob shard directory is not lowercase hex".to_string(),
        });
    }

    Ok(())
}

fn entry_name(entry: &DirEntry) -> Result<String, FileBlobStoreError> {
    entry
        .file_name()
        .into_string()
        .map_err(|name| FileBlobStoreError::InvalidPath {
            path: entry.path(),
            message: format!("path is not valid UTF-8: {name:?}"),
        })
}

fn remove_empty_blob_dirs(blob_path: &Path, namespace: &Path) {
    let Some(second) = blob_path.parent() else {
        return;
    };
    let _ = fs::remove_dir(second);
    let Some(first) = second.parent() else {
        return;
    };
    if first != namespace {
        let _ = fs::remove_dir(first);
    }
}

fn sort_blob_refs(blobs: &mut [BlobRef]) {
    blobs.sort_by(|left, right| left.cid.as_bytes().cmp(right.cid.as_bytes()));
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => '0',
    }
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn is_lower_hex_byte(value: u8) -> bool {
    matches!(value, b'0'..=b'9' | b'a'..=b'f')
}

// ----------------------------------------------------------------------------
// NAPI bindings
// ----------------------------------------------------------------------------

/// Convert a [`ContentError`] into a JS-thrown error.
fn to_napi(context: &str, err: ContentError) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {err}"))
}

/// Convert a JS blob reference into its core form.
fn to_blob_ref(reference: &JsBlobRef) -> napi::Result<BlobRef> {
    let Some(cid) = Cid::from_hex(&reference.cid) else {
        return Err(napi::Error::from_reason(format!(
            "blob cid must be 64 lowercase hex characters: {}",
            reference.cid
        )));
    };
    Ok(BlobRef {
        cid,
        len: reference.len as u64,
    })
}

/// Convert a core blob reference into its JS form.
fn from_blob_ref(reference: &BlobRef) -> JsBlobRef {
    JsBlobRef {
        cid: reference.cid.to_hex(),
        len: reference.len as f64,
    }
}

/// Which side of [`ValueRef`] a JS value reference occupies.
#[napi(string_enum)]
pub enum ValueRefKind {
    /// Inline value bytes.
    Inline,
    /// Content-addressed blob reference.
    Blob,
}

/// A content-addressed blob reference, as surfaced to JavaScript.
#[napi(object)]
pub struct JsBlobRef {
    /// Lowercase hex content id.
    pub cid: String,
    /// Blob length in bytes. A JS `number`: the core value is a `u64`, and
    /// integer-precise byte counts above 2^53 are not reachable in practice.
    pub len: f64,
}

/// A [`ValueRef`], as surfaced to JavaScript: inline values carry `value`, blob
/// reference values carry `cid` and `len`.
#[napi(object)]
pub struct JsValueRef {
    /// Which variant this is.
    pub kind: ValueRefKind,
    /// Inline bytes, present when `kind` is `"Inline"`.
    pub value: Option<Buffer>,
    /// Lowercase hex content id, present when `kind` is `"Blob"`.
    pub cid: Option<String>,
    /// Blob length in bytes, present when `kind` is `"Blob"`.
    pub len: Option<f64>,
}

impl From<&ValueRef> for JsValueRef {
    fn from(value: &ValueRef) -> Self {
        match value {
            ValueRef::Inline(bytes) => Self {
                kind: ValueRefKind::Inline,
                value: Some(Buffer::from(bytes.clone())),
                cid: None,
                len: None,
            },
            ValueRef::Blob(reference) => Self {
                kind: ValueRefKind::Blob,
                value: None,
                cid: Some(reference.cid.to_hex()),
                len: Some(reference.len as f64),
            },
        }
    }
}

/// Convert a JS value reference into its core form.
fn to_value_ref(value: JsValueRef) -> napi::Result<ValueRef> {
    match value.kind {
        ValueRefKind::Inline => {
            let bytes = value
                .value
                .ok_or_else(|| napi::Error::from_reason("inline value reference needs `value`"))?;
            Ok(ValueRef::Inline(bytes.to_vec()))
        }
        ValueRefKind::Blob => {
            let cid = value
                .cid
                .ok_or_else(|| napi::Error::from_reason("blob value reference needs `cid`"))?;
            let len = value
                .len
                .ok_or_else(|| napi::Error::from_reason("blob value reference needs `len`"))?;
            let Some(cid) = Cid::from_hex(&cid) else {
                return Err(napi::Error::from_reason(format!(
                    "blob cid must be 64 lowercase hex characters: {cid}"
                )));
            };
            Ok(ValueRef::Blob(BlobRef {
                cid,
                len: len as u64,
            }))
        }
    }
}

/// In-memory [`MemBlobStore`] exposed to JavaScript.
///
/// Point-reads, writes and deletes are shared with every [`Content`] wrapper
/// built through [`Content::from_mem_store`]: they inherit this instance's
/// `Arc`, so writes from one side are visible from the other.
#[napi(js_name = "MemBlobStore")]
pub struct JsMemBlobStore {
    inner: Arc<MemBlobStore>,
}

#[napi]
impl JsMemBlobStore {
    /// Create an empty in-memory blob store.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MemBlobStore::new()),
        }
    }

    /// Number of stored blobs.
    #[napi(getter)]
    pub fn len(&self) -> napi::Result<u32> {
        let len = self.inner.len().map_err(|err| to_napi("len", into(err)))?;
        Ok(len as u32)
    }

    /// Whether the store contains no blobs.
    #[napi]
    pub fn is_empty(&self) -> napi::Result<bool> {
        self.inner
            .is_empty()
            .map_err(|err| to_napi("is_empty", into(err)))
    }

    /// Store `bytes` and return their content-addressed reference. Idempotent.
    #[napi]
    pub fn put(&self, bytes: Buffer) -> napi::Result<JsBlobRef> {
        let reference = self
            .inner
            .put_blob(&bytes)
            .map_err(|err| to_napi("put_blob", into(err)))?;
        Ok(from_blob_ref(&reference))
    }

    /// Load the bytes for `reference`, or `null` when absent.
    #[napi]
    pub fn get(&self, reference: JsBlobRef) -> napi::Result<Option<Buffer>> {
        let reference = to_blob_ref(&reference)?;
        let bytes = self
            .inner
            .get_blob(&reference)
            .map_err(|err| to_napi("get_blob", into(err)))?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        reference
            .validate_bytes(&bytes)
            .map_err(|err| to_napi("validate_blob", err))?;
        Ok(Some(Buffer::from(bytes)))
    }

    /// Delete `reference`. Deleting a missing blob is not an error.
    #[napi]
    pub fn delete(&self, reference: JsBlobRef) -> napi::Result<()> {
        let reference = to_blob_ref(&reference)?;
        self.inner
            .delete_blob(&reference)
            .map_err(|err| to_napi("delete_blob", into(err)))
    }

    /// Every stored blob reference, sorted by raw CID bytes.
    #[napi]
    pub fn list_refs(&self) -> napi::Result<Vec<JsBlobRef>> {
        let refs = self
            .inner
            .list_blob_refs()
            .map_err(|err| to_napi("list_blob_refs", into(err)))?;
        Ok(refs.iter().map(from_blob_ref).collect())
    }
}

/// Filesystem-backed [`FileBlobStore`] exposed to JavaScript.
///
/// Blobs live under `blobs/sha256/aa/bb/<cid-hex>` below `root`. Writes are
/// content-addressed, idempotent, and published with an atomic rename.
#[napi(js_name = "FileBlobStore")]
pub struct JsFileBlobStore {
    inner: Arc<FileBlobStore>,
}

#[napi]
impl JsFileBlobStore {
    /// Open or create a filesystem blob store rooted at `root`.
    #[napi(constructor)]
    pub fn new(root: String) -> napi::Result<Self> {
        let inner = FileBlobStore::open(root)
            .map_err(|err| napi::Error::from_reason(format!("FileBlobStore::open: {err}")))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// The root directory this store was opened with.
    #[napi(getter)]
    pub fn root(&self) -> String {
        self.inner.root().display().to_string()
    }

    /// The namespace directory containing sharded blob files.
    #[napi(getter)]
    pub fn namespace_dir(&self) -> String {
        self.inner.namespace_dir().display().to_string()
    }

    /// The filesystem path holding the bytes for `reference`.
    #[napi]
    pub fn path_for_ref(&self, reference: JsBlobRef) -> napi::Result<String> {
        let reference = to_blob_ref(&reference)?;
        Ok(self.inner.path_for_ref(&reference).display().to_string())
    }

    /// Store `bytes` and return their content-addressed reference. Idempotent.
    #[napi]
    pub fn put(&self, bytes: Buffer) -> napi::Result<JsBlobRef> {
        let reference = self
            .inner
            .put_blob(&bytes)
            .map_err(|err| to_napi("put_blob", into(err)))?;
        Ok(from_blob_ref(&reference))
    }

    /// Load the bytes for `reference`, or `null` when absent.
    #[napi]
    pub fn get(&self, reference: JsBlobRef) -> napi::Result<Option<Buffer>> {
        let reference = to_blob_ref(&reference)?;
        let bytes = self
            .inner
            .get_blob(&reference)
            .map_err(|err| to_napi("get_blob", into(err)))?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        reference
            .validate_bytes(&bytes)
            .map_err(|err| to_napi("validate_blob", err))?;
        Ok(Some(Buffer::from(bytes)))
    }

    /// Delete `reference`. Deleting a missing blob is not an error.
    #[napi]
    pub fn delete(&self, reference: JsBlobRef) -> napi::Result<()> {
        let reference = to_blob_ref(&reference)?;
        self.inner
            .delete_blob(&reference)
            .map_err(|err| to_napi("delete_blob", into(err)))
    }

    /// Every stored blob reference, sorted by raw CID bytes.
    #[napi]
    pub fn list_refs(&self) -> napi::Result<Vec<JsBlobRef>> {
        let refs = self
            .inner
            .list_blob_refs()
            .map_err(|err| to_napi("list_blob_refs", into(err)))?;
        Ok(refs.iter().map(from_blob_ref).collect())
    }
}

/// Wrap a backend error inside [`ContentError::Store`].
fn into<E: std::error::Error + Send + Sync + 'static>(err: E) -> ContentError {
    ContentError::Store(Box::new(err))
}

/// One concrete blob backend owned by [`Content`].
///
/// The core [`encode_stored_value`] / [`resolve_stored_value`] helpers are
/// generic over `B: BlobStore`, so each arm forwards to them monomorphically
/// instead of going through a trait object — the backends' associated error
/// types differ.
enum BlobStoreHandle {
    Mem(Arc<MemBlobStore>),
    File(Arc<FileBlobStore>),
}

impl BlobStoreHandle {
    fn encode_stored_value(
        &self,
        value: Vec<u8>,
        config: &LargeValueConfig,
    ) -> Result<Vec<u8>, ContentError> {
        match self {
            Self::Mem(store) => encode_stored_value(store.as_ref(), value, config),
            Self::File(store) => encode_stored_value(store.as_ref(), value, config),
        }
    }

    fn resolve_stored_value(&self, stored: &[u8]) -> Result<Vec<u8>, ContentError> {
        match self {
            Self::Mem(store) => resolve_stored_value(store.as_ref(), stored),
            Self::File(store) => resolve_stored_value(store.as_ref(), stored),
        }
    }

    fn get_blob(&self, reference: &BlobRef) -> Result<Option<Vec<u8>>, ContentError> {
        match self {
            Self::Mem(store) => store.get_blob(reference).map_err(into),
            Self::File(store) => store.get_blob(reference).map_err(into),
        }
    }
}

/// Large-value helper bound to one blob store and one inline threshold.
///
/// [`Content::encode`] produces what a leaf should store — either the raw bytes
/// (for values at or below the threshold) or a compact blob-reference envelope.
/// [`Content::resolve`] is the inverse, transparently loading offloaded bytes.
#[napi(js_name = "Content")]
pub struct Content {
    store: BlobStoreHandle,
    config: LargeValueConfig,
}

#[napi]
impl Content {
    /// Bind to an existing [`MemBlobStore`]. `inline_threshold` defaults to
    /// [`DEFAULT_INLINE_VALUE_THRESHOLD`] (64 KiB) when `null`.
    #[napi(factory)]
    pub fn from_mem_store(store: &JsMemBlobStore, inline_threshold: Option<u32>) -> Self {
        Self::with_store(
            BlobStoreHandle::Mem(Arc::clone(&store.inner)),
            inline_threshold,
        )
    }

    /// Bind to an existing [`FileBlobStore`]. `inline_threshold` defaults to
    /// [`DEFAULT_INLINE_VALUE_THRESHOLD`] (64 KiB) when `null`.
    #[napi(factory)]
    pub fn from_file_store(store: &JsFileBlobStore, inline_threshold: Option<u32>) -> Self {
        Self::with_store(
            BlobStoreHandle::File(Arc::clone(&store.inner)),
            inline_threshold,
        )
    }

    fn with_store(store: BlobStoreHandle, inline_threshold: Option<u32>) -> Self {
        let config = match inline_threshold {
            Some(threshold) => LargeValueConfig::new(threshold as usize),
            None => LargeValueConfig::default(),
        };
        Self { store, config }
    }

    /// Values larger than this many bytes are offloaded to the blob store.
    #[napi(getter)]
    pub fn inline_threshold(&self) -> u32 {
        self.config.inline_threshold as u32
    }

    /// Set the inline threshold in bytes.
    #[napi(setter)]
    pub fn set_inline_threshold(&mut self, inline_threshold: u32) {
        self.config.inline_threshold = inline_threshold as usize;
    }

    /// Encode `value` for storage in a leaf: raw bytes when they fit inline, a
    /// blob-reference envelope otherwise. Inline values that begin with the
    /// `PLVB` envelope magic are escaped so they survive the round trip.
    #[napi]
    pub fn encode(&self, value: Buffer) -> napi::Result<Buffer> {
        let stored = self
            .store
            .encode_stored_value(value.to_vec(), &self.config)
            .map_err(|err| to_napi("encode", err))?;
        Ok(Buffer::from(stored))
    }

    /// Decode a stored leaf value into its inline-or-blob form without loading
    /// the referenced bytes.
    #[napi]
    pub fn decode(&self, stored: Buffer) -> napi::Result<JsValueRef> {
        let value = ValueRef::from_stored_bytes(&stored).map_err(|err| to_napi("decode", err))?;
        Ok(JsValueRef::from(&value))
    }

    /// Resolve a stored leaf value to its bytes, loading offloaded blobs.
    ///
    /// Throws when the referenced blob is missing or its bytes do not match the
    /// recorded content id.
    #[napi]
    pub fn resolve(&self, stored: Buffer) -> napi::Result<Buffer> {
        let bytes = self
            .store
            .resolve_stored_value(&stored)
            .map_err(|err| to_napi("resolve", err))?;
        Ok(Buffer::from(bytes))
    }

    /// Load blob bytes directly by reference, bypassing value envelopes.
    #[napi]
    pub fn get_blob(&self, reference: JsBlobRef) -> napi::Result<Option<Buffer>> {
        let reference = to_blob_ref(&reference)?;
        let bytes = self
            .store
            .get_blob(&reference)
            .map_err(|err| to_napi("get_blob", err))?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        reference
            .validate_bytes(&bytes)
            .map_err(|err| to_napi("get_blob", err))?;
        Ok(Some(Buffer::from(bytes)))
    }
}

/// Default maximum value size stored directly in leaf nodes.
#[napi]
pub const DEFAULT_CONTENT_INLINE_VALUE_THRESHOLD: u32 = DEFAULT_INLINE_VALUE_THRESHOLD as u32;

/// Compute the content-addressed reference for `bytes` without storing them.
#[napi]
pub fn blob_ref_from_bytes(bytes: Buffer) -> JsBlobRef {
    from_blob_ref(&BlobRef::from_bytes(&bytes))
}

/// Whether `bytes` match `reference` — same length and same content id.
#[napi(ts_return_type = "boolean")]
pub fn validate_blob_bytes(reference: JsBlobRef, bytes: Buffer) -> napi::Result<bool> {
    let reference = to_blob_ref(&reference)?;
    Ok(reference.validate_bytes(&bytes).is_ok())
}

/// Whether raw inline bytes would be mistaken for a value reference envelope.
#[napi]
pub fn inline_requires_escape(value: Buffer) -> bool {
    ValueRef::inline_requires_escape(&value)
}

/// Encode a value reference into its deterministic leaf-value envelope bytes.
#[napi]
pub fn encode_value_ref(value: JsValueRef) -> napi::Result<Buffer> {
    let value = to_value_ref(value)?;
    Ok(Buffer::from(value.to_bytes()))
}

/// Decode a value reference envelope. Throws when `bytes` is not an envelope;
/// use [`ValueRef::from_stored_bytes`]-style callers ([`Content::decode`]) when
/// unmarked inline bytes must also be accepted.
#[napi]
pub fn decode_value_ref_bytes(bytes: Buffer) -> napi::Result<JsValueRef> {
    let value = ValueRef::from_bytes(&bytes).map_err(|err| to_napi("decode_value_ref", err))?;
    Ok(JsValueRef::from(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid_of(bytes: &[u8]) -> Cid {
        Cid::from_bytes(bytes)
    }

    #[test]
    fn cid_hex_round_trips() {
        let cid = cid_of(b"payload");
        assert_eq!(Cid::from_hex(&cid.to_hex()), Some(cid));
        assert_eq!(cid.to_hex().len(), 64);
        assert_eq!(Cid::from_hex("zz01"), None);
    }

    #[test]
    fn value_ref_round_trips_inline_and_blob_refs() {
        let inline = ValueRef::Inline(b"PLVB-user-data".to_vec());
        assert_eq!(
            ValueRef::from_bytes(&inline.to_bytes()).unwrap(),
            inline.clone()
        );
        assert_eq!(
            ValueRef::from_stored_bytes(&inline.to_bytes()).unwrap(),
            inline
        );

        let blob = ValueRef::Blob(BlobRef::from_bytes(b"large"));
        assert_eq!(
            ValueRef::from_bytes(&blob.to_bytes()).unwrap(),
            blob.clone()
        );
        assert_eq!(ValueRef::from_stored_bytes(&blob.to_bytes()).unwrap(), blob);
    }

    #[test]
    fn stored_value_without_envelope_decodes_as_inline() {
        assert_eq!(
            ValueRef::from_stored_bytes(b"raw").unwrap(),
            ValueRef::Inline(b"raw".to_vec())
        );
    }

    #[test]
    fn mem_blob_store_is_content_addressed_and_idempotent() {
        let store = MemBlobStore::new();
        let first = store.put_blob(b"payload").unwrap();
        let second = store.put_blob(b"payload").unwrap();

        assert_eq!(first, second);
        assert_eq!(store.len().unwrap(), 1);
        assert_eq!(store.get_blob(&first).unwrap(), Some(b"payload".to_vec()));

        store.delete_blob(&first).unwrap();
        assert_eq!(store.get_blob(&first).unwrap(), None);
    }

    #[test]
    fn encode_respects_threshold_and_escapes_like叶绿_bytes() {
        let store = MemBlobStore::new();
        let config = LargeValueConfig::new(4);

        let small = encode_stored_value(&store, b"1234".to_vec(), &config).unwrap();
        assert_eq!(small, b"1234".to_vec());

        let escaped = encode_stored_value(&store, b"PLVB".to_vec(), &config).unwrap();
        assert_eq!(
            ValueRef::from_stored_bytes(&escaped).unwrap(),
            ValueRef::Inline(b"PLVB".to_vec())
        );

        let large = encode_stored_value(&store, b"12345".to_vec(), &config).unwrap();
        match ValueRef::from_stored_bytes(&large).unwrap() {
            ValueRef::Blob(reference) => {
                assert_eq!(reference.len, 5);
                assert_eq!(reference.cid, cid_of(b"12345"));
            }
            other => panic!("expected blob reference, got {other:?}"),
        }

        assert_eq!(resolve_stored_value(&store, &large).unwrap(), b"12345");
        assert_eq!(resolve_stored_value(&store, &escaped).unwrap(), b"PLVB");
    }

    #[test]
    fn resolve_rejects_missing_blobs() {
        let store = MemBlobStore::new();
        let stored = ValueRef::Blob(BlobRef::from_bytes(b"gone")).to_bytes();
        let err = resolve_stored_value(&store, &stored).unwrap_err();
        assert!(matches!(err, ContentError::NotFound(_)));
    }

    #[test]
    fn value_ref_envelope_rejects_truncated_and_unknown_tags() {
        assert!(ValueRef::from_bytes(b"PLVB").is_err());
        assert!(ValueRef::from_bytes(&[b'P', b'L', b'V', b'B', 1, 9, 0, 0]).is_err());
        assert!(ValueRef::from_bytes(&[b'P', b'L', b'V', b'B', 2, 0]).is_err());
    }
}
