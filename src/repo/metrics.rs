use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::DateTime as ChronoDateTime;
use gix::object::tree::diff::ChangeDetached;
use napi_derive::napi;

use crate::error::err;

/// Status of a changed file, mirroring git's delta classification.
#[derive(Copy, Clone)]
#[napi(string_enum)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Other,
}

/// One changed file within a commit.
#[napi(object)]
pub struct FileChange {
    pub path: String,
    pub status: ChangeStatus,
    pub insertions: u32,
    pub deletions: u32,
}

/// A parsed commit and its per-file changes.
#[napi(object)]
pub struct CommitInfo {
    pub hash: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub message: String,
    pub file_changes: Vec<FileChange>,
}

/// Filters controlling which commits/files are read by [`Metrics::new`].
#[napi(object)]
pub struct MetricsConfig {
    /// Skip merge commits (more than one parent).
    pub no_merges: Option<bool>,
    /// Only consider files whose path starts with this prefix.
    pub path: Option<String>,
    /// Only consider files whose path ends with one of these suffixes.
    pub include: Option<Vec<String>>,
    /// Exclude files whose path ends with one of these suffixes.
    pub exclude: Option<Vec<String>>,
}

#[napi(object)]
pub struct SummaryStats {
    pub commits: u32,
    pub files: u32,
    pub file_changes: u32,
    pub authors: u32,
}

#[napi(object)]
pub struct OwnerStats {
    pub commits: u32,
    pub file_changes: u32,
    pub files: u32,
    pub revisions: u32,
}

#[napi(object)]
pub struct DecayEntry {
    pub file: String,
    pub score: f64,
}

#[napi(object)]
pub struct CouplingEntry {
    pub file_a: String,
    pub file_b: String,
    pub percent: f64,
}

#[napi(object)]
pub struct OwnershipEntry {
    pub file: String,
    pub owner: String,
}

#[napi(object)]
pub struct CommunicationEntry {
    pub author_a: String,
    pub author_b: String,
    pub count: u32,
}

#[napi(object)]
pub struct ChurnEntry {
    pub file: String,
    pub revisions: u32,
    pub insertions: u32,
    pub deletions: u32,
    pub created: String,
    pub last_modified: String,
}

/// Code-health metrics over a repository's history. Construct once (it parses the full
/// history), then call the analysis methods, which operate on the parsed [`CommitInfo`]s.
#[napi]
pub struct Metrics {
    inner: Vec<CommitInfo>,
}

#[napi]
impl Metrics {
    /// Open `repo_path`, walk its history, and collect per-commit [`CommitInfo`]s.
    #[napi(constructor)]
    pub fn new(repo_path: String, config: Option<MetricsConfig>) -> napi::Result<Self> {
        let config = config.unwrap_or(MetricsConfig {
            no_merges: None,
            path: None,
            include: None,
            exclude: None,
        });
        let repo = gix::discover(&repo_path).map_err(|e| err("discover", e))?;
        let head = repo.head_commit().map_err(|e| err("head_commit", e))?;
        let tips = std::iter::once(head.id().detach());
        let walk = repo.rev_walk(tips).all().map_err(|e| err("rev_walk", e))?;

        let mut commits = Vec::new();
        for item in walk {
            let info = item.map_err(|e| err("revwalk_next", e))?;
            let oid = info.id;
            let commit = repo.find_commit(oid).map_err(|e| err("find_commit", e))?;

            if config.no_merges.unwrap_or(false) && commit.parent_ids().count() > 1 {
                continue;
            }

            let author = commit.author().map_err(|e| err("author", e))?;
            let author_name = author.name.to_string();
            let author_email = author.email.to_string();
            let timestamp = author.time().map_err(|e| err("author_time", e))?.seconds;
            let message = String::from_utf8_lossy(
                commit
                    .message_raw()
                    .map_err(|e| err("message", e))?
                    .as_ref(),
            )
            .into_owned();

            let new_tree = commit.tree().map_err(|e| err("tree", e))?;
            let parent_tree = commit
                .parent_ids()
                .next()
                .and_then(|oid| repo.find_commit(oid).ok())
                .and_then(|p| p.tree().ok());
            let changes = repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&new_tree), None)
                .map_err(|e| err("diff_tree_to_tree", e))?;

