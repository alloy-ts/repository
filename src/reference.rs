use std::sync::Arc;

use napi_derive::napi;

use gix::config::AsBStr;
use gix::refs::TargetRef;

use crate::blob::{Blob, build_blob};
use crate::commit::{Commit, build_commit};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto};
use crate::object::{Object, build_object, string_to_kind};
use crate::repository::Repository;
use crate::tag::{Tag, build_tag};
use crate::tree::{Tree, build_tree};

/// A git reference (branch, tag, or HEAD) exposed to TypeScript.
#[napi]
pub struct Reference {
    repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
    target: String,
    symbolic_target: Option<String>,
    /// The packed-refs peel target (`git_reference_target_peel`), populated only
    /// for references that originated from `packed-refs`; `null` otherwise.
    target_peel: Option<String>,
}

#[napi]
impl Reference {
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The object id this reference points at directly (empty for symbolic refs).
    #[napi(getter)]
    pub fn target(&self) -> String {
        self.target.clone()
    }

    /// The kind of reference based on its target: `"object"` or `"symbolic"`.
    #[napi(getter)]
    pub fn kind(&self) -> String {
        if self.symbolic_target.is_some() {
            "symbolic".into()
        } else {
            "object".into()
        }
    }

    /// The name of the reference this symbolic reference points to, or `null`
    /// if it points directly at an object.
    #[napi(getter)]
    pub fn symbolic_target(&self) -> Option<String> {
        self.symbolic_target.clone()
    }

    /// The packed-refs peel target of a direct reference that points at a tag
    /// object (`git_reference_target_peel`). Only populated for references that
    /// originated from `packed-refs`; `null` for loose references and for
    /// symbolic references.
    #[napi(getter)]
    pub fn target_peel(&self) -> Option<String> {
        self.target_peel.clone()
    }

    /// The reference category derived from the name prefix, e.g. `tag`, `local_branch`,
    /// `remote_branch`, `note`, `pseudo_ref`, `main_ref`, `linked_ref`, `bisect`,
    /// `rewritten`, or `worktree_private`; `null` when the name has no known category.
    #[napi]
    pub fn category(&self) -> Option<String> {
        category_enum(&self.name).map(category_to_string)
    }

    /// Whether this reference is a local branch (`refs/heads/...`).
    #[napi]
    pub fn is_branch(&self) -> bool {
        matches!(category_enum(&self.name), Some(RefCategory::LocalBranch))
    }

    /// Whether this reference points at another reference (rather than directly at an object).
    #[napi]
    pub fn is_symbolic(&self) -> bool {
        self.symbolic_target.is_some()
    }

    /// Whether this reference is a remote-tracking branch (`refs/remotes/...`).
    #[napi]
    pub fn is_remote(&self) -> bool {
        matches!(category_enum(&self.name), Some(RefCategory::RemoteBranch))
    }

    /// Whether this reference is a tag (`refs/tags/...`).
    #[napi]
    pub fn is_tag(&self) -> bool {
        matches!(category_enum(&self.name), Some(RefCategory::Tag))
    }

    /// Whether this reference is a note (`refs/notes/...`).
    #[napi]
    pub fn is_note(&self) -> bool {
        matches!(category_enum(&self.name), Some(RefCategory::Note))
    }

    /// Whether this reference is a pseudo ref (`HEAD`, `FETCH_HEAD`, ...).
    #[napi]
    pub fn is_pseudo_ref(&self) -> bool {
        matches!(
            category_enum(&self.name),
            Some(RefCategory::PseudoRef) | Some(RefCategory::MainPseudoRef)
        )
    }

    /// Like [`category()`](Self::category), but also returns the shortened name with
    /// the category prefix stripped; `null` when the name has no known category.
    #[napi(ts_return_type = "RefCategoryAndShortName | null")]
    pub fn category_and_short_name(&self) -> Option<RefCategoryAndShortName> {
        let full_name = gix::refs::FullName::try_from(self.name.as_str()).ok()?;
        full_name
            .category_and_short_name()
            .map(|(category, short_name)| RefCategoryAndShortName {
                category: category_to_string(ref_category_from(category)),
                short_name: String::from_utf8_lossy(short_name).into_owned(),
            })
    }

    /// The name with well-known category prefixes stripped, e.g.
    /// `refs/heads/main` → `main`. Names without a known prefix are returned as-is.
    #[napi]
    pub fn shorten(&self) -> napi::Result<String> {
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        Ok(String::from_utf8_lossy(full_name.shorten()).into_owned())
    }

    /// The file name portion of the full name, e.g. `main` for `refs/heads/main`.
    #[napi]
    pub fn file_name(&self) -> napi::Result<String> {
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        Ok(String::from_utf8_lossy(full_name.as_ref().file_name()).into_owned())
    }

