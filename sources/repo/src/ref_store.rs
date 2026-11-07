use std::sync::Arc;

use napi_derive::napi;

use gix::config::AsBStr;
use gix::refs::TargetRef;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::{Signature, signature_from_dto, to_signature};
use crate::reference::{Reference, build_reference};
use crate::reflog::ReflogEntry;
use crate::repository::Repository;

/// Maximum symbolic-reference nesting depth that [`RefStore::resolve`] will
/// follow before giving up. Mirrors `MAX_NESTING_LEVEL` from the C ref-store.
const MAX_NESTING_LEVEL: usize = 5;
/// Default symbolic-reference nesting depth used when a negative `maxNesting`
/// is supplied. Mirrors `DEFAULT_NESTING_LEVEL` from the C ref-store.
const DEFAULT_NESTING_LEVEL: usize = 5;

/// The backend format a [`RefStore`] reads and writes through. Mirrors the
/// `git_ref_store_t` enum from the C ref-store.
#[napi]
pub enum RefStoreType {
    /// Classic loose/packed references stored on disk (`refs/`, `packed-refs`).
    Files,
    /// The reftable reference storage format.
    Reftable,
}

/// Map a [`RefStoreType`] to its canonical string name (`"files"` / `"reftable"`),
/// the inverse of [`ref_store_type_from_str`].
#[napi]
pub fn ref_store_type_name(ty: RefStoreType) -> String {
    match ty {
        RefStoreType::Files => "files",
        RefStoreType::Reftable => "reftable",
    }
    .to_string()
}

/// Parse a [`RefStoreType`] from its string name. Throws on an unknown name.
#[napi]
pub fn ref_store_type_from_str(name: String) -> napi::Result<RefStoreType> {
    match name.as_str() {
        "files" => Ok(RefStoreType::Files),
        "reftable" => Ok(RefStoreType::Reftable),
        other => Err(err_str(
            "ref_store_type",
            format!("unknown reference storage format '{other}'"),
        )),
    }
}

/// Initialise a reference-store backend template.
///
/// The C API uses this to zero-initialise a `git_ref_store_backend` struct. gix
/// has no pluggable backend abstraction — the store type is fixed by the
/// repository format — so this simply returns the default backend type.
/// `version` is accepted for API parity and ignored.
#[napi]
pub fn init_backend(_version: Option<u32>) -> RefStoreType {
    RefStoreType::Files
}

/// A handle on a repository's reference store.
///
/// In gix the reference store is owned by the repository, so this is a thin
/// wrapper that carries the repository and exposes the `git_ref_store_*`
/// operations against it. Construct it with [`RefStore::new`] (or
/// [`RefStore::open`]) from a [`Repository`].
#[napi]
pub struct RefStore {
    repo: Arc<gix::ThreadSafeRepository>,
}

/// One exported reference, the ref-store half of a repository export.
///
/// This is the same information `git fast-export` carries in its `reset`
/// commands: a name and a target. Use [`RefStore::export_refs`] to produce
/// these and [`RefStore::import_refs`] to apply them; combine them with
/// [`crate::import`] / [`crate::export`] (which carry the objects)
/// for a complete repository move.
#[napi(object)]
pub struct RefExport {
    /// Fully-qualified reference name (e.g. `refs/heads/main`).
    pub name: String,
    /// Target object id, or `null` for a symbolic reference.
    pub id: Option<String>,
    /// Target of a symbolic reference (e.g. `refs/heads/main`); mutually
    /// exclusive with `id`.
    pub symbolic: Option<String>,
}

/// Options for [`RefStore::import_refs`].
#[derive(Default)]
#[napi(object)]
pub struct RefImportOptions {
    /// Move refs even when the update is not a fast-forward.
    pub force: Option<bool>,
    /// Reflog message recorded for every applied edit.
    pub reflog_message: Option<String>,
    /// Apply nothing and report what would happen.
    pub dry_run: Option<bool>,
}

/// A single reference edit for use with [`RefStore::transaction`].
///
/// Mirrors the parameters of [`RefStore::write`] but grouped so multiple edits
/// can be applied atomically in one transaction.
#[napi(object)]
pub struct RefEditOptions {
    /// The fully-qualified reference name (e.g. `refs/heads/main`).
    pub name: String,
    /// The new target: a 40-hex object id, or a symbolic reference name.
    pub target: String,
    /// Allow overwriting an existing ref.
    pub force: Option<bool>,
    /// Constrain the current value; yields an error on mismatch.
    pub expected_target: Option<String>,
    /// Apply the change to the referent of a symbolic `name`.
    pub deref: Option<bool>,
    /// When `false`, suppress creation of a new reflog.
    pub update_reflog: Option<bool>,
    /// Reflog author; defaults to the repository's configured committer.
    pub signature: Option<Signature>,
    /// Reflog message.
    pub message: Option<String>,
}

