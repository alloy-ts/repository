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

/// Read and parse a manifest YAML file from disk (e.g., `repository.manifest.yml`).
pub fn parse_manifest(path: impl AsRef<Path>) -> Result<Manifest> {
    let p = path.as_ref();
    let content = fs::read_to_string(p)
        .map_err(|e| napi::Error::from_reason(format!("Failed to read manifest at {:?}: {e}", p)))?;
    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse manifest YAML at {:?}: {e}", p)))?;
    Ok(manifest)
}

/// Reads and parses `repository.manifest.yml` from a repository revision spec (e.g. `"HEAD:repository.manifest.yml"`, `"main:repository.manifest.yml"`, or `"<sha>"`), falling back to filesystem path if it exists.
pub fn read_manifest_from_revision(
    repo_inner: &gix::ThreadSafeRepository,
    rev_spec_or_path: &str,
) -> Result<Manifest> {
    let repo = repo_inner.to_thread_local();

    // 1. Try revision spec formatted as "<rev>:repository.manifest.yml" or verbatim "<rev>:<path>"
    let spec = if rev_spec_or_path.contains(':') {
        rev_spec_or_path.to_string()
    } else {
        format!("{rev_spec_or_path}:repository.manifest.yml")
    };

    if let Ok(object_id) = repo.rev_parse_single(spec.as_str()) {
        if let Ok(blob) = repo.find_blob(object_id) {
            if let Ok(manifest) = serde_yaml::from_slice::<Manifest>(&blob.data) {
                return Ok(manifest);
            }
        }
    }

    // 2. Try verbatim revision spec
    if let Ok(object_id) = repo.rev_parse_single(rev_spec_or_path) {
        if let Ok(blob) = repo.find_blob(object_id) {
            if let Ok(manifest) = serde_yaml::from_slice::<Manifest>(&blob.data) {
                return Ok(manifest);
            }
        }
    }

    // 3. Fall back to local filesystem path
    if Path::new(rev_spec_or_path).exists() {
        return parse_manifest(rev_spec_or_path);
    }

    // 4. Fall back to default filename in process working directory
    if Path::new("repository.manifest.yml").exists() {
        return parse_manifest("repository.manifest.yml");
    }

    Err(napi::Error::from_reason(format!(
        "Could not resolve manifest at revision spec or file path '{rev_spec_or_path}'"
    )))
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

/// Reads `repository.manifest.yml` from a revision spec or file path and sets up repository remotes and configuration.
pub fn setup_repository_from_manifest_spec(
    repo_inner: &gix::ThreadSafeRepository,
    rev_spec_or_path: Option<&str>,
) -> Result<()> {
    let spec = rev_spec_or_path.unwrap_or("HEAD:repository.manifest.yml");
    let manifest = read_manifest_from_revision(repo_inner, spec)?;
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
