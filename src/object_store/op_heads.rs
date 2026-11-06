//! Operation Heads Store (`op-heads-store`) implementation for Jujutsu (jj) repositories.
//!
//! Mirrors `jj_lib::op_heads_store` and `jj_lib::simple_op_heads_store`, managing the current
//! heads of the operation log (DAG of transactions) in `.jj/repo/op_heads/heads/` and the
//! operations/views under `.jj/repo/op_store/`.

use std::any::Any;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::bindgen_prelude::BigInt;
use napi_derive::napi;
use thiserror::Error;

use crate::error::{err, err_str};

/// Represents an operation ID (BLAKE2b or hex string).
pub type OperationId = String;

/// Timestamp range for an operation's execution duration.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct OpTimestampRange {
    pub start_millis: BigInt,
    pub end_millis: BigInt,
    pub tz_offset: i32,
}

/// Metadata recorded for an operation.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct OpMetadata {
    pub description: String,
    pub hostname: String,
    pub username: String,
    pub is_snapshot: bool,
    pub workspace_name: Option<String>,
    pub time: OpTimestampRange,
    pub attributes: BTreeMap<String, String>,
}

/// An Operation representing a transaction/state change in the repository history.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct OperationRecord {
    pub id: String,
    pub view_id: String,
    pub parents: Vec<String>,
    pub metadata: OpMetadata,
}

/// Target of a reference or bookmark in an operation view.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct OpRefTarget {
    pub adds: Vec<String>,
    pub removes: Vec<String>,
    pub is_conflict: bool,
}

/// Represents the repository view at an operation: visible heads, bookmarks, tags, and working copy commits.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ViewRecord {
    pub id: String,
    pub head_ids: Vec<String>,
    pub wc_commit_ids: BTreeMap<String, String>,
    pub local_bookmarks: BTreeMap<String, OpRefTarget>,
    pub local_tags: BTreeMap<String, OpRefTarget>,
    pub git_refs: BTreeMap<String, OpRefTarget>,
    pub git_heads: BTreeMap<String, OpRefTarget>,
}

/// Result of resolving operation heads.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ResolvedOpHeads {
    /// Resolved operation head ID.
    pub op_id: String,
    /// All operation heads before resolution.
    pub heads: Vec<String>,
    /// Whether divergent heads were reconciled into a merge operation.
    pub was_divergent: bool,
}

