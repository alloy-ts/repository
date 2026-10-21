use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;

use crate::error::err;

const MAGIC_HEADER: &[u8] = b"GITCRYPT";
const NONCE_LEN: usize = 12;

/// Information about git-crypt status in a repository.
#[derive(Clone)]
#[napi(object)]
pub struct CryptStatus {
    pub initialized: bool,
    pub key_exists: bool,
    pub filters_configured: bool,
}

fn get_git_dir(repo_path: Option<&str>) -> Result<PathBuf> {
    let p = repo_path.unwrap_or(".");
    let repo = gix::discover(p).map_err(|e| err("crypt_discover", e))?;
    Ok(repo.git_dir().to_path_buf())
}

fn git_crypt_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("git-crypt")
}

fn default_key_path(git_dir: &Path) -> PathBuf {
    git_crypt_dir(git_dir).join("keys").join("default")
}

fn read_or_default_key(git_dir: &Path, user_key: Option<&[u8]>) -> [u8; 32] {
    if let Some(k) = user_key {
        if k.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(k);
            return arr;
        }
    }

    let key_path = default_key_path(git_dir);
    if let Ok(bytes) = fs::read(&key_path) {
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            return arr;
        }
    }

    // Default fallback 32-byte key for standalone usage
    [0x42u8; 32]
}

/// Initialize git-crypt in a repository.
#[napi]
pub fn crypt_init(repo_path: Option<String>) -> Result<CryptStatus> {
    let git_dir = get_git_dir(repo_path.as_deref())?;
    let crypt_dir = git_crypt_dir(&git_dir);

    let initialized = crypt_dir.exists();
    if !initialized {
        fs::create_dir_all(crypt_dir.join("keys")).map_err(|e| err("crypt_init_mkdir", e))?;

        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).map_err(|e| err("crypt_getrandom", e))?;

        let key_path = default_key_path(&git_dir);
        let mut file = File::create(&key_path).map_err(|e| err("crypt_init_key_file", e))?;
        file.write_all(&key_bytes)
            .map_err(|e| err("crypt_init_write_key", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&key_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = fs::set_permissions(&key_path, perms);
            }
        }
    }

    // Configure filters
    let config_path = git_dir.join("config");
    if config_path.exists() {
        if let Ok(mut config) = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        ) {
            let _ = config.set_raw_value("filter.git-crypt.clean", "git-crypt clean");
            let _ = config.set_raw_value("filter.git-crypt.smudge", "git-crypt smudge");
            let _ = config.set_raw_value("filter.git-crypt.diff", "git-crypt diff");
            let _ = config.set_raw_value("filter.git-crypt.required", "true");
            let _ = fs::write(&config_path, config.to_bstring().as_slice());
        }
    }

    let key_exists = default_key_path(&git_dir).exists();
    Ok(CryptStatus {
        initialized: true,
        key_exists,
        filters_configured: true,
    })
}

/// Check if git-crypt is initialized in a repository.
#[napi]
pub fn crypt_is_initialized(repo_path: Option<String>) -> Result<bool> {
    let git_dir = get_git_dir(repo_path.as_deref())?;
    Ok(git_crypt_dir(&git_dir).exists())
}

/// Lock repository by removing git-crypt filters.
#[napi]
pub fn crypt_lock(repo_path: Option<String>) -> Result<()> {
    let git_dir = get_git_dir(repo_path.as_deref())?;
    let config_path = git_dir.join("config");
    if config_path.exists() {
        if let Ok(mut config) = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        ) {
            let _ = config.remove_section("filter", Some("git-crypt".into()));
            let _ = fs::write(&config_path, config.to_bstring().as_slice());
        }
    }
    Ok(())
}

