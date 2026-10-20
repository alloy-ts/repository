use napi::Result;
use napi_derive::napi;

use gix::bstr::BStr;
use gix::submodule::config::{Branch, FetchRecurse, Ignore, Update};

use crate::error::{err, thread_local};
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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
    pub fn path(&self, name: String) -> Result<String> {
        let name = BStr::new(name.as_str());
        let p = self
            .inner
            .path(name)
            .map_err(|e| err("submodule_path", e))?;
        Ok(lossy(&p))
    }

    /// The URL from which to clone/update the named submodule.
    #[napi]
    pub fn url(&self, name: String) -> Result<String> {
        let name = BStr::new(name.as_str());
        let url = self.inner.url(name).map_err(|e| err("submodule_url", e))?;
        Ok(url.to_string())
    }

    /// The `update` strategy configured for the named submodule, or `null` if unset.
    #[napi]
    pub fn update(&self, name: String) -> Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .update(name)
            .map_err(|e| err("submodule_update", e))?;
        Ok(v.map(update_to_string))
    }

    /// The `branch` configured for the named submodule, or `null` if unset.
    #[napi]
    pub fn branch(&self, name: String) -> Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .branch(name)
            .map_err(|e| err("submodule_branch", e))?;
        Ok(v.map(branch_to_string))
    }

    /// The `fetchRecurseSubmodules` setting for the named submodule, or `null` if unset.
    #[napi]
    pub fn fetch_recurse(&self, name: String) -> Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .fetch_recurse(name)
            .map_err(|e| err("submodule_fetch_recurse", e))?;
        Ok(v.map(fetch_recurse_to_string))
    }

    /// The `ignore` setting for the named submodule, or `null` if unset.
    #[napi]
    pub fn ignore(&self, name: String) -> Result<Option<String>> {
        let name = BStr::new(name.as_str());
        let v = self
            .inner
            .ignore(name)
            .map_err(|e| err("submodule_ignore", e))?;
        Ok(v.map(ignore_to_string))
    }

    /// The `shallow` flag for the named submodule, or `null` if unset.
    #[napi]
    pub fn shallow(&self, name: String) -> Result<Option<bool>> {
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
    pub fn open_modules_file(&self) -> Result<Option<ModulesFile>> {
        let r = thread_local(&self.inner);
        let file = r
            .open_modules_file()
            .map_err(|e| err("open_modules_file", e))?;
        Ok(file.map(|f| ModulesFile { inner: f }))
    }

    /// Return the (auto-updating) `.gitmodules` file, or `null` if it is absent.
    #[napi]
    pub fn modules(&self) -> Result<Option<ModulesFile>> {
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
    fn resolve<'a>(repo: &'a gix::Repository, name: &str) -> Result<gix::Submodule<'a>> {
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
    pub fn path(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(lossy(&sm.path().map_err(|e| err("submodule_path", e))?))
    }

    /// The URL from which to clone or update the submodule.
    #[napi]
    pub fn url(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(Some(
            sm.url().map_err(|e| err("submodule_url", e))?.to_string(),
        ))
    }

    /// The resolved state of the submodule within its superproject.
    #[napi]
    pub fn state(&self) -> Result<SubmoduleState> {
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
    pub fn head_id(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .head_id()
            .map_err(|e| err("submodule_head_id", e))?
            .map(|id| id.to_string()))
    }

    /// The object id recorded for the submodule in the superproject's index, or `null`.
    #[napi]
    pub fn index_id(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .index_id()
            .map_err(|e| err("submodule_index_id", e))?
            .map(|id| id.to_string()))
    }

    /// Whether the submodule is considered active (participates in operations).
    #[napi]
    pub fn is_active(&self) -> Result<bool> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        sm.is_active().map_err(|e| err("submodule_is_active", e))
    }

    /// The location of the submodule's git directory (`$commondir/modules/<name>`).
    #[napi]
    pub fn git_dir(&self) -> Result<String> {
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
    pub fn work_dir(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let sm = Self::resolve(&r, &self.name)?;
        Ok(sm
            .work_dir()
            .map_err(|e| err("submodule_work_dir", e))?
            .to_string_lossy()
            .into_owned())
    }

    /// Open the submodule as its own repository, or `null` if not yet initialized.
    #[napi]
    pub fn open(&self) -> Result<Option<Repository>> {
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
    pub fn submodules(&self) -> Result<Vec<Submodule>> {
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
    pub fn submodule(&self, name: String) -> Result<Option<Submodule>> {
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
}
