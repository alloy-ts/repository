use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// A note attached to an object, originating from a specific notes reference.
#[derive(Clone)]
#[napi(object)]
pub struct Note {
    /// The notes reference the note originated from, e.g. `refs/notes/commits`.
    pub reference: String,
    /// The object id of the note blob.
    pub blob_id: String,
    /// The note content as UTF-8 text.
    pub content: String,
}

#[napi]
impl Repository {
    /// Return the configured default notes reference (`core.notesRef`), if set.
    ///
    /// When unset, git's implicit default `refs/notes/commits` still applies to
    /// `notes_for`, but this getter returns `null` because `core.notesRef` was
    /// left untouched.
    #[napi]
    pub fn notes_default_ref(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.notes()
            .map_err(|e| err("notes", e))?
            .default_ref()
            .map(|n| n.as_bstr().to_string()))
    }

    /// Return all notes attached to `object_id` across the configured notes
    /// references, in display order.
    #[napi]
    pub fn notes_for(&self, object_id: String) -> Result<Vec<Note>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&object_id)?;
        let mut platform = r.notes().map_err(|e| err("notes", e))?;
        let mut out = Vec::new();
        for n in platform.get(oid).map_err(|e| err("notes_get", e))? {
            out.push(Note {
                reference: n.reference.as_bstr().to_string(),
                blob_id: n.blob.id.to_string(),
                content: String::from_utf8_lossy(&n.blob.data).into_owned(),
            });
        }
        Ok(out)
    }

    #[napi]
    pub fn notes_in(&self, refs: Vec<String>, object_id: String) -> Result<Vec<Note>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&object_id)?;
        let mut platform = r
            .notes()
            .map_err(|e| err("notes", e))?
            .with_refs(refs)
            .map_err(|e| err("notes_with_refs", e))?;
        let mut out = Vec::new();
        for n in platform.get(oid).map_err(|e| err("notes_get", e))? {
            out.push(Note {
                reference: n.reference.as_bstr().to_string(),
                blob_id: n.blob.id.to_string(),
                content: String::from_utf8_lossy(&n.blob.data).into_owned(),
            });
        }
        Ok(out)
    }

    /// Replace (or add) a note for `object_id` in `notes_ref`, returning the
    /// previous note blob id, or `null` when no note existed before.
    ///
    /// `notes_ref` may be a shorthand (`commits`, `review`), `notes/review`, or a
    /// fully qualified `refs/notes/review`; it is created if absent.
    #[napi]
    pub fn set_note(
        &self,
        notes_ref: String,
        object_id: String,
        content: String,
    ) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&object_id)?;
        let mut platform = r.notes().map_err(|e| err("notes", e))?;
        let previous = platform
            .replace(notes_ref.as_str(), oid, content.as_bytes())
            .map_err(|e| err("notes_replace", e))?;
        Ok(previous.map(|id| id.to_string()))
    }

    /// Remove the note for `object_id` in `notes_ref`, returning the removed note
    /// blob id, or `null` when there was no note to remove.
    #[napi]
    pub fn remove_note(&self, notes_ref: String, object_id: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&object_id)?;
        let mut platform = r.notes().map_err(|e| err("notes", e))?;
        let removed = platform
            .remove(notes_ref.as_str(), oid)
            .map_err(|e| err("notes_remove", e))?;
        Ok(removed.map(|id| id.to_string()))
    }
}
