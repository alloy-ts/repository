use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use gix::bstr::ByteSlice;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;
use prolly::{
    BatchBuilder, BlobStore, Config as GenericConfig, DEFAULT_INLINE_VALUE_THRESHOLD,
    FileBlobStore, FileNodeStore, Prolly as Store, Tree as GenericTree, ValueRef,
};

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

/// Parse the `removeMode` string accepted by [`TreeBuilder::remove`].
fn parse_remove_mode(mode: &str) -> napi::Result<RemoveMode> {
    match mode {
        "any" => Ok(RemoveMode::Any),
        "leaf" => Ok(RemoveMode::Leaf),
        "ifLeaf" => Ok(RemoveMode::IfLeaf),
        other => Err(err_str(
            "TreeBuilder",
            format!("invalid removeMode {other}; expected \"any\", \"leaf\", or \"ifLeaf\""),
        )),
    }
}

/// The wire value of a [`RemoveMode`], as stored by [`encode_edit`].
fn remove_mode_str(mode: RemoveMode) -> &'static str {
    match mode {
        RemoveMode::Any => "any",
        RemoveMode::Leaf => "leaf",
        RemoveMode::IfLeaf => "ifLeaf",
    }
}

/// One recorded edit, keyed by its path in the builder's prolly index.
///
/// Repeated edits to the same path collapse into a single entry holding the
/// latest one, so a builder holding thousands of edits still replays in time
/// proportional to the number of *distinct* paths.
#[derive(Clone, Copy)]
enum Edit {
    Upsert {
        kind: gix::object::tree::EntryKind,
        oid: gix::ObjectId,
    },
    Remove {
        mode: RemoveMode,
    },
}

/// Directory (relative to the git dir) holding the prolly node store.
///
/// [`FileNodeStore`] shards content-addressed nodes below it
/// (`nodes/sha256/<aa>/<bb>/<cid>`), next to its `hints/` and `roots/` namespaces.
pub(crate) const PROLLY_STORE_DIR: &str = "prolly";

/// Open — creating on first use — the repository's prolly node store and the
/// tree handle driver over it.
pub(crate) fn open_prolly(
    repo: &Arc<gix::ThreadSafeRepository>,
) -> napi::Result<Store<FileNodeStore>> {
    let root = thread_local(repo).git_dir().join(PROLLY_STORE_DIR);
    let store = FileNodeStore::open(root).map_err(|e| err("prolly.FileNodeStore.open", e))?;
    Ok(Store::new(store, GenericConfig::default()))
}

/// Encode an edit as the value of its prolly index entry.
///
/// The layout is `"{seq} {tag} {payload}"`: `"7 u 100644 <oid>"` for upserts and
/// `"7 r ifLeaf"` for removals. `seq` is what makes last-write-wins well defined
/// in [`TreeBuilder::get`], where an edit can be shadowed by a later removal of
/// one of its ancestors.
fn encode_edit(seq: u64, edit: Edit) -> Vec<u8> {
    match edit {
        Edit::Upsert { kind, oid } => format!("{seq} u {} {oid}", mode_string(kind)).into_bytes(),
        Edit::Remove { mode } => format!("{seq} r {}", remove_mode_str(mode)).into_bytes(),
    }
}

/// Decode a value produced by [`encode_edit`] back into its sequence number and edit.
fn decode_edit(value: &[u8]) -> napi::Result<(u64, Edit)> {
    let text = std::str::from_utf8(value).map_err(|e| err("TreeBuilder.decode_edit", e))?;
    let mut parts = text.splitn(3, ' ');
    let (seq, tag, payload) = match (parts.next(), parts.next(), parts.next()) {
        (Some(seq), Some(tag), Some(payload)) => (seq, tag, payload),
        _ => {
            return Err(err_str(
                "TreeBuilder.decode_edit",
                format!("malformed edit record {text:?}"),
            ));
        }
    };
    let seq = seq
        .parse::<u64>()
        .map_err(|e| err("TreeBuilder.decode_edit", e))?;
    match tag {
        "u" => {
            let (mode, id) = payload.split_once(' ').ok_or_else(|| {
                err_str(
                    "TreeBuilder.decode_edit",
                    format!("malformed upsert record {text:?}"),
                )
            })?;
            Ok((
                seq,
                Edit::Upsert {
                    kind: mode_to_entry_kind(mode)?,
                    oid: parse_oid(id)?,
                },
            ))
        }
        "r" => Ok((
            seq,
            Edit::Remove {
                mode: parse_remove_mode(payload)?,
            },
        )),
        other => Err(err_str(
            "TreeBuilder.decode_edit",
            format!("unknown edit tag {other:?}"),
        )),
    }
}

