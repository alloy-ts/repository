use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "ssh")]
use std::io::Cursor;

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use napi::bindgen_prelude::Buffer;
use napi::Error as NapiError;
use napi::Result as NapiResult;
use napi_derive::napi;
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::error::thread_local;

// ============================================================================
// CryptError
// ============================================================================

#[derive(Error, Debug)]
pub enum CryptError {
    #[error("Git error: {0}")]
    Git(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Cryptography error: {0}")]
    Crypto(String),

    #[error("GPG error: {0}")]
    Gpg(String),

    #[cfg(feature = "ssh")]
    #[error("age/rage error: {0}")]
    Age(String),

    #[error("Repository not initialized. Run 'init' first")]
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

impl From<CryptError> for NapiError {
    fn from(err: CryptError) -> Self {
        NapiError::from_reason(err.to_string())
    }
}

// ============================================================================
// CryptoKey
// ============================================================================

pub const KEY_SIZE: usize = 32; // 256 bits
pub const NONCE_SIZE: usize = 12; // 96 bits for GCM
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

#[derive(Clone)]
pub struct CryptoKey {
    key: [u8; KEY_SIZE],
}

impl CryptoKey {
    /// Generate a new random key
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    /// Create a key from existing bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptError> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptError::InvalidKeyFormat);
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Get the key as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Encrypt plaintext
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| CryptError::Crypto(e.to_string()))?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| CryptError::Crypto(e.to_string()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    /// Decrypt ciphertext
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptError> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(CryptError::Crypto("Ciphertext too short".into()));
        }

        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(CryptError::Crypto(
                "Invalid encrypted data format".into(),
            ));
        }

        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| CryptError::Crypto(e.to_string()))?;

        let data = &ciphertext[MAGIC_HEADER.len()..];
        let (nonce_bytes, encrypted_data) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, encrypted_data)
            .map_err(|e| CryptError::Crypto(e.to_string()))?;

        Ok(plaintext)
    }

    /// Check if data starts with the magic header
    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

// ============================================================================
// KeyManager
// ============================================================================

pub struct KeyManager {
    git_dir: PathBuf,
}

impl KeyManager {
    pub fn new(git_dir: impl AsRef<Path>) -> Self {
        Self {
            git_dir: git_dir.as_ref().to_path_buf(),
        }
    }

    /// Get the path to the crypt directory inside `.git`
    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("crypt")
    }

    /// Get the path to the default key file
    pub fn default_key_path(&self) -> PathBuf {
        self.git_crypt_dir().join("keys").join("default")
    }

    /// Initialize the crypt directory structure
    pub fn init_dirs(&self) -> Result<(), CryptError> {
        let git_crypt_dir = self.git_crypt_dir();
        if git_crypt_dir.exists() {
            return Err(CryptError::AlreadyInitialized);
        }

        fs::create_dir_all(&git_crypt_dir)?;
        fs::create_dir(git_crypt_dir.join("keys"))?;

        Ok(())
    }

    /// Check if repository is initialized
    pub fn is_initialized(&self) -> bool {
        self.default_key_path().exists() || self.git_crypt_dir().exists()
    }

    /// Generate and save a new key
    pub fn generate_key(&self) -> Result<CryptoKey, CryptError> {
        let key = CryptoKey::generate();
        self.save_key(&key)?;
        Ok(key)
    }

    /// Save a key to disk with 0600 permissions
    pub fn save_key(&self, key: &CryptoKey) -> Result<(), CryptError> {
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

    /// Load the default key from disk
    pub fn load_key(&self) -> Result<CryptoKey, CryptError> {
        let key_path = self.default_key_path();

        if !key_path.exists() {
            return Err(CryptError::KeyNotFound("default".into()));
        }

        let mut file = File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;

        CryptoKey::from_bytes(&key_bytes)
    }

    /// Export key to a file
    pub fn export_key(&self, output_path: impl AsRef<Path>) -> Result<(), CryptError> {
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

    /// Import key from a file
    pub fn import_key(&self, input_path: impl AsRef<Path>) -> Result<(), CryptError> {
        let mut file = File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;

        let key = CryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;

        Ok(())
    }
}

// ============================================================================
// Recipient Helpers
// ============================================================================

pub fn derive_recipient_name(ssh_key: &str, ssh_key_path: &Path) -> String {
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

pub fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect::<String>()
}

pub fn fallback_fingerprint(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}

// ============================================================================
// RageManager (SSH / age)
// ============================================================================

#[cfg(feature = "ssh")]
pub struct RageManager;

#[cfg(feature = "ssh")]
impl RageManager {
    pub fn encrypt_key_for_ssh_recipient(
        key: &CryptoKey,
        recipient: &str,
    ) -> Result<Vec<u8>, CryptError> {
        use age::{ssh::Recipient as SshRecipient, Encryptor};

        let recipient: SshRecipient = recipient
            .trim()
            .parse()
            .map_err(|e| CryptError::Age(format!("Invalid SSH recipient: {e:?}")))?;

        let encryptor = Encryptor::with_recipients(std::iter::once(&recipient as _))
            .map_err(|e| CryptError::Age(format!("age encryption failed: {e}")))?;

        let mut ciphertext = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut ciphertext)
            .map_err(|e| CryptError::Age(format!("age output wrap failed: {e}")))?;
        writer.write_all(key.as_bytes())?;
        writer
            .finish()
            .map_err(|e| CryptError::Age(format!("age finish failed: {e}")))?;

        Ok(ciphertext)
    }

    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        identity_label: &str,
    ) -> Result<CryptoKey, CryptError> {
        use age::{ssh::Identity as SshIdentity, Decryptor};

        let cursor = Cursor::new(identity_content.as_bytes());
        let identity = SshIdentity::from_buffer(cursor, Some(identity_label.to_string()))
            .map_err(|e| CryptError::Age(format!("Invalid SSH identity: {e}")))?;

        let decryptor = Decryptor::new_buffered(Cursor::new(encrypted))
            .map_err(|e| CryptError::Age(format!("age decryption failed: {e}")))?;
        let identity = identity.with_callbacks(PromptCallbacks::new(identity_label));

        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|e| CryptError::Age(format!("age decryption failed: {e}")))?;
        let mut plaintext = Vec::new();
        reader.read_to_end(&mut plaintext)?;

        CryptoKey::from_bytes(&plaintext)
    }
}

