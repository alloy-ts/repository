use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use gix::objs::Write;

use crate::error::{cached_namespace_get, cached_namespace_set, err, parse_oid, thread_local};
use crate::object::{
    Blob, GitObject, Signature, Tag, Tree, build_blob, build_object, build_tag, build_tree,
};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn kind_to_string(kind: gix::object::Kind) -> String {
    match kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
    .to_string()
}

fn string_to_kind(s: &str) -> Result<gix::object::Kind> {
    match s.to_lowercase().as_str() {
        "commit" => Ok(gix::object::Kind::Commit),
        "tree" => Ok(gix::object::Kind::Tree),
        "blob" => Ok(gix::object::Kind::Blob),
        "tag" => Ok(gix::object::Kind::Tag),
        other => Err(err("write_object", format!("invalid object kind {other}"))),
    }
}

fn mode_to_entry_kind(mode: &str) -> Result<gix::objs::tree::EntryKind> {
    match mode {
        "100755" => Ok(gix::objs::tree::EntryKind::BlobExecutable),
        "040000" | "40000" => Ok(gix::objs::tree::EntryKind::Tree),
        "120000" => Ok(gix::objs::tree::EntryKind::Link),
        "160000" => Ok(gix::objs::tree::EntryKind::Commit),
        _ => Ok(gix::objs::tree::EntryKind::Blob),
    }
}

/// An object header: its kind and the size of its encoded data in bytes.
#[napi(object)]
pub struct ObjectHeader {
    pub kind: String,
    pub size: f64,
}

/// Locate the system `git` binary by probing well-known absolute paths, falling back to a
/// `PATH` lookup. Returns `None` only if git cannot be found anywhere.
fn find_git() -> Option<std::path::PathBuf> {
    const CANDIDATES: &[&str] = &[
        "/usr/bin/git",
        "/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
        "/opt/homebrew/opt/git/bin/git",
        "/usr/local/opt/git/bin/git",
    ];
    for c in CANDIDATES {
        if std::path::Path::new(c).is_file() {
            return Some(std::path::PathBuf::from(c));
        }
    }
    if let Ok(out) = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(std::path::PathBuf::from(p));
            }
        }
    }
    None
}

pub(crate) fn signature_from_dto(s: &Signature) -> Result<gix::actor::Signature> {
    Ok(gix::actor::Signature {
        name: s.name.clone().into(),
        email: s.email.clone().into(),
        time: gix::date::Time {
            seconds: s.time,
            offset: s.timezone_offset,
        },
    })
}

/// Input for writing an annotated tag object.
#[derive(Clone)]
#[napi(object)]
pub struct TagInput {
    pub name: String,
    pub target: String,
    pub target_kind: String,
    pub message: String,
    pub tagger: Option<Signature>,
}

/// A single entry in the git index.
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: u32,
}

/// A single status item for working tree / index.
#[derive(Clone)]
#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
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
}

/// A single blame hunk mapping a range of lines in the blamed file to the commit that introduced it.
#[derive(Clone)]
#[napi(object)]
pub struct BlameEntry {
    /// 0-based line index where this hunk begins in the blamed file.
    pub start_in_blamed_file: u32,
    /// 0-based line index where this hunk begins in the source file (may differ if the file moved).
    pub start_in_source_file: u32,
    /// Number of lines this hunk spans.
    pub len: u32,
    /// The id of the commit that introduced this hunk.
    pub commit_id: String,
    /// The source file name, if it differs from the blamed file (renamed file).
    pub source_file_name: Option<String>,
}

/// The result of blaming a file.
#[derive(Clone)]
#[napi(object)]
pub struct BlameOutcome {
    /// The blame hunks, in file order.
    pub entries: Vec<BlameEntry>,
    /// The full content of the blamed file (as it exists at the start commit).
    pub blob: Vec<u8>,
}

/// A git attribute assignment entry.
#[derive(Clone)]
#[napi(object)]
pub struct AttributeEntry {
    pub name: String,
    pub value: String,
}

/// Information about a git submodule.
/// A git note attached to an object.
#[derive(Clone)]
#[napi(object)]
pub struct NoteInfo {
    pub id: String,
    pub message: String,
}

/// Input structure for editing an index entry.
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntryInput {
    pub path: String,
    pub id: String,
    pub mode: Option<String>,
}