            let mut file_changes = Vec::new();
            for change in changes {
                let (location, status, old_id, new_id) = match change {
                    ChangeDetached::Addition { location, id, .. } => {
                        (location, ChangeStatus::Added, None, Some(id))
                    }
                    ChangeDetached::Deletion { location, id, .. } => {
                        (location, ChangeStatus::Deleted, Some(id), None)
                    }
                    ChangeDetached::Modification {
                        location,
                        previous_id,
                        id,
                        ..
                    } => (
                        location,
                        ChangeStatus::Modified,
                        Some(previous_id),
                        Some(id),
                    ),
                    ChangeDetached::Rewrite {
                        location,
                        source_id,
                        id,
                        copy,
                        ..
                    } => (
                        location,
                        if copy {
                            ChangeStatus::Copied
                        } else {
                            ChangeStatus::Renamed
                        },
                        Some(source_id),
                        Some(id),
                    ),
                };

                let path = location.to_string();
                let old_bytes = old_id
                    .and_then(|oid| repo.find_object(oid).ok())
                    .and_then(|o| o.try_into_blob().ok())
                    .map(|b| b.data.to_vec());
                let new_bytes = new_id
                    .and_then(|oid| repo.find_object(oid).ok())
                    .and_then(|o| o.try_into_blob().ok())
                    .map(|b| b.data.to_vec());

                let (insertions, deletions) = match (old_bytes, new_bytes) {
                    (Some(o), Some(n)) => count_line_changes(&o, &n),
                    (None, Some(n)) => (count_lines(&n), 0),
                    (Some(o), None) => (0, count_lines(&o)),
                    (None, None) => (0, 0),
                };

                file_changes.push(FileChange {
                    path,
                    status,
                    insertions: insertions as u32,
                    deletions: deletions as u32,
                });
            }

            let file_changes = file_changes
                .into_iter()
                .filter(|fc| {
                    let p = &fc.path;
                    config
                        .path
                        .as_ref()
                        .map_or(true, |prefix| p.starts_with(prefix))
                        && config
                            .include
                            .as_ref()
                            .map_or(true, |inc| inc.iter().any(|s| p.ends_with(s)))
                        && !config
                            .exclude
                            .as_ref()
                            .map_or(false, |exc| exc.iter().any(|s| p.ends_with(s)))
                })
                .collect();