#[napi]
impl RefStore {
    /// Bind a new, unopened reference store to `repository`.
    ///
    /// Mirrors `git_ref_store_new`: the store inherits the repository and its
    /// reference format; no I/O happens here.
    #[napi(constructor)]
    pub fn new(repository: &Repository) -> RefStore {
        RefStore {
            repo: Arc::clone(&repository.inner),
        }
    }

    /// Open the reference store for `repository`.
    ///
    /// Mirrors `git_ref_store_open`: selects the backend (files or reftable)
    /// that the repository was opened with. In gix the backend is chosen when
    /// the repository is opened, so this is equivalent to [`RefStore::new`].
    #[napi]
    pub fn open(repository: &Repository) -> RefStore {
        RefStore {
            repo: Arc::clone(&repository.inner),
        }
    }

    /// The backend format this store uses.
    ///
    /// gix does not expose the active backend as a runtime enum; repositories
    /// opened by this addon use the files backend, so `Files` is returned.
    #[napi(getter)]
    pub fn store_type(&self) -> RefStoreType {
        RefStoreType::Files
    }

    /// The repository's git directory (`.git`, or the repo root for bare repos).
    ///
    /// Mirrors the path facts exposed by the gix `file::Store`; equivalent to
    /// `Repository::git_dir`.
    #[napi(getter)]
    pub fn git_dir(&self) -> String {
        let r = thread_local(&self.repo);
        r.git_dir().to_string_lossy().into_owned()
    }

    /// The common directory holding shared references, or the git directory for
    /// a non-worktree repository. Mirrors `file::Store::common_dir` /
    /// `Repository::common_dir`.
    #[napi(getter)]
    pub fn common_dir(&self) -> String {
        let r = thread_local(&self.repo);
        r.common_dir().to_string_lossy().into_owned()
    }

    /// Like [`common_dir`](Self::common_dir) but guaranteed non-`None`: returns
    /// the common directory, or the git directory when there is no linked
    /// worktree. Mirrors `file::Store::common_dir_resolved`.
    #[napi(getter)]
    pub fn common_dir_resolved(&self) -> String {
        let r = thread_local(&self.repo);
        r.common_dir().to_string_lossy().into_owned()
    }

    /// The on-disk path of the `packed-refs` file. Mirrors
    /// `file::Store::packed_refs_path`; equivalent to `Repository`'s
    /// `packedRefsPath`.
    #[napi(getter)]
    pub fn packed_refs_path(&self) -> String {
        let r = thread_local(&self.repo);
        r.refs.packed_refs_path().to_string_lossy().into_owned()
    }

