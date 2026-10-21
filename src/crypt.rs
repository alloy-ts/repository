use napi::Result;
use napi_derive::napi;

use crate::error::err;

/// Configure git-crypt filters in repository config.
#[napi]
pub fn crypt_configure_filters(repo_path: Option<String>) -> Result<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_configure_filters", e))?;
    let config_path = repo.git_dir().join("config");

    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| err("crypt_configure_filters", e))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
    };

    config
        .set_raw_value(&"filter.git-crypt.clean", "git-crypt clean")
        .map_err(|e| err("crypt_configure_filters", e))?;
    config
        .set_raw_value(&"filter.git-crypt.smudge", "git-crypt smudge")
        .map_err(|e| err("crypt_configure_filters", e))?;
    config
        .set_raw_value(&"filter.git-crypt.diff", "git-crypt diff")
        .map_err(|e| err("crypt_configure_filters", e))?;
    config
        .set_raw_value(&"filter.git-crypt.required", "true")
        .map_err(|e| err("crypt_configure_filters", e))?;

    let mut file = std::fs::File::create(&config_path).map_err(|e| err("crypt_configure_filters", e))?;
    config.write_to(&mut file).map_err(|e| err("crypt_configure_filters", e))?;

    Ok(())
}

/// Remove git-crypt filters from repository config.
#[napi]
pub fn crypt_remove_filters(repo_path: Option<String>) -> Result<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_remove_filters", e))?;
    let config_path = repo.git_dir().join("config");

    if !config_path.exists() {
        return Ok(());
    }

    let mut config = gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
        .map_err(|e| err("crypt_remove_filters", e))?;

    let _ = config.remove_section("filter", Some("git-crypt".into()));

    let mut file = std::fs::File::create(&config_path).map_err(|e| err("crypt_remove_filters", e))?;
    config.write_to(&mut file).map_err(|e| err("crypt_remove_filters", e))?;

    Ok(())
}

/// Check if a repository is initialized for git-crypt.
#[napi]
pub fn crypt_is_initialized(repo_path: Option<String>) -> Result<bool> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_is_initialized", e))?;
    let git_crypt_dir = repo.git_dir().join("git-crypt");
    Ok(git_crypt_dir.exists())
}

/// Initialize git-crypt structure in repository.
#[napi]
pub fn crypt_init(repo_path: Option<String>) -> Result<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_init", e))?;
    let git_crypt_dir = repo.git_dir().join("git-crypt");

    if git_crypt_dir.exists() {
        return Ok(());
    }

    std::fs::create_dir_all(git_crypt_dir.join("keys")).map_err(|e| err("crypt_init", e))?;

    // Generate random 32-byte key using secure CSPRNG
    let mut key_bytes = [0u8; 32];
    getrandom::getrandom(&mut key_bytes).map_err(|e| err("crypt_init", format!("failed to generate secure random key: {e}")))?;

    let default_key_path = git_crypt_dir.join("keys").join("default");
    std::fs::write(&default_key_path, &key_bytes).map_err(|e| err("crypt_init", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&default_key_path)
            .map_err(|e| err("crypt_init", e))?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&default_key_path, perms);
    }

    crypt_configure_filters(Some(path))?;
    Ok(())
}

/// Export default git-crypt symmetric key to output path.
#[napi]
pub fn crypt_export_key(output_path: String, repo_path: Option<String>) -> Result<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_export_key", e))?;
    let key_path = repo.git_dir().join("git-crypt").join("keys").join("default");

    if !key_path.exists() {
        return Err(err("crypt_export_key", "default key not found"));
    }

    let key_bytes = std::fs::read(&key_path).map_err(|e| err("crypt_export_key", e))?;
    std::fs::write(&output_path, &key_bytes).map_err(|e| err("crypt_export_key", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&output_path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(&output_path, perms);
        }
    }

    Ok(())
}

/// Import git-crypt symmetric key from input path.
#[napi]
pub fn crypt_import_key(input_path: String, repo_path: Option<String>) -> Result<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|e| err("crypt_import_key", e))?;
    let key_bytes = std::fs::read(&input_path).map_err(|e| err("crypt_import_key", e))?;

    if key_bytes.len() != 32 {
        return Err(err("crypt_import_key", "invalid key length, expected 32 bytes"));
    }

    let key_path = repo.git_dir().join("git-crypt").join("keys").join("default");
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err("crypt_import_key", e))?;
    }

    std::fs::write(&key_path, &key_bytes).map_err(|e| err("crypt_import_key", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&key_path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(&key_path, perms);
        }
    }

    Ok(())
}

/// Lock repository (remove git-crypt filters).
#[napi]
pub fn crypt_lock(repo_path: Option<String>) -> Result<()> {
    crypt_remove_filters(repo_path)
}

/// Unlock repository (import optional key file and configure git-crypt filters).
#[napi]
pub fn crypt_unlock(key_path: Option<String>, repo_path: Option<String>) -> Result<()> {
    if let Some(kp) = key_path {
        crypt_import_key(kp, repo_path.clone())?;
    }
    crypt_configure_filters(repo_path)
}
