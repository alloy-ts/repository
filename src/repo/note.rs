use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};
use napi_derive::napi;

use std::sync::Arc;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::Signature;
use crate::repository::{NoteInfo, Repository};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A note attached to an object, originating from a specific notes reference.
#[napi(object)]
pub struct Note {
    /// The notes reference the note originated from, e.g. `refs/notes/commits`.
    pub reference: String,
    /// The object id of the note blob (equals git2's `Note.id()`).
    pub blob_id: String,
    /// The note content as UTF-8 text (equals git2's `Note.message()`).
    pub content: String,
    /// The raw note bytes (non-UTF-8-safe form of [`Note::content`]).
    pub content_bytes: Vec<u8>,
    /// The author of the notes-reference commit that introduced/last touched the note,
    /// or `null` when the notes reference does not yet point at a commit.
    pub author: Option<Signature>,
    /// The committer of the notes-reference commit, or `null` when unavailable.
    pub committer: Option<Signature>,
}

/// Options controlling [`Repository::set_note`] / [`Repository::remove_note`].
#[derive(Clone)]
#[napi(object)]
pub struct NoteChangeOptions {
    /// Override the commit message used for the notes-reference commit.
    pub commit_message: Option<String>,
    /// When `false`, refuse to overwrite an existing note (git2's `force = false`).
    /// Defaults to `true` (always replace).
    pub force: Option<bool>,
}

/// One entry produced by [`Repository::list_notes`], enumerating every note in a
/// notes reference (git2's `Notes` iterator).
#[napi(object)]
pub struct ListNoteEntry {
    /// The object the note annotates (its 40-character hex object id).
    pub annotated_id: String,
    /// The object id of the note blob.
    pub blob_id: String,
    /// The notes reference the note originated from.
    pub reference: String,
    /// The note content as UTF-8 text.
    pub content: String,
    /// The raw note bytes.
    pub content_bytes: Vec<u8>,
}

/// Resolve the effective notes reference name. A bare shorthand (`commits`,
/// `review`) is expanded to `refs/notes/<shorthand>`; an explicit name containing
/// a `/` is used as-is; and `None` yields the configured default, falling back to
/// git's implicit `refs/notes/commits`.
fn resolve_notes_ref_name(r: &gix::Repository, notes_ref: Option<&str>) -> String {
    match notes_ref {
        Some(s) if s.contains('/') => s.to_string(),
        Some(s) => format!("refs/notes/{s}"),
        None => r
            .notes()
            .ok()
            .and_then(|p| p.default_ref().map(|n| n.as_bstr().to_string()))
            .unwrap_or_else(|| "refs/notes/commits".to_string()),
    }
}

/// Author/committer of the commit the notes reference currently points at (git2's
/// `Note.author()` / `Note.committer()`). Returns `(None, None)` when the
/// reference does not resolve to a commit (e.g. no notes yet).
fn note_signatures(r: &gix::Repository, reference: &str) -> (Option<Signature>, Option<Signature>) {
    let (mut author, mut committer) = (None, None);
    if let Ok(mut rf) = r.find_reference(reference) {
        if let Ok(commit) = rf.peel_to_commit() {
            if let Ok(sig) = commit.author() {
                author = Some(crate::identity::signature_from_actor(&sig));
            }
            if let Ok(sig) = commit.committer() {
                committer = Some(crate::identity::signature_from_actor(&sig));
            }
        }
    }
    (author, committer)
}

/// Build a [`Note`] (including author/committer) from a gix note.
fn build_note(r: &gix::Repository, reference: &gix::bstr::BStr, blob: &gix::Blob) -> Note {
    let reference = reference.to_string();
    let (author, committer) = note_signatures(r, &reference);
    Note {
        reference,
        blob_id: blob.id.to_string(),
        content: String::from_utf8_lossy(&blob.data).into_owned(),
        content_bytes: blob.data.to_vec(),
        author,
        committer,
    }
}

// ---------------------------------------------------------------------------
// Shared compute paths (single source of truth for sync + async variants)
// ---------------------------------------------------------------------------

fn compute_notes_for(r: &gix::Repository, object_id: &str) -> napi::Result<Vec<Note>> {
    let oid = parse_oid(object_id)?;
    let mut platform = r.notes().map_err(|e| err("notes", e))?;
    let mut out = Vec::new();
    for n in platform.get(oid).map_err(|e| err("notes_get", e))? {
        out.push(build_note(r, n.reference.as_bstr(), &n.blob));
    }
    Ok(out)
}

