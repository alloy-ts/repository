use std::sync::Arc;

use gix::bstr::ByteSlice;
use napi::Result;
use napi_derive::napi;

use crate::entry::{Entry, build_entry, mode_string, mode_to_entry_kind};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::lossy;
use crate::repository::Repository;

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
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Tree {
    /// Create a new tree from a flat array of [`Entry`] objects, returning a [`Tree`]
    /// with its freshly assigned id.
    ///
    /// The repository is taken from the first entry that carries one, so detached
    /// entries may be mixed in; it is an error if none of them has a repository.
    /// Entry paths may contain `/` — intermediate trees are created automatically.
    #[napi(constructor)]
    pub fn new(entries: Vec<&Entry>) -> Result<Self> {
        if entries.is_empty() {
            return Err(err_str(
                "Tree",
                "creating a tree requires at least one entry",
            ));
        }
        // Detached entries carry no repository, so bind to the first entry that has one.
        let repo = entries
            .iter()
            .find_map(|e| e.repo.as_ref())
            .ok_or_else(|| {
                err_str(
                    "Tree",
                    "no entry carries a repository; include one created via `new Entry(repo, ...)`",
                )
            })?;
        let r = thread_local(repo);
        let base = gix::ObjectId::empty_tree(r.object_hash());
        let mut editor = r.edit_tree(base).map_err(|e| err("edit_tree", e))?;
        for e in &entries {
            let kind = mode_to_entry_kind(&e.mode)?;
            editor
                .upsert(e.filename.as_str(), kind, e.id)
                .map_err(|e| err("edit_tree.upsert", e))?;
        }
        let id = editor
            .write()
            .map_err(|e| err("edit_tree.write", e))?
            .detach();
        build_tree(repo, id)
    }

    /// Create a new tree from a flat array of [`Entry`] objects. Equivalent to
    /// `new Tree(entries)`, exposed as a named factory (e.g. `Tree.fromEntries([...])`).
    ///
    /// See [`Tree::new`] for the repository and nesting semantics.
    #[napi(factory)]
    pub fn from_entries(entries: Vec<&Entry>) -> Result<Self> {
        Self::new(entries)
    }

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

    /// Cast this tree's entries into [`Entry`] objects, each with `treeId` set to this tree.
    ///
    /// Unlike the `entries` getter (which returns plain `{ mode, filename, id }` objects),
    /// these are live [`Entry`] instances you can inspect further (`asTree()`), `replace()`,
    /// or feed back into `new Tree([...])` / `Tree.fromEntries([...])`.
    ///
    /// Only the tree's direct entries are returned — subtrees are not recursed into.
    #[napi]
    pub fn into_entries(&self) -> Result<Vec<Entry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let tree = tree.decode().map_err(|e| err("decode", e))?;
        let mut out = Vec::new();
        for entry in &tree.entries {
            out.push(build_entry(
                &self.repo,
                Some(self.id),
                entry.mode.into(),
                lossy(entry.filename),
                entry.oid.into(),
            ));
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
        Ok(entry.map(|e| {
            build_entry(
                &self.repo,
                Some(self.id),
                e.mode().into(),
                lossy(e.filename()),
                e.id().detach(),
            )
        }))
    }
}

/// Construct a [`Tree`] wrapper from a repository and object id (used by `Repository` / `GitObject`).
pub fn build_tree(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Result<Tree> {
    Ok(Tree {
        repo: Arc::clone(repo),
        id,
    })
}

/// Build a tree object from `TreeItemInput` entries and write it to the object database,
/// returning its id.
fn write_tree_entries(r: &gix::Repository, entries: Vec<TreeItemInput>) -> Result<gix::ObjectId> {
    let mut tree_entries = Vec::new();
    for e in entries {
        let mode = mode_to_entry_kind(
            e.mode
                .as_deref()
                .unwrap_or(crate::entry::DEFAULT_ENTRY_MODE),
        )?
        .into();
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
    Ok(oid.detach())
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
        Ok(write_tree_entries(&r, entries)?.to_string())
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
                let kind = mode_to_entry_kind(
                    e.mode
                        .as_deref()
                        .unwrap_or(crate::entry::DEFAULT_ENTRY_MODE),
                )?;
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
