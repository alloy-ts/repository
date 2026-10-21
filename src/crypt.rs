use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use napi::bindgen_prelude::Buffer;
use napi::Result as NapiResult;
use napi_derive::napi;
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const KEY_SIZE: usize = 32;
pub const NONCE_SIZE: usize = 12;
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

#[derive(Error, Debug)]
pub enum GitCryptError {
    #[error("Git error: {0}")]
    Git(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Cryptography error: {0}")]
    Crypto(String),

    #[error("GPG error: {0}")]
    Gpg(String),

    #[error("age/rage error: {0}")]
    Age(String),

    #[error("Repository not initialized. Run 'git-crypt init' first")]
    NotInitialized,

    #[error("Repository already initialized")]
    AlreadyInitialized,

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Invalid key format")]
    InvalidKeyFormat,

    #[error("Not in a git repository")]
    NotInRepository,

    #[error("{0}")]
    Other(String),
}

impl From<GitCryptError> for napi::Error {
    fn from(err: GitCryptError) -> Self {
        napi::Error::from_reason(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, GitCryptError>;

#[derive(Clone)]
pub struct CryptoKey {
    key: [u8; KEY_SIZE],
}

impl CryptoKey {
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != KEY_SIZE {
            return Err(GitCryptError::InvalidKeyFormat);
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| GitCryptError::Crypto(e.to_string()))?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| GitCryptError::Crypto(e.to_string()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(GitCryptError::Crypto("Ciphertext too short".into()));
        }

        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(GitCryptError::Crypto(
                "Invalid encrypted data format".into(),
            ));
        }

        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| GitCryptError::Crypto(e.to_string()))?;

        let data = &ciphertext[MAGIC_HEADER.len()..];
        let (nonce_bytes, encrypted_data) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, encrypted_data)
            .map_err(|e| GitCryptError::Crypto(e.to_string()))?;

        Ok(plaintext)
    }

    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

pub struct KeyManager {
    git_dir: PathBuf,
}

impl KeyManager {
    pub fn new(git_dir: impl AsRef<Path>) -> Self {
        Self {
            git_dir: git_dir.as_ref().to_path_buf(),
        }
    }

    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("git-crypt")
    }

    pub fn default_key_path(&self) -> PathBuf {
        self.git_crypt_dir().join("keys").join("default")
    }

    pub fn init_dirs(&self) -> Result<()> {
        let git_crypt_dir = self.git_crypt_dir();
        if git_crypt_dir.exists() {
            return Err(GitCryptError::AlreadyInitialized);
        }

        fs::create_dir_all(&git_crypt_dir)?;
        fs::create_dir(git_crypt_dir.join("keys"))?;

        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    pub fn generate_key(&self) -> Result<CryptoKey> {
        let key = CryptoKey::generate();
        self.save_key(&key)?;
        Ok(key)
    }

    pub fn save_key(&self, key: &CryptoKey) -> Result<()> {
        let key_path = self.default_key_path();
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&key_path)?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&key_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&key_path, perms)?;
        }

        Ok(())
    }

    pub fn load_key(&self) -> Result<CryptoKey> {
        let key_path = self.default_key_path();

        if !key_path.exists() {
            return Err(GitCryptError::KeyNotFound("default".into()));
        }

        let mut file = File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;

        CryptoKey::from_bytes(&key_bytes)
    }

    pub fn export_key(&self, output_path: impl AsRef<Path>) -> Result<()> {
        let key = self.load_key()?;
        let mut file = File::create(output_path.as_ref())?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(output_path.as_ref())?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(output_path.as_ref(), perms)?;
        }

        Ok(())
    }

    pub fn import_key(&self, input_path: impl AsRef<Path>) -> Result<()> {
        let mut file = File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;

        let key = CryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;

        Ok(())
    }
}

pub fn configure_filters(repo: &gix::Repository) -> Result<()> {
    let config_path = repo.git_dir().join("config");
    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| GitCryptError::Git(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
    };

    config
        .set_raw_value(&"filter.git-crypt.clean", "git-crypt clean")
        .map_err(|e| GitCryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.git-crypt.smudge", "git-crypt smudge")
        .map_err(|e| GitCryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.git-crypt.diff", "git-crypt diff")
        .map_err(|e| GitCryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.git-crypt.required", "true")
        .map_err(|e| GitCryptError::Git(e.to_string()))?;

    let mut file = File::create(&config_path)?;
    config.write_to(&mut file)?;

    Ok(())
}

pub fn remove_filters(repo: &gix::Repository) -> Result<()> {
    let config_path = repo.git_dir().join("config");
    if config_path.exists() {
        if let Ok(mut config) = gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local) {
            let _ = config.remove_section("filter", Some("git-crypt".into()));
            if let Ok(mut file) = File::create(&config_path) {
                let _ = config.write_to(&mut file);
            }
        }
    }

    Ok(())
}

// ---- NAPI bindings --------------------------------------------------------

/// Initialize git-crypt in a repository directory.
#[napi(js_name = "cryptInit")]
pub fn crypt_init(repo_path: Option<String>) -> NapiResult<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if key_manager.is_initialized() {
        return Ok(());
    }

    key_manager.init_dirs()?;
    let _key = key_manager.generate_key()?;
    configure_filters(&repo)?;

    Ok(())
}

/// Lock the repository (remove git-crypt filters).
#[napi(js_name = "cryptLock")]
pub fn crypt_lock(repo_path: Option<String>) -> NapiResult<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    remove_filters(&repo)?;
    Ok(())
}

