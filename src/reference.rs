use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use gix::config::AsBStr;

use crate::commit::{Commit, build_commit};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::repository::Repository;
use crate::tree::{Tree, build_tree};

/// The repository HEAD, exposed to TypeScript.
#[napi(object)]
pub struct Head {
    pub referent: Option<String>,
}

pub fn build_head(_repo: &Arc<gix::ThreadSafeRepository>, head: gix::Head) -> Result<Head> {
    let referent = head.referent_name().map(|n| n.to_string());
    Ok(Head { referent })
}

/// A git reference (branch, tag, or HEAD) exposed to TypeScript.
#[napi]
pub struct Reference {
    repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
    target: String,
    symbolic_target: Option<String>,
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

    /// The reference category derived from the name prefix, e.g. `tag`, `local_branch`,
    /// `remote_branch`, `note`, `pseudo_ref`, `main_ref`, `linked_ref`, `bisect`,
    /// `rewritten`, or `worktree_private`; `null` when the name has no known category.
    #[napi]
    pub fn category(&self) -> Option<String> {
        let full_name = gix::refs::FullName::try_from(self.name.as_str()).ok()?;
        full_name.category().map(category_to_string)
    }

    /// Like [`category()`](Self::category), but also returns the shortened name with
    /// the category prefix stripped; `null` when the name has no known category.
    #[napi(ts_return_type = "RefCategoryAndShortName | null")]
    pub fn category_and_short_name(&self) -> Option<RefCategoryAndShortName> {
        let full_name = gix::refs::FullName::try_from(self.name.as_str()).ok()?;
        full_name
            .category_and_short_name()
            .map(|(category, short_name)| RefCategoryAndShortName {
                category: category_to_string(category),
                short_name: String::from_utf8_lossy(short_name).into_owned(),
            })
    }

    /// The name with well-known category prefixes stripped, e.g.
    /// `refs/heads/main` → `main`. Names without a known prefix are returned as-is.
    #[napi]
    pub fn shorten(&self) -> Result<String> {
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        Ok(String::from_utf8_lossy(full_name.shorten()).into_owned())
    }

    /// The file name portion of the full name, e.g. `main` for `refs/heads/main`.
    #[napi]
    pub fn file_name(&self) -> Result<String> {
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        Ok(String::from_utf8_lossy(full_name.as_ref().file_name()).into_owned())
    }

