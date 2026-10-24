use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{instrument, warn};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::CryptoError;

// ── Error types ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SafeError {
    #[error("invalid vault: {reason}")]
    InvalidVault { reason: String },

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

    #[error("secret not found: {key}")]
    SecretNotFound { key: String },

    #[error("secret already exists: {key}")]
    SecretAlreadyExists { key: String },

    #[error("migration failed: {reason}")]
    MigrationFailed { reason: String },

    #[error("decryption failed")]
    DecryptionFailed,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("crypto error: {0}")]
    Crypto(String),
}

pub type SafeResult<T> = std::result::Result<T, SafeError>;

impl From<SafeError> for napi::Error {
    fn from(e: SafeError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RbacProfile {
    ReadWrite,
    ReadOnly,
}

impl RbacProfile {
    pub fn ensure_write_allowed(self) -> SafeResult<()> {
        if self == Self::ReadOnly {
            Err(SafeError::InvalidVault {
                reason: "vault opened in read_only mode; write operations are forbidden".into(),
            })
        } else {
            Ok(())
        }
    }
}

// ── File Permission Helpers ──────────────────────────────────────────────────

pub(crate) fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.flush()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

pub(crate) fn set_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

// ── KeyManager & IdentityManager ─────────────────────────────────────────────

/// Stores and manages the repository's crypt key on disk.
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
        if let Some(configured) = self.config_default_key_path() {
            return configured;
        }
        self.git_crypt_dir().join("keys").join("default")
    }

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

