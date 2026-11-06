//! Operation resolution, traversal, reparenting, and op-heads management for repositories.
//!
//! Mirrors `jj_lib::op_walk` and operation set evaluation semantics.

use std::collections::{BTreeMap, HashMap, HashSet};

use napi_derive::napi;
use thiserror::Error;

pub use crate::backend::*;
use crate::error::{err, thread_local};
use crate::object_store::op::OpStore;
use crate::object_store::op_heads::{OpHeadsStore, OperationRecord, ViewRecord};
use crate::repository::Repository;

/// Identifier for a [`View`] object.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[napi]
pub struct ViewId {
    hex: String,
}

#[napi]
impl ViewId {
    #[napi(constructor)]
    pub fn new(hex: String) -> Self {
        Self { hex }
    }

    #[napi]
    pub fn hex(&self) -> String {
        self.hex.clone()
    }
}

/// Identifier for an [`Operation`] object.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[napi]
pub struct OperationId {
    hex: String,
}

#[napi]
impl OperationId {
    #[napi(constructor)]
    pub fn new(hex: String) -> Self {
        Self { hex }
    }

    #[napi]
    pub fn hex(&self) -> String {
        self.hex.clone()
    }
}

/// Whether a remote reference is tracked or not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[napi]
pub enum RemoteRefState {
    New,
    Tracked,
}

/// Bookmark or tag target.
#[derive(Clone, Debug, Eq, PartialEq)]
#[napi(object)]
pub struct RefTarget {
    /// Resolved target commit ID, or None if absent.
    pub commit_id: Option<String>,
    /// Conflict commits if any.
    pub conflict_commits: Option<Vec<String>>,
}

impl RefTarget {
    pub fn absent() -> Self {
        Self {
            commit_id: None,
            conflict_commits: None,
        }
    }

    pub fn normal(commit_id: String) -> Self {
        Self {
            commit_id: Some(commit_id),
            conflict_commits: None,
        }
    }
}

/// Remote bookmark or tag.
#[derive(Clone, Debug, Eq, PartialEq)]
#[napi]
pub struct RemoteRef {
    target: RefTarget,
    state: RemoteRefState,
}

#[napi]
impl RemoteRef {
    #[napi(constructor)]
    pub fn new(target: RefTarget, state: RemoteRefState) -> Self {
        Self { target, state }
    }

    #[napi(factory)]
    pub fn absent() -> Self {
        Self {
            target: RefTarget::absent(),
            state: RemoteRefState::New,
        }
    }

    #[napi]
    pub fn is_absent(&self) -> bool {
        self.target.commit_id.is_none()
    }

    #[napi]
    pub fn is_present(&self) -> bool {
        self.target.commit_id.is_some()
    }

    #[napi]
    pub fn is_tracked(&self) -> bool {
        self.state == RemoteRefState::Tracked
    }

    #[napi]
    pub fn target(&self) -> RefTarget {
        self.target.clone()
    }

    #[napi]
    pub fn state(&self) -> RemoteRefState {
        self.state
    }
}

/// Represents the state of the remote repository.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[napi]
pub struct RemoteView {
    pub(crate) bookmarks: HashMap<String, RemoteRef>,
    pub(crate) tags: HashMap<String, RemoteRef>,
}

#[napi]
impl RemoteView {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    #[napi]
    pub fn get_bookmark(&self, name: String) -> Option<RemoteRef> {
        self.bookmarks.get(&name).cloned()
    }

    #[napi]
    pub fn set_bookmark(&mut self, name: String, remote_ref: &RemoteRef) {
        self.bookmarks.insert(name, remote_ref.clone());
    }

    #[napi]
    pub fn get_tag(&self, name: String) -> Option<RemoteRef> {
        self.tags.get(&name).cloned()
    }

    #[napi]
    pub fn set_tag(&mut self, name: String, remote_ref: &RemoteRef) {
        self.tags.insert(name, remote_ref.clone());
    }
}

/// Represents the way the repository looks at a given time.
#[derive(Clone, Debug, Eq, PartialEq)]
#[napi]
pub struct View {
    head_ids: Vec<String>,
    local_bookmarks: HashMap<String, RefTarget>,
    local_tags: HashMap<String, RefTarget>,
    git_refs: HashMap<String, RefTarget>,
    wc_commit_ids: HashMap<String, String>,
}

