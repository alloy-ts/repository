use std::sync::Arc;

use gix::bstr::ByteSlice;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::lossy;
use crate::repository::Repository;

fn mode_string(kind: gix::object::tree::EntryKind) -> String {
    match kind {
        gix::object::tree::EntryKind::Blob => "100644".to_string(),
        gix::object::tree::EntryKind::BlobExecutable => "100755".to_string(),
        gix::object::tree::EntryKind::Tree => "040000".to_string(),
        gix::object::tree::EntryKind::Link => "120000".to_string(),
        gix::object::tree::EntryKind::Commit => "160000".to_string(),
    }
}

/// A single entry of a git tree.
#[napi(object)]
pub struct TreeEntry {
    pub mode: String,
    pub filename: String,
    pub id: String,
}

/// A git tree exposed to TypeScript.
#[napi]
pub struct Tree {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Tree {
    /// The object id of this tree.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// All entries of the tree.
    #[napi(getter)]
    pub fn entries(&self) -> Result<Vec<TreeEntry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let tree = tree.decode().map_err(|e| err("decode", e))?;
        let mut out = Vec::new();
        for entry in &tree.entries {
            out.push(TreeEntry {
                mode: mode_string(entry.mode.into()),
                filename: lossy(entry.filename),
                id: entry.oid.to_string(),
            });
        }
        Ok(out)
    }

    /// Look up a single entry by file name.
    #[napi]
    pub fn entry(&self, name: String) -> Result<Option<TreeEntry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let tree = tree.decode().map_err(|e| err("decode", e))?;
        for entry in &tree.entries {
            if entry.filename.as_bytes() == name.as_bytes() {
                return Ok(Some(TreeEntry {
                    mode: mode_string(entry.mode.into()),
                    filename: lossy(entry.filename),
                    id: entry.oid.to_string(),
                }));
            }
        }
        Ok(None)
    }

    /// Look up a single entry by `path` relative to this tree, following nested trees
    /// (e.g. `a/b/c.txt`). Returns `null` when no entry matches, or when an intermediate
    /// path component is not a tree.
    #[napi]
    pub fn entry_by_path(&self, path: String) -> Result<Option<Entry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let entry = tree
            .lookup_entry_by_path(std::path::Path::new(&path))
            .map_err(|e| err("lookup_entry_by_path", e))?;
        Ok(entry.map(|e| Entry {
            repo: Arc::clone(&self.repo),
            tree_id: self.id,
            mode: mode_string(e.mode().into()),
            filename: lossy(e.filename()),
            id: e.id().detach(),
        }))
    }
}

/// A tree entry resolved through a path lookup, with repository access for further traversal.
#[napi]
pub struct Entry {
    repo: Arc<gix::ThreadSafeRepository>,
    tree_id: gix::ObjectId,
    mode: String,
    filename: String,
    id: gix::ObjectId,
}

#[napi]
impl Entry {
    /// The entry mode (`100644`, `100755`, `040000`, `120000`, or `160000`).
    #[napi(getter)]
    pub fn mode(&self) -> String {
        self.mode.clone()
    }

    /// The entry's file name (the last path component).
    #[napi(getter)]
    pub fn filename(&self) -> String {
        self.filename.clone()
    }

    /// The object id this entry points to.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// The id of the tree this entry was resolved from.
    #[napi(getter)]
    pub fn tree_id(&self) -> String {
        self.tree_id.to_string()
    }

    /// Whether this entry is a directory (subtree).
    #[napi]
    pub fn is_tree(&self) -> bool {
        self.mode == "040000"
    }

    /// If this entry is a directory, resolve it as a [`Tree`]; otherwise `null`.
    #[napi]
    pub fn as_tree(&self) -> Result<Option<Tree>> {
        if !self.is_tree() {
            return Ok(None);
        }
        build_tree(&self.repo, self.id).map(Some)
    }
}