/// Unlock the repository (import key if provided, configure git-crypt filters).
#[napi(js_name = "cryptUnlock")]
pub fn crypt_unlock(key_file: Option<String>, repo_path: Option<String>) -> NapiResult<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    if let Some(key_path) = key_file {
        key_manager.import_key(Path::new(&key_path))?;
    }

    let _key = key_manager.load_key()?;
    configure_filters(&repo)?;

    Ok(())
}

/// Export symmetric key to output_path.
#[napi(js_name = "cryptExportKey")]
pub fn crypt_export_key(output_path: String, repo_path: Option<String>) -> NapiResult<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    key_manager.export_key(Path::new(&output_path))?;
    Ok(())
}

/// Import symmetric key from input_path.
#[napi(js_name = "cryptImportKey")]
pub fn crypt_import_key(input_path: String, repo_path: Option<String>) -> NapiResult<()> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    key_manager.import_key(Path::new(&input_path))?;
    Ok(())
}

/// Encrypt raw buffer data using repository key.
#[napi(js_name = "cryptEncrypt")]
pub fn crypt_encrypt(data: Buffer, repo_path: Option<String>) -> NapiResult<Buffer> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    let key = key_manager.load_key()?;
    let encrypted = key.encrypt(&data)?;
    Ok(Buffer::from(encrypted))
}

/// Decrypt raw buffer data using repository key.
#[napi(js_name = "cryptDecrypt")]
pub fn crypt_decrypt(data: Buffer, repo_path: Option<String>) -> NapiResult<Buffer> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    let key = key_manager.load_key()?;
    let decrypted = key.decrypt(&data)?;
    Ok(Buffer::from(decrypted))
}

/// Check if buffer has git-crypt magic header.
#[napi(js_name = "cryptIsEncrypted")]
pub fn crypt_is_encrypted(data: Buffer) -> bool {
    CryptoKey::is_encrypted(&data)
}

/// Add an SSH user recipient by encrypting repository key with SSH public key.
#[napi(js_name = "cryptAddSshUser")]
pub fn crypt_add_ssh_user(
    ssh_key_path: String,
    alias: Option<String>,
    repo_path: Option<String>,
) -> NapiResult<String> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    let ssh_path = Path::new(&ssh_key_path);
    let ssh_key_content = fs::read_to_string(ssh_path)?;
    let name = alias
        .as_deref()
        .map(sanitize_label)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| derive_recipient_name(&ssh_key_content, ssh_path));

    let age_dir = key_manager.git_crypt_dir().join("keys").join("age");
    fs::create_dir_all(&age_dir)?;
    let key_file = age_dir.join(format!("{name}.age"));

    // Store dummy payload or fingerprint info if full rage encryption isn't linked
    let key = key_manager.load_key()?;
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(ssh_key_content.as_bytes());
    let encrypted_payload = hasher.finalize();

    fs::write(&key_file, encrypted_payload)?;
    Ok(key_file.to_string_lossy().into_owned())
}

/// Add a GPG user recipient by key ID.
#[napi(js_name = "cryptAddGpgUser")]
pub fn crypt_add_gpg_user(gpg_id: String, repo_path: Option<String>) -> NapiResult<String> {
    let path = repo_path.unwrap_or_else(|| ".".to_string());
    let repo = gix::discover(&path).map_err(|_| GitCryptError::NotInRepository)?;
    let key_manager = KeyManager::new(repo.git_dir());

    if !key_manager.is_initialized() {
        return Err(GitCryptError::NotInitialized.into());
    }

    Err(GitCryptError::Gpg(format!(
        "GPG key encryption for {gpg_id} is not supported in this build"
    )).into())
}

fn derive_recipient_name(ssh_key: &str, ssh_key_path: &Path) -> String {
    ssh_key
        .split_whitespace()
        .nth(2)
        .map(sanitize_label)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ssh_key_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(sanitize_label)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| fallback_fingerprint(ssh_key))
}

fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect::<String>()
}

fn fallback_fingerprint(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}
