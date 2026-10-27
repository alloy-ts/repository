use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use gix::bstr::BStr;
use gix::refs::transaction::{LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, FullName, Target, TargetRef};

use crate::commit::CommitTime;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::reference::{Reference, build_reference};
use crate::remote::{Remote, build_remote};
use crate::repository::Repository;

/// Convert a full (`refs/heads/topic`) or short (`topic`) branch name into a `FullName`,
/// normalizing short names to the local-branch category.
fn to_branch_full_name(name: &str) -> Result<FullName> {
    branch_full_name(name, Category::LocalBranch)
}

/// Like [`to_branch_full_name`], but for an explicit branch `category`.
///
/// Names already starting with `refs/` are used as-is and must belong to `category`,
/// so `refs/tags/v1` is rejected instead of being appended to the branch prefix.
fn branch_full_name(name: &str, category: Category<'_>) -> napi::Result<FullName> {
    let full = if name.starts_with("refs/") {
        FullName::try_from(name).map_err(|e| err("ref_name", e))?
    } else {
        category
            .to_full_name(BStr::new(name.as_bytes()))
            .map_err(|e| err("ref_name", e))?
    };
    if full.category() != Some(category) {
        return Err(err_str(
            "branch_name",
            format!("'{name}' is not a {}", category_name(category)),
        ));
    }
    Ok(full)
}

fn category_name(category: Category<'_>) -> &'static str {
    match category {
        Category::LocalBranch => "local branch",
        Category::RemoteBranch => "remote branch",
        _ => "branch",
    }
}

/// Parse an optional `local`/`remote` branch type string (default: local).
fn parse_branch_type(branch_type: Option<&str>) -> napi::Result<Category<'static>> {
    Ok(match branch_type {
        None | Some("local") | Some("local_branch") => Category::LocalBranch,
        Some("remote") | Some("remote_branch") => Category::RemoteBranch,
        Some(other) => {
            return Err(err_str(
                "branch_type",
                format!("unknown branch type '{other}'"),
            ));
        }
    })
}

/// Parse an optional `fetch`/`push` direction string (default: fetch).
fn parse_direction(direction: Option<&str>) -> gix::remote::Direction {
    match direction {
        Some(d) if d.eq_ignore_ascii_case("push") => gix::remote::Direction::Push,
        _ => gix::remote::Direction::Fetch,
    }
}