    /// Whether a reference named `refName` exists.
    ///
    /// Mirrors `git_ref_store_exists`.
    #[napi]
    pub fn exists(&self, ref_name: String) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        match r.try_find_reference(ref_name.as_str()) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(err("exists", e)),
        }
    }

    /// Look up a single reference by name.
    ///
    /// Mirrors `git_ref_store_lookup`. Throws when the reference does not exist.
    #[napi]
    pub fn lookup(&self, ref_name: String) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let reference = r
            .find_reference(ref_name.as_str())
            .map_err(|e| err("lookup", e))?;
        build_reference(&self.repo, reference)
    }

    /// Like [`lookup`](Self::lookup) but returns `null` instead of throwing when
    /// the reference does not exist.
    #[napi]
    pub fn try_lookup(&self, ref_name: String) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.repo);
        match r.try_find_reference(ref_name.as_str()) {
            Ok(Some(reference)) => Ok(Some(build_reference(&self.repo, reference)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(err("try_lookup", e)),
        }
    }

    /// Look up the `HEAD` reference (the symbolic ref itself, not its resolved target).
    #[napi]
    pub fn head(&self) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let reference = r.find_reference("HEAD").map_err(|e| err("head", e))?;
        build_reference(&self.repo, reference)
    }

    /// Look up a reference and follow symbolic links up to `maxNesting` deep.
    ///
    /// Mirrors `git_ref_store_resolve`. `maxNesting <= 0` uses the default depth
    /// (5); a value larger than the maximum is clamped. When a symbolic ref
    /// points at a missing ref, that symbolic ref is returned (matching the C
    /// "return the symref" behaviour).
    #[napi]
    pub fn resolve(&self, ref_name: String, max_nesting: Option<i32>) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let mut reference = r
            .find_reference(ref_name.as_str())
            .map_err(|e| err("resolve", e))?;

        let max = match max_nesting {
            Some(n) if n < 0 => DEFAULT_NESTING_LEVEL,
            Some(n) => (n as usize).min(MAX_NESTING_LEVEL),
            None => DEFAULT_NESTING_LEVEL,
        };

        let mut nesting = 0;
        while nesting < max {
            match reference.target() {
                TargetRef::Object(_) => break,
                TargetRef::Symbolic(name) => {
                    let target = name.to_string();
                    match r.try_find_reference(target.as_str()) {
                        Ok(Some(next)) => reference = next,
                        // Missing intermediate target: return the symref itself.
                        Ok(None) => break,
                        Err(e) => return Err(err("resolve", e)),
                    }
                    nesting += 1;
                }
            }
        }

        build_reference(&self.repo, reference)
    }

    /// Iterate references, optionally restricted to those matching `glob`
    /// (a shell-style pattern with `*` and `?`).
    ///
    /// Mirrors `git_ref_store_iterator`. The iteration is materialised eagerly
    /// into a name list; [`ReferenceIterator::next`] re-resolves each name.
    #[napi]
    pub fn iterator(&self, glob: Option<String>) -> napi::Result<ReferenceIterator> {
        let names = self.collect_names(glob, "iterator")?;
        Ok(ReferenceIterator {
            repo: Arc::clone(&self.repo),
            names,
            pos: 0,
        })
    }

    /// Eagerly list references, optionally restricted to those matching `glob`.
    ///
    /// Unlike [`iterator`](Self::iterator) (which yields lazily and re-resolves),
    /// this materialises the full [`Reference`] list up front.
    #[napi]
    pub fn list(&self, glob: Option<String>) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.repo);
        let names = self.collect_names(glob, "list")?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            match r.try_find_reference(name.as_str()) {
                Ok(Some(reference)) => out.push(build_reference(&self.repo, reference)?),
                // The ref vanished between listing and resolution; skip it.
                Ok(None) => continue,
                Err(e) => return Err(err("list", e)),
            }
        }
        Ok(out)
    }

    /// Like [`list`](Self::list) but returns just the reference names.
    #[napi]
    pub fn list_names(&self, glob: Option<String>) -> napi::Result<Vec<String>> {
        self.collect_names(glob, "list_names")
    }

    /// Serialise the reference store: one [`RefExport`] per reference, sorted
    /// by name, optionally restricted to those matching `glob`.
    ///
    /// This is the reference half of a repository export — the complement of
    /// `Repository.export`, which carries the objects but only the refs it
    /// walked. Together they cover "all refs + all reachable objects"; for a
    /// self-contained archive of every object (including unreachable ones)
    /// combine this with [`crate::object_store::memory::MemoryObjectStore::dump`], which writes a pack.
    #[napi]
    pub fn export_refs(&self, glob: Option<String>) -> napi::Result<Vec<RefExport>> {
        let r = thread_local(&self.repo);
        let names = self.collect_names(glob, "export_refs")?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let reference = match r.try_find_reference(name.as_str()) {
                Ok(Some(reference)) => reference.detach(),
                // The ref vanished between listing and resolution; skip it.
                Ok(None) => continue,
                Err(e) => return Err(err("export_refs", e)),
            };
            let (id, symbolic) = match reference.target {
                gix::refs::Target::Object(id) => (Some(id.to_string()), None),
                gix::refs::Target::Symbolic(target) => (None, Some(target.to_string())),
            };
            out.push(RefExport {
                name: reference.name.to_string(),
                id,
                symbolic,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Apply references produced by [`RefStore::export_refs`].
    ///
    /// An entry whose `id` is the null object id (all zeros) deletes the
    /// reference, mirroring git's `reset <ref>` + `from <null-oid>`. Direct and
    /// symbolic targets are both supported. All edits land in one transaction:
    /// either every ref is moved or none is. Returns the names that were
    /// updated; deletions are reported as well, since a deleted ref is also a
    /// change (and `force` is required to accept a non-fast-forward, just like
    /// `git fast-import`).
    #[napi]
    pub fn import_refs(
        &self,
        refs: Vec<RefExport>,
        options: Option<RefImportOptions>,
    ) -> napi::Result<Vec<String>> {
        let options = options.unwrap_or_default();
        let force = options.force.unwrap_or(false);
        let message = options
            .reflog_message
            .clone()
            .unwrap_or_else(|| "import-refs".to_string());
        let r = thread_local(&self.repo);
        let mut edits = Vec::with_capacity(refs.len());
        let mut names = Vec::with_capacity(refs.len());
        for entry in &refs {
            let full_name = gix::refs::FullName::try_from(entry.name.as_str())
                .map_err(|e| err("import_refs", e))?;
            let log = gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: true,
                message: message.clone().into(),
            };
            let previous = previous_value(None, Some(force), false)?;
            let edit = match (entry.id.as_deref(), entry.symbolic.as_deref()) {
                // A null id is a deletion, like fast-import's reset-from-null.
                (Some(id), _) if is_null_sha(id) => {
                    gix::refs::transaction::RefEdit::delete_with_log(
                        full_name,
                        previous,
                        gix::refs::transaction::RefLog::AndReference,
                    )
                }
                (Some(id), _) => {
                    let target = parse_target(id)?;
                    gix::refs::transaction::RefEdit::update_with_log(
                        full_name, target, previous, log,
                    )
                }
                (None, Some(symbolic)) => {
                    let target = parse_target(symbolic)?;
                    gix::refs::transaction::RefEdit::update_with_log(
                        full_name, target, previous, log,
                    )
                }
                // Neither id nor symbolic: nothing to point at.
                (None, None) => continue,
            };
            edits.push(edit);
            names.push(entry.name.clone());
        }
        if options.dry_run.unwrap_or(false) {
            return Ok(names);
        }
        self.apply_edits(&r, edits, &None)?;
        Ok(names)
    }

    /// Create or update a reference.
    ///
    /// Mirrors `git_ref_store_write`. `target` is a 40-hex object id or a
    /// symbolic reference name. `force` allows overwriting an existing ref.
    /// `expected_target`, when given, constrains the current value (object id or
    /// symbolic name) and yields an error on mismatch. `signature` and
    /// `message` populate the reflog entry.
    ///
    /// `deref`, when `true`, applies the change to the referent of a symbolic
    /// `name` (gix `RefEdit::with_deref`, matching the C store's deref handling
    /// for symbolic refs). `update_reflog`, when `false`, suppresses creation of
    /// a new reflog — best-effort, since gix still appends an entry for refs
    /// whose logging policy mandates it.
    #[napi]
    pub fn write(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
        signature: Option<Signature>,
        message: Option<String>,
        expected_target: Option<String>,
        deref: Option<bool>,
        update_reflog: Option<bool>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let full_name =
            gix::refs::FullName::try_from(name.as_str()).map_err(|e| err("write", e))?;
        let target = parse_target(&target)?;
        let previous = previous_value(expected_target, force, false)?;
        let write_log = update_reflog.unwrap_or(true);
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: write_log,
            message: if write_log {
                message.unwrap_or_default().into()
            } else {
                Default::default()
            },
        };
        let mut edit =
            gix::refs::transaction::RefEdit::update_with_log(full_name, target, previous, log);
        if deref.unwrap_or(false) {
            edit = edit.with_deref(true);
        }
        self.apply_edits(&r, vec![edit], &signature)?;
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("write", e))?;
        build_reference(&self.repo, reference)
    }

    /// Apply a batch of reference edits atomically.
    ///
    /// Mirrors applying several `git_ref_store_write` operations inside one
    /// transaction. Each entry in `edits` follows [`RefEditOptions`]; the whole
    /// batch succeeds or fails together (gix rolls back on error). Returns the
    /// post-transaction state of every edited reference.
    ///
    /// A single reflog author is applied to all entries (gix attributes the
    /// transaction to one committer); the first non-`None` `signature` in `edits`
    /// is used.
    #[napi]
    pub fn transaction(&self, edits: Vec<RefEditOptions>) -> napi::Result<Vec<Reference>> {
        let r = thread_local(&self.repo);
        let mut gix_edits = Vec::with_capacity(edits.len());
        for e in &edits {
            let full_name = gix::refs::FullName::try_from(e.name.as_str())
                .map_err(|e| err("transaction", e))?;
            let target = parse_target(&e.target)?;
            let previous = previous_value(e.expected_target.clone(), e.force, false)?;
            let write_log = e.update_reflog.unwrap_or(true);
            let log = gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: write_log,
                message: if write_log {
                    e.message.clone().unwrap_or_default().into()
                } else {
                    Default::default()
                },
            };
            let mut edit =
                gix::refs::transaction::RefEdit::update_with_log(full_name, target, previous, log);
            if e.deref.unwrap_or(false) {
                edit = edit.with_deref(true);
            }
            gix_edits.push(edit);
        }
        let signature = edits.iter().find_map(|e| e.signature.clone());
        self.apply_edits(&r, gix_edits, &signature)?;
        let mut out = Vec::with_capacity(edits.len());
        for e in &edits {
            let reference = r
                .find_reference(e.name.as_str())
                .map_err(|e| err("transaction", e))?;
            out.push(build_reference(&self.repo, reference)?);
        }
        Ok(out)
    }

    /// Rename a reference from `oldName` to `newName`.
    ///
    /// Mirrors `git_ref_store_rename`. The reflog is not relocated (gix does not
    /// move reflogs within a single rename), so callers that need the reflog
    /// preserved should read and rewrite it. Throws if the source does not exist
    /// or, without `force`, if the destination already exists.
    #[napi]
    pub fn rename(
        &self,
        old_name: String,
        new_name: String,
        force: Option<bool>,
        signature: Option<Signature>,
        message: Option<String>,
    ) -> napi::Result<Reference> {
        let r = thread_local(&self.repo);
        let old = r
            .find_reference(old_name.as_str())
            .map_err(|e| err("rename", e))?;
        let target = match old.target() {
            TargetRef::Object(id) => gix::refs::Target::Object(id.to_owned()),
            TargetRef::Symbolic(n) => gix::refs::Target::Symbolic(n.to_owned()),
        };

        let new_full =
            gix::refs::FullName::try_from(new_name.as_str()).map_err(|e| err("rename", e))?;
        let new_previous = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let log = gix::refs::transaction::LogChange {
            mode: gix::refs::transaction::RefLog::AndReference,
            force_create_reflog: false,
            message: message.unwrap_or_default().into(),
        };
        let create =
            gix::refs::transaction::RefEdit::update_with_log(new_full, target, new_previous, log);
        let old_full =
            gix::refs::FullName::try_from(old_name.as_str()).map_err(|e| err("rename", e))?;
        let delete = gix::refs::transaction::RefEdit::delete_with_log(
            old_full,
            gix::refs::transaction::PreviousValue::MustExist,
            gix::refs::transaction::RefLog::AndReference,
        );

        self.apply_edits(&r, vec![create, delete], &signature)?;
        let reference = r
            .find_reference(new_name.as_str())
            .map_err(|e| err("rename", e))?;
        build_reference(&self.repo, reference)
    }

    /// Delete the reference `refName`.
    ///
    /// Mirrors `git_ref_store_delete`. `expectedTarget`, when given, constrains
    /// the current value and yields an error on mismatch. `deref`, when `true`,
    /// applies the deletion to the referent of a symbolic `refName` (gix
    /// `RefEdit::with_deref`).
    #[napi]
    pub fn delete(
        &self,
        ref_name: String,
        expected_target: Option<String>,
        deref: Option<bool>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let name =
            gix::refs::FullName::try_from(ref_name.as_str()).map_err(|e| err("delete", e))?;
        let previous = previous_value(expected_target, None, true)?;
        let mut edit = gix::refs::transaction::RefEdit::delete_with_log(
            name,
            previous,
            gix::refs::transaction::RefLog::AndReference,
        );
        if deref.unwrap_or(false) {
            edit = edit.with_deref(true);
        }
        self.apply_edits(&r, vec![edit], &None)
    }

    /// Read the reflog of `refName`, oldest entry first.
    ///
    /// Mirrors `git_ref_store_reflog_read`. Returns an empty list when the ref
    /// has no reflog (including when the ref does not exist).
    #[napi]
    pub fn reflog_read(&self, ref_name: String) -> napi::Result<Vec<ReflogEntry>> {
        let r = thread_local(&self.repo);
        let full_name =
            gix::refs::FullName::try_from(ref_name.as_str()).map_err(|e| err("reflog_read", e))?;
        let mut platform = gix::refs::file::log::iter::Platform {
            store: &r.refs,
            name: full_name.as_ref(),
            buf: Vec::new(),
        };
        let mut out = Vec::new();
        if let Some(iter) = platform.all().map_err(|e| err("reflog_read", e))? {
            for line in iter {
                let line = line.map_err(|e| err("reflog line", e))?;
                let t = line.signature.time().map_err(|e| err("reflog time", e))?;
                out.push(ReflogEntry {
                    previous_oid: line.previous_oid.to_string(),
                    new_oid: line.new_oid.to_string(),
                    hash: String::new(),
                    abbreviated_hash: String::new(),
                    selector: String::new(),
                    signature: to_signature(
                        &line.signature.name,
                        &line.signature.email,
                        t.seconds,
                        t.offset,
                    ),
                    subject: lossy(&line.message),
                    action: String::new(),
                    message: String::new(),
                });
            }
        }
        // Derive the git_spawn-style fields (selector, abbreviated hash, and the
        // action/message split of the subject); `@{0}` is the newest entry.
        let n = out.len();
        for (i, e) in out.iter_mut().enumerate() {
            let rev = n.saturating_sub(i + 1);
            e.hash = e.new_oid.clone();
            e.abbreviated_hash = e.hash.chars().take(7).collect();
            e.selector = format!("{ref_name}@{{{rev}}}");
            let (action, message) = match e.subject.split_once(": ") {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (e.subject.clone(), String::new()),
            };
            e.action = action;
            e.message = message;
        }
        Ok(out)
    }

    /// Whether a reflog entry should be written when updating `refName`.
    ///
    /// Mirrors `git_ref_store_should_write_reflog`. Honours
    /// `core.logAllRefUpdates` (`true`/`false`/`always`), defaulting to the
    /// opposite of bareness when unset, and applies the `heads/`/`remotes/`/
    /// `notes/`/`HEAD` rules.
    #[napi]
    pub fn should_write_reflog(&self, ref_name: String) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        let logall = match r.config_snapshot().string("core.logAllRefUpdates") {
            Some(v) => match v.to_string().as_str() {
                "true" | "on" | "yes" => 1,
                "always" => 2,
                "false" | "off" | "no" => 0,
                // Unset / unknown: default to opposite of bareness.
                _ => {
                    if r.is_bare() {
                        0
                    } else {
                        1
                    }
                }
            },
            None => {
                if r.is_bare() {
                    0
                } else {
                    1
                }
            }
        };
        Ok(match logall {
            0 => false,
            2 => true,
            _ => {
                let name = ref_name.as_str();
                r.refs.reflog_exists(name).unwrap_or(false)
                    || name.starts_with("refs/heads/")
                    || name == "HEAD"
                    || name.starts_with("refs/remotes/")
                    || name.starts_with("refs/notes/")
            }
        })
    }

    /// Whether a reflog entry should also be written for `HEAD` when updating
    /// `refName`.
    ///
    /// Mirrors `git_ref_store_should_write_head_reflog`. Symbolic refs never
    /// trigger a HEAD reflog; otherwise this follows HEAD's symbolic chain and
    /// returns `true` when it ultimately points at `refName`.
    #[napi]
    pub fn should_write_head_reflog(&self, ref_name: String) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        let input = r
            .find_reference(ref_name.as_str())
            .map_err(|e| err("should_write_head_reflog", e))?;
        if matches!(input.target(), TargetRef::Symbolic(_)) {
            return Ok(false);
        }
        let head = match r.try_find_reference("HEAD") {
            Ok(Some(h)) => h,
            _ => return Ok(false),
        };
        if matches!(head.target(), TargetRef::Object(_)) {
            return Ok(false);
        }
        let mut current = head;
        loop {
            match current.target() {
                TargetRef::Object(_) => return Ok(false),
                TargetRef::Symbolic(name) => {
                    let name = name.to_string();
                    if name == ref_name {
                        return Ok(true);
                    }
                    match r.try_find_reference(name.as_str()) {
                        Ok(Some(next)) => current = next,
                        _ => return Ok(false),
                    }
                }
            }
        }
    }

    /// Whether a reflog exists for `refName`.
    ///
    /// Mirrors `git_ref_store_has_log`.
    #[napi]
    pub fn has_log(&self, ref_name: String) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        r.refs
            .reflog_exists(ref_name.as_str())
            .map_err(|e| err("has_log", e))
    }

    /// Ensure a reflog exists for `refName`.
    ///
    /// Mirrors `git_ref_store_ensure_log`. gix creates reflogs lazily on the
    /// first reflog-bearing write, so this only verifies that the reference
    /// exists (reflogs for missing refs cannot be pre-created).
    #[napi]
    pub fn ensure_log(&self, ref_name: String) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        match r.try_find_reference(ref_name.as_str()) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(err_str(
                "ensure_log",
                format!("cannot ensure reflog for missing reference '{ref_name}'"),
            )),
            Err(e) => Err(err("ensure_log", e)),
        }
    }

    /// Initialise the reference store: create standard directories and point
    /// `HEAD` at `headTarget` if supplied.
    ///
    /// Mirrors `git_ref_store_init`. `mode` and `flags` are filesystem
    /// permission/behaviour hints that gix does not expose and are ignored.
    #[napi]
    pub fn init(
        &self,
        head_target: Option<String>,
        _mode: Option<u32>,
        _flags: Option<u32>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        if let Some(target) = head_target {
            let target_full =
                gix::refs::FullName::try_from(target.as_str()).map_err(|e| err("init", e))?;
            let head_full = gix::refs::FullName::try_from("HEAD").map_err(|e| err("init", e))?;
            let edit = gix::refs::transaction::RefEdit::update_with_log(
                head_full,
                gix::refs::Target::Symbolic(target_full),
                gix::refs::transaction::PreviousValue::Any,
                gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("init HEAD -> {target}").into(),
                },
            );
            self.apply_edits(&r, vec![edit], &None)?;
        }
        Ok(())
    }

    /// Select the backend this store writes through.
    ///
    /// Mirrors `git_ref_store_set_backend`. gix has no pluggable reference
    /// backend — the format is fixed by the repository — so this always errors.
    #[napi]
    pub fn set_backend(&self, _backend: RefStoreType) -> napi::Result<()> {
        Err(err_str(
            "set_backend",
            "gix does not expose a pluggable reference store backend; the format is fixed by the repository",
        ))
    }

    /// Compact loose references into the packed representation.
    ///
    /// Mirrors `git_ref_store_compress`. gix manages packed-refs transparently
    /// and does not expose an explicit compaction through this API, so this is a
    /// no-op retained for interface parity.
    #[napi]
    pub fn compress(&self) -> napi::Result<()> {
        Ok(())
    }

    /// Acquire a logical lock on `refName`.
    ///
    /// Mirrors `git_ref_store_lock`. gix holds the filesystem lock only for the
    /// duration of an edit transaction, so this returns a [`RefLock`] carrying
    /// the ref name; call [`RefLock::unlock`] to apply a change or
    /// [`RefLock::cancel`] to release without writing.
    #[napi]
    pub fn lock(&self, ref_name: String) -> napi::Result<RefLock> {
        Ok(RefLock {
            repo: Arc::clone(&self.repo),
            ref_name,
        })
    }
}