/// Checkout task running on libuv thread pool.
pub struct CheckoutTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target: String,
}

impl Task for CheckoutTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<String> {
        let r = thread_local(&self.repo);
        let oid = parse_oid(&self.target)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?;

        let label = format!("checkout {}", self.target);
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("checkout_head", e))?;

        let mut file = r
            .index_from_tree(&tree_id)
            .map_err(|e| err("index_from_tree", e))?;
        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;

        Ok(oid.to_string())
    }

    fn resolve(&mut self, _env: napi::Env, output: String) -> Result<String> {
        Ok(output)
    }
}

/// Status computation task running on libuv thread pool.
pub struct StatusTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for StatusTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.repo);
        let platform = r
            .status(gix::progress::Discard)
            .map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = platform
            .into_index_worktree_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|e| err("status iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path());
            let status = format!("{item:?}");
            out.push(StatusEntry { path, status });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<StatusEntry>) -> Result<Vec<StatusEntry>> {
        Ok(output)
    }
}

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
pub struct CloneTask {
    url: String,
    path: String,
    bare: bool,
}

impl Task for CloneTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> Result<Repository> {
        if std::env::var_os("GIT_COMMITTER_NAME").is_none() {
            unsafe { std::env::set_var("GIT_COMMITTER_NAME", "gitoxide") };
        }
        if std::env::var_os("GIT_COMMITTER_EMAIL").is_none() {
            unsafe { std::env::set_var("GIT_COMMITTER_EMAIL", "gitoxide@localhost") };
        }
        if std::env::var_os("GIT_AUTHOR_NAME").is_none() {
            unsafe { std::env::set_var("GIT_AUTHOR_NAME", "gitoxide") };
        }
        if std::env::var_os("GIT_AUTHOR_EMAIL").is_none() {
            unsafe { std::env::set_var("GIT_AUTHOR_EMAIL", "gitoxide@localhost") };
        }

        let url = self.url.clone();
        let path = self.path.clone();
        let bare = self.bare;

        // Try a gix-native clone first.
        let gix_repo = (|| {
            let mut prepare = if bare {
                gix::prepare_clone_bare(url.as_str(), &path)
            } else {
                gix::prepare_clone(url.as_str(), &path)
            }
            .ok()?;
            let interrupt = AtomicBool::new(false);
            let (repo, _outcome) = prepare
                .fetch_only(gix::progress::Discard, &interrupt)
                .ok()?;
            Some(repo.into())
        })();

        if let Some(repo) = gix_repo {
            return Ok(Repository {
                inner: Arc::new(repo),
            });
        }

