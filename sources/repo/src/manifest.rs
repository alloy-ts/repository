use std::fs;
use std::path::{Path, PathBuf};

use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::{err, err_str, thread_local};
use crate::remote::Remote;
use crate::repository::Repository;

/// A single config key/value pair declared in a repository manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ManifestConfigEntry {
    pub key: String,
    pub value: String,
}

/// A remote definition declared in a repository manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ManifestRemote {
    pub name: String,
    pub url: String,
    pub fetch: Option<String>,
}

/// An SSH or PGP signer entry declared in a repository manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ManifestSigner {
    pub principal: String,
    pub key: String,
}

/// Outcome of applying manifest configuration to a repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ApplyConfigResult {
    pub applied: u32,
    pub unchanged: u32,
}

/// Outcome of applying manifest remotes to a repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ApplyRemotesResult {
    pub created: u32,
    pub unchanged: u32,
    pub updated: u32,
}

/// Outcome of provisioning an allowed signers file (`.gitsigners`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[napi(object, namespace = "Repository")]
pub struct ProvisionSignersResult {
    pub count: u32,
    pub path: String,
    pub changed: bool,
}

/// Derive a default remote shorthand name from its URL.
/// Follows the second-level domain or hostname convention (e.g. `github.com` -> `github`,
/// `repo.chen.software` -> `chen`).
pub fn name_from_url(url: &str) -> String {
    let host = if let Ok(parsed) = url::Url::parse(url) {
        parsed.host_str().map(|s| s.to_string())
    } else {
        // Fallback for scp-like `git@host:path` or `git://host/...` URLs
        let trimmed = url
            .split_once('@')
            .map(|(_, r)| r)
            .unwrap_or(url);
        let host_part = trimmed
            .split_once(':')
            .map(|(l, _)| l)
            .unwrap_or_else(|| {
                trimmed.split_once('/').map(|(l, _)| l).unwrap_or(trimmed)
            });
        if !host_part.is_empty() {
            Some(host_part.to_string())
        } else {
            None
        }
    };

    let host = match host {
        Some(h) if !h.is_empty() => h,
        _ => return "origin".to_string(),
    };

    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2 {
        labels[labels.len() - 2].to_string()
    } else {
        labels.first().copied().unwrap_or("origin").to_string()
    }
}

/// Representation of a parsed `repository.manifest.yml`.
#[derive(Clone, Debug)]
#[napi(namespace = "Repository")]
pub struct Manifest {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) r#type: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) config: Vec<ManifestConfigEntry>,
    pub(crate) remotes: Vec<ManifestRemote>,
    pub(crate) signers: Vec<ManifestSigner>,
    pub(crate) raw: String,
}

#[derive(Deserialize, Serialize)]
struct RawManifestDoc {
    name: Option<String>,
    version: Option<String>,
    #[serde(rename = "type")]
    r#type: Option<String>,
    owner: Option<String>,
    description: Option<String>,
    config: Option<serde_yaml::Value>,
    remote: Option<serde_yaml::Value>,
    remotes: Option<serde_yaml::Value>,
    signers: Option<serde_yaml::Value>,
}

fn parse_config_values(val: &serde_yaml::Value) -> Vec<ManifestConfigEntry> {
    let mut out = Vec::new();
    match val {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                if let serde_yaml::Value::Mapping(map) = item {
                    for (k, v) in map {
                        let key_str = match k {
                            serde_yaml::Value::String(s) => s.clone(),
                            other => format!("{:?}", other),
                        };
                        let val_str = match v {
                            serde_yaml::Value::String(s) => s.clone(),
                            serde_yaml::Value::Bool(b) => b.to_string(),
                            serde_yaml::Value::Number(n) => n.to_string(),
                            other => format!("{:?}", other),
                        };
                        out.push(ManifestConfigEntry {
                            key: key_str,
                            value: val_str,
                        });
                    }
                }
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                let key_str = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => format!("{:?}", other),
                };
                let val_str = match v {
                    serde_yaml::Value::String(s) => s.clone(),
                    serde_yaml::Value::Bool(b) => b.to_string(),
                    serde_yaml::Value::Number(n) => n.to_string(),
                    other => format!("{:?}", other),
                };
                out.push(ManifestConfigEntry {
                    key: key_str,
                    value: val_str,
                });
            }
        }
        _ => {}
    }
    out
}

