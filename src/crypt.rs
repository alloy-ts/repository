use std::fs::{self, File};
use std::io::{self, Read, Write};
#[cfg(feature = "ssh")]
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::error::thread_local;

// ============================================================================
// Error Types
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

impl From<CryptError> for napi::Error {
    fn from(err: CryptError) -> Self {
        napi::Error::from_reason(err.to_string())
    }
}

// ============================================================================
// Cryptographic Key Handling (AES-256-GCM)
// ============================================================================

pub const KEY_SIZE: usize = 32; // 256 bits
pub const NONCE_SIZE: usize = 12; // 96 bits for GCM

const MAGIC_HEADER: &[u8] = b"GITCRYPT";

#[derive(Clone)]
pub struct CryptoKey {
    key: [u8; KEY_SIZE],
}

impl CryptoKey {
    /// Generate a new random 256-bit key
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        rand::fill(&mut key);
        Self { key }
    }

    /// Create a key from raw bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptError> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptError::InvalidKeyFormat);
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Return raw key bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Encrypt plaintext into MAGIC_HEADER + nonce + ciphertext
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| CryptError::Crypto(e.to_string()))?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::fill(&mut nonce_bytes);
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

    /// Decrypt ciphertext (expects MAGIC_HEADER + nonce + payload)
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptError> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(CryptError::Crypto("Ciphertext too short".into()));
        }

        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(CryptError::Crypto("Invalid encrypted data format".into()));
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

    /// Check if data starts with the GITCRYPT magic header
    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

// ============================================================================
// Key Storage & Lifecycle Management
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

    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("crypt")
    }

    pub fn default_key_path(&self) -> PathBuf {
        self.git_crypt_dir().join("keys").join("default")
    }

    pub fn init_dirs(&self) -> Result<(), CryptError> {
        let git_crypt_dir = self.git_crypt_dir();
        if git_crypt_dir.exists() {
            return Err(CryptError::AlreadyInitialized);
        }

        fs::create_dir_all(git_crypt_dir.join("keys"))?;
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    pub fn generate_key(&self) -> Result<CryptoKey, CryptError> {
        let key = CryptoKey::generate();
        self.save_key(&key)?;
        Ok(key)
    }

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
// Git Filter Config Wiring
// ============================================================================

fn configure_filters(git_dir: &Path) -> Result<(), CryptError> {
    let config_path = git_dir.join("config");
    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptError::Git(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(
            gix::config::Source::Local,
        ))
    };

    config
        .set_raw_value(&"filter.crypt.clean", "git-crypt clean")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.crypt.smudge", "git-crypt smudge")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.crypt.diff", "git-crypt diff")
        .map_err(|e| CryptError::Git(e.to_string()))?;
    config
        .set_raw_value(&"filter.crypt.required", "true")
        .map_err(|e| CryptError::Git(e.to_string()))?;

    let mut file = File::create(&config_path)?;
    config
        .write_to(&mut file)
        .map_err(|e| CryptError::Git(e.to_string()))?;

    Ok(())
}

