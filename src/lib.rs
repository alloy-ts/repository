mod error;
mod object;
mod reference;
mod remote;
mod repository;
pub mod util;

use napi::Result;
use napi_derive::napi;
use std::path::Path;

pub use object::*;
pub use reference::*;
pub use remote::*;
pub use repository::*;

#[napi]
pub fn add(left: i64, right: i64) -> i64 {
    left + right
}

/// Setup remotes in `repository` using the manifest file at `manifest_path` (defaulting to `repository.manifest.yml`).
#[napi]
pub fn setup_remotes_from_manifest(
    repository: &Repository,
    manifest_path: Option<String>,
) -> Result<Vec<String>> {
    let default_path = "repository.manifest.yml".to_string();
    let path_str = manifest_path.unwrap_or(default_path);
    util::sync_manifest_remotes(&repository.inner, Path::new(&path_str))
}
