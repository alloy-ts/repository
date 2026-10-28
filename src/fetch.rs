use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use napi::Env;
use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Function};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::auth::{AuthenticatorOptions, GitAuthenticator};
use crate::error::{err, err_str, thread_local};
use crate::progress::{ProgressCallback, ProgressEvent};
use crate::remote::{FetchTags, Remote, from_fetch_tags};
use crate::repository::Repository;

use gix::refs::TargetRef;
use gix::remote::fetch::{RefLogMessage, Shallow};

/// Fetch from a remote, updating local tracking refs — the `git fetch` operation.
///
/// Returns a [`FetchOutcome`] describing what changed. Pass [`RemoteFetchOptions`] to
/// control refspecs, depth, dry-run, reflog and authentication.
#[napi]
impl Remote {
    #[napi(ts_return_type = "Promise<FetchOutcome>")]
    pub fn fetch(
        &self,
        options: Option<RemoteFetchOptions>,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> napi::Result<AsyncTask<FetchTask>> {
        let opts = options.unwrap_or_default();
        let progress = match progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(FetchTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
            refspecs: opts.refspecs,
            depth: opts.depth,
            dry_run: opts.dry_run,
            reflog_message: opts.reflog_message,
            write_packed_refs_only: opts.write_packed_refs_only,
            progress,
            interrupt: Arc::clone(&self.interrupt),
            auth: opts.auth,
            tags: opts.tags,
        }))
    }
}

/// Options controlling [`Remote::fetch`].
#[derive(Default)]
#[napi(object)]
pub struct RemoteFetchOptions {
    /// Extra refspecs to fetch in addition to (or instead of) the remote's configured ones.
    pub refspecs: Option<Vec<String>>,
    /// Shallow fetch depth (e.g. `git fetch --depth N`). `0` or `null` means full history.
    pub depth: Option<u32>,
    /// Negotiate and report what would change, but do not modify the local repository.
    pub dry_run: Option<bool>,
    /// Reflog message prefix (e.g. `"fetch"`); defaults to the standard fetch message.
    pub reflog_message: Option<String>,
    /// Write updated refs straight into `packed-refs` (used for server-side fetches).
    pub write_packed_refs_only: Option<bool>,
    /// Authenticator supplying credentials / SSH keys / agent / credential helper for this fetch.
    pub auth: Option<AuthenticatorOptions>,
    /// How to fetch tags: `"all"`, `"none"`, or `"included"`. `null` leaves the remote's
    /// configured default (git's default auto-follows included tags). Mirrors
    /// `git fetch --tags` / `--no-tags`.
    pub tags: Option<FetchTags>,
}

/// A single local reference affected by a fetch.
#[napi(object)]
pub struct UpdatedRef {
    /// The local tracking reference that was considered, if any.
    pub name: Option<String>,
    /// What happened to it: `up-to-date`, `fast-forward`, `forced-update`, `new`,
    /// or a `rejected (...)` reason.
    pub action: String,
    /// The previous object id, if the ref previously pointed at an object.
    pub old_object_id: Option<String>,
    /// The object id the ref now points at (or would point at).
    pub new_object_id: Option<String>,
}

/// Summary of a completed [`Remote::fetch`] operation.
#[napi(object)]
pub struct FetchOutcome {
    /// `"change"` if a pack was received, `"noPackReceived"` otherwise.
    pub status: String,
    /// Whether a pack was received (i.e. the remote had new objects for us).
    pub pack_received: bool,
    /// Whether the operation ran in dry-run mode (no local changes applied).
    pub dry_run: bool,
    /// Whether any negotiation with the remote occurred.
    pub negotiated: bool,
    /// Each local reference affected by the fetch.
    pub updated_refs: Vec<UpdatedRef>,
}

/// Blocking fetch work, executed on the libuv thread pool by [`AsyncTask`].
pub struct FetchTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) refspecs: Option<Vec<String>>,
    pub(crate) depth: Option<u32>,
    pub(crate) dry_run: Option<bool>,
    pub(crate) reflog_message: Option<String>,
    pub(crate) write_packed_refs_only: Option<bool>,
    pub(crate) progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    pub(crate) interrupt: Arc<AtomicBool>,
    pub(crate) auth: Option<AuthenticatorOptions>,
    pub(crate) tags: Option<FetchTags>,
}

impl Task for FetchTask {
    type Output = FetchOutcome;
    type JsValue = FetchOutcome;

