use std::fs;
use std::path::Path;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// Extract a remote name from a remote URL domain host.
/// e.g. `https://github.com/owner/repo` -> `"github"`
/// `https://repo.chen.software/owner/repo` -> `"origin"`
pub fn remote_name_from_url(url: &str) -> String {
    if url.contains("chen.software") {
        return "origin".to_string();
    }
    if url.contains("codeberg.org") {
        return "codeberg".to_string();
    }
    if url.contains("github.com") {
        return "github".to_string();
    }
    if url.contains("gitlab.com") {
        return "gitlab".to_string();
    }
    if url.contains("bitbucket.org") {
        return "bitbucket".to_string();
    }
    if url.contains("azure.com") {
        return "azure".to_string();
    }
    "remote".to_string()
}

/// Simple helper to parse the `remote:` lines from a YAML manifest file
/// without introducing extra heavy parser dependencies.
pub fn parse_remote_urls_from_manifest(content: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut in_remote_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("remote:") {
            in_remote_section = true;
            continue;
        }

        if in_remote_section {
            if trimmed.starts_with("- ") {
                let url = trimmed.trim_start_matches("- ").trim();
                if !url.is_empty() {
                    urls.push(url.to_string());
                }
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_remote_section = false;
            }
        }
    }

    urls
}

/// Sets up git remotes in `repo` based on the given `repository.manifest.yml` file path.
///
/// Reads the `remote:` array from the manifest, determines remote names (e.g. `origin`, `github`, `codeberg`),
/// and adds or updates the remote configurations in the repository's `.git/config`.
#[napi]
pub fn setup_remotes_from_manifest(
    repo: &Repository,
    manifest_path: Option<String>,
) -> Result<Vec<String>> {
    let path_str = manifest_path.unwrap_or_else(|| "repository.manifest.yml".to_string());
    let path = Path::new(&path_str);

    if !path.exists() {
        return Err(napi::Error::from_reason(format!(
            "Manifest file not found at {}",
            path.display()
        )));
    }

    let content = fs::read_to_string(path)
        .map_err(|e| napi::Error::from_reason(format!("Failed to read manifest file: {e}")))?;

    let remote_urls = parse_remote_urls_from_manifest(&content);
    let r = thread_local(&repo.inner);
    let config_path = r.git_dir().join("config");

    let mut file = if config_path.exists() {
        let config_bytes = fs::read(&config_path)
            .map_err(|e| napi::Error::from_reason(format!("Failed to read git config: {e}")))?;
        gix::config::File::from_bytes_no_includes(&config_bytes, gix::config::file::Metadata::default(), gix::config::file::init::Options::default())
            .map_err(|e| err("parse git config", e))?
    } else {
        gix::config::File::default()
    };

    let mut configured = Vec::new();

    for url_str in &remote_urls {
        let name = remote_name_from_url(url_str);
        let fetch_spec = format!("+refs/heads/*:refs/remotes/{name}/*");

        let url_key = format!("remote.{name}.url");
        let fetch_key = format!("remote.{name}.fetch");

        file.set_raw_value(url_key.as_str(), url_str.as_str())
            .map_err(|e| err("set_raw_value url", e))?;
        file.set_raw_value(fetch_key.as_str(), fetch_spec.as_str())
            .map_err(|e| err("set_raw_value fetch", e))?;

        configured.push(name);
    }

    let mut out = Vec::new();
    file.write_to(&mut out)
        .map_err(|e| napi::Error::from_reason(format!("Failed to write config: {e}")))?;
    fs::write(&config_path, out)
        .map_err(|e| napi::Error::from_reason(format!("Failed to save config file: {e}")))?;

    Ok(configured)
}

#[napi]
impl Repository {
    /// Read `repository.manifest.yml` and set up git remotes on this repository instance.
    #[napi]
    pub fn setup_remotes_from_manifest(
        &self,
        manifest_path: Option<String>,
    ) -> Result<Vec<String>> {
        setup_remotes_from_manifest(self, manifest_path)
    }
}
