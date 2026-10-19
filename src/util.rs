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
/// It attempts to load `repository.manifest.yml` using the gix revision API from the provided `revision_or_path`
/// (defaulting to `"HEAD"`). If `revision_or_path` refers to an existing file on disk, it reads from disk instead.
#[napi]
pub fn setup_remotes_from_manifest(
    repo: &Repository,
    revision_or_path: Option<String>,
) -> Result<Vec<String>> {
    let spec_or_path = revision_or_path.unwrap_or_else(|| "HEAD".to_string());
    let path = Path::new(&spec_or_path);

    let content = if path.exists() && path.is_file() {
        fs::read_to_string(path)
            .map_err(|e| err("read_manifest_file", format!("failed reading {spec_or_path}: {e}")))?
    } else {
        read_manifest_from_revision(repo, &spec_or_path)?
    };

    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| err("parse_manifest", format!("failed parsing manifest YAML: {e}")))?;

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

fn read_manifest_from_revision(repo: &Repository, rev: &str) -> Result<String> {
    let r = thread_local(&repo.inner);
    let oid = r
        .rev_parse_single(rev)
        .map_err(|e| err("rev_parse_single", format!("invalid revision '{rev}': {e}")))?;
    let commit = r
        .find_commit(oid)
        .map_err(|e| err("find_commit", format!("failed finding commit for '{rev}': {e}")))?;
    let tree = commit
        .tree()
        .map_err(|e| err("commit_tree", format!("failed getting tree for '{rev}': {e}")))?;

    let entry = tree
        .lookup_entry_by_path("repository.manifest.yml")
        .map_err(|e| err("lookup_manifest", format!("error looking up manifest in '{rev}': {e}")))?
        .ok_or_else(|| err("manifest_not_found", format!("repository.manifest.yml not found in '{rev}'")))?;

    let blob = r
        .find_blob(entry.object_id())
        .map_err(|e| err("find_blob", format!("failed finding manifest blob in '{rev}': {e}")))?;

    String::from_utf8(blob.data.clone())
        .map_err(|e| err("utf8_decode", format!("manifest blob is not valid UTF-8: {e}")))
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

    #[test]
    fn test_read_manifest_from_head_revision() {
        let repo = Repository::open(".".to_string()).expect("open repo");
        let manifest_content = read_manifest_from_revision(&repo, "HEAD").expect("read manifest from HEAD");
        assert!(manifest_content.contains("yumin-chen"));
    }
}