#[napi]
impl View {
    #[napi(constructor)]
    pub fn new(head_ids: Vec<String>) -> Self {
        Self {
            head_ids,
            local_bookmarks: HashMap::new(),
            local_tags: HashMap::new(),
            git_refs: HashMap::new(),
            wc_commit_ids: HashMap::new(),
        }
    }

    #[napi(factory)]
    pub fn make_root(root_commit_id: String) -> Self {
        Self {
            head_ids: vec![root_commit_id],
            local_bookmarks: HashMap::new(),
            local_tags: HashMap::new(),
            git_refs: HashMap::new(),
            wc_commit_ids: HashMap::new(),
        }
    }

    #[napi]
    pub fn head_ids(&self) -> Vec<String> {
        self.head_ids.clone()
    }

    #[napi]
    pub fn get_local_bookmark(&self, name: String) -> Option<RefTarget> {
        self.local_bookmarks.get(&name).cloned()
    }

    #[napi]
    pub fn set_local_bookmark(&mut self, name: String, target: RefTarget) {
        self.local_bookmarks.insert(name, target);
    }

    #[napi]
    pub fn get_wc_commit_id(&self, workspace_name: String) -> Option<String> {
        self.wc_commit_ids.get(&workspace_name).cloned()
    }

    #[napi]
    pub fn set_wc_commit_id(&mut self, workspace_name: String, commit_id: String) {
        self.wc_commit_ids.insert(workspace_name, commit_id);
    }
}

/// Represents an operation (transaction) on the repository view,
/// storing a pointer to the `OpStore` the operation belongs to.
#[derive(Clone)]
#[napi(js_name = "Operation")]
pub struct Operation {
    op_store: Option<OpStore>,
    id: OperationId,
    data: OperationRecord,
    predecessors: Option<HashMap<String, Vec<String>>>,
}

pub type Op = Operation;

#[napi]
impl Operation {
    /// Construct a new Operation object.
    #[napi(constructor)]
    pub fn new(
        id: String,
        view_id: String,
        parents: Vec<String>,
        description: String,
        metadata: Option<HashMap<String, String>>,
        op_store: Option<&OpStore>,
    ) -> Self {
        let mut attributes = BTreeMap::new();
        if let Some(ref m) = metadata {
            for (k, v) in m {
                attributes.insert(k.clone(), v.clone());
            }
        }

        let data = OperationRecord {
            id: id.clone(),
            view_id: view_id.clone(),
            parents: parents.clone(),
            metadata: crate::object_store::op_heads::OpMetadata {
                description: description.clone(),
                hostname: "localhost".to_string(),
                username: "jules".to_string(),
                is_snapshot: false,
                workspace_name: None,
                time: crate::object_store::op_heads::OpTimestampRange {
                    start_millis: napi::bindgen_prelude::BigInt::from(0i64),
                    end_millis: napi::bindgen_prelude::BigInt::from(1i64),
                    tz_offset: 0,
                },
                attributes,
            },
        };

        Self {
            op_store: op_store.cloned(),
            id: OperationId::new(id),
            data,
            predecessors: Some(HashMap::new()),
        }
    }

    /// Returns the OpStore this operation belongs to, if available.
    #[napi]
    pub fn op_store(&self) -> Option<OpStore> {
        self.op_store.clone()
    }

    /// The OperationId identifier.
    #[napi]
    pub fn id(&self) -> OperationId {
        self.id.clone()
    }

    /// The ViewId identifier.
    #[napi]
    pub fn view_id(&self) -> ViewId {
        ViewId::new(self.data.view_id.clone())
    }

    /// The parent operation IDs as OperationId objects.
    #[napi]
    pub fn parent_ids(&self) -> Vec<OperationId> {
        self.data
            .parents
            .iter()
            .map(|p| OperationId::new(p.clone()))
            .collect()
    }

    /// Read parent operations from the op store.
    #[napi]
    pub fn parents(&self) -> napi::Result<Vec<Operation>> {
        let store = match self.op_store.as_ref() {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        let mut parents = Vec::new();
        for pid in &self.data.parents {
            if let Ok(rec) = store.read_operation(pid.clone()) {
                parents.push(Operation {
                    op_store: Some(store.clone()),
                    id: OperationId::new(rec.id.clone()),
                    data: rec,
                    predecessors: Some(HashMap::new()),
                });
            }
        }
        Ok(parents)
    }

    /// Read the associated View from the op store.
    #[napi]
    pub fn view(&self) -> napi::Result<Option<View>> {
        let store = match self.op_store.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };

        if let Ok(rec) = store.read_view(self.data.view_id.clone()) {
            let mut v = View::new(rec.head_ids);
            for (name, target) in rec.local_bookmarks {
                let first_add = target.adds.first().cloned();
                let conflicts = if target.is_conflict {
                    Some(target.adds)
                } else {
                    None
                };
                v.set_local_bookmark(
                    name,
                    RefTarget {
                        commit_id: first_add,
                        conflict_commits: conflicts,
                    },
                );
            }
            for (name, cid) in rec.wc_commit_ids {
                v.set_wc_commit_id(name, cid);
            }
            Ok(Some(v))
        } else {
            Ok(None)
        }
    }

