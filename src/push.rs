use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use gix::refs::transaction::PreviousValue;
use gix::zlib::Compression;
use gix::zlib::stream::deflate;

use crate::auth::{Auth, AuthOpts};
use crate::error::{err, err_str, thread_local};
use crate::repository::{Repository, find_git};

/// One reference update performed (or rejected) by a push — the counterpart
/// of git2's `Push` (`git_push_update`).
#[napi(object)]
pub struct PushUpdate {
    /// Name of the source (local) reference; omitted for deletions, which have
    /// no source side (git2's `git_push_update.src_refname` is null there).
    pub src_refname: Option<String>,
    /// Name of the reference to update on the server.
    pub dst_refname: String,
    /// `true` when the update was a forced (non-fast-forward) push.
    pub forced: bool,
    /// `true` when the update deleted the remote reference.
    pub deleted: bool,
    /// `"fast-forward"`, `"new"`, `"up-to-date"`, `"deleted"`, `"forced"`,
    /// `"other"` or `"rejected"`.
    pub status: String,
    /// The old target of the reference, as printed by `git push --porcelain`
    /// (abbreviated); `null` when the reference is new or deleted.
    pub src: Option<String>,
    /// The new target of the reference (abbreviated); `null` for deletions.
    pub dst: Option<String>,
    /// Rejection or failure reason, e.g. `non-fast-forward` or
    /// `remote rejected`.
    pub message: Option<String>,
}

/// Options controlling a push.
#[derive(Default)]
#[napi(object)]
pub struct PushOptions {
    /// The remote to push to (defaults to `origin`).
    pub remote: Option<String>,
    /// Push as a single atomic transaction — either all refs update or none.
    pub atomic: Option<bool>,
    /// Overwrite the remote reference even when not a fast-forward
    /// (like `git push --force`).
    pub force: Option<bool>,
    /// Simulate the push: report what would happen without contacting the
    /// server for the update itself.
    pub dry_run: Option<bool>,
    /// Treat every refspec as a deletion (like `git push --delete`).
    pub delete: Option<bool>,
    /// Push this local branch (default: the current `HEAD` branch). A convenience
    /// over `refspecs` — when omitted, the branch is resolved from `HEAD` and a
    /// single `refs/heads/<branch>[:refs/heads/<remoteBranch>]` refspec is built.
    pub branch: Option<String>,
    /// Destination branch name on the remote (default: the local `branch` name).
    /// Maps to the right-hand side of a `local:remote` refspec.
    pub remote_branch: Option<String>,
    /// Configure `branch.<name>.remote` / `branch.<name>.merge` after a successful
    /// push, like `git push -u`. Defaults to `true` on the branch-centric path and
    /// `false` when explicit `refspecs` are supplied (so it is only honored when
    /// `branch`/`remoteBranch` drive the push).
    pub set_upstream: Option<bool>,
    /// Push as if `git push --force-with-lease`: require the remote ref to still point
    /// at the expected value, refusing a non-fast-forward update. Safer than `force`.
    pub force_with_lease: Option<bool>,
    /// Push all tags (`git push --tags`).
    pub tags: Option<bool>,
    /// Push annotated tags reachable from the pushed refs (`git push --follow-tags`).
    pub follow_tags: Option<bool>,
    /// Push all branches (`git push --all`); `refspecs`/`branch` are ignored when set.
    pub push_all: Option<bool>,
    /// Server-side options (`git push --push-option` / `-o`), e.g.
    /// `["merge_request.create"]`.
    pub push_options: Option<Vec<String>>,
    /// Authenticator supplying credentials / SSH keys / agent / credential helper for network pushes.
    pub auth: Option<AuthOpts>,
}

/// The result of a push operation.
#[napi(object)]
pub struct PushOutcome {
    /// `true` when the push command exited successfully (all refs updated).
    pub ok: bool,
    /// Per-reference updates, in the order reported by git.
    pub updates: Vec<PushUpdate>,
}

