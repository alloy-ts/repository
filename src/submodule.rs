use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use gix::bstr::BStr;
use gix::submodule::config::{Branch, FetchRecurse, Ignore, Update};

use std::process::Command;

use crate::error::{err, thread_local};
use crate::repository::{IndexEntryInput, Repository, find_git};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Copy a single `submodule.<name>.<key>` value into the local config transaction, but only when
/// `overwrite` is set or the key is not already present in the resolved config snapshot.
fn copy_submodule_config(
    tx: &mut gix::config::FileTransaction,
    snapshot: &gix::config::Snapshot,
    name: &str,
    key: &str,
    value: Option<String>,
    overwrite: bool,
) -> napi::Result<()> {
    if let Some(v) = value {
        let full = format!("submodule.{name}.{key}");
        if overwrite || snapshot.string(full.as_str()).is_none() {
            tx.set_raw_value(full.as_str(), v.as_str())
                .map_err(|e| err("submodule_init", e))?;
        }
    }
    Ok(())
}

/// Run a `git` command in the superproject's worktree. `args` are the git arguments (without the
/// `git` binary). Enables local `file://` submodule URLs, which modern git blocks by default.
fn git_in_super(r: &gix::Repository, args: &[&str]) -> napi::Result<()> {
    let git = find_git().ok_or_else(|| {
        napi::Error::from_reason(
            "no system git binary was found; cannot perform submodule operation",
        )
    })?;
    let workdir = r
        .workdir()
        .ok_or_else(|| napi::Error::from_reason("bare repository has no worktree"))?;
    let status = Command::new(&git)
        .arg("-c")
        .arg("protocol.file.allow=always")
        .args(args)
        .current_dir(workdir)
        .status()
        .map_err(|e| err("git_spawn", e))?;
    if !status.success() {
        return Err(napi::Error::from_reason(format!(
            "git {} failed (exit {})",
            args.join(" "),
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

/// A parsed `.gitmodules` file, exposing per-submodule configuration.
#[napi]
pub struct ModulesFile {
    inner: gix::submodule::File,
}

#[napi]
impl ModulesFile {
    /// All submodule names defined in this `.gitmodules` file.
    #[napi]
    pub fn names(&self) -> Vec<String> {
        self.inner.names().map(|n| lossy(n.as_ref())).collect()
    }

    /// The path (relative to the worktree) at which the named submodule is checked out.
    #[napi]
    pub fn path(&self, name: String) -> napi::Result<String> {
        let name = BStr::new(name.as_str());
        let p = self
            .inner
            .path(name)
            .map_err(|e| err("submodule_path", e))?;
        Ok(lossy(&p))
    }

    /// The URL from which to clone/update the named submodule.
    #[napi]
    pub fn url(&self, name: String) -> napi::Result<String> {
        let name = BStr::new(name.as_str());
        let url = self.inner.url(name).map_err(|e| err("submodule_url", e))?;
        Ok(url.to_string())
    }

    /// The `update` strategy configured for the named submodule, or `null` if unset.
    #[napi]
    pub fn update(&self, name: String) -> napi::Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .update(name)
            .map_err(|e| err("submodule_update", e))?;
        Ok(v.map(update_to_string))
    }

    /// The `branch` configured for the named submodule, or `null` if unset.
    #[napi]
    pub fn branch(&self, name: String) -> napi::Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .branch(name)
            .map_err(|e| err("submodule_branch", e))?;
        Ok(v.map(branch_to_string))
    }

    /// The `fetchRecurseSubmodules` setting for the named submodule, or `null` if unset.
    #[napi]
    pub fn fetch_recurse(&self, name: String) -> napi::Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .fetch_recurse(name)
            .map_err(|e| err("submodule_fetch_recurse", e))?;
        Ok(v.map(fetch_recurse_to_string))
    }

    /// The `ignore` setting for the named submodule, or `null` if unset.
    #[napi]
    pub fn ignore(&self, name: String) -> napi::Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .ignore(name)
            .map_err(|e| err("submodule_ignore", e))?;
        Ok(v.map(ignore_to_string))
    }

    /// The `shallow` flag for the named submodule, or `null` if unset.
    #[napi]
    pub fn shallow(&self, name: String) -> napi::Result<Option<bool>> {
        let name = BStr::new(name.as_str());
        self.inner
            .shallow(name)
            .map_err(|e| err("submodule_shallow", e))
    }
}

fn update_to_string(u: Update) -> String {
    match u {
        Update::Checkout => "checkout".to_string(),
        Update::Rebase => "rebase".to_string(),
        Update::Merge => "merge".to_string(),
        Update::Command(c) => format!("command:{}", lossy(&c)),
        Update::None => "none".to_string(),
    }
}

fn branch_to_string(b: Branch) -> String {
    match b {
        Branch::CurrentInSuperproject => "current-in-superproject".to_string(),
        Branch::Name(n) => lossy(&n),
    }
}

fn fetch_recurse_to_string(f: FetchRecurse) -> String {
    match f {
        FetchRecurse::OnDemand => "on-demand".to_string(),
        FetchRecurse::Always => "always".to_string(),
        FetchRecurse::Never => "never".to_string(),
    }
}

fn ignore_to_string(i: Ignore) -> String {
    match i {
        Ignore::All => "all".to_string(),
        Ignore::Dirty => "dirty".to_string(),
        Ignore::Untracked => "untracked".to_string(),
        Ignore::None => "none".to_string(),
    }
}

/// Submodule (`.gitmodules`) configuration access on the repository.
#[napi]
impl Repository {
    /// Open the `.gitmodules` file from the worktree, returning `null` if it is absent.
    #[napi]
    pub fn open_modules_file(&self) -> napi::Result<Option<ModulesFile>> {
        let r = thread_local(&self.inner);
        let file = r
            .open_modules_file()
            .map_err(|e| err("open_modules_file", e))?;
        Ok(file.map(|f| ModulesFile { inner: f }))
    }

    /// Return the (auto-updating) `.gitmodules` file, or `null` if it is absent.
    #[napi]
    pub fn modules(&self) -> napi::Result<Option<ModulesFile>> {
        let r = thread_local(&self.inner);
        let snapshot = r.modules().map_err(|e| err("modules", e))?;
        Ok(snapshot.map(|s| ModulesFile {
            inner: (**s).clone(),
        }))
    }
}

/// The resolved state of a submodule within its superproject.
#[napi(object)]
pub struct SubmoduleState {
    /// Whether the submodule's git repository has been cloned.
    pub repository_exists: bool,
    /// Whether the submodule lives directly in the superproject's worktree (old form).
    pub is_old_form: bool,
    /// Whether the submodule worktree is checked out.
    pub worktree_checkout: bool,
    /// Whether submodule configuration exists in the superproject's `.git/config`.
    pub superproject_configuration: bool,
}

/// Options controlling how [`Submodule::status`] is computed.
#[napi(object)]
pub struct SubmoduleStatusOptions {
    /// Which parts of the submodule status to ignore: `"none"`, `"untracked"`, `"dirty"`, or
    /// `"all"`. Defaults to `"none"`.
    pub ignore: Option<String>,
    /// Stop computing as soon as the submodule is determined to be dirty. Defaults to `false`.
    pub check_dirty: Option<bool>,
}

/// The computed status of a submodule — a more exhaustive, potentially expensive cousin of
/// [`SubmoduleState`].
#[napi(object)]
pub struct SubmoduleStatus {
    /// The always-computed cheap state (clone / worktree check).
    pub state: SubmoduleState,
    /// The commit the submodule is expected to be at, per the superproject's index.
    pub index_id: Option<String>,
    /// The commit the submodule's `HEAD` is currently checked out at.
    pub checked_out_head_id: Option<String>,
    /// Whether the submodule worktree contains changes (`null` when the repo/worktree is missing).
    pub is_dirty: Option<bool>,
}

/// Options controlling [`Submodule::clone`] and [`Submodule::update`]. Mirrors the relevant subset
/// of libgit2's `git_submodule_update_options`.
#[napi(object)]
pub struct SubmoduleUpdateOptions {
    /// When `true`, initialize the submodule's local `.git/config` (and, for `update`, clone it if
    /// not already present) before updating. Corresponds to `git submodule update --init`.
    pub init: Option<bool>,
}

/// A handle to a single submodule of the repository, identified by its name.
///
/// Wraps `gix::Submodule`; each call re-resolves the submodule from the
/// superproject via [`gix::Repository::submodules`], so the handle stays valid
/// even as the superproject's index or configuration changes.
#[napi]
pub struct Submodule {
    repo: std::sync::Arc<gix::ThreadSafeRepository>,
    name: String,
}

impl Submodule {
    /// Resolve the named submodule handle from `repo`, or an error if no such
    /// submodule is configured.
    fn resolve<'a>(repo: &'a gix::Repository, name: &str) -> napi::Result<gix::Submodule<'a>> {
        let Some(subs) = repo.submodules().map_err(|e| err("submodules", e))? else {
            return Err(napi::Error::from_reason(format!(
                "submodule '{name}' not found"
            )));
        };
        subs.into_iter()
            .find(|s| {
                let n: &[u8] = s.name().as_ref();
                n == name.as_bytes()
            })
            .ok_or_else(|| napi::Error::from_reason(format!("submodule '{name}' not found")))
    }
}

