use std::sync::Arc;

use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::object::lossy;
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
    /// Object format for the new repository (`"sha1"` or `"sha256"`). Overrides the
    /// `init.defaultObjectFormat` configuration; when omitted, that key (or git's default,
    /// `sha1`) decides the format.
    pub object_format: Option<String>,
    /// Compatibility object format (`extensions.compatObjectFormat`), e.g. `"sha1"` next to an
    /// `objectFormat` of `"sha256"` so the store also accepts SHA-1 object ids. Git only supports
    /// `"sha1"` here.
    pub compat_object_format: Option<String>,
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
    /// `initialHead` for the default branch, `description`, `objectFormat` and `compatObjectFormat`).
    ///
    /// `objectFormat` overrides the `init.defaultObjectFormat` configuration (which itself
    /// defaults to `sha1`), exactly like git's `--object-format` vs `init.defaultObjectFormat`.
    /// When `objectFormat` is `sha256` (or `compatObjectFormat` is set), the repository format
    /// version is bumped and `extensions.objectFormat` / `extensions.compatObjectFormat` are
    /// written into the local config, then the repository is re-opened so the returned handle
    /// actually uses the chosen hash.
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
            .clone()
            .map(to_gix_open_options)
            .unwrap_or_default();

        // 1. Create the repository skeleton. We deliberately do NOT set the object hash at
        //    create time: gix cannot write `extensions.objectFormat` / bump
        //    `core.repositoryformatversion` the way git does, so we resolve the effective
        //    format from the resolved config stack below and apply it ourselves.
        let ts = gix::ThreadSafeRepository::init_opts(&path, kind, create_options, open_opts)
            .map_err(|e| err("init_opts", e))?;
        let repo = Repository {
            inner: Arc::new(ts),
        };

        // 2. Resolve the effective object / compatibility formats from the full config
        //    stack (system/global/user config plus any open-option overrides), mirroring
        //    git's own `init` precedence.
        let default_object_format = {
            let r = thread_local(&repo.inner);
            r.config_snapshot()
                .string("init.defaultObjectFormat")
                .map(|v| lossy(&v).to_ascii_lowercase())
        };

        let object_format = match options.object_format.as_deref() {
            Some(v) => normalize_object_format(v)?,
            None => default_object_format
                .filter(|v| valid_object_format(v))
                .unwrap_or_else(|| "sha1".to_string()),
        };
        let compat = match options.compat_object_format.as_deref() {
            Some(v) => {
                if !v.eq_ignore_ascii_case("sha1") {
                    return Err(err_str(
                        "compatObjectFormat",
                        format!("unsupported compatObjectFormat '{v}' (only 'sha1' is supported)"),
                    ));
                }
                Some("sha1".to_string())
            }
            None => None,
        };

        // 3. Persist the chosen formats into the local config, exactly like git does.
        let needs_v1 = object_format == "sha256" || compat.is_some();
        if needs_v1 {
            repo.config_set("core.repositoryformatversion".to_string(), "1".to_string())?;
        }
        if object_format == "sha256" {
            repo.config_set("extensions.objectFormat".to_string(), "sha256".to_string())?;
        }
        if let Some(compat) = compat {
            repo.config_set("extensions.compatObjectFormat".to_string(), compat)?;
        }

        // 4. Re-open so the returned handle reflects the final configuration — this is
        //    required for the object hash to actually become SHA-256 (config_set cannot
        //    retroactively change a repository's hash once it has been opened).
        let repo = if needs_v1 {
            Repository::open_opts(
                path.clone(),
                options.open_options.clone().unwrap_or_default(),
            )?
        } else {
            repo
        };

        // 5. Apply the remaining post-init steps on the (re-opened) repository.
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
            inner: Arc::new(ts.into()),
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

fn normalize_object_format(v: &str) -> napi::Result<String> {
    let lower = v.to_ascii_lowercase();
    if lower == "sha1" || lower == "sha-1" {
        Ok("sha1".to_string())
    } else if lower == "sha256" || lower == "sha-256" {
        Ok("sha256".to_string())
    } else {
        Err(err_str(
            "objectFormat",
            format!("unsupported object format '{v}' (expected 'sha1' or 'sha256')"),
        ))
    }
}

fn valid_object_format(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    matches!(lower.as_str(), "sha1" | "sha-1" | "sha256" | "sha-256")
}