    pub fn init_dirs(&self) -> CryptoResult<()> {
        let crypt_dir = self.git_crypt_dir();
        if crypt_dir.exists() {
            return Err(CryptoError::AlreadyInitialized);
        }
        std::fs::create_dir_all(&crypt_dir)?;
        std::fs::create_dir(crypt_dir.join("keys"))?;
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    pub fn generate_key(&self) -> CryptoResult<crate::crypto::SymmetricCryptoKey> {
        let key = crate::crypto::SymmetricCryptoKey::generate_random();
        self.save_key(&key)?;
        Ok(key)
    }

    pub fn save_key(&self, key: &crate::crypto::SymmetricCryptoKey) -> CryptoResult<()> {
        let key_path = self.default_key_path();
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&key_path)?;
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

    pub fn load_key(&self) -> CryptoResult<crate::crypto::SymmetricCryptoKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(CryptoError::KeyNotFound("default".into()));
        }
        let mut file = File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)
    }

    pub fn export_key(&self, output_path: &Path) -> CryptoResult<()> {
        let key = self.load_key()?;
        let mut file = File::create(output_path)?;
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

    pub fn import_key(&self, input_path: &Path) -> CryptoResult<()> {
        let mut file = File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

#[cfg(feature = "ssh")]
pub struct IdentityManager;

#[cfg(feature = "ssh")]
impl IdentityManager {
    const MAGIC: &[u8; 8] = b"RINGSSH1";

    pub fn encrypt_key_for_ssh_recipient(
        key: &crate::crypto::SymmetricCryptoKey,
        recipient: &str,
    ) -> CryptoResult<Vec<u8>> {
        let recipient_pk = ssh_ed25519_public_bytes(recipient)?;
        let x25519_pk = ed25519_to_x25519_public(&recipient_pk)?;

        let mut ephemeral = [0u8; 32];
        rand::rng().fill_bytes(&mut ephemeral);
        let esk = x25519_dalek::StaticSecret::from(ephemeral);
        let epk = x25519_dalek::PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);

        let ciphertext = aead_seal(&aead_key, &nonce, key.as_bytes())?;

        let mut out = Vec::with_capacity(8 + 32 + 12 + ciphertext.len());
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(epk.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        passphrase: Option<&str>,
    ) -> CryptoResult<crate::crypto::SymmetricCryptoKey> {
        if encrypted.len() < 8 + 32 + 12 {
            return Err(CryptoError::Ssh("envelope too short".into()));
        }
        if &encrypted[..8] != Self::MAGIC {
            return Err(CryptoError::Ssh("invalid envelope magic".into()));
        }
        let (_, rest) = encrypted.split_at(8);
        let (epk_bytes, rest) = rest.split_at(32);
        let (nonce_bytes, ct) = rest.split_at(12);

        let mut epk_arr = [0u8; 32];
        epk_arr.copy_from_slice(epk_bytes);
        let epk = x25519_dalek::PublicKey::from(epk_arr);

        let seed = ssh_ed25519_seed(identity_content, passphrase)?;
        let scalar = ed25519_seed_to_x25519_scalar(&seed);
        let sk = x25519_dalek::StaticSecret::from(scalar);
        let shared = sk.diffie_hellman(&epk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(nonce_bytes);

        let key_bytes = aead_open(&aead_key, &nonce_arr, ct)?;
        crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)
    }
}

// ── SSH Key & Recipient Helpers ──────────────────────────────────────────────

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

#[cfg(feature = "ssh")]
pub fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

#[cfg(feature = "ssh")]
pub fn fallback_fingerprint(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}

#[cfg(feature = "ssh")]
fn ssh_ed25519_public_bytes(recipient: &str) -> CryptoResult<[u8; 32]> {
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
fn ed25519_pub_from_ssh_public(pubkey: &ssh_key::PublicKey) -> CryptoResult<[u8; 32]> {
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
        _ => Err(CryptoError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

#[cfg(feature = "ssh")]
fn ed25519_pub_from_ssh_private(privkey: &ssh_key::PrivateKey) -> CryptoResult<[u8; 32]> {
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
        _ => Err(CryptoError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

#[cfg(feature = "ssh")]
fn ssh_ed25519_seed(identity_content: &str, passphrase: Option<&str>) -> CryptoResult<[u8; 32]> {
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
        _ => Err(CryptoError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

#[cfg(feature = "ssh")]
fn ed25519_to_x25519_public(edwards: &[u8; 32]) -> CryptoResult<x25519_dalek::PublicKey> {
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
fn ed25519_seed_to_x25519_scalar(seed: &[u8; 32]) -> [u8; 32] {
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
fn derive_aead_key(shared: &[u8]) -> CryptoResult<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"gitcrypt-ssh-v1");
    let prk = salt.extract(shared);
    let okm = prk
        .expand(&[b"ssh-recipient-key"], hkdf::HKDF_SHA256)
        .map_err(|_| CryptoError::Ssh("hkdf expand failed".into()))?;
    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| CryptoError::Ssh("hkdf fill failed".into()))?;
    Ok(key)
}

#[cfg(feature = "ssh")]
fn aead_seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce_obj = Nonce::from_slice(nonce);
    cipher
        .encrypt(nonce_obj, plaintext)
        .map_err(|_| CryptoError::Ssh("encryption failed".into()))
}

#[cfg(feature = "ssh")]
fn aead_open(key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce_obj = Nonce::from_slice(nonce);
    cipher
        .decrypt(nonce_obj, ciphertext)
        .map_err(|_| CryptoError::Ssh("decryption failed (wrong key or tampered)".into()))
}

// ── Vault Constants & Schema Definitions ─────────────────────────────────────

const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
const VAULT_KDF_ALGORITHM: &str = "argon2id";
pub(crate) const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

pub const VAULT_KDF_M_COST: u32 = 19_456;
pub const VAULT_KDF_T_COST: u32 = 2;
pub const VAULT_KDF_P_COST: u32 = 1;

const KDF_M_COST_MIN: u32 = 8_192;
const KDF_M_COST_MAX: u32 = 131_072;
const KDF_T_COST_MIN: u32 = 1;
const KDF_T_COST_MAX: u32 = 20;
const KDF_P_COST_MIN: u32 = 1;
const KDF_P_COST_MAX: u32 = 16;

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

// ── On-Disk Types (serde) ───────────────────────────────────────────────────

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

// ── Cryptographic Key & Cipher Types for Vault ─────────────────────────────

#[derive(Clone)]
pub struct VaultKey(Vec<u8>);

impl VaultKey {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPurpose {
    VaultChallenge,
    SecretData,
    VaultStructure,
}

impl KeyPurpose {
    pub fn info_label(self) -> &'static [u8] {
        match self {
            KeyPurpose::VaultChallenge => b"tsafe/vault/v2/challenge",
            KeyPurpose::SecretData => b"tsafe/vault/v2/secret",
            KeyPurpose::VaultStructure => b"tsafe/vault/v2/structure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySchedule {
    HkdfSha256V1,
    LegacyDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherKind {
    XChaCha20Poly1305,
    ChaCha20Poly1305,
    Aes256Gcm,
}

impl CipherKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CipherKind::XChaCha20Poly1305 => "xchacha20poly1305",
            CipherKind::ChaCha20Poly1305 => "chacha20poly1305",
            CipherKind::Aes256Gcm => "aes-256-gcm",
        }
    }
}

pub fn default_vault_cipher() -> CipherKind {
    CipherKind::XChaCha20Poly1305
}

pub fn parse_cipher_kind(s: &str) -> SafeResult<CipherKind> {
    match s {
        "xchacha20poly1305" => Ok(CipherKind::XChaCha20Poly1305),
        "chacha20poly1305" => Ok(CipherKind::ChaCha20Poly1305),
        "aes-256-gcm" => Ok(CipherKind::Aes256Gcm),
        _ => Err(SafeError::InvalidVault {
            reason: format!("unsupported cipher: {s}"),
        }),
    }
}

pub fn random_salt() -> Vec<u8> {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    salt.to_vec()
}

pub fn derive_key(
    password: &[u8],
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> SafeResult<VaultKey> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(m_cost, t_cost, p_cost, Some(32))
        .map_err(|e| SafeError::InvalidVault { reason: format!("Argon2 params error: {e}") })?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = vec![0u8; 32];
    argon2.hash_password_into(password, salt, &mut key)
        .map_err(|e| SafeError::InvalidVault { reason: format!("Argon2 derivation error: {e}") })?;
    Ok(VaultKey::new(key))
}

pub fn derive_subkey(key: &VaultKey, purpose: KeyPurpose) -> SafeResult<[u8; 32]> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, key.as_slice());
    let mut out = [0u8; 32];
    hk.expand(purpose.info_label(), &mut out)
        .map_err(|_| SafeError::Crypto("HKDF expansion failed".into()))?;
    Ok(out)
}

pub fn encode_b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_b64(s: &str) -> SafeResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| SafeError::InvalidVault {
            reason: "invalid base64 encoding".into(),
        })
}

pub fn mac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    let result = mac.finalize();
    let bytes = result.into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

pub fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    subtle::ConstantTimeEq::ct_eq(a, b).into()
}

pub fn encrypt_for_cipher_aad(
    cipher_kind: CipherKind,
    key_bytes: &[u8],
    plaintext: &[u8],
    aad: &[u8],
) -> SafeResult<(Vec<u8>, Vec<u8>)> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let key_32: &[u8; 32] = key_bytes.try_into().map_err(|_| SafeError::DecryptionFailed)?;
    let key = chacha20poly1305::Key::from_slice(key_32);
    match cipher_kind {
        CipherKind::XChaCha20Poly1305 => {
            use chacha20poly1305::XChaCha20Poly1305;
            let cipher = XChaCha20Poly1305::new(key);
            let mut nonce = [0u8; 24];
            rand::rng().fill_bytes(&mut nonce);
            let payload = Payload {
                msg: plaintext,
                aad,
            };
            let nonce_obj = chacha20poly1305::XNonce::from_slice(&nonce);
            let ct = cipher
                .encrypt(nonce_obj, payload)
                .map_err(|_| SafeError::DecryptionFailed)?;
            Ok((nonce.to_vec(), ct))
        }
        CipherKind::ChaCha20Poly1305 | CipherKind::Aes256Gcm => {
            use chacha20poly1305::ChaCha20Poly1305;
            let cipher = ChaCha20Poly1305::new(key);
            let mut nonce = [0u8; 12];
            rand::rng().fill_bytes(&mut nonce);
            let payload = Payload {
                msg: plaintext,
                aad,
            };
            let nonce_obj = chacha20poly1305::Nonce::from_slice(&nonce);
            let ct = cipher
                .encrypt(nonce_obj, payload)
                .map_err(|_| SafeError::DecryptionFailed)?;
            Ok((nonce.to_vec(), ct))
        }
    }
}

pub fn decrypt_for_cipher_aad(
    cipher_kind: CipherKind,
    key_bytes: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> SafeResult<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let key_32: &[u8; 32] = key_bytes.try_into().map_err(|_| SafeError::DecryptionFailed)?;
    let key = chacha20poly1305::Key::from_slice(key_32);
    match cipher_kind {
        CipherKind::XChaCha20Poly1305 => {
            if nonce.len() != 24 {
                return Err(SafeError::DecryptionFailed);
            }
            use chacha20poly1305::XChaCha20Poly1305;
            let cipher = XChaCha20Poly1305::new(key);
            let payload = Payload {
                msg: ciphertext,
                aad,
            };
            let nonce_obj = chacha20poly1305::XNonce::from_slice(nonce);
            cipher
                .decrypt(nonce_obj, payload)
                .map_err(|_| SafeError::DecryptionFailed)
        }
        CipherKind::ChaCha20Poly1305 | CipherKind::Aes256Gcm => {
            if nonce.len() != 12 {
                return Err(SafeError::DecryptionFailed);
            }
            use chacha20poly1305::ChaCha20Poly1305;
            let cipher = ChaCha20Poly1305::new(key);
            let payload = Payload {
                msg: ciphertext,
                aad,
            };
            let nonce_obj = chacha20poly1305::Nonce::from_slice(nonce);
            cipher
                .decrypt(nonce_obj, payload)
                .map_err(|_| SafeError::DecryptionFailed)
        }
    }
}

pub fn encrypt_for_cipher(
    cipher_kind: CipherKind,
    key: &VaultKey,
    plaintext: &[u8],
) -> SafeResult<(Vec<u8>, Vec<u8>)> {
    encrypt_for_cipher_aad(cipher_kind, key.as_slice(), plaintext, &[])
}

pub fn decrypt_for_cipher(
    cipher_kind: CipherKind,
    key: &VaultKey,
    nonce: &[u8],
    ciphertext: &[u8],
) -> SafeResult<Vec<u8>> {
    decrypt_for_cipher_aad(cipher_kind, key.as_slice(), nonce, ciphertext, &[])
}

pub fn encrypt(key: &VaultKey, plaintext: &[u8]) -> SafeResult<(Vec<u8>, Vec<u8>)> {
    encrypt_for_cipher(CipherKind::XChaCha20Poly1305, key, plaintext)
}

pub fn encrypt_with_key_schedule(
    key: &VaultKey,
    schedule: KeySchedule,
    purpose: KeyPurpose,
    cipher: CipherKind,
    plaintext: &[u8],
) -> SafeResult<(Vec<u8>, Vec<u8>)> {
    encrypt_with_key_schedule_aad(key, schedule, purpose, cipher, plaintext, &[])
}

pub fn encrypt_with_key_schedule_aad(
    key: &VaultKey,
    schedule: KeySchedule,
    purpose: KeyPurpose,
    cipher: CipherKind,
    plaintext: &[u8],
    aad: &[u8],
) -> SafeResult<(Vec<u8>, Vec<u8>)> {
    match schedule {
        KeySchedule::HkdfSha256V1 => {
            let subkey = derive_subkey(key, purpose)?;
            encrypt_for_cipher_aad(cipher, &subkey, plaintext, aad)
        }
        KeySchedule::LegacyDirect => {
            encrypt_for_cipher_aad(cipher, key.as_slice(), plaintext, aad)
        }
    }
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

pub fn decrypt_with_key_schedule_aad(
    key: &VaultKey,
    schedule: KeySchedule,
    purpose: KeyPurpose,
    cipher: CipherKind,
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> SafeResult<Vec<u8>> {
    match schedule {
        KeySchedule::HkdfSha256V1 => {
            let subkey = derive_subkey(key, purpose)?;
            decrypt_for_cipher_aad(cipher, &subkey, nonce, ciphertext, aad)
        }
        KeySchedule::LegacyDirect => {
            decrypt_for_cipher_aad(cipher, key.as_slice(), nonce, ciphertext, aad)
        }
    }
}

pub fn detect_key_schedule(
    key: &VaultKey,
    purpose: KeyPurpose,
    cipher: CipherKind,
    nonce: &[u8],
    ciphertext: &[u8],
    expected_plaintext: &[u8],
) -> SafeResult<KeySchedule> {
    if let Ok(subkey) = derive_subkey(key, purpose) {
        if let Ok(pt) = decrypt_for_cipher_aad(cipher, &subkey, nonce, ciphertext, &[]) {
            if ct_eq_bytes(&pt, expected_plaintext) {
                return Ok(KeySchedule::HkdfSha256V1);
            }
        }
    }
    if let Ok(pt) = decrypt_for_cipher_aad(cipher, key.as_slice(), nonce, ciphertext, &[]) {
        if ct_eq_bytes(&pt, expected_plaintext) {
            return Ok(KeySchedule::LegacyDirect);
        }
    }
    Err(SafeError::DecryptionFailed)
}

// ── In-Memory Vault ─────────────────────────────────────────────────────────

pub struct Vault {
    path: PathBuf,
    pub(crate) file: VaultFile,
    key: VaultKey,
    cipher: CipherKind,
    key_schedule: KeySchedule,
    binding: StructureBinding,
    rollback_suspected: bool,
    access_profile: RbacProfile,
    _lock: Option<LockGuard>,
}

struct LockGuard {
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
            id: Uuid::new_v4().to_string(),
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

impl Vault {
    #[instrument(skip(password, path))]
    pub fn create(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::create_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    #[instrument(skip(password, path))]
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
        let salt = random_salt();
        let key = derive_key(
            password,
            &salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let now = Utc::now();
        let cipher = default_vault_cipher();
        let key_schedule = KeySchedule::HkdfSha256V1;

        let (ch_nonce, ch_ct) = encrypt_with_key_schedule(
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
                salt: encode_b64(&salt),
            },
            cipher: cipher.as_str().to_string(),
            vault_challenge: VaultChallenge {
                nonce: encode_b64(&ch_nonce),
                ciphertext: encode_b64(&ch_ct),
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

    #[instrument(skip(password, path))]
    pub fn open(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    #[instrument(skip(password, path))]
    pub fn open_read_only(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadOnly)
    }

    #[instrument(skip(password, path))]
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
            reason: format!(
                "{e}. The vault has NOT been modified and is probably intact — this is usually \
                 temporary (antivirus, a backup agent, file sync, or another tsafe process \
                 holding the file). Try again in a moment. If it persists, check the file's \
                 permissions before considering recovery."
            ),
        })?;

        let file: VaultFile = serde_json::from_str(&json).map_err(|e| {
            let preserved = preserve_corrupt_vault(path);
            SafeError::VaultCorrupted {
                reason: format!(
                    "vault JSON is invalid ({e}). The damaged file has been preserved{} and was \
                     NOT overwritten. Run `tsafe snapshot list` to review recovery points, then \
                     `tsafe snapshot restore --snapshot <path>` to restore one explicitly — note \
                     that snapshots predate the most recent write, so the newest secret may only \
                     exist in the damaged file.",
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
        let cipher = parse_cipher_kind(&file.cipher)?;
        if file.kdf.algorithm != VAULT_KDF_ALGORITHM {
            return Err(SafeError::InvalidVault {
                reason: format!(
                    "unsupported KDF algorithm: '{}' (expected '{VAULT_KDF_ALGORITHM}')",
                    file.kdf.algorithm
                ),
            });
        }
        if file.kdf.m_cost < KDF_M_COST_MIN || file.kdf.m_cost > KDF_M_COST_MAX {
            return Err(SafeError::InvalidVault {
                reason: format!(
                    "KDF m_cost {} is outside allowed range [{KDF_M_COST_MIN}, {KDF_M_COST_MAX}]",
                    file.kdf.m_cost
                ),
            });
        }
        if file.kdf.t_cost < KDF_T_COST_MIN || file.kdf.t_cost > KDF_T_COST_MAX {
            return Err(SafeError::InvalidVault {
                reason: format!(
                    "KDF t_cost {} is outside allowed range [{KDF_T_COST_MIN}, {KDF_T_COST_MAX}]",
                    file.kdf.t_cost
                ),
            });
        }
        if file.kdf.p_cost < KDF_P_COST_MIN || file.kdf.p_cost > KDF_P_COST_MAX {
            return Err(SafeError::InvalidVault {
                reason: format!(
                    "KDF p_cost {} is outside allowed range [{KDF_P_COST_MIN}, {KDF_P_COST_MAX}]",
                    file.kdf.p_cost
                ),
            });
        }
        let salt = decode_b64(&file.kdf.salt)?;
        let key = derive_key(
            password,
            &salt,
            file.kdf.m_cost,
            file.kdf.t_cost,
            file.kdf.p_cost,
        )?;

        let ch_nonce = decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = detect_key_schedule(
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

    pub fn open_with_key_read_only(path: &Path, key: VaultKey) -> SafeResult<Self> {
        Self::open_with_key_with_access_profile(path, key, RbacProfile::ReadOnly)
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
        let cipher = parse_cipher_kind(&file.cipher)?;
        let ch_nonce = decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = detect_key_schedule(
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

    #[instrument(skip(self), fields(secrets = self.file.secrets.len()))]
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
                                reason: format!(
                                    "could not take a pre-upgrade snapshot of {}: {e}. Upgrading \
                                     to schema v3 is one-way — older tsafe versions cannot read \
                                     the result — so the write was refused rather than proceed \
                                     without a rollback copy. Fix the snapshot failure or copy \
                                     the vault file manually, then retry.",
                                    self.path.display()
                                ),
                            });
                        }
                        Err(_) => {}
                    }
                }
                None if replacing_legacy => {
                    return Err(SafeError::MigrationFailed {
                        reason: format!(
                            "cannot derive a snapshot profile name from {} to take a \
                             pre-upgrade snapshot; refusing the one-way upgrade to schema v3. \
                             Copy the vault file manually, then retry.",
                            self.path.display()
                        ),
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
        self.file.integrity = Some(encode_b64(&Self::integrity_tag(
            &self.file, &self.key,
        )?));
        let json = serde_json::to_string_pretty(&self.file)?;
        let tmp = self.path.with_extension("vault.tmp");
        write_owner_only(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        set_owner_only(&self.path)?;
        record_high_water_version(&self.path, self.file.version);
        if let Some(snap) = pre_upgrade_snapshot {
            eprintln!(
                "note: upgraded {} to schema {VAULT_SCHEMA_V3} — {} secret(s) re-encrypted so \
                 each value is bound to the key name it is stored under. tsafe versions older \
                 than 3.0.0 cannot read this vault. A pinned pre-upgrade copy (exempt from snapshot rotation) was kept at {}; \
                 `tsafe snapshot restore --snapshot <path>` returns to the old format.",
                self.path.display(),
                self.file.secrets.len(),
                snap.display()
            );
        }
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
            let live_pt = decrypt_with_key_schedule(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &decode_b64(&entry.nonce)?,
                &decode_b64(&entry.ciphertext)?,
            )?;
            let (nonce, ct) = encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &live_pt,
                &secret_aad(name, SecretSlot::Live),
            )?;

            let mut history = Vec::with_capacity(entry.history.len());
            for h in &entry.history {
                let pt = decrypt_with_key_schedule(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &decode_b64(&h.nonce)?,
                    &decode_b64(&h.ciphertext)?,
                )?;
                let (hn, hct) = encrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &pt,
                    &secret_aad(name, SecretSlot::History(&h.updated_at)),
                )?;
                history.push(HistoryEntry {
                    nonce: encode_b64(&hn),
                    ciphertext: encode_b64(&hct),
                    updated_at: h.updated_at,
                });
            }

            rebound.insert(
                name.clone(),
                SecretEntry {
                    nonce: encode_b64(&nonce),
                    ciphertext: encode_b64(&ct),
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
                    tags: entry.tags.clone(),
                    history,
                },
            );
        }

        let count = rebound.len();
        self.file.secrets = rebound;
        self.binding = StructureBinding::Bound;
        warn!(
            path = %self.path.display(),
            secrets = count,
            "vault upgraded to schema v3 — ciphertexts are now bound to their key names"
        );
        Ok(())
    }

    fn integrity_tag(file: &VaultFile, key: &VaultKey) -> SafeResult<[u8; 32]> {
        let mac_key = derive_subkey(key, KeyPurpose::VaultStructure)?;
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

        Ok(mac_sha256(&mac_key, &buf))
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
                reason: "vault declares schema v3 but carries no integrity tag — the tag was \
                         stripped, or the schema field was edited to claim a format the file \
                         does not implement"
                    .into(),
            })?;
        let stored_bytes = decode_b64(stored).map_err(|_| SafeError::VaultTampered {
            reason: "integrity tag is not valid base64".into(),
        })?;
        let expected = Self::integrity_tag(file, key)?;
        if !ct_eq_bytes(&stored_bytes, &expected) {
            return Err(SafeError::VaultTampered {
                reason: "structure MAC mismatch — a secret name, value, tag, timestamp or the \
                         version counter was modified outside tsafe"
                    .into(),
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
        let pt = decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &decode_b64(&existing.nonce)?,
            &decode_b64(&existing.ciphertext)?,
            &secret_aad(key, SecretSlot::Live),
        )?;
        let (nonce, ct) = encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &pt,
            &secret_aad(key, SecretSlot::History(&existing.updated_at)),
        )?;
        Ok(HistoryEntry {
            nonce: encode_b64(&nonce),
            ciphertext: encode_b64(&ct),
            updated_at: existing.updated_at,
        })
    }

    pub fn structure_binding(&self) -> StructureBinding {
        self.binding
    }

    #[instrument(skip(self, value, tags, key))]
    pub fn set(&mut self, key: &str, value: &str, tags: HashMap<String, String>) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        validate_secret_key(key)?;
        if self.binding == StructureBinding::Legacy {
            self.upgrade_to_bound()?;
        }
        let (nonce, ct) = encrypt_with_key_schedule_aad(
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
                nonce: encode_b64(&nonce),
                ciphertext: encode_b64(&ct),
                created_at,
                updated_at: now,
                tags,
                history,
            },
        );
        self.file.updated_at = now;
        self.save()
    }

    #[instrument(skip(self, key))]
    pub fn get(&self, key: &str) -> SafeResult<Zeroizing<String>> {
        let entry = self
            .file
            .secrets
            .get(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        let nonce = decode_b64(&entry.nonce)?;
        let ct = decode_b64(&entry.ciphertext)?;
        let pt = decrypt_with_key_schedule_aad(
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
        let live_pt = decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &decode_b64(&entry.nonce)?,
            &decode_b64(&entry.ciphertext)?,
            &secret_aad(from_key, SecretSlot::Live),
        )?;
        let (nonce, ct) = encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &live_pt,
            &secret_aad(to_key, SecretSlot::Live),
        )?;

        let mut history = Vec::with_capacity(entry.history.len());
        for h in &entry.history {
            let pt = decrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &decode_b64(&h.nonce)?,
                &decode_b64(&h.ciphertext)?,
                &secret_aad(from_key, SecretSlot::History(&h.updated_at)),
            )?;
            let (hn, hct) = encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &pt,
                &secret_aad(to_key, SecretSlot::History(&h.updated_at)),
            )?;
            history.push(HistoryEntry {
                nonce: encode_b64(&hn),
                ciphertext: encode_b64(&hct),
                updated_at: h.updated_at,
            });
        }

        Ok(SecretEntry {
            nonce: encode_b64(&nonce),
            ciphertext: encode_b64(&ct),
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
                    reason: format!(
                        "version {version} does not exist for '{key}' (max {})",
                        entry.history.len()
                    ),
                })?;
        let h = &entry.history[hist_idx];
        let nonce = decode_b64(&h.nonce)?;
        let ct = decode_b64(&h.ciphertext)?;
        let pt = decrypt_with_key_schedule_aad(
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

        let (nonce, ct) = encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            target_value.as_bytes(),
            &self.aad_for(key, SecretSlot::Live),
        )?;
        let entry = self.file.secrets.get_mut(key).unwrap();
        entry.nonce = encode_b64(&nonce);
        entry.ciphertext = encode_b64(&ct);
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

    #[instrument(skip(self, new_password), fields(secret_count = self.file.secrets.len()))]
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
                let nonce = decode_b64(&h.nonce)?;
                let ct = decode_b64(&h.ciphertext)?;
                let pt = decrypt_with_key_schedule_aad(
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

        let new_salt = random_salt();
        let new_key = derive_key(
            new_password,
            &new_salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let new_cipher = default_vault_cipher();
        let new_key_schedule = KeySchedule::HkdfSha256V1;

        let now = Utc::now();
        let mut new_secrets = HashMap::with_capacity(all.len());
        for (key, value) in &all {
            let (nonce, ct) = encrypt_with_key_schedule_aad(
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
                    let (hn, hct) = encrypt_with_key_schedule_aad(
                        &new_key,
                        new_key_schedule,
                        KeyPurpose::SecretData,
                        new_cipher,
                        pt.as_bytes(),
                        &secret_aad(key, SecretSlot::History(updated_at)),
                    )?;
                    new_history.push(HistoryEntry {
                        nonce: encode_b64(&hn),
                        ciphertext: encode_b64(&hct),
                        updated_at: *updated_at,
                    });
                }
            }

            new_secrets.insert(
                key.clone(),
                SecretEntry {
                    nonce: encode_b64(&nonce),
                    ciphertext: encode_b64(&ct),
                    created_at,
                    updated_at: now,
                    tags: tags.clone(),
                    history: new_history,
                },
            );
        }

        let (ch_nonce, ch_ct) = encrypt_with_key_schedule(
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
            salt: encode_b64(&new_salt),
        };
        self.file.vault_challenge = VaultChallenge {
            nonce: encode_b64(&ch_nonce),
            ciphertext: encode_b64(&ch_ct),
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

    pub fn with_access_profile(mut self, access_profile: RbacProfile) -> Self {
        self.access_profile = access_profile;
        self
    }

    pub fn file(&self) -> &VaultFile {
        &self.file
    }

    pub fn ensure_write_allowed(&self) -> SafeResult<()> {
        self.access_profile.ensure_write_allowed()
    }

    fn profile_name(&self) -> Option<String> {
        Self::profile_name_from_path(&self.path)
    }

    fn profile_name_from_path(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }
}

// ── Whole-File Rollback Detection & Integrity Helpers ─────────────────────────

pub fn is_supported_schema(schema: &str) -> bool {
    StructureBinding::for_schema(schema).is_some()
}

pub fn schema_is_bound(schema: &str) -> bool {
    StructureBinding::for_schema(schema) == Some(StructureBinding::Bound)
}

pub fn seal_file(file: &mut VaultFile, key: &VaultKey) -> SafeResult<()> {
    file.schema = VAULT_SCHEMA_V3.to_string();
    file.version = file.version.saturating_add(1);
    file.integrity = Some(encode_b64(&Vault::integrity_tag(file, key)?));
    Ok(())
}

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
    let _ = write_owner_only(&path, highest.to_string().as_bytes());
}

fn detect_rollback(vault_path: &Path, file: &VaultFile) -> bool {
    match read_high_water_version(vault_path) {
        Some(seen) if file.version < seen => {
            warn!(
                path = %vault_path.display(),
                observed = file.version,
                highest_seen = seen,
                "vault version counter went backwards — the file may have been replaced with an \
                 older copy (or restored from a snapshot)"
            );
            true
        }
        _ => false,
    }
}

fn preserve_corrupt_vault(path: &Path) -> Option<PathBuf> {
    let damaged = std::fs::read(path).ok()?;
    if let Some(parent) = path.parent() {
        let prefix = format!("{}.corrupt.", path.file_name()?.to_str()?);
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if !name.starts_with(&prefix) {
                    continue;
                }
                if std::fs::read(entry.path()).is_ok_and(|existing| existing == damaged) {
                    return Some(entry.path());
                }
            }
        }
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let dest = path.with_extension(format!("vault.corrupt.{stamp}"));
    if dest.exists() {
        return Some(dest);
    }
    std::fs::write(&dest, &damaged).ok().map(|_| dest)
}

// ── Rotation Policies ─────────────────────────────────────────────────────────

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

// ── Snapshot Module ───────────────────────────────────────────────────────────

pub mod snapshot {
    use super::*;

    pub const DEFAULT_SNAPSHOT_KEEP: usize = 10;

    pub fn snapshot_dir_for_vault(vault_path: &Path, profile: &str) -> PathBuf {
        let parent = vault_path.parent().unwrap_or_else(|| Path::new(""));
        if parent.file_name().and_then(|s| s.to_str()) == Some(profile) {
            if let Some(grandparent) = parent.parent() {
                if grandparent.file_name().and_then(|s| s.to_str()) == Some("profiles") {
                    if let Some(great) = grandparent.parent() {
                        return great.join("snapshots").join(profile);
                    }
                }
            }
        }
        parent.join("snapshots").join(profile)
    }

    pub fn take(vault_path: &Path, profile: &str, keep: usize) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(vault_path, profile);
        std::fs::create_dir_all(&dir)?;
        let stamp = Utc::now().format("%Y%m%d-%H%M%S%f");
        let snap_path = dir.join(format!("{profile}-{stamp}.snap"));
        std::fs::copy(vault_path, &snap_path)?;

        if keep > 0 {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut snaps: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().is_some_and(|ext| ext == "snap")
                            && p.file_name()
                                .and_then(|s| s.to_str())
                                .map_or(false, |name| !name.contains("pre-upgrade"))
                    })
                    .collect();
                snaps.sort();
                if snaps.len() > keep {
                    let to_remove = snaps.len() - keep;
                    for snap in snaps.iter().take(to_remove) {
                        let _ = std::fs::remove_file(snap);
                    }
                }
            }
        }
        Ok(snap_path)
    }

    pub fn take_pre_upgrade(vault_path: &Path, profile: &str) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(vault_path, profile);
        std::fs::create_dir_all(&dir)?;
        let stamp = Utc::now().format("%Y%m%d-%H%M%S%f");
        let snap_path = dir.join(format!("{profile}-pre-upgrade-{stamp}.snap"));
        std::fs::copy(vault_path, &snap_path)?;
        Ok(snap_path)
    }

    pub fn restore(vault_path: &Path, snapshot_path: &Path) -> SafeResult<()> {
        std::fs::copy(snapshot_path, vault_path)?;
        Ok(())
    }
}