#[napi]
impl Submodule {
    /// The submodule's configured name as it appears in `submodule.<name>.*`.
    #[napi]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The path (relative to the superproject worktree) at which the submodule is checked out.
    #[napi]
    pub fn path(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(lossy(
            sm.path().map_err(|e| err("submodule_path", e))?.as_ref(),
        ))
    }

    /// The URL from which to clone or update the submodule.
    #[napi]
    pub fn url(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(Some(
            sm.url().map_err(|e| err("submodule_url", e))?.to_string(),
        ))
    }

    /// The resolved state of the submodule within its superproject.
    #[napi]
    pub fn state(&self) -> napi::Result<SubmoduleState> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let s = sm.state().map_err(|e| err("submodule_state", e))?;
        Ok(SubmoduleState {
            repository_exists: s.repository_exists,
            is_old_form: s.is_old_form,
            worktree_checkout: s.worktree_checkout,
            superproject_configuration: s.superproject_configuration,
        })
    }

    /// The object id recorded for the submodule in `HEAD^{tree}` of the superproject, or `null`.
    #[napi]
    pub fn head_id(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .head_id()
            .map_err(|e| err("submodule_head_id", e))?
            .map(|id| id.to_string()))
    }

    /// The object id recorded for the submodule in the superproject's index, or `null`.
    #[napi]
    pub fn index_id(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .index_id()
            .map_err(|e| err("submodule_index_id", e))?
            .map(|id| id.to_string()))
    }

    /// Whether the submodule is considered active (participates in operations).
    #[napi]
    pub fn is_active(&self) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        sm.is_active().map_err(|e| err("submodule_is_active", e))
    }

    /// The location of the submodule's git directory (`$commondir/modules/<name>`).
    #[napi]
    pub fn git_dir(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .git_dir()
            .map_err(|e| err("submodule_git_dir", e))?
            .to_string_lossy()
            .into_owned())
    }

    /// The location at which the submodule's worktree would be checked out.
    #[napi]
    pub fn work_dir(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .work_dir()
            .map_err(|e| err("submodule_work_dir", e))?
            .to_string_lossy()
            .into_owned())
    }

    /// The `branch` configured for this submodule (override-aware), or `null` if unset.
    #[napi]
    pub fn branch(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .branch()
            .map_err(|e| err("submodule_branch", e))?
            .map(branch_to_string))
    }

    /// The `update` strategy configured for this submodule (override-aware), or `null` if unset.
    #[napi]
    pub fn update_strategy(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .update()
            .map_err(|e| err("submodule_update", e))?
            .map(update_to_string))
    }

    /// The `ignore` setting configured for this submodule (override-aware), or `null` if unset.
    #[napi]
    pub fn ignore_rule(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .ignore()
            .map_err(|e| err("submodule_ignore", e))?
            .map(ignore_to_string))
    }

    /// The `fetchRecurseSubmodules` setting for this submodule (override-aware), or `null` if unset.
    #[napi]
    pub fn fetch_recurse(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .fetch_recurse()
            .map_err(|e| err("submodule_fetch_recurse", e))?
            .map(fetch_recurse_to_string))
    }

    /// The `shallow` flag for this submodule (override-aware), or `null` if unset.
    #[napi]
    pub fn shallow(&self) -> napi::Result<Option<bool>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        sm.shallow().map_err(|e| err("submodule_shallow", e))
    }

    /// The submodule name validated for safe use in paths like `.git/modules/<name>`.
    #[napi]
    pub fn validated_name(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(lossy(
            sm.validated_name()
                .map_err(|e| err("submodule_validated_name", e))?
                .as_ref(),
        ))
    }

    /// The object id of the submodule's currently checked-out `HEAD`, or `null` if the submodule
    /// is not yet initialized (or its `HEAD` is unborn). Equivalent to opening the submodule and
    /// reading its `HEAD`.
    #[napi]
    pub fn workdir_id(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let open = sm.open().map_err(|e| err("submodule_open", e))?;
        Ok(match open {
            Some(repo) => repo.head_id().ok().map(|id| id.to_string()),
            None => None,
        })
    }

    /// Compute the submodule's status: its cheap `state` plus the expected (`index_id`) and actual
    /// (`checked_out_head_id`) commit ids and a `is_dirty` flag. `options` controls which parts to
    /// ignore and whether to short-circuit once the submodule is found dirty.
    #[napi]
    pub fn status(&self, options: Option<SubmoduleStatusOptions>) -> napi::Result<SubmoduleStatus> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let ignore = match options.as_ref().and_then(|o| o.ignore.as_deref()) {
            Some("all") => Ignore::All,
            Some("dirty") => Ignore::Dirty,
            Some("untracked") => Ignore::Untracked,
            _ => Ignore::None,
        };
        let check_dirty = options.and_then(|o| o.check_dirty).unwrap_or(false);
        let st = sm
            .status(ignore, check_dirty)
            .map_err(|e| err("submodule_status", e))?;
        let s = st.state;
        Ok(SubmoduleStatus {
            state: SubmoduleState {
                repository_exists: s.repository_exists,
                is_old_form: s.is_old_form,
                worktree_checkout: s.worktree_checkout,
                superproject_configuration: s.superproject_configuration,
            },
            index_id: st.index_id.map(|id| id.to_string()),
            checked_out_head_id: st.checked_out_head_id.map(|id| id.to_string()),
            is_dirty: st.is_dirty(),
        })
    }

    /// Copy this submodule's configuration from `.gitmodules` into the superproject's local
    /// `.git/config` (the equivalent of `git submodule init`). With `overwrite`, existing local
    /// values are replaced; otherwise they are left untouched. Also marks the submodule active.
    #[napi]
    pub fn init(&self, overwrite: Option<bool>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let overwrite = overwrite.unwrap_or(false);
        let name = &self.name;

        let url = sm.url().map_err(|e| err("submodule_url", e))?.to_string();
        let update = sm
            .update()
            .map_err(|e| err("submodule_update", e))?
            .map(update_to_string);
        let fetch = sm
            .fetch_recurse()
            .map_err(|e| err("submodule_fetch_recurse", e))?
            .map(fetch_recurse_to_string);
        let ignore = sm
            .ignore()
            .map_err(|e| err("submodule_ignore", e))?
            .map(ignore_to_string);
        let branch = sm
            .branch()
            .map_err(|e| err("submodule_branch", e))?
            .map(branch_to_string);

        let snapshot = r.config_snapshot();
        let cfg_path = r.git_dir().join("config");
        let mut tx = r
            .config_file_mut(&cfg_path)
            .map_err(|e| err("config_file_mut", e))?;

        copy_submodule_config(&mut tx, &snapshot, name, "url", Some(url), overwrite)?;
        copy_submodule_config(&mut tx, &snapshot, name, "update", update, overwrite)?;
        copy_submodule_config(
            &mut tx,
            &snapshot,
            name,
            "fetchRecurseSubmodules",
            fetch,
            overwrite,
        )?;
        copy_submodule_config(&mut tx, &snapshot, name, "ignore", ignore, overwrite)?;
        copy_submodule_config(&mut tx, &snapshot, name, "branch", branch, overwrite)?;

        let active_key = format!("submodule.{name}.active");
        if overwrite || snapshot.string(active_key.as_str()).is_none() {
            tx.set_raw_value(active_key.as_str(), "true")
                .map_err(|e| err("submodule_init", e))?;
        }

        tx.commit().map_err(|e| err("config_commit", e))?;
        Ok(())
    }

    /// Synchronize the submodule's `remote.origin.url` with the URL configured in the
    /// superproject (the equivalent of `git submodule sync`). Requires the submodule to be
    /// initialized.
    #[napi]
    pub fn sync(&self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let url = sm.url().map_err(|e| err("submodule_url", e))?.to_string();
        let open = sm.open().map_err(|e| err("submodule_open", e))?;
        let Some(sub_repo) = open else {
            return Err(napi::Error::from_reason(
                "submodule is not initialized; cannot sync",
            ));
        };
        let sub_cfg = sub_repo.git_dir().join("config");
        let mut tx = sub_repo
            .config_file_mut(&sub_cfg)
            .map_err(|e| err("config_file_mut", e))?;
        tx.set_raw_value("remote.origin.url", url.as_str())
            .map_err(|e| err("submodule_sync", e))?;
        tx.commit().map_err(|e| err("config_commit", e))?;
        Ok(())
    }

    /// Record the submodule's currently checked-out `HEAD` as a gitlink entry in the superproject's
    /// index (the gitlink bookkeeping done by `git submodule add`/`update`). Requires the submodule
    /// to be initialized. With `writeIndex` (default `true`), the index is written to disk.
    #[napi(js_name = "addToIndex")]
    pub fn add_to_index(&self, write_index: Option<bool>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let open = sm.open().map_err(|e| err("submodule_open", e))?;
        let Some(sub_repo) = open else {
            return Err(napi::Error::from_reason(
                "submodule is not initialized; cannot record gitlink",
            ));
        };
        let head = sub_repo.head_id().map_err(|e| err("head_id", e))?;
        let path = lossy(&*sm.path().map_err(|e| err("submodule_path", e))?);
        let repo = Repository {
            inner: self.repo.clone(),
        };
        let mut index = repo.index().map_err(|e| err("index", e))?;
        let entry = IndexEntryInput {
            path,
            id: head.to_string(),
            mode: Some("160000".into()),
        };
        repo.add_to_index(&mut index, entry)
            .map_err(|e| err("add_to_index", e))?;
        if write_index.unwrap_or(true) {
            index.write(None).map_err(|e| err("write_index", e))?;
        }
        Ok(())
    }

    /// Stage the `.gitmodules` file into the superproject's index (the final step of
    /// `git submodule add`). Must be called after the `.gitmodules` file exists (e.g. after
    /// `addSubmodule`).
    #[napi(js_name = "addFinalize")]
    pub fn add_finalize(&self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let workdir = r
            .workdir()
            .ok_or_else(|| napi::Error::from_reason("bare repository has no worktree"))?;
        let modules_path = workdir.join(".gitmodules");
        if !modules_path.exists() {
            return Err(napi::Error::from_reason(
                ".gitmodules does not exist; call addSubmodule first",
            ));
        }
        let data = std::fs::read(&modules_path).map_err(|e| err("read_gitmodules", e))?;
        let odb = r.objects.clone().into_arc().map_err(|e| err("odb", e))?;
        let oid = gix::objs::Write::write_buf(&*odb, gix::object::Kind::Blob, &data)
            .map_err(|e| err("write_blob", e))?;
        let repo = Repository {
            inner: self.repo.clone(),
        };
        let mut index = repo.index().map_err(|e| err("index", e))?;
        let entry = IndexEntryInput {
            path: ".gitmodules".into(),
            id: oid.to_string(),
            mode: Some("100644".into()),
        };
        repo.add_to_index(&mut index, entry)
            .map_err(|e| err("add_finalize", e))?;
        index.write(None).map_err(|e| err("write_index", e))?;
        Ok(())
    }

    /// Clone the submodule's repository into the superproject's working directory and check out its
    /// contents (the equivalent of `git submodule update --init`). Requires the submodule to already
    /// be registered in `.gitmodules` (see [`Repository::add_submodule`]). On success the submodule
    /// is laid out at `.git/modules/<name>` (with a `.git` file in the worktree) and the checked-out
    /// commit is recorded as the gitlink in the parent index.
    #[napi]
    pub fn clone(&self, _options: Option<SubmoduleUpdateOptions>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let workdir = r
            .workdir()
            .ok_or_else(|| napi::Error::from_reason("bare repository has no worktree"))?;
        let path = lossy(&*sm.path().map_err(|e| err("submodule_path", e))?);
        let url = sm.url().map_err(|e| err("submodule_url", e))?.to_string();
        let sub_workdir = workdir.join(&path);
        let modules_gitdir = r.git_dir().join("modules").join(&self.name);

        std::fs::create_dir_all(workdir.join(".git").join("modules"))
            .map_err(|e| err("mkdir_modules", e))?;
        std::fs::create_dir_all(&sub_workdir).map_err(|e| err("mkdir_worktree", e))?;

        // Clone the submodule normally into the worktree, then relocate its `.git` into
        // `.git/modules/<name>` and replace it with a gitdir file — git's standard submodule layout.
        git_in_super(&r, &["clone", url.as_str(), path.as_str()])?;
        let src = sub_workdir.join(".git");
        if src.is_dir() {
            std::fs::rename(&src, &modules_gitdir).map_err(|e| err("rename_gitdir", e))?;
        }
        std::fs::write(
            &sub_workdir.join(".git"),
            format!("gitdir: {}\n", modules_gitdir.display()),
        )
        .map_err(|e| err("write_gitdir_file", e))?;

        // Record the checked-out commit as the gitlink in the parent index.
        self.add_to_index(Some(true))?;
        Ok(())
    }

    /// Update the submodule: fetch from its remote and check out the commit recorded in the
    /// superproject's index (the equivalent of `git submodule update`). With `options.init`, clone
    /// it first if it is not yet initialized (`git submodule update --init`).
    #[napi]
    pub fn update(&self, options: Option<SubmoduleUpdateOptions>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let path = lossy(&*sm.path().map_err(|e| err("submodule_path", e))?);
        let init = options.and_then(|o| o.init).unwrap_or(false);
        let mut args: Vec<&str> = vec!["submodule", "update", "--force", path.as_str()];
        if init {
            args.insert(2, "--init");
        }
        git_in_super(&r, &args)?;
        Ok(())
    }

    /// Initialize the submodule's local git repository (the equivalent of
    /// `git_submodule_repo_init`). With `useGitlink` (default `true`) the submodule's git
    /// directory is created under `.git/modules/<name>` and the worktree gains a `.git` file that
    /// points to it, matching git's standard submodule layout. With `false`, a normal `.git`
    /// directory is created directly in the worktree. The submodule's URL is copied from
    /// `.gitmodules` into its local `.git/config`. Does not fetch or check out any content.
    #[napi(js_name = "repoInit")]
    pub fn repo_init(&self, use_gitlink: Option<bool>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let use_gitlink = use_gitlink.unwrap_or(true);
        let workdir = r
            .workdir()
            .ok_or_else(|| napi::Error::from_reason("bare repository has no worktree"))?;
        let path = lossy(&*sm.path().map_err(|e| err("submodule_path", e))?);
        let sub_workdir = workdir.join(&path);

        if use_gitlink {
            let modules_dir = r.git_dir().join("modules");
            let modules_gitdir = modules_dir.join(&self.name);
            std::fs::create_dir_all(&modules_dir).map_err(|e| err("mkdir_modules", e))?;
            std::fs::create_dir_all(&sub_workdir).map_err(|e| err("mkdir_worktree", e))?;
            // Create a normal repo in the worktree, then relocate its `.git` into
            // `.git/modules/<name>` so the standard submodule layout is produced.
            git_in_super(&r, &["init", path.as_str()])?;
            let src = sub_workdir.join(".git");
            if src.is_dir() {
                std::fs::rename(&src, &modules_gitdir).map_err(|e| err("rename_gitdir", e))?;
            }
            std::fs::write(
                &sub_workdir.join(".git"),
                format!("gitdir: {}\n", modules_gitdir.display()),
            )
            .map_err(|e| err("write_gitdir_file", e))?;
        } else {
            std::fs::create_dir_all(&sub_workdir).map_err(|e| err("mkdir_worktree", e))?;
            git_in_super(&r, &["init", path.as_str()])?;
        }

        // Copy the configured URL from `.gitmodules` into the submodule's local `.git/config`.
        git_in_super(&r, &["submodule", "init", path.as_str()])?;
        Ok(())
    }

    /// Open the submodule as its own repository, or `null` if not yet initialized.
    #[napi]
    pub fn open(&self) -> napi::Result<Option<Repository>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        let inner = sm.open().map_err(|e| err("submodule_open", e))?;
        Ok(inner.map(|repo| Repository {
            inner: std::sync::Arc::new(repo.into_sync()),
        }))
    }
}

