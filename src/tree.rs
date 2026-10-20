use std::sync::Arc;

use gix::bstr::ByteSlice;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;

use crate::blob::{Blob, build_blob};
use crate::entry::{DEFAULT_ENTRY_MODE, Entry, build_entry, mode_string, mode_to_entry_kind};
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

#[napi]
impl TreeEntry {
    /// If this entry is a blob, resolve it as a [`Blob`] using `repo`; otherwise `null`.
    ///
    /// `TreeEntry` carries no repository of its own (it is a plain `{ mode, filename, id }`
    /// record), so the repository to resolve against must be supplied here.
    #[napi]
    pub fn to_blob(&self, repo: &Repository) -> napi::Result<Option<Blob>> {
        if self.mode == "040000" {
            return Ok(None);
        }
        let id = parse_oid(&self.id)?;
        Ok(Some(build_blob(&repo.inner, id)))
    }

    /// If this entry is a subtree (mode `040000`), resolve it as a [`Tree`];
    /// otherwise `null`. Parity with [`Entry::as_tree`].
    #[napi]
    pub fn to_tree(&self, repo: &Repository) -> napi::Result<Option<Tree>> {
        if self.mode != "040000" {
            return Ok(None);
        }
        let id = parse_oid(&self.id)?;
        Ok(Some(build_tree(&repo.inner, id)?))
    }

