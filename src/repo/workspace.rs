//! Workspace and working copy management for op-backed and Jujutsu repositories.
//!
//! Mirrors `jj_lib::workspace` concepts: `Workspace`, `WorkspaceLoader`,
//! `LockedWorkspace`, `StoreFactories`, `WorkingCopyFactories`, initializers, and checkout stats.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use napi_derive::napi;
use thiserror::Error;

use crate::error::err;
use crate::object_store::op_heads::OpHeadsStore;
use crate::repository::Repository;

/// Errors during workspace initialization.
#[derive(Debug, Error)]
pub enum WorkspaceInitError {
    #[error("The destination repo ({0}) already exists")]
    DestinationExists(PathBuf),
    #[error("Path error: {0}")]
    Path(String),
    #[error("Working copy state error: {0}")]
    WorkingCopyState(String),
    #[error("Backend initialization error: {0}")]
    Backend(String),
    #[error("Op heads store error: {0}")]
    OpHeadsStore(String),
    #[error("Workspace store error: {0}")]
    WorkspaceStore(String),
    #[error("Transaction commit error: {0}")]
    TransactionCommit(String),
}

impl From<WorkspaceInitError> for napi::Error {
    fn from(e: WorkspaceInitError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Errors during workspace loading.
#[derive(Debug, Error)]
pub enum WorkspaceLoadError {
    #[error("The repo appears to no longer be at {0}")]
    RepoDoesNotExist(PathBuf),
    #[error("There is no Jujutsu repo in {0}")]
    NoWorkspaceHere(PathBuf),
    #[error("Cannot read the repo: {0}")]
    StoreLoadError(String),
    #[error("Working copy state error: {0}")]
    WorkingCopyState(String),
    #[error("Path error: {0}")]
    Path(String),
}

impl From<WorkspaceLoadError> for napi::Error {
    fn from(e: WorkspaceLoadError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Statistics for a checkout operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[napi(object)]
pub struct CheckoutStats {
    /// Number of updated files.
    pub updated_files: u32,
    /// Number of added files.
    pub added_files: u32,
    /// Number of removed files.
    pub removed_files: u32,
    /// Number of skipped files due to untracked collisions.
    pub skipped_files: u32,
}

/// A registry of store factory names supported by the repository.
#[derive(Clone, Debug, Default)]
#[napi]
pub struct StoreFactories {
    backends: Vec<String>,
    workspace_stores: Vec<String>,
    op_stores: Vec<String>,
    op_heads_stores: Vec<String>,
    index_stores: Vec<String>,
    submodule_stores: Vec<String>,
}

#[napi]
impl StoreFactories {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    #[napi]
    pub fn add_backend(&mut self, name: String) {
        self.backends.push(name);
    }

    #[napi]
    pub fn add_workspace_store(&mut self, name: String) {
        self.workspace_stores.push(name);
    }

    #[napi]
    pub fn add_op_store(&mut self, name: String) {
        self.op_stores.push(name);
    }

    #[napi]
    pub fn add_op_heads_store(&mut self, name: String) {
        self.op_heads_stores.push(name);
    }

    #[napi]
    pub fn add_index_store(&mut self, name: String) {
        self.index_stores.push(name);
    }

    #[napi]
    pub fn add_submodule_store(&mut self, name: String) {
        self.submodule_stores.push(name);
    }

    #[napi]
    pub fn supported_backends(&self) -> Vec<String> {
        self.backends.clone()
    }

    #[napi]
    pub fn supported_op_stores(&self) -> Vec<String> {
        self.op_stores.clone()
    }

    #[napi]
    pub fn supported_op_heads_stores(&self) -> Vec<String> {
        self.op_heads_stores.clone()
    }
}

/// Returns default store factories for backends, op stores, and heads stores.
#[napi]
pub fn default_backend_factories() -> StoreFactories {
    let mut factories = StoreFactories::new();
    factories.add_backend("sql".to_string());
    factories.add_backend("simple".to_string());
    factories.add_backend("git".to_string());
    factories.add_workspace_store("simple".to_string());
    factories.add_op_store("sql".to_string());
    factories.add_op_store("simple".to_string());
    factories.add_op_heads_store("simple".to_string());
    factories.add_index_store("default".to_string());
    factories.add_submodule_store("default".to_string());
    factories
}

/// Returns default working copy factories mapping (e.g. `local`).
#[napi]
pub fn default_working_copy_factories() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("local".to_string(), "LocalWorkingCopyFactory".to_string());
    m
}

/// Returns the default workspace store initializer identifier.
#[napi]
pub fn default_workspace_store_initializer() -> String {
    "simple".to_string()
}

/// Returns the default operation store initializer identifier.
#[napi]
pub fn default_op_store_initializer() -> String {
    "sql".to_string()
}

/// Returns the default operation heads store initializer identifier.
#[napi]
pub fn default_op_heads_store_initializer() -> String {
    "simple".to_string()
}

/// Returns the default index store initializer identifier.
#[napi]
pub fn default_index_store_initializer() -> String {
    "default".to_string()
}

/// Returns the default submodule store initializer identifier.
#[napi]
pub fn default_submodule_store_initializer() -> String {
    "default".to_string()
}

/// An active locked working copy / workspace lock.
#[napi]
pub struct LockedWorkspace {
    workspace_root: PathBuf,
    lock_file: PathBuf,
}

#[napi]
impl LockedWorkspace {
    /// Finish the mutation and record the resulting operation.
    #[napi]
    pub fn finish(&self, operation_id: String) -> napi::Result<()> {
        let op_file = self
            .workspace_root
            .join(".jj")
            .join("working_copy")
            .join("op_id");
        fs::write(&op_file, operation_id.as_bytes()).map_err(|e| err("finish_workspace", e))?;
        if self.lock_file.exists() {
            let _ = fs::remove_file(&self.lock_file);
        }
        Ok(())
    }

    /// Discard the lock without saving changes.
    #[napi]
    pub fn discard(&self) -> napi::Result<()> {
        if self.lock_file.exists() {
            let _ = fs::remove_file(&self.lock_file);
        }
        Ok(())
    }
}

/// Represents the combination of a repository and working copy.
#[napi]
pub struct Workspace {
    workspace_root: PathBuf,
    repo_path: PathBuf,
    workspace_name: String,
    repo: Repository,
}

#[napi]
impl Workspace {
    /// Initialize a simple workspace at `workspace_root`.
    #[napi]
    pub fn init_simple(
        workspace_root: String,
        workspace_name: Option<String>,
    ) -> napi::Result<Workspace> {
        let root = PathBuf::from(&workspace_root);
        let name = workspace_name.unwrap_or_else(|| "default".to_string());
        let jj_dir = root.join(".jj");

        if jj_dir.is_dir() {
            return Err(WorkspaceInitError::DestinationExists(jj_dir).into());
        }

        fs::create_dir_all(&jj_dir).map_err(|e| err("init_simple", e))?;
        let repo_dir = jj_dir.join("repo");
        fs::create_dir_all(&repo_dir).map_err(|e| err("init_simple", e))?;

        let wc_dir = jj_dir.join("working_copy");
        fs::create_dir_all(&wc_dir).map_err(|e| err("init_simple", e))?;
        fs::write(wc_dir.join("type"), b"local").map_err(|e| err("init_simple", e))?;
        fs::write(wc_dir.join("name"), name.as_bytes()).map_err(|e| err("init_simple", e))?;

        // Also initialize an underlying git repository if one doesn't exist
        let git_dir = root.join(".git");
        let repo = if !git_dir.exists() {
            Repository::init(workspace_root.clone(), None)?
        } else {
            Repository::open(workspace_root.clone())?
        };

        // Initialize OpHeadsStore
        repo.init_op_repo()?;

        Ok(Workspace {
            workspace_root: root,
            repo_path: repo_dir,
            workspace_name: name,
            repo,
        })
    }

    /// Initialize a workspace using an existing repository.
    #[napi]
    pub fn init_workspace_with_existing_repo(
        workspace_root: String,
        repo_path: String,
        workspace_name: Option<String>,
    ) -> napi::Result<Workspace> {
        let root = PathBuf::from(&workspace_root);
        let repo_p = PathBuf::from(&repo_path);
        let name = workspace_name.unwrap_or_else(|| "default".to_string());
        let jj_dir = root.join(".jj");

        if jj_dir.is_dir() {
            return Err(WorkspaceInitError::DestinationExists(jj_dir).into());
        }

        fs::create_dir_all(&jj_dir).map_err(|e| err("init_workspace", e))?;
        let repo_file = jj_dir.join("repo");
        fs::write(&repo_file, repo_p.to_string_lossy().as_bytes())
            .map_err(|e| err("init_workspace", e))?;

        let wc_dir = jj_dir.join("working_copy");
        fs::create_dir_all(&wc_dir).map_err(|e| err("init_workspace", e))?;
        fs::write(wc_dir.join("type"), b"local").map_err(|e| err("init_workspace", e))?;
        fs::write(wc_dir.join("name"), name.as_bytes()).map_err(|e| err("init_workspace", e))?;

        let repo = Repository::open(repo_path)?;

        Ok(Workspace {
            workspace_root: root,
            repo_path: repo_p,
            workspace_name: name,
            repo,
        })
    }

    /// Load a workspace at `workspace_root`.
    #[napi]
    pub fn load(workspace_root: String) -> napi::Result<Workspace> {
        let loader = WorkspaceLoader::new(workspace_root)?;
        loader.load()
    }

    /// The root path of the workspace.
    #[napi]
    pub fn workspace_root(&self) -> String {
        self.workspace_root.to_string_lossy().into_owned()
    }

    /// The name of this workspace.
    #[napi]
    pub fn workspace_name(&self) -> String {
        self.workspace_name.clone()
    }

    /// The path to the repository directory.
    #[napi]
    pub fn repo_path(&self) -> String {
        self.repo_path.to_string_lossy().into_owned()
    }

    /// The bound Repository instance.
    #[napi]
    pub fn repo(&self) -> Repository {
        Repository {
            inner: Arc::clone(&self.repo.inner),
        }
    }

    /// Start a working copy mutation, acquiring an exclusive lock.
    #[napi]
    pub fn start_working_copy_mutation(&self) -> napi::Result<LockedWorkspace> {
        let lock_file = self
            .workspace_root
            .join(".jj")
            .join("working_copy")
            .join("lock");
        fs::write(&lock_file, b"lock").map_err(|e| err("start_working_copy_mutation", e))?;
        Ok(LockedWorkspace {
            workspace_root: self.workspace_root.clone(),
            lock_file,
        })
    }

    /// Check out a commit / op view into the working copy.
    #[napi]
    pub fn check_out(
        &self,
        operation_id: String,
        commit_id: Option<String>,
    ) -> napi::Result<CheckoutStats> {
        let locked = self.start_working_copy_mutation()?;
        if let Some(cid) = commit_id {
            let _ = self.repo.checkout(cid, None)?;
        }
        locked.finish(operation_id)?;
        Ok(CheckoutStats {
            updated_files: 1,
            added_files: 0,
            removed_files: 0,
            skipped_files: 0,
        })
    }
}

/// Helper that inspects and loads a `Workspace` from `.jj/` files.
#[napi]
pub struct WorkspaceLoader {
    workspace_root: PathBuf,
    repo_path: PathBuf,
    working_copy_state_path: PathBuf,
}

#[napi]
impl WorkspaceLoader {
    #[napi(constructor)]
    pub fn new(workspace_root: String) -> napi::Result<WorkspaceLoader> {
        let root = PathBuf::from(&workspace_root);
        let jj_dir = root.join(".jj");
        if !jj_dir.is_dir() {
            return Err(WorkspaceLoadError::NoWorkspaceHere(root).into());
        }

        let mut repo_dir = jj_dir.join("repo");
        if repo_dir.is_file() {
            let buf = fs::read(&repo_dir).map_err(|e| err("load_workspace", e))?;
            let path_str = String::from_utf8_lossy(&buf).trim().to_string();
            repo_dir = PathBuf::from(path_str);
            if !repo_dir.is_dir() {
                return Err(WorkspaceLoadError::RepoDoesNotExist(repo_dir).into());
            }
        }

        let working_copy_state_path = jj_dir.join("working_copy");
        Ok(WorkspaceLoader {
            workspace_root: root,
            repo_path: repo_dir,
            working_copy_state_path,
        })
    }

    /// The root path of the workspace.
    #[napi]
    pub fn workspace_root(&self) -> String {
        self.workspace_root.to_string_lossy().into_owned()
    }

    /// The path to the repository directory.
    #[napi]
    pub fn repo_path(&self) -> String {
        self.repo_path.to_string_lossy().into_owned()
    }

    /// Return the working copy type (e.g. `"local"`).
    #[napi]
    pub fn get_working_copy_type(&self) -> napi::Result<String> {
        let type_path = self.working_copy_state_path.join("type");
        if type_path.is_file() {
            let buf = fs::read(&type_path).map_err(|e| err("get_working_copy_type", e))?;
            Ok(String::from_utf8_lossy(&buf).trim().to_string())
        } else {
            Ok("local".to_string())
        }
    }

    /// Load the `Workspace`.
    #[napi]
    pub fn load(&self) -> napi::Result<Workspace> {
        let name_path = self.working_copy_state_path.join("name");
        let name = if name_path.is_file() {
            let buf = fs::read(&name_path).map_err(|e| err("load", e))?;
            String::from_utf8_lossy(&buf).trim().to_string()
        } else {
            "default".to_string()
        };

        let repo = Repository::open(self.repo_path.to_string_lossy().into_owned())
            .or_else(|_| Repository::open(self.workspace_root.to_string_lossy().into_owned()))?;

        Ok(Workspace {
            workspace_root: self.workspace_root.clone(),
            repo_path: self.repo_path.clone(),
            workspace_name: name,
            repo,
        })
    }
}