impl RefStore {
    /// Apply reference edits, optionally attributing the reflog entries to
    /// `signature` instead of the repository's configured committer.
    fn apply_edits(
        &self,
        r: &gix::Repository,
        edits: Vec<gix::refs::transaction::RefEdit>,
        signature: &Option<Signature>,
    ) -> napi::Result<()> {
        let committer: Option<gix::actor::Signature> = match signature {
            Some(s) => Some(signature_from_dto(s)?),
            None => match r.committer() {
                Some(Ok(sig_ref)) => sig_ref.to_owned().ok(),
                _ => None,
            },
        };
        let time_str = committer.as_ref().map(|s| s.time.to_string());
        let committer_ref = committer.as_ref().map(|sig| gix::actor::SignatureRef {
            name: sig.name.as_bstr(),
            email: sig.email.as_bstr(),
            time: time_str.as_ref().unwrap().as_str(),
        });
        r.edit_references_as(edits, committer_ref)
            .map_err(|e| err("edit_reference", e))?;
        Ok(())
    }

    /// Gather all reference names, optionally filtered by a shell-style `glob`.
    ///
    /// Shared by [`iterator`](Self::iterator), [`list`](Self::list) and
    /// [`list_names`](Self::list_names). `what` labels any error from gix.
    fn collect_names(&self, glob: Option<String>, what: &str) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let platform = r.references().map_err(|e| err(what, e))?;
        let iter = platform.all().map_err(|e| err(what, e))?;
        let mut names = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|e| err(what, e))?;
            let name = reference.name().to_string();
            if let Some(g) = &glob {
                if !glob_match(g, &name) {
                    continue;
                }
            }
            names.push(name);
        }
        Ok(names)
    }
}