    /// The entry kind derived from its [`TreeEntry::mode`], as a friendly string:
    /// `"blob"`, `"blobExecutable"`, `"tree"`, `"link"`, `"commit"`, or
    /// `"unknown"`. Easier to branch on than parsing the raw mode.
    #[napi(getter)]
    pub fn kind(&self) -> String {
        match self.mode.as_str() {
            "100644" => "blob",
            "100755" => "blobExecutable",
            "040000" | "40000" => "tree",
            "120000" => "link",
            "160000" => "commit",
            _ => "unknown",
        }
        .to_string()
    }
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
    pub fn new(entries: Vec<&Entry>) -> napi::Result<Self> {
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
    pub fn from_entries(entries: Vec<&Entry>) -> napi::Result<Self> {
        Self::new(entries)
    }

    /// Build a tree directly from raw file contents, writing each blob and creating
    /// intermediate directories as needed. Equivalent to the nested `Tree<Node>`
    /// builder pattern, expressed as a flat list of `{ path, mode?, content }`.
    ///
    /// `content` is written as a blob; `mode` defaults to `"100644"`. Returns the
    /// resulting [`Tree`] (call `.id` to get the object id). This is the
    /// content-oriented counterpart to [`Tree::from_entries`], which expects
    /// pre-written objects.
    #[napi(factory)]
    pub fn from_contents(repo: &Repository, inputs: Vec<TreeContentInput>) -> napi::Result<Self> {
        if inputs.is_empty() {
            return Err(err_str(
                "Tree",
                "creating a tree from contents requires at least one input",
            ));
        }
        let r = thread_local(&repo.inner);
        let base = gix::ObjectId::empty_tree(r.object_hash());
        let mut editor = r.edit_tree(base).map_err(|e| err("edit_tree", e))?;
        for input in &inputs {
            let data: Vec<u8> = match &input.content {
                Either::A(s) => s.clone().into_bytes(),
                Either::B(b) => b.to_vec(),
            };
            let oid = r
                .write_blob(data)
                .map_err(|e| err("write_blob", e))?
                .detach();
            let kind = mode_to_entry_kind(input.mode.as_deref().unwrap_or(DEFAULT_ENTRY_MODE))?;
            editor
                .upsert(input.path.as_str(), kind, oid)
                .map_err(|e| err("edit_tree.upsert", e))?;
        }
        let id = editor
            .write()
            .map_err(|e| err("edit_tree.write", e))?
            .detach();
        build_tree(&repo.inner, id)
    }

    /// The object id of this tree.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// All entries of the tree.
    #[napi(getter)]
    pub fn entries(&self) -> napi::Result<Vec<TreeEntry>> {
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
    pub fn into_entries(&self) -> napi::Result<Vec<Entry>> {
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
    pub fn entry(&self, name: String) -> napi::Result<Option<TreeEntry>> {
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
    pub fn entry_by_path(&self, path: String) -> napi::Result<Option<Entry>> {
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

    /// Recursively walk the tree, returning every entry with its full path from the
    /// root. By default only `{ path, mode, id }` are returned; pass
    /// `{ contents: true }` to additionally include each blob's raw `content`.
    ///
    /// Subtrees are recursed into automatically; entries are emitted in depth-first,
    /// lexicographically-ordered-by-path order.
    #[napi]
    pub fn walk(&self, options: Option<TreeWalkOptions>) -> napi::Result<Vec<TreeWalkEntry>> {
        let with_content = options.and_then(|o| o.contents).unwrap_or(false);
        let r = thread_local(&self.repo);
        let mut out = Vec::new();
        walk_tree(&r, self.id, "", with_content, &mut out)?;
        Ok(out)
    }

    /// Open a [`TreeBuilder`] that constructs a tree incrementally on top of
    /// `base_tree_id` (or the empty tree when omitted). Equivalent to
    /// `new TreeBuilder(repo, baseTreeId?)`.
    ///
    /// Exposed as a static method: `Tree.build(repo, baseTreeId?)`.
    #[napi]
    pub fn build(repo: &Repository, base_tree_id: Option<String>) -> napi::Result<TreeBuilder> {
        TreeBuilder::new(repo, base_tree_id)
    }
}

/// Construct a [`Tree`] wrapper from a repository and object id (used by `Repository` / `GitObject`).
pub fn build_tree(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> napi::Result<Tree> {
    Ok(Tree {
        repo: Arc::clone(repo),
        id,
    })
}

/// Depth-first walk of `tree_id`, appending every entry (with its full `prefix` path)
/// to `out`. When `with_content` is set, each blob's raw data is also read into `content`.
fn walk_tree(
    r: &gix::Repository,
    tree_id: gix::ObjectId,
    prefix: &str,
    with_content: bool,
    out: &mut Vec<TreeWalkEntry>,
) -> napi::Result<()> {
    let tree = r.find_tree(tree_id).map_err(|e| err("find_tree", e))?;
    let tree = tree.decode().map_err(|e| err("decode", e))?;
    for entry in &tree.entries {
        let name = lossy(entry.filename);
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind: gix::object::tree::EntryKind = entry.mode.into();
        let oid: gix::ObjectId = entry.oid.to_owned();
        let is_tree = matches!(kind, gix::object::tree::EntryKind::Tree);
        let content = if with_content && !is_tree {
            let blob = r.find_blob(oid).map_err(|e| err("find_blob", e))?;
            Some(Buffer::from(blob.data.to_vec()))
        } else {
            None
        };
        out.push(TreeWalkEntry {
            path: path.clone(),
            mode: mode_string(kind),
            id: oid.to_string(),
            content,
        });
        if is_tree {
            walk_tree(r, oid, &path, with_content, out)?;
        }
    }
    Ok(())
}

/// Build a tree object from `TreeItemInput` entries and write it to the object database,
/// returning its id.
fn write_tree_entries(
    r: &gix::Repository,
    entries: Vec<TreeItemInput>,
) -> napi::Result<gix::ObjectId> {
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

/// Input for [`Tree::from_contents`]: a single file described by its raw content
/// (rather than a pre-written object id, as [`TreeItemInput`] requires).
///
/// The blob for `content` is written to the object database automatically; the
/// tree is assembled from the resulting blob ids. `path` may contain `/` — the
/// intermediate trees are created for you.
#[napi(object)]
pub struct TreeContentInput {
    /// Full path relative to the tree root, e.g. `"src/lib/util.ts"`. Intermediate
    /// directories are created automatically.
    pub path: String,
    /// Entry mode; defaults to `"100644"`. Other well-known values: `"100755"`
    /// (executable), `"040000"` (subtree), `"120000"` (symlink), `"160000"` (gitlink).
    pub mode: Option<String>,
    /// Raw file content: a string or an `ArrayBuffer`/`Buffer`.
    pub content: Either<String, Buffer>,
}

/// Options controlling [`Tree::walk`].
#[napi(object)]
pub struct TreeWalkOptions {
    /// Include each blob's raw content in the returned `content` field. Defaults to `false`.
    pub contents: Option<bool>,
}

/// A single entry produced by [`Tree::walk`], carrying its full path from the
/// tree root (unlike [`Tree::entries`], which returns only direct children).
#[napi(object)]
pub struct TreeWalkEntry {
    /// Full path from the tree root, e.g. `"src/lib/util.ts"`.
    pub path: String,
    /// Entry mode (`100644`, `100755`, `040000`, `120000`, or `160000`).
    pub mode: String,
    /// Object id of the entry.
    pub id: String,
    /// Raw blob content; present only when [`TreeWalkOptions::contents`] was set
    /// and the entry is not a subtree.
    pub content: Option<Buffer>,
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
    pub fn find_tree(&self, id: String) -> napi::Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r
            .find_tree(parse_oid(&id)?)
            .map_err(|e| err("find_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Write a tree object constructed from tree item inputs and return its OID.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        Ok(write_tree_entries(&r, entries)?.to_string())
    }

    /// Edit a tree by applying `edits` (upsert/remove entries) on top of `base_tree_id`,
    /// returning the new tree id.
    #[napi]
    pub fn edit_tree(
        &self,
        base_tree_id: String,
        edits: Vec<TreeEditInput>,
    ) -> napi::Result<String> {
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
    pub fn empty_tree(&self) -> napi::Result<Tree> {
        let r = thread_local(&self.inner);
        let id = gix::ObjectId::empty_tree(r.object_hash());
        build_tree(&self.inner, id)
    }
}

/// How a [`TreeBuilder::remove`] edit targets the entry at a path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RemoveMode {
    /// Remove the entry together with any subtree below it (no error when absent).
    Any,
    /// Error when the entry at the path is a tree.
    Leaf,
    /// Leave an existing tree at the path untouched instead of removing it.
    IfLeaf,
}

/// One recorded edit, replayed onto a fresh editor when the builder is written.
#[derive(Clone)]
enum Edit {
    Upsert {
        path: String,
        kind: gix::object::tree::EntryKind,
        oid: gix::ObjectId,
    },
    Remove {
        path: String,
        mode: RemoveMode,
    },
}

/// Options for [`TreeBuilder::write`].
#[napi(object)]
pub struct TreeBuilderWriteOptions {
    /// Validate the produced tree before writing: every entry must have a legal
    /// filename and its object id must resolve to an existing object. Defaults to
    /// `true`. Set to `false` to skip validation entirely (matching git2's
    /// `TreeBuilder`, which performs no checks) — the tree is then written as-is.
    pub validate: Option<bool>,
}

/// Incremental tree builder: record upserts/removals, then `write()` the resulting tree.
///
/// Edits are buffered in memory and replayed onto a base tree (or the empty tree) at
/// `write()` time, so intermediate builder calls never touch the object database. Open
/// one via `Tree.build(repo, baseTreeId?)` or `new TreeBuilder(repo, baseTreeId?)`.
#[napi]
pub struct TreeBuilder {
    repo: Arc<gix::ThreadSafeRepository>,
    base: Option<gix::ObjectId>,
    log: Vec<Edit>,
}

#[napi]
impl TreeBuilder {
    /// Create a builder that applies edits on top of `base_tree_id`, or on top of the
    /// empty tree when omitted. The base tree must exist and be a tree.
    #[napi(constructor)]
    pub fn new(repo: &Repository, base_tree_id: Option<String>) -> napi::Result<Self> {
        let base = match base_tree_id {
            Some(id) => {
                let oid = parse_oid(&id)?;
                let r = thread_local(&repo.inner);
                r.find_tree(oid)
                    .map_err(|e| err("TreeBuilder: base tree", e))?;
                Some(oid)
            }
            None => None,
        };
        Ok(TreeBuilder {
            repo: Arc::clone(&repo.inner),
            base,
            log: Vec::new(),
        })
    }

    /// Insert or overwrite the entry at `path` (nested paths create intermediate trees
    /// automatically). `mode` defaults to `100644`; see `EntryMode` for the well-known
    /// values. Fails fast on an invalid mode or object id; object existence is checked
    /// at `write()` time (unless validation is disabled).
    #[napi]
    pub fn upsert(&mut self, path: String, id: String, mode: Option<String>) -> napi::Result<()> {
        let kind = mode_to_entry_kind(mode.as_deref().unwrap_or(DEFAULT_ENTRY_MODE))?;
        let oid = parse_oid(&id)?;
        self.log.push(Edit::Upsert { path, kind, oid });
        Ok(())
    }

    /// Remove the entry at `path`. `removeMode` defaults to `"any"`:
    /// - `"any"`: remove the entry including any subtree below it (no error when absent)
    /// - `"leaf"`: error at `write()` when the entry at `path` is a tree
    /// - `"ifLeaf"`: leave an existing tree at `path` untouched
    #[napi]
    pub fn remove(&mut self, path: String, remove_mode: Option<String>) -> napi::Result<()> {
        let mode = match remove_mode.as_deref() {
            None | Some("any") => RemoveMode::Any,
            Some("leaf") => RemoveMode::Leaf,
            Some("ifLeaf") => RemoveMode::IfLeaf,
            Some(other) => {
                return Err(err_str(
                    "TreeBuilder",
                    format!(
                        "invalid removeMode {other}; expected \"any\", \"leaf\", or \"ifLeaf\""
                    ),
                ));
            }
        };
        self.log.push(Edit::Remove { path, mode });
        Ok(())
    }

    /// Write `content` as a blob and upsert it at `path` (intermediate trees are
    /// created automatically). `mode` defaults to `100644`. This is the content
    /// counterpart to [`TreeBuilder::upsert`], for when you have raw content
    /// rather than a pre-written object id.
    ///
    /// Call it repeatedly on the same builder, then [`TreeBuilder::write`]:
    /// ```ts
    /// const b = Tree.build(repo);
    /// b.file("a.txt", { content: "x" });
    /// b.file("src/b.txt", { content: "y", mode: "100644" });
    /// const tree = b.write();
    /// ```
    #[napi]
    pub fn file(
        &mut self,
        path: String,
        content: Either<String, Buffer>,
        mode: Option<String>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let data: Vec<u8> = match &content {
            Either::A(s) => s.clone().into_bytes(),
            Either::B(b) => b.to_vec(),
        };
        let oid = r
            .write_blob(data)
            .map_err(|e| err("write_blob", e))?
            .detach();
        let kind = mode_to_entry_kind(mode.as_deref().unwrap_or(DEFAULT_ENTRY_MODE))?;
        self.log.push(Edit::Upsert { path, kind, oid });
        Ok(())
    }

    /// Look up the entry at `path` in the builder's current effective state: the last
    /// recorded edit targeting `path` (or removing one of its ancestors) wins; otherwise
    /// the entry is looked up in the base tree. Returns `null` when the effective state
    /// has no entry at `path`. This is a local view — the authoritative state is the
    /// tree returned by `write()`.
    #[napi]
    pub fn get(&self, path: String) -> napi::Result<Option<TreeEntry>> {
        let mut resolved: Option<Option<TreeEntry>> = None;
        for edit in &self.log {
            match edit {
                Edit::Upsert { path: p, kind, oid } if p == &path => {
                    let filename = p.rsplit('/').next().unwrap_or(p).to_string();
                    resolved = Some(Some(TreeEntry {
                        mode: mode_string(*kind),
                        filename,
                        id: oid.to_string(),
                    }));
                }
                Edit::Remove { path: p, .. } if p == &path => resolved = Some(None),
                // Removing an ancestor takes everything below it with it.
                Edit::Remove { path: p, .. }
                    if path.len() > p.len()
                        && path.starts_with(p.as_str())
                        && path.as_bytes()[p.len()] == b'/' =>
                {
                    resolved = Some(None)
                }
                _ => {}
            }
        }
        if let Some(entry) = resolved {
            return Ok(entry);
        }
        let Some(base) = self.base else {
            return Ok(None);
        };
        let r = thread_local(&self.repo);
        let base_tree = r.find_tree(base).map_err(|e| err("TreeBuilder.get", e))?;
        let entry = base_tree
            .lookup_entry_by_path(std::path::Path::new(&path))
            .map_err(|e| err("TreeBuilder.get", e))?;
        Ok(entry.map(|e| TreeEntry {
            mode: mode_string(e.mode().into()),
            filename: lossy(e.filename()),
            id: e.id().detach().to_string(),
        }))
    }

    /// Reset the builder to an empty-tree base and drop all recorded edits.
    #[napi]
    pub fn clear(&mut self) {
        self.base = None;
        self.log.clear();
    }

    /// Replay the recorded edits onto the base tree and write the resulting tree to the
    /// object database, returning it. By default the produced tree is validated first
    /// (legal filenames, all referenced objects exist); pass `{ validate: false }` to
    /// skip validation.
    #[napi]
    pub fn write(&self, options: Option<TreeBuilderWriteOptions>) -> napi::Result<Tree> {
        let validate = options.and_then(|o| o.validate).unwrap_or(true);
        let r = thread_local(&self.repo);
        let base = self
            .base
            .unwrap_or_else(|| gix::ObjectId::empty_tree(r.object_hash()));
        let oid = if validate {
            let mut editor = r
                .edit_tree(base)
                .map_err(|e| err("TreeBuilder.edit_tree", e))?;
            self.replay_facade(&r, &mut editor)?;
            editor
                .write()
                .map_err(|e| err("TreeBuilder.write", e))?
                .detach()
        } else {
            let mut editor = r
                .edit_tree(base)
                .map_err(|e| err("TreeBuilder.edit_tree", e))?
                .detach();
            self.replay_plumbing(&mut editor)?;
            // The plumbing editor performs no validation; write every changed tree
            // directly, mirroring git2's `TreeBuilder::write()`.
            editor.write(|tree| {
                r.write_object(tree)
                    .map(|id| id.detach())
                    .map_err(|e| err("TreeBuilder.write_object", e))
            })?
        };
        build_tree(&self.repo, oid)
    }
}

impl TreeBuilder {
    /// Replay the edit log onto the validating facade editor.
    fn replay_facade(
        &self,
        r: &gix::Repository,
        editor: &mut gix::object::tree::Editor<'_>,
    ) -> napi::Result<()> {
        for edit in &self.log {
            match edit {
                Edit::Upsert { path, kind, oid } => {
                    editor
                        .upsert(path.as_str(), *kind, *oid)
                        .map_err(|e| err("TreeBuilder.upsert", e))?;
                }
                Edit::Remove { path, mode } => match mode {
                    RemoveMode::Any => {
                        editor
                            .remove(path.as_str())
                            .map_err(|e| err("TreeBuilder.remove", e))?;
                    }
                    RemoveMode::Leaf => {
                        editor
                            .remove_leaf(path.as_str())
                            .map_err(|e| err("TreeBuilder.remove_leaf", e))?;
                    }
                    RemoveMode::IfLeaf => {
                        // The facade editor has no `remove_if_leaf()`; emulate it against
                        // the base tree (same approach as `Repository::edit_tree`).
                        let is_tree = base_entry_is_tree(r, self.base, path)?;
                        if is_tree == Some(false) {
                            editor
                                .remove(path.as_str())
                                .map_err(|e| err("TreeBuilder.remove", e))?;
                        }
                    }
                },
            }
        }
        Ok(())
    }

    /// Replay the edit log onto the unvalidated plumbing editor.
    fn replay_plumbing(&self, editor: &mut gix::objs::tree::Editor<'_>) -> napi::Result<()> {
        for edit in &self.log {
            match edit {
                Edit::Upsert { path, kind, oid } => {
                    editor
                        .upsert(components(path), *kind, *oid)
                        .map_err(|e| err("TreeBuilder.upsert", e))?;
                }
                Edit::Remove { path, mode } => match mode {
                    RemoveMode::Any => {
                        editor
                            .remove(components(path))
                            .map_err(|e| err("TreeBuilder.remove", e))?;
                    }
                    RemoveMode::Leaf => {
                        editor
                            .remove_leaf(components(path))
                            .map_err(|e| err("TreeBuilder.remove_leaf", e))?;
                    }
                    RemoveMode::IfLeaf => {
                        editor
                            .remove_if_leaf(components(path))
                            .map_err(|e| err("TreeBuilder.remove_if_leaf", e))?;
                    }
                },
            }
        }
        Ok(())
    }
}

/// Whether the base tree has an entry at `path` and whether it is a tree.
fn base_entry_is_tree(
    r: &gix::Repository,
    base: Option<gix::ObjectId>,
    path: &str,
) -> napi::Result<Option<bool>> {
    let Some(base) = base else {
        return Ok(None);
    };
    let base_tree = r
        .find_tree(base)
        .map_err(|e| err("TreeBuilder.find_tree", e))?;
    let entry = base_tree
        .lookup_entry_by_path(std::path::Path::new(path))
        .map_err(|e| err("TreeBuilder.remove_if_leaf", e))?;
    Ok(entry.map(|e| e.mode().is_tree()))
}

/// Split `path` into the components the plumbing editor expects.
fn components(path: &str) -> impl Iterator<Item = &gix::bstr::BStr> {
    path.split('/').map(|c| gix::bstr::BStr::new(c.as_bytes()))
}