    /// Operation metadata details.
    #[napi]
    pub fn metadata(&self) -> crate::object_store::op_heads::OpMetadata {
        self.data.metadata.clone()
    }

    /// Operation description.
    #[napi]
    pub fn description(&self) -> String {
        self.data.metadata.description.clone()
    }

    /// Returns true if predecessors are recorded in this operation.
    #[napi]
    pub fn stores_commit_predecessors(&self) -> bool {
        self.predecessors.is_some()
    }

    /// Returns predecessors of the specified commit if recorded.
    #[napi]
    pub fn predecessors_for_commit(&self, commit_id: String) -> Option<Vec<String>> {
        self.predecessors.as_ref()?.get(&commit_id).cloned()
    }

    /// Return all commit IDs referenced by this operation.
    #[napi]
    pub fn all_referenced_commit_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(ref map) = self.predecessors {
            for (new_id, old_ids) in map {
                ids.push(new_id.clone());
                ids.extend(old_ids.clone());
            }
        }
        if let Some(cid) = self.data.metadata.attributes.get("commit_id") {
            ids.push(cid.clone());
        }
        ids
    }

    /// Underlying raw operation record.
    #[napi]
    pub fn store_operation(&self) -> OperationRecord {
        self.data.clone()
    }

    /// Convert to an OperationRecord for OpStore persistence.
    #[napi]
    pub fn to_record(&self) -> OperationRecord {
        self.data.clone()
    }

    /// Convert a single Commit object into an Operation, mapping any parent operations or parent commit IDs.
    #[napi(js_name = "from_commit")]
    pub fn from_commit(
        repo: &Repository,
        commit: &crate::commit::Commit,
        parent_op_ids: Option<Vec<String>>,
    ) -> napi::Result<Operation> {
        let commit_id = commit.id();
        let msg = commit.message()?;
        let author = commit.author()?;

        let parents = match parent_op_ids {
            Some(p) => p,
            None => {
                let parent_commit_ids = commit.parent_ids()?;
                parent_commit_ids
                    .into_iter()
                    .map(|pid| {
                        let mut hasher = blake3::Hasher::new();
                        hasher.update(pid.as_bytes());
                        hasher.finalize().to_hex().to_string()
                    })
                    .collect()
            }
        };

        let op_id = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(commit_id.as_bytes());
            hasher.update(msg.as_bytes());
            for p in &parents {
                hasher.update(p.as_bytes());
            }
            hasher.finalize().to_hex().to_string()
        };

        let view_id = format!("view-{}", op_id);
        let mut meta = HashMap::new();
        meta.insert("author".to_string(), author.name);
        meta.insert("email".to_string(), author.email);
        meta.insert("commit_id".to_string(), commit_id.clone());

        let op_store = repo.op_store().ok();

        let mut predecessors = HashMap::new();
        predecessors.insert(commit_id, Vec::new());

        let mut op = Operation::new(
            op_id,
            view_id,
            parents,
            msg.trim().to_string(),
            Some(meta),
            op_store.as_ref(),
        );
        op.predecessors = Some(predecessors);
        Ok(op)
    }

    /// Compose multiple operations into a merged operation.
    #[napi]
    pub fn compose(operations: Vec<&Operation>) -> napi::Result<Operation> {
        if operations.is_empty() {
            return Err(napi::Error::from_reason(
                "Cannot compose empty list of operations",
            ));
        }

        let first = operations[0];
        let last = operations[operations.len() - 1];

        let mut combined_desc = Vec::new();
        let mut combined_meta = HashMap::new();
        let all_parents: Vec<String> = first.parent_ids().into_iter().map(|p| p.hex()).collect();

        for op in &operations {
            combined_desc.push(op.description());
            for (k, v) in &op.data.metadata.attributes {
                combined_meta.insert(k.clone(), v.clone());
            }
        }

        let op_id = {
            let mut hasher = blake3::Hasher::new();
            for op in &operations {
                hasher.update(op.id().hex().as_bytes());
            }
            hasher.finalize().to_hex().to_string()
        };

        let op_store = last.op_store.clone().or_else(|| first.op_store.clone());

        Ok(Operation::new(
            op_id,
            last.view_id().hex(),
            all_parents,
            combined_desc.join("\n\n"),
            Some(combined_meta),
            op_store.as_ref(),
        ))
    }
}