/// Whether a remote URL targets a local repository (so we can push directly
/// without shelling out to `git`).
fn is_local_url(url: &str) -> bool {
    if let Some(path) = url.strip_prefix("file://") {
        let _ = path;
        return true;
    }
    // Anything with an explicit scheme other than file is a network remote.
    if url.contains("://") {
        return false;
    }
    // scp-like ssh syntax: `user@host:path`.
    if url.contains('@') && url.contains(':') {
        return false;
    }
    true
}

/// Map a [`gix::object::Kind`] to git's loose-object type string.
fn kind_str(kind: gix::object::Kind) -> &'static str {
    match kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
}

/// Write a single object into a git object directory in the standard loose
/// format (`<dir>/<aa>/<bbb...>`) using zlib compression — exactly as `git`
/// stores it, so the target repository (whether opened by gix or git) can read
/// it back. The object identity is content-addressed, so compression level and
/// path are irrelevant to correctness.
fn write_loose(
    objects_dir: &Path,
    oid: &gix::oid,
    kind: gix::object::Kind,
    data: &[u8],
) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(data.len() + 32);
    write!(header, "{} {}\0", kind_str(kind), data.len()).unwrap();
    header.extend_from_slice(data);

    let mut enc = deflate::Write::new(Vec::new(), Compression::new(6).unwrap());
    enc.write_all(&header)?;
    enc.flush()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let compressed = enc.into_inner();

    let hex = oid.to_string();
    let (prefix, rest) = hex.split_at(2);
    let dir = objects_dir.join(prefix);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(rest);
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, compressed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))?;
    }
    Ok(())
}

/// Copy every object the target repository is missing from the source into the
/// target's loose object store. This is the object-transfer half of a push for a
/// local target (the equivalent of the pack a remote `receive-pack` would
/// receive). Extra objects the source has but the pushed ref does not reach are
/// harmless: git repositories simply hold unreferenced objects.
fn transfer_missing(source: &gix::Repository, target: &gix::Repository) -> napi::Result<()> {
    let target_objects_dir = target.git_dir().join("objects");
    let iter = source.objects.iter().map_err(|e| err("push", e))?;
    for oid in iter {
        let oid = oid.map_err(|e| err("push", e))?;
        if target.find_object(oid).is_ok() {
            continue;
        }
        let obj = source.find_object(oid).map_err(|e| err("push", e))?;
        write_loose(&target_objects_dir, obj.id.as_ref(), obj.kind, &obj.data)
            .map_err(|e| err_str("push", format!("failed to write object {oid}: {e}")))?;
    }
    Ok(())
}

/// A single planned reference update, resolved from a refspec.
struct Plan {
    force: bool,
    delete: bool,
    src_refname: Option<String>,
    dst: String,
    old_oid: Option<gix::ObjectId>,
    new_oid: Option<gix::ObjectId>,
}

/// Walk commit parents from `descendant` to determine whether `ancestor` is an
/// ancestor of `descendant` (i.e. a fast-forward is possible).
fn is_ancestor(
    repo: &gix::Repository,
    ancestor: &gix::ObjectId,
    descendant: &gix::ObjectId,
) -> bool {
    if ancestor == descendant {
        return true;
    }
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![*descendant];
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if &oid == ancestor {
            return true;
        }
        if let Ok(commit) = repo.find_commit(oid) {
            for p in commit.parent_ids() {
                stack.push(p.detach());
            }
        }
    }
    seen.contains(ancestor)
}

/// Resolve the object id a source refspec's source refers to.
fn resolve_source_oid(repo: &gix::Repository, src: &str) -> napi::Result<gix::ObjectId> {
    if let Ok(oid) = gix::ObjectId::from_hex(src.as_bytes()) {
        return Ok(oid);
    }
    let mut reference = repo.find_reference(src).map_err(|e| err("push", e))?;
    let id = reference.peel_to_id().map_err(|e| err("push", e))?;
    Ok(id.detach())
}

