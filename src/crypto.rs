use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, Key, KeyInit, Nonce};
use gix::bstr::ByteSlice;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use rand::Rng;
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

    #[cfg(feature = "gpg")]
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
        let mut key = [0u8; KEY_SIZE];
        rand::rng().fill_bytes(&mut key);
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

    /// Return raw key bytes as a NAPI Buffer.
    #[napi]
    pub fn to_buffer(&self) -> Buffer {
        Buffer::from(self.key.to_vec())
    }

    /// Encrypt `plaintext` into `MAGIC_HEADER || nonce || ciphertext+tag` (ChaCha20-Poly1305).
    #[napi]
    pub fn encrypt(&self, plaintext: Buffer) -> Result<Buffer> {
        let out = self.encrypt_bytes(plaintext.as_ref())?;
        Ok(Buffer::from(out))
    }

    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);

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
    #[napi]
    pub fn decrypt(&self, ciphertext: Buffer) -> Result<Buffer> {
        let out = self.decrypt_bytes(ciphertext.as_ref())?;
        Ok(Buffer::from(out))
    }

    pub fn decrypt_bytes(&self, ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
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
    #[napi]
    pub fn is_encrypted(data: Buffer) -> bool {
        Self::is_encrypted_bytes(data.as_ref())
    }

    pub fn is_encrypted_bytes(data: &[u8]) -> bool {
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
        let out = key.encrypt_bytes(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn decrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt_bytes(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        SymmetricCryptoKey::is_encrypted_bytes(data.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Standalone Git / git-crypt helper functions matching C++ utilities
// ---------------------------------------------------------------------------

fn get_exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "git-crypt".to_string())
}

fn attribute_name(key_name: Option<&str>) -> String {
    match key_name {
        Some(k) if !k.is_empty() => format!("git-crypt-{k}"),
        _ => "git-crypt".to_string(),
    }
}

/// Configure git-crypt clean, smudge, required and diff filters in git config for optional key_name.
#[napi(js_name = "configureGitFilters")]
pub fn configure_git_filters(key_name: Option<String>) -> Result<()> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let git_dir = repo.git_dir();
    let config_path = git_dir.join("config");

    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
    };

    let exe = get_exe_path();
    let attr = attribute_name(key_name.as_deref());

    let (smudge, clean, diff) = if let Some(ref k) = key_name {
        (
            format!("{exe} smudge --key-name={k}"),
            format!("{exe} clean --key-name={k}"),
            format!("{exe} diff --key-name={k}"),
        )
    } else {
        (
            format!("{exe} smudge"),
            format!("{exe} clean"),
            format!("{exe} diff"),
        )
    };

    config
        .set_raw_value(&format!("filter.{attr}.smudge"), &smudge)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    config
        .set_raw_value(&format!("filter.{attr}.clean"), &clean)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    config
        .set_raw_value(&format!("{attr}.required"), "true")
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    config
        .set_raw_value(&format!("diff.{attr}.textconv"), &diff)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut file = File::create(&config_path).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    config
        .write_to(&mut file)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    Ok(())
}

/// Deconfigure git-crypt filters from git config for optional key_name.
#[napi(js_name = "deconfigureGitFilters")]
pub fn deconfigure_git_filters(key_name: Option<String>) -> Result<()> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let git_dir = repo.git_dir();
    let config_path = git_dir.join("config");

    if !config_path.exists() {
        return Ok(());
    }

    let mut config = gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let attr = attribute_name(key_name.as_deref());
    let _ = config.remove_section("filter", Some(attr.as_str().into()));
    let _ = config.remove_section("diff", Some(attr.as_str().into()));

    let mut file = File::create(&config_path).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    config
        .write_to(&mut file)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    Ok(())
}

/// Get internal state path (`<git-dir>/git-crypt`).
#[napi(js_name = "getInternalStatePath")]
pub fn get_internal_state_path() -> Result<String> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(repo.git_dir().join("git-crypt").to_string_lossy().into_owned())
}