fn add_remote_value(out: &mut Vec<ManifestRemote>, val: &serde_yaml::Value) {
    match val {
        serde_yaml::Value::String(url) => {
            out.push(ManifestRemote {
                name: name_from_url(url),
                url: url.clone(),
                fetch: None,
            });
        }
        serde_yaml::Value::Mapping(map) => {
            let url = map.get(serde_yaml::Value::String("url".to_string())).and_then(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                _ => None,
            });
            if let Some(url) = url {
                let name = map
                    .get(serde_yaml::Value::String("name".to_string()))
                    .and_then(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| name_from_url(&url));

                let fetch = map
                    .get(serde_yaml::Value::String("fetch".to_string()))
                    .and_then(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        _ => None,
                    });

                out.push(ManifestRemote { name, url, fetch });
            }
        }
        _ => {}
    }
}

fn parse_remote_values(val: &serde_yaml::Value) -> Vec<ManifestRemote> {
    let mut out = Vec::new();
    match val {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                add_remote_value(&mut out, item);
            }
        }
        other => {
            add_remote_value(&mut out, other);
        }
    }
    out
}

fn add_signer_value(out: &mut Vec<ManifestSigner>, val: &serde_yaml::Value) {
    match val {
        serde_yaml::Value::String(key) => {
            out.push(ManifestSigner {
                principal: String::new(),
                key: key.clone(),
            });
        }
        serde_yaml::Value::Mapping(map) => {
            let key = map.get(serde_yaml::Value::String("key".to_string())).and_then(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                _ => None,
            });
            if let Some(key) = key {
                let principal = map
                    .get(serde_yaml::Value::String("principal".to_string()))
                    .or_else(|| map.get(serde_yaml::Value::String("email".to_string())))
                    .and_then(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                out.push(ManifestSigner { principal, key });
            }
        }
        _ => {}
    }
}

fn parse_signer_values(val: &serde_yaml::Value) -> Vec<ManifestSigner> {
    let mut out = Vec::new();
    match val {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                add_signer_value(&mut out, item);
            }
        }
        other => {
            add_signer_value(&mut out, other);
        }
    }
    out
}

#[napi(namespace = "Repository")]
impl Manifest {
    /// Parse a repository manifest from YAML (or JSON) string.
    #[napi(factory)]
    pub fn parse(text: String) -> napi::Result<Manifest> {
        let doc: RawManifestDoc = serde_yaml::from_str(&text)
            .map_err(|e| err_str("Manifest::parse", format!("invalid YAML: {e}")))?;

        let mut config = Vec::new();
        if let Some(c) = &doc.config {
            config.extend(parse_config_values(c));
        }

        let mut remotes = Vec::new();
        if let Some(r) = &doc.remote {
            remotes.extend(parse_remote_values(r));
        }
        if let Some(r) = &doc.remotes {
            remotes.extend(parse_remote_values(r));
        }

        let mut signers = Vec::new();
        if let Some(s) = &doc.signers {
            signers.extend(parse_signer_values(s));
        }

        Ok(Manifest {
            name: doc.name,
            version: doc.version,
            r#type: doc.r#type,
            owner: doc.owner,
            description: doc.description,
            config,
            remotes,
            signers,
            raw: text,
        })
    }

