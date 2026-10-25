use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, Key, KeyInit, Nonce};
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use thiserror::Error;

use crate::error::thread_local;

/// Domain errors for the crypt subsystem. Converted into [`napi::Error`] at the NAPI
/// boundary so internal helpers can stay typed and use `?`.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("git error: {0}")]
    Git(String),

    #[error("cryptography error: {0}")]
    Crypto(String),

    #[error("repository not initialized for crypt; run init() first")]
    NotInitialized,

    #[error("repository already initialized for crypt")]
    AlreadyInitialized,

    #[error("key not found: {0}")]
    KeyNotFound(String),

    #[error("invalid key format")]
    InvalidKeyFormat,

    #[cfg(feature = "ssh")]
    #[error("ssh key error: {0}")]
    Ssh(String),

    #[error("gpg error: {0}")]
    Gpg(String),

    #[cfg(feature = "xmss")]
    #[error("xmss error: {0}")]
    Xmss(String),
}

/// Alias for internal, typed results.
pub type CryptoResult<T> = std::result::Result<T, CryptoError>;

impl From<CryptoError> for napi::Error {
    fn from(e: CryptoError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Reverse conversion so internal `CryptoResult`-returning helpers can `?`-propagate
/// errors produced by NAPI-typed helpers (e.g. `SymmetricCryptoKey::from_bytes`).
impl From<napi::Error> for CryptoError {
    fn from(e: napi::Error) -> Self {
        CryptoError::Crypto(e.to_string())
    }
}

/// Key size in bytes (256 bits).
pub const KEY_SIZE: usize = 32;
/// Nonce size in bytes (96 bits, ChaCha20-Poly1305 standard).
pub const NONCE_SIZE: usize = 12;
/// Magic header identifying crypt-encrypted payloads.
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

/// Batch size for git checkout operations.
pub const GIT_CHECKOUT_BATCH_SIZE: usize = 500;

/// A symmetric ChaCha20-Poly1305 key. Exposed to JS as the `SymmetricCryptoKey` class.
#[napi(js_name = "SymmetricCryptoKey")]
#[derive(Clone)]
pub struct SymmetricCryptoKey {
    key: [u8; KEY_SIZE],
}

#[napi]
impl SymmetricCryptoKey {
    #[napi(constructor)]
    pub fn new(bytes: Option<&[u8]>) -> Result<Self> {
        match bytes {
            Some(bytes) => Ok(Self::from_bytes(bytes)?),
            None => Ok(Self::generate_random()),
        }
    }

    /// Generate a new random key from the OS CSPRNG.
    #[napi(factory)]
    pub fn generate_random() -> Self {
        use rand::RngCore as _;
        let mut key = [0u8; KEY_SIZE];
        rand::thread_rng().fill_bytes(&mut key);
        Self { key }
    }

    /// Build a key from raw bytes, rejecting any length other than [`KEY_SIZE`].
    #[napi]
    pub fn from_bytes(bytes: &[u8]) -> napi::Result<SymmetricCryptoKey> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKeyFormat.into());
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Borrow the raw key bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Encrypt `plaintext` into `MAGIC_HEADER || nonce || ciphertext+tag` (ChaCha20-Poly1305).
    pub fn encrypt(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        use rand::RngCore as _;
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(
                Nonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes),
                plaintext,
            )
            .map_err(|_| CryptoError::Crypto("encryption failed".into()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Decrypt a payload produced by [`SymmetricCryptoKey::encrypt`].
    pub fn decrypt(&self, ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(CryptoError::Crypto("ciphertext too short".into()));
        }
        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(CryptoError::Crypto("invalid encrypted data format".into()));
        }

        let data = &ciphertext[MAGIC_HEADER.len()..];
        let (nonce_bytes, enc) = data.split_at(NONCE_SIZE);

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        cipher
            .decrypt(Nonce::<ChaCha20Poly1305>::from_slice(nonce_bytes), enc)
            .map_err(|_| CryptoError::Crypto("decryption failed (wrong key or tampered)".into()))
    }

    /// Whether `data` carries the crypt magic header.
    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

pub use crate::vault::KeyManager;
#[cfg(feature = "ssh")]
pub use crate::vault::IdentityManager;

/// A crypt handle bound to an open repository. Obtain via `Repository::crypt()`.
#[napi]
pub struct Crypto {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

/// Options controlling the git clean/smudge/diff filter registered by [`Crypto::init`].
#[napi(object)]
pub struct CryptFilterOptions {
    pub filter_name: Option<String>,
    pub clean: Option<String>,
    pub smudge: Option<String>,
    pub diff: Option<String>,
}

impl Default for CryptFilterOptions {
    fn default() -> Self {
        Self {
            filter_name: None,
            clean: None,
            smudge: None,
            diff: None,
        }
    }
}

struct ResolvedFilter {
    name: String,
    clean: String,
    smudge: String,
    diff: String,
}

impl CryptFilterOptions {
    fn resolve(&self) -> ResolvedFilter {
        ResolvedFilter {
            name: self
                .filter_name
                .clone()
                .unwrap_or_else(|| "crypto".to_string()),
            clean: self
                .clean
                .clone()
                .unwrap_or_else(|| "crypto clean %f".to_string()),
            smudge: self
                .smudge
                .clone()
                .unwrap_or_else(|| "crypto smudge %f".to_string()),
            diff: self
                .diff
                .clone()
                .unwrap_or_else(|| "crypto diff".to_string()),
        }
    }
}

impl Crypto {
    fn gix(&self) -> gix::Repository {
        thread_local(&self.repo)
    }

    fn key_manager(&self) -> KeyManager {
        KeyManager::new(self.gix().git_dir())
    }

    fn load_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized);
        }
        km.load_key()
    }

