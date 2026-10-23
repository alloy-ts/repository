use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use rand::RngCore;
use crate::crypto::{CryptoError, CryptoResult, SymmetricCryptoKey, KEY_SIZE, NONCE_SIZE};

// ---------------------------------------------------------------------------
// KeyManager (on-disk key storage under <git-dir>/crypt)
// ---------------------------------------------------------------------------

/// Stores and manages the repository's crypt key on disk.
pub struct KeyManager {
    git_dir: PathBuf,
}

impl KeyManager {
    /// Create a manager rooted at `git_dir` (the `.git` directory).
    pub fn new(git_dir: impl AsRef<Path>) -> Self {
        Self {
            git_dir: git_dir.as_ref().to_path_buf(),
        }
    }

    /// Path to the `<git-dir>/crypt` directory.
    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("crypt")
    }

    /// Path to the default symmetric key file.
    ///
    /// Honors `gpg.ssh.defaultKeyPath` from git config when set; otherwise
    /// falls back to `<git-dir>/crypt/keys/default`.
    pub fn default_key_path(&self) -> PathBuf {
        if let Some(configured) = self.config_default_key_path() {
            return configured;
        }
        self.git_crypt_dir().join("keys").join("default")
    }

    /// Read `gpg.ssh.defaultKeyPath` from git config, resolving a relative
    /// value against the git directory. Returns `None` when the config is
    /// unset or unreadable.
    fn config_default_key_path(&self) -> Option<PathBuf> {
        let repo = gix::open(&self.git_dir).ok()?;
        let raw = repo.config_snapshot().string("gpg.ssh.defaultKeyPath")?;
        let raw = String::from_utf8_lossy(&raw);
        if raw.is_empty() {
            return None;
        }
        let path = PathBuf::from(raw.into_owned());
        Some(if path.is_absolute() {
            path
        } else {
            self.git_dir.join(path)
        })
    }

    /// Create the `crypt` + `crypt/keys` directories. Fails if already initialized.
    pub fn init_dirs(&self) -> CryptoResult<()> {
        let crypt_dir = self.git_crypt_dir();
        if crypt_dir.exists() {
            return Err(CryptoError::AlreadyInitialized);
        }
        std::fs::create_dir_all(&crypt_dir)?;
        std::fs::create_dir(crypt_dir.join("keys"))?;
        Ok(())
    }

    /// Whether the repository has been initialized for crypt.
    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    /// Generate a new random key and persist it.
    pub fn generate_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let key = SymmetricCryptoKey::generate_random();
        self.save_key(&key)?;
        Ok(key)
    }

    /// Persist `key` to the default path with `0600` permissions on Unix.
    pub fn save_key(&self, key: &SymmetricCryptoKey) -> CryptoResult<()> {
        let key_path = self.default_key_path();
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::File::create(&key_path)?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&key_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&key_path, perms)?;
        }

        Ok(())
    }

    /// Load the default key from disk.
    pub fn load_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(CryptoError::KeyNotFound("default".into()));
        }
        let mut file = std::fs::File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        SymmetricCryptoKey::from_bytes(&key_bytes)
    }

    /// Export the default key to `output_path` (with `0600` on Unix).
    pub fn export_key(&self, output_path: &Path) -> CryptoResult<()> {
        let key = self.load_key()?;
        let mut file = std::fs::File::create(output_path)?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(output_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(output_path, perms)?;
        }

        Ok(())
    }

    /// Import a key from `input_path` and save it as the default key.
    pub fn import_key(&self, input_path: &Path) -> CryptoResult<()> {
        let mut file = std::fs::File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = SymmetricCryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IdentityManager & SSH Recipient Helpers
// ---------------------------------------------------------------------------

/// Derive a filesystem-safe recipient name from an SSH public key, preferring the comment
/// field, then the file stem, then a truncated SHA-256 fingerprint.
#[cfg(feature = "ssh")]
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

/// Keep only filesystem-safe characters.
#[cfg(feature = "ssh")]
pub fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Deterministic `ssh-<hex>` fallback name.
#[cfg(feature = "ssh")]
pub fn fallback_fingerprint(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}

#[cfg(feature = "ssh")]
pub struct IdentityManager;

#[cfg(feature = "ssh")]
impl IdentityManager {
    /// Magic header for the encrypted-recipient-key envelope.
    const MAGIC: &[u8; 8] = b"RINGSSH1";

    /// Encrypt the repo's symmetric key so only the holder of the matching ed25519 SSH
    /// private key can recover it. `recipient` is the contents of an `ssh-ed25519` public
    /// (or private) key file.
    pub fn encrypt_key_for_ssh_recipient(
        key: &SymmetricCryptoKey,
        recipient: &str,
    ) -> CryptoResult<Vec<u8>> {
        let recipient_pk = ssh_ed25519_public_bytes(recipient)?;
        let x25519_pk = ed25519_to_x25519_public(&recipient_pk)?;

        // Ephemeral X25519 keypair (random).
        let mut ephemeral = [0u8; 32];
        rand::rng().fill_bytes(&mut ephemeral);
        let esk = x25519_dalek::StaticSecret::from(ephemeral);
        let epk = x25519_dalek::PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce);

        let ciphertext = aead_seal(&aead_key, &nonce, key.as_bytes())?;

        let mut out = Vec::with_capacity(8 + 32 + NONCE_SIZE + ciphertext.len());
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(epk.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt a `IdentityManager` envelope using an OpenSSH ed25519 private key. When the
    /// private key is passphrase-protected, `passphrase` must be supplied.
    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        passphrase: Option<&str>,
    ) -> CryptoResult<SymmetricCryptoKey> {
        if encrypted.len() < 8 + 32 + NONCE_SIZE {
            return Err(CryptoError::Ssh("envelope too short".into()));
        }
        if &encrypted[..8] != Self::MAGIC {
            return Err(CryptoError::Ssh("invalid envelope magic".into()));
        }
        let (_, rest) = encrypted.split_at(8);
        let (epk_bytes, rest) = rest.split_at(32);
        let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);

        let mut epk_arr = [0u8; 32];
        epk_arr.copy_from_slice(epk_bytes);
        let epk = x25519_dalek::PublicKey::from(epk_arr);

        let seed = ssh_ed25519_seed(identity_content, passphrase)?;
        let scalar = ed25519_seed_to_x25519_scalar(&seed);
        let sk = x25519_dalek::StaticSecret::from(scalar);
        let shared = sk.diffie_hellman(&epk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce_arr = [0u8; NONCE_SIZE];
        nonce_arr.copy_from_slice(nonce_bytes);

        let key_bytes = aead_open(&aead_key, &nonce_arr, ct)?;
        SymmetricCryptoKey::from_bytes(&key_bytes)
    }
}

#[cfg(feature = "ssh")]
pub fn ssh_ed25519_public_bytes(recipient: &str) -> CryptoResult<[u8; 32]> {
    if let Ok(pubkey) = ssh_key::PublicKey::from_openssh(recipient.trim()) {
        if let Ok(bytes) = ed25519_pub_from_ssh_public(&pubkey) {
            return Ok(bytes);
        }
    }
    let privkey = ssh_key::PrivateKey::from_openssh(recipient.trim())
        .map_err(|e| CryptoError::Ssh(format!("invalid SSH key: {e}")))?;
    ed25519_pub_from_ssh_private(&privkey)
}

#[cfg(feature = "ssh")]
pub fn ed25519_pub_from_ssh_public(pubkey: &ssh_key::PublicKey) -> CryptoResult<[u8; 32]> {
    use ssh_key::public::KeyData;
    match pubkey.key_data() {
        KeyData::Ed25519(p) => {
            let b: &[u8] = p.as_ref();
            if b.len() != 32 {
                return Err(CryptoError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

#[cfg(feature = "ssh")]
pub fn ed25519_pub_from_ssh_private(privkey: &ssh_key::PrivateKey) -> CryptoResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => {
            let b: &[u8] = kp.public.as_ref();
            if b.len() != 32 {
                return Err(CryptoError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

#[cfg(feature = "ssh")]
pub fn ssh_ed25519_seed(identity_content: &str, passphrase: Option<&str>) -> CryptoResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    let privkey = if let Some(pw) = passphrase {
        ssh_key::PrivateKey::from_openssh(identity_content.trim())
            .and_then(|k| k.decrypt(pw))
            .map_err(|e| CryptoError::Ssh(format!("failed to decrypt SSH identity: {e}")))
    } else {
        ssh_key::PrivateKey::from_openssh(identity_content.trim()).map_err(|e| {
            CryptoError::Ssh(format!(
                "invalid/unencrypted SSH identity (passphrase needed?): {e}"
            ))
        })
    }?;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => Ok(kp.private.to_bytes()),
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

#[cfg(feature = "ssh")]
pub fn ed25519_to_x25519_public(edwards: &[u8; 32]) -> CryptoResult<x25519_dalek::PublicKey> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    use curve25519_dalek::montgomery::MontgomeryPoint;
    let compressed = CompressedEdwardsY::from_slice(edwards)
        .map_err(|_| CryptoError::Ssh("invalid ed25519 public point".into()))?;
    let point = compressed
        .decompress()
        .ok_or_else(|| CryptoError::Ssh("invalid ed25519 public point".into()))?;
    let mont: MontgomeryPoint = point.to_montgomery();
    Ok(x25519_dalek::PublicKey::from(mont.to_bytes()))
}

#[cfg(feature = "ssh")]
pub fn ed25519_seed_to_x25519_scalar(seed: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha512};
    let hash = Sha512::digest(seed);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hash[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    scalar
}

#[cfg(feature = "ssh")]
pub fn derive_aead_key(shared: &[u8]) -> CryptoResult<[u8; 32]> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"gitcrypt-ssh-v1"), shared);
    let mut key = [0u8; 32];
    hk.expand(b"ssh-recipient-key", &mut key)
        .map_err(|_| CryptoError::Ssh("hkdf expand failed".into()))?;
    Ok(key)
}

#[cfg(feature = "ssh")]
pub fn aead_seal(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
    use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, ChaCha20Poly1305, Nonce};
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
    let nonce_arr = Nonce::from_slice(nonce);
    let ct = cipher.encrypt(nonce_arr, Payload { msg: plaintext, aad: &[] }).map_err(|_| CryptoError::Ssh("encryption failed".into()))?;
    Ok(ct)
}

#[cfg(feature = "ssh")]
pub fn aead_open(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
    use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, ChaCha20Poly1305, Nonce};
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
    let nonce_arr = Nonce::from_slice(nonce);
    let pt = cipher.decrypt(nonce_arr, Payload { msg: ciphertext, aad: &[] }).map_err(|_| CryptoError::Ssh("decryption failed (wrong key or tampered)".into()))?;
    Ok(pt)
}

// ---------------------------------------------------------------------------
// Vault Implementation
// ---------------------------------------------------------------------------

/// Original personal-vault format.
const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
/// Team-vault format (age-wrapped DEK). Unbound, like v1.
const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
/// Current format.
const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
/// Schema written for newly created vaults.
const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
const VAULT_KDF_ALGORITHM: &str = "argon2id";
/// Known plaintext used to verify the vault key on open.
pub(crate) const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

const KDF_M_COST_MIN: u32 = 8_192; // 8 MiB
const KDF_M_COST_MAX: u32 = 131_072; // 128 MiB
const KDF_T_COST_MIN: u32 = 1;
const KDF_T_COST_MAX: u32 = 20;
const KDF_P_COST_MIN: u32 = 1;
const KDF_P_COST_MAX: u32 = 16;

const VAULT_KDF_M_COST: u32 = 19_456;
const VAULT_KDF_T_COST: u32 = 2;
const VAULT_KDF_P_COST: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SafeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("vault already exists at path: {path}")]
    VaultAlreadyExists { path: String },

    #[error("vault not found at path: {path}")]
    VaultNotFound { path: String },

    #[error("vault unreadable: {reason}")]
    VaultUnreadable { reason: String },

    #[error("vault corrupted: {reason}")]
    VaultCorrupted { reason: String },

    #[error("vault tampered: {reason}")]
    VaultTampered { reason: String },

    #[error("invalid vault: {reason}")]
    InvalidVault { reason: String },

    #[error("secret key '{key}' not found")]
    SecretNotFound { key: String },

    #[error("secret key '{key}' already exists")]
    SecretAlreadyExists { key: String },

    #[error("migration failed: {reason}")]
    MigrationFailed { reason: String },

    #[error("decryption failed")]
    DecryptionFailed,

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type SafeResult<T> = std::result::Result<T, SafeError>;

impl From<SafeError> for napi::Error {
    fn from(e: SafeError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

pub type VaultKey = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherKind {
    XChaCha20Poly1305,
    Aes256Gcm,
}

impl CipherKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::XChaCha20Poly1305 => "xchacha20poly1305",
            Self::Aes256Gcm => "aes256gcm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySchedule {
    HkdfSha256V1,
    LegacyDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPurpose {
    VaultChallenge,
    SecretData,
    VaultStructure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RbacProfile {
    ReadWrite,
    ReadOnly,
}

impl RbacProfile {
    pub fn ensure_write_allowed(&self) -> SafeResult<()> {
        match self {
            Self::ReadWrite => Ok(()),
            Self::ReadOnly => Err(SafeError::InvalidVault {
                reason: "vault opened in read_only mode; writes are disallowed".into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureBinding {
    Legacy,
    Bound,
}

impl StructureBinding {
    fn for_schema(schema: &str) -> Option<Self> {
        match schema {
            VAULT_SCHEMA_V1 | VAULT_SCHEMA_V2 => Some(Self::Legacy),
            VAULT_SCHEMA_V3 => Some(Self::Bound),
            _ => None,
        }
    }

    pub fn of_file(file: &VaultFile) -> Self {
        Self::for_schema(&file.schema).unwrap_or(Self::Legacy)
    }

    pub fn aad(self, key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
        match self {
            Self::Legacy => Vec::new(),
            Self::Bound => secret_aad(key, slot),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SecretSlot<'a> {
    Live,
    History(&'a DateTime<Utc>),
}

pub fn secret_aad(key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
    let mut aad = Vec::with_capacity(key.len() + 64);
    aad.extend_from_slice(b"tsafe/vault/v2/secret\x00");
    aad.extend_from_slice(key.as_bytes());
    aad.push(0);
    match slot {
        SecretSlot::Live => aad.extend_from_slice(b"live"),
        SecretSlot::History(ts) => {
            aad.extend_from_slice(b"history\x00");
            aad.extend_from_slice(ts.to_rfc3339().as_bytes());
        }
    }
    aad
}

pub fn validate_secret_key(key: &str) -> SafeResult<()> {
    if key.is_empty() {
        return Err(SafeError::InvalidVault {
            reason: "secret key must not be empty".into(),
        });
    }
    if key.len() > 256 {
        return Err(SafeError::InvalidVault {
            reason: format!("secret key too long ({} chars, max 256)", key.len()),
        });
    }
    let mut chars = key.chars().peekable();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(SafeError::InvalidVault {
            reason: format!("secret key '{key}' must start with a letter or underscore"),
        });
    }
    let is_sep = |c: char| c == '.' || c == '-' || c == '/';
    let mut prev = first;
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || is_sep(c)) {
            return Err(SafeError::InvalidVault {
                reason: format!("secret key '{key}' contains invalid character '{c}' — use letters, digits, _, -, ., /"),
            });
        }
        if is_sep(c) && is_sep(prev) {
            return Err(SafeError::InvalidVault {
                reason: format!("secret key '{key}' has consecutive separators"),
            });
        }
        prev = c;
    }
    if is_sep(prev) {
        return Err(SafeError::InvalidVault {
            reason: format!("secret key '{key}' must not end with a separator"),
        });
    }
    Ok(())
}

// ── serde types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub updated_at: DateTime<Utc>,
}

pub const DEFAULT_HISTORY_KEEP: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultChallenge {
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    #[serde(rename = "_schema")]
    pub schema: String,
    pub kdf: KdfParams,
    pub cipher: String,
    pub vault_challenge: VaultChallenge,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub secrets: HashMap<String, SecretEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub age_recipients: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_dek: Option<String>,
    #[serde(default)]
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

// ── lock management ──────────────────────────────────────────────────────────

pub struct LockGuard {
    path: PathBuf,
    contents: String,
    _file: File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockFileContents {
    version: u8,
    id: String,
    pid: u32,
    created_at: DateTime<Utc>,
}

impl LockFileContents {
    fn new() -> Self {
        Self {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            pid: std::process::id(),
            created_at: Utc::now(),
        }
    }
}

fn acquire_lock(path: &Path) -> SafeResult<LockGuard> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..2 {
        let contents = serde_json::to_string(&LockFileContents::new())?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                file.flush()?;
                return Ok(LockGuard {
                    path: lock_path.clone(),
                    contents,
                    _file: file,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if try_recover_dead_lock(&lock_path)? {
                    continue;
                }
                return Err(lock_held_error(&lock_path));
            }
            Err(e) => return Err(SafeError::Io(e)),
        }
    }
    Err(lock_held_error(&lock_path))
}

fn lock_path_for(vault_path: &Path) -> PathBuf {
    vault_path.with_extension("vault.lock")
}

pub fn live_lock_holder(vault_path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(lock_path_for(vault_path)).ok()?;
    let lock: LockFileContents = serde_json::from_str(&contents).ok()?;
    process_is_running(lock.pid).then_some(lock.pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    HeldBy(u32),
    Unparseable,
}

pub fn lock_state(vault_path: &Path) -> Option<LockState> {
    let path = lock_path_for(vault_path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) if path.exists() => return Some(LockState::Unparseable),
        Err(_) => return None,
    };
    match serde_json::from_str::<LockFileContents>(&contents) {
        Ok(lock) if process_is_running(lock.pid) => Some(LockState::HeldBy(lock.pid)),
        Ok(_) => None,
        Err(_) => Some(LockState::Unparseable),
    }
}

fn lock_held_error(lock_path: &Path) -> SafeError {
    SafeError::InvalidVault {
        reason: format!(
            "vault is locked by another process: {}",
            lock_path.display()
        ),
    }
}

fn try_recover_dead_lock(lock_path: &Path) -> SafeResult<bool> {
    let original_contents = match std::fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(SafeError::Io(e)),
    };
    let lock: LockFileContents = match serde_json::from_str(&original_contents) {
        Ok(lock) => lock,
        Err(_) => return Ok(false),
    };
    if process_is_running(lock.pid) {
        return Ok(false);
    }

    let current_contents = match std::fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(SafeError::Io(e)),
    };
    if current_contents != original_contents {
        return Ok(false);
    }

    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(SafeError::Io(e)),
    }
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        true
    } else {
        std::io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code != libc::ESRCH)
            .unwrap_or(true)
    }
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();
        return code != windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32;
    }

    let mut exit_code = 0u32;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe {
        CloseHandle(handle);
    }
    ok != 0 && exit_code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn process_is_running(_pid: u32) -> bool {
    true
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let should_remove = std::fs::read_to_string(&self.path)
            .map(|contents| contents == self.contents)
            .unwrap_or(false);
        if should_remove {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ── crypto helpers ──────────────────────────────────────────────────────────

mod vault_crypto {
    use super::*;
    use base64ct::{Base64, Encoding};

    pub fn random_salt() -> Vec<u8> {
        let mut salt = vec![0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        salt
    }

    pub fn derive_key(
        password: &[u8],
        salt: &[u8],
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    ) -> SafeResult<VaultKey> {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params = Params::new(m_cost, t_cost, p_cost, Some(32)).map_err(|_| SafeError::InvalidVault {
            reason: "invalid KDF parameters".into(),
        })?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = [0u8; 32];
        argon2
            .hash_password_into(password, salt, &mut key)
            .map_err(|_| SafeError::DecryptionFailed)?;
        Ok(key)
    }

    pub fn default_vault_cipher() -> CipherKind {
        CipherKind::XChaCha20Poly1305
    }

    pub fn parse_cipher_kind(s: &str) -> SafeResult<CipherKind> {
        match s {
            "xchacha20poly1305" => Ok(CipherKind::XChaCha20Poly1305),
            "aes256gcm" => Ok(CipherKind::Aes256Gcm),
            _ => Err(SafeError::InvalidVault {
                reason: format!("unsupported cipher: {s}"),
            }),
        }
    }

    pub fn encode_b64(bytes: &[u8]) -> String {
        Base64::encode_string(bytes)
    }

    pub fn decode_b64(s: &str) -> SafeResult<Vec<u8>> {
        Base64::decode_vec(s.trim()).map_err(|_| SafeError::InvalidVault {
            reason: "invalid base64 encoding".into(),
        })
    }

    pub fn derive_subkey(key: &VaultKey, purpose: KeyPurpose) -> SafeResult<[u8; 32]> {
        let info = match purpose {
            KeyPurpose::VaultChallenge => b"tsafe/vault/challenge/v1".as_slice(),
            KeyPurpose::SecretData => b"tsafe/vault/secret/v1".as_slice(),
            KeyPurpose::VaultStructure => b"tsafe/vault/structure/v1".as_slice(),
        };
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, key);
        let mut out = [0u8; 32];
        hk.expand(info, &mut out)
            .map_err(|_| SafeError::DecryptionFailed)?;
        Ok(out)
    }

    pub fn encrypt_with_key_schedule(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        cipher: CipherKind,
        plaintext: &[u8],
    ) -> SafeResult<([u8; 12], Vec<u8>)> {
        encrypt_with_key_schedule_aad(key, schedule, purpose, cipher, plaintext, &[])
    }

    pub fn decrypt_with_key_schedule(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        cipher: CipherKind,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> SafeResult<Vec<u8>> {
        decrypt_with_key_schedule_aad(key, schedule, purpose, cipher, nonce, ciphertext, &[])
    }

    pub fn encrypt_with_key_schedule_aad(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        cipher_kind: CipherKind,
        plaintext: &[u8],
        aad: &[u8],
    ) -> SafeResult<([u8; 12], Vec<u8>)> {
        use chacha20poly1305::{
            aead::{Aead, KeyInit, Payload},
            ChaCha20Poly1305, Nonce,
        };

        let subkey = match schedule {
            KeySchedule::HkdfSha256V1 => derive_subkey(key, purpose)?,
            KeySchedule::LegacyDirect => *key,
        };
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        rand::thread_rng().fill_bytes(&mut ephemeral);
        rand::thread_rng().fill_bytes(&mut nonce);

        let ct = match cipher_kind {
            CipherKind::XChaCha20Poly1305 => {
                let cipher_instance =
                    ChaCha20Poly1305::new_from_slice(&subkey).map_err(|_| SafeError::DecryptionFailed)?;
                let nonce = Nonce::from_slice(&nonce_bytes);
                let payload = Payload { msg: plaintext, aad };
                cipher_instance
                    .encrypt(nonce, payload)
                    .map_err(|_| SafeError::DecryptionFailed)?
            }
            CipherKind::Aes256Gcm => {
                use aes_gcm::{Aes256Gcm, KeyInit as _};
                let cipher_instance =
                    Aes256Gcm::new_from_slice(&subkey).map_err(|_| SafeError::DecryptionFailed)?;
                let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
                let payload = aes_gcm::aead::Payload { msg: plaintext, aad };
                cipher_instance
                    .encrypt(nonce, payload)
                    .map_err(|_| SafeError::DecryptionFailed)?
            }
        };
        Ok((nonce_bytes, ct))
    }

    pub fn decrypt_with_key_schedule_aad(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        cipher_kind: CipherKind,
        nonce: &[u8],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> SafeResult<Vec<u8>> {
        use chacha20poly1305::{
            aead::{Aead, KeyInit, Payload},
            ChaCha20Poly1305, Nonce,
        };

        if nonce.len() != 12 {
            return Err(SafeError::DecryptionFailed);
        }
        let subkey = match schedule {
            KeySchedule::HkdfSha256V1 => derive_subkey(key, purpose)?,
            KeySchedule::LegacyDirect => *key,
        };

        let pt = match cipher_kind {
            CipherKind::XChaCha20Poly1305 => {
                let cipher_instance =
                    ChaCha20Poly1305::new_from_slice(&subkey).map_err(|_| SafeError::DecryptionFailed)?;
                let nonce_arr = Nonce::from_slice(nonce);
                let payload = Payload { msg: ciphertext, aad };
                cipher_instance
                    .decrypt(nonce_arr, payload)
                    .map_err(|_| SafeError::DecryptionFailed)?
            }
            CipherKind::Aes256Gcm => {
                use aes_gcm::{Aes256Gcm, KeyInit as _};
                let cipher_instance =
                    Aes256Gcm::new_from_slice(&subkey).map_err(|_| SafeError::DecryptionFailed)?;
                let nonce_arr = aes_gcm::Nonce::from_slice(nonce);
                let payload = aes_gcm::aead::Payload { msg: ciphertext, aad };
                cipher_instance
                    .decrypt(nonce_arr, payload)
                    .map_err(|_| SafeError::DecryptionFailed)?
            }
        };
        Ok(pt)
    }

    pub fn detect_key_schedule(
        key: &VaultKey,
        purpose: KeyPurpose,
        cipher: CipherKind,
        nonce: &[u8],
        ciphertext: &[u8],
        expected_plaintext: &[u8],
    ) -> SafeResult<KeySchedule> {
        if let Ok(pt) = decrypt_with_key_schedule(
            key,
            KeySchedule::HkdfSha256V1,
            purpose,
            cipher,
            nonce,
            ciphertext,
        ) {
            if pt == expected_plaintext {
                return Ok(KeySchedule::HkdfSha256V1);
            }
        }
        if let Ok(pt) = decrypt_with_key_schedule(
            key,
            KeySchedule::LegacyDirect,
            purpose,
            cipher,
            nonce,
            ciphertext,
        ) {
            if pt == expected_plaintext {
                return Ok(KeySchedule::LegacyDirect);
            }
        }
        Err(SafeError::DecryptionFailed)
    }

    pub fn encrypt(key: &VaultKey, plaintext: &[u8]) -> SafeResult<([u8; 12], Vec<u8>)> {
        encrypt_with_key_schedule_aad(
            key,
            KeySchedule::LegacyDirect,
            KeyPurpose::SecretData,
            CipherKind::XChaCha20Poly1305,
            plaintext,
            &[],
        )
    }

    pub fn mac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(key).unwrap();
        mac.update(data);
        let result = mac.finalize();
        let mut tag = [0u8; 32];
        tag.copy_from_slice(&result.into_bytes());
        tag
    }

    pub fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
        use subtle::ConstantTimeEq;
        a.ct_eq(b).into()
    }
}

// ── snapshot module ──────────────────────────────────────────────────────────

mod snapshot {
    use super::*;

    pub const DEFAULT_SNAPSHOT_KEEP: usize = 10;

    pub fn snapshot_dir_for_vault(vault_path: &Path, profile: &str) -> PathBuf {
        let parent = vault_path.parent().unwrap_or_else(|| Path::new("."));
        parent.join("snapshots").join(profile)
    }

    pub fn take(vault_path: &Path, profile: &str, keep: usize) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(vault_path, profile);
        std::fs::create_dir_all(&dir)?;
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%f");
        let snap_path = dir.join(format!("{profile}-{stamp}.snap"));
        std::fs::copy(vault_path, &snap_path)?;

        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut snaps: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "snap"))
                .collect();
            snaps.sort();
            if snaps.len() > keep {
                for old in &snaps[..snaps.len() - keep] {
                    let _ = std::fs::remove_file(old);
                }
            }
        }
        Ok(snap_path)
    }

    pub fn take_pre_upgrade(vault_path: &Path, profile: &str) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(vault_path, profile);
        std::fs::create_dir_all(&dir)?;
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%f");
        let snap_path = dir.join(format!("{profile}-{stamp}.pre_upgrade.snap"));
        std::fs::copy(vault_path, &snap_path)?;
        Ok(snap_path)
    }

    pub fn restore(vault_path: &Path, snapshot_path: &Path) -> SafeResult<()> {
        std::fs::copy(snapshot_path, vault_path)?;
        Ok(())
    }
}

// ── Vault struct ─────────────────────────────────────────────────────────────

/// An open, authenticated vault handle.
#[napi(js_name = "Vault")]
pub struct Vault {
    path: PathBuf,
    file: VaultFile,
    key: VaultKey,
    cipher: CipherKind,
    key_schedule: KeySchedule,
    binding: StructureBinding,
    rollback_suspected: bool,
    access_profile: RbacProfile,
    _lock: Option<LockGuard>,
}

impl Vault {
    pub fn create(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::create_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    pub fn create_with_access_profile(
        path: &Path,
        password: &[u8],
        access_profile: RbacProfile,
    ) -> SafeResult<Self> {
        access_profile.ensure_write_allowed()?;
        if path.exists() {
            return Err(SafeError::VaultAlreadyExists {
                path: path.display().to_string(),
            });
        }
        let salt = vault_crypto::random_salt();
        let key = vault_crypto::derive_key(
            password,
            &salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let now = Utc::now();
        let cipher = vault_crypto::default_vault_cipher();
        let key_schedule = KeySchedule::HkdfSha256V1;

        let (ch_nonce, ch_ct) = vault_crypto::encrypt_with_key_schedule(
            &key,
            key_schedule,
            KeyPurpose::VaultChallenge,
            cipher,
            VAULT_CHALLENGE_PLAINTEXT,
        )?;
        let file = VaultFile {
            schema: VAULT_SCHEMA.to_string(),
            kdf: KdfParams {
                algorithm: "argon2id".to_string(),
                m_cost: VAULT_KDF_M_COST,
                t_cost: VAULT_KDF_T_COST,
                p_cost: VAULT_KDF_P_COST,
                salt: vault_crypto::encode_b64(&salt),
            },
            cipher: cipher.as_str().to_string(),
            vault_challenge: VaultChallenge {
                nonce: vault_crypto::encode_b64(&ch_nonce),
                ciphertext: vault_crypto::encode_b64(&ch_ct),
            },
            created_at: now,
            updated_at: now,
            secrets: HashMap::new(),
            age_recipients: Vec::new(),
            wrapped_dek: None,
            version: 0,
            integrity: None,
        };
        let lock = acquire_lock(path)?;
        let mut vault = Self {
            path: path.to_path_buf(),
            file,
            key,
            cipher,
            key_schedule,
            binding: StructureBinding::Bound,
            rollback_suspected: false,
            access_profile,
            _lock: Some(lock),
        };
        vault.save()?;
        Ok(vault)
    }

    pub fn open(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    pub fn open_read_only(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadOnly)
    }

    pub fn open_with_access_profile(
        path: &Path,
        password: &[u8],
        access_profile: RbacProfile,
    ) -> SafeResult<Self> {
        let lock = acquire_lock(path)?;
        if !path.exists() {
            return Err(SafeError::VaultNotFound {
                path: path.display().to_string(),
            });
        }

        let json = std::fs::read_to_string(path).map_err(|e| SafeError::VaultUnreadable {
            reason: format!("{e}"),
        })?;

        let file: VaultFile = serde_json::from_str(&json).map_err(|e| {
            let preserved = preserve_corrupt_vault(path);
            SafeError::VaultCorrupted {
                reason: format!(
                    "vault JSON is invalid ({e}). The damaged file has been preserved{}",
                    preserved
                        .map(|p| format!(" at {}", p.display()))
                        .unwrap_or_default()
                ),
            }
        })?;

        let binding =
            StructureBinding::for_schema(&file.schema).ok_or_else(|| SafeError::InvalidVault {
                reason: format!("unknown schema: {}", file.schema),
            })?;
        let cipher = vault_crypto::parse_cipher_kind(&file.cipher)?;
        if file.kdf.algorithm != VAULT_KDF_ALGORITHM {
            return Err(SafeError::InvalidVault {
                reason: format!("unsupported KDF algorithm: '{}'", file.kdf.algorithm),
            });
        }
        if file.kdf.m_cost < KDF_M_COST_MIN || file.kdf.m_cost > KDF_M_COST_MAX {
            return Err(SafeError::InvalidVault {
                reason: format!("KDF m_cost {} is outside allowed range", file.kdf.m_cost),
            });
        }
        let salt = vault_crypto::decode_b64(&file.kdf.salt)?;
        let key = vault_crypto::derive_key(
            password,
            &salt,
            file.kdf.m_cost,
            file.kdf.t_cost,
            file.kdf.p_cost,
        )?;

        let ch_nonce = vault_crypto::decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = vault_crypto::decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = vault_crypto::detect_key_schedule(
            &key,
            KeyPurpose::VaultChallenge,
            cipher,
            &ch_nonce,
            &ch_ct,
            VAULT_CHALLENGE_PLAINTEXT,
        )?;

        Self::verify_integrity(&file, &key, binding)?;
        let rollback_suspected = detect_rollback(path, &file);

        Ok(Self {
            path: path.to_path_buf(),
            file,
            key,
            cipher,
            key_schedule,
            binding,
            rollback_suspected,
            access_profile,
            _lock: Some(lock),
        })
    }

    pub fn open_with_key(path: &Path, key: VaultKey) -> SafeResult<Self> {
        Self::open_with_key_with_access_profile(path, key, RbacProfile::ReadWrite)
    }

    pub fn open_with_key_with_access_profile(
        path: &Path,
        key: VaultKey,
        access_profile: RbacProfile,
    ) -> SafeResult<Self> {
        let lock = acquire_lock(path)?;
        if !path.exists() {
            return Err(SafeError::VaultNotFound {
                path: path.display().to_string(),
            });
        }
        let json = std::fs::read_to_string(path)?;
        let file: VaultFile =
            serde_json::from_str(&json).map_err(|e| SafeError::VaultCorrupted {
                reason: format!("vault JSON parse error: {e}"),
            })?;
        let binding =
            StructureBinding::for_schema(&file.schema).ok_or_else(|| SafeError::InvalidVault {
                reason: format!("unknown schema: {}", file.schema),
            })?;
        let cipher = vault_crypto::parse_cipher_kind(&file.cipher)?;
        let ch_nonce = vault_crypto::decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = vault_crypto::decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = vault_crypto::detect_key_schedule(
            &key,
            KeyPurpose::VaultChallenge,
            cipher,
            &ch_nonce,
            &ch_ct,
            VAULT_CHALLENGE_PLAINTEXT,
        )?;
        Self::verify_integrity(&file, &key, binding)?;
        let rollback_suspected = detect_rollback(path, &file);
        Ok(Self {
            path: path.to_path_buf(),
            file,
            key,
            cipher,
            key_schedule,
            binding,
            rollback_suspected,
            access_profile,
            _lock: Some(lock),
        })
    }

    pub fn is_team_vault(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<VaultFile>(&json).ok())
            .map(|f| !f.age_recipients.is_empty() && f.wrapped_dek.is_some())
            .unwrap_or(false)
    }

    pub fn save(&mut self) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut pre_upgrade_snapshot: Option<PathBuf> = None;
        if self.path.exists() {
            let replacing_legacy = Self::on_disk_schema_is_unbound(&self.path);
            match self.profile_name() {
                Some(profile) => {
                    let snap_result = if replacing_legacy {
                        snapshot::take_pre_upgrade(&self.path, &profile)
                    } else {
                        snapshot::take(&self.path, &profile, snapshot::DEFAULT_SNAPSHOT_KEEP)
                    };
                    match snap_result {
                        Ok(snap) if replacing_legacy => pre_upgrade_snapshot = Some(snap),
                        Ok(_) => {}
                        Err(e) if replacing_legacy => {
                            return Err(SafeError::MigrationFailed {
                                reason: format!("could not take pre-upgrade snapshot: {e}"),
                            });
                        }
                        Err(_) => {}
                    }
                }
                None if replacing_legacy => {
                    return Err(SafeError::MigrationFailed {
                        reason: "cannot derive snapshot profile name".to_string(),
                    });
                }
                None => {}
            }
        }
        if self.binding == StructureBinding::Legacy {
            self.upgrade_to_bound()?;
        }
        let high_water = read_high_water_version(&self.path).unwrap_or(0);
        self.file.version = self.file.version.max(high_water).saturating_add(1);
        self.file.schema = VAULT_SCHEMA_V3.to_string();
        self.file.integrity = Some(vault_crypto::encode_b64(&Self::integrity_tag(
            &self.file, &self.key,
        )?));
        let json = serde_json::to_string_pretty(&self.file)?;
        let tmp = self.path.with_extension("vault.tmp");
        crate::fsperm::write_owner_only(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        crate::fsperm::set_owner_only(&self.path)?;
        record_high_water_version(&self.path, self.file.version);
        let _ = pre_upgrade_snapshot;
        Ok(())
    }

    fn on_disk_schema_is_unbound(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<VaultFile>(&json).ok())
            .map(|f| !schema_is_bound(&f.schema))
            .unwrap_or(false)
    }

    fn upgrade_to_bound(&mut self) -> SafeResult<()> {
        let mut rebound: HashMap<String, SecretEntry> =
            HashMap::with_capacity(self.file.secrets.len());

        for (name, entry) in &self.file.secrets {
            let live_pt = vault_crypto::decrypt_with_key_schedule(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &vault_crypto::decode_b64(&entry.nonce)?,
                &vault_crypto::decode_b64(&entry.ciphertext)?,
            )?;
            let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &live_pt,
                &secret_aad(name, SecretSlot::Live),
            )?;

            let mut history = Vec::with_capacity(entry.history.len());
            for h in &entry.history {
                let pt = vault_crypto::decrypt_with_key_schedule(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &vault_crypto::decode_b64(&h.nonce)?,
                    &vault_crypto::decode_b64(&h.ciphertext)?,
                )?;
                let (hn, hct) = vault_crypto::encrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &pt,
                    &secret_aad(name, SecretSlot::History(&h.updated_at)),
                )?;
                history.push(HistoryEntry {
                    nonce: vault_crypto::encode_b64(&hn),
                    ciphertext: vault_crypto::encode_b64(&hct),
                    updated_at: h.updated_at,
                });
            }

            rebound.insert(
                name.clone(),
                SecretEntry {
                    nonce: vault_crypto::encode_b64(&nonce),
                    ciphertext: vault_crypto::encode_b64(&ct),
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                    tags: entry.tags.clone(),
                    history,
                },
            );
        }

        self.file.secrets = rebound;
        self.binding = StructureBinding::Bound;
        Ok(())
    }

    fn integrity_tag(file: &VaultFile, key: &VaultKey) -> SafeResult<[u8; 32]> {
        let mac_key = vault_crypto::derive_subkey(key, KeyPurpose::VaultStructure)?;
        let mut buf: Vec<u8> = Vec::new();

        fn push_field(buf: &mut Vec<u8>, bytes: &[u8]) {
            buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            buf.extend_from_slice(bytes);
        }

        push_field(&mut buf, b"tsafe/vault/structure/v1");
        push_field(&mut buf, file.schema.as_bytes());
        push_field(&mut buf, file.cipher.as_bytes());
        push_field(&mut buf, file.kdf.algorithm.as_bytes());
        buf.extend_from_slice(&file.kdf.m_cost.to_le_bytes());
        buf.extend_from_slice(&file.kdf.t_cost.to_le_bytes());
        buf.extend_from_slice(&file.kdf.p_cost.to_le_bytes());
        push_field(&mut buf, file.kdf.salt.as_bytes());
        push_field(&mut buf, file.vault_challenge.nonce.as_bytes());
        push_field(&mut buf, file.vault_challenge.ciphertext.as_bytes());
        push_field(&mut buf, file.created_at.to_rfc3339().as_bytes());
        push_field(&mut buf, file.updated_at.to_rfc3339().as_bytes());
        buf.extend_from_slice(&file.version.to_le_bytes());

        let mut recipients: Vec<&String> = file.age_recipients.iter().collect();
        recipients.sort();
        buf.extend_from_slice(&(recipients.len() as u64).to_le_bytes());
        for r in recipients {
            push_field(&mut buf, r.as_bytes());
        }
        push_field(
            &mut buf,
            file.wrapped_dek.as_deref().unwrap_or("").as_bytes(),
        );

        let mut names: Vec<&String> = file.secrets.keys().collect();
        names.sort();
        buf.extend_from_slice(&(names.len() as u64).to_le_bytes());
        for name in names {
            let entry = &file.secrets[name];
            push_field(&mut buf, name.as_bytes());
            push_field(&mut buf, entry.nonce.as_bytes());
            push_field(&mut buf, entry.ciphertext.as_bytes());
            push_field(&mut buf, entry.created_at.to_rfc3339().as_bytes());
            push_field(&mut buf, entry.updated_at.to_rfc3339().as_bytes());

            let mut tag_names: Vec<&String> = entry.tags.keys().collect();
            tag_names.sort();
            buf.extend_from_slice(&(tag_names.len() as u64).to_le_bytes());
            for tag_name in tag_names {
                push_field(&mut buf, tag_name.as_bytes());
                push_field(&mut buf, entry.tags[tag_name].as_bytes());
            }

            buf.extend_from_slice(&(entry.history.len() as u64).to_le_bytes());
            for h in &entry.history {
                push_field(&mut buf, h.nonce.as_bytes());
                push_field(&mut buf, h.ciphertext.as_bytes());
                push_field(&mut buf, h.updated_at.to_rfc3339().as_bytes());
            }
        }

        Ok(vault_crypto::mac_sha256(&mac_key, &buf))
    }

    fn verify_integrity(
        file: &VaultFile,
        key: &VaultKey,
        binding: StructureBinding,
    ) -> SafeResult<()> {
        if binding == StructureBinding::Legacy {
            return Ok(());
        }
        let stored = file
            .integrity
            .as_deref()
            .ok_or_else(|| SafeError::VaultTampered {
                reason: "vault declares schema v3 but carries no integrity tag".into(),
            })?;
        let stored_bytes =
            vault_crypto::decode_b64(stored).map_err(|_| SafeError::VaultTampered {
                reason: "integrity tag is not valid base64".into(),
            })?;
        let expected = Self::integrity_tag(file, key)?;
        if !vault_crypto::ct_eq_bytes(&stored_bytes, &expected) {
            return Err(SafeError::VaultTampered {
                reason: "structure MAC mismatch".into(),
            });
        }
        Ok(())
    }

    fn aad_for(&self, key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
        match self.binding {
            StructureBinding::Legacy => Vec::new(),
            StructureBinding::Bound => secret_aad(key, slot),
        }
    }

    fn demote_to_history(&self, key: &str, existing: &SecretEntry) -> SafeResult<HistoryEntry> {
        if self.binding == StructureBinding::Legacy {
            return Ok(HistoryEntry {
                nonce: existing.nonce.clone(),
                ciphertext: existing.ciphertext.clone(),
                updated_at: existing.updated_at,
            });
        }
        let pt = vault_crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &vault_crypto::decode_b64(&existing.nonce)?,
            &vault_crypto::decode_b64(&existing.ciphertext)?,
            &secret_aad(key, SecretSlot::Live),
        )?;
        let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &pt,
            &secret_aad(key, SecretSlot::History(&existing.updated_at)),
        )?;
        Ok(HistoryEntry {
            nonce: vault_crypto::encode_b64(&nonce),
            ciphertext: vault_crypto::encode_b64(&ct),
            updated_at: existing.updated_at,
        })
    }

    pub fn set(&mut self, key: &str, value: &str, tags: HashMap<String, String>) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        validate_secret_key(key)?;
        if self.binding == StructureBinding::Legacy {
            self.upgrade_to_bound()?;
        }
        let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            value.as_bytes(),
            &self.aad_for(key, SecretSlot::Live),
        )?;
        let now = Utc::now();
        let (created_at, history, tags) = match self.file.secrets.get(key) {
            Some(existing) => {
                let mut h = existing.history.clone();
                h.push(self.demote_to_history(key, existing)?);
                if h.len() > DEFAULT_HISTORY_KEEP {
                    h.drain(..h.len() - DEFAULT_HISTORY_KEEP);
                }
                let merged_tags = if tags.is_empty() {
                    existing.tags.clone()
                } else {
                    tags
                };
                (existing.created_at, h, merged_tags)
            }
            None => (now, Vec::new(), tags),
        };
        self.file.secrets.insert(
            key.to_string(),
            SecretEntry {
                nonce: vault_crypto::encode_b64(&nonce),
                ciphertext: vault_crypto::encode_b64(&ct),
                created_at,
                updated_at: now,
                tags,
                history,
            },
        );
        self.file.updated_at = now;
        self.save()
    }

    pub fn get(&self, key: &str) -> SafeResult<Zeroizing<String>> {
        let entry = self
            .file
            .secrets
            .get(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        let nonce = vault_crypto::decode_b64(&entry.nonce)?;
        let ct = vault_crypto::decode_b64(&entry.ciphertext)?;
        let pt = vault_crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &nonce,
            &ct,
            &self.aad_for(key, SecretSlot::Live),
        )?;
        match String::from_utf8(pt) {
            Ok(s) => Ok(Zeroizing::new(s)),
            Err(e) => {
                let mut bytes = e.into_bytes();
                bytes.zeroize();
                Err(SafeError::InvalidVault {
                    reason: "secret is not valid UTF-8".into(),
                })
            }
        }
    }

    pub fn delete(&mut self, key: &str) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        if !self.file.secrets.contains_key(key) {
            return Err(SafeError::SecretNotFound {
                key: key.to_string(),
            });
        }
        self.file.secrets.remove(key);
        self.file.updated_at = Utc::now();
        self.save()
    }

    pub fn rename_key(&mut self, old_key: &str, new_key: &str, overwrite: bool) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        validate_secret_key(new_key)?;
        if !self.file.secrets.contains_key(old_key) {
            return Err(SafeError::SecretNotFound {
                key: old_key.to_string(),
            });
        }
        if !overwrite && self.file.secrets.contains_key(new_key) {
            return Err(SafeError::SecretAlreadyExists {
                key: new_key.to_string(),
            });
        }
        let entry = self.file.secrets.remove(old_key).unwrap();
        let rebound = self.rebind_entry(old_key, new_key, &entry)?;
        self.file.secrets.insert(new_key.to_string(), rebound);
        self.file.updated_at = Utc::now();
        self.save()
    }

    fn rebind_entry(
        &self,
        from_key: &str,
        to_key: &str,
        entry: &SecretEntry,
    ) -> SafeResult<SecretEntry> {
        if self.binding == StructureBinding::Legacy {
            return Ok(entry.clone());
        }
        let live_pt = vault_crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &vault_crypto::decode_b64(&entry.nonce)?,
            &vault_crypto::decode_b64(&entry.ciphertext)?,
            &secret_aad(from_key, SecretSlot::Live),
        )?;
        let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &live_pt,
            &secret_aad(to_key, SecretSlot::Live),
        )?;

        let mut history = Vec::with_capacity(entry.history.len());
        for h in &entry.history {
            let pt = vault_crypto::decrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &vault_crypto::decode_b64(&h.nonce)?,
                &vault_crypto::decode_b64(&h.ciphertext)?,
                &secret_aad(from_key, SecretSlot::History(&h.updated_at)),
            )?;
            let (hn, hct) = vault_crypto::encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &pt,
                &secret_aad(to_key, SecretSlot::History(&h.updated_at)),
            )?;
            history.push(HistoryEntry {
                nonce: vault_crypto::encode_b64(&hn),
                ciphertext: vault_crypto::encode_b64(&hct),
                updated_at: h.updated_at,
            });
        }

        Ok(SecretEntry {
            nonce: vault_crypto::encode_b64(&nonce),
            ciphertext: vault_crypto::encode_b64(&ct),
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            tags: entry.tags.clone(),
            history,
        })
    }

    pub fn list(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.file.secrets.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    pub fn export_all(&self) -> SafeResult<HashMap<String, String>> {
        self.list()
            .iter()
            .map(|k| {
                let val = self.get(k)?;
                Ok((k.to_string(), (*val).clone()))
            })
            .collect()
    }

    pub fn get_version(&self, key: &str, version: usize) -> SafeResult<Zeroizing<String>> {
        if version == 0 {
            return self.get(key);
        }
        let entry = self
            .file
            .secrets
            .get(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        let hist_idx =
            entry
                .history
                .len()
                .checked_sub(version)
                .ok_or_else(|| SafeError::InvalidVault {
                    reason: format!("version {version} does not exist for '{key}'"),
                })?;
        let h = &entry.history[hist_idx];
        let nonce = vault_crypto::decode_b64(&h.nonce)?;
        let ct = vault_crypto::decode_b64(&h.ciphertext)?;
        let pt = vault_crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &nonce,
            &ct,
            &self.aad_for(key, SecretSlot::History(&h.updated_at)),
        )?;
        match String::from_utf8(pt) {
            Ok(s) => Ok(Zeroizing::new(s)),
            Err(e) => {
                let mut bytes = e.into_bytes();
                bytes.zeroize();
                Err(SafeError::InvalidVault {
                    reason: "secret is not valid UTF-8".into(),
                })
            }
        }
    }

    pub fn history(&self, key: &str) -> SafeResult<Vec<(usize, DateTime<Utc>)>> {
        let entry = self
            .file
            .secrets
            .get(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        let mut versions = vec![(0usize, entry.updated_at)];
        for (i, h) in entry.history.iter().rev().enumerate() {
            versions.push((i + 1, h.updated_at));
        }
        Ok(versions)
    }

    pub fn revert_to_version(&mut self, key: &str, version: usize) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        if version == 0 {
            return Ok(());
        }
        let target_value = self.get_version(key, version)?;
        let now = Utc::now();
        let demoted = {
            let entry = self
                .file
                .secrets
                .get(key)
                .ok_or_else(|| SafeError::SecretNotFound {
                    key: key.to_string(),
                })?;
            self.demote_to_history(key, entry)?
        };
        let entry = self.file.secrets.get_mut(key).unwrap();
        entry.history.push(demoted);
        if entry.history.len() > DEFAULT_HISTORY_KEEP {
            entry
                .history
                .drain(..entry.history.len() - DEFAULT_HISTORY_KEEP);
        }

        let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            target_value.as_bytes(),
            &self.aad_for(key, SecretSlot::Live),
        )?;
        let entry = self.file.secrets.get_mut(key).unwrap();
        entry.nonce = vault_crypto::encode_b64(&nonce);
        entry.ciphertext = vault_crypto::encode_b64(&ct);
        entry.updated_at = now;
        self.file.updated_at = now;
        self.save()
    }

    pub fn prune_history(&mut self, key: &str, keep_n: usize) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        let entry = self
            .file
            .secrets
            .get_mut(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        if entry.history.len() > keep_n {
            entry.history.drain(..entry.history.len() - keep_n);
        }
        self.file.updated_at = Utc::now();
        self.save()
    }

    pub fn rotate(&mut self, new_password: &[u8]) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        let all = self.export_all()?;
        let meta: HashMap<String, _> = self
            .file
            .secrets
            .iter()
            .map(|(k, e)| (k.clone(), (e.tags.clone(), e.created_at, e.history.clone())))
            .collect();

        let mut history_plaintext: HashMap<String, Vec<(String, DateTime<Utc>)>> = HashMap::new();
        for (key, entry) in &self.file.secrets {
            let mut pts = Vec::new();
            for h in &entry.history {
                let nonce = vault_crypto::decode_b64(&h.nonce)?;
                let ct = vault_crypto::decode_b64(&h.ciphertext)?;
                let pt = vault_crypto::decrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &nonce,
                    &ct,
                    &self.aad_for(key, SecretSlot::History(&h.updated_at)),
                )?;
                let s = String::from_utf8(pt).map_err(|_| SafeError::InvalidVault {
                    reason: "history entry is not valid UTF-8".into(),
                })?;
                pts.push((s, h.updated_at));
            }
            history_plaintext.insert(key.clone(), pts);
        }

        let new_salt = vault_crypto::random_salt();
        let new_key = vault_crypto::derive_key(
            new_password,
            &new_salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let new_cipher = vault_crypto::default_vault_cipher();
        let new_key_schedule = KeySchedule::HkdfSha256V1;

        let now = Utc::now();
        let mut new_secrets = HashMap::with_capacity(all.len());
        for (key, value) in &all {
            let (nonce, ct) = vault_crypto::encrypt_with_key_schedule_aad(
                &new_key,
                new_key_schedule,
                KeyPurpose::SecretData,
                new_cipher,
                value.as_bytes(),
                &secret_aad(key, SecretSlot::Live),
            )?;
            let (ref tags, created_at, _) = meta[key];

            let mut new_history = Vec::new();
            if let Some(pts) = history_plaintext.get(key) {
                for (pt, updated_at) in pts {
                    let (hn, hct) = vault_crypto::encrypt_with_key_schedule_aad(
                        &new_key,
                        new_key_schedule,
                        KeyPurpose::SecretData,
                        new_cipher,
                        pt.as_bytes(),
                        &secret_aad(key, SecretSlot::History(updated_at)),
                    )?;
                    new_history.push(HistoryEntry {
                        nonce: vault_crypto::encode_b64(&hn),
                        ciphertext: vault_crypto::encode_b64(&hct),
                        updated_at: *updated_at,
                    });
                }
            }

            new_secrets.insert(
                key.clone(),
                SecretEntry {
                    nonce: vault_crypto::encode_b64(&nonce),
                    ciphertext: vault_crypto::encode_b64(&ct),
                    created_at,
                    updated_at: now,
                    tags: tags.clone(),
                    history: new_history,
                },
            );
        }

        let (ch_nonce, ch_ct) = vault_crypto::encrypt_with_key_schedule(
            &new_key,
            new_key_schedule,
            KeyPurpose::VaultChallenge,
            new_cipher,
            VAULT_CHALLENGE_PLAINTEXT,
        )?;
        self.file.kdf = KdfParams {
            algorithm: "argon2id".to_string(),
            m_cost: VAULT_KDF_M_COST,
            t_cost: VAULT_KDF_T_COST,
            p_cost: VAULT_KDF_P_COST,
            salt: vault_crypto::encode_b64(&new_salt),
        };
        self.file.vault_challenge = VaultChallenge {
            nonce: vault_crypto::encode_b64(&ch_nonce),
            ciphertext: vault_crypto::encode_b64(&ch_ct),
        };
        self.file.cipher = new_cipher.as_str().to_string();
        self.file.secrets = new_secrets;
        self.file.updated_at = now;
        self.key = new_key;
        self.cipher = new_cipher;
        self.key_schedule = new_key_schedule;
        self.binding = StructureBinding::Bound;
        self.save()
    }

    pub fn rollback_suspected(&self) -> bool {
        self.rollback_suspected
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn secret_count(&self) -> usize {
        self.file.secrets.len()
    }

    pub fn access_profile(&self) -> RbacProfile {
        self.access_profile
    }

    pub fn file(&self) -> &VaultFile {
        &self.file
    }

    pub fn ensure_write_allowed(&self) -> SafeResult<()> {
        self.access_profile.ensure_write_allowed()
    }

    fn profile_name(&self) -> Option<String> {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }
}