/// Error type for operation heads store operations.
#[derive(Debug, Error)]
pub enum OpHeadsStoreError {
    #[error("Failed to read operation heads: {0}")]
    Read(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Failed to record operation head {new_op_id}: {source}")]
    Write {
        new_op_id: OperationId,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Failed to lock operation heads store: {0}")]
    Lock(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<OpHeadsStoreError> for napi::Error {
    fn from(e: OpHeadsStoreError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Lock trait for OpHeadsStore.
pub trait OpHeadsStoreLock: Send + Sync {}

/// Manages the set of current heads of the operation log.
pub trait OpHeadsStoreTrait: Any + Send + Sync + Debug {
    fn name(&self) -> &str;

    /// Remove the old op heads and add the new one.
    fn update_op_heads(
        &self,
        old_ids: &[OperationId],
        new_id: &OperationId,
    ) -> Result<(), OpHeadsStoreError>;

    /// Return the current op heads.
    fn get_op_heads(&self) -> Result<Vec<OperationId>, OpHeadsStoreError>;

    /// Optionally takes a lock on the op heads store.
    fn lock(&self) -> Result<Box<dyn OpHeadsStoreLock>, OpHeadsStoreError>;
}

/// File-based lock implementing `OpHeadsStoreLock`.
pub struct SimpleOpHeadsStoreLock {
    lock_file: PathBuf,
    pub file: Option<fs::File>,
    active: bool,
}

impl OpHeadsStoreLock for SimpleOpHeadsStoreLock {}

impl Drop for SimpleOpHeadsStoreLock {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.lock_file);
            self.active = false;
        }
    }
}

/// File-system based operation heads store (mirrors `SimpleOpHeadsStore`).
#[derive(Clone, Debug)]
pub struct SimpleOpHeadsStore {
    dir: PathBuf,
}

impl SimpleOpHeadsStore {
    pub const NAME: &'static str = "simple_op_heads_store";

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn init(dir: &Path, root_op_id: &OperationId) -> Result<Self, OpHeadsStoreError> {
        let op_heads_dir = dir.join("heads");
        fs::create_dir_all(&op_heads_dir)
            .map_err(|e| OpHeadsStoreError::Write {
                new_op_id: root_op_id.clone(),
                source: Box::new(e),
            })?;
        let store = Self { dir: op_heads_dir };
        store.add_op_head(root_op_id)?;
        Ok(store)
    }

    pub fn load(dir: &Path) -> Self {
        let op_heads_dir = dir.join("heads");
        Self { dir: op_heads_dir }
    }

    pub fn add_op_head(&self, id: &OperationId) -> Result<(), OpHeadsStoreError> {
        let path = self.dir.join(id);
        fs::write(&path, "")
            .map_err(|e| OpHeadsStoreError::Write {
                new_op_id: id.clone(),
                source: Box::new(e),
            })
    }

    pub fn remove_op_head(&self, id: &OperationId) -> Result<(), OpHeadsStoreError> {
        let path = self.dir.join(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(OpHeadsStoreError::Write {
                new_op_id: id.clone(),
                source: Box::new(e),
            }),
        }
    }
}

impl OpHeadsStoreTrait for SimpleOpHeadsStore {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn update_op_heads(
        &self,
        old_ids: &[OperationId],
        new_id: &OperationId,
    ) -> Result<(), OpHeadsStoreError> {
        self.add_op_head(new_id)?;
        for old in old_ids {
            if old != new_id {
                self.remove_op_head(old)?;
            }
        }
        Ok(())
    }

    fn get_op_heads(&self) -> Result<Vec<OperationId>, OpHeadsStoreError> {
        if !self.dir.exists() {
            return Err(OpHeadsStoreError::Read(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Op heads directory does not exist",
            ))));
        }
        let mut op_heads = Vec::new();
        let entries = fs::read_dir(&self.dir).map_err(|e| OpHeadsStoreError::Read(Box::new(e)))?;
        for entry in entries {
            let entry = entry.map_err(|e| OpHeadsStoreError::Read(Box::new(e)))?;
            if let Ok(file_name) = entry.file_name().into_string() {
                if file_name != "lock" && !file_name.starts_with('.') {
                    op_heads.push(file_name);
                }
            }
        }
        op_heads.sort();
        if op_heads.is_empty() {
            Err(OpHeadsStoreError::Read("Corrupt repository: no head operation".into()))
        } else {
            Ok(op_heads)
        }
    }

    fn lock(&self) -> Result<Box<dyn OpHeadsStoreLock>, OpHeadsStoreError> {
        let lock_file = self.dir.join("lock");
        fs::create_dir_all(&self.dir).map_err(|e| OpHeadsStoreError::Lock(Box::new(e)))?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_file)
            .map_err(|e| OpHeadsStoreError::Lock(Box::new(e)))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                if libc::flock(file.as_raw_fd(), libc::LOCK_EX) != 0 {
                    return Err(OpHeadsStoreError::Lock(Box::new(std::io::Error::last_os_error())));
                }
            }
        }

        Ok(Box::new(SimpleOpHeadsStoreLock {
            lock_file,
            file: Some(file),
            active: true,
        }))
    }
}

/// NAPI-exposed handle to an OpHeadsStore and associated operation/view storage.
#[napi]
pub struct OpHeadsStore {
    inner: Arc<dyn OpHeadsStoreTrait>,
    root_dir: PathBuf,
    operations_dir: PathBuf,
    views_dir: PathBuf,
}