/// Construct a [`Tree`] wrapper from a repository and object id (used by `Repository` / `GitObject`).
pub fn build_tree(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Result<Tree> {
    Ok(Tree {
        repo: Arc::clone(repo),
        id,
    })
}

/// Input structure for constructing a tree item.
#[derive(Clone)]
#[napi(object)]
pub struct TreeItemInput {
    pub path: String,
    pub mode: Option<String>,
    pub id: String,
}

/// A single edit applied by [`Repository::edit_tree`].
#[napi(object)]
pub struct TreeEditInput {
    /// Path of the entry to upsert or remove (e.g. `a/b/c.txt`).
    pub path: String,
    /// Mode for upserts (`100644`, `100755`, `040000`, `120000`, `160000`). Defaults to `100644`.
    pub mode: Option<String>,
    /// Object id for upserts. Required unless `remove` is set.
    pub id: Option<String>,
    /// When true, remove the entry at `path` instead of upserting.
    pub remove: Option<bool>,
    /// How to remove when `remove` is set: `"any"` (default) removes the entry including any
    /// subtree below it; `"leaf"` errors if the entry at `path` is a tree; `"ifLeaf"` leaves
    /// an existing tree at `path` untouched instead of removing it.
    pub remove_mode: Option<String>,
}

fn mode_to_entry_kind(mode: &str) -> Result<gix::objs::tree::EntryKind> {
    match mode {
        "100644" => Ok(gix::objs::tree::EntryKind::Blob),
        "100755" => Ok(gix::objs::tree::EntryKind::BlobExecutable),
        "040000" | "40000" => Ok(gix::objs::tree::EntryKind::Tree),
        "120000" => Ok(gix::objs::tree::EntryKind::Link),
        "160000" => Ok(gix::objs::tree::EntryKind::Commit),
        other => Err(err_str("tree entry", format!("invalid file mode {other}"))),
    }
}

#[napi]
impl Repository {
    /// Find and decode a tree by id.
    #[napi]
    pub fn find_tree(&self, id: String) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r
            .find_tree(parse_oid(&id)?)
            .map_err(|e| err("find_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Write a tree object constructed from tree item inputs and return its OID.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree_entries = Vec::new();
        for e in entries {
            let mode = mode_to_entry_kind(e.mode.as_deref().unwrap_or("100644"))?.into();
            tree_entries.push(gix::objs::tree::Entry {
                mode,
                filename: e.path.into(),
                oid: parse_oid(&e.id)?,
            });
        }
        let tree = gix::objs::Tree {
            entries: tree_entries,
        };
        let oid = r.write_object(&tree).map_err(|e| err("write_tree", e))?;
        Ok(oid.to_string())
    }

    /// Edit a tree by applying `edits` (upsert/remove entries) on top of `base_tree_id`,
    /// returning the new tree id.
    #[napi]
    pub fn edit_tree(&self, base_tree_id: String, edits: Vec<TreeEditInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let base = parse_oid(&base_tree_id)?;
        let mut editor = r.edit_tree(base).map_err(|e| err("edit_tree", e))?;
        for e in &edits {
            if e.remove.unwrap_or(false) {
                match e.remove_mode.as_deref() {
                    None | Some("any") => {
                        editor
                            .remove(e.path.as_str())
                            .map_err(|e| err("edit_tree.remove", e))?;
                    }
                    Some("leaf") => {
                        editor
                            .remove_leaf(e.path.as_str())
                            .map_err(|e| err("edit_tree.remove_leaf", e))?;
                    }
                    // The facade editor has no `remove_if_leaf()`; emulate it by checking the
                    // base tree: remove only when the entry exists and is not a tree. Note the
                    // editor's own `get()` only sees in-memory trees, so it can't be used on a
                    // freshly created editor.
                    Some("ifLeaf") => {
                        let base_tree = r.find_tree(base).map_err(|e| err("find_tree", e))?;
                        let is_tree = base_tree
                            .lookup_entry_by_path(std::path::Path::new(&e.path))
                            .map_err(|e| err("edit_tree.remove_if_leaf", e))?
                            .map(|entry| entry.mode().is_tree());
                        if is_tree == Some(false) {
                            editor
                                .remove(e.path.as_str())
                                .map_err(|e| err("edit_tree.remove", e))?;
                        }
                    }
                    Some(other) => {
                        return Err(err_str(
                            "edit_tree",
                            format!(
                                "invalid removeMode {other}; expected \"any\", \"leaf\", or \"ifLeaf\""
                            ),
                        ));
                    }
                };
            } else {
                let kind = mode_to_entry_kind(e.mode.as_deref().unwrap_or("100644"))?;
                let oid = parse_oid(
                    e.id.as_deref()
                        .ok_or_else(|| err_str("edit_tree", "upsert edit requires an `id`"))?,
                )?;
                editor
                    .upsert(e.path.as_str(), kind, oid)
                    .map_err(|e| err("edit_tree.upsert", e))?;
            }
        }
        let new_id = editor
            .write()
            .map_err(|e| err("edit_tree.write", e))?
            .detach();
        Ok(new_id.to_string())
    }

    /// Return the well-known empty tree object id as a [`Tree`].
    #[napi]
    pub fn empty_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let id = gix::ObjectId::empty_tree(r.object_hash());
        build_tree(&self.inner, id)
    }
}
