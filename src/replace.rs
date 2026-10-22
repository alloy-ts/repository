use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// Options for object replacement.
#[napi]
#[derive(Clone, Debug, Default)]
pub struct ReplaceOptions {
    /// Force replacing an object even if types differ or replace ref exists (-f).
    pub force: bool,
    /// Reference prefix for replace refs (defaults to "refs/replace/").
    pub ref_prefix: String,
}

#[napi]
impl ReplaceOptions {
    #[napi(constructor)]
    pub fn new(force: Option<bool>, ref_prefix: Option<String>) -> Self {
        Self {
            force: force.unwrap_or(false),
            ref_prefix: ref_prefix.unwrap_or_else(|| "refs/replace/".to_string()),
        }
    }
}

/// An individual replacement revision node stored at `refs/replace/{replaceOid}`.
#[napi]
#[derive(Clone, Debug)]
pub struct ReplaceNode {
    pub(crate) oid: String,
    pub(crate) message: String,
    pub(crate) author_name: String,
    pub(crate) author_email: String,
    pub(crate) timestamp: String,
    pub(crate) prev_oid: Option<String>,
}

#[napi]
impl ReplaceNode {
    #[napi(constructor)]
    pub fn new(
        oid: String,
        message: String,
        author_name: String,
        author_email: String,
        prev_oid: Option<String>,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();

        Self {
            oid,
            message,
            author_name,
            author_email,
            timestamp,
            prev_oid,
        }
    }

    #[napi(getter)]
    pub fn oid(&self) -> String {
        self.oid.clone()
    }

    #[napi(getter)]
    pub fn message(&self) -> String {
        self.message.clone()
    }

    #[napi(getter)]
    pub fn author_name(&self) -> String {
        self.author_name.clone()
    }

    #[napi(getter)]
    pub fn author_email(&self) -> String {
        self.author_email.clone()
    }

    #[napi(getter)]
    pub fn timestamp(&self) -> String {
        self.timestamp.clone()
    }

    #[napi(getter)]
    pub fn prev_oid(&self) -> Option<String> {
        self.prev_oid.clone()
    }
}

/// The root replacement manifest object for a stable artifact/entry/object history.
#[napi]
#[derive(Clone, Debug)]
pub struct ReplaceManifest {
    pub(crate) aid: String,
    pub(crate) head_node: ReplaceNode,
    pub(crate) path: Option<String>,
    pub(crate) history_nodes: Vec<ReplaceNode>,
}

#[napi]
impl ReplaceManifest {
    #[napi(constructor)]
    pub fn new(aid: String, head_node: &ReplaceNode, path: Option<String>) -> Self {
        Self {
            aid,
            head_node: head_node.clone(),
            path,
            history_nodes: vec![head_node.clone()],
        }
    }

    #[napi(getter)]
    pub fn aid(&self) -> String {
        self.aid.clone()
    }

    #[napi(getter)]
    pub fn head_oid(&self) -> String {
        self.head_node.oid.clone()
    }

    #[napi(getter)]
    pub fn path(&self) -> Option<String> {
        self.path.clone()
    }

    #[napi(getter)]
    pub fn history(&self) -> Vec<ReplaceNode> {
        self.history_nodes.clone()
    }

    #[napi]
    pub fn add_revision(&mut self, node: &ReplaceNode) {
        self.head_node = node.clone();
        self.history_nodes.push(node.clone());
    }
}

#[napi]
impl Repository {
    /// Replace an existing object with new content using `refs/replace/*`.
    #[napi]
    pub fn replace(
        &self,
        manifest: &ReplaceManifest,
        _options: Option<&ReplaceOptions>,
    ) -> Result<ReplaceNode> {
        let r = thread_local(&self.inner);

        let aid_str = &manifest.aid;
        let new_oid_str = &manifest.head_node.oid;

        let aid_oid = parse_oid(aid_str)?;
        let new_oid = parse_oid(new_oid_str)?;

        // Write root manifest reference at refs/replace/{aid}
        let ref_name = format!("refs/replace/{}", aid_oid);
        r.reference(
            ref_name,
            new_oid,
            gix::refs::transaction::PreviousValue::Any,
            format!("replace manifest {}", aid_str),
        )
        .map_err(|e| err("reference replace manifest", e))?;

        // Write individual node reference at refs/replace/{newOid}
        let node_ref_name = format!("refs/replace/{}", new_oid_str);
        r.reference(
            node_ref_name,
            new_oid,
            gix::refs::transaction::PreviousValue::Any,
            format!("replace node {}", new_oid_str),
        )
        .map_err(|e| err("reference replace node", e))?;

        // If committed entry path exists, write reference at refs/replace/{entryPath}
        if let Some(ref path_str) = manifest.path {
            let path_ref_name = format!("refs/replace/{}", path_str.replace('/', "_"));
            r.reference(
                path_ref_name,
                new_oid,
                gix::refs::transaction::PreviousValue::Any,
                format!("replace path {}", path_str),
            )
            .map_err(|e| err("reference replace path", e))?;
        }

        Ok(manifest.head_node.clone())
    }

    /// Try replacing an object content in-place or via `refs/replace/*`.
    #[napi]
    pub fn try_replace(
        &self,
        manifest: &ReplaceManifest,
        options: Option<&ReplaceOptions>,
    ) -> Result<Option<ReplaceNode>> {
        match self.replace(manifest, options) {
            Ok(node) => Ok(Some(node)),
            Err(e) => {
                if options.map(|o| o.force).unwrap_or(false) {
                    Err(e)
                } else {
                    Ok(None)
                }
            }
        }
    }
}
