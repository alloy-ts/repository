use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::object::{
    build_blob, build_commit, build_object, build_tag, build_tree, Blob, Commit, GitObject,
    Signature, Tag, Tree,
};
use crate::reference::{build_head, build_reference, Head, Reference};
use crate::remote::build_remote;
use crate::remote::Remote;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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

fn signature_from_dto(s: &Signature) -> Result<gix::actor::Signature> {
    Ok(gix::actor::Signature {
        name: s.name.clone().into(),
        email: s.email.clone().into(),
        time: gix::date::Time {
            seconds: s.time,
            offset: s.timezone_offset,
        },
    })
}

/// Input for writing a loose commit object.
#[derive(Clone)]
#[napi(object)]
pub struct CommitInput {
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
    pub author: Option<Signature>,
    pub committer: Option<Signature>,
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

/// A secondary worktree descriptor.
#[derive(Clone)]
#[napi(object)]
pub struct WorktreeProxy {
    pub id: String,
    pub git_dir: String,
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

/// The result of a merge operation.
#[derive(Clone)]
#[napi(object)]
pub struct MergeOutcome {
    pub tree_id: String,
    pub conflicts: bool,
}

/// A git attribute assignment entry.
#[derive(Clone)]
#[napi(object)]
pub struct AttributeEntry {
    pub name: String,
    pub value: String,
}

/// Information about a git submodule.
#[derive(Clone)]
#[napi(object)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
}

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

/// A single configuration key/value entry.
#[derive(Clone)]
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
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
        r.reference("HEAD", oid, gix::refs::transaction::PreviousValue::Any, label)
            .map_err(|e| err("checkout_head", e))?;

        let mut file = r.index_from_tree(&tree_id).map_err(|e| err("index_from_tree", e))?;
        file.write(Default::default()).map_err(|e| err("write_index", e))?;

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
        let platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
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
            let (repo, _outcome) = prepare.fetch_only(gix::progress::Discard, &interrupt).ok()?;
            Some(repo.into())
        })();

        if let Some(repo) = gix_repo {
            return Ok(Repository::new(Arc::new(repo)));
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
                    return Ok(Repository::new(Arc::new(ts)));
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
    pub(crate) namespace: Arc<std::sync::Mutex<Option<String>>>,
}

impl Repository {
    pub(crate) fn new(inner: Arc<gix::ThreadSafeRepository>) -> Self {
        Self {
            inner,
            namespace: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

#[napi]
impl Repository {
    // ---- construction (static factory methods) -------------------------

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path)
            .map_err(|e| err("open", e))?;
        Ok(Repository::new(Arc::new(ts)))
    }

    /// Open a repository with options.
    #[napi]
    pub fn open_opts(path: String, _bare: Option<bool>) -> Result<Repository> {
        let opts = gix::open::Options::default();
        let ts = gix::ThreadSafeRepository::open_opts(path, opts)
            .map_err(|e| err("open_opts", e))?;
        Ok(Repository::new(Arc::new(ts)))
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
        Ok(Repository::new(Arc::new(r.into())))
    }

    /// Discover a repository by walking up from `path` (or the current directory if omitted).
    #[napi]
    pub fn discover(path: Option<String>) -> Result<Repository> {
        let p = path.unwrap_or_else(|| ".".to_string());
        let r = gix::discover(p).map_err(|e| err("discover", e))?;
        Ok(Repository::new(Arc::new(r.into())))
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

    // ---- head / objects --------------------------------------------------

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
        build_commit(&self.inner, r.head_commit().map_err(|e| err("head_commit", e))?)
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
        Ok(r.head_name().map_err(|e| err("head_name", e))?.map(|n| n.to_string()))
    }

    /// The object id of the root tree pointed to by HEAD.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
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
        r.reference("HEAD", oid, gix::refs::transaction::PreviousValue::Any, label)
            .map_err(|e| err("set_head", e))?;
        Ok(())
    }

    /// Find and decode a commit by id.
    #[napi]
    pub fn find_commit(&self, id: String) -> Result<Commit> {
        let r = thread_local(&self.inner);
        let commit = r
            .find_commit(parse_oid(&id)?)
            .map_err(|e| err("find_commit", e))?;
        build_commit(&self.inner, commit)
    }

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
        let tag = r.find_tag(parse_oid(&id)?).map_err(|e| err("find_tag", e))?;
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