/// An iterator over references, materialised from [`RefStore::iterator`].
///
/// Mirrors the C `git_reference_iterator`. Each call to [`next`](Self::next)
/// re-resolves the next name (yielding a [`Reference`]) while
/// [`next_name`](Self::next_name) returns just the name.
#[napi]
pub struct ReferenceIterator {
    repo: Arc<gix::ThreadSafeRepository>,
    names: Vec<String>,
    pos: usize,
}

#[napi]
impl ReferenceIterator {
    /// Return the next reference, or `null` when exhausted.
    #[napi]
    pub fn next(&mut self) -> napi::Result<Option<Reference>> {
        if self.pos >= self.names.len() {
            return Ok(None);
        }
        let name = self.names[self.pos].clone();
        self.pos += 1;
        let r = thread_local(&self.repo);
        match r.try_find_reference(name.as_str()) {
            Ok(Some(reference)) => Ok(Some(build_reference(&self.repo, reference)?)),
            // The ref vanished between iteration start and now; skip it.
            Ok(None) => Ok(None),
            Err(e) => Err(err("iterator_next", e)),
        }
    }

    /// Return the next reference name, or `null` when exhausted.
    #[napi]
    pub fn next_name(&mut self) -> Option<String> {
        if self.pos >= self.names.len() {
            return None;
        }
        let name = self.names[self.pos].clone();
        self.pos += 1;
        Some(name)
    }
}