// ── NAPI Bindings for Vault & KvVault ─────────────────────────────────────────

#[napi(string_enum)]
pub enum VaultStructureBinding {
    Legacy,
    Bound,
}

impl From<StructureBinding> for VaultStructureBinding {
    fn from(b: StructureBinding) -> Self {
        match b {
            StructureBinding::Legacy => Self::Legacy,
            StructureBinding::Bound => Self::Bound,
        }
    }
}

#[napi(string_enum)]
pub enum VaultAccessProfile {
    ReadWrite,
    ReadOnly,
}

impl From<RbacProfile> for VaultAccessProfile {
    fn from(p: RbacProfile) -> Self {
        match p {
            RbacProfile::ReadWrite => Self::ReadWrite,
            RbacProfile::ReadOnly => Self::ReadOnly,
        }
    }
}

#[napi(object)]
pub struct VaultHistoryEntry {
    pub version: u32,
    pub timestamp: String,
}

#[napi(object)]
pub struct VaultRotationDue {
    pub key: String,
    pub overdue_days: i64,
    pub policy: String,
}

#[napi(js_name = "Vault")]
pub struct NapiVault {
    inner: Vault,
}

#[napi]
impl NapiVault {
    #[napi(factory)]
    pub fn create(path: String, password: Buffer) -> Result<Self> {
        let p = Path::new(&path);
        let vault = Vault::create(p, password.as_ref())?;
        Ok(Self { inner: vault })
    }

