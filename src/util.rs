use std::fs;
use std::path::Path;

/// Manifest file structure parsed from `repository.manifest.yml`.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub remote_urls: Vec<String>,
}

/// Parse `repository.manifest.yml` content into a `Manifest` struct.
pub fn parse_manifest(content: &str) -> Manifest {
    let mut manifest = Manifest::default();
    let mut in_remote_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "remote:" {
            in_remote_section = true;
            continue;
        } else if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
            in_remote_section = false;
        }

        if in_remote_section && trimmed.starts_with("- ") {
            let url = trimmed[2..].trim().trim_matches('"').trim_matches('\'');
            if !url.is_empty() {
                manifest.remote_urls.push(url.to_string());
            }
        } else if !in_remote_section {
            if let Some((key, val)) = trimmed.split_once(':') {
                let k = key.trim();
                let v = val.trim().trim_matches('"').trim_matches('\'');
                match k {
                    "name" => manifest.name = v.to_string(),
                    "version" => manifest.version = v.to_string(),
                    "description" => manifest.description = v.to_string(),
                    _ => {}
                }
            }
        }
    }

    manifest
}

/// Infer a sensible remote name based on the URL or its position index.
pub fn remote_name_for_url(url: &str, index: usize) -> String {
    if index == 0 {
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
    } else if url.contains("dev.azure.com") || url.contains("visualstudio.com") {
        "azure".to_string()
    } else {
        format!("remote-{index}")
    }
}

/// Read `repository.manifest.yml` from `manifest_path` and configure remotes on a `gix::Repository`.
pub fn setup_remotes_from_manifest(
    repo: &gix::Repository,
    manifest_path: &Path,
) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(manifest_path)
        .map_err(|e| format!("failed to read manifest at {}: {}", manifest_path.display(), e))?;
    let manifest = parse_manifest(&content);

    let mut configured = Vec::new();
    let mut config_file = repo
        .config_snapshot()
        .to_owned();

    for (i, url) in manifest.remote_urls.iter().enumerate() {
        let name = remote_name_for_url(url, i);
        config_file
            .set_raw_value_by("remote", Some(name.as_str().into()), "url", url.as_str())
            .map_err(|e| format!("failed to set remote {name}: {e}"))?;
        configured.push((name, url.clone()));
    }

    Ok(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let yaml = r#"
name: repository
version: 0.0.0
type: application
owner: yumin-chen
description: Versioned application repository for yumin-chen projects.
config:
  - commit.gpgsign: true
remote:
  - https://repo.chen.software/yumin-chen/repository
  - https://codeberg.org/yumin-chen/repository
  - https://github.com/yumin-chen/repository
"#;
        let manifest = parse_manifest(yaml);
        assert_eq!(manifest.name, "repository");
        assert_eq!(manifest.version, "0.0.0");
        assert_eq!(manifest.remote_urls.len(), 3);
        assert_eq!(manifest.remote_urls[0], "https://repo.chen.software/yumin-chen/repository");
    }

    #[test]
    fn test_remote_names() {
        assert_eq!(remote_name_for_url("https://repo.chen.software/yumin-chen/repository", 0), "origin");
        assert_eq!(remote_name_for_url("https://codeberg.org/yumin-chen/repository", 1), "codeberg");
        assert_eq!(remote_name_for_url("https://github.com/yumin-chen/repository", 2), "github");
        assert_eq!(remote_name_for_url("https://gitlab.com/yumin-chen/repository", 3), "gitlab");
        assert_eq!(remote_name_for_url("https://bitbucket.org/yumin-chen/repository", 4), "bitbucket");
        assert_eq!(remote_name_for_url("https://dev.azure.com/yumin-chen/repository", 5), "azure");
    }
}