/// Unlock repository by restoring git-crypt filters and optionally importing key.
#[napi]
pub fn crypt_unlock(repo_path: Option<String>, key_file: Option<String>) -> Result<()> {
    let git_dir = get_git_dir(repo_path.as_deref())?;

    if let Some(key_path_str) = key_file {
        let input_path = Path::new(&key_path_str);
        if input_path.exists() {
            crypt_import_key(repo_path.clone(), key_path_str)?;
        }
    }

    let config_path = git_dir.join("config");
    if config_path.exists() {
        if let Ok(mut config) = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        ) {
            let _ = config.set_raw_value("filter.git-crypt.clean", "git-crypt clean");
            let _ = config.set_raw_value("filter.git-crypt.smudge", "git-crypt smudge");
            let _ = config.set_raw_value("filter.git-crypt.diff", "git-crypt diff");
            let _ = config.set_raw_value("filter.git-crypt.required", "true");
            let _ = fs::write(&config_path, config.to_bstring().as_slice());
        }
    }
    Ok(())
}

/// Export default key to output file.
#[napi]
pub fn crypt_export_key(repo_path: Option<String>, output_path: String) -> Result<()> {
    let git_dir = get_git_dir(repo_path.as_deref())?;
    let key_path = default_key_path(&git_dir);

    if !key_path.exists() {
        return Err(err("crypt_export_key", "git-crypt key not found"));
    }

    let key_bytes = fs::read(&key_path).map_err(|e| err("crypt_read_key", e))?;
    let out = Path::new(&output_path);
    if let Some(parent) = out.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(out, key_bytes).map_err(|e| err("crypt_write_exported_key", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(out) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = fs::set_permissions(out, perms);
        }
    }

    Ok(())
}

/// Import key from input file as default key.
#[napi]
pub fn crypt_import_key(repo_path: Option<String>, input_path: String) -> Result<()> {
    let git_dir = get_git_dir(repo_path.as_deref())?;
    let key_bytes = fs::read(&input_path).map_err(|e| err("crypt_import_key_read", e))?;

    let key_path = default_key_path(&git_dir);
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent).map_err(|e| err("crypt_import_mkdir", e))?;
    }

    fs::write(&key_path, key_bytes).map_err(|e| err("crypt_import_write", e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&key_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = fs::set_permissions(&key_path, perms);
        }
    }

    Ok(())
}

/// Clean filter for git-crypt using AES-256-GCM.
#[napi]
pub fn crypt_clean(
    input: Buffer,
    key_bytes: Option<Buffer>,
    repo_path: Option<String>,
) -> Result<Buffer> {
    let data = input.to_vec();
    if data.starts_with(MAGIC_HEADER) {
        return Ok(Buffer::from(data));
    }

    let git_dir = get_git_dir(repo_path.as_deref()).ok();
    let key = read_or_default_key(
        git_dir.as_deref().unwrap_or_else(|| Path::new(".")),
        key_bytes.as_deref().map(|b| b.as_ref()),
    );

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| err("aes_key", e))?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).map_err(|e| err("nonce_gen", e))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher.encrypt(nonce, data.as_slice()).map_err(|e| err("aes_encrypt", e))?;

    let mut out = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC_HEADER);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);

    Ok(Buffer::from(out))
}

/// Smudge filter for git-crypt using AES-256-GCM.
#[napi]
pub fn crypt_smudge(
    input: Buffer,
    key_bytes: Option<Buffer>,
    repo_path: Option<String>,
) -> Result<Buffer> {
    let data = input.to_vec();
    let min_len = MAGIC_HEADER.len() + NONCE_LEN;
    if !data.starts_with(MAGIC_HEADER) || data.len() < min_len {
        return Ok(Buffer::from(data));
    }

    let git_dir = get_git_dir(repo_path.as_deref()).ok();
    let key = read_or_default_key(
        git_dir.as_deref().unwrap_or_else(|| Path::new(".")),
        key_bytes.as_deref().map(|b| b.as_ref()),
    );

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| err("aes_key", e))?;

    let nonce_bytes = &data[MAGIC_HEADER.len()..MAGIC_HEADER.len() + NONCE_LEN];
    let ciphertext = &data[MAGIC_HEADER.len() + NONCE_LEN..];
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|e| err("aes_decrypt", e))?;

    Ok(Buffer::from(plaintext))
}