#[cfg(feature = "ssh")]
#[derive(Clone)]
struct PromptCallbacks {
    identity_label: String,
}

#[cfg(feature = "ssh")]
impl PromptCallbacks {
    fn new(identity_label: &str) -> Self {
        Self {
            identity_label: identity_label.to_string(),
        }
    }
}

#[cfg(feature = "ssh")]
impl age::Callbacks for PromptCallbacks {
    fn display_message(&self, message: &str) {
        eprintln!("{message}");
    }

    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }

    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }

    fn request_passphrase(&self, description: &str) -> Option<age::secrecy::SecretString> {
        let prompt = if description.is_empty() {
            format!("Passphrase for {}", self.identity_label)
        } else {
            description.to_string()
        };

        match rpassword::prompt_password(format!("{prompt}: ")) {
            Ok(passphrase) => Some(age::secrecy::SecretString::new(passphrase.into())),
            Err(err) => {
                eprintln!("Failed to read passphrase: {err}");
                None
            }
        }
    }
}

// ============================================================================
// GpgManager
// ============================================================================

pub struct GpgManager;

impl GpgManager {
    pub fn encrypt_key_for_recipient(
        _key: &CryptoKey,
        _recipient_fingerprint: &str,
    ) -> Result<Vec<u8>, CryptError> {
        Err(CryptError::Gpg(
            "GPG keyring lookup is not implemented".into(),
        ))
    }
}

// ============================================================================
// Internal Filter Helpers
// ============================================================================

pub fn clean_filter(key: &CryptoKey) -> Result<(), CryptError> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    if CryptoKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }

    let encrypted = key.encrypt(&input)?;
    io::stdout().write_all(&encrypted)?;
    Ok(())
}

pub fn smudge_filter(key: &CryptoKey) -> Result<(), CryptError> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    if !CryptoKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }

    let decrypted = key.decrypt(&input)?;
    io::stdout().write_all(&decrypted)?;
    Ok(())
}

pub fn diff_filter() -> Result<(), CryptError> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    if CryptoKey::is_encrypted(&input) {
        writeln!(
            io::stdout(),
            "*** This file is encrypted with git-crypt ***"
        )?;
    } else {
        io::stdout().write_all(&input)?;
    }

    Ok(())
}

fn configure_git_filters(git_dir: &Path) -> Result<(), CryptError> {
    let config_path = git_dir.join("config");
    let mut file = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptError::Git(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(
            gix::config::Source::Local,
        ))
    };

    file.set_raw_value(&"filter.crypt.clean", "git-crypt clean")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    file.set_raw_value(&"filter.crypt.smudge", "git-crypt smudge")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    file.set_raw_value(&"filter.crypt.diff", "git-crypt diff")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    file.set_raw_value(&"filter.crypt.required", "true")
        .map_err(|e| CryptError::Git(e.to_string()))?;

    let mut out_file = File::create(&config_path)?;
    file.write_to(&mut out_file)?;
    Ok(())
}