/// Decide the destination ref name when a refspec does not give one explicitly.
fn expand_dst(repo: &gix::Repository, src: &str) -> Option<String> {
    if let Ok(reference) = repo.find_reference(src) {
        return Some(reference.name().to_string());
    }
    if src.starts_with("refs/") {
        return Some(src.to_string());
    }
    Some(format!("refs/heads/{src}"))
}

/// Parse one refspec into its components.
fn parse_refspec(spec: &str, delete_opt: bool) -> (bool, bool, String, Option<String>) {
    let (force, rest) = if let Some(s) = spec.strip_prefix('+') {
        (true, s)
    } else {
        (false, spec)
    };
    let (left, right) = match rest.split_once(':') {
        Some((l, r)) => (l, r),
        None => (rest, ""),
    };
    let delete = delete_opt || left.is_empty();
    // For deletions (`+` is irrelevant) the meaningful name is on the right.
    let src = if left.is_empty() {
        right.to_string()
    } else {
        left.to_string()
    };
    let dst = if right.is_empty() {
        None
    } else {
        Some(right.to_string())
    };
    (force, delete, src, dst)
}

/// Evaluate a plan into a status without applying anything.
fn evaluate(plan: &Plan, repo: &gix::Repository) -> (String, bool, Option<String>) {
    if plan.delete {
        return match plan.old_oid {
            None => (
                "rejected".to_string(),
                false,
                Some("remote ref does not exist".to_string()),
            ),
            Some(_) => ("deleted".to_string(), false, None),
        };
    }
    let new = plan.new_oid.expect("non-delete plan has a new oid");
    match plan.old_oid {
        None => ("new".to_string(), false, None),
        Some(old) if old == new => ("up-to-date".to_string(), false, None),
        Some(_old) if plan.force => ("forced".to_string(), true, None),
        Some(old) if is_ancestor(repo, &old, &new) => ("fast-forward".to_string(), false, None),
        Some(_) => (
            "rejected".to_string(),
            false,
            Some("non-fast-forward".to_string()),
        ),
    }
}

/// Apply a single planned update to the target repository.
fn apply(plan: &Plan, target: &gix::Repository) -> napi::Result<()> {
    if plan.delete {
        if let Ok(reference) = target.find_reference(plan.dst.as_str()) {
            reference.delete().map_err(|e| err("push", e))?;
        }
        return Ok(());
    }
    let new = plan.new_oid.expect("non-delete plan has a new oid");
    let previous = if plan.force {
        PreviousValue::Any
    } else if plan.old_oid.is_none() {
        PreviousValue::MustNotExist
    } else {
        PreviousValue::MustExistAndMatch(gix::refs::Target::Object(plan.old_oid.unwrap()))
    };
    let label = format!("push {new} -> {}", plan.dst);
    target
        .reference(plan.dst.clone(), new, previous, label)
        .map_err(|e| err("push", e))?;
    Ok(())
}

