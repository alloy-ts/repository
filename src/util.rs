use std::path::Path;
use std::sync::Arc;

use napi::bindgen_prelude::Error as NapiError;
use napi::Result;

use crate::error::thread_local;

/// A simple structure representing the subset of `repository.manifest.yml` we care about.
#[derive(Debug, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub remote: Vec<String>,
}

/// Parse `repository.manifest.yml` at `manifest_path` (or relative to working tree).
pub fn read_manifest<P: AsRef<Path>>(path: P) -> Result<Manifest> {
    let content = std::fs::read_to_string(path.as_ref()).map_err(|e| {
        NapiError::from_reason(format!(
            "failed to read manifest at {:?}: {e}",
            path.as_ref()
        ))
    })?;

    parse_manifest_yaml(&content)
}

fn parse_manifest_yaml(yaml: &str) -> Result<Manifest> {
    let mut manifest = Manifest::default();
    let mut current_section: Option<String> = None;

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with("- ") {
            let item = trimmed[2..].trim().to_string();
            if current_section.as_deref() == Some("remote") {
                manifest.remote.push(item);
            }
        } else if let Some((key, val)) = trimmed.split_once(':') {
            let key = key.trim();
            let val = val.trim();

            if val.is_empty() {
                current_section = Some(key.to_string());
            } else {
                current_section = None;
                if key == "name" {
                    manifest.name = Some(val.to_string());
                } else if key == "version" {
                    manifest.version = Some(val.to_string());
                }
            }
        }
    }

    Ok(manifest)
}

/// Synchronize remotes in `repo` according to `repository.manifest.yml`.
///
/// Remotes listed in the manifest will be set up in the git configuration (`.git/config`).
/// Remote names default to `origin` for the first remote, or are named according to host/domain if multiple.
pub fn sync_manifest_remotes(
    repo_handle: &Arc<gix::ThreadSafeRepository>,
    manifest_path: &Path,
) -> Result<Vec<String>> {
    let manifest = read_manifest(manifest_path)?;
    if manifest.remote.is_empty() {
        return Ok(Vec::new());
    }

    let repo = thread_local(repo_handle);
    let config_path = repo.git_dir().join("config");
    let mut config = repo
        .config_snapshot()
        .to_owned();

    let mut configured = Vec::new();

    for (idx, url_str) in manifest.remote.iter().enumerate() {
        let name = if idx == 0 {
            "origin".to_string()
        } else if let Ok(url) = gix::url::parse(url_str.as_bytes()) {
            if let Some(host) = url.host() {
                let host_str = host.to_string();
                let parts: Vec<&str> = host_str.split('.').collect();
                if parts.len() >= 2 {
                    parts[parts.len() - 2].to_string()
                } else {
                    format!("remote-{idx}")
                }
            } else {
                format!("remote-{idx}")
            }
        } else {
            format!("remote-{idx}")
        };

        // Check if remote already exists with same URL
        if let Ok(existing) = repo.find_remote(&name) {
            if existing.url(gix::remote::Direction::Fetch).map(|u| u.to_string()) == Some(url_str.clone()) {
                configured.push(name);
                continue;
            }
        }

        // Set remote URL in config
        let section_header = format!("remote \"{name}\"");
        let section_name = "remote";
        let subsection_name = name.as_str();

        let mut section = config
            .section_mut_or_create_new(section_name, Some(subsection_name.into()))
            .map_err(|e| NapiError::from_reason(format!("failed to edit config section '{section_header}': {e}")))?;

        let _ = section.push("url", url_str.as_str());
        configured.push(name);
    }

    // Save config back to disk
    if let Ok(mut file) = std::fs::File::create(&config_path) {
        use std::io::Write;
        let _ = file.write_all(config.to_bstring().as_ref());
    }

    Ok(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest_yaml() {
        let yaml = r#"
name: repository
version: 0.0.0
remote:
  - https://repo.chen.software/yumin-chen/repository
  - https://codeberg.org/yumin-chen/repository
"#;
        let manifest = parse_manifest_yaml(yaml).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("repository"));
        assert_eq!(manifest.version.as_deref(), Some("0.0.0"));
        assert_eq!(manifest.remote.len(), 2);
        assert_eq!(manifest.remote[0], "https://repo.chen.software/yumin-chen/repository");
    }
}