fn remove_git_filters(git_dir: &Path) -> Result<(), CryptError> {
    let config_path = git_dir.join("config");
    if !config_path.exists() {
        return Ok(());
    }

    let mut file =
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptError::Git(e.to_string()))?;

    let _ = file.remove_section("filter", Some("crypt".into()));

    let mut out_file = File::create(&config_path)?;
    file.write_to(&mut out_file)?;
    Ok(())
}

// ============================================================================
// GitCrypt NAPI Class
// ============================================================================

#[napi]
pub struct GitCrypt {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl GitCrypt {
    /// Check whether git-crypt is initialized in the repository
    #[napi]
    pub fn is_initialized(&self) -> bool {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());
        key_manager.is_initialized()
    }

    /// Initialize git-crypt key and filter configuration
    #[napi]
    pub fn init(&self) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if key_manager.is_initialized() {
            return Err(CryptError::AlreadyInitialized.into());
        }

        key_manager.init_dirs()?;
        key_manager.generate_key()?;
        configure_git_filters(r.git_dir())?;

        Ok(())
    }

    /// Lock the repository by removing filter configuration
    #[napi]
    pub fn lock(&self) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if !key_manager.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        remove_git_filters(r.git_dir())?;
        Ok(())
    }

    /// Unlock the repository, optionally importing a key file first
    #[napi]
    pub fn unlock(&self, key_file: Option<String>) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if !key_manager.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        if let Some(key_path) = key_file {
            key_manager.import_key(&key_path)?;
        }

        let _key = key_manager.load_key()?;
        configure_git_filters(r.git_dir())?;

        Ok(())
    }

    /// Export the symmetric key to a file
    #[napi]
    pub fn export_key(&self, output_path: String) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if !key_manager.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        key_manager.export_key(&output_path)?;
        Ok(())
    }

    /// Import a symmetric key from a file
    #[napi]
    pub fn import_key(&self, input_path: String) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if !key_manager.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        key_manager.import_key(&input_path)?;
        Ok(())
    }

    /// Encrypt raw buffer data using the repository key
    #[napi]
    pub fn encrypt(&self, data: Buffer) -> NapiResult<Buffer> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        let key = key_manager.load_key()?;
        let ciphertext = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(ciphertext))
    }

    /// Decrypt raw buffer data using the repository key
    #[napi]
    pub fn decrypt(&self, data: Buffer) -> NapiResult<Buffer> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        let key = key_manager.load_key()?;
        let plaintext = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(plaintext))
    }

    /// Check if buffer starts with the git-crypt magic header
    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        CryptoKey::is_encrypted(data.as_ref())
    }

    /// Add an SSH user recipient (requires `ssh` feature)
    #[napi]
    pub fn add_ssh_user(
        &self,
        ssh_key_path: String,
        alias: Option<String>,
    ) -> NapiResult<()> {
        #[cfg(feature = "ssh")]
        {
            let r = thread_local(&self.repo);
            let key_manager = KeyManager::new(r.git_dir());

            if !key_manager.is_initialized() {
                return Err(CryptError::NotInitialized.into());
            }

            let key = key_manager.load_key()?;
            let ssh_key_p = Path::new(&ssh_key_path);
            let ssh_key = fs::read_to_string(ssh_key_p)?;
            let encrypted_key = RageManager::encrypt_key_for_ssh_recipient(&key, &ssh_key)?;

            let name = alias
                .as_deref()
                .map(sanitize_label)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| derive_recipient_name(&ssh_key, ssh_key_p));

            let age_dir = key_manager.git_crypt_dir().join("keys").join("age");
            fs::create_dir_all(&age_dir)?;
            let key_file = age_dir.join(format!("{name}.age"));
            fs::write(&key_file, encrypted_key)?;

            Ok(())
        }
        #[cfg(not(feature = "ssh"))]
        {
            let _ = (ssh_key_path, alias);
            Err(CryptError::Other("SSH/age feature not enabled in build".into()).into())
        }
    }

    /// Import an age-encrypted key using an SSH identity (requires `ssh` feature)
    #[napi]
    pub fn import_age_key(
        &self,
        encrypted_path: String,
        identity_path: String,
    ) -> NapiResult<()> {
        #[cfg(feature = "ssh")]
        {
            let r = thread_local(&self.repo);
            let key_manager = KeyManager::new(r.git_dir());

            if !key_manager.is_initialized() {
                return Err(CryptError::NotInitialized.into());
            }

            let encrypted = fs::read(&encrypted_path)?;
            let identity = fs::read_to_string(&identity_path)?;
            let identity_p = Path::new(&identity_path);
            let identity_label = identity_p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("ssh identity");

            let key = RageManager::decrypt_key_with_ssh_identity(&encrypted, &identity, identity_label)?;
            key_manager.save_key(&key)?;

            Ok(())
        }
        #[cfg(not(feature = "ssh"))]
        {
            let _ = (encrypted_path, identity_path);
            Err(CryptError::Other("SSH/age feature not enabled in build".into()).into())
        }
    }

    /// Add a GPG user recipient (stub / returns actionable error)
    #[napi]
    pub fn add_gpg_user(&self, gpg_id: String) -> NapiResult<()> {
        let r = thread_local(&self.repo);
        let key_manager = KeyManager::new(r.git_dir());

        if !key_manager.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        let key = key_manager.load_key()?;
        let _encrypted_key = GpgManager::encrypt_key_for_recipient(&key, &gpg_id)?;
        Ok(())
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_git_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    // --- CryptoKey Tests ---

    #[test]
    fn test_crypto_key_encrypt_decrypt() {
        let key = CryptoKey::generate();
        let plaintext = b"Hello, git-crypt!";

        let ciphertext = key.encrypt(plaintext).unwrap();
        assert_ne!(plaintext.as_slice(), &ciphertext[..]);
        assert!(CryptoKey::is_encrypted(&ciphertext));

        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn test_crypto_key_empty_and_large_data() {
        let key = CryptoKey::generate();

        let empty = b"";
        let ciphertext = key.encrypt(empty).unwrap();
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(empty.as_slice(), &decrypted[..]);

        let large = vec![0x55u8; 100 * 1024];
        let ciphertext = key.encrypt(&large).unwrap();
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(large, decrypted);
    }

    #[test]
    fn test_wrong_key_decryption_fails() {
        let key1 = CryptoKey::generate();
        let key2 = CryptoKey::generate();
        let plaintext = b"Top secret";

        let ciphertext = key1.encrypt(plaintext).unwrap();
        assert!(key2.decrypt(&ciphertext).is_err());
    }

    #[test]
    fn test_corrupted_ciphertext_fails() {
        let key = CryptoKey::generate();
        let plaintext = b"Top secret";

        let mut ciphertext = key.encrypt(plaintext).unwrap();
        let len = ciphertext.len();
        ciphertext[len - 1] ^= 0xFF;

        assert!(key.decrypt(&ciphertext).is_err());
    }

    // --- KeyManager Tests ---

    #[test]
    fn test_key_manager_paths() {
        let temp = create_test_git_dir();
        let km = KeyManager::new(temp.path());

        assert_eq!(km.git_crypt_dir(), temp.path().join("crypt"));
        assert_eq!(
            km.default_key_path(),
            temp.path().join("crypt").join("keys").join("default")
        );
    }

    #[test]
    fn test_key_manager_lifecycle() {
        let temp = create_test_git_dir();
        let km = KeyManager::new(temp.path());

        assert!(!km.is_initialized());
        km.init_dirs().unwrap();
        assert!(km.is_initialized());

        let key = km.generate_key().unwrap();
        let loaded = km.load_key().unwrap();
        assert_eq!(key.as_bytes(), loaded.as_bytes());

        let export_path = temp.path().join("exported.key");
        km.export_key(&export_path).unwrap();
        assert!(export_path.exists());

        let temp2 = create_test_git_dir();
        let km2 = KeyManager::new(temp2.path());
        km2.init_dirs().unwrap();
        km2.import_key(&export_path).unwrap();

        let imported = km2.load_key().unwrap();
        assert_eq!(key.as_bytes(), imported.as_bytes());
    }

    #[test]
    fn test_key_permissions_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let temp = create_test_git_dir();
            let km = KeyManager::new(temp.path());
            km.init_dirs().unwrap();
            km.generate_key().unwrap();

            let metadata = fs::metadata(km.default_key_path()).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    // --- Recipient Helper Tests ---

    #[test]
    fn test_sanitize_label() {
        assert_eq!(sanitize_label("alice@example.com"), "aliceexample.com");
        assert_eq!(sanitize_label("Team-Name_01"), "Team-Name_01");
    }

    #[test]
    fn test_derive_recipient_name() {
        let pubkey = "ssh-ed25519 AAAAC3 comment@example.com";
        let name = derive_recipient_name(pubkey, Path::new("id_ed25519.pub"));
        assert_eq!(name, "commentexample.com");

        let pubkey2 = "ssh-ed25519 AAAAC3";
        let name2 = derive_recipient_name(pubkey2, Path::new("keys/my-key.pub"));
        assert_eq!(name2, "my-key");
    }
}
