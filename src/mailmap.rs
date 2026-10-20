use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::identity::{Signature, to_signature};
use crate::repository::Repository;

/// A resolved mailmap, mapping a commit's author/committer identity to the
/// canonical identity declared in `.mailmap`.
///
/// Obtain one with [`Repository::openMailmap()`]. Note that git treats a missing or
/// malformed mailmap as an empty one, so `openMailmap()` returns `null` when there is
/// nothing to apply — which is indistinguishable from an unpopulated mailmap.
#[napi]
pub struct Mailmap {
    pub(crate) inner: gix::mailmap::Snapshot,
}

#[napi]
impl Mailmap {
    /// The number of mailmap entries that were merged in.
    #[napi]
    pub fn len(&self) -> u32 {
        self.inner.entries().len() as u32
    }

    /// Resolve the identity for `name`/`email` through the mailmap, or `null` if there
    /// is no entry that applies.
    ///
    /// This is the canonical form used by `git log`/`git shortlog` to display authors.
    #[napi]
    pub fn resolve(&self, name: String, email: String) -> Option<Signature> {
        let signature = gix::actor::SignatureRef {
            name: gix::bstr::BStr::new(name.as_bytes()),
            email: gix::bstr::BStr::new(email.as_bytes()),
            time: "",
        };
        self.inner.try_resolve(signature).map(|resolved| {
            to_signature(
                &resolved.name,
                &resolved.email,
                resolved.time.seconds,
                resolved.time.offset,
            )
        })
    }
}

#[napi]
impl Repository {
    /// Open and merge the mailmaps that apply to this repository, or `null` if the
    /// resulting mailmap is empty (no `.mailmap` in the worktree, and nothing
    /// configured via `mailmap.blob`/`mailmap.file`).
    ///
    /// Sources are consulted in the same order git uses: the worktree `.mailmap` (or
    /// `HEAD:.mailmap` for bare repositories), then `mailmap.blob`, then `mailmap.file`.
    #[napi]
    pub fn open_mailmap(&self) -> Result<Option<Mailmap>> {
        let r = thread_local(&self.inner);
        let snapshot = r.open_mailmap();
        if snapshot.entries().is_empty() {
            return Ok(None);
        }
        Ok(Some(Mailmap { inner: snapshot }))
    }

    /// Merge additional mailmap files into `mailmap`, reporting the first error
    /// encountered. Mirrors [`gix::Repository::open_mailmap_into()`].
    #[napi]
    pub fn open_mailmap_into(&self, mailmap: &mut Mailmap) -> Result<()> {
        let r = thread_local(&self.inner);
        r.open_mailmap_into(&mut mailmap.inner)
            .map_err(|e| err("open_mailmap_into", e))
    }
}