/// The short name (`topic`) of a branch reference, used for `branch.<name>` configuration.
fn short_branch_name(full: &FullName) -> String {
    String::from_utf8_lossy(full.shorten()).into_owned()
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// All values of `section.<subsection>.*`, e.g. every `branch.main.*` key.
fn section_values(
    file: &gix::config::File,
    section: &str,
    subsection: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for candidate in file.sections() {
        let header = candidate.header();
        if header.name() != section
            || header.subsection_name() != Some(BStr::new(subsection.as_bytes()))
        {
            continue;
        }
        for key in candidate.value_names() {
            if let Some(value) = candidate.value(&key) {
                out.push((key.to_string(), lossy(value.as_ref())));
            }
        }
    }
    out
}

/// Remove `keys` from `section.<subsection>.*`, keeping the remaining values.
///
/// `gix` has no per-key removal, so the section is rewritten without the removed keys.
fn remove_section_keys(
    file: &mut gix::config::File,
    section: &str,
    subsection: &str,
    keys: &[&str],
) {
    let values = section_values(file, section, subsection);
    if !values.iter().any(|(key, _)| keys.contains(&key.as_str())) {
        return;
    }
    file.remove_section(section, subsection);
    for (key, value) in values
        .into_iter()
        .filter(|(key, _)| !keys.contains(&key.as_str()))
    {
        let _ = file.set_raw_value_by(section, subsection, key.as_str(), value.as_str());
    }
}

/// Move every value of `section.<old>.*` to `section.<new>.*`, dropping the old section.
fn rename_branch_config(file: &mut gix::config::File, old: &str, new: &str) {
    let values = section_values(file, "branch", old);
    if values.is_empty() {
        return;
    }
    file.remove_section("branch", old);
    for (key, value) in values {
        let _ = file.set_raw_value_by("branch", new, key.as_str(), value.as_str());
    }
}

/// Read the local configuration file from disk.
///
/// Configuration is cached when the repository is opened, so a fresh read is the only
/// way to observe changes another call (or another process) wrote since then.
fn local_config_file(r: &gix::Repository) -> napi::Result<Option<gix::config::File>> {
    let path = r.common_dir().join("config");
    if !path.exists() {
        return Ok(None);
    }
    gix::config::File::from_path_no_includes(path, gix::config::Source::Local)
        .map(Some)
        .map_err(|e| err("local_config", e))
}

/// Whether `branch.<short_name>.remote` is set in the local configuration file on disk.
fn local_config_has_branch_remote(r: &gix::Repository, short_name: &str) -> napi::Result<bool> {
    Ok(match local_config_file(r)? {
        Some(file) => section_values(&file, "branch", short_name)
            .iter()
            .any(|(key, _)| key == "remote"),
        None => false,
    })
}

/// Apply `edit` to the configuration of this repository instance *and* to the local
/// configuration file on disk.
///
/// [`gix::Repository::config_snapshot_mut`] only alters the in-memory view of this one
/// instance, so the same edit is written to `common_dir/config` to survive a reopen.
fn edit_local_config(
    r: &mut gix::Repository,
    edit: impl Fn(&mut gix::config::File),
) -> napi::Result<()> {
    {
        let mut snapshot = r.config_snapshot_mut();
        edit(&mut snapshot);
        let _ = snapshot.commit();
    }
    let path = r.common_dir().join("config");
    if path.exists() {
        let mut file =
            gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
                .map_err(|e| err("local_config", e))?;
        edit(&mut file);
        std::fs::write(&path, file.to_bstring().as_slice())
            .map_err(|e| err("local_config_write", e))?;
    }
    Ok(())
}

/// Resolve the `upstream` argument of [`Repository::set_branch_upstream`].
///
/// Returns the remote to configure — `Some` when `upstream` was given as a
/// remote-tracking branch — together with the remote-side branch ref to merge.
fn resolve_upstream(
    r: &gix::Repository,
    upstream: &str,
) -> napi::Result<(Option<String>, FullName)> {
    let tracking = if upstream.starts_with("refs/remotes/") {
        upstream.to_string()
    } else {
        format!("refs/remotes/{upstream}")
    };
    if let Ok(tracking) = branch_full_name(&tracking, Category::RemoteBranch) {
        if let Ok(Some((merge, remote))) =
            r.upstream_branch_and_remote_for_tracking_branch(tracking.as_ref())
        {
            let name = remote.name().map(|n| lossy(n.as_bstr().as_ref()));
            return Ok((name, merge));
        }
    }
    // Otherwise `upstream` names the branch on the remote side, as git2's
    // `Branch::set_upstream` takes it.
    let merge = if upstream.starts_with("refs/") {
        upstream.to_string()
    } else {
        format!("refs/heads/{upstream}")
    };
    Ok((
        None,
        FullName::try_from(merge.as_str()).map_err(|e| err("ref_name", e))?,
    ))
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
    /// `null` for remotes that only have a URL.
    pub remote_name: Option<String>,
}

/// A branch together with the tip-commit data needed for a `git branch -vv`-style
/// listing — the resolved tip id and time, the current-`HEAD` flag, and the
/// configured upstream tracking ref.
#[napi(object)]
pub struct BranchListItem {
    /// Short branch name (`topic`, or `origin/main` for remote-tracking branches).
    pub name: String,
    /// Full reference name (`refs/heads/topic`, `refs/remotes/origin/main`, …).
    pub full_name: String,
    /// Object id of the branch tip (the commit it points at).
    pub target: String,
    /// Time of the tip commit.
    pub commit_time: CommitTime,
    /// Whether this branch is the one `HEAD` points at.
    pub is_head: bool,
    /// Configured upstream tracking ref (`refs/remotes/origin/main`), or `null`.
    pub upstream: Option<String>,
}

/// Whether `name` is a valid local branch name, i.e. whether `refs/heads/<name>` is a
/// valid reference name. Accepts short names (`topic`); unlike Git, names already
/// carrying the `refs/heads/` prefix are accepted as well.
#[napi]
pub fn branch_name_is_valid(name: String) -> bool {
    Category::LocalBranch
        .to_full_name(BStr::new(name.as_bytes()))
        .is_ok()
}

#[napi]
impl Repository {
    /// List configured branch names.
    ///
    /// Note that this is the set of `branch.<name>` configuration sections, not the set
    /// of branch references: a branch without configuration is not listed. Use
    /// [`local_branches`](Self::local_branches) to enumerate branch references.
    #[napi]
    pub fn branch_names(&self) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        Ok(r.branch_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect())
    }

    /// All local branches (`refs/heads/*`) of the repository.
    #[napi]
    pub fn local_branches(&self) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform
            .local_branches()
            .map_err(|e| err("local_branches", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("local_branches", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// All remote-tracking branches (`refs/remotes/*`) of the repository.
    #[napi]
    pub fn remote_branches(&self) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform
            .remote_branches()
            .map_err(|e| err("remote_branches", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("remote_branches", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// Find a branch by full (`refs/heads/topic`) or short (`topic`) name, restricted to
    /// `branchType` (`"local"` (default) or `"remote"`). Throws if it does not exist or
    /// belongs to another category.
    #[napi]
    pub fn find_branch(
        &self,
        name: String,
        branch_type: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        let full = branch_full_name(&name, parse_branch_type(branch_type.as_deref())?)?;
        let reference = r
            .find_reference(full.to_string().as_str())
            .map_err(|e| err("find_branch", e))?;
        build_reference(&self.inner, reference)
    }

    /// Like [`find_branch`](Self::find_branch), but returns `null` instead of throwing
    /// when the branch does not exist.
    #[napi]
    pub fn try_find_branch(
        &self,
        name: String,
        branch_type: Option<String>,
    ) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let full = branch_full_name(&name, parse_branch_type(branch_type.as_deref())?)?;
        match r
            .try_find_reference(full.to_string().as_str())
            .map_err(|e| err("try_find_branch", e))?
        {
            Some(reference) => Ok(Some(build_reference(&self.inner, reference)?)),
            None => Ok(None),
        }
    }

    /// Create the local branch `name` pointing at `target`, which is either an object id
    /// or a revision such as `HEAD`. Throws when the branch already exists unless `force`
    /// is set.
    #[napi]
    pub fn create_branch(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        let full = to_branch_full_name(&name)?;
        let id = match parse_oid(&target) {
            Ok(id) => id,
            Err(_) => r
                .rev_parse_single(target.as_str())
                .map_err(|e| err("rev_parse_single", e))?
                .into(),
        };
        // `PreviousValue::MustNotExist` only rejects refs whose target differs, so an
        // existing branch pointing at the same commit has to be caught explicitly.
        if !force.unwrap_or(false)
            && r.try_find_reference(full.to_string().as_str())
                .map_err(|e| err("create_branch", e))?
                .is_some()
        {
            return Err(err_str(
                "create_branch",
                format!("branch '{name}' already exists"),
            ));
        }
        let constraint = if force.unwrap_or(false) {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        };
        let reference = r
            .reference(
                full.to_string(),
                id,
                constraint,
                format!("branch: created from {target}"),
            )
            .map_err(|e| err("create_branch", e))?;
        build_reference(&self.inner, reference)
    }

    /// Rename the local branch `oldName` to `newName`.
    ///
    /// The `branch.<name>` configuration section follows the rename, as `git branch -m`
    /// does. Throws when the source does not exist or, without `force`, when the
    /// destination already exists.
    #[napi]
    pub fn rename_branch(
        &self,
        old_name: String,
        new_name: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let mut r = thread_local(&self.inner);
        let old_full = to_branch_full_name(&old_name)?;
        let new_full = to_branch_full_name(&new_name)?;
        let old_short = short_branch_name(&old_full);
        let new_short = short_branch_name(&new_full);

        let target = {
            let source = r
                .find_reference(old_full.to_string().as_str())
                .map_err(|e| err("rename_branch", e))?;
            match source.target() {
                TargetRef::Object(id) => Target::Object(id.to_owned()),
                TargetRef::Symbolic(name) => Target::Symbolic(name.to_owned()),
            }
        };
        // As with creating, an existing destination is only rejected when the caller did
        // not ask for `force`, even if it happens to point at the same commit.
        if !force.unwrap_or(false)
            && r.try_find_reference(new_full.to_string().as_str())
                .map_err(|e| err("rename_branch", e))?
                .is_some()
        {
            return Err(err_str(
                "rename_branch",
                format!("branch '{new_name}' already exists"),
            ));
        }
        let previous = if force.unwrap_or(false) {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        };
        let log = LogChange {
            mode: RefLog::AndReference,
            force_create_reflog: false,
            message: format!("branch: renamed {old_short} to {new_short}").into(),
        };
        let edits = vec![
            RefEdit::update_with_log(new_full.clone(), target, previous, log),
            RefEdit::delete_with_log(old_full, PreviousValue::MustExist, RefLog::AndReference),
        ];
        r.edit_references(edits)
            .map_err(|e| err("rename_branch", e))?;

        // A missing `branch.<old>` section is not an error: renaming an unconfigured
        // branch simply has no configuration to move.
        edit_local_config(&mut r, |file| {
            rename_branch_config(file, &old_short, &new_short);
        })?;

        let reference = r
            .find_reference(new_full.to_string().as_str())
            .map_err(|e| err("rename_branch", e))?;
        build_reference(&self.inner, reference)
    }

    /// Set the upstream of the local branch `branch`, or unset it with `null`.
    ///
    /// `upstream` is either the name of the branch on the remote (`main`,
    /// `refs/heads/main`), which requires `branch.<name>.remote` to be configured, or a
    /// remote-tracking branch (`origin/main`, `refs/remotes/origin/main`), which also
    /// sets the remote. Unsetting removes `branch.<name>.merge` and `branch.<name>.remote`.
    #[napi]
    pub fn set_branch_upstream(
        &self,
        branch: String,
        upstream: Option<String>,
    ) -> napi::Result<()> {
        let mut r = thread_local(&self.inner);
        let full = to_branch_full_name(&branch)?;
        let short = short_branch_name(&full);
        let Some(upstream) = upstream else {
            return edit_local_config(&mut r, |file| {
                remove_section_keys(file, "branch", &short, &["merge", "remote"]);
            });
        };

        let (remote, merge) = resolve_upstream(&r, &upstream)?;
        let merge = merge.to_string();
        // Configuration is cached when the repository is opened, so a remote written
        // moments ago is only visible by reading the local file again.
        if remote.is_none()
            && r.branch_remote_name(short.as_str(), gix::remote::Direction::Fetch)
                .is_none()
            && !local_config_has_branch_remote(&r, &short)?
        {
            return Err(err_str(
                "set_branch_upstream",
                format!(
                    "branch '{branch}' has no remote: configure branch.{short}.remote first, \
                     or pass a remote-tracking branch"
                ),
            ));
        }
        edit_local_config(&mut r, |file| {
            if let Some(remote) = remote.as_deref() {
                let _ = file.set_raw_value_by("branch", short.as_str(), "remote", remote);
            }
            let _ = file.set_raw_value_by("branch", short.as_str(), "merge", merge.as_str());
        })
    }

    /// Whether the local branch `name` is the branch `HEAD` points at.
    #[napi]
    pub fn branch_is_head(&self, name: String) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        let full = to_branch_full_name(&name)?;
        let head = r.head_name().map_err(|e| err("head_name", e))?;
        Ok(head.as_ref().map(|h| h.as_bstr()) == Some(full.as_bstr()))
    }

    /// Retrieve the configured remote name for a branch.
    #[napi]
    pub fn branch_remote_name(
        &self,
        branch_name: String,
        direction: Option<String>,
    ) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let short = short_branch_name(&to_branch_full_name(&branch_name)?);
        Ok(
            r.branch_remote_name(short.as_str(), parse_direction(direction.as_deref()))
                .map(|n| n.as_bstr().to_string()),
        )
    }

    /// Delete the given local branches and remove their local configuration.
    /// Returns the full ref names of the branches actually deleted.
    #[napi]
    pub fn delete_local_branches(&self, names: Vec<String>) -> napi::Result<Vec<String>> {
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
    ) -> napi::Result<()> {
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
    ) -> napi::Result<Option<String>> {
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
    ) -> napi::Result<Option<String>> {
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
    ) -> napi::Result<Option<BranchUpstream>> {
        let r = thread_local(&self.inner);
        let full = gix::refs::FullName::try_from(tracking_branch.as_str())
            .map_err(|e| err("ref_name", e))?;
        match r.upstream_branch_and_remote_for_tracking_branch(full.as_ref()) {
            Ok(Some((upstream, remote))) => Ok(Some(BranchUpstream {
                upstream_branch: upstream.to_string(),
                remote_name: remote.name().map(|n| lossy(n.as_bstr().as_ref())),
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
    ) -> napi::Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        let short = short_branch_name(&to_branch_full_name(&branch_name)?);
        match r.branch_remote(short.as_str(), parse_direction(direction.as_deref())) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("branch_remote", e)),
            None => Ok(None),
        }
    }

    /// List branches (`local` or `remote`, default `local`) as structured items that
    /// bundle the tip commit's id and time, the current-`HEAD` flag, and the configured
    /// upstream — a single-call convenience for a `git branch -vv`-style view.
    ///
    /// Unlike [`local_branches`](Self::local_branches) / [`remote_branches`](Self::remote_branches),
    /// which return raw `Reference` objects, this resolves the tip commit id and time so
    /// callers don't have to `peel_to_commit()` each ref.
    #[napi]
    pub fn list_branches(&self, branch_type: Option<String>) -> napi::Result<Vec<BranchListItem>> {
        let r = thread_local(&self.inner);
        let category = parse_branch_type(branch_type.as_deref())?;
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = match category {
            Category::LocalBranch => platform
                .local_branches()
                .map_err(|e| err("local_branches", e))?,
            Category::RemoteBranch => platform
                .remote_branches()
                .map_err(|e| err("remote_branches", e))?,
            _ => return Err(err_str("branch_type", "unknown branch type")),
        };
        let head = r.head_name().map_err(|e| err("head_name", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("branches", e))?;
            let full = reference.name().to_string();
            let short = String::from_utf8_lossy(reference.name().shorten()).into_owned();
            let commit = reference.peel_to_commit().map_err(|e| err("branches", e))?;
            let time = commit.time().map_err(|e| err("branches", e))?;
            let target = commit.id.to_string();
            let is_head = head.as_ref().map(|h| h.as_bstr()) == Some(reference.name().as_bstr());
            let upstream = match category {
                Category::LocalBranch => match r.branch_remote_tracking_ref_name(
                    reference.name(),
                    gix::remote::Direction::Fetch,
                ) {
                    Some(Ok(name)) => Some(name.to_string()),
                    _ => None,
                },
                Category::RemoteBranch => None,
            };
            out.push(BranchListItem {
                name: short,
                full_name: full,
                target,
                commit_time: CommitTime {
                    seconds: time.seconds,
                    offset: time.offset,
                },
                is_head,
                upstream,
            });
        }
        Ok(out)
    }
}
