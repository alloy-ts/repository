use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use gix::config::AsBStr;

use crate::commit::{Commit, build_commit};
use crate::error::{err, parse_oid, thread_local};
use crate::object::{Signature, Tree, build_tree};
use crate::repository::{Repository, signature_from_dto};

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
    name: String,
    target: String,
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

pub fn build_reference(
    repo: &Arc<gix::ThreadSafeRepository>,
    reference: gix::Reference,
) -> Result<Reference> {
    let name = reference.name().to_string();
    let target = match reference.target() {
        gix::refs::TargetRef::Object(id) => id.to_string(),
        gix::refs::TargetRef::Symbolic(_) => String::new(),
    };
    Ok(Reference {
        repo: Arc::clone(repo),
        name,
        target,
    })
}

/// A single reference edit, mirroring `gix::refs::RefEdit` for use from TypeScript.
#[derive(Clone)]
#[napi(object)]
pub struct RefEditInput {
    /// The full reference name, e.g. `refs/heads/branch`.
    pub name: String,
    /// The new target: a 40-hex object id, or a symbolic reference name (e.g. `refs/heads/main`, `HEAD`).
    pub new_target: String,
    /// When `true`, overwrite an existing reference (`PreviousValue::Any`); when `false`, only create (`PreviousValue::MustNotExist`).
    pub force: bool,
    /// Optional reflog message.
    pub log_message: Option<String>,
}

/// Build a `gix::refs::RefEdit` from the input DTO.
fn build_ref_edit(input: &RefEditInput) -> Result<gix::refs::transaction::RefEdit> {
    let name =
        gix::refs::FullName::try_from(input.name.as_str()).map_err(|e| err("ref_name", e))?;
    let target = match parse_oid(&input.new_target) {
        Ok(oid) => gix::refs::Target::Object(oid),
        Err(_) => gix::refs::Target::Symbolic(
            gix::refs::FullName::try_from(input.new_target.as_str())
                .map_err(|e| err("ref_target", e))?,
        ),
    };
    let expected = if input.force {
        gix::refs::transaction::PreviousValue::Any
    } else {
        gix::refs::transaction::PreviousValue::MustNotExist
    };
    Ok(gix::refs::transaction::RefEdit::update(
        name,
        target,
        expected,
        input.log_message.clone().unwrap_or_default(),
    ))
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
