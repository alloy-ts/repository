use std::fs;
use std::path::Path;

use napi::Result;
use napi_derive::napi;

use crate::error::err;
use crate::repository::Repository;

/// Helper function to parse `repository.manifest.yml` remotes.
fn parse_manifest_remotes(manifest_content: &str) -> Vec<(String, String)> {
    let mut remotes = Vec::new();
    let mut in_remote = false;

    for line in manifest_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("remote:") {
            in_remote = true;
            continue;
        }
        if in_remote {
            if trimmed.starts_with('-') {
                let url = trimmed.trim_start_matches('-').trim();
                if !url.is_empty() {
                    let name = if url.contains("github.com") {
                        "github"
                    } else if url.contains("codeberg.org") {
                        "codeberg"
                    } else if url.contains("gitlab.com") {
                        "gitlab"
                    } else if url.contains("bitbucket.org") {
                        "bitbucket"
                    } else if url.contains("azure.com") {
                        "azure"
                    } else if url.contains("repo.chen.software") {
                        "origin"
                    } else {
                        "origin"
                    };
                    remotes.push((name.to_string(), url.to_string()));
                }
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_remote = false;
            }
        }
    }

    remotes
}

/// Configure remotes on a repository based on `repository.manifest.yml`.
#[napi]
pub fn setup_remotes_from_manifest(repo: &Repository, manifest_path: Option<String>) -> Result<Vec<String>> {
    let path_str = manifest_path.unwrap_or_else(|| "repository.manifest.yml".to_string());
    let path = Path::new(&path_str);

    if !path.exists() {
        return Err(err("setup_remotes_from_manifest", format!("manifest file not found at '{path_str}'")));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| err("read manifest", e))?;

    let remotes = parse_manifest_remotes(&content);
    let mut configured = Vec::new();

    let r = repo.inner.to_thread_local();
    for (name, url) in remotes {
        let key = format!("remote.{name}.url");
        if let Ok(path) = r.config_path(gix::config::Source::Local) {
            if let Ok(mut file) = r.config_file_mut(path) {
                let _ = file.set_raw_value(key.as_str(), url.as_str());
                let _ = file.commit();
            }
        }
        configured.push(format!("{name} = {url}"));
    }

    Ok(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest_remotes() {
        let manifest = r#"
name: repository
remote:
  - https://repo.chen.software/yumin-chen/repository
  - https://codeberg.org/yumin-chen/repository
  - https://github.com/yumin-chen/repository
"#;
        let remotes = parse_manifest_remotes(manifest);
        assert_eq!(remotes.len(), 3);
        assert_eq!(remotes[0].0, "origin");
        assert_eq!(remotes[1].0, "codeberg");
        assert_eq!(remotes[2].0, "github");
    }
}