#[napi]
impl OpHeadsStore {
    /// Open the OpHeadsStore rooted at `repo_dir` (e.g. `.jj/repo`).
    #[napi(constructor)]
    pub fn new(repo_dir: String) -> napi::Result<Self> {
        let root = PathBuf::from(repo_dir);
        let heads_dir = root.join("op_heads").join("heads");
        let operations_dir = root.join("op_store").join("operations");
        let views_dir = root.join("op_store").join("views");

        let store = SimpleOpHeadsStore::new(heads_dir);
        Ok(Self {
            inner: Arc::new(store),
            root_dir: root,
            operations_dir,
            views_dir,
        })
    }

    /// Initialize a new Jujutsu op-heads-store directory structure with an optional root commit ID.
    #[napi]
    pub fn init(repo_dir: String, root_commit_id: Option<String>) -> napi::Result<Self> {
        let root = PathBuf::from(&repo_dir);
        let op_heads_dir = root.join("op_heads");
        let heads_dir = op_heads_dir.join("heads");
        let op_store_dir = root.join("op_store");
        let operations_dir = op_store_dir.join("operations");
        let views_dir = op_store_dir.join("views");

        fs::create_dir_all(&heads_dir).map_err(|e| err("init op_heads", e))?;
        fs::create_dir_all(&operations_dir).map_err(|e| err("init operations", e))?;
        fs::create_dir_all(&views_dir).map_err(|e| err("init views", e))?;

        fs::write(op_heads_dir.join("type"), "simple_op_heads_store")
            .map_err(|e| err("init op_heads type", e))?;
        fs::write(op_store_dir.join("type"), "simple_op_store")
            .map_err(|e| err("init op_store type", e))?;

        let root_commit = root_commit_id.unwrap_or_else(|| "0".repeat(40));
        let root_view_id = "0".repeat(64);
        let root_op_id = "0".repeat(64);

        let mut wc_commit_ids = BTreeMap::new();
        wc_commit_ids.insert("default".to_string(), root_commit.clone());

        let root_view = ViewRecord {
            id: root_view_id.clone(),
            head_ids: vec![root_commit.clone()],
            wc_commit_ids,
            local_bookmarks: BTreeMap::new(),
            local_tags: BTreeMap::new(),
            git_refs: BTreeMap::new(),
            git_heads: BTreeMap::new(),
        };

        let store = SimpleOpHeadsStore::init(&op_heads_dir, &root_op_id)
            .map_err(|e| err("init SimpleOpHeadsStore", e))?;

        let handle = Self {
            inner: Arc::new(store),
            root_dir: root,
            operations_dir,
            views_dir,
        };

        handle.write_view(root_view)?;

        let root_op = OperationRecord {
            id: root_op_id.clone(),
            view_id: root_view_id,
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
        };

        handle.write_operation(root_op)?;
        Ok(handle)
    }

