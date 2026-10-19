use std::fs;
use std::path::Path;

use napi::Result;
use napi_derive::napi;
use serde::Deserialize;

use crate::error::{err, thread_local};
use crate::repository::Repository;

#[derive(Debug, Deserialize)]
pub struct ManifestConfig {
    #[serde(rename = "commit.gpgsign")]
    pub commit_gpgsign: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub r#type: Option<String>,
    pub owner: Option<String>,
    pub description: Option<String>,
    pub config: Option<Vec<ManifestConfig>>,
    pub remote: Option<Vec<String>>,
}

/// Helper function to parse `repository.manifest.yml` and configure repository remotes using gix.
///
/// It reads the manifest file from `manifest_path` (or `repository.manifest.yml` relative to the repository workDir/gitDir),
/// parses the remote URLs listed under `remote`, and adds or updates remotes on the gix Repository.
#[napi]
pub fn setup_remotes_from_manifest(
    repo: &Repository,
    manifest_path: Option<String>,
) -> Result<Vec<String>> {
    let repo_dir = repo.work_dir().unwrap_or_else(|| repo.git_dir());
    let path_str = manifest_path.unwrap_or_else(|| {
        Path::new(&repo_dir)
            .join("repository.manifest.yml")
            .to_string_lossy()
            .into_owned()
    });

    let content = fs::read_to_string(&path_str)
        .map_err(|e| err("read_manifest", format!("failed to read {path_str}: {e}")))?;

    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| err("parse_manifest", format!("failed to parse YAML in {path_str}: {e}")))?;

    let remotes = manifest.remote.unwrap_or_default();
    let mut configured_remotes = Vec::new();

    let mut r = thread_local(&repo.inner);
    let local_config_path = r.git_dir().join("config");

    let mut local_file = if local_config_path.exists() {
        gix::config::File::from_path_no_includes(
            local_config_path.clone(),
            gix::config::Source::Local,
        )
        .ok()
    } else {
        None
    };

    let mut snapshot = r.config_snapshot_mut();

    for remote_url in remotes {
        let remote_name = derive_remote_name(&remote_url);
        let key = format!("remote.{remote_name}.url");

        snapshot
            .set_raw_value(&key, remote_url.as_str())
            .map_err(|e| err("setup_remote", format!("failed setting remote {remote_name}: {e}")))?;

        if let Some(ref mut file) = local_file {
            let _ = file.set_raw_value(&key, remote_url.as_str());
        }

        configured_remotes.push(format!("{}: {}", remote_name, remote_url));
    }

    let _ = snapshot.commit();

    if let Some(file) = local_file {
        let _ = fs::write(&local_config_path, file.to_bstring().as_slice());
    }

    Ok(configured_remotes)
}

fn derive_remote_name(url: &str) -> String {
    if url.contains("github.com") {
        "github".to_string()
    } else if url.contains("codeberg.org") {
        "codeberg".to_string()
    } else if url.contains("gitlab.com") {
        "gitlab".to_string()
    } else if url.contains("bitbucket.org") {
        "bitbucket".to_string()
    } else if url.contains("dev.azure.com") {
        "azure".to_string()
    } else if url.contains("repo.chen.software") {
        "origin".to_string()
    } else {
        "origin".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setup_remotes_from_manifest() {
        let repo = Repository::open(".".to_string()).expect("open repo");
        let remotes = setup_remotes_from_manifest(&repo, None).expect("setup remotes");
        assert!(!remotes.is_empty());
        assert!(remotes.iter().any(|r| r.contains("github")));
    }
}