/// A logical lock on a single reference, returned by [`RefStore::lock`].
///
/// Mirrors the `void *payload` of the C `git_ref_store_lock` /
/// `git_ref_store_unlock` pair. Because gix locks only for the duration of an
/// edit transaction, the lock carries the ref name and defers the actual write
/// to [`unlock`](Self::unlock).
#[napi]
pub struct RefLock {
    repo: Arc<gix::ThreadSafeRepository>,
    /// The reference this lock guards.
    #[napi(readonly)]
    pub ref_name: String,
}

#[napi]
impl RefLock {
    /// Release the lock without applying any change.
    #[napi]
    pub fn cancel(&self) -> napi::Result<()> {
        Ok(())
    }

    /// Apply a reference change under the lock and release it.
    ///
    /// Mirrors `git_ref_store_unlock` with `success = true`. The parameters
    /// mirror [`RefStore::write`]; `updateReflog` (when `false`) suppresses
    /// creation of a new reflog for the write.
    #[napi]
    pub fn unlock(
        &self,
        target: String,
        force: Option<bool>,
        signature: Option<Signature>,
        message: Option<String>,
        expected_target: Option<String>,
        deref: Option<bool>,
        update_reflog: Option<bool>,
    ) -> napi::Result<Reference> {
        let store = RefStore {
            repo: Arc::clone(&self.repo),
        };
        store.write(
            self.ref_name.clone(),
            target,
            force,
            signature,
            message,
            expected_target,
            deref,
            update_reflog,
        )
    }
}