    fn config_path(&self) -> PathBuf {
        self.gix().git_dir().join("config")
    }

    fn configure_filters(&self, f: &ResolvedFilter) -> CryptoResult<()> {
        let config_path = self.config_path();
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(
                config_path.clone(),
                gix::config::Source::Local,
            )
            .map_err(|e| CryptoError::Git(e.to_string()))?
        } else {
            gix::config::File::new(gix::config::file::Metadata::from(
                gix::config::Source::Local,
            ))
        };

        let prefix = format!("filter.{}", f.name);
        config
            .set_raw_value(&format!("{prefix}.clean"), &f.clean)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.smudge"), &f.smudge)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.diff"), &f.diff)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.required"), "true")
            .map_err(|e| CryptoError::Git(e.to_string()))?;

        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }

    fn remove_filters(&self, name: &str) -> CryptoResult<()> {
        let config_path = self.config_path();
        if !config_path.exists() {
            return Ok(());
        }
        let mut config = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| CryptoError::Git(e.to_string()))?;
        let _ = config.remove_section("filter", Some(name.into()));
        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }
}

#[napi]
impl Crypto {
    #[napi]
    pub fn is_initialized(&self) -> bool {
        self.key_manager().is_initialized()
    }

    #[napi]
    pub fn key_dir(&self) -> String {
        self.key_manager()
            .git_crypt_dir()
            .to_string_lossy()
            .into_owned()
    }

