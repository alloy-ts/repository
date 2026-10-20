use std::sync::Arc;

use napi::Result;
use napi::bindgen_prelude::{Buffer, ClassInstance, Either};
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::{Object, build_object};
use crate::replace::{ReplaceOptions, ReplaceResult};
use crate::repository::Repository;
use crate::tree::{Tree, build_tree};

/// Convert a git tree entry kind into its canonical mode string.
pub(crate) fn mode_string(kind: gix::object::tree::EntryKind) -> String {
    match kind {
        gix::object::tree::EntryKind::Blob => "100644".to_string(),
        gix::object::tree::EntryKind::BlobExecutable => "100755".to_string(),
        gix::object::tree::EntryKind::Tree => "040000".to_string(),
        gix::object::tree::EntryKind::Link => "120000".to_string(),
        gix::object::tree::EntryKind::Commit => "160000".to_string(),
    }
}

/// Parse a mode string into a git tree entry kind, rejecting anything else.
pub(crate) fn mode_to_entry_kind(mode: &str) -> Result<gix::objs::tree::EntryKind> {
    match mode {
        "100644" => Ok(gix::objs::tree::EntryKind::Blob),
        "100755" => Ok(gix::objs::tree::EntryKind::BlobExecutable),
        "040000" | "40000" => Ok(gix::objs::tree::EntryKind::Tree),
        "120000" => Ok(gix::objs::tree::EntryKind::Link),
        "160000" => Ok(gix::objs::tree::EntryKind::Commit),
        other => Err(err_str("tree entry", format!("invalid file mode {other}"))),
    }
}

/// The default entry mode: a regular, non-executable file.
pub(crate) const DEFAULT_ENTRY_MODE: &str = "100644";

/// Options for constructing an [`Entry`].
///
/// Every field is optional except `content`, which is written as the entry's blob.
/// `repo` may be omitted, in which case the repository is resolved from the
/// environment (`GIT_DIR`, then `REPO_DIR`, then the current directory).
///
/// Note this type is deliberately *rich* — it can carry a `Buffer` and a
/// `Repository` handle — and is therefore not `Send`, so it cannot cross the
/// thread pool. Describing entries that already exist (e.g. worktree stream
/// results, which are produced asynchronously) uses the plain, `Send`-safe
/// `StreamEntry` descriptor instead.
#[napi(object)]
pub struct EntryOpts {
    /// The entry content, written to the object database as a blob. Accepts a
    /// string (e.g. `JSON.stringify({ name: "Entry" })`) or an ArrayBuffer.
    pub content: Option<Either<String, Buffer>>,
    /// The entry path / name, relative to the repository. Defaults to
    /// `objects/<content-hash>.json`, making the entry content-addressed —
    /// see [`Entry::new`] for the trade-offs.
    pub path: Option<String>,
    /// The entry mode. Defaults to `100644`; see [`EntryMode`] / `Entry.mode()`.
    pub mode: Option<String>,
    /// The repository to write into. Omit to resolve it from the environment.
    pub repo: Option<ClassInstance<'static, Repository>>,
}

/// The well-known git entry modes, as a constant-value string enum:
/// `EntryMode.Blob === "100644"`, `EntryMode.Tree === "040000"`, and so on.
///
/// These are plain strings at runtime, so any of them can be passed wherever a
/// mode string is accepted (e.g. `new Entry({ mode: EntryMode.Blob })`).
#[napi(string_enum)]
pub enum EntryMode {
    /// Regular, non-executable file.
    #[napi(value = "100644")]
    Blob,
    /// Executable file.
    #[napi(value = "100755")]
    BlobExecutable,
    /// Directory (subtree).
    #[napi(value = "040000")]
    Tree,
    /// Symbolic link.
    #[napi(value = "120000")]
    Link,
    /// Gitlink (submodule commit).
    #[napi(value = "160000")]
    Commit,
}

impl EntryMode {
    /// The git mode string this variant represents.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            EntryMode::Blob => "100644",
            EntryMode::BlobExecutable => "100755",
            EntryMode::Tree => "040000",
            EntryMode::Link => "120000",
            EntryMode::Commit => "160000",
        }
    }
}

/// Every well-known git entry mode at once, returned by `Entry.mode()`.
#[napi(object)]
pub struct Mode {
    /// Regular, non-executable file.
    pub blob: String,
    /// Executable file.
    pub blob_executable: String,
    /// Directory (subtree).
    pub tree: String,
    /// Symbolic link.
    pub link: String,
    /// Gitlink (submodule commit).
    pub commit: String,
}