    /// Follow all symbolic targets and peel the underlying object to the end of the
    /// tag chain, returning the first non-tag object id this reference ultimately
    /// points to. Throws when a symbolic chain loops or an object is missing.
    #[napi]
    pub fn peel_to_id(&self) -> Result<String> {
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
    pub fn follow(&self) -> Result<Option<Reference>> {
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

    /// Delete this reference.
    #[napi]
    pub fn delete(&self) -> Result<()> {
        let r = thread_local(&self.repo);
        let reference = r
            .find_reference(self.name.as_str())
            .map_err(|e| err("find_reference", e))?;
        reference.delete().map_err(|e| err("delete", e))?;
        Ok(())
    }
}

fn category_to_string(category: gix::refs::Category<'_>) -> String {
    use gix::refs::Category;
    match category {
        Category::Tag => "tag",
        Category::LocalBranch => "local_branch",
        Category::RemoteBranch => "remote_branch",
        Category::Note => "note",
        Category::PseudoRef => "pseudo_ref",
        Category::MainPseudoRef => "main_pseudo_ref",
        Category::MainRef => "main_ref",
        Category::LinkedPseudoRef { .. } => "linked_pseudo_ref",
        Category::LinkedRef { .. } => "linked_ref",
        Category::Bisect => "bisect",
        Category::Rewritten => "rewritten",
        Category::WorktreePrivate => "worktree_private",
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

fn category_from_string(category: &str) -> Result<gix::refs::Category<'static>> {
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

/// Build a fully qualified reference name from a `category` (as returned by
/// [`Reference.category()`](Reference::category)) and a `shortName`, e.g.
/// `local_branch` + `main` → `refs/heads/main`.
///
/// If `shortName` already carries the category prefix, it is used as-is. The
/// linked-worktree categories (`linked_pseudo_ref`, `linked_ref`) are not
/// supported as they require a worktree name.
#[napi]
pub fn ref_category_full_name(category: String, short_name: String) -> Result<String> {
    let category = category_from_string(&category)?;
    let full_name = category
        .to_full_name(gix::bstr::BStr::new(short_name.as_bytes()))
        .map_err(|e| err("to_full_name", e))?;
    Ok(full_name.to_string())
}

pub fn build_reference(
    repo: &Arc<gix::ThreadSafeRepository>,
    reference: gix::Reference,
) -> Result<Reference> {
    let name = reference.name().to_string();
    let (target, symbolic_target) = match reference.target() {
        gix::refs::TargetRef::Object(id) => (id.to_string(), None),
        gix::refs::TargetRef::Symbolic(name) => (String::new(), Some(name.to_string())),
    };
    Ok(Reference {
        repo: Arc::clone(repo),
        name,
        target,
        symbolic_target,
    })
}

#[napi(object)]
pub struct ReflogEntry {
    /// The previous object id of the reference; the null hash when the ref was created.
    pub previous_oid: String,
    /// The new object id of the reference; the null hash when the ref was deleted.
    pub new_oid: String,
    /// The identity that made the change.
    pub signature: Signature,
    /// The reflog message; empty when there was none.
    pub message: String,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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
fn parse_expected_target(target: &str) -> Result<gix::refs::Target> {
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
) -> Result<gix::refs::transaction::PreviousValue> {
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
fn build_ref_edit(input: &RefEditInput) -> Result<gix::refs::transaction::RefEdit> {
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
    /// The repository `HEAD`.
    #[napi]
    pub fn head(&self) -> Result<Head> {
        let r = thread_local(&self.inner);
        build_head(&self.inner, r.head().map_err(|e| err("head", e))?)
    }

    /// The commit `HEAD` points to.
    #[napi]
    pub fn head_commit(&self) -> Result<Commit> {
        let r = thread_local(&self.inner);
        build_commit(
            &self.inner,
            r.head_commit().map_err(|e| err("head_commit", e))?,
        )
    }

    /// The object id `HEAD` points to.
    #[napi]
    pub fn head_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_id().map_err(|e| err("head_id", e))?;
        Ok(id.to_string())
    }

    /// The symbolic referent name of `HEAD`, if it is not detached.
    #[napi]
    pub fn head_name(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.head_name()
            .map_err(|e| err("head_name", e))?
            .map(|n| n.to_string()))
    }

    /// The object id of the root tree pointed to by HEAD.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// The object id of the root tree pointed to by HEAD, falling back to the empty tree
    /// if HEAD is unborn.
    #[napi]
    pub fn head_tree_id_or_empty(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .head_tree_id_or_empty()
            .map_err(|e| err("head_tree_id_or_empty", e))?;
        Ok(id.to_string())
    }

    /// The reference HEAD points to, or null if HEAD is detached or unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        match r.head_ref().map_err(|e| err("head_ref", e))? {
            Some(rf) => Ok(Some(build_reference(&self.inner, rf)?)),
            None => Ok(None),
        }
    }

    /// The root tree object HEAD points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r.head_tree().map_err(|e| err("head_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Set HEAD to point to a commit ID.
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target)?;
        let label = format!("set_head {target}");
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("set_head", e))?;
        Ok(())
    }

    /// All references in the repository.
    #[napi]
    pub fn references(&self) -> Result<Vec<Reference>> {
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
    pub fn references_prefixed(&self, prefix: String) -> Result<Vec<Reference>> {
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
    pub fn references_pseudo(&self) -> Result<Vec<Reference>> {
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

    /// Whether a reflog exists for the given reference name.
    #[napi]
    pub fn reflog_exists(&self, name: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        r.refs
            .reflog_exists(name.as_str())
            .map_err(|e| err("reflog_exists", e))
    }

    /// All reflog entries of `name`, oldest first (or most recent first when
    /// `reverse` is set). Empty when the ref has no reflog; a missing ref also
    /// yields an empty list instead of an error.
    #[napi]
    pub fn reflog(&self, name: String, reverse: Option<bool>) -> Result<Vec<ReflogEntry>> {
        fn push_entry(out: &mut Vec<ReflogEntry>, line: gix::refs::log::Line) {
            out.push(ReflogEntry {
                previous_oid: line.previous_oid.to_string(),
                new_oid: line.new_oid.to_string(),
                signature: to_signature(
                    &line.signature.name,
                    &line.signature.email,
                    line.signature.time.seconds,
                    line.signature.time.offset,
                ),
                message: lossy(&line.message),
            });
        }
        let r = thread_local(&self.inner);
        // Iterate through the store directly so that refs without a reflog (or refs
        // that no longer exist, e.g. after a log-only deletion) yield empty results.
        let full_name =
            gix::refs::FullName::try_from(name.as_str()).map_err(|e| err("ref_name", e))?;
        let mut platform = gix::refs::file::log::iter::Platform {
            store: &r.refs,
            name: full_name.as_ref(),
            buf: Vec::new(),
        };
        let mut out = Vec::new();
        if reverse.unwrap_or(false) {
            if let Some(iter) = platform.rev().map_err(|e| err("reflog.rev", e))? {
                for line in iter {
                    push_entry(&mut out, line.map_err(|e| err("reflog line", e))?.into());
                }
            }
        } else if let Some(iter) = platform.all().map_err(|e| err("reflog.all", e))? {
            for line in iter {
                push_entry(&mut out, line.map_err(|e| err("reflog line", e))?.into());
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
    pub fn force_refresh_packed_buffer(&self) -> Result<()> {
        let r = thread_local(&self.inner);
        r.refs
            .force_refresh_packed_buffer()
            .map_err(|e| err("force_refresh_packed_buffer", e))
    }

    /// Find a single reference by name (e.g. `main`, `refs/heads/main`, `HEAD`).
    #[napi]
    pub fn find_reference(&self, name: String) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("find_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Find a single reference by name, returning `null` if it does not exist (instead of throwing).
    #[napi]
    pub fn try_find_reference(&self, name: String) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        match r.try_find_reference(name.as_str()) {
            Ok(Some(reference)) => Ok(Some(build_reference(&self.inner, reference)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(err("try_find_reference", e)),
        }
    }

    /// Create (or, if `force`, overwrite) a reference pointing at `target`.
    #[napi]
    pub fn create_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
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

    /// Create an annotated tag `name` -> `target`.
    #[napi]
    pub fn create_tag(
        &self,
        name: String,
        target: String,
        message: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let target_id = parse_oid(&target)?;
        let kind = r
            .find_object(target_id.clone())
            .map_err(|e| err("find_object", e))?
            .kind;
        let tagger = r.committer().and_then(|res| res.ok());
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag(name, target_id, kind, tagger, message.as_str(), constraint)
            .map_err(|e| err("create_tag", e))?;
        build_reference(&self.inner, reference)
    }

    /// Create a lightweight tag `name` -> `target`.
    #[napi]
    pub fn create_lightweight_tag(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag_reference(name, parse_oid(&target)?, constraint)
            .map_err(|e| err("create_lightweight_tag", e))?;
        build_reference(&self.inner, reference)
    }

    /// Create (or, if `force`, overwrite) a lightweight tag reference `name` -> `target`.
    #[napi]
    pub fn tag_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag_reference(name, parse_oid(&target)?, constraint)
            .map_err(|e| err("tag_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Apply a single reference edit (create or update) and return the affected reference names.
    #[napi]
    pub fn edit_reference(&self, edit: RefEditInput) -> Result<Vec<String>> {
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
    pub fn edit_references(&self, edits: Vec<RefEditInput>) -> Result<Vec<String>> {
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
    ) -> Result<Vec<String>> {
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