    #[napi]
    pub fn generate_key(&self) -> Result<SymmetricCryptoKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        Ok(km.generate_key()?)
    }

    #[napi]
    pub fn init(&self, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if km.is_initialized() {
            return Ok(());
        }
        km.init_dirs()?;
        let _key = km.generate_key()?;
        self.configure_filters(&opts.unwrap_or_default().resolve())?;
        Ok(())
    }

    #[napi]
    pub fn lock(&self, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        let name = opts.unwrap_or_default().resolve().name;
        self.remove_filters(&name)?;
        Ok(())
    }

    #[napi]
    pub fn unlock(&self, key_file: Option<String>, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        if let Some(kf) = key_file {
            km.import_key(Path::new(&kf))?;
        }
        let _key = km.load_key()?;
        self.configure_filters(&opts.unwrap_or_default().resolve())?;
        Ok(())
    }

    #[napi]
    pub fn export_key(&self, output_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    #[napi]
    pub fn import_key(&self, input_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    #[napi]
    pub fn encrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn decrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        SymmetricCryptoKey::is_encrypted(data.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Git filter and path configuration functions
// ---------------------------------------------------------------------------

pub fn configure_git_filters(key_name: Option<&str>) -> CryptoResult<()> {
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "git-crypt".to_string());
    let escaped_path = format!("\"{exe_path}\"");

    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let config_path = repo.git_dir().join("config");
    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptoError::Git(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
    };

    let attr_name = attribute_name(key_name);

    let smudge_cmd = match key_name {
        Some(k) => format!("{escaped_path} smudge --key-name={k}"),
        None => format!("{escaped_path} smudge"),
    };
    let clean_cmd = match key_name {
        Some(k) => format!("{escaped_path} clean --key-name={k}"),
        None => format!("{escaped_path} clean"),
    };
    let diff_cmd = match key_name {
        Some(k) => format!("{escaped_path} diff --key-name={k}"),
        None => format!("{escaped_path} diff"),
    };

    config
        .set_raw_value(&format!("filter.{attr_name}.smudge"), &smudge_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("filter.{attr_name}.clean"), &clean_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("filter.{attr_name}.required"), "true")
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("diff.{attr_name}.textconv"), &diff_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;

    let mut file = std::fs::File::create(&config_path)?;
    config.write_to(&mut file)?;
    Ok(())
}

pub fn deconfigure_git_filters(key_name: Option<&str>) -> CryptoResult<()> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let config_path = repo.git_dir().join("config");
    if !config_path.exists() {
        return Ok(());
    }
    let mut config = gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
        .map_err(|e| CryptoError::Git(e.to_string()))?;

    let attr_name = attribute_name(key_name);

    let _ = config.remove_section("filter", Some(attr_name.as_str().into()));
    let _ = config.remove_section("diff", Some(attr_name.as_str().into()));

    let mut file = std::fs::File::create(&config_path)?;
    config.write_to(&mut file)?;
    Ok(())
}

pub fn git_checkout_batch(paths: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("checkout").arg("--");
    for path in paths {
        cmd.arg(path);
    }
    match cmd.status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

pub fn git_checkout(paths: &[String]) -> bool {
    for chunk in paths.chunks(GIT_CHECKOUT_BATCH_SIZE) {
        if !git_checkout_batch(chunk) {
            return false;
        }
    }
    true
}

pub fn validate_key_name(key_name: Option<&str>) -> std::result::Result<(), String> {
    if let Some(name) = key_name {
        if name.is_empty() {
            return Err("Key name cannot be empty".to_string());
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
            return Err("Key name contains invalid characters".to_string());
        }
    }
    Ok(())
}

pub fn validate_key_name_or_throw(key_name: Option<&str>) -> CryptoResult<()> {
    validate_key_name(key_name).map_err(CryptoError::Crypto)
}

pub fn get_git_config(name: &str) -> CryptoResult<String> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let val = repo
        .config_snapshot()
        .string(name)
        .ok_or_else(|| CryptoError::Git(format!("'git config' missing value for key '{name}'")))?;
    Ok(val.to_string())
}

pub fn get_internal_state_path() -> CryptoResult<PathBuf> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    Ok(repo.git_dir().join("git-crypt"))
}

pub fn get_internal_keys_path() -> CryptoResult<PathBuf> {
    Ok(get_internal_state_path()?.join("keys"))
}

pub fn get_internal_key_path(key_name: Option<&str>) -> CryptoResult<PathBuf> {
    let keys_dir = get_internal_keys_path()?;
    let name = key_name.unwrap_or("default");
    Ok(keys_dir.join(name))
}

pub fn get_repo_state_path() -> CryptoResult<PathBuf> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let top = repo
        .workdir()
        .ok_or_else(|| CryptoError::Git("Could not determine Git working tree".to_string()))?;

    if let Ok(rel) = get_git_config("git-crypt.repoStateDir") {
        Ok(top.join(rel))
    } else {
        Ok(top.join(".git-crypt"))
    }
}

pub fn get_repo_keys_path() -> CryptoResult<PathBuf> {
    Ok(get_repo_state_path()?.join("keys"))
}

pub fn get_path_to_top() -> CryptoResult<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-cdup"])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git rev-parse --show-cdup' failed".to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn get_git_status() -> CryptoResult<String> {
    let output = std::process::Command::new("git")
        .args(["status", "-uno", "--porcelain"])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git status' failed".to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn get_file_attributes(filename: &str) -> CryptoResult<(String, String)> {
    let output = std::process::Command::new("git")
        .args(["check-attr", "filter", "diff", "--", filename])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git check-attr' failed".to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut filter_attr = String::new();
    let mut diff_attr = String::new();

    for line in stdout.lines() {
        if let Some(value_pos) = line.rfind(": ") {
            if value_pos == 0 {
                continue;
            }
            if let Some(name_pos) = line[..value_pos].rfind(": ") {
                let attr_name = &line[name_pos + 2..value_pos];
                let attr_value = &line[value_pos + 2..];
                if attr_value != "unspecified" && attr_value != "unset" && attr_value != "set" {
                    if attr_name == "filter" {
                        filter_attr = attr_value.to_string();
                    } else if attr_name == "diff" {
                        diff_attr = attr_value.to_string();
                    }
                }
            }
        }
    }
    Ok((filter_attr, diff_attr))
}

