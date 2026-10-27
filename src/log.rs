//! Log / history bindings: the flat [`CommitInfo`] DTO returned by `Repository::log`, plus the
//! `log` method itself and the commit-decoding helper it shares with `Commit::info`.

use crate::error::{RepositoryResult as Result, err, parse_oid, thread_local};
use crate::object::{lossy, message_body, rfc3339, short_sha};
use crate::repository::Repository;

use gix::ObjectId;
use napi_derive::napi;

/// A flat, JSON-serializable snapshot of a commit, as returned by [`Repository::log`]. Mirrors the
/// fields of a typical `git log` entry so callers can render history without opening each commit.
#[derive(Clone)]
#[napi(object)]
pub struct CommitInfo {
    /// Full object id (SHA-1 / SHA-256).
    pub sha: String,
    /// Abbreviated object id: the shortest unambiguous prefix (never shorter than 7 characters).
    pub short_sha: String,
    /// Full commit message.
    pub message: String,
    /// First line (subject) of the message.
    pub subject: String,
    /// Message body: everything after the first paragraph, trimmed; empty when there is none.
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    /// Authoring time, unix seconds.
    pub author_time: i64,
    /// Authoring time as an RFC 3339 / ISO 8601 string.
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    /// Committing time, unix seconds.
    pub committer_time: i64,
    /// Committing time as an RFC 3339 / ISO 8601 string.
    pub committer_date: String,
    pub parent_ids: Vec<String>,
    pub tree_id: String,
    /// Raw `gpgsig` / `gpgsig-sha256` header when the commit is signed, else `null`.
    pub signature: Option<String>,
}

#[napi]
impl Repository {
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
    ) -> napi::Result<Vec<CommitInfo>> {
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
}

/// Decode a commit into owned data for [`CommitInfo`], returning the entry plus its parent ids.
/// Shared by `Repository::log` and `Commit::info`.
pub(crate) fn log_summary(
    r: &gix::Repository,
    id: gix::ObjectId,
) -> napi::Result<(CommitInfo, Vec<String>)> {
    let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
    let c = commit.decode().map_err(|e| err("decode commit", e))?;
    let author = c.author().map_err(|e| err("author", e))?;
    let committer = c.committer().map_err(|e| err("committer", e))?;
    let at = author.time().map_err(|e| err("author time", e))?;
    let ct = committer.time().map_err(|e| err("committer time", e))?;
    let full = lossy(c.message);
    let subject = full.split('\n').next().unwrap_or("").to_string();
    let start = full.bytes().take_while(|&b| b == b'\n').count();
    let body = message_body(&full.as_bytes()[start..]).unwrap_or_default();
    let parent_ids: Vec<String> = c.parents().map(|p| p.to_string()).collect();
    let signature = c
        .extra_headers()
        .find("gpgsig")
        .or_else(|| c.extra_headers().find("gpgsig-sha256"))
        .map(|v| lossy(v));
    let entry = CommitInfo {
        sha: id.to_string(),
        short_sha: short_sha(r, &id),
        message: full,
        subject,
        body,
        author_name: lossy(author.name),
        author_email: lossy(author.email),
        author_time: at.seconds,
        author_date: rfc3339(at.seconds, at.offset),
        committer_name: lossy(committer.name),
        committer_email: lossy(committer.email),
        committer_time: ct.seconds,
        committer_date: rfc3339(ct.seconds, ct.offset),
        parent_ids: parent_ids.clone(),
        tree_id: c.tree().to_string(),
        signature,
    };
    Ok((entry, parent_ids))
}