/// Native, shell-out-free push to a local target repository: transfer missing
/// objects directly into the target's object store and update its references
/// with gix ref transactions.
fn native_local_push(
    source: &gix::Repository,
    target_path: &Path,
    refspecs: &[String],
    opts: &PushOptions,
) -> napi::Result<PushOutcome> {
    let target = gix::open(target_path).map_err(|e| err("push", e))?;
    let delete_opt = opts.delete.unwrap_or(false);

    // 1. Plan every refspec.
    let mut plans: Vec<Plan> = Vec::new();
    for spec in refspecs {
        let (parsed_force, delete, src, dst) = parse_refspec(spec, delete_opt);
        // Honor the `force` option in addition to a leading `+` in the refspec, so
        // `push(refspecs, { force: true })` works for local targets too (the network
        // fallback already passes `--force`).
        let force = parsed_force
            || (opts.force.unwrap_or(false) && !opts.force_with_lease.unwrap_or(false));
        let new_oid = if delete {
            None
        } else {
            Some(resolve_source_oid(source, &src)?)
        };
        let dst = dst.or_else(|| expand_dst(source, &src)).ok_or_else(|| {
            err_str(
                "push",
                format!("could not determine destination for '{spec}'"),
            )
        })?;
        let old_oid = target
            .find_reference(dst.as_str())
            .ok()
            .and_then(|r| match r.target() {
                gix::refs::TargetRef::Object(id) => Some(id.to_owned()),
                _ => None,
            });
        plans.push(Plan {
            force,
            delete,
            src_refname: if delete { None } else { Some(src.clone()) },
            dst,
            old_oid,
            new_oid,
        });
    }

    // 2. Evaluate statuses (no side effects).
    let mut updates: Vec<PushUpdate> = Vec::with_capacity(plans.len());
    let mut any_rejected = false;
    for plan in &plans {
        let (status, forced, message) = evaluate(plan, &target);
        if status == "rejected" {
            any_rejected = true;
        }
        updates.push(PushUpdate {
            src_refname: plan.src_refname.clone(),
            dst_refname: plan.dst.clone(),
            forced,
            deleted: plan.delete,
            status: status.clone(),
            src: plan.old_oid.map(|o| o.to_string()),
            dst: plan.new_oid.map(|o| o.to_string()),
            message,
        });
    }

    let dry_run = opts.dry_run.unwrap_or(false);
    let atomic = opts.atomic.unwrap_or(false);

    // Dry-run reports what would happen; atomic aborts entirely if anything
    // would be rejected.
    if dry_run || (atomic && any_rejected) {
        return Ok(PushOutcome {
            ok: !any_rejected && !dry_run,
            updates,
        });
    }

    // 3. Transfer objects the target is missing.
    transfer_missing(source, &target)?;

    // 4. Apply the non-rejected, non-noop updates.
    for (plan, update) in plans.iter().zip(updates.iter_mut()) {
        if update.status == "rejected" || update.status == "up-to-date" {
            continue;
        }
        if let Err(e) = apply(plan, &target) {
            update.status = "rejected".to_string();
            update.message = Some(e.to_string());
            any_rejected = true;
        }
    }

    Ok(PushOutcome {
        ok: !any_rejected,
        updates,
    })
}

/// Push `refspecs` to `remote` and report per-reference updates.
///
/// gix 0.89 has no wire-push implementation, and even `gix-transport`'s local
/// transport shells out to `git receive-pack`. For **local** targets (`file://`
/// or filesystem paths) this performs a native, in-process push — transferring
/// the missing objects directly into the target's object store and updating its
/// references with gix ref transactions, with no `git` subprocess. For network
/// targets (ssh/http/git) the system `git push --porcelain` is still used as a
/// temporary fallback until the receive-pack wire protocol is implemented over
/// `gix-transport`.
///
/// Async: the (blocking) object transfer / `git push` runs on a libuv task
/// thread and resolves a `Promise<PushOutcome>`, so it does not block the JS
/// event loop.
#[napi]
impl Repository {
    #[napi(ts_return_type = "Promise<PushOutcome>")]
    pub fn push(
        &self,
        refspecs: Option<Vec<String>>,
        options: Option<PushOptions>,
    ) -> napi::Result<AsyncTask<PushTask>> {
        let options = options.unwrap_or_default();
        Ok(AsyncTask::new(PushTask {
            inner: self.inner.clone(),
            refspecs,
            options,
        }))
    }

    /// Push a branch to its remote — a discoverable, branch-centric entry point
    /// equivalent to `push(undefined, options)`.
    ///
    /// `branch` defaults to the current `HEAD` branch; `remoteBranch` defaults to
    /// the same name; `remote` defaults to `origin`; and upstream tracking is set
    /// by default (like `git push -u`). Pass `refspecs` to [`push`](Self::push)
    /// instead when you need raw, multi-refspec control.
    ///
    /// Async, returning a `Promise<PushOutcome>` (see [`push`](Self::push)).
    #[napi(ts_return_type = "Promise<PushOutcome>")]
    pub fn push_branch(&self, options: Option<PushOptions>) -> napi::Result<AsyncTask<PushTask>> {
        self.push(None, options)
    }
}

