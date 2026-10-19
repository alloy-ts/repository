use std::path::Path;
use napi::Result;
use napi_derive::napi;

use crate::error::err;
use crate::repository::Repository;

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub owner: Option<String>,
    pub description: Option<String>,
    pub config: Vec<String>,
    pub remote: Vec<String>,
}

/// Parse `repository.manifest.yml` content into a [`Manifest`].
pub fn parse_manifest(content: &str) -> Result<Manifest> {
    let mut manifest = Manifest::default();
    let mut in_config = false;
    let mut in_remote = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with("config:") {
            in_config = true;
            in_remote = false;
            continue;
        }
        if line.starts_with("remote:") {
            in_remote = true;
            in_config = false;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_config = false;
            in_remote = false;
        }

        if in_config && trimmed.starts_with("- ") {
            let item = trimmed.trim_start_matches("- ").trim();
            manifest.config.push(item.to_string());
        } else if in_remote && trimmed.starts_with("- ") {
            let item = trimmed.trim_start_matches("- ").trim();
            manifest.remote.push(item.to_string());
        } else if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            match k {
                "name" => manifest.name = Some(v.to_string()),
                "version" => manifest.version = Some(v.to_string()),
                "owner" => manifest.owner = Some(v.to_string()),
                "description" => manifest.description = Some(v.to_string()),
                _ => {}
            }
        }
    }

    Ok(manifest)
}

/// Reads `repository.manifest.yml` from `repo`'s workdir or gitdir and sets up remotes
/// and repo configuration using the repository handle.
#[napi]
pub fn setup_repository_from_manifest(
    repo: &Repository,
    manifest_path: Option<String>,
) -> Result<Vec<String>> {
    let path = match manifest_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let base = repo
                .work_dir()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(repo.git_dir()));
            base.join("repository.manifest.yml")
        }
    };

    if !path.exists() {
        return Err(napi::Error::from_reason(format!(
            "manifest file not found at {}",
            path.display()
        )));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| err(&format!("read manifest {}", path.display()), e))?;
    let manifest = parse_manifest(&content)?;

    let mut configured = Vec::new();

    // Edit local config file `.git/config`
    let config_file_path = Path::new(&repo.git_dir()).join("config");
    let mut config_content = if config_file_path.exists() {
        std::fs::read_to_string(&config_file_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Apply config entries from manifest
    for cfg in &manifest.config {
        if let Some((k, v)) = cfg.split_once(':') {
            let key = k.trim();
            let val = v.trim();
            if key == "commit.gpgsign" {
                if !config_content.contains("[commit]") {
                    config_content.push_str("\n[commit]\n");
                }
                if !config_content.contains("gpgsign") {
                    config_content.push_str(&format!("\tgpgsign = {val}\n"));
                }
                configured.push(format!("config {key}={val}"));
            }
        }
    }

    // Apply remotes
    if !manifest.remote.is_empty() {
        if !config_content.contains("[remote \"origin\"]") {
            config_content.push_str("\n[remote \"origin\"]\n");
            if let Some(first) = manifest.remote.first() {
                config_content.push_str(&format!("\turl = {first}\n"));
                config_content.push_str("\tfetch = +refs/heads/*:refs/remotes/origin/*\n");
            }
            for url in manifest.remote.iter().skip(1) {
                config_content.push_str(&format!("\tpushurl = {url}\n"));
            }
        }
        for (i, url) in manifest.remote.iter().enumerate() {
            configured.push(format!("remote url[{i}]={url}"));
        }
    }

    std::fs::write(&config_file_path, config_content)
        .map_err(|e| err("write git config", e))?;

    Ok(configured)
}