            commits.push(CommitInfo {
                hash: oid.to_string(),
                author_name,
                author_email,
                timestamp,
                message,
                file_changes,
            });
        }

        Ok(Metrics { inner: commits })
    }

    #[napi]
    pub fn summary(&self) -> napi::Result<SummaryStats> {
        Ok(get_summary(&self.inner))
    }

    #[napi]
    pub fn owner_summary(&self) -> napi::Result<HashMap<String, OwnerStats>> {
        Ok(get_owner_summary(&self.inner))
    }

    #[napi]
    pub fn file_statuses(&self) -> napi::Result<HashMap<String, ChangeStatus>> {
        Ok(get_file_statuses(&self.inner))
    }

    #[napi]
    pub fn owners(&self) -> napi::Result<HashMap<String, HashMap<String, u32>>> {
        Ok(get_owners(&self.inner))
    }

    #[napi]
    pub fn primary_owners(&self) -> napi::Result<HashMap<String, String>> {
        Ok(get_primary_owners(&get_owners(&self.inner)))
    }

    #[napi]
    pub fn decay(&self, threshold_days: i64, top: Option<u32>) -> napi::Result<Vec<DecayEntry>> {
        let decay = filter_deleted(get_decay(&self.inner, threshold_days), &self.inner);
        let mut entries: Vec<DecayEntry> = decay
            .into_iter()
            .map(|(file, score)| DecayEntry { file, score })
            .collect();
        entries.sort_by(|a, b| b.score.total_cmp(&a.score));
        entries.truncate(top.unwrap_or(u32::MAX) as usize);
        Ok(entries)
    }

    #[napi]
    pub fn coupling(
        &self,
        max_changeset_size: u32,
        min_coupling_percent: f64,
        min_revision_count: u32,
        top: Option<u32>,
    ) -> napi::Result<Vec<CouplingEntry>> {
        let coupling = get_coupling_percentage(
            &self.inner,
            max_changeset_size as usize,
            min_revision_count as usize,
        );
        let file_statuses = get_file_statuses(&self.inner);
        let mut entries: Vec<CouplingEntry> = coupling
            .into_iter()
            .filter(|((a, b), p)| {
                !matches!(file_statuses.get(a), Some(ChangeStatus::Deleted))
                    && !matches!(file_statuses.get(b), Some(ChangeStatus::Deleted))
                    && *p > min_coupling_percent / 100.0
            })
            .map(|((file_a, file_b), percent)| CouplingEntry {
                file_a,
                file_b,
                percent,
            })
            .collect();
        entries.sort_by(|a, b| b.percent.total_cmp(&a.percent));
        entries.truncate(top.unwrap_or(u32::MAX) as usize);
        Ok(entries)
    }

    #[napi]
    pub fn communication(&self, top: Option<u32>) -> napi::Result<Vec<CommunicationEntry>> {
        let mut owner_coupling: Vec<((String, String), u32)> =
            get_owner_coupling(&self.inner).into_iter().collect();
        owner_coupling.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(owner_coupling
            .into_iter()
            .take(top.unwrap_or(u32::MAX) as usize)
            .map(|((author_a, author_b), count)| CommunicationEntry {
                author_a,
                author_b,
                count,
            })
            .collect())
    }

    #[napi]
    pub fn churn(
        &self,
        top: Option<u32>,
        filtered: Option<Vec<CommitInfo>>,
    ) -> napi::Result<Vec<ChurnEntry>> {
        let commits = &self.inner;
        let filtered_ref = filtered.as_deref().unwrap_or(&self.inner);
        let created = get_files_creation(commits);
        let last_modified = get_files_last_modified(commits);
        let line_changes = get_line_changes(filtered_ref);
        let revisions = get_revision_counts(filtered_ref);

        let mut entries: Vec<ChurnEntry> = line_changes
            .into_iter()
            .map(|(file, (insertions, deletions))| ChurnEntry {
                file: file.clone(),
                revisions: *revisions.get(&file).unwrap_or(&0),
                insertions,
                deletions,
                created: ts_to_date(*created.get(&file).unwrap_or(&0)),
                last_modified: ts_to_date(*last_modified.get(&file).unwrap_or(&0)),
            })
            .collect();
        entries.sort_by(|a, b| a.file.cmp(&b.file));
        entries.truncate(top.unwrap_or(u32::MAX) as usize);
        Ok(entries)
    }
}

// ---------- analytics (pure, operate on &[CommitInfo]) ----------

const DAYS_TO_SECONDS: i64 = 86_400;

fn ts_to_date(ts: i64) -> String {
    ChronoDateTime::from_timestamp(ts, 0)
        .map(|d| d.date_naive().to_string())
        .unwrap_or_default()
}

fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = b.split(|&c| c == b'\n').collect();
    if b.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
}

fn count_lines(b: &[u8]) -> usize {
    split_lines(b).len()
}

/// Longest-common-subsequence line diff → (insertions, deletions). Bounded to avoid pathological
/// O(n*m) on huge files; falls back to a full-line delta above the threshold.
fn count_line_changes(old: &[u8], new: &[u8]) -> (usize, usize) {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let n = old_lines.len();
    let m = new_lines.len();
    if n == 0 {
        return (m, 0);
    }
    if m == 0 {
        return (0, n);
    }
    if (n as u64) * (m as u64) > 4_000_000 {
        return (m, n);
    }
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if old_lines[i] == new_lines[j] {
                dp[(i + 1) * w + (j + 1)] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + (j + 1)])
            };
        }
    }
    let lcs = dp[0] as usize;
    (m - lcs, n - lcs)
}