pub fn check_if_blob_is_encrypted(object_id: &str) -> CryptoResult<bool> {
    let output = std::process::Command::new("git")
        .args(["cat-file", "blob", object_id])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git cat-file' failed".to_string()));
    }
    Ok(SymmetricCryptoKey::is_encrypted(&output.stdout))
}

pub fn check_if_file_is_encrypted(filename: &str) -> CryptoResult<bool> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-sz", "--", filename])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git ls-files' failed".to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() >= 2 {
        let object_id = parts[1];
        return check_if_blob_is_encrypted(object_id);
    }
    Ok(false)
}

pub fn attribute_name(key_name: Option<&str>) -> String {
    match key_name {
        Some(k) => format!("git-crypt-{k}"),
        None => "git-crypt".to_string(),
    }
}

pub fn get_encrypted_files(files: &mut Vec<String>, key_name: Option<&str>) -> CryptoResult<()> {
    let target_attr = attribute_name(key_name);
    let path_to_top = get_path_to_top().unwrap_or_default();
    let mut args = vec!["ls-files", "-csz", "--"];
    if !path_to_top.is_empty() {
        args.push(&path_to_top);
    }
    let output = std::process::Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git ls-files' failed".to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for entry in stdout.split('\0') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split_whitespace().collect();
        if parts.len() >= 4 {
            let filename = parts[3];
            if let Ok((filter_attr, _)) = get_file_attributes(filename) {
                if filter_attr == target_attr {
                    files.push(filename.to_string());
                }
            }
        }
    }
    Ok(())
}

pub fn decrypt_repo_key(
    key_name: Option<&str>,
    key_version: u32,
    secret_keys: &[String],
    keys_path: &Path,
) -> CryptoResult<Vec<u8>> {
    let key_dir_name = key_name.unwrap_or("default");
    for seckey in secret_keys {
        let file_path = keys_path.join(key_dir_name).join(key_version.to_string()).join(format!("{seckey}.gpg"));
        if file_path.exists() {
            if let Ok(decrypted) = crate::gpg::gpg_decrypt_from_file(file_path.to_string_lossy().into_owned()) {
                return Ok(decrypted.to_vec());
            }
        }
    }
    Err(CryptoError::Gpg("Failed to decrypt repo key with GPG".to_string()))
}

pub fn encrypt_repo_key(
    key_name: Option<&str>,
    key_bytes: &[u8],
    collab_keys: &[(String, bool)],
    keys_path: &Path,
    key_version: u32,
) -> CryptoResult<Vec<String>> {
    let mut new_files = Vec::new();
    let key_dir_name = key_name.unwrap_or("default");

    for (fingerprint, key_is_trusted) in collab_keys {
        let file_path = keys_path
            .join(key_dir_name)
            .join(key_version.to_string())
            .join(format!("{fingerprint}.gpg"));

        if file_path.exists() {
            continue;
        }

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        crate::gpg::gpg_encrypt_to_file(
            file_path.to_string_lossy().into_owned(),
            fingerprint.clone(),
            *key_is_trusted,
            Buffer::from(key_bytes.to_vec()),
        )
        .map_err(|e| CryptoError::Gpg(e.to_string()))?;

        new_files.push(file_path.to_string_lossy().into_owned());
    }

    Ok(new_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_key_name() {
        assert!(validate_key_name(None).is_ok());
        assert!(validate_key_name(Some("valid_key-1.0")).is_ok());
        assert!(validate_key_name(Some("invalid key!")).is_err());
        assert!(validate_key_name(Some("")).is_err());
    }

    #[test]
    fn test_attribute_name() {
        assert_eq!(attribute_name(None), "git-crypt");
        assert_eq!(attribute_name(Some("mykey")), "git-crypt-mykey");
    }

    #[test]
    fn test_checkout_batch() {
        assert!(git_checkout_batch(&[]));
    }
}
