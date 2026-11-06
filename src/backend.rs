//! Backend interfaces and SimpleBackend storage implementations for commits, trees, and objects.
//!
//! Mirrors `jj_lib::backend` and `jj_lib::simple_backend`.

use std::fs;
use std::path::{Path, PathBuf};

use napi_derive::napi;
use thiserror::Error;

use crate::error::err;

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
#[napi]
pub struct SimpleBackend {
    path: PathBuf,
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
