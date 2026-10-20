use std::sync::Arc;

use gix::objs::Write;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

#[napi]
pub struct Blob {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Blob {
    /// Raw bytes of the blob.
    #[napi]
    pub fn content(&self) -> Result<Buffer> {
        let r = thread_local(&self.repo);
        if let Some(replaced) = crate::replace::resolve_replace_head(&self.repo, &self.id) {
            let obj = r
                .find_object(replaced)
                .map_err(|e| err("find_object", e))?
                .detach();
            return Ok(Buffer::from(obj.data));
        }
        let blob = r.find_blob(self.id).map_err(|e| err("find_blob", e))?;
        Ok(Buffer::from(blob.data.to_vec()))
    }

    /// The object id of this blob.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }
}

/// Construct a [`Blob`] wrapper from a repository and object id (used by `Repository` / `GitObject`).
pub fn build_blob(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Blob {
    Blob {
        repo: Arc::clone(repo),
        id,
    }
}

#[napi]
impl Repository {
    /// Find a blob by id (content is read lazily via [`Blob::content`]).
    #[napi]
    pub fn find_blob(&self, id: String) -> Result<Blob> {
        let id = parse_oid(&id)?;
        Ok(build_blob(&self.inner, id))
    }

    /// Write raw bytes as a blob and return its id.
    #[napi]
    pub fn write_blob(&self, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write a blob using the streaming write path, returning its id.
    #[napi]
    pub fn write_blob_stream(&self, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(gix::object::Kind::Blob, data.len() as u64, &mut buf)
            .map_err(|e| err("write_blob_stream", e))?;
        Ok(id.to_string())
    }

    /// Return the well-known empty blob object id as a [`Blob`].
    #[napi]
    pub fn empty_blob(&self) -> Result<Blob> {
        let r = thread_local(&self.inner);
        let id = r.object_hash().empty_blob();
        Ok(build_blob(&self.inner, id))
    }
}