/// A single record of a git tree: a `{ mode, filename, id }` triple bound to a
/// repository (unless detached) and, once resolved through a tree, to that tree.
#[napi]
pub struct Entry {
    /// `None` for detached entries ([`Entry::detached`]) that carry no repository binding.
    pub(crate) repo: Option<Arc<gix::ThreadSafeRepository>>,
    /// `None` for entries created via `new Entry(...)` that are not yet attached to a tree.
    pub(crate) tree_id: Option<gix::ObjectId>,
    pub(crate) mode: String,
    pub(crate) filename: String,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Entry {
    /// Create a new entry by writing `opts.content` into the object database as a
    /// blob and describing it as `{ mode, path, id }`.
    ///
    /// `opts.path` defaults to `objects/<content-hash>.json`, making entries
    /// content-addressed: identical content always yields the same path, and the
    /// same set of entries always yields the same tree. The trade-off is that any
    /// edit changes the hash and therefore the path, so history shows a delete plus
    /// an add rather than a modification — pass an explicit `path` when entries have
    /// a stable logical identity you want to track across edits.
    ///
    /// `opts.mode` defaults to `100644`. `opts.repo` may be omitted, in which case
    /// the repository is resolved from the environment.
    ///
    /// The entry is not attached to any tree yet, so `treeId` is `null`. Nest it with
    /// `new Tree([entry])` / `Tree.fromEntries([entry])`, which create intermediate
    /// trees for paths containing `/`.
    #[napi(constructor)]
    pub fn new(opts: EntryOpts) -> Result<Self> {
        let inner = match opts.repo.as_ref() {
            Some(r) => Arc::clone(&r.inner),
            None => crate::repository::current_repo()?,
        };
        let r = thread_local(&inner);
        let data: Vec<u8> = match opts.content {
            Some(Either::A(s)) => s.into_bytes(),
            Some(Either::B(b)) => b.to_vec(),
            None => return Err(err_str("Entry", "creating an entry requires `content`")),
        };
        let id = r
            .write_blob(data)
            .map_err(|e| err("write_blob", e))?
            .detach();
        Ok(Entry {
            repo: Some(inner),
            tree_id: None,
            mode: opts.mode.unwrap_or_else(|| DEFAULT_ENTRY_MODE.to_string()),
            filename: opts.path.unwrap_or_else(|| format!("objects/{id}.json")),
            id,
        })
    }

    /// The well-known git entry modes, as
    /// `{ blob: "100644", blobExecutable: "100755", tree: "040000",
    /// link: "120000", commit: "160000" }`.
    ///
    /// Exposed as `Entry.Mode()`; instances keep their own `entry.mode` getter.
    /// The same values are available individually through the [`EntryMode`] enum.
    #[napi]
    pub fn Mode() -> Mode {
        Mode {
            blob: DEFAULT_ENTRY_MODE.to_string(),
            blob_executable: EntryMode::BlobExecutable.as_str().to_string(),
            tree: EntryMode::Tree.as_str().to_string(),
            link: EntryMode::Link.as_str().to_string(),
            commit: EntryMode::Commit.as_str().to_string(),
        }
    }

    /// Create a detached, repository-less entry from raw components (`mode`, `filename`,
    /// `id`) without touching any repository or writing any object.
    ///
    /// Useful to hand-build entry sets; such entries can be nested into a tree via
    /// `new Tree([...])` / `Tree.fromEntries([...])` as long as at least one other entry
    /// carries a repository. Repository-backed operations (`asTree`, `replace`) throw
    /// on a detached entry.
    #[napi(factory)]
    pub fn detached(mode: String, filename: String, id: String) -> Result<Self> {
        Ok(Entry {
            repo: None,
            tree_id: None,
            mode,
            filename,
            id: parse_oid(&id)?,
        })
    }

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

    /// The id of the tree this entry was resolved from, or `null` for entries created via
    /// `new Entry(...)` that are not yet attached to a tree.
    #[napi(getter)]
    pub fn tree_id(&self) -> Option<String> {
        self.tree_id.map(|id| id.to_string())
    }

    /// Resolve this entry's target object as an [`Object`], giving it access to
    /// header/data/peel/`replace`, plus the `as_commit()` / `as_tree()` / `as_blob()` /
    /// `as_tag()` downcasts shared by every object type.
    ///
    /// Throws for a detached entry ([`Entry::detached`]), which has no repository to
    /// resolve against.
    #[napi]
    pub fn object(&self) -> Result<Object> {
        let repo = self.require_repo("object")?;
        build_object(repo, self.id)
    }

    /// Whether this entry is a directory (subtree).
    #[napi]
    pub fn is_tree(&self) -> bool {
        self.mode == "040000"
    }

    /// If this entry is a directory, resolve it as a [`Tree`]; otherwise `null`.
    ///
    /// Throws for a detached entry ([`Entry::detached`]), which has no repository to
    /// resolve against.
    #[napi]
    pub fn as_tree(&self) -> Result<Option<Tree>> {
        if !self.is_tree() {
            return Ok(None);
        }
        let repo = self.require_repo("as_tree")?;
        build_tree(repo, self.id).map(Some)
    }

    /// Replace this entry's target object, recording a new revision in the versioned
    /// history. This is exactly the repository-level [`Repository::replace`] API, with
    /// this entry's object id supplied automatically.
    ///
    /// Because the entry is by definition committed in a tree, the resulting manifest's
    /// `type` is `"entry"` (and its `path` is the entry's location in the tree).
    ///
    /// Throws if a `refs/replace/<oid>` already exists for this object and
    /// `options.force` is unset (use [`Repository::try_replace`] for a non-throwing call).
    ///
    /// Throws for a detached entry ([`Entry::detached`]), which has no repository to
    /// write the replacement into.
    #[napi]
    pub fn replace(
        &self,
        content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> Result<ReplaceResult> {
        let repo = Repository {
            inner: Arc::clone(self.require_repo("replace")?),
        };
        repo.replace(self.id.to_string(), content, options)
    }

    /// Return this entry's repository handle, throwing if the entry is detached.
    fn require_repo(&self, what: &str) -> Result<&Arc<gix::ThreadSafeRepository>> {
        self.repo.as_ref().ok_or_else(|| {
            err_str(
                what,
                "entry is detached (created via Entry.detached) and has no repository",
            )
        })
    }
}

/// Construct an [`Entry`] bound to `repo` and, when resolved through a tree, to that
/// tree's id. Used by [`Tree`] when materializing its records.
pub(crate) fn build_entry(
    repo: &Arc<gix::ThreadSafeRepository>,
    tree_id: Option<gix::ObjectId>,
    kind: gix::object::tree::EntryKind,
    filename: String,
    id: gix::ObjectId,
) -> Entry {
    Entry {
        repo: Some(Arc::clone(repo)),
        tree_id,
        mode: mode_string(kind),
        filename,
        id,
    }
}