fn combinations2<T: Clone>(items: &[T]) -> Vec<(T, T)> {
    let mut out = Vec::new();
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            out.push((items[i].clone(), items[j].clone()));
        }
    }
    out
}

pub fn get_summary(commits: &[CommitInfo]) -> SummaryStats {
    let files = get_file_statuses(commits)
        .iter()
        .filter(|(_, s)| !matches!(s, ChangeStatus::Deleted))
        .count();
    let file_changes: u32 = commits.iter().map(|p| p.file_changes.len() as u32).sum();
    let authors = get_user_last_active(commits).len();
    SummaryStats {
        commits: commits.len() as u32,
        files: files as u32,
        file_changes,
        authors: authors as u32,
    }
}

pub fn get_owner_summary(commits: &[CommitInfo]) -> HashMap<String, OwnerStats> {
    let mut map: HashMap<String, (u32, u32, HashSet<String>, u32)> = HashMap::new();
    for commit in commits {
        let e = map.entry(commit.author_name.clone()).or_default();
        e.0 += 1;
        e.1 += commit.file_changes.len() as u32;
        e.3 += commit
            .file_changes
            .iter()
            .map(|f| f.insertions + f.deletions)
            .sum::<u32>();
        for f in &commit.file_changes {
            e.2.insert(f.path.clone());
        }
    }
    map.into_iter()
        .map(|(name, (commits, file_changes, files, revisions))| {
            (
                name,
                OwnerStats {
                    commits,
                    file_changes,
                    files: files.len() as u32,
                    revisions,
                },
            )
        })
        .collect()
}

pub fn get_file_statuses(commits: &[CommitInfo]) -> HashMap<String, ChangeStatus> {
    let mut out: HashMap<String, ChangeStatus> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            out.entry(f.path.clone()).or_insert(f.status);
        }
    }
    out
}

pub fn get_owners(commits: &[CommitInfo]) -> HashMap<String, HashMap<String, u32>> {
    let mut files: HashMap<String, HashMap<String, u32>> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            *files
                .entry(f.path.clone())
                .or_default()
                .entry(commit.author_name.clone())
                .or_insert(0) += 1;
        }
    }
    files
}

pub fn get_coupling(
    commits: &[CommitInfo],
    max_changeset_size: usize,
) -> HashMap<(String, String), u32> {
    let mut couplings: HashMap<(String, String), u32> = HashMap::new();
    for commit in commits
        .iter()
        .filter(|p| p.file_changes.len() <= max_changeset_size)
    {
        let mut changed: Vec<String> = commit.file_changes.iter().map(|p| p.path.clone()).collect();
        changed.sort();
        for pair in combinations2(&changed) {
            *couplings.entry(pair).or_insert(0) += 1;
        }
    }
    couplings
}

pub fn get_owner_coupling(commits: &[CommitInfo]) -> HashMap<(String, String), u32> {
    let mut out: HashMap<(String, String), u32> = HashMap::new();
    let file_owners = get_owners(commits);
    for owners in file_owners.values() {
        let mut keys: Vec<&String> = owners.keys().collect();
        keys.sort();
        for pair in combinations2(&keys) {
            *out.entry((pair.0.clone(), pair.1.clone())).or_insert(0) += 1;
        }
    }
    out
}

pub fn get_primary_owners(
    file_owners: &HashMap<String, HashMap<String, u32>>,
) -> HashMap<String, String> {
    file_owners
        .iter()
        .map(|(path, authors)| {
            let primary = authors
                .iter()
                .max_by_key(|(_, c)| *c)
                .map(|(a, _)| a.clone())
                .unwrap_or_default();
            (path.clone(), primary)
        })
        .collect()
}

pub fn get_user_last_active(commits: &[CommitInfo]) -> HashMap<String, i64> {
    let mut users: HashMap<String, i64> = HashMap::new();
    for commit in commits {
        users
            .entry(commit.author_name.clone())
            .or_insert(commit.timestamp);
    }
    users
}

