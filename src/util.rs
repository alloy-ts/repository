use std::fs;
use std::path::Path;

use napi::Result;
use serde::{Deserialize, Serialize};

use crate::error::err;

/// Data model corresponding to `repository.manifest.yml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub r#type: Option<String>,
    pub owner: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub config: Vec<serde_json::Value>,
    #[serde(default)]
    pub remote: Vec<String>,
}

/// Read and parse a manifest YAML file (e.g., `repository.manifest.yml`).
pub fn parse_manifest(path: impl AsRef<Path>) -> Result<Manifest> {
    let p = path.as_ref();
    let content = fs::read_to_string(p)
        .map_err(|e| napi::Error::from_reason(format!("Failed to read manifest at {:?}: {e}", p)))?;
    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse manifest YAML at {:?}: {e}", p)))?;
    Ok(manifest)
}

/// Sets up repository remotes and git configuration using a parsed manifest.
pub fn setup_repository_from_manifest(
    repo_inner: &gix::ThreadSafeRepository,
    manifest: &Manifest,
) -> Result<()> {
    let repo = repo_inner.to_thread_local();
    let config_path = repo.git_dir().join("config");
    let mut config_tx = repo
        .config_file_mut(&config_path)
        .map_err(|e| err("config_file_mut", e))?;

    // Apply configuration settings listed in manifest
    for cfg in &manifest.config {
        if let Some(obj) = cfg.as_object() {
            for (key, val) in obj {
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => val.to_string(),
                };
                config_tx
                    .set_raw_value(key.as_str(), val_str.as_str())
                    .map_err(|e| err("set_raw_value config", e))?;
            }
        }
    }

    // Apply remotes listed in manifest
    if !manifest.remote.is_empty() {
        // Set origin primary URL and fetch refspec
        let primary_url = &manifest.remote[0];
        config_tx
            .set_raw_value("remote.origin.url", primary_url.as_str())
            .map_err(|e| err("set_raw_value remote.origin.url", e))?;
        config_tx
            .set_raw_value(
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            )
            .map_err(|e| err("set_raw_value remote.origin.fetch", e))?;

        // Add push URLs and create named remotes based on URL hostname
        for url in &manifest.remote {
            config_tx
                .set_raw_value("remote.origin.pushurl", url.as_str())
                .map_err(|e| err("set_raw_value remote.origin.pushurl", e))?;

            if let Ok(parsed) = gix::url::parse(url.as_str()) {
                if let Some(host) = parsed.host() {
                    let name = match host {
                        "github.com" => "github",
                        "gitlab.com" => "gitlab",
                        "codeberg.org" => "codeberg",
                        "bitbucket.org" => "bitbucket",
                        "dev.azure.com" => "azure",
                        "repo.chen.software" => "origin",
                        _ => host,
                    };
                    let key_url = format!("remote.{name}.url");
                    let key_fetch = format!("remote.{name}.fetch");
                    let fetch_val = format!("+refs/heads/*:refs/remotes/{name}/*");
                    config_tx
                        .set_raw_value(key_url.as_str(), url.as_str())
                        .map_err(|e| err("set_raw_value named remote url", e))?;
                    config_tx
                        .set_raw_value(key_fetch.as_str(), fetch_val.as_str())
                        .map_err(|e| err("set_raw_value named remote fetch", e))?;
                }
            }
        }
    }

    config_tx
        .commit()
        .map_err(|e| err("commit config_file_mut", e))?;
    Ok(())
}

/// Reads `repository.manifest.yml` and sets up repository remotes and configuration.
pub fn setup_repository_from_manifest_file(
    repo_inner: &gix::ThreadSafeRepository,
    manifest_path: impl AsRef<Path>,
) -> Result<()> {
    let manifest = parse_manifest(manifest_path)?;
    setup_repository_from_manifest(repo_inner, &manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let manifest_content = r#"
name: repository
version: 0.0.0
type: application
owner: yumin-chen
description: Test repo
config:
  - commit.gpgsign: true
remote:
  - https://repo.chen.software/yumin-chen/repository
  - https://github.com/yumin-chen/repository
"#;
        let manifest: Manifest = serde_yaml::from_str(manifest_content).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("repository"));
        assert_eq!(manifest.remote.len(), 2);
    }
}