fn compute_notes_in(
    r: &gix::Repository,
    refs: &[String],
    object_id: &str,
) -> napi::Result<Vec<Note>> {
    let oid = parse_oid(object_id)?;
    let mut platform = r
        .notes()
        .map_err(|e| err("notes", e))?
        .with_refs(refs.iter().map(|s| s.as_bytes()))
        .map_err(|e| err("notes_with_refs", e))?;
    let mut out = Vec::new();
    for n in platform.get(oid).map_err(|e| err("notes_get", e))? {
        out.push(build_note(r, n.reference.as_bstr(), &n.blob));
    }
    Ok(out)
}

fn compute_find_note(
    r: &gix::Repository,
    target_id: &str,
    notes_ref: Option<&str>,
) -> napi::Result<Option<NoteInfo>> {
    let oid = parse_oid(target_id)?;
    let mut platform = r.notes().map_err(|e| err("notes", e))?;
    if let Some(ref n) = notes_ref {
        platform = platform
            .with_refs([n.as_bytes()])
            .map_err(|e| err("notes_with_refs", e))?;
    }
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

fn compute_set_note(
    r: &gix::Repository,
    notes_ref: &str,
    object_id: &str,
    content: &str,
    options: Option<&NoteChangeOptions>,
) -> napi::Result<Option<String>> {
    let oid = parse_oid(object_id)?;
    if matches!(options.and_then(|o| o.force), Some(false)) {
        let probe = r.notes().map_err(|e| err("notes", e))?;
        let mut platform = probe
            .with_refs([notes_ref.as_bytes()])
            .map_err(|e| err("notes_with_refs", e))?;
        let existing = platform.get(oid).map_err(|e| err("notes_get", e))?;
        if !existing.is_empty() {
            return Err(err_str(
                "set_note",
                "a note already exists for this object; pass force:true to overwrite",
            ));
        }
    }
    let mut platform = r.notes().map_err(|e| err("notes", e))?;
    if let Some(msg) = options.and_then(|o| o.commit_message.as_ref()) {
        platform = platform.with_commit_message(msg.clone());
    }
    let previous = platform
        .replace(notes_ref, oid, content.as_bytes())
        .map_err(|e| err("notes_replace", e))?;
    Ok(previous.map(|id| id.to_string()))
}

fn compute_remove_note(
    r: &gix::Repository,
    notes_ref: &str,
    object_id: &str,
    options: Option<&NoteChangeOptions>,
) -> napi::Result<Option<String>> {
    let oid = parse_oid(object_id)?;
    let mut platform = r.notes().map_err(|e| err("notes", e))?;
    if let Some(msg) = options.and_then(|o| o.commit_message.as_ref()) {
        platform = platform.with_commit_message(msg.clone());
    }
    let removed = platform
        .remove(notes_ref, oid)
        .map_err(|e| err("notes_remove", e))?;
    Ok(removed.map(|id| id.to_string()))
}

fn compute_notes_refs(r: &gix::Repository) -> napi::Result<Vec<String>> {
    let platform = r.notes().map_err(|e| err("notes", e))?;
    Ok(platform.refs().map(|n| n.as_bstr().to_string()).collect())
}

/// Recursively walk a notes tree (git's fanout layout), emitting one
/// [`ListNoteEntry`] per note blob. The annotated object id is the concatenation
/// of path segments (each fanout directory contributes 2 hex characters).
fn collect_notes(
    r: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    reference: &str,
    out: &mut Vec<ListNoteEntry>,
) -> napi::Result<()> {
    for entry in tree.iter() {
        let entry = entry.map_err(|e| err("notes_iter", e))?;
        let inner = &entry.inner;
        if inner.mode.is_tree() {
            let sub = r.find_tree(inner.oid).map_err(|e| err("find_tree", e))?;
            let next = format!("{prefix}{}", lossy(inner.filename));
            collect_notes(r, &sub, &next, reference, out)?;
        } else if inner.mode.is_blob() {
            let annotated = format!("{prefix}{}", lossy(inner.filename));
            if annotated.len() == 40 && annotated.bytes().all(|b| b.is_ascii_hexdigit()) {
                let blob = r.find_blob(inner.oid).map_err(|e| err("find_blob", e))?;
                out.push(ListNoteEntry {
                    annotated_id: annotated,
                    blob_id: blob.id.to_string(),
                    reference: reference.to_string(),
                    content: String::from_utf8_lossy(&blob.data).into_owned(),
                    content_bytes: blob.data.to_vec(),
                });
            }
        }
    }
    Ok(())
}

fn compute_list_notes(
    r: &gix::Repository,
    notes_ref: Option<&str>,
) -> napi::Result<Vec<ListNoteEntry>> {
    let ref_name = resolve_notes_ref_name(r, notes_ref);
    let mut out = Vec::new();
    let mut rf = match r.find_reference(&ref_name) {
        Ok(rf) => rf,
        Err(_) => return Ok(out),
    };
    let tree = match rf.peel_to_tree() {
        Ok(t) => t,
        Err(_) => return Ok(out),
    };
    collect_notes(r, &tree, "", &ref_name, &mut out)?;
    Ok(out)
}

#[napi]
impl Repository {
    /// Return the effective default notes reference. When `core.notesRef` is unset
    /// this is git's implicit `refs/notes/commits`; otherwise the configured value.
    #[napi]
    pub fn notes_default_ref(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        Ok(r.notes()
            .map_err(|e| err("notes", e))?
            .default_ref()
            .map(|n| n.as_bstr().to_string())
            .unwrap_or_else(|| "refs/notes/commits".to_string()))
    }

    /// Return all notes attached to `object_id` across the configured notes
    /// references, in display order.
    #[napi]
    pub fn notes_for(&self, object_id: String) -> napi::Result<Vec<Note>> {
        let r = thread_local(&self.inner);
        compute_notes_for(&r, &object_id)
    }

    /// Return all notes attached to `object_id` across the explicitly listed notes
    /// references (`refs` accepts shorthands like `commits` or `review`, or fully
    /// qualified `refs/notes/...`).
    #[napi]
    pub fn notes_in(&self, refs: Vec<String>, object_id: String) -> napi::Result<Vec<Note>> {
        let r = thread_local(&self.inner);
        compute_notes_in(&r, &refs, &object_id)
    }

    /// Find the first note for `object_id`. When `notes_ref` is omitted the
    /// configured default reference is searched; otherwise only that reference is
    /// consulted. Returns `null` when no note exists.
    #[napi]
    pub fn find_note(
        &self,
        target_id: String,
        notes_ref: Option<String>,
    ) -> napi::Result<Option<NoteInfo>> {
        let r = thread_local(&self.inner);
        compute_find_note(&r, &target_id, notes_ref.as_deref())
    }

    /// Replace (or add) a note for `object_id` in `notes_ref`, returning the
    /// previous note blob id, or `null` when no note existed before.
    ///
    /// `notes_ref` may be a shorthand (`commits`, `review`), `notes/review`, or a
    /// fully qualified `refs/notes/review`; it is created if absent.
    ///
    /// Set `options.force` to `false` to refuse overwriting an existing note.
    #[napi]
    pub fn set_note(
        &self,
        notes_ref: String,
        object_id: String,
        content: String,
        options: Option<NoteChangeOptions>,
    ) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        compute_set_note(&r, &notes_ref, &object_id, &content, options.as_ref())
    }

    /// Remove the note for `object_id` in `notes_ref`, returning the removed note
    /// blob id, or `null` when there was no note to remove.
    #[napi]
    pub fn remove_note(
        &self,
        notes_ref: String,
        object_id: String,
        options: Option<NoteChangeOptions>,
    ) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        compute_remove_note(&r, &notes_ref, &object_id, options.as_ref())
    }

    /// Enumerate every note in `notes_ref` (or the default reference), returning
    /// each note's annotated object id, blob id, and content. This mirrors git2's
    /// `Notes` iterator, which git's high-level API does not expose directly.
    #[napi]
    pub fn list_notes(&self, notes_ref: Option<String>) -> napi::Result<Vec<ListNoteEntry>> {
        let r = thread_local(&self.inner);
        compute_list_notes(&r, notes_ref.as_deref())
    }

    /// Return the configured notes references in lookup order (the default
    /// reference plus any `notes.displayRef` entries).
    #[napi]
    pub fn notes_refs(&self) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        compute_notes_refs(&r)
    }

    // --- Async (libuv thread pool) variants ---------------------------------

    #[napi(ts_return_type = "Promise<Array<Note>>")]
    pub fn notes_for_async(&self, object_id: String) -> napi::Result<AsyncTask<NotesForTask>> {
        Ok(AsyncTask::new(NotesForTask {
            repo: Arc::clone(&self.inner),
            object_id,
        }))
    }

    #[napi(ts_return_type = "Promise<Array<Note>>")]
    pub fn notes_in_async(
        &self,
        refs: Vec<String>,
        object_id: String,
    ) -> napi::Result<AsyncTask<NotesInTask>> {
        Ok(AsyncTask::new(NotesInTask {
            repo: Arc::clone(&self.inner),
            refs,
            object_id,
        }))
    }

    #[napi(ts_return_type = "Promise<NoteInfo | null | undefined>")]
    pub fn find_note_async(
        &self,
        target_id: String,
        notes_ref: Option<String>,
    ) -> napi::Result<AsyncTask<FindNoteTask>> {
        Ok(AsyncTask::new(FindNoteTask {
            repo: Arc::clone(&self.inner),
            target_id,
            notes_ref,
        }))
    }

    #[napi(ts_return_type = "Promise<string | null | undefined>")]
    pub fn set_note_async(
        &self,
        notes_ref: String,
        object_id: String,
        content: String,
        options: Option<NoteChangeOptions>,
    ) -> napi::Result<AsyncTask<SetNoteTask>> {
        Ok(AsyncTask::new(SetNoteTask {
            repo: Arc::clone(&self.inner),
            notes_ref,
            object_id,
            content,
            options,
        }))
    }

    #[napi(ts_return_type = "Promise<string | null | undefined>")]
    pub fn remove_note_async(
        &self,
        notes_ref: String,
        object_id: String,
        options: Option<NoteChangeOptions>,
    ) -> napi::Result<AsyncTask<RemoveNoteTask>> {
        Ok(AsyncTask::new(RemoveNoteTask {
            repo: Arc::clone(&self.inner),
            notes_ref,
            object_id,
            options,
        }))
    }

    #[napi(ts_return_type = "Promise<Array<ListNoteEntry>>")]
    pub fn list_notes_async(
        &self,
        notes_ref: Option<String>,
    ) -> napi::Result<AsyncTask<ListNotesTask>> {
        Ok(AsyncTask::new(ListNotesTask {
            repo: Arc::clone(&self.inner),
            notes_ref,
        }))
    }
}

