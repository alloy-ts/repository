use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::remote::{Remote, build_remote};
use crate::repository::Repository;

/// Convert a full (`refs/heads/topic`) or short (`topic`) branch name into a `FullName`,
/// normalizing short names to the local-branch category.
fn to_branch_full_name(name: &str) -> Result<gix::refs::FullName> {
    let full = if name.starts_with("refs/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    };
    gix::refs::FullName::try_from(full.as_str()).map_err(|e| err("ref_name", e))
}

/// Parse an optional `fetch`/`push` direction string (default: fetch).
fn parse_direction(direction: Option<&str>) -> gix::remote::Direction {
    match direction {
        Some(d) if d.eq_ignore_ascii_case("push") => gix::remote::Direction::Push,
        _ => gix::remote::Direction::Fetch,
    }
}

/// A branch and the target its ref must still have for a guarded delete.
#[napi(object)]
pub struct BranchDeleteGuard {
    /// Full (`refs/heads/topic`) or short (`topic`) branch name.
    pub name: String,
    /// The object id (or symbolic ref) the branch ref is expected to point at.
    pub target: String,
}

/// The upstream branch and remote a remote-tracking branch maps back to.
#[napi(object)]
pub struct BranchUpstream {
    /// The upstream branch on the remote side (e.g. `refs/heads/main`).
    pub upstream_branch: String,
    /// The name of the remote whose fetch refspecs map to the tracking branch.
    pub remote_name: String,
}

#[napi]
impl Repository {
    /// List configured branch names.
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        Ok(r.branch_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect())
    }

    /// Retrieve the configured remote name for a branch.
    #[napi]
    pub fn branch_remote_name(&self, branch_name: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.branch_remote_name(branch_name.as_str(), gix::remote::Direction::Fetch)
            .map(|n| n.as_bstr().to_string())
    }

    /// Delete the given local branches and remove their local configuration.
    /// Returns the full ref names of the branches actually deleted.
    #[napi]
    pub fn delete_local_branches(&self, names: Vec<String>) -> Result<Vec<String>> {
        let mut r = thread_local(&self.inner);
        let full_names = names
            .iter()
            .map(|n| to_branch_full_name(n))
            .collect::<Result<Vec<_>>>()?;
        r.delete_local_branches(full_names)
            .map(|deleted| deleted.into_iter().map(|n| n.to_string()).collect())
            .map_err(|e| err("delete_local_branches", e))
    }

    /// Like [`delete_local_branches`](Self::delete_local_branches) but each branch is only
    /// deleted if its ref still points at the given target (an object id or symbolic ref).
    #[napi]
    pub fn delete_local_branches_if_unchanged(
        &self,
        branches: Vec<BranchDeleteGuard>,
    ) -> Result<()> {
        let mut r = thread_local(&self.inner);
        let guarded = branches
            .iter()
            .map(|g| {
                let name = to_branch_full_name(&g.name)?;
                let target = match gix::hash::ObjectId::from_hex(g.target.as_bytes()) {
                    Ok(oid) => gix::refs::Target::Object(oid),
                    Err(_) => gix::refs::Target::Symbolic(to_branch_full_name(&g.target)?),
                };
                Ok((name, target))
            })
            .collect::<Result<Vec<_>>>()?;
        r.delete_local_branches_if_unchanged(guarded)
            .map_err(|e| err("delete_local_branches_if_unchanged", e))
    }

    /// Return the remote ref name configured for the branch (e.g. `refs/heads/main` on the
    /// remote side for fetch, or the push refspec mapping for push), or `null` if unconfigured.
    #[napi]
    pub fn branch_remote_ref_name(
        &self,
        branch_name: String,
        direction: Option<String>,
    ) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let name = to_branch_full_name(&branch_name)?;
        match r.branch_remote_ref_name(name.as_ref(), parse_direction(direction.as_deref())) {
            Some(Ok(full)) => Ok(Some(full.to_string())),
            Some(Err(e)) => Err(err("branch_remote_ref_name", e)),
            None => Ok(None),
        }
    }

    /// Return the remote-tracking ref name configured for the branch (e.g.
    /// `refs/remotes/origin/main`), or `null` if unconfigured.
    #[napi]
    pub fn branch_remote_tracking_ref_name(
        &self,
        branch_name: String,
        direction: Option<String>,
    ) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let name = to_branch_full_name(&branch_name)?;
        match r
            .branch_remote_tracking_ref_name(name.as_ref(), parse_direction(direction.as_deref()))
        {
            Some(Ok(full)) => Ok(Some(full.to_string())),
            Some(Err(e)) => Err(err("branch_remote_tracking_ref_name", e)),
            None => Ok(None),
        }
    }

    /// Reverse-map a remote-tracking branch (e.g. `refs/remotes/origin/main`) to the upstream
    /// branch and remote it tracks, or `null` if no unambiguous fetch refspec mapping exists.
    #[napi]
    pub fn upstream_branch_and_remote_for_tracking_branch(
        &self,
        tracking_branch: String,
    ) -> Result<Option<BranchUpstream>> {
        let r = thread_local(&self.inner);
        let full = gix::refs::FullName::try_from(tracking_branch.as_str())
            .map_err(|e| err("ref_name", e))?;
        match r.upstream_branch_and_remote_for_tracking_branch(full.as_ref()) {
            Ok(Some((upstream, remote))) => Ok(Some(BranchUpstream {
                upstream_branch: upstream.to_string(),
                remote_name: remote
                    .name()
                    .map(|n| String::from_utf8_lossy(n.as_bstr()).into_owned())
                    .unwrap_or_default(),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(err("upstream_branch_and_remote_for_tracking_branch", e)),
        }
    }

    /// Return the fully configured remote for the branch, or `null` if unconfigured.
    #[napi]
    pub fn branch_remote(
        &self,
        branch_name: String,
        direction: Option<String>,
    ) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.branch_remote(branch_name.as_str(), parse_direction(direction.as_deref())) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("branch_remote", e)),
            None => Ok(None),
        }
    }
}