fn remove_filters(git_dir: &Path) -> Result<(), CryptError> {
    let config_path = git_dir.join("config");
    if !config_path.exists() {
        return Ok(());
    }

    let mut config = gix::config::File::from_path_no_includes(
        config_path.clone(),
        gix::config::Source::Local,
    )
    .map_err(|e| CryptError::Git(e.to_string()))?;

    let _ = config.remove_section("filter", Some("crypt".into()));

    let mut file = File::create(&config_path)?;
    config
        .write_to(&mut file)
        .map_err(|e| CryptError::Git(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Internal Clean / Smudge / Diff Filter Drivers
// ============================================================================

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
pub fn diff_filter() -> Result<(), CryptError> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    if CryptoKey::is_encrypted(&input) {
        writeln!(io::stdout(), "*** This file is encrypted with git-crypt ***")?;
    } else {
        io::stdout().write_all(&input)?;
    }

    Ok(())
}

// ============================================================================
// Recipient Name Helpers
// ============================================================================

#[allow(dead_code)]
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

#[allow(dead_code)]
pub fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect::<String>()
}

#[allow(dead_code)]
pub fn fallback_fingerprint(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}

// ============================================================================
// SSH / Age Recipient Manager (feature = "ssh")
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
            .map_err(|e| CryptError::Age(e.to_string()))?;
        writer.write_all(key.as_bytes())?;
        writer
            .finish()
            .map_err(|e| CryptError::Age(e.to_string()))?;

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

        let identity_with_cb = identity.with_callbacks(PromptCallbacks::new(identity_label));

        let mut reader = decryptor
            .decrypt(std::iter::once(&identity_with_cb as &dyn age::Identity))
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
// GPG Recipient Manager (feature = "gpg")
// ============================================================================

#[allow(dead_code)]
pub struct GpgManager;

impl GpgManager {
    #[allow(dead_code)]
    #[cfg(feature = "gpg")]
    pub fn encrypt_key_for_recipient(
        _key: &CryptoKey,
        recipient_fingerprint: &str,
    ) -> Result<Vec<u8>, CryptError> {
        // Keyring lookup stub
        Err(CryptError::Gpg(format!(
            "Certificate lookup not yet implemented for: {recipient_fingerprint}"
        )))
    }

    #[allow(dead_code)]
    #[cfg(not(feature = "gpg"))]
    pub fn encrypt_key_for_recipient(
        _key: &CryptoKey,
        _recipient_fingerprint: &str,
    ) -> Result<Vec<u8>, CryptError> {
        Err(CryptError::Gpg(
            "GPG support not enabled. Rebuild with --features gpg".into(),
        ))
    }
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
    #[napi]
    pub fn is_initialized(&self) -> bool {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        km.is_initialized()
    }

    #[napi]
    pub fn init(&self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if km.is_initialized() {
            return Err(CryptError::AlreadyInitialized.into());
        }
        km.init_dirs()?;
        km.generate_key()?;
        configure_filters(r.git_dir())?;
        Ok(())
    }

    #[napi]
    pub fn lock(&self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        remove_filters(r.git_dir())?;
        Ok(())
    }

    #[napi]
    pub fn unlock(&self, key_file: Option<String>) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        if let Some(key_path) = key_file {
            km.import_key(Path::new(&key_path))?;
        }
        let _key = km.load_key()?;
        configure_filters(r.git_dir())?;
        Ok(())
    }

    #[napi]
    pub fn export_key(&self, output_path: String) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    #[napi]
    pub fn import_key(&self, input_path: String) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    #[napi]
    pub fn encrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        let key = km.load_key()?;
        let encrypted = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(encrypted))
    }

    #[napi]
    pub fn decrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let r = thread_local(&self.repo);
        let km = KeyManager::new(r.git_dir());
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        let key = km.load_key()?;
        let decrypted = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(decrypted))
    }

    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        CryptoKey::is_encrypted(data.as_ref())
    }

    #[napi]
    pub fn add_ssh_user(&self, ssh_key_path: String, alias: Option<String>) -> napi::Result<()> {
        #[cfg(feature = "ssh")]
        {
            let r = thread_local(&self.repo);
            let km = KeyManager::new(r.git_dir());
            if !km.is_initialized() {
                return Err(CryptError::NotInitialized.into());
            }
            let key = km.load_key()?;
            let ssh_key_path_buf = PathBuf::from(&ssh_key_path);
            let ssh_key = fs::read_to_string(&ssh_key_path_buf)?;
            let encrypted_key = RageManager::encrypt_key_for_ssh_recipient(&key, &ssh_key)?;

            let name = alias
                .as_deref()
                .map(sanitize_label)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| derive_recipient_name(&ssh_key, &ssh_key_path_buf));

            let age_dir = km.git_crypt_dir().join("keys").join("age");
            fs::create_dir_all(&age_dir)?;
            let key_file = age_dir.join(format!("{name}.age"));
            fs::write(&key_file, encrypted_key)?;
            Ok(())
        }
        #[cfg(not(feature = "ssh"))]
        {
            let _ = (ssh_key_path, alias);
            Err(CryptError::Other("SSH support not enabled. Rebuild with --features ssh".into()).into())
        }
    }

    #[napi]
    pub fn import_age_key(&self, encrypted_path: String, identity_path: String) -> napi::Result<()> {
        #[cfg(feature = "ssh")]
        {
            let r = thread_local(&self.repo);
            let km = KeyManager::new(r.git_dir());
            if !km.is_initialized() {
                return Err(CryptError::NotInitialized.into());
            }
            let encrypted_p = Path::new(&encrypted_path);
            let identity_p = Path::new(&identity_path);

            let encrypted = fs::read(encrypted_p)?;
            let identity = fs::read_to_string(identity_p)?;
            let identity_label = identity_p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("ssh identity");

            let key = RageManager::decrypt_key_with_ssh_identity(&encrypted, &identity, identity_label)?;
            km.save_key(&key)?;
            Ok(())
        }
        #[cfg(not(feature = "ssh"))]
        {
            let _ = (encrypted_path, identity_path);
            Err(CryptError::Other("SSH support not enabled. Rebuild with --features ssh".into()).into())
        }
    }

    #[napi]
    pub fn add_gpg_user(&self, gpg_id: String) -> napi::Result<()> {
        #[cfg(feature = "gpg")]
        {
            let r = thread_local(&self.repo);
            let km = KeyManager::new(r.git_dir());
            if !km.is_initialized() {
                return Err(CryptError::NotInitialized.into());
            }
            let key = km.load_key()?;
            let encrypted_key = GpgManager::encrypt_key_for_recipient(&key, &gpg_id)?;
            let gpg_keys_dir = km.git_crypt_dir().join("keys").join("gpg");
            fs::create_dir_all(&gpg_keys_dir)?;
            let key_file = gpg_keys_dir.join(format!("{}.key", gpg_id));
            fs::write(&key_file, encrypted_key)?;
            Ok(())
        }
        #[cfg(not(feature = "gpg"))]
        {
            let _ = gpg_id;
            Err(CryptError::Gpg("GPG support not enabled. Rebuild with --features gpg".into()).into())
        }
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

    #[test]
    fn test_crypto_encrypt_decrypt() {
        let key = CryptoKey::generate();
        let plaintext = b"Hello, World!";

        let ciphertext = key.encrypt(plaintext).unwrap();
        assert_ne!(plaintext.as_slice(), &ciphertext[..]);
        assert!(CryptoKey::is_encrypted(&ciphertext));

        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn test_crypto_empty_data() {
        let key = CryptoKey::generate();
        let plaintext = b"";

        let ciphertext = key.encrypt(plaintext).unwrap();
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn test_crypto_wrong_key_fails() {
        let key1 = CryptoKey::generate();
        let key2 = CryptoKey::generate();
        let plaintext = b"Secret message";

        let ciphertext = key1.encrypt(plaintext).unwrap();
        let result = key2.decrypt(&ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_key_manager_lifecycle() {
        let temp = create_test_git_dir();
        let km = KeyManager::new(temp.path());

        assert!(!km.is_initialized());
        km.init_dirs().unwrap();
        assert!(km.is_initialized());

        let key1 = km.generate_key().unwrap();
        let key2 = km.load_key().unwrap();
        assert_eq!(key1.as_bytes(), key2.as_bytes());

        let export_path = temp.path().join("exported.key");
        km.export_key(&export_path).unwrap();
        assert!(export_path.exists());

        let temp2 = create_test_git_dir();
        let km2 = KeyManager::new(temp2.path());
        km2.init_dirs().unwrap();
        km2.import_key(&export_path).unwrap();

        let imported_key = km2.load_key().unwrap();
        assert_eq!(key1.as_bytes(), imported_key.as_bytes());
    }

    #[test]
    fn test_recipient_label_functions() {
        assert_eq!(sanitize_label("alice@example.com"), "aliceexample.com");
        assert_eq!(sanitize_label("Team-Name_01"), "Team-Name_01");

        let pubkey = "ssh-ed25519 AAAAC3 comment@example.com";
        let name = derive_recipient_name(pubkey, Path::new("id_ed25519.pub"));
        assert_eq!(name, "commentexample.com");

        let fp = fallback_fingerprint("test-data");
        assert!(fp.starts_with("ssh-"));
    }
}