// ── NAPI Bindings ────────────────────────────────────────────────────────────

#[napi(object)]
pub struct VaultHistoryItem {
    pub version: u32,
    pub updated_at: String,
}

#[napi(object)]
pub struct VaultRotationDueItem {
    pub key: String,
    pub days_overdue: f64,
    pub policy: String,
}

#[napi(object)]
pub struct VaultStatusObject {
    pub root: String,
    pub is_initialized: bool,
    pub exists: bool,
    pub format_version: Option<u32>,
    pub pending_transaction: bool,
}

#[napi(object)]
pub struct VaultLockStateObject {
    pub state: String,
    pub pid: Option<u32>,
}

#[napi]
impl Vault {
    #[napi(factory)]
    pub fn create_vault(path: String, password: Buffer) -> Result<Self> {
        Self::create(Path::new(&path), password.as_ref()).map_err(Into::into)
    }

    #[napi(factory)]
    pub fn open_vault(path: String, password: Buffer) -> Result<Self> {
        Self::open(Path::new(&path), password.as_ref()).map_err(Into::into)
    }

    #[napi(factory)]
    pub fn open_vault_read_only(path: String, password: Buffer) -> Result<Self> {
        Self::open_read_only(Path::new(&path), password.as_ref()).map_err(Into::into)
    }