/// Every strict ancestor path of `path`: `"a/b/c.txt"` yields `["a", "a/b"]`.
fn ancestor_paths(path: &str) -> Vec<&str> {
    path.match_indices('/')
        .map(|(idx, _)| &path[..idx])
        .collect()
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
/// Edits are buffered in a content-addressed prolly tree keyed by path — every
/// `upsert()`/`remove()` write derives a new root over the previous one's nodes, so
/// repeated edits keep their share of untouched nodes and [`TreeBuilder::get`] costs
/// `O(log n)` instead of scanning the whole edit history. The result is materialized
/// into git tree objects at `write()` time, so nothing reaches the object database
/// until then. Nodes are persisted under `<git_dir>/prolly` via [`FileNodeStore`].
///
/// Open one via `Tree.build(repo, baseTreeId?)` or `new TreeBuilder(repo, baseTreeId?)`.
#[napi]
pub struct TreeBuilder {
    repo: Arc<gix::ThreadSafeRepository>,
    /// Content-addressed node store driver for [`TreeBuilder::index`].
    prolly: Store<FileNodeStore>,
    /// The effective overlay: `path -> [`encode_edit`] payload`. Empty until the
    /// first edit; the base tree is never copied into it.
    index: GenericTree,
    base: Option<gix::ObjectId>,
    /// Monotonic counter stamped onto each edit so that later edits win.
    seq: u64,
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
        let prolly = open_prolly(&repo.inner)?;
        let index = prolly.create();
        Ok(TreeBuilder {
            repo: Arc::clone(&repo.inner),
            prolly,
            index,
            base,
            seq: 0,
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
        self.stage(path, Edit::Upsert { kind, oid })
    }

    /// Remove the entry at `path`. `removeMode` defaults to `"any"`:
    /// - `"any"`: remove the entry including any subtree below it (no error when absent)
    /// - `"leaf"`: error at `write()` when the entry at `path` is a tree
    /// - `"ifLeaf"`: leave an existing tree at `path` untouched
    #[napi]
    pub fn remove(&mut self, path: String, remove_mode: Option<String>) -> napi::Result<()> {
        let mode = parse_remove_mode(remove_mode.as_deref().unwrap_or("any"))?;
        self.stage(path, Edit::Remove { mode })
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
        self.stage(path, Edit::Upsert { kind, oid })
    }

    /// Look up the entry at `path` in the builder's current effective state: the last
    /// recorded edit targeting `path` (or removing one of its ancestors) wins; otherwise
    /// the entry is looked up in the base tree. Returns `null` when the effective state
    /// has no entry at `path`. This is a local view — the authoritative state is the
    /// tree returned by `write()`.
    #[napi]
    pub fn get(&self, path: String) -> napi::Result<Option<TreeEntry>> {
        // Last-write-wins over the exact path plus any ancestor removals: removing
        // a directory takes everything below it with it, but only if nothing was
        // staged for a descendant afterwards.
        let mut winner: Option<(u64, Edit)> = None;
        for key in std::iter::once(path.as_str()).chain(ancestor_paths(&path)) {
            let Some(value) = self
                .prolly
                .get(&self.index, key.as_bytes())
                .map_err(|e| err("TreeBuilder.get", e))?
            else {
                continue;
            };
            let candidate = decode_edit(&value)?;
            let is_exact = key.len() == path.len();
            let cascades = is_exact || matches!(candidate.1, Edit::Remove { .. });
            if cascades && winner.is_none_or(|(seq, _)| candidate.0 > seq) {
                winner = Some(candidate);
            }
        }
        if let Some((_, edit)) = winner {
            return Ok(match edit {
                Edit::Upsert { kind, oid } => Some(TreeEntry {
                    mode: mode_string(kind),
                    filename: path.rsplit('/').next().unwrap_or(&path).to_string(),
                    id: oid.to_string(),
                }),
                Edit::Remove { .. } => None,
            });
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
    ///
    /// The prolly index is replaced by a fresh empty root; nodes written for the
    /// discarded overlays become unreachable and stay in the node store.
    #[napi]
    pub fn clear(&mut self) {
        self.base = None;
        self.index = self.prolly.create();
        self.seq = 0;
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
    /// Record `edit` at `path`, deriving a new prolly root over the previous one.
    fn stage(&mut self, path: String, edit: Edit) -> napi::Result<()> {
        self.seq += 1;
        let value = encode_edit(self.seq, edit);
        self.index = self
            .prolly
            .put(&self.index, path.into_bytes(), value)
            .map_err(|e| err("TreeBuilder.put", e))?;
        Ok(())
    }

    /// Every edit in the overlay, in path order (prolly range order — which is also
    /// git tree order, so replays are deterministic regardless of insertion order).
    fn edits(&self) -> napi::Result<Vec<(String, Edit)>> {
        let mut out = Vec::new();
        for item in self
            .prolly
            .range(&self.index, b"", None)
            .map_err(|e| err("TreeBuilder.range", e))?
        {
            let (path, value) = item.map_err(|e| err("TreeBuilder.range", e))?;
            let (_, edit) = decode_edit(&value)?;
            out.push((String::from_utf8_lossy(&path).into_owned(), edit));
        }
        Ok(out)
    }

    /// Replay the overlay onto the validating facade editor.
    fn replay_facade(
        &self,
        r: &gix::Repository,
        editor: &mut gix::object::tree::Editor<'_>,
    ) -> napi::Result<()> {
        for (path, edit) in self.edits()? {
            match edit {
                Edit::Upsert { kind, oid } => {
                    editor
                        .upsert(path.as_str(), kind, oid)
                        .map_err(|e| err("TreeBuilder.upsert", e))?;
                }
                Edit::Remove { mode } => match mode {
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
                        let is_tree = base_entry_is_tree(r, self.base, &path)?;
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

    /// Replay the overlay onto the unvalidated plumbing editor.
    fn replay_plumbing(&self, editor: &mut gix::objs::tree::Editor<'_>) -> napi::Result<()> {
        for (path, edit) in self.edits()? {
            match edit {
                Edit::Upsert { kind, oid } => {
                    editor
                        .upsert(components(&path), kind, oid)
                        .map_err(|e| err("TreeBuilder.upsert", e))?;
                }
                Edit::Remove { mode } => match mode {
                    RemoveMode::Any => {
                        editor
                            .remove(components(&path))
                            .map_err(|e| err("TreeBuilder.remove", e))?;
                    }
                    RemoveMode::Leaf => {
                        editor
                            .remove_leaf(components(&path))
                            .map_err(|e| err("TreeBuilder.remove_leaf", e))?;
                    }
                    RemoveMode::IfLeaf => {
                        editor
                            .remove_if_leaf(components(&path))
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

/// Map a git object kind to the single-byte tag used in canonical prolly tree keys:
/// commits `1`, trees `2`, blobs `3`, tags `4`.
pub(crate) fn type_tag_of(kind: gix::object::Kind) -> u8 {
    match kind {
        gix::object::Kind::Commit => 1,
        gix::object::Kind::Tree => 2,
        gix::object::Kind::Blob => 3,
        gix::object::Kind::Tag => 4,
    }
}

/// Raw canonical bytes of a git object — exactly the bytes its object id hashes
/// to, so they are already a deterministic normal form.
fn object_raw(object: &gix::Object) -> Vec<u8> {
    object.data.to_vec()
}

/// Encode an object's canonical bytes as a prolly leaf value.
///
/// Small objects are stored inline; larger ones are offloaded to the
/// content-addressed [`FileBlobStore`] and referenced by blob id. Both encodings
/// are deterministic, so identical content always yields an identical leaf value
/// and therefore an identical tree root.
fn prolly_value_for(store: &FileBlobStore, raw: &[u8]) -> napi::Result<Vec<u8>> {
    if raw.len() <= DEFAULT_INLINE_VALUE_THRESHOLD {
        Ok(ValueRef::Inline(raw.to_vec()).to_bytes())
    } else {
        let reference = store
            .put_blob(raw)
            .map_err(|e| err("prolly.blob_store", e))?;
        Ok(ValueRef::Blob(reference).to_bytes())
    }
}

/// Open — creating on first use — the repository's prolly node store.
///
/// Unlike [`open_prolly`], this takes a repository view rather than a shared
/// handle, so it is usable from any code already holding a `gix::Repository`.
pub(crate) fn open_prolly_nodes(r: &gix::Repository) -> napi::Result<FileNodeStore> {
    FileNodeStore::open(r.git_dir().join(PROLLY_STORE_DIR))
        .map_err(|e| err("prolly.FileNodeStore.open", e))
}

/// Open — creating on first use — the blob store that backs large-object
/// offloading for the canonical prolly store.
///
/// It shares the `prolly` root with the node store: blobs land below
/// `blobs/sha256/…` while nodes land below `nodes/sha256/…`, so the namespaces
/// stay disjoint.
pub(crate) fn open_prolly_blobs(r: &gix::Repository) -> napi::Result<FileBlobStore> {
    FileBlobStore::open(r.git_dir().join(PROLLY_STORE_DIR))
        .map_err(|e| err("prolly.FileBlobStore.open", e))
}

/// Hex content id of a built prolly tree; empty when the tree has no entries.
fn prolly_root_hex(tree: &GenericTree) -> String {
    match &tree.root {
        Some(cid) => hex::encode(cid.as_bytes()),
        None => String::new(),
    }
}

/// Result of building a canonical prolly tree over a repository's object graph.
#[napi(object)]
pub struct TreeStore {
    /// Content id (hex) of the top-level prolly tree — the canonical fingerprint
    /// of the mirrored content. Identical content always yields an identical root.
    pub root: String,
    /// Directory holding the content-addressed prolly node store.
    pub node_store: String,
    /// Directory holding offloaded (large) object blobs.
    pub blob_store: String,
    /// Number of commit objects mirrored.
    pub commits: u32,
    /// Number of tree objects mirrored.
    pub trees: u32,
    /// Number of blob objects mirrored.
    pub blobs: u32,
    /// Number of tag objects mirrored.
    pub tags: u32,
}

/// Result of building a live prolly tree from a single git tree.
///
/// Unlike [`TreeStore`], this mirrors exactly one tree (not a commit DAG), so
/// it reports only the resulting tree's content id and the store it lives in.
#[napi(object)]
pub struct GenericTreeRef {
    /// Content id (hex) of the built prolly tree — identical tree content always
    /// yields an identical root.
    pub root: String,
    /// Directory holding the content-addressed prolly node store.
    pub node_store: String,
}

/// Mirror every object reachable from `tips` — commits, trees, blobs and
/// annotated tags — into the repository's canonical prolly node store.
///
/// Each object is stored under the key `[type_tag][oid]` with its own canonical
/// bytes as the value; `extra` key/value pairs (e.g. a reference table) are
/// published into the same tree. [`BatchBuilder`] sorts and deduplicates its
/// input before building, so the published root is independent of traversal and
/// insertion order.
pub(crate) fn prolly_mirror_objects(
    r: &gix::Repository,
    tips: &[gix::ObjectId],
    extra: &[(Vec<u8>, Vec<u8>)],
) -> napi::Result<TreeStore> {
    let node_store = open_prolly_nodes(r)?;
    let blob_store = open_prolly_blobs(r)?;
    let mut builder = BatchBuilder::new(node_store, GenericConfig::default());
    for (key, value) in extra {
        builder.add(key.clone(), value.clone());
    }

    let mut seen: HashSet<gix::ObjectId> = HashSet::new();
    let mut queue: VecDeque<gix::ObjectId> = tips.iter().copied().collect();
    let mut counts = (0u32, 0u32, 0u32, 0u32); // commits, trees, blobs, tags

    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let object = r.find_object(id).map_err(|e| err("find_object", e))?;
        let raw = object_raw(&object);
        let mut key = Vec::with_capacity(1 + id.as_bytes().len());
        key.push(type_tag_of(object.kind));
        key.extend_from_slice(id.as_bytes());
        builder.add(key, prolly_value_for(&blob_store, &raw)?);

        match object.kind {
            gix::object::Kind::Commit => {
                counts.0 += 1;
                let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
                let decoded = commit.decode().map_err(|e| err("decode_commit", e))?;
                queue.push_back(decoded.tree());
                for parent in decoded.parents() {
                    queue.push_back(parent);
                }
            }
            gix::object::Kind::Tree => {
                counts.1 += 1;
                let tree = r.find_tree(id).map_err(|e| err("find_tree", e))?;
                let decoded = tree.decode().map_err(|e| err("decode_tree", e))?;
                for entry in &decoded.entries {
                    // Gitlinks point at objects in another repository; skip them.
                    if !entry.mode.is_commit() {
                        queue.push_back(entry.oid.into());
                    }
                }
            }
            gix::object::Kind::Blob => {
                counts.2 += 1;
            }
            gix::object::Kind::Tag => {
                counts.3 += 1;
                let tag = r.find_tag(id).map_err(|e| err("find_tag", e))?;
                let decoded = tag.decode().map_err(|e| err("decode_tag", e))?;
                queue.push_back(decoded.target());
            }
        }
    }

    let tree = builder.build().map_err(|e| err("prolly.build", e))?;
    let git_dir = r.git_dir().to_path_buf();
    Ok(TreeStore {
        root: prolly_root_hex(&tree),
        node_store: git_dir
            .join(PROLLY_STORE_DIR)
            .to_string_lossy()
            .into_owned(),
        blob_store: git_dir
            .join(PROLLY_STORE_DIR)
            .join("blobs")
            .to_string_lossy()
            .into_owned(),
        commits: counts.0,
        trees: counts.1,
        blobs: counts.2,
        tags: counts.3,
    })
}

/// Flatten `tree_id` into `path -> (kind, oid)`, recursing into subtrees.
///
/// This is the single source of truth for the "flatten a git tree to leaf
/// entries" step shared by the canonical prolly export ([`prolly_canonicalise_tree`])
/// and the prolly tree merge in `merge.rs` ([`crate::merge::collect_flat_entries`]).
/// Subtrees are elided (implied by their leaves); gitlinks are kept as leaf
/// entries. Keys are ordered by path, which is what makes a downstream prolly
/// tree canonical regardless of creation or traversal order.
pub(crate) fn flatten_tree(
    r: &gix::Repository,
    tree_id: gix::ObjectId,
) -> napi::Result<BTreeMap<String, (gix::object::tree::EntryKind, gix::ObjectId)>> {
    let mut map = BTreeMap::new();
    fn recurse(
        r: &gix::Repository,
        map: &mut BTreeMap<String, (gix::object::tree::EntryKind, gix::ObjectId)>,
        prefix: &str,
        tree: &gix::Tree,
    ) -> napi::Result<()> {
        for entry in tree.iter() {
            let entry = entry.map_err(|e| err("flatten_tree", e))?;
            let name = entry.filename().to_string();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = mode_to_entry_kind(&mode_string(entry.mode().into()))?;
            match kind {
                gix::object::tree::EntryKind::Tree => {
                    let sub = r.find_tree(entry.id()).map_err(|e| err("find_tree", e))?;
                    recurse(r, map, &path, &sub)?;
                }
                _ => {
                    map.insert(path, (kind, entry.id().detach()));
                }
            }
        }
        Ok(())
    }
    let tree = r.find_tree(tree_id).map_err(|e| err("find_tree", e))?;
    recurse(r, &mut map, "", &tree)?;
    Ok(map)
}

/// Canonicalise (normalise) a git tree into the repository's prolly node store.
///
/// The tree is flattened to its leaf entries as `path -> "mode oid"` (via
/// [`flatten_tree`]), sorted and deduplicated by path, and published as a prolly
/// tree. Subtree entries are implied by their leaves, so the flat leaf set is the
/// normal form: two trees with identical content produce identical roots
/// regardless of how their entries were created or ordered.
///
/// Returns the hex content id of the canonical root (empty for an empty tree).
pub(crate) fn prolly_canonicalise_tree(
    r: &gix::Repository,
    tree_id: gix::ObjectId,
) -> napi::Result<String> {
    let flat = flatten_tree(r, tree_id)?;
    if flat.is_empty() {
        return Ok(String::new());
    }

    let node_store = open_prolly_nodes(r)?;
    let mut builder = BatchBuilder::new(node_store, GenericConfig::default());
    for (path, (kind, id)) in &flat {
        builder.add(
            path.clone().into_bytes(),
            format!("{} {}", mode_string(*kind), id).into_bytes(),
        );
    }
    let tree = builder.build().map_err(|e| err("prolly.build", e))?;
    Ok(prolly_root_hex(&tree))
}

/// Build a live prolly tree by inserting `path -> value` pairs in order.
///
/// Shared by the prolly tree merge (`merge.rs`) and any live-tree builder, so the
/// "insert a flattened entry set into a `Store`" step lives in exactly one place.
/// `what` labels any store error from this tree. Returns the resulting live tree.
pub(crate) fn build_prolly_tree_values(
    prolly: &Store<FileNodeStore>,
    entries: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    what: &str,
) -> napi::Result<GenericTree> {
    let mut tree = prolly.create();
    for (path, value) in entries {
        tree = prolly.put(&tree, path, value).map_err(|e| err(what, e))?;
    }
    Ok(tree)
}

/// Build a live prolly tree mirroring `tree_id`, flattening it via
/// [`flatten_tree`] and inserting each leaf as `path -> "mode oid"`.
///
/// Unlike [`prolly_canonicalise_tree`] (which returns only the immutable root
/// hash via a [`BatchBuilder`]), this returns a live [`GenericTree`] handle — the
/// form the merge resolver needs.
pub(crate) fn build_prolly_tree_from_tree(
    r: &gix::Repository,
    prolly: &Store<FileNodeStore>,
    tree_id: gix::ObjectId,
    what: &str,
) -> napi::Result<GenericTree> {
    let flat = flatten_tree(r, tree_id)?;
    let pairs = flat.into_iter().map(|(path, (kind, id))| {
        (
            path.into_bytes(),
            format!("{} {}", mode_string(kind), id).into_bytes(),
        )
    });
    build_prolly_tree_values(prolly, pairs, what)
}

#[napi]
impl Tree {
    /// Canonicalise (normalise) this tree into the repository's prolly node
    /// store.
    ///
    /// Leaf entries are flattened to `path -> "mode oid"`, sorted, and published
    /// as a content-addressed prolly tree; the return value is the hex content id
    /// of the canonical root. Identical tree content always yields an identical
    /// root, no matter how the entries were created or ordered.
    #[napi]
    pub fn canonicalise(&self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        prolly_canonicalise_tree(&r, self.id)
    }
}

#[napi]
impl Repository {
    /// Build a canonical prolly tree from `commitId` containing the entire
    /// commit DAG reachable from it: every commit (with its metadata — parents,
    /// author, committer, message), tree and blob, plus any annotated tags
    /// encountered, mirrored into the repository's content-addressed prolly node
    /// store.
    ///
    /// `root` is the content id of the top-level prolly tree and is reproducible:
    /// the same commit DAG always yields the same root. See
    /// [`export_prolly`](crate::export::export_prolly) for the ref-aware variant.
    #[napi]
    pub fn prolly_tree_from_commit(&self, commit_id: String) -> napi::Result<TreeStore> {
        let oid = parse_oid(&commit_id)?;
        let r = thread_local(&self.inner);
        let object = r.find_object(oid).map_err(|e| err("find_object", e))?;
        if object.kind != gix::object::Kind::Commit {
            return Err(err_str(
                "prollyTreeFromCommit",
                format!("{commit_id} is not a commit"),
            ));
        }
        prolly_mirror_objects(&r, &[oid], &[])
    }

    /// Build a live prolly tree from `treeId` — the flattened leaves of that single
    /// git tree, inserted as `path -> "mode oid"` into the repository's prolly node
    /// store.
    ///
    /// This is the live-tree counterpart of [`Tree::canonicalise`]: it returns a
    /// [`GenericTreeRef`] (root + store) rather than mirroring a whole commit DAG.
    /// It reuses the same [`flatten_tree`] + [`build_prolly_tree_from_tree`]
    /// primitives the prolly tree merge uses, so all three paths stay consistent.
    #[napi]
    pub fn prolly_tree_from_tree(&self, tree_id: String) -> napi::Result<GenericTreeRef> {
        let oid = parse_oid(&tree_id)?;
        let r = thread_local(&self.inner);
        let object = r.find_object(oid).map_err(|e| err("find_object", e))?;
        if object.kind != gix::object::Kind::Tree {
            return Err(err_str(
                "prollyTreeFromTree",
                format!("{tree_id} is not a tree"),
            ));
        }
        let prolly = open_prolly(&self.inner)?;
        let tree = build_prolly_tree_from_tree(&r, &prolly, oid, "prollyTreeFromTree")?;
        let git_dir = r.git_dir().to_path_buf();
        Ok(GenericTreeRef {
            root: prolly_root_hex(&tree),
            node_store: git_dir
                .join(PROLLY_STORE_DIR)
                .to_string_lossy()
                .into_owned(),
        })
    }
}