    /// Follow all symbolic targets and peel the underlying object to the end of the
    /// tag chain, returning the first non-tag object id this reference ultimately
    /// points to. Throws when a symbolic chain loops or an object is missing.
    #[napi]
    pub fn peel_to_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let id = reference.peel_to_id().map_err(|e| err("peel_to_id", e))?;
        Ok(id.to_string())
    }

    /// Follow this symbolic reference one level and return the reference it points
    /// to, or `null` if this reference points directly at an object.
    #[napi]
    pub fn follow(&self) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.repo);
        let reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        match reference.follow() {
            Some(next) => {
                let next = next.map_err(|e| err("follow", e))?;
                Ok(Some(build_reference(&self.repo, next)?))
            }
            None => Ok(None),
        }
    }

    /// Follow all symbolic references until a direct reference is reached,
    /// returning that reference. Throws when the symbolic chain loops or a
    /// referent is missing.
    #[napi]
    pub fn resolve(&self) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let mut name = self.name.clone();
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(name.clone()) {
                return Err(err_str(
                    "resolve",
                    format!("symbolic reference cycle detected at '{name}'"),
                ));
            }
            let reference = r
                .find_reference(name.as_str())
                .map_err(|e| err("resolve", e))?;
            match reference.target() {
                TargetRef::Object(_) => return build_reference(&self.repo, reference),
                TargetRef::Symbolic(next) => name = next.to_string(),
            }
        }
    }

    /// Peel this reference to an object of the given kind (`"commit"`, `"tree"`,
    /// `"blob"`, or `"tag"`), following symbolic references and annotated tags,
    /// and return the resulting object. Throws when the reference cannot be
    /// peeled to that kind.
    #[napi]
    pub fn peel_to_kind(&self, kind: String) -> napi::Result<Object> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let kind = string_to_kind(&kind)?;
        let obj = reference
            .peel_to_kind(kind)
            .map_err(|e| err("peel_to_kind", e))?;
        build_object(&self.repo, obj.id)
    }

    /// Peel this reference to a commit.
    #[napi]
    pub fn peel_to_commit(&self) -> napi::Result<Commit> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let commit = reference
            .peel_to_commit()
            .map_err(|e| err("peel_to_commit", e))?;
        build_commit(&self.repo, commit)
    }

    /// Peel this reference to a tree.
    #[napi]
    pub fn peel_to_tree(&self) -> napi::Result<Tree> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let tree = reference
            .peel_to_tree()
            .map_err(|e| err("peel_to_tree", e))?;
        build_tree(&self.repo, tree.id)
    }

    /// Peel this reference to a blob.
    #[napi]
    pub fn peel_to_blob(&self) -> napi::Result<Blob> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let blob = reference
            .peel_to_blob()
            .map_err(|e| err("peel_to_blob", e))?;
        Ok(build_blob(&self.repo, blob.id))
    }

    /// Peel this reference to a tag.
    #[napi]
    pub fn peel_to_tag(&self) -> napi::Result<Tag> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let tag = reference.peel_to_tag().map_err(|e| err("peel_to_tag", e))?;
        build_tag(&self.repo, tag)
    }

    /// Re-point this direct reference at `id`, writing a reflog entry. By default
    /// the reference must already exist; pass `expected` to additionally require
    /// its current target to match (a 40-hex object id or symbolic name) before
    /// updating, for a compare-and-swap.
    #[napi]
    pub fn set_target(
        &self,
        id: String,
        message: Option<String>,
        expected: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let oid = parse_oid(&id)?;
        let previous = match expected {
            Some(e) => {
                gix::refs::transaction::PreviousValue::MustExistAndMatch(parse_ref_target(&e)?)
            }
            None => gix::refs::transaction::PreviousValue::MustExist,
        };
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: message.unwrap_or_default().into(),
        };
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            full_name,
            gix::refs::Target::Object(oid),
            previous,
            log,
        );
        r.edit_reference(edit).map_err(|e| err("set_target", e))?;
        build_reference(
            &self.repo,
            r.find_reference(self.name.as_str())
                .map_err(|e| err("find_reference", e))?,
        )
    }

    /// Re-point this symbolic reference at `target`, writing a reflog entry. The
    /// reference must already exist.
    #[napi]
    pub fn set_symbolic_target(
        &self,
        target: String,
        message: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        let symbolic =
            gix::refs::FullName::try_from(target.as_str()).map_err(|e| err("ref_target", e))?;
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: message.unwrap_or_default().into(),
        };
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            full_name,
            gix::refs::Target::Symbolic(symbolic),
            gix::refs::transaction::PreviousValue::MustExist,
            log,
        );
        r.edit_reference(edit)
            .map_err(|e| err("set_symbolic_target", e))?;
        build_reference(
            &self.repo,
            r.find_reference(self.name.as_str())
                .map_err(|e| err("find_reference", e))?,
        )
    }

    /// Rename this reference to `newName`. By default the destination must not
    /// exist; pass `force` to overwrite it. This is implemented as a
    /// create-at-destination followed by a delete-at-source in a single
    /// transaction; unlike libgit2 it does not move the reflog, so the new ref
    /// starts without history.
    #[napi]
    pub fn rename(
        &self,
        new_name: String,
        force: Option<bool>,
        message: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let target = match reference.target() {
            TargetRef::Object(id) => gix::refs::Target::Object(id.to_owned()),
            TargetRef::Symbolic(name) => gix::refs::Target::Symbolic(name.to_owned()),
        };
        let new_full =
            gix::refs::FullName::try_from(new_name.as_str()).map_err(|e| err("rename", e))?;
        if !force.unwrap_or(false)
            && r.try_find_reference(new_name.as_str())
                .map_err(|e| err("rename", e))?
                .is_some()
        {
            return Err(err_str(
                "rename",
                format!("reference '{new_name}' already exists"),
            ));
        }
        let new_previous = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: message.unwrap_or_default().into(),
        };
        let create =
            gix::refs::transaction::RefEdit::update_with_log(new_full, target, new_previous, log);
        let old_full =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("rename", e))?;
        let delete = gix::refs::transaction::RefEdit::delete_with_log(
            old_full,
            gix::refs::transaction::PreviousValue::MustExist,
            gix::refs::transaction::RefLog::AndReference,
        );
        r.edit_references([create, delete])
            .map_err(|e| err("rename", e))?;
        build_reference(
            &self.repo,
            r.find_reference(new_name.as_str())
                .map_err(|e| err("find_reference", e))?,
        )
    }

    /// Re-resolve this reference by name and return a fresh snapshot, or `null`
    /// if it no longer exists. Useful after external changes.
    #[napi]
    pub fn reload(&self) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.repo);
        match r.try_find_reference(self.name.as_str()) {
            Ok(Some(reference)) => Ok(Some(build_reference(&self.repo, reference)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(err("reload", e)),
        }
    }

    /// Delete this reference.
    #[napi]
    pub fn delete(&self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        reference.delete().map_err(|e| err("delete", e))?;
        Ok(())
    }
}