    #[napi]
    pub fn set_secret(
        &mut self,
        key: String,
        value: String,
        tags: Option<HashMap<String, String>>,
    ) -> Result<()> {
        self.set(&key, &value, tags.unwrap_or_default())
            .map_err(Into::into)
    }

    #[napi]
    pub fn get_secret(&self, key: String) -> Result<String> {
        let val = self.get(&key)?;
        Ok((*val).clone())
    }

    #[napi]
    pub fn delete_secret(&mut self, key: String) -> Result<()> {
        self.delete(&key).map_err(Into::into)
    }

    #[napi]
    pub fn rename_secret_key(
        &mut self,
        old_key: String,
        new_key: String,
        overwrite: Option<bool>,
    ) -> Result<()> {
        self.rename_key(&old_key, &new_key, overwrite.unwrap_or(false))
            .map_err(Into::into)
    }

    #[napi]
    pub fn list_secret_keys(&self) -> Vec<String> {
        self.list().into_iter().map(|s| s.to_string()).collect()
    }

    #[napi]
    pub fn export_all_secrets(&self) -> Result<HashMap<String, String>> {
        self.export_all().map_err(Into::into)
    }

    #[napi]
    pub fn rotate_password(&mut self, new_password: Buffer) -> Result<()> {
        self.rotate(new_password.as_ref()).map_err(Into::into)
    }

