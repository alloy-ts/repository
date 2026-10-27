use std::sync::Arc;

use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::{OpenOptions, Repository, to_gix_open_options};

/// Options controlling [`Repository::init_opts`] (repository creation).
#[napi(object)]
pub struct InitOptions {
    /// Create a bare repository (no worktree). Defaults to `false`.
    pub bare: Option<bool>,
    /// Require the destination to be empty. `None` behaves like git (allow non-empty unless a `.git` exists);
    /// `Some(true)` requires an empty destination; `Some(false)` explicitly allows a non-empty one.
    pub destination_must_be_empty: Option<bool>,
    /// Options used when re-opening the freshly created repository.
    pub open_options: Option<OpenOptions>,
    /// Initial HEAD / default branch name (e.g. `"main"`). Sets `HEAD` to point at
    /// `refs/heads/<name>` — mirroring git's `--initial-branch`.
    pub initial_head: Option<String>,
    /// Repository description written to `$GIT_DIR/description`.
    pub description: Option<String>,
}

fn to_gix_create_options(o: &InitOptions) -> gix::create::Options {
    let mut opts = gix::create::Options::default();
    if let Some(v) = o.destination_must_be_empty {
        opts.destination_must_be_empty = Some(v);
    }
    opts
}

#[napi]
impl Repository {
    /// Initialize a repository with the given `options` (`bare`, `destinationMustBeEmpty`, open options,
    /// `initialHead` for the default branch, and `description`).
    #[napi]
    pub fn init_opts(path: String, options: InitOptions) -> napi::Result<Repository> {
        let kind = if options.bare.unwrap_or(false) {
            gix::create::Kind::Bare
        } else {
            gix::create::Kind::WithWorktree
        };
        let create_options = to_gix_create_options(&options);
        let open_opts = options
            .open_options
            .map(to_gix_open_options)
            .unwrap_or_default();
        let ts = gix::ThreadSafeRepository::init_opts(path, kind, create_options, open_opts)
            .map_err(|e| err("init_opts", e))?;
        let repo = Repository {
            inner: Arc::new(ts),
        };

        // gix cannot set these at create time, so apply them as post-init steps.
        if let Some(initial_head) = &options.initial_head {
            repo.set_head(format!("refs/heads/{initial_head}"))?;
        }
        if let Some(description) = &options.description {
            let r = thread_local(&repo.inner);
            std::fs::write(r.git_dir().join("description"), description.as_bytes())
                .map_err(|e| err("init_description", e))?;
        }

        Ok(repo)
    }

    /// Initialize a bare repository (no worktree) at `path`. Mirrors git2's `Repository::init_bare`.
    #[napi]
    pub fn init_bare(path: String) -> napi::Result<Repository> {
        let ts = gix::init_bare(path).map_err(|e| err("init_bare", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize a repository at `path`. Pass `bare: true` for a bare repository.
    #[napi]
    pub fn init(path: String, bare: Option<bool>) -> napi::Result<Repository> {
        let r = if bare.unwrap_or(false) {
            gix::init_bare(path)
        } else {
            gix::init(path)
        }
        .map_err(|e| err("init", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }
}
