use std::sync::Arc;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};

/// Outcome of converting data to git-internal representation (e.g. 'clean' filter).
#[napi(object)]
pub struct FilterToGitOutcome {
    /// The converted data buffer.
    pub data: napi::bindgen_prelude::Buffer,
    /// Whether any filter actually modified the input.
    pub changed: bool,
}

/// Outcome of converting data to worktree representation (e.g. 'smudge' filter).
#[napi(object)]
pub struct FilterToWorktreeOutcome {
    /// The converted data buffer.
    pub data: napi::bindgen_prelude::Buffer,
    /// Whether any filter actually modified the input.
    pub changed: bool,
}

/// A handle to git's attribute-driven filter pipeline ('clean', 'smudge', CRLF, ident).
#[napi]
pub struct FilterPipeline {
    inner: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl FilterPipeline {
    pub(crate) fn new(inner: Arc<gix::ThreadSafeRepository>) -> Self {
        Self { inner }
    }

    /// Convert input bytes for `path` (repository-relative) into the git-internal format
    /// (e.g. applying 'clean' filters and normalized CRLF -> LF endings).
    #[napi]
    pub fn convert_to_git(
        &self,
        data: napi::bindgen_prelude::Buffer,
        path: String,
    ) -> Result<FilterToGitOutcome> {
        let r = thread_local(&self.inner);
        let (mut pipeline, index) = r
            .filter_pipeline(None)
            .map_err(|e| err("filter_pipeline", e))?;
        let src = std::io::Cursor::new(data.as_ref());
        let outcome = pipeline
            .convert_to_git(src, std::path::Path::new(path.as_str()), &index)
            .map_err(|e| err("convert_to_git", e))?;

        use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
        let (bytes, changed) = match outcome {
            ToGitOutcome::Unchanged(cursor) => (cursor.into_inner().to_vec(), false),
            ToGitOutcome::Buffer(buf) => (buf.to_vec(), true),
            ToGitOutcome::Process(mut reader) => {
                let mut out = Vec::new();
                std::io::Read::read_to_end(&mut reader, &mut out)
                    .map_err(|e| err("read_to_end", e))?;
                (out, true)
            }
        };

        Ok(FilterToGitOutcome {
            data: napi::bindgen_prelude::Buffer::from(bytes),
            changed,
        })
    }

    /// Convert input bytes for `path` (repository-relative) from git format to worktree format
    /// (e.g. applying 'smudge' filters and worktree line endings).
    #[napi]
    pub fn convert_to_worktree(
        &self,
        data: napi::bindgen_prelude::Buffer,
        path: String,
    ) -> Result<FilterToWorktreeOutcome> {
        let r = thread_local(&self.inner);
        let (mut pipeline, _index) = r
            .filter_pipeline(None)
            .map_err(|e| err("filter_pipeline", e))?;
        let rela_path = gix::bstr::BStr::new(path.as_str());
        let outcome = pipeline
            .convert_to_worktree(data.as_ref(), rela_path, Default::default())
            .map_err(|e| err("convert_to_worktree", e))?;

        use gix::filter::plumbing::driver::apply::MaybeDelayed;
        use gix::filter::plumbing::pipeline::convert::ToWorktreeOutcome;
        let (bytes, changed) = match outcome {
            ToWorktreeOutcome::Unchanged(slice) => (slice.to_vec(), false),
            ToWorktreeOutcome::Buffer(buf) => (buf.to_vec(), true),
            ToWorktreeOutcome::Process(MaybeDelayed::Immediate(mut reader)) => {
                let mut out = Vec::new();
                std::io::Read::read_to_end(&mut reader, &mut out)
                    .map_err(|e| err("read_to_end", e))?;
                (out, true)
            }
            ToWorktreeOutcome::Process(MaybeDelayed::Delayed(_)) => {
                return Err(err("convert_to_worktree", "delayed processing not supported"));
            }
        };

        Ok(FilterToWorktreeOutcome {
            data: napi::bindgen_prelude::Buffer::from(bytes),
            changed,
        })
    }
}