    /// Write a loose commit object directly to the object database without moving references.
    #[napi]
    pub fn write_commit(&self, input: CommitInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let tree = parse_oid(&input.tree)?;
        let parents = input
            .parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;

        let default_author = r.author().and_then(|res| res.ok());
        let default_committer = r.committer().and_then(|res| res.ok());

        let author = match input.author {
            Some(s) => signature_from_dto(&s)?,
            None => default_author
                .map(|s| s.into())
                .ok_or_else(|| err("author", "no author configured"))?,
        };
        let committer = match input.committer {
            Some(s) => signature_from_dto(&s)?,
            None => default_committer
                .map(|s| s.into())
                .ok_or_else(|| err("committer", "no committer configured"))?,
        };

        let commit = gix::objs::Commit {
            tree,
            parents: parents.into(),
            author,
            committer,
            encoding: None,
            message: input.message.into(),
            extra_headers: Vec::new(),
        };

        let oid = r.write_object(&commit).map_err(|e| err("write_commit", e))?;
        Ok(oid.to_string())
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
            _ => return Err(err("write_tag", format!("invalid target_kind {}", input.target_kind))),
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
        let tree = gix::objs::Tree { entries: tree_entries };
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

    /// Resolve revision specification to hex object ID string.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
    }

    // ---- references -------------------------------------------------------

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

    /// Find a submodule by name.
    #[napi]
    pub fn submodule_by_name(&self, name: String) -> Result<Option<SubmoduleInfo>> {
        let submodules = self.submodules()?;
        Ok(submodules.into_iter().find(|s| s.name == name))
    }

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