    #[napi]
    pub fn get_secret_version(&self, key: String, version: u32) -> Result<String> {
        let val = self.get_version(&key, version as usize)?;
        Ok((*val).clone())
    }

    #[napi]
    pub fn get_secret_history(&self, key: String) -> Result<Vec<VaultHistoryItem>> {
        let hist = self.history(&key)?;
        Ok(hist
            .into_iter()
            .map(|(ver, ts)| VaultHistoryItem {
                version: ver as u32,
                updated_at: ts.to_rfc3339(),
            })
            .collect())
    }

    #[napi]
    pub fn revert_secret_to_version(&mut self, key: String, version: u32) -> Result<()> {
        self.revert_to_version(&key, version as usize)
            .map_err(Into::into)
    }

    #[napi]
    pub fn prune_secret_history(&mut self, key: String, keep_n: u32) -> Result<()> {
        self.prune_history(&key, keep_n as usize)
            .map_err(Into::into)
    }

    #[napi]
    pub fn is_rollback_suspected(&self) -> bool {
        self.rollback_suspected()
    }

    #[napi]
    pub fn get_secret_count(&self) -> u32 {
        self.secret_count() as u32
    }

    #[napi]
    pub fn get_path(&self) -> String {
        self.path().to_string_lossy().into_owned()
    }

