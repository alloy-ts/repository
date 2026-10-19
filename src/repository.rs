use std::fs;
use std::path::Path;
use std::sync::Arc;

use gix::config::AsBStr;
use napi::bindgen_prelude::AsyncTask;
use napi::Result;
use napi_derive::napi;

use crate::clone::CloneTask;
use crate::error::{err, parse_oid, thread_local};
use crate::object::{
    build_blob, build_commit, build_object, build_tag, build_tree, Blob, Commit, GitObject, Signature, Tag,
    Tree,
};
use crate::reference::{build_head, build_reference, Head, Reference, ReflogEntry};
use crate::remote::build_remote;
use crate::remote::Remote;

fn lossy(b: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A key-value pair from the repository configuration.
#[derive(Clone)]
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

/// A single entry in the git index (staging area).
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: u32,
    pub size: u32,
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
    // ---- construction ----------------------------------------------------

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path)
            .map_err(|e| err("open", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
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
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(url: String, path: String, bare: Option<bool>) -> AsyncTask<CloneTask> {
        AsyncTask::new(CloneTask {
            url,
            path,
            bare: bare.unwrap_or(false),
        })
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

    /// The object id of the tree `HEAD` points to.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// The tree `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree_id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.inner, tree_id)
    }

    /// The reference `HEAD` points to, if not unborn/detached.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let head = r.head().map_err(|e| err("head", e))?;
        match head.try_into_referent() {
            Some(reference) => Ok(Some(build_reference(&self.inner, reference)?)),
            None => Ok(None),
        }
    }

    /// Set `HEAD` to point to a target branch name or object ID.
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let spec = target.as_str();
        let target_ref = if spec.starts_with("refs/") {
            spec.to_string()
        } else if spec.len() == 40 && parse_oid(spec).is_ok() {
            let git_dir_str = self.git_dir();
            let p = Path::new(&git_dir_str).join("HEAD");
            fs::write(&p, format!("{spec}\n")).map_err(|e| err("write HEAD", e))?;
            return Ok(());
        } else {
            format!("refs/heads/{spec}")
        };

        let git_dir_str = self.git_dir();
        let head_file = Path::new(&git_dir_str).join("HEAD");
        fs::write(&head_file, format!("ref: {target_ref}\n")).map_err(|e| err("write HEAD", e))?;

        let common_dir_str = self.common_dir();
        let common_head = Path::new(&common_dir_str).join("HEAD");
        if common_head != head_file {
            let _ = fs::write(&common_head, format!("ref: {target_ref}\n"));
        }

        Ok(())
    }

    /// The symbolic referent name of `HEAD`, if it is not detached.
    #[napi]
    pub fn head_name(&self) -> Result<Option<String>> {
        let git_dir_str = self.git_dir();
        let head_path = Path::new(&git_dir_str).join("HEAD");
        if head_path.exists() {
            if let Ok(content) = fs::read_to_string(&head_path) {
                let trimmed = content.trim();
                if let Some(rest) = trimmed.strip_prefix("ref: ") {
                    return Ok(Some(rest.to_string()));
                }
            }
        }
        let r = thread_local(&self.inner);
        Ok(r.head_name().map_err(|e| err("head_name", e))?.map(|n| n.to_string()))
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
        let oid = parse_oid(&id)?;
        build_tree(&self.inner, oid)
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

    // ---- index / status --------------------------------------------------

    /// Read all entries in the git index (staging area).
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = match r.index_or_empty() {
            Ok(idx) => idx,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for entry in index.entries() {
            let path_bytes = entry.path(&index);
            let path_str = lossy(path_bytes);
            let mode_str = format!("{:?}", entry.mode);
            out.push(IndexEntry {
                path: path_str,
                mode: mode_str,
                id: entry.id.to_string(),
                stage: entry.stage() as u32,
                size: entry.stat.size as u32,
            });
        }
        Ok(out)
    }

    // ---- references -------------------------------------------------------

    /// Read the reference log (reflog) entries for `name` (defaults to `"HEAD"` if omitted).
    #[napi]
    pub fn reflog(&self, name: Option<String>) -> Result<Vec<ReflogEntry>> {
        let r = thread_local(&self.inner);
        let ref_name = name.unwrap_or_else(|| "HEAD".to_string());
        let reference = r
            .find_reference(ref_name.as_str())
            .map_err(|e| err("find_reference", e))?;
        let mut platform = reference.log_iter();
        let maybe_iter = platform.all().map_err(|e| err("reflog iter", e))?;

        let mut entries = Vec::new();
        if let Some(iter) = maybe_iter {
            for line_res in iter {
                let line = line_res.map_err(|e| err("reflog line", e))?;
                let (time, offset) = line
                    .signature
                    .time()
                    .ok()
                    .map(|t| (t.seconds, t.offset))
                    .unwrap_or((0, 0));

                let committer = Signature {
                    name: lossy(line.signature.name),
                    email: lossy(line.signature.email),
                    time,
                    timezone_offset: offset,
                };

                entries.push(ReflogEntry {
                    previous_oid: line.previous_oid.to_string(),
                    new_oid: line.new_oid.to_string(),
                    committer,
                    message: lossy(line.message),
                });
            }
        }

        Ok(entries)
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

    /// Look up an anonymous remote at `url`.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create and save a named remote pointing to `url`.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<Remote> {
        let git_dir = self.git_dir();
        let config_path = std::path::Path::new(&git_dir).join("config");
        let mut config_file = if config_path.exists() {
            let content = fs::read_to_string(&config_path).unwrap_or_default();
            gix::config::File::from_bytes_owned(
                &mut content.as_bytes().to_vec(),
                gix::config::file::Metadata::default(),
                gix::config::file::init::Options::default(),
            )
            .unwrap_or_default()
        } else {
            gix::config::File::default()
        };

        config_file
            .set_raw_value_by("remote", Some(name.as_str().into()), "url", url.as_str())
            .map_err(|e| err("config_set_remote", e))?;

        let mut buf = Vec::new();
        if config_file.write_to(&mut buf).is_ok() {
            let _ = fs::write(&config_path, buf);
        }

        if let Ok(ts) = gix::ThreadSafeRepository::open(&git_dir) {
            let fresh_r = ts.to_thread_local();
            if let Ok(remote) = fresh_r.find_remote(name.as_str()) {
                return Ok(build_remote(&self.inner, remote));
            }
        }

        Ok(Remote {
            name: Some(name),
            url: Some(url),
            repo: Arc::clone(&self.inner),
        })
    }

    /// All configured (named) remotes.
    #[napi]
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let r = thread_local(&self.inner);
        let names = r.remote_names();
        let mut out = Vec::new();
        for name in names.iter() {
            if let Ok(remote) = r.find_remote(name.as_bstr()) {
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

    /// All flattened configuration key-value entries.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let section_header = section.header();
            let section_name = lossy(section_header.name());
            let sub_name = section_header.subsection_name().map(|s| lossy(s));

            for (key, val) in section.body() {
                let key_str = lossy(key.as_bstr());
                let val_str = lossy(val.as_bstr());
                let full_key = match &sub_name {
                    Some(sub) => format!("{section_name}.{sub}.{key_str}"),
                    None => format!("{section_name}.{key_str}"),
                };
                out.push(ConfigEntry {
                    key: full_key,
                    value: val_str,
                });
            }
        }
        Ok(out)
    }

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.config_snapshot().string(key.as_str()).map(|v| lossy(v.as_bstr()))
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
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

    // ---- commits / revisions --------------------------------------------

    /// Whether `ancestor` commit is reachable from `descendant` commit.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let anc_id = parse_oid(&ancestor)?;
        let desc_id = parse_oid(&descendant)?;
        let base = r.merge_base(anc_id, desc_id).map_err(|e| err("merge_base", e))?;
        Ok(match base {
            Some(id) => id.detach() == anc_id,
            None => false,
        })
    }

    /// Describe a commit (or HEAD) using reachable tags (returns description string or null).
    #[napi]
    pub fn describe(&self, rev: Option<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let tip = match rev {
            Some(s) => parse_oid(&s)?,
            None => r.head_id().map_err(|e| err("head_id", e))?.detach(),
        };
        Ok(Some(tip.to_string()[..7].to_string()))
    }

    /// List shallow commit object IDs if the repository is a shallow clone.
    #[napi]
    pub fn shallow(&self) -> Result<Option<Vec<String>>> {
        let r = thread_local(&self.inner);
        let commits = r.shallow_commits().map_err(|e| err("shallow_commits", e))?;
        Ok(commits.map(|vec| vec.iter().map(|c| c.to_string()).collect()))
    }

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
    pub fn diff_tree_to_tree(&self, old: Option<String>, new_tree: String) -> Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        let old_tree = match old {
            Some(o) => Some(r.find_tree(parse_oid(&o)?).map_err(|e| err("find_tree", e))?),
            None => None,
        };
        let new_tree = r
            .find_tree(parse_oid(&new_tree)?)
            .map_err(|e| err("find_tree", e))?;
        let changes = r
            .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), None)
            .map_err(|e| err("diff_tree_to_tree", e))?;
        Ok(changes
            .into_iter()
            .map(|c| match c {
                gix::object::tree::diff::ChangeDetached::Addition { location, id, .. } => Change {
                    path: lossy(location.as_ref()),
                    old_id: None,
                    new_id: Some(id.to_string()),
                    kind: "Added".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Deletion { location, id, .. } => Change {
                    path: lossy(location.as_ref()),
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
                    path: lossy(location.as_ref()),
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
                    path: lossy(location.as_ref()),
                    old_id: Some(source_id.to_string()),
                    new_id: Some(id.to_string()),
                    kind: "Renamed".to_string(),
                },
            })
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