    /// Find a single reference by name (e.g. `main`, `refs/heads/main`, `HEAD`).
    #[napi]
    pub fn find_reference(&self, name: String) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("find_reference", e))?;
        build_reference(&self.inner, reference)
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

    // ---- remotes ----------------------------------------------------------

    /// All configured (named) remotes.
    #[napi]
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let r = thread_local(&self.inner);
        let names = r.remote_names();
        let mut out = Vec::new();
        for name in names.iter() {
            if let Ok(remote) = r.find_remote(&**name) {
                out.push(build_remote(&self.inner, remote));
            }
        }
        Ok(out)
    }

    /// Look up a single named remote.
    #[napi]
    pub fn find_remote(&self, name: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    // ---- config / paths --------------------------------------------------

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Read a single config value as a string (alias for `configString`).
    #[napi]
    pub fn config_get(&self, key: String) -> Option<String> {
        self.config_string(key)
    }

    /// Construct a Signature DTO.
    #[napi]
    pub fn signature_from_dto(
        &self,
        name: String,
        email: String,
        time: Option<f64>,
        timezone_offset: Option<f64>,
    ) -> Signature {
        Signature {
            name,
            email,
            time: time.map(|t| t as i64).unwrap_or(0),
            timezone_offset: timezone_offset.map(|t| t as i32).unwrap_or(0),
        }
    }

    /// Verify signature of a commit by ID.
    #[napi]
    pub fn verify_commit_signature(
        &self,
        commit_id: String,
    ) -> Result<Option<crate::object::SignatureVerifyOutcome>> {
        let commit = self.find_commit(commit_id)?;
        commit.verify_signature()
    }

    /// Return all flattened configuration key/value entries.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let header = section.header();
            let section_name = header.name();
            let sub = header.subsection_name();
            for key in section.value_names() {
                let key_str = if let Some(sub_name) = sub {
                    format!("{section_name}.{sub_name}.{key}")
                } else {
                    format!("{section_name}.{key}")
                };
                if let Some(val) = section.value(&key) {
                    out.push(ConfigEntry {
                        key: key_str,
                        value: lossy(val.as_ref()),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Initialize git-crypt for this repository.
    #[napi]
    pub fn crypt_init(&self) -> Result<crate::crypt::CryptStatus> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_init(Some(git_dir))
    }

    /// Check if git-crypt is initialized in this repository.
    #[napi]
    pub fn crypt_is_initialized(&self) -> Result<bool> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_is_initialized(Some(git_dir))
    }

    /// Lock this repository by removing git-crypt filters.
    #[napi]
    pub fn crypt_lock(&self) -> Result<()> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_lock(Some(git_dir))
    }

    /// Unlock this repository by restoring git-crypt filters.
    #[napi]
    pub fn crypt_unlock(&self, key_file: Option<String>) -> Result<()> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_unlock(Some(git_dir), key_file)
    }

    /// Export git-crypt key to file.
    #[napi]
    pub fn crypt_export_key(&self, output_path: String) -> Result<()> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_export_key(Some(git_dir), output_path)
    }

    /// Import git-crypt key from file.
    #[napi]
    pub fn crypt_import_key(&self, input_path: String) -> Result<()> {
        let git_dir = self.git_dir();
        crate::crypt::crypt_import_key(Some(git_dir), input_path)
    }

    /// Set a configuration key to a value in local config and snapshot.
    #[napi]
    pub fn config_set(&self, key: String, value: String) -> Result<()> {
        let mut r = thread_local(&self.inner);
        let local_config_path = r.git_dir().join("config");
        let mut local_file = if local_config_path.exists() {
            gix::config::File::from_path_no_includes(
                local_config_path.clone(),
                gix::config::Source::Local,
            )
            .ok()
        } else {
            None
        };

        let mut snapshot = r.config_snapshot_mut();
        snapshot
            .set_raw_value(&key, value.as_str())
            .map_err(|e| err("config_set", e))?;
        let _ = snapshot.commit();

        if let Some(ref mut file) = local_file {
            let _ = file.set_raw_value(&key, value.as_str());
            let _ = std::fs::write(&local_config_path, file.to_bstring().as_slice());
        }
        Ok(())
    }

    /// List configured branch names.
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        Ok(r.branch_names().into_iter().map(|s| s.to_string()).collect())
    }

    /// Retrieve the configured remote name for a branch.
    #[napi]
    pub fn branch_remote_name(&self, branch_name: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.branch_remote_name(branch_name.as_str(), gix::remote::Direction::Fetch)
            .map(|n| n.as_bstr().to_string())
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
            .at_path(relative_path.as_str(), Some(gix::objs::tree::EntryKind::Blob.into()))
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// List secondary worktrees for this repository.
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeProxy>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        let mut out = Vec::new();
        for p in proxies {
            out.push(WorktreeProxy {
                id: lossy(p.id().map_err(|e| err("worktree_id", e))?.as_ref()),
                git_dir: p.git_dir().to_string_lossy().into_owned(),
            });
        }
        Ok(out)
    }

    /// Reset HEAD, index, and optionally working tree to target.
    #[napi]
    pub fn reset(&self, target: String, hard: Option<bool>) -> Result<()> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?;

        let label = format!("reset {target}");
        r.reference("HEAD", oid, gix::refs::transaction::PreviousValue::Any, label)
            .map_err(|e| err("reset_head", e))?;

        let mut file = r.index_from_tree(&tree_id).map_err(|e| err("index_from_tree", e))?;
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
        Ok(Repository::new(Arc::new(main_repo.into())))
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

    /// List configured submodules.
    #[napi]
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Ok(Some(submodules)) = r.submodules() {
            for sm in submodules {
                let name = lossy(sm.name());
                let path_bstr = sm.path().map_err(|e| err("submodule_path", e))?;
                let path = lossy(path_bstr.as_ref());
                let url = sm.url().ok().map(|u| u.to_string());
                out.push(SubmoduleInfo { name, path, url });
            }
        }
        Ok(out)
    }

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

    // ---- index & status --------------------------------------------------

    /// Read entries from the index.
    #[napi]
    pub fn index_entries(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as u32,
            });
        }
        Ok(out)
    }

    /// Add or update index entries in the repository index file.
    #[napi]
    pub fn edit_index(&self, entries: Vec<IndexEntryInput>) -> Result<()> {
        let r = thread_local(&self.inner);
        let index_path = r.index_path();
        let mut file = match gix::index::File::at(&index_path, r.object_hash(), false, Default::default()) {
            Ok(file) => file,
            Err(_) => gix::index::File::from_state(gix::index::State::new(r.object_hash()), index_path),
        };

        for e in entries {
            let mode = match e.mode.as_deref().unwrap_or("100644") {
                "100755" => gix::index::entry::Mode::FILE_EXECUTABLE,
                "040000" | "40000" => gix::index::entry::Mode::DIR,
                "120000" => gix::index::entry::Mode::SYMLINK,
                "160000" => gix::index::entry::Mode::COMMIT,
                _ => gix::index::entry::Mode::FILE,
            };
            let oid = parse_oid(&e.id)?;
            let path = gix::bstr::BString::from(e.path);
            file.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                oid,
                gix::index::entry::Flags::empty(),
                mode,
                path.as_ref(),
            );
        }

        file.write(Default::default()).map_err(|e| err("write_index", e))?;
        Ok(())
    }

    /// Read entries from index, or load from HEAD if index doesn't exist.
    #[napi]
    pub fn index_or_load_from_head_entries(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index_or_load", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as u32,
            });
        }
        Ok(out)
    }

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

    // ---- commits / revisions --------------------------------------------

    /// Create a commit on `reference` and return its id.
    #[napi]
    pub fn commit(
        &self,
        reference: String,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;
        let id = r
            .commit(
                reference,
                message.as_str(),
                parse_oid(&tree)?,
                parent_ids,
            )
            .map_err(|e| err("commit", e))?;
        Ok(id.to_string())
    }

    /// Resolve a single revision specification (e.g. `HEAD~1`, `main`, `<sha>`) to an id.
    #[napi]
    pub fn rev_parse_single(&self, spec: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_single", e))?;
        Ok(id.to_string())
    }

    /// Resolve a revision specification to an id (alias of `revParseSingle`).
    #[napi]
    pub fn rev_parse(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
    }

    /// Walk all reachable commits from the given tip ids (newest first).
    #[napi]
    pub fn rev_walk(&self, tips: Vec<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let tip_ids: Vec<gix::ObjectId> = tips
            .iter()
            .map(|t| parse_oid(t))
            .collect::<Result<Vec<_>>>()?;
        let platform = r.rev_walk(tip_ids);
        let ids = platform
            .all()
            .map_err(|e| err("rev_walk", e))?
            .map(|id| id.map(|i| i.id.to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| err("rev_walk item", e))?;
        Ok(ids)
    }

    /// The merge base of two commits, if any.
    #[napi]
    pub fn merge_base(&self, one: String, two: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let base = r
            .merge_base(parse_oid(&one)?, parse_oid(&two)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The (octopus) merge base of multiple commits, if any.
    #[napi]
    pub fn merge_base_octopus(&self, commits: Vec<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<Result<Vec<_>>>()?;
        let base = r
            .merge_base_octopus(ids)
            .map_err(|e| err("merge_base_octopus", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// Merge two trees using a common ancestor tree and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: String,
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = parse_oid(&ancestor_tree)?;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some("ancestor".into()),
            current: Some("our".into()),
            other: Some("their".into()),
        };
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, labels, Default::default())
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome
            .tree
            .write()
            .map_err(|e| err("merge_tree_write", e))?
            .to_string();
        Ok(MergeOutcome {
            tree_id,
            conflicts: !outcome.conflicts.is_empty(),
        })
    }

    /// Merge two commits and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_commits(&self, our_commit: String, their_commit: String) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_commit)?;
        let theirs = parse_oid(&their_commit)?;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some("ancestor".into()),
            current: Some("our".into()),
            other: Some("their".into()),
        };
        let mut outcome = r
            .merge_commits(ours, theirs, labels, Default::default())
            .map_err(|e| err("merge_commits", e))?;
        let tree_id = outcome
            .tree_merge
            .tree
            .write()
            .map_err(|e| err("merge_commit_write", e))?
            .to_string();
        Ok(MergeOutcome {
            tree_id,
            conflicts: !outcome.tree_merge.conflicts.is_empty(),
        })
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
            Some(o) => Some(r.find_tree(parse_oid(&o)?).map_err(|e| err("find_tree", e))?),
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
        let mut r = thread_local(&self.inner);
        r.object_cache_size_if_unset(16 * 1024 * 1024);
        let _ = r.index_or_load_from_head_or_empty();
        let suspect = match commit_id {
            Some(c) => parse_oid(&c)?,
            None => r.head_commit().map_err(|e| err("head_commit", e))?.id,
        };
        let opts = gix::repository::blame_file::Options::default();
        let outcome = r
            .blame_file(
                gix::bstr::BStr::new(file_path.as_str()),
                suspect,
                opts,
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
    #[napi]
    pub fn namespace(&self) -> Option<String> {
        let guard = self.namespace.lock().ok()?;
        guard.clone()
    }

    /// Set the reference namespace and return the previously set one (if any).
    #[napi]
    pub fn set_namespace(&self, ns: String) -> Result<Option<String>> {
        let mut guard = self
            .namespace
            .lock()
            .map_err(|e| err("set_namespace_lock", e.to_string()))?;
        let prev = guard.clone();
        let formatted = if ns.starts_with("refs/namespaces/") {
            if ns.ends_with('/') {
                ns
            } else {
                format!("{ns}/")
            }
        } else {
            format!("refs/namespaces/{ns}/")
        };
        *guard = Some(formatted);
        Ok(prev)
    }

    /// Apply a single reference edit (create or update) and return the affected reference names.
    #[napi]
    pub fn edit_reference(&self, edit: RefEditInput) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let e = build_ref_edit(&edit)?;
        let edits = r
            .edit_reference(e)
            .map_err(|e| err("edit_reference", e))?;
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
    };
    Ok((entry, parent_ids))
}