    #[napi(factory)]
    pub fn open(path: String, password: Buffer) -> Result<Self> {
        let p = Path::new(&path);
        let vault = Vault::open(p, password.as_ref())?;
        Ok(Self { inner: vault })
    }

    #[napi(factory)]
    pub fn open_read_only(path: String, password: Buffer) -> Result<Self> {
        let p = Path::new(&path);
        let vault = Vault::open_read_only(p, password.as_ref())?;
        Ok(Self { inner: vault })
    }

    #[napi(factory)]
    pub fn open_with_key(path: String, key: Buffer) -> Result<Self> {
        let key_bytes: [u8; 32] = key.as_ref().try_into().map_err(|_| {
            napi::Error::from_reason("key must be exactly 32 bytes".to_string())
        })?;
        let p = Path::new(&path);
        let vault = Vault::open_with_key(p, VaultKey::new(key_bytes.to_vec()))?;
        Ok(Self { inner: vault })
    }

    #[napi]
    pub fn is_team_vault(path: String) -> bool {
        Vault::is_team_vault(Path::new(&path))
    }

    #[napi]
    pub fn set(&mut self, key: String, value: String, tags: Option<HashMap<String, String>>) -> Result<()> {
        self.inner.set(&key, &value, tags.unwrap_or_default())?;
        Ok(())
    }