#[napi]
impl Repository {
    /// Return handles for all configured submodules, or an empty list if none.
    #[napi]
    pub fn submodules(&self) -> napi::Result<Vec<Submodule>> {
        let r = thread_local(&self.inner);
        let Some(subs) = r.submodules().map_err(|e| err("submodules", e))? else {
            return Ok(Vec::new());
        };
        Ok(subs
            .into_iter()
            .map(|s| Submodule {
                repo: self.inner.clone(),
                name: s.name().to_string(),
            })
            .collect())
    }

    /// Return a handle for the named submodule, or `null` if no such submodule is configured.
    #[napi]
    pub fn submodule(&self, name: String) -> napi::Result<Option<Submodule>> {
        let r = thread_local(&self.inner);
        let Some(subs) = r.submodules().map_err(|e| err("submodules", e))? else {
            return Ok(None);
        };
        Ok(subs
            .into_iter()
            .find(|s| {
                let n: &[u8] = s.name().as_ref();
                n == name.as_bytes()
            })
            .map(|s| Submodule {
                repo: self.inner.clone(),
                name: s.name().to_string(),
            }))
    }

    /// Compute [`Submodule::status`] for every configured submodule, returned in the same order as
    /// [`submodules`].
    #[napi]
    pub fn submodule_statuses(&self) -> napi::Result<Vec<SubmoduleStatus>> {
        self.submodules()?.iter().map(|s| s.status(None)).collect()
    }

