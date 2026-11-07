//! Operation Store (`op-store`) NAPI bindings for Jujutsu (jj) repositories.
//!
//! Mirrors `jj_core::op_store` and `jj_lib::simple_op_store`, providing storage for
//! operations (`Operation`) and repository views (`View`) under `.jj/repo/op_store/`.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Debug;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use napi::bindgen_prelude::BigInt;
use napi_derive::napi;
use thiserror::Error;

use crate::error::{err, err_str};
use crate::object_store::op_heads::{
    OpMetadata, OpRefTarget, OpTimestampRange, OperationRecord, OperationRecordDto, ViewRecord,
    ViewRecordDto,
};

// BLAKE2b-512 / BLAKE3 hash length in bytes
pub const OPERATION_ID_LENGTH: usize = 64;
pub const VIEW_ID_LENGTH: usize = 64;

/// Error type for operation store operations.
#[derive(Debug, Error)]
pub enum OpStoreError {
    #[error("Failed to read {object_type} {hash}: {source:?}")]
    ReadObject {
        object_type: &'static str,
        hash: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    #[error("{object_type} not found: {hash}")]
    ObjectNotFound {
        object_type: &'static str,
        hash: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    #[error("Failed to write {object_type}: {source}")]
    WriteObject {
        object_type: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Other operation store error: {0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl From<OpStoreError> for napi::Error {
    fn from(e: OpStoreError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Root operation initialization data.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct RootOperationData {
    pub root_commit_id: String,
}

/// Prefix resolution result for an operation ID.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct OpPrefixResolution {
    pub found: bool,
    pub ambiguous: bool,
    pub id: Option<String>,
}

/// Trait defining the interface for operation store backends.
pub trait OpStoreTrait: Any + Send + Sync + Debug {
    fn name(&self) -> &str;
    fn root_operation_id(&self) -> &str;
    fn read_view(&self, id: &str) -> Result<ViewRecord, OpStoreError>;
    fn write_view(&self, view: &ViewRecord) -> Result<String, OpStoreError>;
    fn read_operation(&self, id: &str) -> Result<OperationRecord, OpStoreError>;
    fn write_operation(&self, operation: &OperationRecord) -> Result<String, OpStoreError>;
    fn resolve_operation_id_prefix(&self, prefix: &str)
        -> Result<OpPrefixResolution, OpStoreError>;
    fn gc(&self, head_ids: &[String], keep_newer_seconds: i64) -> Result<u32, OpStoreError>;
}

/// File-system based implementation of `OpStore` (mirrors `SimpleOpStore`).
#[derive(Clone, Debug)]
pub struct SimpleOpStore {
    path: PathBuf,
    root_data: RootOperationData,
    root_operation_id: String,
    root_view_id: String,
}

impl SimpleOpStore {
    pub const NAME: &'static str = "simple_op_store";

    pub fn new(store_path: &Path, root_data: RootOperationData) -> Self {
        Self {
            path: store_path.to_path_buf(),
            root_data,
            root_operation_id: "0".repeat(OPERATION_ID_LENGTH),
            root_view_id: "0".repeat(VIEW_ID_LENGTH),
        }
    }

    pub fn init(store_path: &Path, root_data: RootOperationData) -> Result<Self, OpStoreError> {
        let store = Self::new(store_path, root_data);
        store
            .init_base_dirs()
            .map_err(|e| OpStoreError::Other(Box::new(e)))?;
        Ok(store)
    }

    pub fn load(store_path: &Path, root_data: RootOperationData) -> Self {
        Self::new(store_path, root_data)
    }

    fn init_base_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.views_dir())?;
        fs::create_dir_all(self.operations_dir())?;
        fs::write(self.path.join("type"), Self::NAME)?;
        Ok(())
    }

    fn views_dir(&self) -> PathBuf {
        self.path.join("views")
    }

    fn operations_dir(&self) -> PathBuf {
        self.path.join("operations")
    }
}

impl OpStoreTrait for SimpleOpStore {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn root_operation_id(&self) -> &str {
        &self.root_operation_id
    }

    fn read_view(&self, id: &str) -> Result<ViewRecord, OpStoreError> {
        if id == self.root_view_id {
            let mut wc = BTreeMap::new();
            wc.insert("default".to_string(), self.root_data.root_commit_id.clone());
            return Ok(ViewRecord {
                id: self.root_view_id.clone(),
                head_ids: vec![self.root_data.root_commit_id.clone()],
                wc_commit_ids: wc,
                local_bookmarks: BTreeMap::new(),
                local_tags: BTreeMap::new(),
                git_refs: BTreeMap::new(),
                git_heads: BTreeMap::new(),
            });
        }

        let p = self.views_dir().join(id);
        if !p.exists() {
            return Err(OpStoreError::ObjectNotFound {
                object_type: "view",
                hash: id.to_string(),
                source: None,
            });
        }
        let data = fs::read(&p).map_err(|e| OpStoreError::ReadObject {
            object_type: "view",
            hash: id.to_string(),
            source: Some(Box::new(e)),
        })?;
        let dto: ViewRecordDto =
            serde_json::from_slice(&data).map_err(|e| OpStoreError::ReadObject {
                object_type: "view",
                hash: id.to_string(),
                source: Some(Box::new(e)),
            })?;
        Ok(dto.into_record(id.to_string()))
    }

    fn write_view(&self, view: &ViewRecord) -> Result<String, OpStoreError> {
        fs::create_dir_all(self.views_dir()).map_err(|e| OpStoreError::WriteObject {
            object_type: "view",
            source: Box::new(e),
        })?;
        let id = if view.id.is_empty() {
            compute_view_hash(view)
        } else {
            view.id.clone()
        };

        let mut to_write = view.clone();
        to_write.id = id.clone();

        let data = serde_json::to_vec_pretty(&ViewRecordDto::from(&to_write)).map_err(|e| {
            OpStoreError::WriteObject {
                object_type: "view",
                source: Box::new(e),
            }
        })?;
        let p = self.views_dir().join(&id);
        fs::write(&p, data).map_err(|e| OpStoreError::WriteObject {
            object_type: "view",
            source: Box::new(e),
        })?;
        Ok(id)
    }

    fn read_operation(&self, id: &str) -> Result<OperationRecord, OpStoreError> {
        if id == self.root_operation_id {
            return Ok(OperationRecord {
                id: self.root_operation_id.clone(),
                view_id: self.root_view_id.clone(),
                parents: vec![],
                metadata: OpMetadata {
                    description: "root operation".to_string(),
                    hostname: "localhost".to_string(),
                    username: "jj".to_string(),
                    is_snapshot: false,
                    workspace_name: Some("default".to_string()),
                    time: OpTimestampRange {
                        start_millis: BigInt::from(0i64),
                        end_millis: BigInt::from(0i64),
                        tz_offset: 0,
                    },
                    attributes: BTreeMap::new(),
                },
            });
        }

        let p = self.operations_dir().join(id);
        if !p.exists() {
            return Err(OpStoreError::ObjectNotFound {
                object_type: "operation",
                hash: id.to_string(),
                source: None,
            });
        }
        let data = fs::read(&p).map_err(|e| OpStoreError::ReadObject {
            object_type: "operation",
            hash: id.to_string(),
            source: Some(Box::new(e)),
        })?;
        let dto: OperationRecordDto =
            serde_json::from_slice(&data).map_err(|e| OpStoreError::ReadObject {
                object_type: "operation",
                hash: id.to_string(),
                source: Some(Box::new(e)),
            })?;
        Ok(dto.into_record(id.to_string()))
    }

    fn write_operation(&self, operation: &OperationRecord) -> Result<String, OpStoreError> {
        fs::create_dir_all(self.operations_dir()).map_err(|e| OpStoreError::WriteObject {
            object_type: "operation",
            source: Box::new(e),
        })?;
        let id = if operation.id.is_empty() {
            compute_op_hash(operation)
        } else {
            operation.id.clone()
        };

        let mut to_write = operation.clone();
        to_write.id = id.clone();

        let data =
            serde_json::to_vec_pretty(&OperationRecordDto::from(&to_write)).map_err(|e| {
                OpStoreError::WriteObject {
                    object_type: "operation",
                    source: Box::new(e),
                }
            })?;
        let p = self.operations_dir().join(&id);
        fs::write(&p, data).map_err(|e| OpStoreError::WriteObject {
            object_type: "operation",
            source: Box::new(e),
        })?;
        Ok(id)
    }

    fn resolve_operation_id_prefix(
        &self,
        prefix: &str,
    ) -> Result<OpPrefixResolution, OpStoreError> {
        let op_dir = self.operations_dir();
        if prefix.len() == OPERATION_ID_LENGTH {
            if prefix == self.root_operation_id || op_dir.join(prefix).exists() {
                return Ok(OpPrefixResolution {
                    found: true,
                    ambiguous: false,
                    id: Some(prefix.to_string()),
                });
            } else {
                return Ok(OpPrefixResolution {
                    found: false,
                    ambiguous: false,
                    id: None,
                });
            }
        }

        let mut matches = Vec::new();
        if self.root_operation_id.starts_with(prefix) {
            matches.push(self.root_operation_id.clone());
        }

        if op_dir.exists() {
            if let Ok(entries) = fs::read_dir(&op_dir) {
                for entry in entries.flatten() {
                    if let Ok(name) = entry.file_name().into_string() {
                        if name.starts_with(prefix) && !matches.contains(&name) {
                            matches.push(name);
                        }
                    }
                }
            }
        }

        match matches.len() {
            0 => Ok(OpPrefixResolution {
                found: false,
                ambiguous: false,
                id: None,
            }),
            1 => Ok(OpPrefixResolution {
                found: true,
                ambiguous: false,
                id: Some(matches.pop().unwrap()),
            }),
            _ => Ok(OpPrefixResolution {
                found: false,
                ambiguous: true,
                id: None,
            }),
        }
    }

    fn gc(&self, head_ids: &[String], keep_newer_seconds: i64) -> Result<u32, OpStoreError> {
        let mut reachable_ops: HashMap<String, OperationRecord> = HashMap::new();
        let mut queue: Vec<String> = head_ids.to_vec();

        while let Some(id) = queue.pop() {
            if reachable_ops.contains_key(&id) {
                continue;
            }
            if let Ok(op) = self.read_operation(&id) {
                for parent in &op.parents {
                    if !reachable_ops.contains_key(parent) {
                        queue.push(parent.clone());
                    }
                }
                reachable_ops.insert(id, op);
            }
        }

        let mut reachable_views: HashSet<String> = HashSet::new();
        for op in reachable_ops.values() {
            reachable_views.insert(op.view_id.clone());
        }

        let cutoff = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(keep_newer_seconds.max(0) as u64);
        let mut pruned = 0u32;

        // Prune unreachable operations
        if let Ok(entries) = fs::read_dir(self.operations_dir()) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if !reachable_ops.contains_key(&name) {
                        let path = entry.path();
                        if let Ok(meta) = entry.metadata() {
                            if let Ok(mtime) = meta.modified() {
                                if mtime < cutoff {
                                    if fs::remove_file(&path).is_ok() {
                                        pruned += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Prune unreachable views
        if let Ok(entries) = fs::read_dir(self.views_dir()) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if !reachable_views.contains(&name) {
                        let path = entry.path();
                        if let Ok(meta) = entry.metadata() {
                            if let Ok(mtime) = meta.modified() {
                                if mtime < cutoff {
                                    if fs::remove_file(&path).is_ok() {
                                        pruned += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(pruned)
    }
}

/// Options for configuring / opening an OpStore.
#[derive(Clone, Debug, Default)]
#[napi(object)]
pub struct OpStoreOpts {
    /// Directory or database path for the operation store.
    pub ops_dir: Option<String>,
    /// Whether to recursively create the directory if it does not already exist. Defaults to `true`.
    pub create: Option<bool>,
}

/// NAPI-exposed handle to an OpStore.
#[derive(Clone)]
#[napi]
pub struct OpStore {
    pub(crate) inner: Arc<dyn OpStoreTrait>,
    path: PathBuf,
}

impl OpStore {
    pub fn get_inner(&self) -> Arc<dyn OpStoreTrait> {
        self.inner.clone()
    }
}

/// NAPI alias for `OpStore`.
pub type OperationStore = OpStore;

#[napi]
impl OpStore {
    /// Bind an `OpStore` to a directory, file URL, or options object.
    ///
    /// Accepts a path string, `file://` URL, or `OpStoreOpts` (`{ opsDir?: string, create?: boolean }`).
    /// If `create` is `true` (the default), parent directories are created automatically.
    #[napi(constructor)]
    pub fn new(
        store_path_or_opts: napi::bindgen_prelude::Either<String, OpStoreOpts>,
        root_commit_id: Option<String>,
    ) -> napi::Result<Self> {
        let (raw_path, should_create) = match store_path_or_opts {
            napi::bindgen_prelude::Either::A(path_str) => (path_str, true),
            napi::bindgen_prelude::Either::B(opts) => {
                let p = opts
                    .ops_dir
                    .unwrap_or_else(|| ".git/ops/op_store".to_string());
                let c = opts.create.unwrap_or(true);
                (p, c)
            }
        };

        let store_path = parse_file_path_or_url(&raw_path);
        let p = PathBuf::from(&store_path);

        if should_create && !p.exists() {
            fs::create_dir_all(&p).map_err(|e| err("create_ops_dir", e))?;
        }

        let root_data = RootOperationData {
            root_commit_id: root_commit_id.unwrap_or_else(|| "0".repeat(40)),
        };
        let store = SimpleOpStore::load(&p, root_data);
        Ok(Self {
            inner: Arc::new(store),
            path: p,
        })
    }

    /// Factory constructor function to initiate an op store at a designated file or directory
    /// (argument can be a path string, `file://` URL, or `OpStoreOpts`).
    #[napi(factory)]
    pub fn open(
        store_path_or_opts: napi::bindgen_prelude::Either<String, OpStoreOpts>,
        root_commit_id: Option<String>,
    ) -> napi::Result<Self> {
        Self::new(store_path_or_opts, root_commit_id)
    }

    /// Initialise a new `SimpleOpStore` at `store_path` (file path, file URL, or `OpStoreOpts`).
    #[napi]
    pub fn init(
        store_path_or_opts: napi::bindgen_prelude::Either<String, OpStoreOpts>,
        root_commit_id: Option<String>,
    ) -> napi::Result<Self> {
        let (raw_path, should_create) = match store_path_or_opts {
            napi::bindgen_prelude::Either::A(path_str) => (path_str, true),
            napi::bindgen_prelude::Either::B(opts) => {
                let p = opts
                    .ops_dir
                    .unwrap_or_else(|| ".git/ops/op_store".to_string());
                let c = opts.create.unwrap_or(true);
                (p, c)
            }
        };

        let store_path = parse_file_path_or_url(&raw_path);
        let p = PathBuf::from(&store_path);

        if should_create && !p.exists() {
            fs::create_dir_all(&p).map_err(|e| err("create_ops_dir", e))?;
        }

        let root_data = RootOperationData {
            root_commit_id: root_commit_id.unwrap_or_else(|| "0".repeat(40)),
        };
        let store = SimpleOpStore::init(&p, root_data).map_err(|e| err("OpStore::init", e))?;
        Ok(Self {
            inner: Arc::new(store),
            path: p,
        })
    }

    /// Backend name (`"simple_op_store"`).
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name().to_string()
    }

    /// Root operation ID.
    #[napi(getter)]
    pub fn root_operation_id(&self) -> String {
        self.inner.root_operation_id().to_string()
    }

    /// Read a view by its hex ID.
    #[napi]
    pub fn read_view(&self, id: String) -> napi::Result<ViewRecord> {
        self.inner.read_view(&id).map_err(|e| err("read_view", e))
    }

    /// Write a view and return its content hash ID.
    #[napi]
    pub fn write_view(&self, view: ViewRecord) -> napi::Result<String> {
        self.inner
            .write_view(&view)
            .map_err(|e| err("write_view", e))
    }

    /// Read an operation by its hex ID.
    #[napi]
    pub fn read_operation(&self, id: String) -> napi::Result<OperationRecord> {
        self.inner
            .read_operation(&id)
            .map_err(|e| err("read_operation", e))
    }

    /// Write an operation and return its content hash ID.
    #[napi]
    pub fn write_operation(&self, operation: OperationRecord) -> napi::Result<String> {
        self.inner
            .write_operation(&operation)
            .map_err(|e| err("write_operation", e))
    }

    /// Disambiguate an operation ID prefix.
    #[napi]
    pub fn resolve_operation_id_prefix(&self, prefix: String) -> napi::Result<OpPrefixResolution> {
        self.inner
            .resolve_operation_id_prefix(&prefix)
            .map_err(|e| err("resolve_operation_id_prefix", e))
    }

    /// Garbage-collect unreachable operations and views older than `keep_newer_seconds`.
    #[napi]
    pub fn gc(&self, head_ids: Vec<String>, keep_newer_seconds: Option<f64>) -> napi::Result<u32> {
        let sec = keep_newer_seconds.unwrap_or(0.0) as i64;
        self.inner.gc(&head_ids, sec).map_err(|e| err("gc", e))
    }
}

fn compute_op_hash(op: &OperationRecord) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(op.view_id.as_bytes());
    for p in &op.parents {
        hasher.update(p.as_bytes());
    }
    hasher.update(op.metadata.description.as_bytes());
    let (start_val, _) = op.metadata.time.start_millis.get_i64();
    let (end_val, _) = op.metadata.time.end_millis.get_i64();
    hasher.update(&start_val.to_le_bytes());
    hasher.update(&end_val.to_le_bytes());
    let hash = hasher.finalize();
    hex::encode(hash.as_bytes())
}

fn parse_file_path_or_url(input: &str) -> String {
    if let Some(stripped) = input.strip_prefix("file://") {
        stripped.to_string()
    } else {
        input.to_string()
    }
}

fn compute_view_hash(view: &ViewRecord) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    for h in &view.head_ids {
        hasher.update(h.as_bytes());
    }
    for (k, v) in &view.wc_commit_ids {
        hasher.update(k.as_bytes());
        hasher.update(v.as_bytes());
    }
    for (k, v) in &view.local_bookmarks {
        hasher.update(k.as_bytes());
        for add in &v.adds {
            hasher.update(add.as_bytes());
        }
    }
    let hash = hasher.finalize();
    hex::encode(hash.as_bytes())
}

use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::sync::Mutex;

/// Commit ID byte length in SimpleBackend (64 bytes / Blake2b-512).
#[napi]
pub const COMMIT_ID_LENGTH: u32 = 64;

/// Change ID byte length in SimpleBackend (16 bytes).
#[napi]
pub const CHANGE_ID_LENGTH: u32 = 16;

/// Backend error types mirroring Jujutsu's backend error semantics.
#[derive(Debug, Error)]
pub enum BackendError {
    #[error("Object not found ({object_type}): {hash}")]
    ObjectNotFound { object_type: String, hash: String },
    #[error("Failed to read object ({object_type}) {hash}: {details}")]
    ReadObject {
        object_type: String,
        hash: String,
        details: String,
    },
    #[error("Unsupported backend operation: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

impl From<BackendError> for napi::Error {
    fn from(e: BackendError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Map an I/O error into a BackendError for missing or unreadable objects.
pub fn map_not_found_err(err: std::io::Error, object_type: &str, hash: &str) -> BackendError {
    if err.kind() == std::io::ErrorKind::NotFound {
        BackendError::ObjectNotFound {
            object_type: object_type.to_string(),
            hash: hash.to_string(),
        }
    } else {
        BackendError::ReadObject {
            object_type: object_type.to_string(),
            hash: hash.to_string(),
            details: err.to_string(),
        }
    }
}

/// Simple filesystem-based backend for commits, trees, files, symlinks, and conflicts.
#[derive(Clone)]
#[napi]
pub struct SimpleBackend {
    pub(crate) path: PathBuf,
    root_commit_id: String,
    root_change_id: String,
    empty_tree_id: String,
}

#[napi]
impl SimpleBackend {
    #[napi]
    pub fn name(&self) -> String {
        "Simple".to_string()
    }

    #[napi(factory)]
    pub fn init(store_path: String) -> napi::Result<SimpleBackend> {
        let p = PathBuf::from(&store_path);
        fs::create_dir_all(p.join("commits")).map_err(|e| err("init_simple_backend", e))?;
        fs::create_dir_all(p.join("trees")).map_err(|e| err("init_simple_backend", e))?;
        fs::create_dir_all(p.join("files")).map_err(|e| err("init_simple_backend", e))?;
        fs::create_dir_all(p.join("symlinks")).map_err(|e| err("init_simple_backend", e))?;
        fs::create_dir_all(p.join("conflicts")).map_err(|e| err("init_simple_backend", e))?;
        Ok(Self::load(store_path))
    }

    #[napi(factory)]
    pub fn load(store_path: String) -> SimpleBackend {
        let root_commit_id = "0".repeat((COMMIT_ID_LENGTH * 2) as usize);
        let root_change_id = "0".repeat((CHANGE_ID_LENGTH * 2) as usize);
        let empty_tree_id =
            "482ae5a29fbe856c7272f2071b8b0f0359ee2d89ff392b8a900643fbd0836eccd067b8bf41909e206c90d45d6e7d8b6686b93ecaee5fe1a9060d87b672101310".to_string();
        SimpleBackend {
            path: PathBuf::from(store_path),
            root_commit_id,
            root_change_id,
            empty_tree_id,
        }
    }

    #[napi]
    pub fn commit_id_length(&self) -> u32 {
        COMMIT_ID_LENGTH
    }

    #[napi]
    pub fn change_id_length(&self) -> u32 {
        CHANGE_ID_LENGTH
    }

    #[napi]
    pub fn root_commit_id(&self) -> String {
        self.root_commit_id.clone()
    }

    #[napi]
    pub fn root_change_id(&self) -> String {
        self.root_change_id.clone()
    }

    #[napi]
    pub fn empty_tree_id(&self) -> String {
        self.empty_tree_id.clone()
    }

    #[napi]
    pub fn concurrency(&self) -> u32 {
        1
    }

    #[napi]
    pub fn read_file(&self, id: String) -> napi::Result<String> {
        let file_p = self.path.join("files").join(&id);
        fs::read_to_string(&file_p).map_err(|e| map_not_found_err(e, "file", &id).into())
    }

    #[napi]
    pub fn write_file(&self, contents: String) -> napi::Result<String> {
        let bytes = contents.as_bytes();
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(bytes);
        let id = hasher.try_finalize().map_err(|e| err("write_file", e))?;
        let hash_id = id.to_hex().to_string();
        let file_p = self.path.join("files").join(&hash_id);
        fs::write(&file_p, bytes).map_err(|e| err("write_file", e))?;
        Ok(hash_id)
    }

    #[napi]
    pub fn read_symlink(&self, id: String) -> napi::Result<String> {
        let p = self.path.join("symlinks").join(&id);
        fs::read_to_string(&p).map_err(|e| map_not_found_err(e, "symlink", &id).into())
    }

    #[napi]
    pub fn write_symlink(&self, target: String) -> napi::Result<String> {
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(target.as_bytes());
        let id = hasher.try_finalize().map_err(|e| err("write_symlink", e))?;
        let hash_id = id.to_hex().to_string();
        let p = self.path.join("symlinks").join(&hash_id);
        fs::write(&p, target.as_bytes()).map_err(|e| err("write_symlink", e))?;
        Ok(hash_id)
    }

    #[napi]
    pub fn read_tree(&self, id: String) -> napi::Result<String> {
        let p = self.path.join("trees").join(&id);
        fs::read_to_string(&p).map_err(|e| map_not_found_err(e, "tree", &id).into())
    }

    #[napi]
    pub fn write_tree(&self, contents: String) -> napi::Result<String> {
        let bytes = contents.as_bytes();
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(bytes);
        let id = hasher.try_finalize().map_err(|e| err("write_tree", e))?;
        let hash_id = id.to_hex().to_string();
        let p = self.path.join("trees").join(&hash_id);
        fs::write(&p, bytes).map_err(|e| err("write_tree", e))?;
        Ok(hash_id)
    }

    #[napi]
    pub fn read_commit(&self, id: String) -> napi::Result<String> {
        if id == self.root_commit_id {
            return Ok("root_commit".to_string());
        }
        let p = self.path.join("commits").join(&id);
        fs::read_to_string(&p).map_err(|e| map_not_found_err(e, "commit", &id).into())
    }

    #[napi]
    pub fn write_commit(&self, commit_data: String) -> napi::Result<String> {
        let bytes = commit_data.as_bytes();
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(bytes);
        let id = hasher.try_finalize().map_err(|e| err("write_commit", e))?;
        let hash_id = id.to_hex().to_string();
        let p = self.path.join("commits").join(&hash_id);
        fs::write(&p, bytes).map_err(|e| err("write_commit", e))?;
        Ok(hash_id)
    }
}

/// SQLite-backed SQL storage backend configured with WAL (Write-Ahead Logging) mode.
#[napi]
pub struct SqlBackend {
    conn: Mutex<SqliteConnection>,
    database_url: String,
    root_commit_id: String,
    root_change_id: String,
    empty_tree_id: String,
}

#[napi]
impl SqlBackend {
    #[napi]
    pub fn name(&self) -> String {
        "SqlBackend".to_string()
    }

    #[napi(factory)]
    pub fn init(database_url: String) -> napi::Result<SqlBackend> {
        let clean_path = database_url.trim_start_matches("file:");
        if let Some(parent) = Path::new(clean_path).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let mut conn =
            SqliteConnection::establish(&database_url).map_err(|e| err("init_sql_backend", e))?;

        // Enable SQLite WAL mode and normal synchronous for optimal concurrent read/write
        conn.batch_execute(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS objects (
                 id TEXT PRIMARY KEY NOT NULL,
                 kind TEXT NOT NULL,
                 data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS operations (
                 id TEXT PRIMARY KEY NOT NULL,
                 view_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS views (
                 id TEXT PRIMARY KEY NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .map_err(|e| err("init_sql_backend_schema", e))?;

        Ok(Self::load(database_url)?)
    }

    #[napi(factory)]
    pub fn load(database_url: String) -> napi::Result<SqlBackend> {
        let mut conn =
            SqliteConnection::establish(&database_url).map_err(|e| err("load_sql_backend", e))?;
        conn.batch_execute("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(|e| err("load_sql_backend_wal", e))?;

        let root_commit_id = "0".repeat((COMMIT_ID_LENGTH * 2) as usize);
        let root_change_id = "0".repeat((CHANGE_ID_LENGTH * 2) as usize);
        let empty_tree_id =
            "482ae5a29fbe856c7272f2071b8b0f0359ee2d89ff392b8a900643fbd0836eccd067b8bf41909e206c90d45d6e7d8b6686b93ecaee5fe1a9060d87b672101310".to_string();

        Ok(SqlBackend {
            conn: Mutex::new(conn),
            database_url,
            root_commit_id,
            root_change_id,
            empty_tree_id,
        })
    }

    #[napi]
    pub fn is_wal_mode(&self) -> napi::Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        #[derive(QueryableByName)]
        struct JournalMode {
            #[diesel(sql_type = diesel::sql_types::Text)]
            journal_mode: String,
        }
        let mode: Vec<JournalMode> = diesel::sql_query("PRAGMA journal_mode;")
            .load(&mut *conn)
            .map_err(|e| err("is_wal_mode", e))?;
        Ok(mode
            .first()
            .map(|m| m.journal_mode.to_uppercase() == "WAL")
            .unwrap_or(false))
    }

    #[napi]
    pub fn commit_id_length(&self) -> u32 {
        COMMIT_ID_LENGTH
    }

    #[napi]
    pub fn change_id_length(&self) -> u32 {
        CHANGE_ID_LENGTH
    }

    #[napi]
    pub fn root_commit_id(&self) -> String {
        self.root_commit_id.clone()
    }

    #[napi]
    pub fn root_change_id(&self) -> String {
        self.root_change_id.clone()
    }

    #[napi]
    pub fn empty_tree_id(&self) -> String {
        self.empty_tree_id.clone()
    }

    #[napi]
    pub fn concurrency(&self) -> u32 {
        4
    }

    #[napi]
    pub fn read_file(&self, id: String) -> napi::Result<String> {
        self.read_object(&id, "file")
    }

    #[napi]
    pub fn write_file(&self, contents: String) -> napi::Result<String> {
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(contents.as_bytes());
        let id = hasher
            .try_finalize()
            .map_err(|e| err("write_file", e))?
            .to_hex()
            .to_string();
        self.write_object(&id, "file", &contents)?;
        Ok(id)
    }

    #[napi]
    pub fn read_symlink(&self, id: String) -> napi::Result<String> {
        self.read_object(&id, "symlink")
    }

    #[napi]
    pub fn write_symlink(&self, target: String) -> napi::Result<String> {
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(target.as_bytes());
        let id = hasher
            .try_finalize()
            .map_err(|e| err("write_symlink", e))?
            .to_hex()
            .to_string();
        self.write_object(&id, "symlink", &target)?;
        Ok(id)
    }

    #[napi]
    pub fn read_tree(&self, id: String) -> napi::Result<String> {
        self.read_object(&id, "tree")
    }

    #[napi]
    pub fn write_tree(&self, contents: String) -> napi::Result<String> {
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(contents.as_bytes());
        let id = hasher
            .try_finalize()
            .map_err(|e| err("write_tree", e))?
            .to_hex()
            .to_string();
        self.write_object(&id, "tree", &contents)?;
        Ok(id)
    }

    #[napi]
    pub fn read_commit(&self, id: String) -> napi::Result<String> {
        if id == self.root_commit_id {
            return Ok("root_commit".to_string());
        }
        self.read_object(&id, "commit")
    }

    #[napi]
    pub fn write_commit(&self, commit_data: String) -> napi::Result<String> {
        let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
        hasher.update(commit_data.as_bytes());
        let id = hasher
            .try_finalize()
            .map_err(|e| err("write_commit", e))?
            .to_hex()
            .to_string();
        self.write_object(&id, "commit", &commit_data)?;
        Ok(id)
    }

    fn read_object(&self, id: &str, kind: &str) -> napi::Result<String> {
        let mut conn = self.conn.lock().unwrap();
        #[derive(QueryableByName)]
        struct ObjectRow {
            #[diesel(sql_type = diesel::sql_types::Text)]
            data: String,
        }

        let rows: Vec<ObjectRow> =
            diesel::sql_query("SELECT data FROM objects WHERE id = ? AND kind = ? LIMIT 1")
                .bind::<diesel::sql_types::Text, _>(id)
                .bind::<diesel::sql_types::Text, _>(kind)
                .load(&mut *conn)
                .map_err(|e| err("read_object", e))?;

        if let Some(row) = rows.first() {
            Ok(row.data.clone())
        } else {
            Err(BackendError::ObjectNotFound {
                object_type: kind.to_string(),
                hash: id.to_string(),
            }
            .into())
        }
    }

    fn write_object(&self, id: &str, kind: &str, data: &str) -> napi::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        diesel::sql_query("INSERT OR REPLACE INTO objects (id, kind, data) VALUES (?, ?, ?)")
            .bind::<diesel::sql_types::Text, _>(id)
            .bind::<diesel::sql_types::Text, _>(kind)
            .bind::<diesel::sql_types::Text, _>(data)
            .execute(&mut *conn)
            .map_err(|e| err("write_object", e))?;
        Ok(())
    }
}