    #[napi]
    pub fn get(&self, key: String) -> Result<String> {
        let val = self.inner.get(&key)?;
        Ok((*val).clone())
    }

    #[napi]
    pub fn delete(&mut self, key: String) -> Result<()> {
        self.inner.delete(&key)?;
        Ok(())
    }

    #[napi]
    pub fn rename_key(&mut self, old_key: String, new_key: String, overwrite: Option<bool>) -> Result<()> {
        self.inner.rename_key(&old_key, &new_key, overwrite.unwrap_or(false))?;
        Ok(())
    }

    #[napi]
    pub fn list(&self) -> Vec<String> {
        self.inner.list().into_iter().map(String::from).collect()
    }

    #[napi]
    pub fn export_all(&self) -> Result<HashMap<String, String>> {
        Ok(self.inner.export_all()?)
    }

    #[napi]
    pub fn get_version(&self, key: String, version: u32) -> Result<String> {
        let val = self.inner.get_version(&key, version as usize)?;
        Ok((*val).clone())
    }

    #[napi]
    pub fn history(&self, key: String) -> Result<Vec<VaultHistoryEntry>> {
        let hist = self.inner.history(&key)?;
        Ok(hist
            .into_iter()
            .map(|(v, ts)| VaultHistoryEntry {
                version: v as u32,
                timestamp: ts.to_rfc3339(),
            })
            .collect())
    }