/// An owned, lifetime-free view of a reference category (mirrors
/// [`gix::refs::Category`] but without the embedded name borrow).
#[derive(PartialEq, Clone, Copy)]
enum RefCategory {
    Tag,
    LocalBranch,
    RemoteBranch,
    Note,
    PseudoRef,
    MainPseudoRef,
    MainRef,
    LinkedPseudoRef,
    LinkedRef,
    Bisect,
    Rewritten,
    WorktreePrivate,
}

fn category_enum(name: &str) -> Option<RefCategory> {
    use gix::refs::Category;
    let full = gix::refs::FullName::try_from(name).ok()?;
    Some(match full.category()? {
        Category::Tag => RefCategory::Tag,
        Category::LocalBranch => RefCategory::LocalBranch,
        Category::RemoteBranch => RefCategory::RemoteBranch,
        Category::Note => RefCategory::Note,
        Category::PseudoRef => RefCategory::PseudoRef,
        Category::MainPseudoRef => RefCategory::MainPseudoRef,
        Category::MainRef => RefCategory::MainRef,
        Category::LinkedPseudoRef { .. } => RefCategory::LinkedPseudoRef,
        Category::LinkedRef { .. } => RefCategory::LinkedRef,
        Category::Bisect => RefCategory::Bisect,
        Category::Rewritten => RefCategory::Rewritten,
        Category::WorktreePrivate => RefCategory::WorktreePrivate,
    })
}

fn ref_category_from(category: gix::refs::Category<'_>) -> RefCategory {
    match category {
        gix::refs::Category::Tag => RefCategory::Tag,
        gix::refs::Category::LocalBranch => RefCategory::LocalBranch,
        gix::refs::Category::RemoteBranch => RefCategory::RemoteBranch,
        gix::refs::Category::Note => RefCategory::Note,
        gix::refs::Category::PseudoRef => RefCategory::PseudoRef,
        gix::refs::Category::MainPseudoRef => RefCategory::MainPseudoRef,
        gix::refs::Category::MainRef => RefCategory::MainRef,
        gix::refs::Category::LinkedPseudoRef { .. } => RefCategory::LinkedPseudoRef,
        gix::refs::Category::LinkedRef { .. } => RefCategory::LinkedRef,
        gix::refs::Category::Bisect => RefCategory::Bisect,
        gix::refs::Category::Rewritten => RefCategory::Rewritten,
        gix::refs::Category::WorktreePrivate => RefCategory::WorktreePrivate,
    }
}

fn category_to_string(category: RefCategory) -> String {
    match category {
        RefCategory::Tag => "tag",
        RefCategory::LocalBranch => "local_branch",
        RefCategory::RemoteBranch => "remote_branch",
        RefCategory::Note => "note",
        RefCategory::PseudoRef => "pseudo_ref",
        RefCategory::MainPseudoRef => "main_pseudo_ref",
        RefCategory::MainRef => "main_ref",
        RefCategory::LinkedPseudoRef => "linked_pseudo_ref",
        RefCategory::LinkedRef => "linked_ref",
        RefCategory::Bisect => "bisect",
        RefCategory::Rewritten => "rewritten",
        RefCategory::WorktreePrivate => "worktree_private",
    }
    .into()
}

/// A reference category together with the name shortened by its category prefix.
#[napi(object)]
pub struct RefCategoryAndShortName {
    /// The category, e.g. `tag`, `local_branch`, `remote_branch`, `pseudo_ref`.
    pub category: String,
    /// The name with the category prefix stripped, e.g. `main` for
    /// `refs/heads/main`.
    pub short_name: String,
}