    /// The name of this store backend.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name().to_string()
    }

    /// Path to the root `.jj/repo` directory.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.root_dir.to_string_lossy().into_owned()
    }

    /// Return the list of current operation heads.
    #[napi]
    pub fn get_op_heads(&self) -> napi::Result<Vec<String>> {
        self.inner.get_op_heads().map_err(|e| err("get_op_heads", e))
    }

    /// Add an operation head ID.
    #[napi]
    pub fn add_op_head(&self, op_id: String) -> napi::Result<()> {
        self.inner.update_op_heads(&[], &op_id).map_err(|e| err("add_op_head", e))
    }

    /// Remove an operation head ID.
    #[napi]
    pub fn remove_op_head(&self, op_id: String) -> napi::Result<()> {
        let heads = self.get_op_heads()?;
        let remaining: Vec<String> = heads.into_iter().filter(|h| h != &op_id).collect();
        if let Some(first) = remaining.first() {
            self.inner.update_op_heads(&[op_id], first).map_err(|e| err("remove_op_head", e))
        } else {
            Err(err_str("remove_op_head", "cannot remove the only op head"))
        }
    }

    /// Update operation heads: remove `old_ids` and add `new_id`.
    #[napi]
    pub fn update_op_heads(&self, old_ids: Vec<String>, new_id: String) -> napi::Result<()> {
        self.inner.update_op_heads(&old_ids, &new_id).map_err(|e| err("update_op_heads", e))
    }

    /// Acquire an exclusive lock on the op heads store.
    #[napi]
    pub fn lock(&self) -> napi::Result<OpHeadsLock> {
        let lock_path = self.root_dir.join("op_heads").join("heads").join("lock");
        let inner_lock = self.inner.lock().map_err(|e| err("lock", e))?;
        Ok(OpHeadsLock {
            lock_path,
            _inner: Some(inner_lock),
            active: true,
        })
    }

    /// Write an operation into `.jj/repo/op_store/operations/<id>`.
    #[napi]
    pub fn write_operation(&self, operation: OperationRecord) -> napi::Result<String> {
        fs::create_dir_all(&self.operations_dir).map_err(|e| err("write_operation", e))?;
        let op_id = if operation.id.is_empty() {
            compute_op_hash(&operation)
        } else {
            operation.id.clone()
        };

        let mut op_to_write = operation;
        op_to_write.id = op_id.clone();

        let json = serde_json::to_vec_pretty(&OperationRecordDto::from(&op_to_write))
            .map_err(|e| err("serialize operation", e))?;
        let p = self.operations_dir.join(&op_id);
        fs::write(&p, json).map_err(|e| err("write operation", e))?;
        Ok(op_id)
    }

    /// Read an operation from `.jj/repo/op_store/operations/<id>`.
    #[napi]
    pub fn read_operation(&self, op_id: String) -> napi::Result<OperationRecord> {
        let p = self.operations_dir.join(&op_id);
        if !p.exists() {
            return Err(err_str("read_operation", format!("operation not found: {op_id}")));
        }
        let data = fs::read(&p).map_err(|e| err("read operation file", e))?;
        let dto: OperationRecordDto = serde_json::from_slice(&data)
            .map_err(|e| err("parse operation JSON", e))?;
        Ok(dto.into_record(op_id))
    }

    /// Write a repository view into `.jj/repo/op_store/views/<id>`.
    #[napi]
    pub fn write_view(&self, view: ViewRecord) -> napi::Result<String> {
        fs::create_dir_all(&self.views_dir).map_err(|e| err("write_view", e))?;
        let view_id = if view.id.is_empty() {
            compute_view_hash(&view)
        } else {
            view.id.clone()
        };

        let mut view_to_write = view;
        view_to_write.id = view_id.clone();

        let json = serde_json::to_vec_pretty(&ViewRecordDto::from(&view_to_write))
            .map_err(|e| err("serialize view", e))?;
        let p = self.views_dir.join(&view_id);
        fs::write(&p, json).map_err(|e| err("write view", e))?;
        Ok(view_id)
    }

    /// Read a repository view from `.jj/repo/op_store/views/<id>`.
    #[napi]
    pub fn read_view(&self, view_id: String) -> napi::Result<ViewRecord> {
        let p = self.views_dir.join(&view_id);
        if !p.exists() {
            return Err(err_str("read_view", format!("view not found: {view_id}")));
        }
        let data = fs::read(&p).map_err(|e| err("read view file", e))?;
        let dto: ViewRecordDto = serde_json::from_slice(&data)
            .map_err(|e| err("parse view JSON", e))?;
        Ok(dto.into_record(view_id))
    }

    /// Resolve op heads to a single operation head, reconciling divergent heads if needed.
    #[napi]
    pub fn resolve_op_heads(&self) -> napi::Result<ResolvedOpHeads> {
        let heads = self.get_op_heads()?;
        if heads.is_empty() {
            return Err(err_str("resolve_op_heads", "corrupt repository: no operation heads"));
        }

        if heads.len() == 1 {
            return Ok(ResolvedOpHeads {
                op_id: heads[0].clone(),
                heads: heads.clone(),
                was_divergent: false,
            });
        }

        // Multiple divergent heads: take lock and resolve
        let _lock = self.lock()?;
        let current_heads = self.get_op_heads()?;
        if current_heads.len() == 1 {
            return Ok(ResolvedOpHeads {
                op_id: current_heads[0].clone(),
                heads: current_heads,
                was_divergent: false,
            });
        }

        // Merge views into a unified view
        let mut merged_view_heads = HashSet::new();
        let mut merged_wc_commits = BTreeMap::new();
        let mut merged_local_bookmarks = BTreeMap::new();
        let mut merged_local_tags = BTreeMap::new();
        let mut merged_git_refs = BTreeMap::new();
        let mut merged_git_heads = BTreeMap::new();

        for head in &current_heads {
            let op = self.read_operation(head.clone())?;
            let view = self.read_view(op.view_id)?;
            for h in view.head_ids {
                merged_view_heads.insert(h);
            }
            for (k, v) in view.wc_commit_ids {
                merged_wc_commits.insert(k, v);
            }
            for (k, v) in view.local_bookmarks {
                merged_local_bookmarks.insert(k, v);
            }
            for (k, v) in view.local_tags {
                merged_local_tags.insert(k, v);
            }
            for (k, v) in view.git_refs {
                merged_git_refs.insert(k, v);
            }
            for (k, v) in view.git_heads {
                merged_git_heads.insert(k, v);
            }
        }

        let mut head_ids: Vec<String> = merged_view_heads.into_iter().collect();
        head_ids.sort();

        let merged_view = ViewRecord {
            id: String::new(),
            head_ids,
            wc_commit_ids: merged_wc_commits,
            local_bookmarks: merged_local_bookmarks,
            local_tags: merged_local_tags,
            git_refs: merged_git_refs,
            git_heads: merged_git_heads,
        };

        let new_view_id = self.write_view(merged_view)?;

        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let reconcile_op = OperationRecord {
            id: String::new(),
            view_id: new_view_id,
            parents: current_heads.clone(),
            metadata: OpMetadata {
                description: format!("reconcile divergent operations ({})", current_heads.join(", ")),
                hostname: "localhost".to_string(),
                username: "jj".to_string(),
                is_snapshot: false,
                workspace_name: Some("default".to_string()),
                time: OpTimestampRange {
                    start_millis: BigInt::from(now_millis),
                    end_millis: BigInt::from(now_millis),
                    tz_offset: 0,
                },
                attributes: BTreeMap::new(),
            },
        };

        let new_op_id = self.write_operation(reconcile_op)?;
        self.update_op_heads(current_heads.clone(), new_op_id.clone())?;

        Ok(ResolvedOpHeads {
            op_id: new_op_id,
            heads: current_heads,
            was_divergent: true,
        })
    }
}