    #[napi]
    pub fn revert_to_version(&mut self, key: String, version: u32) -> Result<()> {
        self.inner.revert_to_version(&key, version as usize)?;
        Ok(())
    }

    #[napi]
    pub fn prune_history(&mut self, key: String, keep_n: u32) -> Result<()> {
        self.inner.prune_history(&key, keep_n as usize)?;
        Ok(())
    }

    #[napi]
    pub fn rotate(&mut self, new_password: Buffer) -> Result<()> {
        self.inner.rotate(new_password.as_ref())?;
        Ok(())
    }

    #[napi(getter)]
    pub fn rollback_suspected(&self) -> bool {
        self.inner.rollback_suspected()
    }

    #[napi(getter)]
    pub fn secret_count(&self) -> u32 {
        self.inner.secret_count() as u32
    }

    #[napi(getter)]
    pub fn path(&self) -> String {
        self.inner.path().to_string_lossy().into_owned()
    }

    #[napi(getter)]
    pub fn access_profile(&self) -> VaultAccessProfile {
        self.inner.access_profile().into()
    }

    #[napi(getter)]
    pub fn structure_binding(&self) -> VaultStructureBinding {
        self.inner.structure_binding().into()
    }

    #[napi]
    pub fn save(&mut self) -> Result<()> {
        self.inner.save()?;
        Ok(())
    }

    #[napi]
    pub fn rotation_due(&self) -> Vec<VaultRotationDue> {
        rotation_due(self.inner.file())
            .into_iter()
            .map(|(key, days, policy)| VaultRotationDue {
                key,
                overdue_days: days,
                policy,
            })
            .collect()
    }
}

#[napi]
pub fn vault_is_team_vault(path: String) -> bool {
    Vault::is_team_vault(Path::new(&path))
}

#[napi]
pub fn vault_lock_state(path: String) -> Option<String> {
    lock_state(Path::new(&path)).map(|st| match st {
        LockState::HeldBy(pid) => format!("HeldBy({pid})"),
        LockState::Unparseable => "Unparseable".to_string(),
    })
}

// ── KvVault (fjall-backed local KV store) ────────────────────────────────────

pub struct KvVault {
    keyspace: fjall::TxKeyspace,
    partition: fjall::TxPartition,
}

impl KvVault {
    pub fn open(path: &Path) -> SafeResult<Self> {
        let keyspace = fjall::Config::new(path).open_transactional().map_err(|e| {
            SafeError::InvalidVault {
                reason: format!("failed to open fjall kv store at {}: {e}", path.display()),
            }
        })?;
        let partition = keyspace
            .open_partition("vault_blob", Default::default())
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("failed to open fjall partition: {e}"),
            })?;
        Ok(Self { keyspace, partition })
    }

    pub fn put(&self, key: &str, value: &str) -> SafeResult<()> {
        self.partition
            .insert(key, value)
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("kv put error: {e}"),
            })?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> SafeResult<Option<String>> {
        let val = self
            .partition
            .get(key)
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("kv get error: {e}"),
            })?;
        match val {
            Some(bytes) => {
                let vec_bytes: Vec<u8> = bytes.as_ref().to_vec();
                let s = String::from_utf8(vec_bytes).map_err(|_| {
                    SafeError::InvalidVault {
                        reason: "stored kv value is not valid utf-8".into(),
                    }
                })?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    pub fn delete(&self, key: &str) -> SafeResult<()> {
        self.partition
            .remove(key)
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("kv delete error: {e}"),
            })?;
        Ok(())
    }

    pub fn list(&self, prefix: &str) -> SafeResult<Vec<String>> {
        let mut keys = Vec::new();
        for item in self.partition.prefix(prefix) {
            let (k, _) = item.map_err(|e| SafeError::InvalidVault {
                reason: format!("kv prefix scan error: {e}"),
            })?;
            let vec_k: Vec<u8> = k.as_ref().to_vec();
            if let Ok(s) = String::from_utf8(vec_k) {
                keys.push(s);
            }
        }
        keys.sort();
        Ok(keys)
    }
}

#[napi(js_name = "KvVault")]
pub struct NapiKvVault {
    inner: KvVault,
}

#[napi]
impl NapiKvVault {
    #[napi(factory)]
    pub fn open(path: String) -> Result<Self> {
        let p = Path::new(&path);
        let kv = KvVault::open(p)?;
        Ok(Self { inner: kv })
    }

    #[napi]
    pub fn put(&self, key: String, value: String) -> Result<()> {
        self.inner.put(&key, &value)?;
        Ok(())
    }

    #[napi]
    pub fn get(&self, key: String) -> Result<Option<String>> {
        Ok(self.inner.get(&key)?)
    }

    #[napi]
    pub fn delete(&self, key: String) -> Result<()> {
        self.inner.delete(&key)?;
        Ok(())
    }

    #[napi]
    pub fn list(&self, prefix: String) -> Result<Vec<String>> {
        Ok(self.inner.list(&prefix)?)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

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
    fn read_only_open_blocks_save_and_mutation_paths() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut writable = Vault::create(&path, pw()).unwrap();
        writable.set("K", "value", HashMap::new()).unwrap();
        drop(writable);

        let mut vault = Vault::open_read_only(&path, pw()).unwrap();
        assert_eq!(vault.access_profile(), RbacProfile::ReadOnly);
        assert_eq!(&*vault.get("K").unwrap(), "value");

        for result in [
            vault.save(),
            vault.set("NEW", "value", HashMap::new()),
            vault.delete("K"),
            vault.rename_key("K", "RENAMED", false),
            vault.rotate(b"new-password"),
        ] {
            match result {
                Err(SafeError::InvalidVault { reason }) => {
                    assert!(reason.contains("read_only"));
                }
                other => panic!("expected read-only write denial, got {other:?}"),
            }
        }
    }

    #[test]
    fn read_only_open_does_not_restore_missing_snapshot() {
        let dir = tempdir().unwrap();
        let profile_dir = dir.path().join("profiles").join("default");
        std::fs::create_dir_all(&profile_dir).unwrap();
        let path = profile_dir.join("vault.vault");
        let snapshots = dir.path().join("snapshots").join("default");
        std::fs::create_dir_all(&snapshots).unwrap();
        std::fs::write(
            snapshots.join("default-20260407-0000000000000.0000.snap"),
            "{}",
        )
        .unwrap();

        match Vault::open_read_only(&path, pw()) {
            Err(SafeError::VaultNotFound { .. }) => {}
            Ok(_) => panic!("expected read-only open to refuse snapshot restore"),
            Err(other) => panic!("expected VaultNotFound, got {other:?}"),
        }
        assert!(!path.exists(), "read-only open must not restore snapshots");
    }