    /// Read `repository.manifest.yml` from a repository.
    ///
    /// Searches first in the repository's working tree (if non-bare), and falls
    /// back to the metadata branches (`refs/heads/meta/repository`, `refs/heads/meta/repo`,
    /// `refs/heads/meta`), checking both `repository/repository.manifest.yml` and the root tree.
    #[napi]
    pub fn read_from_repo(repo: &Repository) -> napi::Result<Option<Manifest>> {
        let file_candidates = [
            "repository.manifest.yml",
            "repository.manifest.yaml",
            ".repository.manifest.yml",
            ".repository.manifest.yaml",
        ];

        // 1. Try reading directly from working tree if present
        if let Some(work_dir) = repo.work_dir() {
            let base = Path::new(&work_dir);
            for name in &file_candidates {
                let p = base.join(name);
                if p.is_file() {
                    if let Ok(text) = fs::read_to_string(&p) {
                        return Ok(Some(Self::parse(text)?));
                    }
                }
                let repo_p = base.join("repository").join(name);
                if repo_p.is_file() {
                    if let Ok(text) = fs::read_to_string(&repo_p) {
                        return Ok(Some(Self::parse(text)?));
                    }
                }
            }
        }

        // 2. Try resolving from meta branches:
        let branch_candidates = [
            "refs/heads/meta/repository",
            "refs/heads/meta/repo",
            "refs/heads/meta",
        ];

        let r = thread_local(&repo.inner);
        for branch_name in &branch_candidates {
            let mut reference = match r.find_reference(*branch_name) {
                Ok(ref_) => ref_,
                Err(_) => continue,
            };
            let commit_id = match reference.peel_to_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            };

            let commit = match r.find_commit(commit_id) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let tree_id = match commit.tree_id() {
                Ok(tid) => tid,
                Err(_) => continue,
            };
            let root_tree = match r.find_tree(tree_id) {
                Ok(t) => t,
                Err(_) => continue,
            };

            let repo_sub_id = root_tree
                .iter()
                .filter_map(|e| e.ok())
                .find(|e| e.filename() == "repository" && e.mode().is_tree())
                .map(|e| e.id().detach());

            let target_tree = if let Some(sub_id) = repo_sub_id {
                r.find_tree(sub_id).unwrap_or(root_tree)
            } else {
                root_tree
            };

            for name in &file_candidates {
                let blob_id = target_tree
                    .iter()
                    .filter_map(|e| e.ok())
                    .find(|e| e.filename() == *name && e.mode().is_blob())
                    .map(|e| e.id().detach());

                if let Some(id) = blob_id {
                    if let Ok(obj) = r.find_object(id) {
                        let text = String::from_utf8_lossy(&obj.data).into_owned();
                        return Ok(Some(Self::parse(text)?));
                    }
                }
            }
        }