/// Get internal keys path (`<git-dir>/git-crypt/keys` or `<git-dir>/git-crypt/keys/<keyName>`).
#[napi(js_name = "getInternalKeysPath")]
pub fn get_internal_keys_path(key_name: Option<String>) -> Result<String> {
    let state = get_internal_state_path()?;
    let keys = format!("{state}/keys");
    if let Some(k) = key_name {
        Ok(format!("{keys}/{k}"))
    } else {
        Ok(keys)
    }
}

/// Get repo state path (`<worktree>/.git-crypt`).
#[napi(js_name = "getRepoStatePath")]
pub fn get_repo_state_path() -> Result<String> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| napi::Error::from_reason("Bare repository has no working tree"))?;

    if let Some(repo_state_dir) = repo.config_snapshot().string("git-crypt.repoStateDir") {
        let rel = String::from_utf8_lossy(&repo_state_dir).into_owned();
        Ok(workdir.join(rel).to_string_lossy().into_owned())
    } else {
        Ok(workdir.join(".git-crypt").to_string_lossy().into_owned())
    }
}

/// Get repo keys path (`<worktree>/.git-crypt/keys`).
#[napi(js_name = "getRepoKeysPath")]
pub fn get_repo_keys_path() -> Result<String> {
    let state = get_repo_state_path()?;
    Ok(format!("{state}/keys"))
}

#[napi(object)]
pub struct FileAttributes {
    pub filter: String,
    pub diff: String,
}

/// Get `filter` and `diff` attributes for a file.
#[napi(js_name = "getFileAttributes")]
pub fn get_file_attributes(filename: String) -> Result<FileAttributes> {
    let repo = crate::repository::Repository::discover(None)?;
    let entries = repo.attributes_only(filename, None)?;
    let mut filter = String::new();
    let mut diff = String::new();

    for entry in entries {
        if entry.name == "filter" {
            filter = entry.value;
        } else if entry.name == "diff" {
            diff = entry.value;
        }
    }

    Ok(FileAttributes { filter, diff })
}

/// Check if blob bytes carry the git-crypt header (`\0GITCRYPT\0`).
#[napi(js_name = "checkIfBlobIsEncrypted")]
pub fn check_if_blob_is_encrypted(object_id: String) -> Result<bool> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let oid = gix::ObjectId::from_hex(object_id.as_bytes())
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let obj = repo
        .find_object(oid)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let data = obj.data.as_slice();
    Ok(data.len() >= 10 && &data[..10] == b"\0GITCRYPT\0")
}

/// Check if file is encrypted in the repository index.
#[napi(js_name = "checkIfFileIsEncrypted")]
pub fn check_if_file_is_encrypted(filename: String) -> Result<bool> {
    let repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let index = repo.open_index().map_err(|e| napi::Error::from_reason(e.to_string()))?;

    if let Some(entry) = index.entry_by_path(filename.as_bytes().as_bstr()) {
        return check_if_blob_is_encrypted(entry.id.to_string());
    }

    Ok(false)
}

/// Get list of encrypted file paths for a given key_name (or default).
#[napi(js_name = "getEncryptedFiles")]
pub fn get_encrypted_files(key_name: Option<String>) -> Result<Vec<String>> {
    let repo = crate::repository::Repository::discover(None)?;
    let target_attr = attribute_name(key_name.as_deref());

    let gix_repo = gix::discover(".").map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let index = gix_repo.open_index().map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut files = Vec::new();

    for entry in index.entries() {
        let path = String::from_utf8_lossy(entry.path(&index)).into_owned();
        if let Ok(entries) = repo.attributes_only(path.clone(), None) {
            for attr in entries {
                if attr.name == "filter" && (attr.value.contains(&target_attr) || attr.value.contains("git-crypt")) {
                    files.push(path.clone());
                    break;
                }
            }
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attribute_name_helper() {
        assert_eq!(attribute_name(None), "git-crypt");
        assert_eq!(attribute_name(Some("key1")), "git-crypt-key1");
    }
}