pub struct NotesForTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) object_id: String,
}
impl Task for NotesForTask {
    type Output = Vec<Note>;
    type JsValue = Vec<Note>;
    fn compute(&mut self) -> napi::Result<Vec<Note>> {
        let r = thread_local(&self.repo);
        compute_notes_for(&r, &self.object_id)
    }
    fn resolve(&mut self, _env: Env, output: Vec<Note>) -> napi::Result<Vec<Note>> {
        Ok(output)
    }
}

pub struct NotesInTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) refs: Vec<String>,
    pub(crate) object_id: String,
}
impl Task for NotesInTask {
    type Output = Vec<Note>;
    type JsValue = Vec<Note>;
    fn compute(&mut self) -> napi::Result<Vec<Note>> {
        let r = thread_local(&self.repo);
        compute_notes_in(&r, &self.refs, &self.object_id)
    }
    fn resolve(&mut self, _env: Env, output: Vec<Note>) -> napi::Result<Vec<Note>> {
        Ok(output)
    }
}

pub struct FindNoteTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target_id: String,
    pub(crate) notes_ref: Option<String>,
}
impl Task for FindNoteTask {
    type Output = Option<NoteInfo>;
    type JsValue = Option<NoteInfo>;
    fn compute(&mut self) -> napi::Result<Option<NoteInfo>> {
        let r = thread_local(&self.repo);
        compute_find_note(&r, &self.target_id, self.notes_ref.as_deref())
    }
    fn resolve(&mut self, _env: Env, output: Option<NoteInfo>) -> napi::Result<Option<NoteInfo>> {
        Ok(output)
    }
}