fn category_from_string(category: &str) -> napi::Result<gix::refs::Category<'static>> {
    use gix::refs::Category;
    Ok(match category {
        "tag" => Category::Tag,
        "local_branch" => Category::LocalBranch,
        "remote_branch" => Category::RemoteBranch,
        "note" => Category::Note,
        "pseudo_ref" => Category::PseudoRef,
        "main_pseudo_ref" => Category::MainPseudoRef,
        "main_ref" => Category::MainRef,
        "bisect" => Category::Bisect,
        "rewritten" => Category::Rewritten,
        "worktree_private" => Category::WorktreePrivate,
        // `LinkedPseudoRef`/`LinkedRef` need a worktree name, which the string
        // form cannot carry.
        other => {
            return Err(err_str(
                "category",
                format!("unknown or unsupported category '{other}'"),
            ));
        }
    })
}

/// Whether `name` is a valid fully qualified reference name (e.g. `refs/heads/main`),
/// validated with the same rules git applies when creating references.
#[napi]
pub fn ref_name_is_valid(name: String) -> bool {
    <&gix::refs::FullNameRef>::try_from(name.as_str()).is_ok()
}

/// Whether `name` is a valid partial (possibly un-prefixed) reference name, e.g.
/// `main` or `heads/main`. Notably, names must not contain the `@{` sequence,
/// end with `.lock`, or contain `..`.
#[napi]
pub fn ref_partial_name_is_valid(name: String) -> bool {
    <&gix::refs::PartialNameRef>::try_from(name.as_str()).is_ok()
}

/// Whether `name` is a valid local-branch reference name. Like
/// [`refNameIsValid`](refNameIsValid) but additionally rejects `refs/heads/HEAD`,
/// matching git's branch-specific validation.
#[napi]
pub fn ref_branch_name_is_valid(name: String) -> bool {
    gix::validate::reference::branch_name(gix::bstr::BStr::new(name.as_bytes())).is_ok()
}

/// Sanitise `name` into a valid partial reference name, replacing illegal
/// characters and sequences (this is the closest gix equivalent to libgit2's
/// `git_reference_normalise_name`; it does not support the `ReferenceFormat`
/// flags). Empty or fully-stripped input yields `-`.
#[napi]
pub fn ref_name_sanitise(name: String) -> String {
    let sanitised =
        gix::validate::reference::name_partial_or_sanitize(gix::bstr::BStr::new(name.as_bytes()));
    String::from_utf8_lossy(sanitised.as_ref()).into_owned()
}

/// Options controlling [`ref_name_normalise`].
#[derive(Default)]
#[napi(object)]
pub struct RefNameNormaliseOptions {
    /// Allow one-level (unprefixed) names such as `HEAD` or `ORIG_HEAD`.
    pub allow_one_level: Option<bool>,
    /// Also allow shorthand branch names (`main`) together with `allowOneLevel`.
    pub refspec_shorthand: Option<bool>,
    /// Allow a single `*` as a full path component (e.g. `refs/heads/*`).
    pub refspec_pattern: Option<bool>,
}

/// Normalise `name` into a valid reference name, mirroring git2's
/// `git_reference_normalise_name` as closely as `gix` permits: collapse runs of
/// adjacent slashes, then validate as a full name (default), a partial/one-level
/// name (`allowOneLevel` / `refspecShorthand`), or a refspec pattern
/// (`refspecPattern`). Throws when the result is not a valid name.
///
/// Unlike git2 this is a best-effort wrapper around `gix`'s validators and does
/// not implement the precise flag matrix byte-for-byte.
#[napi]
pub fn ref_name_normalise(
    name: String,
    opts: Option<RefNameNormaliseOptions>,
) -> napi::Result<String> {
    // 1. Collapse runs of adjacent '/' into a single '/' and strip leading/trailing '/'.
    let mut collapsed = String::with_capacity(name.len());
    let mut last_slash = false;
    for c in name.chars() {
        if c == '/' {
            if !last_slash {
                collapsed.push('/');
            }
            last_slash = true;
        } else {
            collapsed.push(c);
            last_slash = false;
        }
    }
    let collapsed = collapsed.trim_matches('/').to_string();

    let opts = opts.unwrap_or_default();
    let allow_one_level = opts.allow_one_level.unwrap_or(false);
    let refspec_shorthand = opts.refspec_shorthand.unwrap_or(false);
    let refspec_pattern = opts.refspec_pattern.unwrap_or(false);

    // 2. Validate. A `*` is only legal as a full component in patterns, so substitute a
    //    placeholder for validation purposes (at most one `*` allowed).
    let validation_target = if refspec_pattern {
        if collapsed.chars().filter(|&c| c == '*').count() > 1 {
            return Err(err_str(
                "ref_name_normalise",
                "a refspec pattern may contain at most one '*'",
            ));
        }
        collapsed.replace('*', "x")
    } else {
        collapsed.clone()
    };

    if refspec_pattern || allow_one_level || refspec_shorthand {
        <&gix::refs::PartialNameRef>::try_from(validation_target.as_str())
            .map_err(|e| err("ref_name_normalise", e))?;
    } else {
        gix::refs::FullName::try_from(validation_target.as_str())
            .map_err(|e| err("ref_name_normalise", e))?;
    }

    Ok(collapsed)
}

