use std::collections::BTreeMap;
use std::path::Path;

use napi::Result;
use napi_derive::napi;
use serde::Deserialize;

/// One entry of the manifest's `remote:` list.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RemoteSpec {
    /// A bare URL; the name is derived (the first entry becomes `origin`).
    Url(String),
    /// An explicit `{ name: ..., url: ... }` mapping.
    Named { name: String, url: String },
}

/// The subset of `repository.manifest.yml` this module understands.
#[derive(Debug, Default, Deserialize)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    /// The manifest's `type:` field (e.g. `application`).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub owner: Option<String>,
    pub description: Option<String>,
    /// Single-entry config maps to apply, e.g. `- commit.gpgsign: true`.
    #[serde(default)]
    pub config: Vec<BTreeMap<String, serde_yaml::Value>>,
    /// Declared remotes, in priority order.
    #[serde(default)]
    pub remote: Vec<RemoteSpec>,
}

impl Manifest {
    /// Read and parse the manifest at `path` on the filesystem (working-tree path).
    pub fn read(path: impl AsRef<Path>) -> Result<Manifest> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| napi::Error::from_reason(format!("read {}: {e}", path.display())))?;
        serde_yaml::from_str(&text)
            .map_err(|e| napi::Error::from_reason(format!("parse {}: {e}", path.display())))
    }
    pub fn read_at(repo: &gix::Repository, revision: &str, rel_path: &str) -> Result<Manifest> {
        // 1) Resolve the revision spec to a single object id (commit/tag/tree/blob).
        let id = repo
            .rev_parse_single(revision)
            .map_err(|e| napi::Error::from_reason(format!("rev_parse {revision}: {e}")))?;
        // 2) Load the object and peel whatever it is down to its tree.
        let tree = id
            .object()
            .map_err(|e| napi::Error::from_reason(format!("object for {revision}: {e}")))?
            .peel_to_kind(gix::objs::Kind::Tree)
            .map_err(|e| napi::Error::from_reason(format!("peel {revision} to tree: {e}")))?
            .try_into_tree()
            .map_err(|_| napi::Error::from_reason(format!("{revision} is not a tree-ish")))?;
        // 3) Look the manifest path up in the tree.
        let entry = tree
            .lookup_entry_by_path(rel_path)
            .map_err(|e| napi::Error::from_reason(format!("lookup {rel_path}: {e}")))?
            .ok_or_else(|| {
                napi::Error::from_reason(format!("no {rel_path} at revision {revision}"))
            })?;
        // 4) Materialize and decode the blob.
        let blob = entry
            .object()
            .map_err(|e| napi::Error::from_reason(format!("read {rel_path}: {e}")))?
            .try_into_blob()
            .map_err(|_| {
                napi::Error::from_reason(format!("{rel_path} at {revision} is not a blob"))
            })?;
        let text = String::from_utf8_lossy(&blob.data).into_owned();
        serde_yaml::from_str(&text)
            .map_err(|e| napi::Error::from_reason(format!("parse {rel_path} at {revision}: {e}")))
    }
}

/// Render a scalar YAML value as a git-config value string.
fn scalar_to_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None, // sequences/mappings/null have no single-value config form
    }
}

/// The host part of a git URL: `https://host/path`, `git@host:path`, `host/path`.
fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    // Strip any `user@` prefix (scp-like syntax has no scheme).
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let host = rest.split(['/', ':', '?', '#']).next()?;
    (!host.is_empty()).then_some(host)
}

/// Derive a remote name from a URL: the host's domain label — `github.com` ->
/// `github`, `dev.azure.com` -> `azure`, `localhost` -> `localhost`.
fn remote_name_for_url(url: &str) -> String {
    let host = url_host(url).unwrap_or_default().to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let name = match labels.as_slice() {
        [] => "remote",
        [single] => single,
        [.., domain, _tld] => domain,
    };
    if name.is_empty() {
        "remote".to_string()
    } else {
        name.to_string()
    }
}

/// `name`, or `name-2`, `name-3`, ... when it is already taken.
fn dedup(name: String, taken: &[String]) -> String {
    if !taken.contains(&name) {
        return name;
    }
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an untaken suffix always exists")
}