        Ok(None)
    }

    /// The name of the project/repository.
    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The version string declared in the manifest.
    #[napi(getter)]
    pub fn version(&self) -> Option<String> {
        self.version.clone()
    }

    /// The type of the repository (`module`, `metadata`, etc.).
    #[napi(getter, js_name = "type")]
    pub fn get_type(&self) -> Option<String> {
        self.r#type.clone()
    }

    /// The owner or organization identifier.
    #[napi(getter)]
    pub fn owner(&self) -> Option<String> {
        self.owner.clone()
    }

    /// The repository description.
    #[napi(getter)]
    pub fn description(&self) -> Option<String> {
        self.description.clone()
    }

    /// The configuration entries parsed from `config:`.
    #[napi(getter)]
    pub fn config(&self) -> Vec<ManifestConfigEntry> {
        self.config.clone()
    }

    /// The remotes parsed from `remote:` / `remotes:`.
    #[napi(getter)]
    pub fn remotes(&self) -> Vec<ManifestRemote> {
        self.remotes.clone()
    }

    /// The signers parsed from `signers:`.
    #[napi(getter)]
    pub fn signers(&self) -> Vec<ManifestSigner> {
        self.signers.clone()
    }

    /// The raw manifest YAML string.
    #[napi(getter)]
    pub fn raw(&self) -> String {
        self.raw.clone()
    }

    /// Apply all `config:` entries from the manifest to the repository's configuration.
    #[napi]
    pub fn apply_config(&self, repo: &Repository) -> napi::Result<ApplyConfigResult> {
        let mut applied = 0;
        let mut unchanged = 0;

        for entry in &self.config {
            let existing = repo.config_string(entry.key.clone());
            if existing.as_deref() == Some(&entry.value) {
                unchanged += 1;
            } else {
                repo.config_set(entry.key.clone(), entry.value.clone())?;
                applied += 1;
            }
        }

        Ok(ApplyConfigResult { applied, unchanged })
    }

    /// Configure remotes declared in the manifest in the repository.
    #[napi]
    pub fn apply_remotes(&self, repo: &Repository) -> napi::Result<ApplyRemotesResult> {
        let mut created = 0;
        let mut unchanged = 0;
        let mut updated = 0;

        let existing_remotes: Vec<Remote> = repo.remotes()?;

        for r in &self.remotes {
            if let Some(existing) = existing_remotes.iter().find(|item: &&Remote| item.name().as_deref() == Some(r.name.as_str())) {
                if existing.url() == Some(r.url.clone()) {
                    unchanged += 1;
                } else {
                    repo.config_set(format!("remote.{}.url", r.name), r.url.clone())?;
                    updated += 1;
                }
            } else {
                repo.create_remote(r.name.clone(), r.url.clone())?;
                created += 1;
            }

            if let Some(fetch) = &r.fetch {
                repo.config_set(format!("remote.{}.fetch", r.name), fetch.clone())?;
            }
        }

        Ok(ApplyRemotesResult {
            created,
            unchanged,
            updated,
        })
    }

    /// Provision `.gitsigners` (or custom allowed signers file) from the manifest's `signers:` section.
    #[napi]
    pub fn provision_signers(&self, repo: &Repository) -> napi::Result<Option<ProvisionSignersResult>> {
        if self.signers.is_empty() {
            return Ok(None);
        }

        let base_str = repo.work_dir().unwrap_or_else(|| repo.git_dir());
        let base = PathBuf::from(base_str);

        // Check if repo has configured allowedSignersFile
        let file_name = repo
            .config_string("gpg.ssh.allowedSignersFile".to_string())
            .unwrap_or_else(|| ".gitsigners".to_string());

        let file_path = if Path::new(&file_name).is_absolute() {
            PathBuf::from(file_name)
        } else {
            base.join(file_name)
        };

        let mut lines = Vec::new();
        for s in &self.signers {
            if s.principal.is_empty() {
                lines.push(s.key.clone());
            } else {
                lines.push(format!("{} namespaces=\"git\" {}", s.principal, s.key));
            }
        }
        let content = lines.join("\n") + "\n";

        let changed = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(existing) => existing != content,
                Err(_) => true,
            }
        } else {
            true
        };

        if changed {
            if let Some(parent) = file_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&file_path, &content).map_err(|e| err("provision_signers", e))?;
        }

        Ok(Some(ProvisionSignersResult {
            count: self.signers.len() as u32,
            path: file_path.to_string_lossy().into_owned(),
            changed,
        }))
    }

    /// Serialise the manifest back into YAML.
    #[napi]
    pub fn to_yaml(&self) -> napi::Result<String> {
        let mut map = serde_yaml::Mapping::new();
        if let Some(v) = &self.name {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.version {
            map.insert(
                serde_yaml::Value::String("version".to_string()),
                serde_yaml::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.r#type {
            map.insert(
                serde_yaml::Value::String("type".to_string()),
                serde_yaml::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.owner {
            map.insert(
                serde_yaml::Value::String("owner".to_string()),
                serde_yaml::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.description {
            map.insert(
                serde_yaml::Value::String("description".to_string()),
                serde_yaml::Value::String(v.clone()),
            );
        }

        if !self.config.is_empty() {
            let mut cfg_seq = Vec::new();
            for c in &self.config {
                let mut item = serde_yaml::Mapping::new();
                item.insert(
                    serde_yaml::Value::String(c.key.clone()),
                    serde_yaml::Value::String(c.value.clone()),
                );
                cfg_seq.push(serde_yaml::Value::Mapping(item));
            }
            map.insert(
                serde_yaml::Value::String("config".to_string()),
                serde_yaml::Value::Sequence(cfg_seq),
            );
        }

        if !self.remotes.is_empty() {
            let mut rem_seq = Vec::new();
            for r in &self.remotes {
                let mut item = serde_yaml::Mapping::new();
                item.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(r.name.clone()),
                );
                item.insert(
                    serde_yaml::Value::String("url".to_string()),
                    serde_yaml::Value::String(r.url.clone()),
                );
                if let Some(fetch) = &r.fetch {
                    item.insert(
                        serde_yaml::Value::String("fetch".to_string()),
                        serde_yaml::Value::String(fetch.clone()),
                    );
                }
                rem_seq.push(serde_yaml::Value::Mapping(item));
            }
            map.insert(
                serde_yaml::Value::String("remote".to_string()),
                serde_yaml::Value::Sequence(rem_seq),
            );
        }

        if !self.signers.is_empty() {
            let mut sig_seq = Vec::new();
            for s in &self.signers {
                let mut item = serde_yaml::Mapping::new();
                if !s.principal.is_empty() {
                    item.insert(
                        serde_yaml::Value::String("principal".to_string()),
                        serde_yaml::Value::String(s.principal.clone()),
                    );
                }
                item.insert(
                    serde_yaml::Value::String("key".to_string()),
                    serde_yaml::Value::String(s.key.clone()),
                );
                sig_seq.push(serde_yaml::Value::Mapping(item));
            }
            map.insert(
                serde_yaml::Value::String("signers".to_string()),
                serde_yaml::Value::Sequence(sig_seq),
            );
        }

        serde_yaml::to_string(&serde_yaml::Value::Mapping(map))
            .map_err(|e| err_str("Manifest::to_yaml", format!("serialisation error: {e}")))
    }

    /// Serialise the manifest into JSON.
    #[napi]
    pub fn to_json(&self) -> napi::Result<String> {
        let mut map = serde_json::Map::new();
        if let Some(v) = &self.name {
            map.insert("name".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.version {
            map.insert("version".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.r#type {
            map.insert("type".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.owner {
            map.insert("owner".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &self.description {
            map.insert("description".to_string(), serde_json::Value::String(v.clone()));
        }

        let cfg_val: Vec<serde_json::Value> = self
            .config
            .iter()
            .map(|c| {
                let mut m = serde_json::Map::new();
                m.insert(c.key.clone(), serde_json::Value::String(c.value.clone()));
                serde_json::Value::Object(m)
            })
            .collect();
        map.insert("config".to_string(), serde_json::Value::Array(cfg_val));

        let rem_val: Vec<serde_json::Value> = self
            .remotes
            .iter()
            .map(|r| {
                let mut m = serde_json::Map::new();
                m.insert("name".to_string(), serde_json::Value::String(r.name.clone()));
                m.insert("url".to_string(), serde_json::Value::String(r.url.clone()));
                if let Some(fetch) = &r.fetch {
                    m.insert("fetch".to_string(), serde_json::Value::String(fetch.clone()));
                }
                serde_json::Value::Object(m)
            })
            .collect();
        map.insert("remotes".to_string(), serde_json::Value::Array(rem_val));

        let sig_val: Vec<serde_json::Value> = self
            .signers
            .iter()
            .map(|s| {
                let mut m = serde_json::Map::new();
                m.insert("principal".to_string(), serde_json::Value::String(s.principal.clone()));
                m.insert("key".to_string(), serde_json::Value::String(s.key.clone()));
                serde_json::Value::Object(m)
            })
            .collect();
        map.insert("signers".to_string(), serde_json::Value::Array(sig_val));

        serde_json::to_string_pretty(&serde_json::Value::Object(map))
            .map_err(|e| err_str("Manifest::to_json", format!("serialisation error: {e}")))
    }
}