/// Blocking push work, executed on the libuv thread pool by [`AsyncTask`].
pub struct PushTask {
    inner: Arc<gix::ThreadSafeRepository>,
    refspecs: Option<Vec<String>>,
    options: PushOptions,
}

impl Task for PushTask {
    type Output = PushOutcome;
    type JsValue = PushOutcome;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let refspecs = self.refspecs.take();
        let mut options = std::mem::take(&mut self.options);
        let auth = options.auth.take();

        let mut r = thread_local(&self.inner);

        // Resolve the effective refspecs. When explicit `refspecs` are given they
        // win; otherwise a single refspec is derived from `branch`/`remoteBranch`
        // (defaulting to the current `HEAD` branch). The branch-centric path also
        // yields the `(local, remote)` branch pair used to set upstream tracking.
        let (effective_refspecs, upstream_target) = resolve_push_refspecs(&r, &refspecs, &options)?;

        // Resolve the remote URL to decide local vs network.
        let remote_name = options
            .remote
            .clone()
            .unwrap_or_else(|| "origin".to_string());
        let url: String = match r.find_remote(remote_name.as_str()) {
            Ok(remote) => remote
                .url(gix::remote::Direction::Push)
                .or_else(|| remote.url(gix::remote::Direction::Fetch))
                .map(|u| u.to_string())
                .unwrap_or_else(|| remote_name.clone()),
            Err(_) => remote_name.clone(),
        };

        let target_path = if let Some(path) = url.strip_prefix("file://") {
            Some(PathBuf::from(path))
        } else if is_local_url(&url) {
            // Filesystem path: resolve relative paths against the source repo.
            let p = Path::new(&url);
            if p.is_absolute() {
                Some(p.to_path_buf())
            } else {
                Some(r.git_dir().join(p))
            }
        } else {
            None
        };

        let outcome = if let Some(path) = target_path {
            // Local target: expand tags/all/follow-tags into concrete refspecs and
            // perform a native, in-process push.
            let final_refspecs = expand_push_refspecs(&r, &effective_refspecs, &options)?;
            native_local_push(&r, &path, &final_refspecs, &options)?
        } else {
            // Network fallback: shell out to `git push --porcelain`. TODO: native
            // send-pack over `gix-transport` (receive-pack wire protocol).
            git_push(&r, &effective_refspecs, &options, auth)?
        };

        // Configure upstream tracking after a successful branch-centric push.
        if outcome.ok {
            if let Some((branch_short, remote_branch)) = upstream_target {
                if options.set_upstream.unwrap_or(true) {
                    set_upstream_tracking(&mut r, &remote_name, &branch_short, &remote_branch)?;
                }
            }
        }

        Ok(outcome)
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Resolve the refspecs to push from either an explicit `refspecs` list or the
/// branch-centric `branch` / `remote_branch` options.
///
/// Returns the refspecs plus, for the branch-centric path, the `(local_short,
/// remote_short)` pair needed to configure upstream tracking. When explicit
/// `refspecs` are supplied the pair is `None` — `set_upstream` is only honored on
/// the branch-centric path.
fn resolve_push_refspecs(
    r: &gix::Repository,
    refspecs: &Option<Vec<String>>,
    options: &PushOptions,
) -> napi::Result<(Vec<String>, Option<(String, String)>)> {
    if let Some(specs) = refspecs {
        if !specs.is_empty() {
            return Ok((specs.clone(), None));
        }
    }

    // Branch-centric path: resolve the local branch from the option or HEAD.
    let branch = match &options.branch {
        Some(b) => b.clone(),
        None => {
            let head = r.head().map_err(|e| err("push", e))?;
            head.referent_name()
                .ok_or_else(|| err_str("push", "could not determine the current branch from HEAD"))?
                .to_string()
        }
    };
    let branch_short = branch
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch)
        .to_string();
    let remote_branch = options
        .remote_branch
        .clone()
        .unwrap_or_else(|| branch_short.clone());
    let prefix = if options.force.unwrap_or(false) {
        "+"
    } else {
        ""
    };
    let refspec = format!("{prefix}refs/heads/{branch_short}:refs/heads/{remote_branch}");
    Ok((vec![refspec], Some((branch_short, remote_branch))))
}

