use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};

/// The repository HEAD, exposed to TypeScript.
#[napi(object)]
pub struct Head {
    pub referent: Option<String>,
}

pub fn build_head(
    _repo: &Arc<gix::ThreadSafeRepository>,
    head: gix::Head,
) -> Result<Head> {
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
