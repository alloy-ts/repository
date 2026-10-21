use std::io::Read;

use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// A configured filter pipeline bound to a repository.
#[napi]
pub struct FilterPipeline {
    repo: Repository,
}

#[napi]
impl FilterPipeline {
    /// Create a pipeline bound to `repo`.
    #[napi(constructor)]
    pub fn new(repo: &Repository) -> Self {
        FilterPipeline {
            repo: Repository {
                inner: repo.inner.clone(),
            },
        }
    }

    /// Convert `src` (located at repo-relative `rela_path`) from the working-tree representation
    /// to the representation stored in `git`. Returns the (possibly filtered) bytes.
    #[napi]
    pub fn convert_to_git(&mut self, src: Buffer, rela_path: String) -> Result<Buffer> {
        let r = thread_local(&self.repo.inner);
        let (mut pipeline, index) = r.filter_pipeline(None).map_err(|e| err("filter_pipeline", e))?;
        let path = std::path::Path::new(&rela_path);
        let mut outcome = pipeline
            .convert_to_git(src.as_ref(), path, &index)
            .map_err(|e| err("convert_to_git", e))?;
        let mut buf = Vec::new();
        outcome.read_to_end(&mut buf)?;
        Ok(Buffer::from(buf))
    }

    /// Convert `src` (located at repo-relative `rela_path`) from what is stored in `git` to the
    /// working-tree representation. Delayed/process filters are disabled (`can_delay: false`), so
    /// the full result is always available synchronously. Returns the (possibly filtered) bytes.
    #[napi]
    pub fn convert_to_worktree(&mut self, src: Buffer, rela_path: String) -> Result<Buffer> {
        let r = thread_local(&self.repo.inner);
        let (mut pipeline, _index) = r.filter_pipeline(None).map_err(|e| err("filter_pipeline", e))?;
        let bstr = gix::bstr::BStr::new(rela_path.as_bytes());
        let mut outcome = pipeline
            .convert_to_worktree(
                src.as_ref(),
                bstr,
                gix::filter::plumbing::pipeline::convert::to_worktree::Options {
                    can_delay: gix::filter::plumbing::driver::apply::Delay::Forbid,
                    unknown_encoding:
                        gix::filter::plumbing::pipeline::convert::to_worktree::UnknownEncoding::Ignore,
                },
            )
            .map_err(|e| err("convert_to_worktree", e))?;
        let mut buf = Vec::new();
        outcome.read_to_end(&mut buf)?;
        Ok(Buffer::from(buf))
    }

    /// Add the working-tree file at repo-relative `rela_path` to the object database, applying the
    /// to-git filters, and return its identity and entry kind. Returns `null` if the file does not
    /// exist or is of an untrackable type.
    #[napi]
    pub fn worktree_file_to_object(
        &mut self,
        rela_path: String,
    ) -> Result<Option<FilterFileObject>> {
        let r = thread_local(&self.repo.inner);
        let (mut pipeline, index) = r.filter_pipeline(None).map_err(|e| err("filter_pipeline", e))?;
        let bstr = gix::bstr::BStr::new(rela_path.as_bytes());
        let res = pipeline
            .worktree_file_to_object(bstr, &index)
            .map_err(|e| err("worktree_file_to_object", e))?;
        match res {
            Some((id, kind, md)) => Ok(Some(FilterFileObject {
                id: id.to_string(),
                kind: format!("{:?}", kind),
                is_executable: gix::fs::is_executable(&md),
                is_symlink: md.is_symlink(),
                is_file: md.is_file(),
                is_dir: md.is_dir(),
            })),
            None => Ok(None),
        }
    }
}

/// The outcome of [`FilterPipeline::worktree_file_to_object`]: the object id, its entry kind, and
/// basic filesystem metadata of the source file.
#[napi(object)]
pub struct FilterFileObject {
    /// The hex object id written to the database.
    pub id: String,
    /// The entry kind, Debug-formatted (`Blob`, `BlobExecutable`, `Link`, …).
    pub kind: String,
    /// Whether the source file is executable.
    pub is_executable: bool,
    /// Whether the source file is a symlink.
    pub is_symlink: bool,
    /// Whether the source file is a regular file.
    pub is_file: bool,
    /// Whether the source path is a directory.
    pub is_dir: bool,
}