/// Configure `branch.<name>.remote` and `branch.<name>.merge` to track the just
/// pushed ref — the config half of `git push -u`. Writes only when no upstream is
/// already configured, so an existing tracking configuration is never clobbered.
fn set_upstream_tracking(
    r: &mut gix::Repository,
    remote_name: &str,
    branch_short: &str,
    remote_branch: &str,
) -> napi::Result<()> {
    if r.config_snapshot()
        .string(format!("branch.{branch_short}.remote").as_str())
        .is_some()
    {
        return Ok(());
    }
    let merge = format!("refs/heads/{remote_branch}");
    {
        let mut snapshot = r.config_snapshot_mut();
        let _ = snapshot.set_raw_value_by("branch", branch_short, "remote", remote_name);
        let _ = snapshot.set_raw_value_by("branch", branch_short, "merge", merge.as_str());
        let _ = snapshot.commit();
    }
    // `config_snapshot_mut` only mutates this instance's in-memory view; mirror the
    // edit to the on-disk local config so it survives a reopen.
    let path = r.common_dir().join("config");
    if path.exists() {
        let mut file =
            gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
                .map_err(|e| err("local_config", e))?;
        let _ = file.set_raw_value_by("branch", branch_short, "remote", remote_name);
        let _ = file.set_raw_value_by("branch", branch_short, "merge", merge.as_str());
        std::fs::write(&path, file.to_bstring().as_slice())
            .map_err(|e| err("local_config_write", e))?;
    }
    Ok(())
}

/// Enumerate all reference names under `prefix` (e.g. `refs/heads/` or `refs/tags/`).
fn list_refs(repo: &gix::Repository, prefix: &str) -> napi::Result<Vec<String>> {
    let mut out = Vec::new();
    for reference in repo
        .references()
        .map_err(|e| err("push", e))?
        .all()
        .map_err(|e| err("push", e))?
    {
        let reference = reference.map_err(|e| err("push", e))?;
        let name = reference.name().to_string();
        if name.starts_with(prefix) {
            out.push(name);
        }
    }
    Ok(out)
}

/// Expand `base` refspecs with the `--all` / `--tags` / `--follow-tags` additions so the
/// native local push can honor them (the network `git push` handles these via flags instead).
fn expand_push_refspecs(
    repo: &gix::Repository,
    base: &[String],
    options: &PushOptions,
) -> napi::Result<Vec<String>> {
    let mut specs = base.to_vec();
    if options.push_all.unwrap_or(false) {
        for name in list_refs(repo, "refs/heads/")? {
            specs.push(format!("{name}:{name}"));
        }
    }
    if options.tags.unwrap_or(false) {
        for name in list_refs(repo, "refs/tags/")? {
            specs.push(format!("{name}:{name}"));
        }
    }
    if options.follow_tags.unwrap_or(false) {
        // Tags to follow: those pointing at a commit reachable from a pushed commit.
        let pushed: Vec<gix::ObjectId> = base
            .iter()
            .filter_map(|s| {
                let (_, del, src, _) = parse_refspec(s, options.delete.unwrap_or(false));
                if del || src.is_empty() {
                    None
                } else {
                    resolve_source_oid(repo, &src).ok()
                }
            })
            .collect();
        for name in list_refs(repo, "refs/tags/")? {
            let mut reference = match repo.find_reference(&name) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let tag_commit = match reference.peel_to_commit() {
                Ok(c) => c.id().detach(),
                Err(_) => continue,
            };
            if pushed.iter().any(|p| is_ancestor(repo, &tag_commit, p)) {
                specs.push(format!("{name}:{name}"));
            }
        }
    }
    Ok(specs)
}