    /// Register a new submodule in this superproject: write its `path` and `url` into `.gitmodules`
    /// and, if a git repository already exists at `path`, record its `HEAD` as a gitlink entry in
    /// the index. This is the config/index half of `git submodule add`; cloning the submodule is
    /// out of scope for this binding and handled separately. Returns a handle to the new submodule.
    /// `name` defaults to `path`.
    #[napi]
    pub fn add_submodule(
        &self,
        url: String,
        path: String,
        name: Option<String>,
    ) -> napi::Result<Submodule> {
        let r = thread_local(&self.inner);
        let name = name.unwrap_or_else(|| path.clone());
        let workdir = r
            .workdir()
            .ok_or_else(|| napi::Error::from_reason("bare repository has no worktree"))?;
        let modules_path = workdir.join(".gitmodules");
        let mut tx = r
            .config_file_mut(&modules_path)
            .map_err(|e| err("config_file_mut", e))?;
        tx.set_raw_value(&format!("submodule.{name}.path"), path.as_str())
            .map_err(|e| err("set_raw_value", e))?;
        tx.set_raw_value(&format!("submodule.{name}.url"), url.as_str())
            .map_err(|e| err("set_raw_value", e))?;
        tx.commit().map_err(|e| err("config_commit", e))?;

        // If a repository already exists at `path`, record its HEAD as the gitlink.
        let sub_repo_path = workdir.join(&path);
        if sub_repo_path.exists() {
            if let Ok(ts) = gix::ThreadSafeRepository::open(&sub_repo_path) {
                let arc = std::sync::Arc::new(ts);
                let sub_r = thread_local(&arc);
                if let Some(head) = sub_r.head_id().ok().map(|id| id.to_string()) {
                    let repo = Repository {
                        inner: self.inner.clone(),
                    };
                    let mut index = repo.index().map_err(|e| err("index", e))?;
                    let entry = IndexEntryInput {
                        path: path.clone(),
                        id: head,
                        mode: Some("160000".into()),
                    };
                    repo.add_to_index(&mut index, entry)
                        .map_err(|e| err("add_submodule_gitlink", e))?;
                    index.write(None).map_err(|e| err("write_index", e))?;
                }
            }
        }

        Ok(Submodule {
            repo: self.inner.clone(),
            name,
        })
    }
}