pub struct SetNoteTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) notes_ref: String,
    pub(crate) object_id: String,
    pub(crate) content: String,
    pub(crate) options: Option<NoteChangeOptions>,
}
impl Task for SetNoteTask {
    type Output = Option<String>;
    type JsValue = Option<String>;
    fn compute(&mut self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        compute_set_note(
            &r,
            &self.notes_ref,
            &self.object_id,
            &self.content,
            self.options.as_ref(),
        )
    }
    fn resolve(&mut self, _env: Env, output: Option<String>) -> napi::Result<Option<String>> {
        Ok(output)
    }
}

pub struct RemoveNoteTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) notes_ref: String,
    pub(crate) object_id: String,
    pub(crate) options: Option<NoteChangeOptions>,
}
impl Task for RemoveNoteTask {
    type Output = Option<String>;
    type JsValue = Option<String>;
    fn compute(&mut self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.repo);
        compute_remove_note(&r, &self.notes_ref, &self.object_id, self.options.as_ref())
    }
    fn resolve(&mut self, _env: Env, output: Option<String>) -> napi::Result<Option<String>> {
        Ok(output)
    }
}

pub struct ListNotesTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) notes_ref: Option<String>,
}
impl Task for ListNotesTask {
    type Output = Vec<ListNoteEntry>;
    type JsValue = Vec<ListNoteEntry>;
    fn compute(&mut self) -> napi::Result<Vec<ListNoteEntry>> {
        let r = thread_local(&self.repo);
        compute_list_notes(&r, self.notes_ref.as_deref())
    }
    fn resolve(
        &mut self,
        _env: Env,
        output: Vec<ListNoteEntry>,
    ) -> napi::Result<Vec<ListNoteEntry>> {
        Ok(output)
    }
}
