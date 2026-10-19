use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};

/// Representation of `repository.manifest.yml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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

/// Helper function to derive a clean remote name from a remote URL.
pub fn derive_remote_name(url: &str, index: usize) -> String {
    if index == 0 {
        return "origin".to_string();
    }
    if let Ok(parsed) = gix::url::parse(url.as_bytes()) {
        if let Some(host) = parsed.host() {
            let host_str = host.to_string();
            let name = host_str
                .trim_start_matches("dev.")
                .trim_start_matches("repo.")
                .split('.')
                .next()
                .unwrap_or("remote");
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    format!("remote_{}", index + 1)
}

/// Read and parse `repository.manifest.yml` from `path`.
pub fn read_manifest(path: impl AsRef<Path>) -> Result<Manifest, String> {
    let content = fs::read_to_string(path.as_ref())
        .map_err(|e| format!("Failed to read manifest file: {e}"))?;
    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| format!("Failed to parse repository.manifest.yml: {e}"))?;
    Ok(manifest)
}

/// Read `repository.manifest.yml` and set up repository remotes using `gix::Repository`.
pub fn setup_remotes_from_manifest(
    repo: &gix::Repository,
    manifest: &Manifest,
) -> Result<Vec<String>, String> {
    let config_path = repo.git_dir().join("config");
    let mut config_file = gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
        .unwrap_or_default();

    let mut configured_names = Vec::new();

    for (i, url_str) in manifest.remote.iter().enumerate() {
        let remote_name = derive_remote_name(url_str, i);
        let mut remote = repo
            .remote_at(url_str.as_str())
            .map_err(|e| format!("Failed to construct remote for {url_str}: {e}"))?;

        remote
            .save_as_to(remote_name.as_str(), &mut config_file)
            .map_err(|e| format!("Failed to save remote {remote_name}: {e}"))?;

        configured_names.push(remote_name);
    }

    // Write updated config back to disk
    let mut out = fs::File::create(&config_path)
        .map_err(|e| format!("Failed to open config for writing: {e}"))?;
    config_file
        .write_to(&mut out)
        .map_err(|e| format!("Failed to write updated git config: {e}"))?;

    Ok(configured_names)
}

/// Reads `repository.manifest.yml` at the repository root and configures all remotes.
pub fn setup_repository_from_manifest(
    repo: &gix::Repository,
    manifest_path: impl AsRef<Path>,
) -> Result<Vec<String>, String> {
    let manifest = read_manifest(manifest_path)?;
    setup_remotes_from_manifest(repo, &manifest)
}

/// Read and parse `repository.manifest.yml` at a specific revision (e.g. `"main"`, `"HEAD"`, or SHA).
pub fn read_manifest_at_revision(
    repo: &gix::Repository,
    revision: &str,
    manifest_path: Option<&str>,
) -> Result<Manifest, String> {
    let path = manifest_path.unwrap_or("repository.manifest.yml");
    let spec = format!("{revision}:{path}");
    let object_id = repo
        .rev_parse_single(spec.as_str())
        .map_err(|e| format!("Failed to resolve revision spec '{spec}': {e}"))?;
    let blob = repo
        .find_blob(object_id)
        .map_err(|e| format!("Failed to find blob at '{spec}': {e}"))?;
    let content = String::from_utf8(blob.data.to_vec())
        .map_err(|e| format!("Invalid UTF-8 in '{spec}': {e}"))?;
    let manifest: Manifest = serde_yaml::from_str(&content)
        .map_err(|e| format!("Failed to parse manifest at '{spec}': {e}"))?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_remote_name() {
        assert_eq!(derive_remote_name("https://repo.chen.software/yumin-chen/repository", 0), "origin");
        assert_eq!(derive_remote_name("https://codeberg.org/yumin-chen/repository", 1), "codeberg");
        assert_eq!(derive_remote_name("https://github.com/yumin-chen/repository", 2), "github");
        assert_eq!(derive_remote_name("https://gitlab.com/yumin-chen/repository", 3), "gitlab");
        assert_eq!(derive_remote_name("https://bitbucket.org/yumin-chen/repository", 4), "bitbucket");
        assert_eq!(derive_remote_name("https://dev.azure.com/yumin-chen/repository/_git/repository", 5), "azure");
    }

    #[test]
    fn test_read_manifest() {
        let manifest = read_manifest("repository.manifest.yml").unwrap();
        assert_eq!(manifest.name.as_deref(), Some("repository"));
        assert_eq!(manifest.owner.as_deref(), Some("yumin-chen"));
        assert!(!manifest.remote.is_empty());
    }

    #[test]
    fn test_read_manifest_at_revision() {
        let repo = gix::open(".").unwrap();
        let manifest = read_manifest_at_revision(&repo, "HEAD", None).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("repository"));
    }

    #[test]
    fn test_setup_remotes_in_temp_repo() {
        let temp = tempfile::tempdir().unwrap();
        let repo = gix::init(temp.path()).unwrap();
        let manifest = read_manifest("repository.manifest.yml").unwrap();
        let names = setup_remotes_from_manifest(&repo, &manifest).unwrap();
        assert_eq!(names[0], "origin");
        assert_eq!(names[1], "codeberg");
        assert_eq!(names[2], "github");

        // Re-open repo and check configured remotes
        let repo_reopened = gix::open(temp.path()).unwrap();
        let remote_names: Vec<String> = repo_reopened
            .remote_names()
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        assert!(remote_names.contains(&"origin".to_string()));
        assert!(remote_names.contains(&"github".to_string()));
    }
}