/// A held lock on the OpHeadsStore.
#[napi]
pub struct OpHeadsLock {
    lock_path: PathBuf,
    #[napi(skip)]
    pub _inner: Option<Box<dyn OpHeadsStoreLock>>,
    active: bool,
}

#[napi]
impl OpHeadsLock {
    /// Explicitly release the lock.
    #[napi]
    pub fn release(&mut self) -> napi::Result<()> {
        if self.active {
            self.active = false;
            self._inner = None;
            let _ = fs::remove_file(&self.lock_path);
        }
        Ok(())
    }
}

impl Drop for OpHeadsLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

// Serialization DTO helpers
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct OpMetadataDto {
    pub description: String,
    pub hostname: String,
    pub username: String,
    pub is_snapshot: bool,
    pub workspace_name: Option<String>,
    pub start_millis: i64,
    pub end_millis: i64,
    pub tz_offset: i32,
    pub attributes: BTreeMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct OperationRecordDto {
    pub id: String,
    pub view_id: String,
    pub parents: Vec<String>,
    pub metadata: OpMetadataDto,
}

impl From<&OperationRecord> for OperationRecordDto {
    fn from(op: &OperationRecord) -> Self {
        let (start_val, _) = op.metadata.time.start_millis.get_i64();
        let (end_val, _) = op.metadata.time.end_millis.get_i64();
        Self {
            id: op.id.clone(),
            view_id: op.view_id.clone(),
            parents: op.parents.clone(),
            metadata: OpMetadataDto {
                description: op.metadata.description.clone(),
                hostname: op.metadata.hostname.clone(),
                username: op.metadata.username.clone(),
                is_snapshot: op.metadata.is_snapshot,
                workspace_name: op.metadata.workspace_name.clone(),
                start_millis: start_val,
                end_millis: end_val,
                tz_offset: op.metadata.time.tz_offset,
                attributes: op.metadata.attributes.clone(),
            },
        }
    }
}

impl OperationRecordDto {
    pub fn into_record(self, id: String) -> OperationRecord {
        OperationRecord {
            id,
            view_id: self.view_id,
            parents: self.parents,
            metadata: OpMetadata {
                description: self.metadata.description,
                hostname: self.metadata.hostname,
                username: self.metadata.username,
                is_snapshot: self.metadata.is_snapshot,
                workspace_name: self.metadata.workspace_name,
                time: OpTimestampRange {
                    start_millis: BigInt::from(self.metadata.start_millis),
                    end_millis: BigInt::from(self.metadata.end_millis),
                    tz_offset: self.metadata.tz_offset,
                },
                attributes: self.metadata.attributes,
            },
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct OpRefTargetDto {
    pub adds: Vec<String>,
    pub removes: Vec<String>,
    pub is_conflict: bool,
}

impl From<&OpRefTarget> for OpRefTargetDto {
    fn from(t: &OpRefTarget) -> Self {
        Self {
            adds: t.adds.clone(),
            removes: t.removes.clone(),
            is_conflict: t.is_conflict,
        }
    }
}

impl From<OpRefTargetDto> for OpRefTarget {
    fn from(d: OpRefTargetDto) -> Self {
        Self {
            adds: d.adds,
            removes: d.removes,
            is_conflict: d.is_conflict,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ViewRecordDto {
    pub id: String,
    pub head_ids: Vec<String>,
    pub wc_commit_ids: BTreeMap<String, String>,
    pub local_bookmarks: BTreeMap<String, OpRefTargetDto>,
    pub local_tags: BTreeMap<String, OpRefTargetDto>,
    pub git_refs: BTreeMap<String, OpRefTargetDto>,
    pub git_heads: BTreeMap<String, OpRefTargetDto>,
}

impl From<&ViewRecord> for ViewRecordDto {
    fn from(v: &ViewRecord) -> Self {
        Self {
            id: v.id.clone(),
            head_ids: v.head_ids.clone(),
            wc_commit_ids: v.wc_commit_ids.clone(),
            local_bookmarks: v.local_bookmarks.iter().map(|(k, val)| (k.clone(), OpRefTargetDto::from(val))).collect(),
            local_tags: v.local_tags.iter().map(|(k, val)| (k.clone(), OpRefTargetDto::from(val))).collect(),
            git_refs: v.git_refs.iter().map(|(k, val)| (k.clone(), OpRefTargetDto::from(val))).collect(),
            git_heads: v.git_heads.iter().map(|(k, val)| (k.clone(), OpRefTargetDto::from(val))).collect(),
        }
    }
}

impl ViewRecordDto {
    pub fn into_record(self, id: String) -> ViewRecord {
        ViewRecord {
            id,
            head_ids: self.head_ids,
            wc_commit_ids: self.wc_commit_ids,
            local_bookmarks: self.local_bookmarks.into_iter().map(|(k, val)| (k, OpRefTarget::from(val))).collect(),
            local_tags: self.local_tags.into_iter().map(|(k, val)| (k, OpRefTarget::from(val))).collect(),
            git_refs: self.git_refs.into_iter().map(|(k, val)| (k, OpRefTarget::from(val))).collect(),
            git_heads: self.git_heads.into_iter().map(|(k, val)| (k, OpRefTarget::from(val))).collect(),
        }
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