    #[napi(factory)]
    pub fn check_is_team_vault(path: String) -> bool {
        Self::is_team_vault(Path::new(&path))
    }

    #[napi(factory)]
    pub fn get_vault_status(path: String) -> VaultStatusObject {
        let p = Path::new(&path);
        let exists = p.exists();
        let is_initialized = exists && std::fs::read_to_string(p).ok().and_then(|j| serde_json::from_str::<VaultFile>(&j).ok()).is_some();
        let format_version = if is_initialized {
            std::fs::read_to_string(p).ok().and_then(|j| serde_json::from_str::<VaultFile>(&j).ok()).map(|f| match f.schema.as_str() {
                VAULT_SCHEMA_V1 => 1,
                VAULT_SCHEMA_V2 => 2,
                VAULT_SCHEMA_V3 => 3,
                _ => 0,
            })
        } else {
            None
        };
        VaultStatusObject {
            root: path,
            is_initialized,
            exists,
            format_version,
            pending_transaction: false,
        }
    }

    #[napi(factory)]
    pub fn get_vault_lock_state(path: String) -> VaultLockStateObject {
        match lock_state(Path::new(&path)) {
            Some(LockState::HeldBy(pid)) => VaultLockStateObject {
                state: "held".to_string(),
                pid: Some(pid),
            },
            Some(LockState::Unparseable) => VaultLockStateObject {
                state: "unparseable".to_string(),
                pid: None,
            },
            None => VaultLockStateObject {
                state: "unlocked".to_string(),
                pid: None,
            },
        }
    }