    fn root_key_from_file(file: &VaultFile, password: &[u8]) -> VaultKey {
        let salt = decode_b64(&file.kdf.salt).unwrap();
        derive_key(
            password,
            &salt,
            file.kdf.m_cost,
            file.kdf.t_cost,
            file.kdf.p_cost,
        )
        .unwrap()
    }

    fn legacy_vault_file(password: &[u8], value: &str) -> VaultFile {
        let salt = random_salt();
        let key = derive_key(
            password,
            &salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )
        .unwrap();
        let now = Utc::now();
        let (ch_nonce, ch_ct) = encrypt(&key, VAULT_CHALLENGE_PLAINTEXT).unwrap();
        let (nonce, ciphertext) = encrypt(&key, value.as_bytes()).unwrap();
        let mut secrets = HashMap::new();
        secrets.insert(
            "LEGACY".into(),
            SecretEntry {
                nonce: encode_b64(&nonce),
                ciphertext: encode_b64(&ciphertext),
                created_at: now,
                updated_at: now,
                tags: HashMap::new(),
                history: Vec::new(),
            },
        );
        VaultFile {
            schema: VAULT_SCHEMA_V1.to_string(),
            kdf: KdfParams {
                algorithm: VAULT_KDF_ALGORITHM.to_string(),
                m_cost: VAULT_KDF_M_COST,
                t_cost: VAULT_KDF_T_COST,
                p_cost: VAULT_KDF_P_COST,
                salt: encode_b64(&salt),
            },
            cipher: CipherKind::XChaCha20Poly1305.as_str().to_string(),
            vault_challenge: VaultChallenge {
                nonce: encode_b64(&ch_nonce),
                ciphertext: encode_b64(&ch_ct),
            },
            created_at: now,
            updated_at: now,
            secrets,
            age_recipients: Vec::new(),
            wrapped_dek: None,
            version: 0,
            integrity: None,
        }
    }

    #[test]
    fn second_open_fails_while_lock_is_held() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let _v = Vault::create(&path, pw()).unwrap();