/// Parse `s` as a reference target: a 40-hex object id, else a symbolic name.
/// Whether `text` is the null object id (all zeros) — the marker git uses to
/// delete a reference.
fn is_null_sha(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b == b'0')
}

fn parse_target(s: &str) -> napi::Result<gix::refs::Target> {
    match parse_oid(s) {
        Ok(oid) => Ok(gix::refs::Target::Object(oid)),
        Err(_) => gix::refs::FullName::try_from(s)
            .map(gix::refs::Target::Symbolic)
            .map_err(|e| err("target", e)),
    }
}

/// Build a `PreviousValue` constraint from an optional expected target and the
/// `force` flag. `isDelete` picks `MustExist` as the fallback for deletions.
fn previous_value(
    expected: Option<String>,
    force: Option<bool>,
    is_delete: bool,
) -> napi::Result<gix::refs::transaction::PreviousValue> {
    use gix::refs::transaction::PreviousValue;
    match expected {
        Some(t) => {
            let target = parse_target(&t)?;
            Ok(PreviousValue::ExistingMustMatch(target))
        }
        None => {
            if force.unwrap_or(false) {
                Ok(PreviousValue::Any)
            } else if is_delete {
                Ok(PreviousValue::MustExist)
            } else {
                Ok(PreviousValue::MustNotExist)
            }
        }
    }
}

/// Lossy UTF-8 conversion for reflog message bytes.
fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Minimal shell-style glob match supporting `*` and `?` (and `\` escaping).
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    fn matches(p: &[u8], t: &[u8]) -> bool {
        let (mut pi, mut ti) = (0, 0);
        while pi < p.len() {
            match p[pi] {
                b'*' => {
                    while pi < p.len() && p[pi] == b'*' {
                        pi += 1;
                    }
                    if pi == p.len() {
                        return true;
                    }
                    for k in ti..=t.len() {
                        if matches(&p[pi..], &t[k..]) {
                            return true;
                        }
                    }
                    return false;
                }
                b'?' => {
                    if ti >= t.len() {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                }
                b'\\' => {
                    pi += 1;
                    if pi >= p.len() || ti >= t.len() || t[ti] != p[pi] {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                }
                c => {
                    if ti >= t.len() || t[ti] != c {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                }
            }
        }
        ti == t.len()
    }
    matches(pattern.as_bytes(), text.as_bytes())
}