/// Build a fully qualified reference name from a `category` (as returned by
/// [`Reference.category()`](Reference::category)) and a `shortName`, e.g.
/// `local_branch` + `main` → `refs/heads/main`.
///
/// If `shortName` already carries the category prefix, it is used as-is. The
/// linked-worktree categories (`linked_pseudo_ref`, `linked_ref`) are not
/// supported as they require a worktree name.
#[napi]
pub fn ref_category_full_name(category: String, short_name: String) -> napi::Result<String> {
    let category = category_from_string(&category)?;
    let full_name = category
        .to_full_name(gix::bstr::BStr::new(short_name.as_bytes()))
        .map_err(|e| err("to_full_name", e))?;
    Ok(full_name.to_string())
}

pub fn build_reference(
    repo: &Arc<gix::ThreadSafeRepository>,
    reference: gix::Reference,
) -> napi::Result<Reference> {
    let detached = reference.detach();
    let name = detached.name.to_string();
    let (target, symbolic_target) = match &detached.target {
        gix::refs::Target::Object(id) => (id.to_string(), None),
        gix::refs::Target::Symbolic(name) => (String::new(), Some(name.to_string())),
    };
    let target_peel = detached.peeled.as_ref().map(|id| id.to_string());
    Ok(Reference {
        repo: Arc::clone(repo),
        name,
        target,
        symbolic_target,
        target_peel,
    })
}

/// Parse `s` as a reference target: a 40-hex object id, else a symbolic name.
fn parse_ref_target(s: &str) -> napi::Result<gix::refs::Target> {
    match parse_oid(s) {
        Ok(oid) => Ok(gix::refs::Target::Object(oid)),
        Err(_) => gix::refs::FullName::try_from(s)
            .map(gix::refs::Target::Symbolic)
            .map_err(|e| err("ref_target", e)),
    }
}

/// A single reference edit, mirroring `gix::refs::RefEdit` for use from TypeScript.
///
/// `change` selects between an update (default) and a deletion. `previousValue`
/// constrains the current state of the reference before the edit applies; when
/// omitted, `force` chooses `any` (true) or `must_not_exist` (false) for updates,
/// and `must_exist` for deletions.
#[derive(Clone)]
#[napi(object)]
pub struct RefEditInput {
    /// The full reference name, e.g. `refs/heads/branch`.
    pub name: String,
    /// The new target for updates: a 40-hex object id, or a symbolic reference name
    /// (e.g. `refs/heads/main`, `HEAD`). Required for updates, ignored for deletions.
    pub new_target: Option<String>,
    /// `"update"` (default) or `"delete"`.
    pub change: Option<String>,
    /// Constrain the current value of the reference: `any`, `must_exist`,
    /// `must_not_exist`, `must_exist_and_match`, or `existing_must_match`. The two
    /// `*and_match` variants require `expectedTarget`. Overrides `force`.
    pub previous_value: Option<String>,
    /// The expected current target for `must_exist_and_match` / `existing_must_match`:
    /// a 40-hex object id or a symbolic reference name.
    pub expected_target: Option<String>,
    /// When `true`, symbolic references are dereferenced so the change applies to
    /// their referent (one level per round, transitively).
    pub deref: Option<bool>,
    /// How to treat the reflog: `and_reference` (default) or `only` (deletions).
    pub ref_log: Option<String>,
    /// Create the reflog even when general rules would prohibit it.
    pub force_create_reflog: Option<bool>,
    /// Convenience alias: `true` maps `previousValue` to `any`, `false` (default) to
    /// `must_not_exist` (updates) / `must_exist` (deletions). Ignored when
    /// `previousValue` is set.
    pub force: Option<bool>,
    /// Optional reflog message.
    pub log_message: Option<String>,
}

/// Parse `expectedTarget` into a `gix::refs::Target` (object id or symbolic name).
fn parse_expected_target(target: &str) -> napi::Result<gix::refs::Target> {
    match parse_oid(target) {
        Ok(oid) => Ok(gix::refs::Target::Object(oid)),
        Err(_) => gix::refs::FullName::try_from(target)
            .map(gix::refs::Target::Symbolic)
            .map_err(|e| err("expected_target", e)),
    }
}