    fn compute(&mut self) -> napi::Result<FetchOutcome> {
        self.interrupt.store(false, Ordering::Relaxed);
        // Best-effort credential wiring for the gix-native transport: set the authenticator's
        // environment (SSH command, GIT_TERMINAL_PROMPT, credential helper via GIT_CONFIG_*).
        // TODO: replace with a proper `gix::credentials::Program` / config overlay.
        let _guard = self
            .auth
            .take()
            .map(|o| GitAuthenticator::new(Some(o)).env_guard());
        let r = thread_local(&self.repo);
        let mut remote = match &self.name {
            Some(name) => r
                .find_remote(name.as_str())
                .map_err(|e| err("find_remote", e))?,
            None => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| err_str("remote", "anonymous remote has no url"))?;
                r.remote_at(url).map_err(|e| err("remote_at", e))?
            }
        };
        if let Some(tags) = self.tags.take() {
            remote = remote.with_fetch_tags(from_fetch_tags(tags));
        }
        let conn = remote
            .connect(gix::remote::Direction::Fetch)
            .map_err(|e| err("connect", e))?;

        let mut map_options = gix::remote::ref_map::Options::default();
        if let Some(specs) = &self.refspecs {
            for spec in specs {
                let parsed = gix::refspec::parse(
                    spec.as_str().into(),
                    gix::refspec::parse::Operation::Fetch,
                )
                .map_err(|e| err("refspec", e))?;
                map_options.extra_refspecs.push(parsed.into());
            }
        }

        let progress = ProgressCallback::new(self.progress.clone());
        let mut prepare = conn
            .prepare_fetch(progress.clone(), map_options)
            .map_err(|e| err("prepare_fetch", e))?;

        if let Some(depth) = self.depth {
            if let Some(nz) = std::num::NonZeroU32::new(depth) {
                prepare = prepare.with_shallow(Shallow::DepthAtRemote(nz));
            }
        }
        if self.dry_run.unwrap_or(false) {
            prepare = prepare.with_dry_run(true);
        }
        if let Some(msg) = &self.reflog_message {
            prepare = prepare.with_reflog_message(RefLogMessage::Override {
                message: msg.clone().into(),
            });
        }
        if self.write_packed_refs_only.unwrap_or(false) {
            prepare = prepare.with_write_packed_refs_only(true);
        }

        let outcome = prepare
            .receive(progress, &self.interrupt)
            .map_err(|e| err("receive", e))?;

        let (status, pack_received, dry_run, negotiated) = match &outcome.status {
            gix::remote::fetch::Status::Change { .. } => ("change", true, false, true),
            gix::remote::fetch::Status::NoPackReceived {
                dry_run, negotiate, ..
            } => ("noPackReceived", false, dry_run, negotiate.is_some()),
        };

        let update_refs = match &outcome.status {
            gix::remote::fetch::Status::Change { update_refs, .. } => update_refs,
            gix::remote::fetch::Status::NoPackReceived { update_refs, .. } => update_refs,
        };

        let updated_refs: Vec<UpdatedRef> = update_refs
            .iter_mapping_updates(
                &outcome.ref_map.mappings,
                &outcome.ref_map.refspecs,
                &outcome.ref_map.extra_refspecs,
            )
            .map(|(update, mapping, _spec, edit)| {
                let name = mapping
                    .local
                    .as_ref()
                    .map(|n| n.to_string())
                    .or_else(|| edit.map(|e| e.name.as_bstr().to_string()));
                let action = update.mode.to_string();
                let new_object_id = edit.and_then(|e| {
                    e.change.new_value().and_then(|t| match t {
                        TargetRef::Object(id) => Some(id.to_string()),
                        _ => None,
                    })
                });
                let old_object_id = edit.and_then(|e| {
                    e.change.previous_value().and_then(|t| match t {
                        TargetRef::Object(id) => Some(id.to_string()),
                        _ => None,
                    })
                });
                UpdatedRef {
                    name,
                    action,
                    old_object_id,
                    new_object_id,
                }
            })
            .collect();

        Ok(FetchOutcome {
            status: status.to_string(),
            pack_received,
            dry_run,
            negotiated,
            updated_refs,
        })
    }

    fn resolve(&mut self, _env: Env, output: FetchOutcome) -> napi::Result<FetchOutcome> {
        Ok(output)
    }
}

/// Convenience `git fetch` from a named remote (`"origin"` by default). Resolves the remote
/// and delegates to the same plumbing as [`Remote::fetch`]. Returns a [`FetchOutcome`]
/// describing what changed.
#[napi]
impl Repository {
    pub fn fetch(
        &self,
        remote_name: Option<String>,
        options: Option<RemoteFetchOptions>,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> napi::Result<AsyncTask<FetchTask>> {
        let opts = options.unwrap_or_default();
        let progress = match progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        let remote = remote_name.or_else(|| Some("origin".to_string()));
        Ok(AsyncTask::new(FetchTask {
            repo: Arc::clone(&self.inner),
            name: remote,
            url: None,
            refspecs: opts.refspecs,
            depth: opts.depth,
            dry_run: opts.dry_run,
            reflog_message: opts.reflog_message,
            write_packed_refs_only: opts.write_packed_refs_only,
            tags: opts.tags,
            progress,
            interrupt: Arc::new(AtomicBool::new(false)),
            auth: opts.auth,
        }))
    }

    /// Convenience helper to fetch a specific refspec from a remote.
    #[napi]
    pub fn fetch_refspec(
        &self,
        remote_name: String,
        refspec: String,
        options: Option<RemoteFetchOptions>,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> napi::Result<AsyncTask<FetchTask>> {
        let mut opts = options.unwrap_or_default();
        let mut refspecs = opts.refspecs.unwrap_or_default();
        refspecs.push(refspec);
        opts.refspecs = Some(refspecs);
        self.fetch(Some(remote_name), Some(opts), progress)
    }
}