/// Shell out to the system `git push --porcelain` (network remotes only).
fn git_push(
    r: &gix::Repository,
    refspecs: &[String],
    options: &PushOptions,
    auth: Option<AuthOpts>,
) -> napi::Result<PushOutcome> {
    let what = "push";
    let git = find_git().ok_or_else(|| err_str(what, "no system git binary was found"))?;

    let mut cmd = std::process::Command::new(&git);
    cmd.arg("push").arg("--porcelain");
    if options.atomic.unwrap_or(false) {
        cmd.arg("--atomic");
    }
    if options.force.unwrap_or(false) {
        cmd.arg("--force");
    }
    if options.force_with_lease.unwrap_or(false) {
        cmd.arg("--force-with-lease");
    }
    if options.dry_run.unwrap_or(false) {
        cmd.arg("--dry-run");
    }
    if options.delete.unwrap_or(false) {
        cmd.arg("--delete");
    }
    if options.tags.unwrap_or(false) {
        cmd.arg("--tags");
    }
    if options.follow_tags.unwrap_or(false) {
        cmd.arg("--follow-tags");
    }
    if let Some(opts) = &options.push_options {
        for o in opts {
            cmd.arg("-o").arg(o);
        }
    }
    let use_all = options.push_all.unwrap_or(false);
    if use_all {
        cmd.arg("--all");
    }
    if let Some(auth) = auth {
        Auth::new(Some(auth)).apply_to_command(&mut cmd);
    }
    cmd.arg(
        options
            .remote
            .clone()
            .unwrap_or_else(|| "origin".to_string()),
    );
    if !use_all {
        cmd.args(refspecs);
    }
    cmd.current_dir(r.path());
    cmd.stdin(std::process::Stdio::null());

    let out = cmd
        .output()
        .map_err(|e| err_str(what, format!("failed to spawn git: {e}")))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let updates: Vec<PushUpdate> = stdout.lines().filter_map(parse_push_line).collect();

    if !out.status.success() && updates.is_empty() {
        return Err(err_str(
            what,
            format!(
                "git push failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(PushOutcome {
        ok: out.status.success(),
        updates,
    })
}

/// Parse one `git push --porcelain` report line:
/// `<flags>\t<from>:<to>\t<summary> (<reason>)`.
fn parse_push_line(line: &str) -> Option<PushUpdate> {
    let mut parts = line.split('\t');
    let flags = parts.next()?;
    if flags.is_empty() {
        return None;
    }
    let refs = parts.next()?;
    let (src_refname, dst_refname) = refs.split_once(':').unwrap_or((refs, ""));
    let tail = parts.next().unwrap_or("").trim();

    let (summary, message) = match tail.split_once(" (") {
        Some((summary, rest)) => (
            summary.trim(),
            Some(rest.trim_end_matches(')').trim().to_string()).filter(|m| !m.is_empty()),
        ),
        None => (tail, None),
    };

    let status = if flags.contains('=') {
        "up-to-date"
    } else if flags.contains('!') {
        "rejected"
    } else if flags.contains('-') {
        "deleted"
    } else if flags.contains('+') {
        "forced"
    } else if flags.contains('*') {
        "new"
    } else if summary.starts_with('[') {
        "other"
    } else {
        "fast-forward"
    };

    let (src, dst) = match summary.split_once("..") {
        Some((old, new)) => {
            let new = new.trim_start_matches('.');
            (
                (!old.is_empty()).then(|| old.to_string()),
                (!new.is_empty()).then(|| new.to_string()),
            )
        }
        None => (None, None),
    };

    Some(PushUpdate {
        src_refname: (!src_refname.is_empty()).then(|| src_refname.to_string()),
        dst_refname: dst_refname.to_string(),
        forced: flags.contains('+'),
        deleted: flags.contains('-'),
        status: status.into(),
        src,
        dst,
        message,
    })
}
