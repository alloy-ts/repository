use std::sync::Arc;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// One entry of a materialized [`Repository::worktree_stream`].
#[napi(object)]
pub struct WorktreeStreamEntry {
    /// Slash-separated path relative to the repository.
    pub path: String,
    /// Entry mode, Debug-formatted (e.g. `Blob`, `BlobExecutable`, `Link`, `Tree`).
    pub mode: String,
    /// The object id of the entry.
    pub id: String,
    /// The entry size in bytes, or `null` when unknown before reading.
    pub size: Option<f64>,
}

/// Worktree-stream materialization task running on the libuv thread pool.
pub struct WorktreeStreamTask {
    repo: Arc<gix::ThreadSafeRepository>,
    tree_id: String,
}

impl Task for WorktreeStreamTask {
    type Output = Vec<WorktreeStreamEntry>;
    type JsValue = Vec<WorktreeStreamEntry>;

    fn compute(&mut self) -> Result<Vec<WorktreeStreamEntry>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (mut stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut out = Vec::new();
        while let Some(mut entry) = stream.next_entry().map_err(|e| err("stream_entry", e))? {
            // Capture the metadata first, then drain the entry: gix panics on the
            // next `next_entry()` call if an entry is dropped before exhaustion.
            let path = String::from_utf8_lossy(entry.relative_path().as_ref()).into_owned();
            let mode = format!("{:?}", entry.mode);
            let id = entry.id.to_string();
            let size = entry.bytes_remaining().map(|n| n as f64);
            std::io::copy(&mut entry, &mut std::io::sink())
                .map_err(|e| err("stream_entry_read", e))?;
            out.push(WorktreeStreamEntry {
                path,
                mode,
                id,
                size,
            });
        }
        Ok(out)
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        output: Vec<WorktreeStreamEntry>,
    ) -> Result<Vec<WorktreeStreamEntry>> {
        Ok(output)
    }
}

/// Worktree-stream raw-bytes task running on the libuv thread pool.
pub struct WorktreeStreamRawTask {
    repo: Arc<gix::ThreadSafeRepository>,
    tree_id: String,
}

impl Task for WorktreeStreamRawTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut read = stream.into_read();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut read, &mut bytes)
            .map_err(|e| err("stream_encode", e))?;
        Ok(bytes)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<u8>) -> Result<Buffer> {
        Ok(Buffer::from(output))
    }
}

#[napi]
impl Repository {
    /// Materialize the worktree stream of the tree at `tree_id` into an array of
    /// entries (path, mode, id, size), with filters applied and `export-ignore`
    /// honored. A live stream cannot cross the FFI boundary, so the whole stream
    /// is drained on the thread pool.
    #[napi]
    pub fn worktree_stream(&self, tree_id: String) -> Result<AsyncTask<WorktreeStreamTask>> {
        Ok(AsyncTask::new(WorktreeStreamTask {
            repo: self.inner.clone(),
            tree_id,
        }))
    }

    /// Encode the worktree stream of the tree at `tree_id` into its raw,
    /// unspecified transport bytes — the exact format produced by
    /// `Stream::into_read`, decodable again by `Stream::from_read`.
    ///
    /// This is the low-level counterpart to [`Repository::worktree_stream`]:
    /// instead of draining entries into metadata, it yields the precise bytes
    /// the encoder writes, which can be stored or re-fed into a decoder
    /// elsewhere (e.g. for `git archive`-style transport). Like the
    /// materialized variant, the whole stream is produced on the thread pool.
    #[napi]
    pub fn worktree_stream_raw(&self, tree_id: String) -> Result<AsyncTask<WorktreeStreamRawTask>> {
        Ok(AsyncTask::new(WorktreeStreamRawTask {
            repo: self.inner.clone(),
            tree_id,
        }))
    }
}