    #[napi]
    pub fn get_rotation_due_secrets(&self) -> Vec<VaultRotationDueItem> {
        rotation_due(&self.file)
            .into_iter()
            .map(|(key, days, policy)| VaultRotationDueItem {
                key,
                days_overdue: days as f64,
                policy,
            })
            .collect()
    }

    #[napi]
    pub fn get_structure_binding_type(&self) -> String {
        match self.structure_binding() {
            StructureBinding::Bound => "bound".to_string(),
            StructureBinding::Legacy => "legacy".to_string(),
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn high_water_path(vault_path: &Path) -> PathBuf {
    vault_path.with_extension("vault.hwm")
}

fn read_high_water_version(vault_path: &Path) -> Option<u64> {
    std::fs::read_to_string(high_water_path(vault_path))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn record_high_water_version(vault_path: &Path, version: u64) {
    let path = high_water_path(vault_path);
    let highest = read_high_water_version(vault_path)
        .unwrap_or(0)
        .max(version);
    let _ = crate::fsperm::write_owner_only(&path, highest.to_string().as_bytes());
}

fn detect_rollback(vault_path: &Path, file: &VaultFile) -> bool {
    match read_high_water_version(vault_path) {
        Some(seen) if file.version < seen => true,
        _ => false,
    }
}

pub fn is_supported_schema(schema: &str) -> bool {
    StructureBinding::for_schema(schema).is_some()
}

pub fn schema_is_bound(schema: &str) -> bool {
    StructureBinding::for_schema(schema) == Some(StructureBinding::Bound)
}

fn preserve_corrupt_vault(path: &Path) -> Option<PathBuf> {
    let damaged = std::fs::read(path).ok()?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let dest = path.with_extension(format!("vault.corrupt.{stamp}"));
    if dest.exists() {
        return Some(dest);
    }
    std::fs::write(&dest, &damaged).ok().map(|_| dest)
}

pub fn parse_rotation_days(policy: &str) -> Option<i64> {
    let s = policy.trim();
    s.strip_suffix('d')
        .and_then(|prefix| prefix.parse::<i64>().ok())
        .filter(|&d| d > 0)
}

pub fn rotation_due(file: &VaultFile) -> Vec<(String, i64, String)> {
    let now = Utc::now();
    let mut due = Vec::new();
    for (key, entry) in &file.secrets {
        if let Some(policy) = entry.tags.get("rotate_policy") {
            if let Some(days) = parse_rotation_days(policy) {
                let age = (now - entry.updated_at).num_days();
                if age >= days {
                    due.push((key.clone(), age - days, policy.clone()));
                }
            }
        }
    }
    due.sort_by(|a, b| a.0.cmp(&b.0));
    due
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pw() -> &'static [u8] {
        b"test-master-password"
    }

    #[test]
    fn create_and_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("K", "val", HashMap::new()).unwrap();
        drop(v);
        let v2 = Vault::open(&path, pw()).unwrap();
        assert_eq!(&*v2.get("K").unwrap(), "val");
    }

    #[test]
    fn set_get_delete_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("DB_PASS", "s3cr3t", HashMap::new()).unwrap();
        assert_eq!(&*v.get("DB_PASS").unwrap(), "s3cr3t");
        v.delete("DB_PASS").unwrap();
        assert!(matches!(
            v.get("DB_PASS"),
            Err(SafeError::SecretNotFound { .. })
        ));
    }

    #[test]
    fn list_is_sorted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("ZZZ", "z", HashMap::new()).unwrap();
        v.set("AAA", "a", HashMap::new()).unwrap();
        v.set("MMM", "m", HashMap::new()).unwrap();
        assert_eq!(v.list(), vec!["AAA", "MMM", "ZZZ"]);
    }

    #[test]
    fn export_all_decrypts_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("A", "alpha", HashMap::new()).unwrap();
        v.set("B", "beta", HashMap::new()).unwrap();
        let all = v.export_all().unwrap();
        assert_eq!(all["A"], "alpha");
        assert_eq!(all["B"], "beta");
    }

    #[test]
    fn rotate_re_encrypts_under_new_password() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("SECRET", "value", HashMap::new()).unwrap();
        v.rotate(b"new-password").unwrap();
        drop(v);
        assert!(Vault::open(&path, pw()).is_err());
        let v2 = Vault::open(&path, b"new-password").unwrap();
        assert_eq!(&*v2.get("SECRET").unwrap(), "value");
    }
}