        // Fallback: shell out to the system `git` binary. This is robust to environments
        // where git is not on PATH (we probe well-known absolute locations) and to gix
        // ref-update edge cases on local clones.
        if let Some(git) = find_git() {
            let _ = std::fs::remove_dir_all(&path);
            let mut cmd = std::process::Command::new(&git);
            cmd.arg("clone").arg("--local");
            if bare {
                cmd.arg("--bare");
            }
            cmd.arg(&url).arg(&path);
            match cmd.output() {
                Ok(out) if out.status.success() => {
                    let ts = gix::ThreadSafeRepository::open(&path)
                        .map_err(|e| napi::Error::from_reason(format!("open: {e}")))?;
                    return Ok(Repository {
                        inner: Arc::new(ts),
                    });
                }
                Ok(out) => {
                    return Err(napi::Error::from_reason(format!(
                        "git clone failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
                Err(e) => {
                    return Err(napi::Error::from_reason(format!(
                        "failed to spawn git at {}: {e}",
                        git.display()
                    )));
                }
            }
        }

        Err(napi::Error::from_reason(
            "clone failed (gix unavailable) and no system git binary was found",
        ))
    }

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}

/// The primary handle to a git repository, exposed to TypeScript as a class.
///
/// Internally wraps `Arc<gix::ThreadSafeRepository>` so it can be shared and offloaded to worker
/// threads for async network operations, while sync methods obtain a cheap thread-local view.
#[napi]
pub struct Repository {
    pub(crate) inner: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Repository {
    // ---- construction (static factory methods) -------------------------

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path).map_err(|e| err("open", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Open a repository with options.
    #[napi]
    pub fn open_opts(path: String, _bare: Option<bool>) -> Result<Repository> {
        let opts = gix::open::Options::default();
        let ts =
            gix::ThreadSafeRepository::open_opts(path, opts).map_err(|e| err("open_opts", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize a repository with options.
    #[napi]
    pub fn init_opts(path: String, bare: Option<bool>) -> Result<Repository> {
        Repository::init(path, bare)
    }

    /// Initialize a repository at `path`. Pass `bare: true` for a bare repository.
    #[napi]
    pub fn init(path: String, bare: Option<bool>) -> Result<Repository> {
        let r = if bare.unwrap_or(false) {
            gix::init_bare(path)
        } else {
            gix::init(path)
        }
        .map_err(|e| err("init", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Discover a repository with environment overrides.
    #[napi]
    pub fn discover_with_environment_overrides(path: Option<String>) -> Result<Repository> {
        let p = path.unwrap_or_else(|| ".".to_string());
        let r = gix::discover_with_environment_overrides(p)
            .map_err(|e| err("discover_with_environment_overrides", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Open a repository with environment overrides.
    #[napi]
    pub fn open_with_environment_overrides(path: String) -> Result<Repository> {
        let r = gix::open_with_environment_overrides(path)
            .map_err(|e| err("open_with_environment_overrides", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Discover a repository by walking up from `path` (or the current directory if omitted).
    #[napi]
    pub fn discover(path: Option<String>) -> Result<Repository> {
        let p = path.unwrap_or_else(|| ".".to_string());
        let r = gix::discover(p).map_err(|e| err("discover", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Clone a repository from `url` into `path`.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread and resolves a
    /// `Promise<Repository>`.
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(url: String, path: String, bare: Option<bool>) -> Result<AsyncTask<CloneTask>> {
        Ok(AsyncTask::new(CloneTask {
            url,
            path,
            bare: bare.unwrap_or(false),
        }))
    }

    // ---- objects ----------------------------------------------------------

    /// Find and decode a tree by id.
    #[napi]
    pub fn find_tree(&self, id: String) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r
            .find_tree(parse_oid(&id)?)
            .map_err(|e| err("find_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Find a blob by id (content is read lazily via [`Blob::content`]).
    #[napi]
    pub fn find_blob(&self, id: String) -> Result<Blob> {
        let id = parse_oid(&id)?;
        Ok(build_blob(&self.inner, id))
    }

    /// Find and decode a tag by id.
    #[napi]
    pub fn find_tag(&self, id: String) -> Result<Tag> {
        let r = thread_local(&self.inner);
        let tag = r
            .find_tag(parse_oid(&id)?)
            .map_err(|e| err("find_tag", e))?;
        build_tag(&self.inner, tag)
    }

    /// Find an object of any kind, returning a wrapper with `as_commit()` / `as_tree()` / etc.
    #[napi]
    pub fn find_object(&self, id: String) -> Result<GitObject> {
        let r = thread_local(&self.inner);
        let object = r
            .find_object(parse_oid(&id)?)
            .map_err(|e| err("find_object", e))?;
        build_object(&self.inner, object)
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn object_exists(&self, id: String) -> bool {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => r.has_object(oid),
            Err(_) => false,
        }
    }

    /// Write raw bytes as a blob and return its id.
    #[napi]
    pub fn write_blob(&self, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write an annotated tag object directly to the object database.
    #[napi]
    pub fn write_tag(&self, input: TagInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let target = parse_oid(&input.target)?;
        let target_kind = match input.target_kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => {
                return Err(err(
                    "write_tag",
                    format!("invalid target_kind {}", input.target_kind),
                ));
            }
        };

        let default_tagger = r.committer().and_then(|res| res.ok()).map(|s| s.into());
        let tagger = match input.tagger {
            Some(s) => Some(signature_from_dto(&s)?),
            None => default_tagger,
        };

        let tag = gix::objs::Tag {
            target,
            target_kind,
            name: input.name.into(),
            tagger,
            message: input.message.into(),
            signature: None,
        };

        let oid = r.write_object(&tag).map_err(|e| err("write_tag", e))?;
        Ok(oid.to_string())
    }

    /// Peel an object to a target object kind (`commit`, `tree`, `blob`, `tag`).
    #[napi]
    pub fn peel(&self, id: String, kind: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let target_kind = match kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(err("peel", format!("invalid target kind {kind}"))),
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        Ok(peeled.id().to_string())
    }

    /// Write a tree object constructed from tree item inputs and return its OID.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree_entries = Vec::new();
        for e in entries {
            let mode = match e.mode.as_deref().unwrap_or("100644") {
                "100755" => gix::objs::tree::EntryKind::BlobExecutable.into(),
                "040000" | "40000" => gix::objs::tree::EntryKind::Tree.into(),
                "120000" => gix::objs::tree::EntryKind::Link.into(),
                "160000" => gix::objs::tree::EntryKind::Commit.into(),
                _ => gix::objs::tree::EntryKind::Blob.into(),
            };
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

    /// Try to find an object by id, returning null if missing instead of throwing.
    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_object(oid) {
                Ok(obj) => Ok(Some(build_object(&self.inner, obj)?)),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(
        &self,
        kind: String,
        data: napi::bindgen_prelude::Buffer,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let kind = string_to_kind(&kind)?;
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(kind, data.len() as u64, &mut buf)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Write a blob using the streaming write path, returning its id.
    #[napi]
    pub fn write_blob_stream(&self, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(gix::object::Kind::Blob, data.len() as u64, &mut buf)
            .map_err(|e| err("write_blob_stream", e))?;
        Ok(id.to_string())
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
                editor
                    .remove(e.path.as_str())
                    .map_err(|e| err("edit_tree.remove", e))?;
            } else {
                let kind = mode_to_entry_kind(e.mode.as_deref().unwrap_or("100644"))?;
                let oid = parse_oid(
                    e.id.as_deref()
                        .ok_or_else(|| err("edit_tree", "upsert edit requires an `id`"))?,
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

    /// Return the well-known empty blob object id as a [`Blob`].
    #[napi]
    pub fn empty_blob(&self) -> Result<Blob> {
        let r = thread_local(&self.inner);
        let id = r.object_hash().empty_blob();
        Ok(build_blob(&self.inner, id))
    }

    /// Read an object's header (kind + size) without decoding its full content.
    #[napi]
    pub fn find_header(&self, id: String) -> Result<ObjectHeader> {
        let r = thread_local(&self.inner);
        let header = r
            .find_header(parse_oid(&id)?)
            .map_err(|e| err("find_header", e))?;
        Ok(ObjectHeader {
            kind: kind_to_string(header.kind()),
            size: header.size() as f64,
        })
    }

    /// Like [`find_header`](Self::find_header) but returns `null` if the object is missing.
    #[napi]
    pub fn try_find_header(&self, id: String) -> Result<Option<ObjectHeader>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_header(oid) {
                Ok(header) => Ok(Some(ObjectHeader {
                    kind: kind_to_string(header.kind()),
                    size: header.size() as f64,
                })),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    // ---- performance / object cache -----------------------------------------

    /// Set the in-memory object cache size (in bytes). Pass `0` to disable.
    ///
    /// Note: each synchronous call gets a fresh thread-local repository view, so this only
    /// affects the cache of the view used within this single call.
    #[napi]
    pub fn object_cache_size(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size(bytes as usize);
        Ok(())
    }

    /// Set the object cache size only if it has not been set already.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn object_cache_size_if_unset(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size_if_unset(bytes as usize);
        Ok(())
    }

    /// Compute a recommended object cache size (in bytes) for tree-diff operations,
    /// based on the number of tracked files in the index.
    #[napi]
    pub fn compute_object_cache_size_for_tree_diffs(&self) -> Result<f64> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        Ok(r.compute_object_cache_size_for_tree_diffs(&*index) as f64)
    }

    /// Enable keeping decompressed objects in memory for the lifetime of this view.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn with_object_memory(&self) -> Result<()> {
        let r = thread_local(&self.inner);
        r.with_object_memory();
        Ok(())
    }

    // ---- notes ------------------------------------------------------------

    /// Find a git note for an object.
    #[napi]
    pub fn find_note(&self, target_id: String) -> Result<Option<NoteInfo>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target_id)?;
        let mut platform = r.notes().map_err(|e| err("notes", e))?;
        if let Ok(notes) = platform.get(oid) {
            if let Some(n) = notes.into_iter().next() {
                return Ok(Some(NoteInfo {
                    id: n.blob.id.to_string(),
                    message: lossy(&n.blob.data),
                }));
            }
        }
        Ok(None)
    }

    // ---- attributes & excludes -------------------------------------------

    /// Check attributes for a relative path.
    #[napi]
    pub fn attributes(&self, relative_path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                None,
            )
            .map_err(|e| err("attributes", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let mut out = Vec::new();
        let mut selected = gix::attrs::search::Outcome::default();
        platform.matching_attributes(&mut selected);
        for m in selected.iter() {
            out.push(AttributeEntry {
                name: m.assignment.name.as_str().to_string(),
                value: format!("{:?}", m.assignment.state),
            });
        }
        Ok(out)
    }

    /// Check attributes for a relative path, without applying exclude/ignore information.
    ///
    /// Unlike [`attributes`](Self::attributes), `.gitignore`/`.git/info/exclude` files are
    /// not consulted, so this is the appropriate query when only `.gitattributes` matches
    /// are relevant.
    #[napi]
    pub fn attributes_only(&self, relative_path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .map_err(|e| err("attributes_only", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let mut out = Vec::new();
        let mut selected = gix::attrs::search::Outcome::default();
        platform.matching_attributes(&mut selected);
        for m in selected.iter() {
            out.push(AttributeEntry {
                name: m.assignment.name.as_str().to_string(),
                value: format!("{:?}", m.assignment.state),
            });
        }
        Ok(out)
    }

    /// Check whether a relative path is excluded by gitignore / exclude files.
    #[napi]
    pub fn is_path_excluded(&self, relative_path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .map_err(|e| err("excludes", e))?;
        let platform = cache
            .at_path(
                relative_path.as_str(),
                Some(gix::objs::tree::EntryKind::Blob.into()),
            )
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// Reset HEAD, index, and optionally working tree to target.
    #[napi]
    pub fn reset(&self, target: String, hard: Option<bool>) -> Result<()> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?;

        let label = format!("reset {target}");
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("reset_head", e))?;

        let mut file = r
            .index_from_tree(&tree_id)
            .map_err(|e| err("index_from_tree", e))?;
        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;

        if hard.unwrap_or(false) {
            if let Some(workdir) = r.workdir() {
                let _ = std::fs::remove_dir_all(workdir);
                let _ = std::fs::create_dir_all(workdir);
            }
        }

        Ok(())
    }

    /// Whether this repository instance is a linked worktree.
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        r.worktree().map(|wt| !wt.is_main()).unwrap_or(false)
    }

    /// Return the main repository if this is a linked worktree.
    #[napi]
    pub fn main_repository(&self) -> Result<Repository> {
        let r = thread_local(&self.inner);
        let main_repo = r.main_repo().map_err(|e| err("main_repo", e))?;
        Ok(Repository {
            inner: Arc::new(main_repo.into()),
        })
    }

    /// The `.git` directory.
    #[napi]
    pub fn git_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.git_dir().to_string_lossy().into_owned()
    }

    /// The common git directory (may differ from `git_dir` for linked worktrees).
    #[napi]
    pub fn common_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.common_dir().to_string_lossy().into_owned()
    }

    /// Whether this repository is bare.
    #[napi]
    pub fn is_bare(&self) -> bool {
        let r = thread_local(&self.inner);
        r.is_bare()
    }

    /// The repository kind as a string (`bare` / `working-tree`).
    #[napi]
    pub fn kind(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.kind())
    }

    /// The object hash kind (`sha1` / `sha256`).
    #[napi]
    pub fn object_hash(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.object_hash())
    }

    // ---- submodules & identity -------------------------------------------

    /// Retrieve default author signature configured for this repository.
    #[napi]
    pub fn default_author(&self) -> Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        match r.author() {
            Some(Ok(s)) => Ok(Some(Signature {
                name: lossy(s.name),
                email: lossy(s.email),
                time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
                timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
            })),
            _ => Ok(None),
        }
    }

    /// Retrieve default committer signature configured for this repository.
    #[napi]
    pub fn default_committer(&self) -> Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        match r.committer() {
            Some(Ok(s)) => Ok(Some(Signature {
                name: lossy(s.name),
                email: lossy(s.email),
                time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
                timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
            })),
            _ => Ok(None),
        }
    }

    // ---- status / checkout ----------------------------------------------

    /// Compute working tree status asynchronously.
    #[napi(ts_return_type = "Promise<Vec<StatusEntry>>")]
    pub fn status(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Checkout a target commit asynchronously.
    #[napi(ts_return_type = "Promise<String>")]
    pub fn checkout(&self, target: String) -> Result<AsyncTask<CheckoutTask>> {
        Ok(AsyncTask::new(CheckoutTask {
            repo: Arc::clone(&self.inner),
            target,
        }))
    }

    /// The operation currently in progress (e.g. `Merge`, `Rebase`, `CherryPick`), or `null`.
    #[napi]
    pub fn state(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.state().map(|s| format!("{:?}", s))
    }

    /// The trust level assigned to this repository's git-dir (`full` or `reduced`).
    #[napi]
    pub fn git_dir_trust(&self) -> String {
        let r = thread_local(&self.inner);
        match r.git_dir_trust() {
            gix::sec::Trust::Full => "full".to_string(),
            gix::sec::Trust::Reduced => "reduced".to_string(),
        }
    }

    /// The current working directory as present during repository instantiation.
    #[napi]
    pub fn current_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.current_dir().display().to_string()
    }

    /// The path to the `.git/modules` directory of the worktree, or `null` if bare.
    #[napi]
    pub fn modules_path(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.modules_path().map(|p| p.display().to_string())
    }

    /// The directory the git binary was installed to (used for exec-path lookups).
    #[napi]
    pub fn install_dir(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        r.install_dir()
            .map(|p| p.display().to_string())
            .map_err(|e| err("install_dir", e))
    }

    /// The relative path between the working tree and the current working dir, or `null`.
    #[napi]
    pub fn prefix(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.prefix()
            .map_err(|e| err("prefix", e))?
            .map(|p| p.display().to_string()))
    }

    /// Normalize `path` into a repository-relative Git path with slash separators.
    #[napi]
    pub fn normalize_path(&self, path: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let normalized = r
            .normalize_path(path.as_str())
            .map_err(|e| err("normalize_path", e))?;
        let bytes: &[u8] = &normalized;
        Ok(lossy(bytes))
    }

    /// Whether this repository has no commits and no configured `init.defaultBranch` deviation.
    #[napi]
    pub fn is_pristine(&self) -> Option<bool> {
        let r = thread_local(&self.inner);
        r.is_pristine()
    }

    /// Resolve a repository-relative path against the working tree, or `null` if bare.
    #[napi]
    pub fn workdir_path(&self, relative_path: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir_path(gix::bstr::BStr::new(relative_path.as_bytes()))
            .map(|p| p.display().to_string())
    }

    /// Replace the working tree directory (validated to exist), returning the previous one.
    #[napi]
    pub fn set_workdir(&self, workdir: Option<String>) -> Result<Option<String>> {
        let mut r = thread_local(&self.inner);
        let previous = r
            .set_workdir(workdir.map(std::path::PathBuf::from))
            .map_err(|e| err("set_workdir", e))?;
        Ok(previous.map(|p| p.display().to_string()))
    }

    // ---- log ---------------------------------------------------------------

    /// Read commit history (a "git log") starting at `rev` (or HEAD).
    ///
    /// Returns commit summaries in reverse-chronological order. `limit` caps the number of
    /// commits; `firstParent` restricts the walk to first-parent history. This resolves the
    /// whole range in one call (no per-commit round-trips).
    #[napi]
    pub fn log(
        &self,
        rev: Option<String>,
        limit: Option<f64>,
        first_parent: Option<bool>,
    ) -> Result<Vec<LogEntry>> {
        let r = thread_local(&self.inner);
        let tip: gix::ObjectId = match &rev {
            Some(s) => r
                .rev_parse_single(s.as_str())
                .map_err(|e| err("rev_parse_single", e))?
                .into(),
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        let limit = limit.map(|l| l as usize);
        let mut out = Vec::new();

        if first_parent.unwrap_or(false) {
            let mut current = Some(tip);
            while let Some(id) = current {
                let (entry, parents) = log_summary(&r, id)?;
                out.push(entry);
                if let Some(l) = limit {
                    if out.len() >= l {
                        break;
                    }
                }
                current = parents
                    .into_iter()
                    .next()
                    .map(|p| parse_oid(&p))
                    .transpose()?;
            }
        } else {
            let platform = r.rev_walk([tip]);
            for id in platform.all().map_err(|e| err("rev_walk", e))? {
                let id = id.map_err(|e| err("rev_walk item", e))?;
                let (entry, _parents) = log_summary(&r, id.id)?;
                out.push(entry);
                if let Some(l) = limit {
                    if out.len() >= l {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    // ---- diff -------------------------------------------------------------

    /// Diff two trees (or one tree against empty when `old` is omitted).
    #[napi]
    pub fn diff_tree_to_tree(&self, old: Option<String>, new_id: String) -> Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        let old_tree = match old {
            Some(o) => Some(
                r.find_tree(parse_oid(&o)?)
                    .map_err(|e| err("find_tree", e))?,
            ),
            None => None,
        };
        let new_tree = r
            .find_tree(parse_oid(&new_id)?)
            .map_err(|e| err("find_tree", e))?;
        let changes = r
            .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), None)
            .map_err(|e| err("diff_tree_to_tree", e))?;
        Ok(changes
            .into_iter()
            .map(|c| match c {
                gix::object::tree::diff::ChangeDetached::Addition { location, id, .. } => Change {
                    path: lossy(&*location),
                    old_id: None,
                    new_id: Some(id.to_string()),
                    kind: "Added".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Deletion { location, id, .. } => Change {
                    path: lossy(&*location),
                    old_id: Some(id.to_string()),
                    new_id: None,
                    kind: "Deleted".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => Change {
                    path: lossy(&*location),
                    old_id: Some(previous_id.to_string()),
                    new_id: Some(id.to_string()),
                    kind: "Modified".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Rewrite {
                    location,
                    source_id,
                    id,
                    ..
                } => Change {
                    path: lossy(&*location),
                    old_id: Some(source_id.to_string()),
                    new_id: Some(id.to_string()),
                    kind: "Renamed".to_string(),
                },
            })
            .collect())
    }

    /// Compute a blame for `file_path` (relative to the repository root), starting from
    /// `commit_id` (or `HEAD` if omitted). Returns the per-hunk blame entries plus the full
    /// content of the blamed file.
    #[napi]
    pub fn blame(&self, file_path: String, commit_id: Option<String>) -> Result<BlameOutcome> {
        let r = thread_local(&self.inner);
        let suspect = match commit_id {
            Some(c) => parse_oid(&c)?,
            None => r.head_commit().map_err(|e| err("head_commit", e))?.id,
        };
        let outcome = r
            .blame_file(
                gix::bstr::BStr::new(file_path.as_str()),
                suspect,
                Default::default(),
            )
            .map_err(|e| err("blame_file", e))?;

        let mut entries = Vec::with_capacity(outcome.entries.len());
        for e in &outcome.entries {
            entries.push(BlameEntry {
                start_in_blamed_file: e.start_in_blamed_file,
                start_in_source_file: e.start_in_source_file,
                len: e.len.get(),
                commit_id: e.commit_id.to_string(),
                source_file_name: e.source_file_name.as_ref().map(|n| lossy(n)),
            });
        }
        Ok(BlameOutcome {
            entries,
            blob: outcome.blob,
        })
    }

    /// Return the currently set reference namespace, if any.
    ///
    /// The value is cached per repository because each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist.
    #[napi]
    pub fn namespace(&self) -> Option<String> {
        cached_namespace_get(Arc::as_ptr(&self.inner) as usize)
    }

    /// Set the reference namespace and return the previously set one (if any).
    #[napi]
    pub fn set_namespace(&self, ns: String) -> Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key);
        let mut r = thread_local(&self.inner);
        r.set_namespace(ns.as_str())
            .map_err(|e| err("set_namespace", e))?;
        // Compute the canonical `refs/namespaces/<name>/` form directly (a fresh
        // thread-local view would not otherwise report the value we just set).
        let current = gix::refs::namespace::expand(ns.as_str())
            .map(|n| String::from_utf8_lossy(n.into_bstring().as_slice()).into_owned())
            .map_err(|e| err("namespace_expand", e))?;
        cached_namespace_set(key, Some(current));
        Ok(previous)
    }

    /// Clear the reference namespace and return the previously set one (if any).
    #[napi]
    pub fn clear_namespace(&self) -> Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key);
        let mut r = thread_local(&self.inner);
        r.clear_namespace();
        cached_namespace_set(key, None);
        Ok(previous)
    }

    /// Return true if `ancestor` is an ancestor of (or equal to) `descendant`.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let ancestor_oid = parse_oid(&ancestor)?;
        let descendant_oid = parse_oid(&descendant)?;
        let platform = r.rev_walk([descendant_oid]);
        let ids = platform.all().map_err(|e| err("rev_walk", e))?;
        Ok(ids
            .into_iter()
            .any(|id| id.map(|i| i.id == ancestor_oid).unwrap_or(false)))
    }

    /// Produce a `git describe` string for `id` using all refs as candidate names.
    #[napi]
    pub fn describe(&self, id: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let describe_out = commit
            .describe()
            .names(gix::commit::describe::SelectRef::AllRefs)
            .try_format()
            .map_err(|e| err("describe_format", e))?;
        Ok(describe_out.map(|f| f.to_string()))
    }

    /// True if this repository is a shallow clone.
    #[napi]
    pub fn is_shallow(&self) -> Result<bool> {
        let r = thread_local(&self.inner);
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    /// Path to the `shallow` file (may not exist if not a shallow clone).
    #[napi]
    pub fn shallow_file(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// List the shallow boundary commits, or `null` if the repo is not shallow.
    #[napi]
    pub fn shallow_commits(&self) -> Result<Option<Vec<String>>> {
        let r = thread_local(&self.inner);
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        if !path.exists() {
            return Ok(None);
        }
        let buf = std::fs::read_to_string(&path).map_err(|e| err("read_shallow", e))?;
        let ids = buf
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                gix::ObjectId::from_hex(line.as_bytes())
                    .ok()
                    .map(|id| id.to_string())
            })
            .collect();
        Ok(Some(ids))
    }

    /// Compute the index↔worktree status (same underlying computation as [`status`](Self::status)).
    #[napi]
    pub fn index_worktree(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: self.inner.clone(),
        }))
    }
}

/// A single change produced by [`Repository::diff_tree_to_tree`].
#[derive(Clone)]
#[napi(object)]
pub struct Change {
    pub path: String,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    /// The kind of change: `Added`, `Modified`, `Deleted`, `Renamed`, `Copied`.
    pub kind: String,
}

/// A single commit summary returned by [`Repository::log`].
#[derive(Clone)]
#[napi(object)]
pub struct LogEntry {
    pub id: String,
    /// Full commit message.
    pub message: String,
    /// First line (subject) of the message.
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    /// Authoring time, unix seconds.
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
    /// Committing time, unix seconds.
    pub committer_time: i64,
    pub parent_ids: Vec<String>,
    pub tree_id: String,
    pub signature: Option<String>,
}

/// Decode a commit into owned data for [`LogEntry`], returning the entry plus its parent ids.
fn log_summary(r: &gix::Repository, id: gix::ObjectId) -> Result<(LogEntry, Vec<String>)> {
    let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
    let c = commit.decode().map_err(|e| err("decode commit", e))?;
    let author = c.author().map_err(|e| err("author", e))?;
    let committer = c.committer().map_err(|e| err("committer", e))?;
    let at = author.time().map_err(|e| err("author time", e))?;
    let ct = committer.time().map_err(|e| err("committer time", e))?;
    let full = lossy(c.message);
    let subject = full.split('\n').next().unwrap_or("").to_string();
    let parent_ids: Vec<String> = c.parents().map(|p| p.to_string()).collect();
    let signature = c
        .extra_headers()
        .find("gpgsig")
        .or_else(|| c.extra_headers().find("gpgsig-sha256"))
        .map(|v| lossy(v));
    let entry = LogEntry {
        id: id.to_string(),
        message: full,
        subject,
        author_name: lossy(author.name),
        author_email: lossy(author.email),
        author_time: at.seconds,
        committer_name: lossy(committer.name),
        committer_email: lossy(committer.email),
        committer_time: ct.seconds,
        parent_ids: parent_ids.clone(),
        tree_id: c.tree().to_string(),
        signature,
    };
    Ok((entry, parent_ids))
}