/// Resolve `previousValue` (with `force` fallback) to a `PreviousValue`.
fn build_previous_value(
    input: &RefEditInput,
    is_delete: bool,
) -> napi::Result<gix::refs::transaction::PreviousValue> {
    use gix::refs::transaction::PreviousValue;
    let fallback = if input.force.unwrap_or(false) {
        PreviousValue::Any
    } else if is_delete {
        // `MustNotExist` is invalid for deletions.
        PreviousValue::MustExist
    } else {
        PreviousValue::MustNotExist
    };
    match input.previous_value.as_deref() {
        None => Ok(fallback),
        Some("any") => Ok(PreviousValue::Any),
        Some("must_exist") => Ok(PreviousValue::MustExist),
        Some("must_not_exist") if !is_delete => Ok(PreviousValue::MustNotExist),
        Some("must_not_exist") => Err(err_str(
            "previous_value",
            "must_not_exist is invalid for deletions",
        )),
        Some("must_exist_and_match") => Ok(PreviousValue::MustExistAndMatch(
            parse_expected_target(input.expected_target.as_deref().ok_or_else(|| {
                err_str(
                    "previous_value",
                    "must_exist_and_match requires expectedTarget",
                )
            })?)?,
        )),
        Some("existing_must_match") => Ok(PreviousValue::ExistingMustMatch(parse_expected_target(
            input.expected_target.as_deref().ok_or_else(|| {
                err_str(
                    "previous_value",
                    "existing_must_match requires expectedTarget",
                )
            })?,
        )?)),
        Some(other) => Err(err_str(
            "previous_value",
            format!("unknown previousValue '{other}'"),
        )),
    }
}

/// Build a `gix::refs::RefEdit` from the input DTO.
fn build_ref_edit(input: &RefEditInput) -> napi::Result<gix::refs::transaction::RefEdit> {
    use gix::refs::{Target, transaction::LogChange, transaction::RefLog};
    let name =
        gix::refs::FullName::try_from(input.name.as_str()).map_err(|e| err("ref_name", e))?;
    let deref = input.deref.unwrap_or(false);
    let ref_log = match input.ref_log.as_deref() {
        None | Some("and_reference") => RefLog::AndReference,
        Some("only") => RefLog::Only,
        Some(other) => return Err(err_str("ref_log", format!("unknown refLog '{other}'"))),
    };

    let edit = match input.change.as_deref().unwrap_or("update") {
        "update" => {
            let expected = build_previous_value(input, false)?;
            let new_target = input
                .new_target
                .as_deref()
                .ok_or_else(|| err_str("new_target", "update edits require newTarget"))?;
            let target = match parse_oid(new_target) {
                Ok(oid) => Target::Object(oid),
                Err(_) => Target::Symbolic(
                    gix::refs::FullName::try_from(new_target).map_err(|e| err("ref_target", e))?,
                ),
            };
            gix::refs::transaction::RefEdit::update_with_log(
                name,
                target,
                expected,
                LogChange {
                    mode: ref_log,
                    force_create_reflog: input.force_create_reflog.unwrap_or(false),
                    message: input.log_message.clone().unwrap_or_default().into(),
                },
            )
        }
        "delete" => {
            let expected = build_previous_value(input, true)?;
            gix::refs::transaction::RefEdit::delete_with_log(name, expected, ref_log)
        }
        other => return Err(err_str("change", format!("unknown change '{other}'"))),
    };
    Ok(edit.with_deref(deref))
}