pub fn get_revision_counts(commits: &[CommitInfo]) -> HashMap<String, u32> {
    let mut out: HashMap<String, u32> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            *out.entry(f.path.clone()).or_insert(0) += 1;
        }
    }
    out
}

pub fn get_line_changes(commits: &[CommitInfo]) -> HashMap<String, (u32, u32)> {
    let mut out: HashMap<String, (u32, u32)> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            let e = out.entry(f.path.clone()).or_insert((0, 0));
            e.0 += f.insertions;
            e.1 += f.deletions;
        }
    }
    out
}

pub fn get_files_last_modified(commits: &[CommitInfo]) -> HashMap<String, i64> {
    let mut out: HashMap<String, i64> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            out.entry(f.path.clone()).or_insert(commit.timestamp);
        }
    }
    out
}

pub fn get_files_creation(commits: &[CommitInfo]) -> HashMap<String, i64> {
    let mut out: HashMap<String, i64> = HashMap::new();
    for commit in commits {
        for f in &commit.file_changes {
            out.insert(f.path.clone(), commit.timestamp);
        }
    }
    out
}

pub fn filter_deleted<V>(files: HashMap<String, V>, commits: &[CommitInfo]) -> HashMap<String, V> {
    let file_statuses = get_file_statuses(commits);
    files
        .into_iter()
        .filter(|(p, _)| !matches!(file_statuses.get(p), Some(ChangeStatus::Deleted)))
        .collect()
}

pub fn get_decay(commits: &[CommitInfo], decay_threshold: i64) -> HashMap<String, f64> {
    let decay_threshold = decay_threshold * DAYS_TO_SECONDS;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let files_last_modified = get_files_last_modified(commits);
    let file_owners = get_owners(commits);
    let file_primary_owners = get_primary_owners(&file_owners);
    let file_concentrations = get_file_concentrations(&file_owners, &file_primary_owners);
    let users_last_active = get_user_last_active(commits);

    let mut file_decays: HashMap<String, f64> = HashMap::new();
    for path in file_owners.keys() {
        let last_modified = *files_last_modified.get(path).unwrap();
        let primary_owner = file_primary_owners.get(path).unwrap();
        let concentration = *file_concentrations.get(path).unwrap();
        let user_last_active = *users_last_active.get(primary_owner).unwrap();

        let staleness = ((time - last_modified) as f64 / decay_threshold as f64).min(1.0);
        let inactivity = ((time - user_last_active) as f64 / decay_threshold as f64).min(1.0);
        let decay = (0.7 * staleness + 0.3 * inactivity * concentration).min(1.0);
        file_decays.insert(path.clone(), decay);
    }
    file_decays
}

pub fn get_file_concentrations(
    file_owners: &HashMap<String, HashMap<String, u32>>,
    file_primary_owners: &HashMap<String, String>,
) -> HashMap<String, f64> {
    let mut files: HashMap<String, f64> = HashMap::new();
    for (path, authors) in file_owners {
        let total: u32 = authors.values().sum();
        let concentration =
            *authors.get(file_primary_owners.get(path).unwrap()).unwrap() as f64 / total as f64;
        files.insert(path.clone(), concentration);
    }
    files
}

pub fn get_coupling_percentage(
    commits: &[CommitInfo],
    max_changeset_size: usize,
    min_revision: usize,
) -> HashMap<(String, String), f64> {
    let min_revision = min_revision as u32;
    let mut out: HashMap<(String, String), f64> = HashMap::new();
    let couplings = get_coupling(commits, max_changeset_size);
    let modifications = get_revision_counts(commits);
    for ((file1, file2), count) in couplings {
        let rev1 = *modifications.get(&file1).unwrap();
        let rev2 = *modifications.get(&file2).unwrap();
        if rev1 < min_revision || rev2 < min_revision {
            continue;
        }
        let percentage = count as f64 / rev1.min(rev2) as f64;
        out.insert((file1, file2), percentage);
    }
    out
}
