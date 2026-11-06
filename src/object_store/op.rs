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
    /// Bind an `OpStore` to a directory or file URL (e.g. `.git/repo/op_store`).
    #[napi(constructor)]
    pub fn new(store_path: String, root_commit_id: Option<String>) -> napi::Result<Self> {
        let store_path = parse_file_path_or_url(&store_path);
        let p = PathBuf::from(store_path);
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
    /// (argument can be a path string or `file://` URL).
    #[napi(factory)]
    pub fn open(store_path: String, root_commit_id: Option<String>) -> napi::Result<Self> {
        Self::new(store_path, root_commit_id)
    }

    /// Initialise a new `SimpleOpStore` at `store_path` (file path or file URL).
    #[napi]
    pub fn init(store_path: String, root_commit_id: Option<String>) -> napi::Result<Self> {
        let store_path = parse_file_path_or_url(&store_path);
        let p = PathBuf::from(&store_path);
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