#[napi]
impl Repository {
    /// Point HEAD at a commit id (detached HEAD) or at a branch/ref name (symbolic HEAD).
    ///
    /// `target` is interpreted as a 40-hex object id when it parses as one; otherwise it is
    /// treated as a reference name. A fully-qualified name (`refs/heads/main`) is used directly,
    /// while a short name (`main`) is DWIM-resolved to its full name — mirroring git2's
    /// `git_repository_set_head`.
    #[napi]
    pub fn set_head(&self, target: String) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let ref_target = if let Ok(oid) = parse_oid(&target) {
            gix::refs::Target::Object(oid)
        } else {
            // symbolic: accept a fully-qualified name, or DWIM a short name.
            let sym = match gix::refs::FullName::try_from(target.as_str()) {
                Ok(f) => f,
                Err(_) => {
                    let resolved = r
                        .find_reference(target.as_str())
                        .map_err(|e| err("set_head", e))?;
                    gix::refs::FullName::try_from(resolved.name())
                        .map_err(|e| err("set_head", e))?
                }
            };
            gix::refs::Target::Symbolic(sym)
        };
        let label = format!("set_head {target}");
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: label.into(),
        };
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            gix::refs::FullName::try_from("HEAD").unwrap(),
            ref_target,
            gix::refs::transaction::PreviousValue::Any,
            log,
        );
        r.edit_reference(edit).map_err(|e| err("set_head", e))?;
        Ok(())
    }

    /// Point HEAD directly at a commit object id (detached HEAD). `target` must parse as a
    /// 40-hex object id. Mirrors git2's `git_repository_set_head_detached`.
    #[napi]
    pub fn set_head_detached(&self, target: String) -> napi::Result<()> {
        let _ = parse_oid(&target)?;
        self.set_head(target)
    }

    /// Point HEAD at a commit object id. In this gix-backed binding there is no distinct
    /// annotated-commit type, so this is identical to
    /// [`set_head_detached`](Self::set_head_detached). Mirrors git2's
    /// `git_repository_set_head_detached_from_annotated`.
    #[napi]
    pub fn set_head_detached_from_annotated(&self, target: String) -> napi::Result<()> {
        self.set_head_detached(target)
    }

    /// All references in the repository.
    #[napi]
    pub fn references(&self) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut out = Vec::new();
        for reference in platform.all().map_err(|e| err("references iter", e))? {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    #[napi]
    pub fn references_prefixed(&self, prefix: String) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform
            .prefixed(prefix.as_str())
            .map_err(|e| err("references_prefixed", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// The pseudo references in the root of the `.git` directory, like `HEAD` or
    /// `FETCH_HEAD` (anything ending in `HEAD` there), sorted by name.
    #[napi]
    pub fn references_pseudo(&self) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform.pseudo().map_err(|e| err("references_pseudo", e))?;
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// All reference names in the repository, optionally restricted to those
    /// matching `prefix` (e.g. `refs/heads/`). Cheaper than `references` because
    /// the references are not materialised into objects (mirrors libgit2's
    /// `git_reference_next_name`).
    #[napi]
    pub fn reference_names(&self, prefix: Option<String>) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = match prefix {
            Some(p) => platform
                .prefixed(p.as_str())
                .map_err(|e| err("references_prefixed", e))?,
            None => platform.all().map_err(|e| err("references", e))?,
        };
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(reference.name().to_string());
        }
        Ok(out)
    }

    /// All references whose name matches the shell-style `glob` (with `*` and
    /// `?`), mirroring libgit2's `git_reference_iterator_glob`. Uses
    /// [`gix::glob::wildmatch`](gix::glob::wildmatch) rather than a hand-rolled
    /// matcher.
    #[napi]
    pub fn references_glob(&self, glob: String) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let iter = platform.all().map_err(|e| err("references", e))?;
        let pattern = gix::bstr::BStr::new(glob.as_bytes());
        let mode = gix::glob::wildmatch::Mode::empty();
        let mut out = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err("reference", e))?;
            let name = reference.name().to_string();
            if gix::glob::wildmatch(pattern, gix::bstr::BStr::new(name.as_bytes()), mode) {
                out.push(build_reference(&self.inner, reference)?);
            }
        }
        Ok(out)
    }

    /// The path at which the `packed-refs` file is stored (within the common
    /// directory).
    #[napi]
    pub fn packed_refs_path(&self) -> String {
        let r = thread_local(&self.inner);
        r.refs.packed_refs_path().to_string_lossy().into_owned()
    }

    /// Set the size in bytes at which the `packed-refs` file is memory mapped
    /// (default 32768) and return the previous value. The threshold is mirrored
    /// across calls on this repository instance.
    #[napi]
    pub fn set_packed_buffer_mmap_threshold(&self, bytes: f64) -> f64 {
        use crate::error::{cached_mmap_threshold_get, cached_mmap_threshold_set};
        let key = Arc::as_ptr(&self.inner) as usize;
        let mut r = thread_local(&self.inner);
        let bytes = bytes as u64;
        let previous = match cached_mmap_threshold_get(key) {
            // The fresh thread-local view always carries the default, so the true
            // previous value comes from the cache once a threshold was set.
            Some(cached) => {
                r.refs.set_packed_buffer_mmap_threshold(bytes);
                cached
            }
            None => r.refs.set_packed_buffer_mmap_threshold(bytes),
        };
        cached_mmap_threshold_set(key, bytes);
        previous as f64
    }

    /// Forcefully reload the packed-refs buffer so that changes made by other
    /// processes become visible; some filesystems lack nanosecond granularity, so
    /// changes within the same second are otherwise likely to be missed.
    #[napi]
    pub fn force_refresh_packed_buffer(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        r.refs
            .force_refresh_packed_buffer()
            .map_err(|e| err("force_refresh_packed_buffer", e))
    }

    /// Find a single reference by name (e.g. `main`, `refs/heads/main`, `HEAD`).
    #[napi]
    pub fn find_reference(&self, name: String) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("find_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Find a single reference by name, returning `null` if it does not exist (instead of throwing).
    #[napi]
    pub fn try_find_reference(&self, name: String) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        match r.try_find_reference(name.as_str()) {
            Ok(Some(reference)) => Ok(Some(build_reference(&self.inner, reference)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(err("try_find_reference", e)),
        }
    }

    #[napi]
    pub fn get_reference(&self, name: String) -> napi::Result<String> {
        let oid: gix::ObjectId = thread_local(&self.inner)
            .rev_parse_single(name.as_str())
            .map_err(|e| err("rev_parse_single", e))?
            .into();
        match crate::replace::resolve_replace_head(&self.inner, &oid) {
            Some(replaced) => Ok(replaced.to_string()),
            None => Ok(oid.to_string()),
        }
    }

    /// Create (or, if `force`, overwrite) a reference pointing at `target`.
    #[napi]
    pub fn create_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        // `PreviousValue::MustNotExist` is not reliably enforced by the transaction
        // convenience in this gix version, so check explicitly (as `rename` does).
        if !force.unwrap_or(false) {
            if r.try_find_reference(name.as_str())
                .map_err(|e| err("create_reference", e))?
                .is_some()
            {
                return Err(err_str(
                    "create_reference",
                    format!("reference '{name}' already exists"),
                ));
            }
        }
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let label = format!("create reference {name}");
        let reference = r
            .reference(name, parse_oid(&target)?, constraint, label)
            .map_err(|e| err("create_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Create (or, if `force`, overwrite) a symbolic reference `name` pointing at the
    /// reference `target` (a fully-qualified or DWIM-resolved ref name). Mirrors git2's
    /// `git_reference_symbolic`. To create a direct (object) reference, use
    /// [`create_reference`](Self::create_reference) instead.
    #[napi]
    pub fn create_symbolic_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
        message: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        let full = gix::refs::FullName::try_from(name.as_str()).map_err(|e| err("ref_name", e))?;
        // `PreviousValue::MustNotExist` is not reliably enforced by the transaction
        // convenience in this gix version, so check explicitly (as `rename` does).
        if !force.unwrap_or(false) {
            if r.try_find_reference(name.as_str())
                .map_err(|e| err("create_symbolic_reference", e))?
                .is_some()
            {
                return Err(err_str(
                    "create_symbolic_reference",
                    format!("reference '{name}' already exists"),
                ));
            }
        }
        let symbolic = match gix::refs::FullName::try_from(target.as_str()) {
            Ok(f) => f,
            Err(_) => {
                let resolved = r
                    .find_reference(target.as_str())
                    .map_err(|e| err("ref_target", e))?;
                gix::refs::FullName::try_from(resolved.name()).map_err(|e| err("ref_target", e))?
            }
        };
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let label = message.unwrap_or_else(|| format!("create symbolic reference {name}"));
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: label.into(),
        };
        let edit = gix::refs::transaction::RefEdit::update_with_log(
            full,
            gix::refs::Target::Symbolic(symbolic),
            constraint,
            log,
        );
        r.edit_reference(edit)
            .map_err(|e| err("create_symbolic_reference", e))?;
        build_reference(
            &self.inner,
            r.find_reference(name.as_str())
                .map_err(|e| err("find_reference", e))?,
        )
    }

    /// Create a lightweight tag `name` -> `target`, i.e. a `refs/tags/<name>` reference
    /// pointing directly at `target`. `name` may be short (`v1.0.0`) or fully qualified
    /// (`refs/tags/v1.0.0`). Throws when the tag exists unless `force` is set.
    ///
    /// See [`create_tag`](Self::create_tag) for annotated (and signed) tags.
    #[napi]
    pub fn create_lightweight_tag(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        self.tag_reference(name, target, force)
    }

    /// Create (or, if `force`, overwrite) a lightweight tag reference `name` -> `target`.
    #[napi]
    pub fn tag_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.inner);
        // Normalised here (instead of handing the name to gix's `tag_reference`) so that a
        // name that already carries `refs/tags/` is not prefixed a second time.
        let full = crate::tag::to_tag_full_name(&name)?;
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .reference(
                full.to_string(),
                parse_oid(&target)?,
                constraint,
                format!("tag: created {name}"),
            )
            .map_err(|e| err("tag_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Apply a single reference edit (create or update) and return the affected reference names.
    #[napi]
    pub fn edit_reference(&self, edit: RefEditInput) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let e = build_ref_edit(&edit)?;
        let edits = r.edit_reference(e).map_err(|e| err("edit_reference", e))?;
        Ok(edits
            .into_iter()
            .map(|e| String::from_utf8_lossy(e.name.as_bstr().as_ref()).into_owned())
            .collect())
    }

    /// Apply multiple reference edits and return all affected reference names.
    #[napi]
    pub fn edit_references(&self, edits: Vec<RefEditInput>) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let mut built = Vec::with_capacity(edits.len());
        for input in &edits {
            built.push(build_ref_edit(input)?);
        }
        let result = r
            .edit_references(built)
            .map_err(|e| err("edit_references", e))?;
        Ok(result
            .into_iter()
            .map(|e| String::from_utf8_lossy(e.name.as_bstr().as_ref()).into_owned())
            .collect())
    }

    /// Apply multiple reference edits as a specific `committer` identity, returning affected names.
    #[napi]
    pub fn edit_references_as(
        &self,
        edits: Vec<RefEditInput>,
        committer: Option<Signature>,
    ) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let mut built = Vec::with_capacity(edits.len());
        for input in &edits {
            built.push(build_ref_edit(input)?);
        }
        let committer_data = match committer {
            Some(s) => {
                let sig = signature_from_dto(&s)?;
                let time_str = sig.time.to_string();
                Some((sig, time_str))
            }
            None => None,
        };
        let committer_ref =
            committer_data
                .as_ref()
                .map(|(sig, time_str)| gix::actor::SignatureRef {
                    name: sig.name.as_bstr(),
                    email: sig.email.as_bstr(),
                    time: time_str.as_str(),
                });
        let result = r
            .edit_references_as(built, committer_ref)
            .map_err(|e| err("edit_references_as", e))?;
        Ok(result
            .into_iter()
            .map(|e| String::from_utf8_lossy(e.name.as_bstr().as_ref()).into_owned())
            .collect())
    }
}