        match Vault::open(&path, pw()) {
            Err(SafeError::InvalidVault { reason }) => {
                assert!(reason.contains("vault is locked by another process"));
            }
            Ok(_) => panic!("expected lock error, got open vault"),
            Err(other) => panic!("expected lock error, got {other:?}"),
        }
    }

    #[test]
    fn wrong_password_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        v.set("K", "v", HashMap::new()).unwrap();
        drop(v);
        assert!(matches!(
            Vault::open(&path, b"wrong"),
            Err(SafeError::DecryptionFailed)
        ));
    }

    #[test]
    fn empty_vault_wrong_password_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        Vault::create(&path, pw()).unwrap();
        assert!(Vault::open(&path, b"wrong").is_err());
    }

    #[test]
    fn validate_secret_key_rejects_non_ascii() {
        assert!(validate_secret_key("café_KEY").is_err());
        assert!(validate_secret_key("emoji_🔑").is_err());
        assert!(validate_secret_key("K_日本").is_err());
    }

    #[test]
    fn set_get_roundtrip_unicode_secret_value() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, pw()).unwrap();
        let val = "snowman☃café日本語";
        v.set("UNICODE_VAL", val, HashMap::new()).unwrap();
        assert_eq!(&*v.get("UNICODE_VAL").unwrap(), val);
    }

    #[test]
    fn empty_master_password_vault_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut v = Vault::create(&path, b"").unwrap();
        v.set("K", "v", HashMap::new()).unwrap();
        drop(v);
        let v2 = Vault::open(&path, b"").unwrap();
        assert_eq!(&*v2.get("K").unwrap(), "v");
    }

    #[test]
    fn create_twice_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        Vault::create(&path, pw()).unwrap();
        assert!(matches!(
            Vault::create(&path, pw()),
            Err(SafeError::VaultAlreadyExists { .. })
        ));
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

    #[test]
    fn new_vault_uses_hkdf_scoped_keys_for_challenge_and_secret_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.vault");
        let mut vault = Vault::create(&path, pw()).unwrap();
        vault.set("SECRET", "value", HashMap::new()).unwrap();

        let root_key = root_key_from_file(vault.file(), pw());
        let challenge_nonce = decode_b64(&vault.file.vault_challenge.nonce).unwrap();
        let challenge_ct = decode_b64(&vault.file.vault_challenge.ciphertext).unwrap();
        assert!(matches!(
            decrypt_for_cipher(vault.cipher, &root_key, &challenge_nonce, &challenge_ct),
            Err(SafeError::DecryptionFailed)
        ));
        assert_eq!(
            decrypt_with_key_schedule(
                &root_key,
                KeySchedule::HkdfSha256V1,
                KeyPurpose::VaultChallenge,
                vault.cipher,
                &challenge_nonce,
                &challenge_ct
            )
            .unwrap(),
            VAULT_CHALLENGE_PLAINTEXT
        );

        let entry = &vault.file().secrets["SECRET"];
        let secret_nonce = decode_b64(&entry.nonce).unwrap();
        let secret_ct = decode_b64(&entry.ciphertext).unwrap();
        assert!(matches!(
            decrypt_for_cipher(vault.cipher, &root_key, &secret_nonce, &secret_ct),
            Err(SafeError::DecryptionFailed)
        ));
        assert!(matches!(
            decrypt_with_key_schedule(
                &root_key,
                KeySchedule::HkdfSha256V1,
                KeyPurpose::SecretData,
                vault.cipher,
                &secret_nonce,
                &secret_ct
            ),
            Err(SafeError::DecryptionFailed)
        ));
        assert_eq!(
            decrypt_with_key_schedule_aad(
                &root_key,
                KeySchedule::HkdfSha256V1,
                KeyPurpose::SecretData,
                vault.cipher,
                &secret_nonce,
                &secret_ct,
                &secret_aad("SECRET", SecretSlot::Live),
            )
            .unwrap(),
            b"value"
        );
    }

    #[test]
    fn open_legacy_vault_detects_legacy_schedule_and_keeps_writes_consistent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.vault");
        let file = legacy_vault_file(pw(), "legacy-value");
        std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

        let mut vault = Vault::open(&path, pw()).unwrap();
        assert_eq!(vault.key_schedule, KeySchedule::LegacyDirect);
        assert_eq!(vault.cipher, CipherKind::XChaCha20Poly1305);
        assert_eq!(&*vault.get("LEGACY").unwrap(), "legacy-value");

        vault
            .set("NEW_SECRET", "new-value", HashMap::new())
            .unwrap();
        assert_eq!(vault.key_schedule, KeySchedule::LegacyDirect);
        assert_eq!(vault.structure_binding(), StructureBinding::Bound);
        let root_key = root_key_from_file(vault.file(), pw());
        let entry = &vault.file().secrets["NEW_SECRET"];
        let nonce = decode_b64(&entry.nonce).unwrap();
        let ciphertext = decode_b64(&entry.ciphertext).unwrap();
        assert_eq!(
            decrypt_for_cipher_aad(
                vault.cipher,
                root_key.as_slice(),
                &nonce,
                &ciphertext,
                &secret_aad("NEW_SECRET", SecretSlot::Live)
            )
            .unwrap(),
            b"new-value"
        );
        assert_eq!(&*vault.get("LEGACY").unwrap(), "legacy-value");
    }

    #[test]
    fn upgrade_write_preserves_a_pre_upgrade_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.vault");
        let file = legacy_vault_file(pw(), "legacy-value");
        std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

        let mut vault = Vault::open(&path, pw()).unwrap();
        vault
            .set("NEW_SECRET", "new-value", HashMap::new())
            .unwrap();
        drop(vault);

        let on_disk: VaultFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.schema, VAULT_SCHEMA_V3);

        let snap_dir = snapshot::snapshot_dir_for_vault(&path, "legacy");
        let snaps: Vec<_> = std::fs::read_dir(&snap_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "snap"))
            .collect();
        assert_eq!(snaps.len(), 1, "exactly one pre-upgrade snapshot");
        let snap: VaultFile =
            serde_json::from_str(&std::fs::read_to_string(&snaps[0]).unwrap()).unwrap();
        assert_eq!(
            snap.schema, VAULT_SCHEMA_V1,
            "the snapshot must be the pre-upgrade copy, not the v3 rewrite"
        );
    }

    #[test]
    fn upgrade_write_is_refused_when_snapshot_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.vault");
        let file = legacy_vault_file(pw(), "legacy-value");
        let original = serde_json::to_string_pretty(&file).unwrap();
        std::fs::write(&path, &original).unwrap();
        std::fs::write(dir.path().join("snapshots"), b"in the way").unwrap();

        let mut vault = Vault::open(&path, pw()).unwrap();
        match vault.set("NEW_SECRET", "new-value", HashMap::new()) {
            Err(SafeError::MigrationFailed { reason }) => {
                assert!(reason.contains("pre-upgrade snapshot"), "reason: {reason}");
            }
            Ok(()) => panic!("expected the upgrade write to be refused"),
            Err(other) => panic!("expected MigrationFailed, got {other:?}"),
        }
        drop(vault);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a refused upgrade write must leave the legacy vault untouched"
        );
    }

    #[test]
    fn restoring_the_pre_upgrade_snapshot_downgrades_the_vault() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.vault");
        let file = legacy_vault_file(pw(), "legacy-value");
        std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

        {
            let mut vault = Vault::open(&path, pw()).unwrap();
            vault
                .set("NEW_SECRET", "new-value", HashMap::new())
                .unwrap();
        }

        let snap_dir = snapshot::snapshot_dir_for_vault(&path, "legacy");
        let snap = std::fs::read_dir(&snap_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "snap"))
            .expect("pre-upgrade snapshot exists");
        snapshot::restore(&path, &snap).unwrap();

        let on_disk: VaultFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.schema, VAULT_SCHEMA_V1,
            "restore must return the vault to its pre-upgrade format"
        );

        let vault = Vault::open(&path, pw()).unwrap();
        assert_eq!(&*vault.get("LEGACY").unwrap(), "legacy-value");
        assert!(vault.get("NEW_SECRET").is_err());
    }

    fn edit_vault_json(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        edit(&mut v);
        std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    #[test]
    fn swapping_ciphertexts_between_keys_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("swap.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("PROD_DB_PASSWORD", "PROD-VALUE-AAA", HashMap::new())
                .unwrap();
            v.set("DEV_DB_PASSWORD", "DEV-VALUE-BBB", HashMap::new())
                .unwrap();
        }

        edit_vault_json(&path, |v| {
            let secrets = v["secrets"].as_object_mut().unwrap();
            let prod = secrets["PROD_DB_PASSWORD"].clone();
            let dev = secrets["DEV_DB_PASSWORD"].clone();
            for (name, donor) in [("PROD_DB_PASSWORD", &dev), ("DEV_DB_PASSWORD", &prod)] {
                let e = secrets[name].as_object_mut().unwrap();
                e["nonce"] = donor["nonce"].clone();
                e["ciphertext"] = donor["ciphertext"].clone();
            }
        });

        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn swapped_ciphertexts_fail_aead_even_without_the_structure_tag() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("swap2.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("A_KEY", "value-a", HashMap::new()).unwrap();
            v.set("B_KEY", "value-b", HashMap::new()).unwrap();
        }
        edit_vault_json(&path, |v| {
            let secrets = v["secrets"].as_object_mut().unwrap();
            let a = secrets["A_KEY"].clone();
            let b = secrets["B_KEY"].clone();
            for (name, donor) in [("A_KEY", &b), ("B_KEY", &a)] {
                let e = secrets[name].as_object_mut().unwrap();
                e["nonce"] = donor["nonce"].clone();
                e["ciphertext"] = donor["ciphertext"].clone();
            }
            v["_schema"] = serde_json::Value::String("tsafe/vault/v1".into());
            v.as_object_mut().unwrap().remove("integrity");
        });

        let vault = Vault::open(&path, pw()).unwrap();
        assert!(matches!(
            vault.get("A_KEY"),
            Err(SafeError::DecryptionFailed)
        ));
        assert!(matches!(
            vault.get("B_KEY"),
            Err(SafeError::DecryptionFailed)
        ));
    }

    #[test]
    fn promoting_a_history_entry_into_the_live_slot_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rollback.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("TOKEN", "old-compromised-token", HashMap::new())
                .unwrap();
            v.set("TOKEN", "new-rotated-token", HashMap::new()).unwrap();
        }

        edit_vault_json(&path, |v| {
            let entry = v["secrets"]["TOKEN"].as_object_mut().unwrap();
            let hist = entry["history"][0].clone();
            entry["nonce"] = hist["nonce"].clone();
            entry["ciphertext"] = hist["ciphertext"].clone();
        });

        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn stripping_a_tag_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tags.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            let mut tags = HashMap::new();
            tags.insert("rotate_policy".to_string(), "90d".to_string());
            v.set("API_KEY", "secret", tags).unwrap();
        }
        edit_vault_json(&path, |v| {
            v["secrets"]["API_KEY"]["tags"] = serde_json::json!({});
        });
        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn backdating_a_timestamp_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dates.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("API_KEY", "secret", HashMap::new()).unwrap();
        }
        edit_vault_json(&path, |v| {
            v["secrets"]["API_KEY"]["updated_at"] =
                serde_json::Value::String("2020-01-01T00:00:00Z".into());
        });
        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn deleting_an_entry_out_of_band_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("delete.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("KEEP", "a", HashMap::new()).unwrap();
            v.set("REMOVE_ME", "b", HashMap::new()).unwrap();
        }
        edit_vault_json(&path, |v| {
            v["secrets"].as_object_mut().unwrap().remove("REMOVE_ME");
        });
        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn bound_schema_without_integrity_tag_is_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stripped.vault");
        {
            let mut v = Vault::create(&path, pw()).unwrap();
            v.set("KEY", "value", HashMap::new()).unwrap();
        }
        edit_vault_json(&path, |v| {
            v.as_object_mut().unwrap().remove("integrity");
        });
        assert!(matches!(
            Vault::open(&path, pw()).err(),
            Some(SafeError::VaultTampered { .. })
        ));
    }

    #[test]
    fn kv_vault_operations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv_store");
        let kv = KvVault::open(&path).unwrap();

        assert_eq!(kv.get("k1").unwrap(), None);

        kv.put("prefix_k1", "val1").unwrap();
        kv.put("prefix_k2", "val2").unwrap();
        kv.put("other_k3", "val3").unwrap();

        assert_eq!(kv.get("prefix_k1").unwrap().as_deref(), Some("val1"));
        assert_eq!(kv.get("prefix_k2").unwrap().as_deref(), Some("val2"));

        let prefix_list = kv.list("prefix_").unwrap();
        assert_eq!(prefix_list, vec!["prefix_k1", "prefix_k2"]);

        kv.delete("prefix_k1").unwrap();
        assert_eq!(kv.get("prefix_k1").unwrap(), None);

        let prefix_list2 = kv.list("prefix_").unwrap();
        assert_eq!(prefix_list2, vec!["prefix_k2"]);
    }
}