/// Error that may occur during evaluation or resolution of operation set expressions.
#[derive(Debug, Error)]
pub enum OpsetResolutionError {
    #[error(r#"The "{expr}" expression resolved to more than one operation"#)]
    MultipleOperations {
        expr: String,
        candidates: Vec<String>,
    },
    #[error(r#"The "{0}" expression resolved to no operations"#)]
    EmptyOperations(String),
    #[error(r#"Operation ID "{0}" is not a valid hexadecimal prefix"#)]
    InvalidIdPrefix(String),
    #[error(r#"No operation ID matching "{0}""#)]
    NoSuchOperation(String),
    #[error(r#"Operation ID prefix "{0}" is ambiguous"#)]
    AmbiguousIdPrefix(String),
}

impl From<OpsetResolutionError> for napi::Error {
    fn from(e: OpsetResolutionError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Statistics about an operation reparenting run.
#[derive(Clone, Debug, Eq, PartialEq)]
#[napi(object)]
pub struct ReparentStats {
    /// New head operation IDs in order of the old head operations.
    pub new_head_ids: Vec<String>,
    /// The number of rewritten operations.
    pub rewritten_count: u32,
    /// The number of ancestor operations that became unreachable from the rewritten heads.
    pub unreachable_count: u32,
}

#[napi]
impl Repository {
    fn ops_dir_path(&self) -> std::path::PathBuf {
        let r = thread_local(&self.inner);
        if let Some(custom) = r
            .config_snapshot()
            .string("init.opsDir")
            .map(|v| String::from_utf8_lossy(&v).into_owned())
        {
            let p = std::path::PathBuf::from(custom);
            if p.is_dir() || p.join("op_store").is_dir() {
                return p;
            }
        }
        let git_dir = r.git_dir();
        if git_dir.join("ops").join("op_store").is_dir() || git_dir.join("ops").is_dir() {
            return git_dir.join("ops");
        }
        if git_dir.join("repo").join("op_store").is_dir() {
            return git_dir.join("repo");
        }
        if let Some(wd) = r.workdir() {
            if wd.join(".git").join("ops").is_dir() {
                return wd.join(".git").join("ops");
            }
            if wd.join(".jj").join("repo").is_dir() {
                return wd.join(".jj").join("repo");
            }
        }
        git_dir.join("ops")
    }

    /// Check whether an op store directory exists.
    #[napi]
    pub fn is_op_repo(&self) -> bool {
        let r = thread_local(&self.inner);
        let git_dir = r.git_dir();
        let config_ops_dir = r
            .config_snapshot()
            .string("init.opsDir")
            .map(|v| String::from_utf8_lossy(&v).into_owned());

        if let Some(custom) = config_ops_dir {
            let p = std::path::PathBuf::from(custom);
            if p.exists() {
                return true;
            }
        }
        if git_dir.join("ops").is_dir() || git_dir.join("repo").join("op_store").is_dir() {
            return true;
        }
        if let Some(wd) = r.workdir() {
            wd.join(".git").join("ops").is_dir()
                || wd.join(".git").join("repo").join("op_store").is_dir()
                || wd.join(".jj").is_dir()
        } else {
            git_dir.join(".jj").is_dir()
        }
    }

    #[napi]
    pub fn has_op_store(&self) -> bool {
        self.is_op_repo()
    }

    #[napi]
    pub fn is_jj_repo(&self) -> bool {
        self.is_op_repo()
    }

    /// Obtain the OpHeadsStore handle for this repository.
    /// Defaults to `<git-dir>/ops/op_heads/`.
    #[napi(js_name = "OpHeadsStore")]
    pub fn op_heads_store(&self) -> napi::Result<OpHeadsStore> {
        let base_dir = self.ops_dir_path();
        OpHeadsStore::new(base_dir.to_string_lossy().into_owned())
    }

    #[napi]
    pub fn jj_op_heads_store(&self) -> napi::Result<OpHeadsStore> {
        self.op_heads_store()
    }

    /// Obtain the OpStore handle for this repository.
    /// Defaults to `<git-dir>/ops/op_store/`.
    #[napi(js_name = "OpStore")]
    pub fn op_store(&self) -> napi::Result<OpStore> {
        let base_dir = self.ops_dir_path();
        let op_store_path = if base_dir.join("op_store").is_dir() || !base_dir.exists() {
            base_dir.join("op_store")
        } else {
            base_dir.clone()
        };
        let r = thread_local(&self.inner);
        let head_id = r.head_commit().ok().map(|c| c.id.to_string());
        OpStore::new(op_store_path.to_string_lossy().into_owned(), head_id)
    }

    #[napi]
    pub fn jj_op_store(&self) -> napi::Result<OpStore> {
        self.op_store()
    }

    /// Initialise repository operation store metadata (`<git-dir>/ops/op_store/` and `op_heads/`).
    #[napi]
    pub fn init_op_repo(&self) -> napi::Result<OpHeadsStore> {
        self.init_op_repo_at(None)
    }

    /// Initialise operation store metadata at a designated directory (or default `<git-dir>/ops`).
    #[napi]
    pub fn init_op_repo_at(&self, ops_dir: Option<String>) -> napi::Result<OpHeadsStore> {
        let base_dir = match ops_dir {
            Some(d) => std::path::PathBuf::from(d),
            None => self.ops_dir_path(),
        };
        let r = thread_local(&self.inner);
        let head_id = r.head_commit().ok().map(|c| c.id.to_string());
        OpHeadsStore::init(base_dir.to_string_lossy().into_owned(), head_id)
    }

    #[napi]
    pub fn init_op_store(&self) -> napi::Result<OpHeadsStore> {
        self.init_op_repo()
    }

    #[napi]
    pub fn init_jj(&self) -> napi::Result<OpHeadsStore> {
        self.init_op_repo()
    }

    /// Load current head operations from the repository's OpStore.
    #[napi]
    pub fn current_head_ops(&self) -> napi::Result<Vec<OperationRecord>> {
        let op_store = self.op_store()?;
        let heads_store = self.op_heads_store()?;
        get_current_head_ops(&heads_store, &op_store)
    }

    /// Resolve an operation expression (e.g. `"@"`, `"-1"`, `"@-"`, `"head-id"`) against the repository.
    #[napi]
    pub fn resolve_op(&self, op_str: String) -> napi::Result<OperationRecord> {
        resolve_op_with_repo(self, op_str)
    }

    /// Reparent an operation range onto `dest_op_id`.
    #[napi]
    pub fn reparent_operations(
        &self,
        root_op_ids: Vec<String>,
        head_op_ids: Vec<String>,
        dest_op_id: String,
    ) -> napi::Result<ReparentStats> {
        let op_store = self.op_store()?;
        let dest_op = op_store.read_operation(dest_op_id)?;

        let mut root_ops = Vec::new();
        for id in root_op_ids {
            root_ops.push(op_store.read_operation(id)?);
        }

        let mut head_ops = Vec::new();
        for id in head_op_ids {
            head_ops.push(op_store.read_operation(id)?);
        }

        reparent_range(&op_store, root_ops, head_ops, dest_op)
    }
}

/// Load current head operations given an `OpHeadsStore` and `OpStore`.
#[napi]
pub fn get_current_head_ops(
    op_heads_store: &OpHeadsStore,
    op_store: &OpStore,
) -> napi::Result<Vec<OperationRecord>> {
    let head_ids = op_heads_store.get_op_heads()?;
    let mut ops = Vec::new();
    for id in head_ids {
        if let Ok(op) = op_store.read_operation(id) {
            ops.push(op);
        }
    }
    Ok(ops)
}

/// Resolve an operation expression against a repository, returning the resolved OperationRecord.
#[napi]
pub fn resolve_op_for_load(repo: &Repository, op_str: String) -> napi::Result<OperationRecord> {
    resolve_op_with_repo(repo, op_str)
}

/// Resolve an operation expression against a repository.
#[napi]
pub fn resolve_op_with_repo(repo: &Repository, op_str: String) -> napi::Result<OperationRecord> {
    let op_store = repo.op_store()?;
    let heads = repo.current_head_ops()?;
    resolve_op_at(&op_store, heads, op_str).map_err(|e| err("resolve_op", e))
}

/// Resolves an operation expression given the current head operations.
#[napi]
pub fn resolve_op_at(
    op_store: &OpStore,
    head_ops: Vec<OperationRecord>,
    op_str: String,
) -> napi::Result<OperationRecord> {
    resolve_op_at_internal(op_store, &head_ops, &op_str).map_err(|e| err("resolve_op_at", e))
}

pub fn resolve_op_at_internal(
    op_store: &OpStore,
    head_ops: &[OperationRecord],
    op_str: &str,
) -> Result<OperationRecord, OpsetResolutionError> {
    let op_symbol = op_str.trim_end_matches(['-', '+']);
    let op_postfix = &op_str[op_symbol.len()..];

    let mut current = match op_symbol {
        "@" | "" => match head_ops {
            [single] => single.clone(),
            [] => return Err(OpsetResolutionError::EmptyOperations("@".to_string())),
            multiple => {
                return Err(OpsetResolutionError::MultipleOperations {
                    expr: "@".to_string(),
                    candidates: multiple.iter().map(|o| o.id.clone()).collect(),
                });
            }
        },
        id_or_prefix => {
            let res = op_store
                .resolve_operation_id_prefix(id_or_prefix.to_string())
                .map_err(|e| OpsetResolutionError::NoSuchOperation(e.to_string()))?;
            if res.ambiguous {
                return Err(OpsetResolutionError::AmbiguousIdPrefix(
                    id_or_prefix.to_string(),
                ));
            }
            if !res.found || res.id.is_none() {
                return Err(OpsetResolutionError::NoSuchOperation(
                    id_or_prefix.to_string(),
                ));
            }
            op_store
                .read_operation(res.id.unwrap())
                .map_err(|_| OpsetResolutionError::NoSuchOperation(id_or_prefix.to_string()))?
        }
    };

    for c in op_postfix.chars() {
        match c {
            '-' => {
                if current.parents.is_empty() {
                    return Err(OpsetResolutionError::EmptyOperations(op_str.to_string()));
                }
                let parent_id = &current.parents[0];
                current = op_store
                    .read_operation(parent_id.clone())
                    .map_err(|_| OpsetResolutionError::NoSuchOperation(parent_id.clone()))?;
            }
            '+' => {
                let children = find_child_ops(op_store, head_ops.to_vec(), current.id.clone());
                if children.is_empty() {
                    return Err(OpsetResolutionError::EmptyOperations(op_str.to_string()));
                }
                current = children[0].clone();
            }
            _ => break,
        }
    }

    Ok(current)
}

/// Find all child operations of `target_id` among heads and their ancestors.
#[napi]
pub fn find_child_ops(
    op_store: &OpStore,
    head_ops: Vec<OperationRecord>,
    target_id: String,
) -> Vec<OperationRecord> {
    let mut children = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = head_ops;

    while let Some(op) = queue.pop() {
        if !visited.insert(op.id.clone()) {
            continue;
        }
        if op.parents.iter().any(|p| p == &target_id) {
            children.push(op.clone());
        }
        for parent_id in &op.parents {
            if let Ok(parent_op) = op_store.read_operation(parent_id.clone()) {
                queue.push(parent_op);
            }
        }
    }
    children
}

/// Reparent a range of operations onto `dest_op`.
#[napi]
pub fn reparent_range(
    op_store: &OpStore,
    _root_ops: Vec<OperationRecord>,
    head_ops: Vec<OperationRecord>,
    dest_op: OperationRecord,
) -> napi::Result<ReparentStats> {
    let mut rewritten_ids = HashMap::new();
    let mut dest_once = Some(dest_op.id.clone());

    for old_op in &head_ops {
        let mut data = old_op.clone();
        data.parents = data
            .parents
            .iter()
            .filter_map(|id| rewritten_ids.get(id).cloned().or_else(|| dest_once.take()))
            .collect();
        data.id = String::new();
        let new_id = op_store.write_operation(data)?;
        rewritten_ids.insert(old_op.id.clone(), new_id);
    }

    let mut dest_once = Some(dest_op.id.clone());
    let new_head_ids: Vec<String> = head_ops
        .iter()
        .filter_map(|op| {
            rewritten_ids
                .get(&op.id)
                .cloned()
                .or_else(|| dest_once.take())
        })
        .collect();

    Ok(ReparentStats {
        new_head_ids,
        rewritten_count: rewritten_ids.len() as u32,
        unreachable_count: 0,
    })
}