/// Resolve manifest entries to `(name, url)` pairs, in manifest order.
fn resolve_remotes(specs: &[RemoteSpec]) -> Vec<(String, String)> {
    let mut taken: Vec<String> = Vec::new();
    specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let (name, url) = match spec {
                RemoteSpec::Url(url) => {
                    // The first URL is the primary remote, hence `origin`.
                    let name = if i == 0 {
                        "origin".to_string()
                    } else {
                        remote_name_for_url(url)
                    };
                    (name, url.clone())
                }
                RemoteSpec::Named { name, url } => (name.clone(), url.clone()),
            };
            let name = dedup(name, &taken);
            taken.push(name.clone());
            (name, url)
        })
        .collect()
}

/// Configure `manifest`'s remotes (and config entries) on `repo`, returning the
/// remote names in manifest order.
pub fn configure_remotes(repo: &mut gix::Repository, manifest: &Manifest) -> Result<Vec<String>> {
    let config_path = repo
        .config_path(gix::config::Source::Local)
        .map_err(|e| napi::Error::from_reason(format!("config_path: {e}")))?;

    // One transaction for the whole manifest: remotes and config land atomically.
    let mut tx = repo
        .config_file_mut(&config_path)
        .map_err(|e| napi::Error::from_reason(format!("open {}: {e}", config_path.display())))?;

    let mut names = Vec::new();
    for (name, url) in resolve_remotes(&manifest.remote) {
        // Anonymous in-memory remote with the standard fetch refspec...
        let mut remote = repo
            .remote_at(url.as_str())
            .map_err(|e| napi::Error::from_reason(format!("remote_at {url}: {e}")))?
            .with_refspecs(
                [format!("+refs/heads/*:refs/remotes/{name}/*").as_str()],
                gix::remote::Direction::Fetch,
            )
            .map_err(|e| napi::Error::from_reason(format!("refspec for {name}: {e}")))?;
        // ...persisted as `[remote "<name>"]` (replacing any previous section).
        remote
            .save_as_to(name.as_str(), &mut tx)
            .map_err(|e| napi::Error::from_reason(format!("save remote {name}: {e}")))?;
        names.push(name);
    }

    // Manifest `config:` entries, e.g. `commit.gpgsign: true`.
    for entry in &manifest.config {
        for (dotted_key, value) in entry {
            let Some(value) = scalar_to_string(value) else {
                continue; // only scalars are representable as config values
            };
            let (section_name, key) = dotted_key
                .split_once('.')
                .unwrap_or(("core", dotted_key.as_str()));
            let mut section = tx
                .section_mut_or_create_new(section_name, None::<&gix::bstr::BStr>)
                .map_err(|e| napi::Error::from_reason(format!("section {section_name}: {e}")))?;
            section
                .set(key, value.as_str())
                .map_err(|e| napi::Error::from_reason(format!("set {section_name}.{key}: {e}")))?;
        }
    }

    tx.commit()
        .map_err(|e| napi::Error::from_reason(format!("write {}: {e}", config_path.display())))?;
    // Refresh the in-memory snapshot so the new remotes are visible on this instance.
    repo.reload()
        .map_err(|e| napi::Error::from_reason(format!("reload: {e}")))?;
    Ok(names)
}

/// Read `manifest_path` (relative to the repo root) at `revision` in the repository
/// at `path`, then configure its remotes (and config). Returns the configured remote
/// names in manifest order.
///
/// Defaults: `path` = `"."`, `revision` = `"HEAD"`, `manifest_path` = `"repository.manifest.yml"`.
#[napi]
pub fn setup_remotes(
    path: Option<String>,
    revision: Option<String>,
    manifest_path: Option<String>,
) -> Result<Vec<String>> {
    let path = path.unwrap_or_else(|| ".".to_string());
    let revision = revision.unwrap_or_else(|| "HEAD".to_string());
    let manifest_path = manifest_path.unwrap_or_else(|| "repository.manifest.yml".to_string());
    let mut repo = gix::open(&path)
        .map_err(|e| napi::Error::from_reason(format!("open {path}: {e}")))?;
    let manifest = Manifest::read_at(&repo, &revision, &manifest_path)?;
    configure_remotes(&mut repo, &manifest)
}
