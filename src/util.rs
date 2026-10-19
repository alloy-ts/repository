use std::fs;
use std::path::Path;

use gix::config::AsBStr;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// Information about a configured remote.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// Parsed structure of `repository.manifest.yml`.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub r#type: String,
    pub owner: String,
    pub description: String,
    pub config: Vec<(String, String)>,
    pub remotes: Vec<String>,
}

/// Parse a `repository.manifest.yml` file content.
pub fn parse_manifest(content: &str) -> Manifest {
    let mut manifest = Manifest::default();
    let mut current_section = "";

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "config:" {
            current_section = "config";
            continue;
        } else if trimmed == "remote:" || trimmed == "remotes:" {
            current_section = "remote";
            continue;
        }

        if current_section == "config" && trimmed.starts_with('-') {
            let item = trimmed.trim_start_matches('-').trim();
            if let Some((k, v)) = item.split_once(':') {
                manifest.config.push((k.trim().to_string(), v.trim().to_string()));
            }
        } else if current_section == "remote" && trimmed.starts_with('-') {
            let url = trimmed.trim_start_matches('-').trim();
            if !url.is_empty() {
                manifest.remotes.push(url.to_string());
            }
        } else if let Some((k, v)) = trimmed.split_once(':') {
            current_section = "";
            let key = k.trim();
            let val = v.trim();
            match key {
                "name" => manifest.name = val.to_string(),
                "version" => manifest.version = val.to_string(),
                "type" => manifest.r#type = val.to_string(),
                "owner" => manifest.owner = val.to_string(),
                "description" => manifest.description = val.to_string(),
                _ => {}
            }
        }
    }

    manifest
}

/// Derive a remote name from its URL (or use 'origin' for the primary remote).
pub fn remote_name_from_url(url: &str, is_first: bool) -> String {
    if is_first {
        return "origin".to_string();
    }
    if url.contains("codeberg.org") {
        "codeberg".to_string()
    } else if url.contains("github.com") {
        "github".to_string()
    } else if url.contains("gitlab.com") {
        "gitlab".to_string()
    } else if url.contains("bitbucket.org") {
        "bitbucket".to_string()
    } else if url.contains("dev.azure.com") {
        "azure".to_string()
    } else {
        // Fallback: extract domain name or return remote
        let cleaned = url.trim_start_matches("https://").trim_start_matches("http://");
        if let Some((domain, _)) = cleaned.split_once('/') {
            let parts: Vec<&str> = domain.split('.').collect();
            if parts.len() >= 2 {
                parts[parts.len() - 2].to_string()
            } else {
                domain.replace('.', "-")
            }
        } else {
            "remote".to_string()
        }
    }
}

/// Setup remotes in `repo` according to the manifest file at `manifest_path`.
pub fn setup_repository_remotes(
    repo: &gix::Repository,
    manifest_path: &Path,
) -> Result<Vec<(String, String)>> {
    let content = fs::read_to_string(manifest_path).map_err(|e| {
        napi::Error::from_reason(format!(
            "failed to read manifest at {}: {e}",
            manifest_path.display()
        ))
    })?;

    let manifest = parse_manifest(&content);
    let mut configured = Vec::new();

    let config_path = repo
        .config_path(gix::config::Source::Local)
        .map_err(|e| err("config_path", e))?;

    let mut file = repo
        .config_file_mut(config_path)
        .map_err(|e| err("config_file_mut", e))?;

    for (idx, url) in manifest.remotes.iter().enumerate() {
        let name = remote_name_from_url(url, idx == 0);
        let section = "remote";
        let sub_section = Some(gix::bstr::BString::from(name.as_str()));

        let sub_bstr = sub_section.as_ref().map(|s| s.as_bstr());
        file.set_raw_value_by(section, sub_bstr, "url", url.as_str())
            .map_err(|e| napi::Error::from_reason(format!("failed to set remote url: {e}")))?;

        let fetch_refspec = format!("+refs/heads/*:refs/remotes/{name}/*");
        file.set_raw_value_by(section, sub_bstr, "fetch", fetch_refspec.as_str())
            .map_err(|e| napi::Error::from_reason(format!("failed to set remote fetch refspec: {e}")))?;

        configured.push((name, url.clone()));
    }

    for (key, val) in &manifest.config {
        if let Some((section, value_key)) = key.split_once('.') {
            file.set_raw_value_by(section, None, value_key, val.as_str())
                .map_err(|e| napi::Error::from_reason(format!("failed to set config {key}: {e}")))?;
        }
    }

    file.commit()
        .map_err(|e| err("config_commit", e))?;

    Ok(configured)
}

/// Setup remotes from `repository.manifest.yml` for a given `Repository`.
#[napi]
pub fn setup_remotes_from_manifest(
    repo: &Repository,
    manifest_path: Option<String>,
) -> Result<Vec<RemoteInfo>> {
    let mut r = thread_local(&repo.inner);
    let path = match manifest_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let work_dir = r.workdir().ok_or_else(|| {
                napi::Error::from_reason("bare repository has no default workdir for manifest")
            })?;
            work_dir.join("repository.manifest.yml")
        }
    };
    let remotes = setup_repository_remotes(&r, &path)?;
    r.reload().map_err(|e| err("reload", e))?;
    Ok(remotes
        .into_iter()
        .map(|(name, url)| RemoteInfo { name, url })
        .collect())
}
