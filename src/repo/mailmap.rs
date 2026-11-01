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
/// A single mailmap mapping: an old identity — always keyed by `oldEmail`,
/// optionally by `oldName` too — rewritten to the canonical (new) name and/or email.
#[napi(object)]
pub struct MailmapEntry {
    /// The canonical name to map to; omitted when the entry only rewrites emails.
    pub new_name: Option<String>,
    /// The canonical email to map to; omitted when the entry only rewrites names.
    pub new_email: Option<String>,
    /// The name that must additionally match for this entry to apply; omitted to
    /// match any name with `oldEmail`.
    pub old_name: Option<String>,
    /// The email this entry matches on (case-insensitively).
    pub old_email: String,
}

#[napi]
pub struct Mailmap {
    pub(crate) inner: gix::mailmap::Snapshot,
}

#[napi]
impl Mailmap {
    /// Create an empty, in-memory mailmap with no entries.
    ///
    /// Combine with [`addEntry`](Self::addEntry) to build mappings programmatically,
    /// or with [`Repository::openMailmapInto()`] to merge the repository's mailmap
    /// sources into it.
    #[napi(factory)]
    pub fn empty() -> Self {
        Mailmap {
            inner: gix::mailmap::Snapshot::default(),
        }
    }

    /// Parse `buf` (in `.mailmap` format) into an in-memory mailmap.
    ///
    /// Lines that fail to parse are skipped, matching git's tolerance for malformed
    /// mailmaps; use [`entries`](Self::entries) to inspect what was kept.
    #[napi(factory)]
    pub fn from_buffer(buf: String) -> Self {
        Mailmap {
            inner: gix::mailmap::Snapshot::from_bytes(buf.as_bytes()),
        }
    }

    /// The number of mailmap entries that were merged in.
    #[napi]
    pub fn len(&self) -> u32 {
        self.inner.entries().len() as u32
    }

    /// Whether this mailmap has no entries.
    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.inner.iter().next().is_none()
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

    /// Resolve `sig` through the mailmap, libgit2-style: the mapped name and/or email
    /// replace the original while the timestamp is preserved, and when no entry
    /// applies `sig` is returned unchanged.
    #[napi]
    pub fn resolve_signature(&self, sig: Signature) -> Signature {
        let time_str = gix::date::Time {
            seconds: sig.time,
            offset: sig.timezone_offset,
        }
        .to_string();
        let resolved = self.inner.try_resolve(gix::actor::SignatureRef {
            name: gix::bstr::BStr::new(sig.name.as_bytes()),
            email: gix::bstr::BStr::new(sig.email.as_bytes()),
            time: time_str.as_str(),
        });
        match resolved {
            Some(r) => to_signature(&r.name, &r.email, r.time.seconds, r.time.offset),
            None => sig,
        }
    }

    /// Add one entry to this in-memory mailmap, mirroring `git_mailmap_add_entry`.
    ///
    /// `replaceEmail` is the only required field: it is the identity the entry
    /// matches on. `realName`/`realEmail` are the canonical values to rewrite to,
    /// and `replaceName` additionally constrains the match to that name.
    ///
    /// When both `realName` and `realEmail` are `null` the call is a no-op (there
    /// is nothing to map to). gix cannot express a name-only rewrite constrained
    /// by `replaceName`, so when `realName` is set but `realEmail` is not, the
    /// entry matches `replaceEmail` alone.
    #[napi]
    pub fn add_entry(
        &mut self,
        real_name: Option<String>,
        real_email: Option<String>,
        replace_name: Option<String>,
        replace_email: String,
    ) -> napi::Result<()> {
        // Treat `""` like an absent field.
        let real_name = real_name.filter(|s| !s.is_empty());
        let real_email = real_email.filter(|s| !s.is_empty());
        let replace_name = replace_name.filter(|s| !s.is_empty());

        // Borrow as `&str` so the entry can borrow from the locals, which outlive
        // the `merge()` call below (the snapshot copies into owned storage).
        let entry = match (
            real_name.as_deref(),
            real_email.as_deref(),
            replace_name.as_deref(),
        ) {
            (Some(name), Some(email), Some(old_name)) => {
                gix::mailmap::Entry::change_name_and_email_by_name_and_email(
                    name,
                    email,
                    old_name,
                    replace_email.as_str(),
                )
            }
            (Some(name), Some(email), None) => gix::mailmap::Entry::change_name_and_email_by_email(
                name,
                email,
                replace_email.as_str(),
            ),
            // gix has no name-only rewrite matched by name+email; match by email alone.
            (Some(name), None, _) => {
                gix::mailmap::Entry::change_name_by_email(name, replace_email.as_str())
            }
            (None, Some(email), Some(old_name)) => {
                gix::mailmap::Entry::change_email_by_name_and_email(
                    email,
                    old_name,
                    replace_email.as_str(),
                )
            }
            (None, Some(email), None) => {
                gix::mailmap::Entry::change_email_by_email(email, replace_email.as_str())
            }
            // Nothing to map to; gix ignores such entries too.
            (None, None, _) => return Ok(()),
        };
        self.inner.merge([entry]);
        Ok(())
    }

    /// All entries in this mailmap, ordered by `oldEmail` and then `oldName` (gix's
    /// internal ordering, not the original file order).
    #[napi]
    pub fn entries(&self) -> Vec<MailmapEntry> {
        self.inner
            .iter()
            .map(|e| MailmapEntry {
                new_name: e
                    .new_name()
                    .map(|v| String::from_utf8_lossy(v).into_owned()),
                new_email: e
                    .new_email()
                    .map(|v| String::from_utf8_lossy(v).into_owned()),
                old_name: e
                    .old_name()
                    .map(|v| String::from_utf8_lossy(v).into_owned()),
                old_email: String::from_utf8_lossy(e.old_email()).into_owned(),
            })
            .collect()
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
    pub fn open_mailmap(&self) -> napi::Result<Option<Mailmap>> {
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
    pub fn open_mailmap_into(&self, mailmap: &mut Mailmap) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        r.open_mailmap_into(&mut mailmap.inner)
            .map_err(|e| err("open_mailmap_into", e))
    }
}
