use std::collections::HashMap;
use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use tracing::{instrument, warn};
use uuid::Uuid;
use zeroize::Zeroize;
use zeroize::Zeroizing;

// Re-export the crypto submodule's public types so the pasted `Vault` code
// (which refers to `VaultKey`/`CipherKind`/`KeySchedule`/`KeyPurpose` as bare
// names) resolves.
use crypto::{CipherKind, KeyPurpose, KeySchedule, VaultKey};

// ── error type ───────────────────────────────────────────────────────────────

/// Error type for all vault operations.
#[derive(Debug, thiserror::Error)]
pub enum SafeError {
    #[error("invalid vault: {reason}")]
    InvalidVault { reason: String },
    #[error("vault already exists: {path}")]
    VaultAlreadyExists { path: String },
    #[error("vault not found: {path}")]
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
    #[error("decryption failed")]
    DecryptionFailed,
    #[error("migration failed: {reason}")]
    MigrationFailed { reason: String },
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

pub type SafeResult<T> = Result<T, SafeError>;

impl From<SafeError> for napi::Error {
    fn from(e: SafeError) -> Self {
        napi::Error::new(napi::Status::GenericFailure, e.to_string())
    }
}

// ── access profiles ───────────────────────────────────────────────────────────

/// Read/write policy attached to a vault handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RbacProfile {
    ReadWrite,
    ReadOnly,
}

impl RbacProfile {
    pub fn ensure_write_allowed(&self) -> SafeResult<()> {
        if let RbacProfile::ReadOnly = self {
            return Err(SafeError::InvalidVault {
                reason: "read_only profile cannot perform writes".into(),
            });
        }
        Ok(())
    }
}

// ── crypto submodule (self-contained) ─────────────────────────────────────────

mod crypto {
    use super::{SafeError, SafeResult};
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use chacha20poly1305::aead;
    use chacha20poly1305::aead::{AeadInPlace, KeyInit};
    use chacha20poly1305::XChaCha20Poly1305;
    use hkdf::Hkdf;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    pub type VaultKey = [u8; 32];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CipherKind {
        XChaCha20Poly1305,
    }

    impl CipherKind {
        pub fn as_str(&self) -> &'static str {
            match self {
                CipherKind::XChaCha20Poly1305 => "XChaCha20Poly1305",
            }
        }
    }

    pub fn default_vault_cipher() -> CipherKind {
        CipherKind::XChaCha20Poly1305
    }

    pub fn parse_cipher_kind(s: &str) -> SafeResult<CipherKind> {
        match s {
            "XChaCha20Poly1305" => Ok(CipherKind::XChaCha20Poly1305),
            _ => Err(SafeError::InvalidVault {
                reason: format!("unsupported cipher: {s}"),
            }),
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum KeySchedule {
        LegacyDirect,
        HkdfSha256V1,
    }

    #[derive(Debug, Clone, Copy)]
    pub enum KeyPurpose {
        VaultChallenge,
        SecretData,
        VaultStructure,
    }

    impl KeyPurpose {
        fn info(&self) -> &'static [u8] {
            match self {
                KeyPurpose::VaultChallenge => b"tsafe/vault/challenge",
                KeyPurpose::SecretData => b"tsafe/vault/secret",
                KeyPurpose::VaultStructure => b"tsafe/vault/structure",
            }
        }
    }

    fn aead_encrypt(key: &[u8; 32], pt: &[u8], aad: &[u8]) -> SafeResult<(Vec<u8>, Vec<u8>)> {
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|_| SafeError::DecryptionFailed)?;
        let nonce: [u8; 24] = rand::random();
        let nonce_ga = chacha20poly1305::XNonce::from_slice(&nonce);
        let mut buf = pt.to_vec();
        cipher
            .encrypt_in_place(nonce_ga, aad, &mut buf)
            .map_err(|_| SafeError::DecryptionFailed)?;
        Ok((nonce.to_vec(), buf))
    }

    fn aead_decrypt(key: &[u8; 32], nonce: &[u8], ct: &[u8], aad: &[u8]) -> SafeResult<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|_| SafeError::DecryptionFailed)?;
        let nonce_ga = chacha20poly1305::XNonce::from_slice(nonce);
        let mut buf = ct.to_vec();
        cipher
            .decrypt_in_place(nonce_ga, aad, &mut buf)
            .map_err(|_| SafeError::DecryptionFailed)?;
        Ok(buf)
    }

    fn resolve_key(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
    ) -> SafeResult<[u8; 32]> {
        match schedule {
            KeySchedule::LegacyDirect => Ok(*key),
            KeySchedule::HkdfSha256V1 => derive_subkey(key, purpose),
        }
    }

    pub fn derive_key(
        password: &[u8],
        salt: &[u8; 16],
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    ) -> SafeResult<VaultKey> {
        let params = argon2::Params::new(m_cost, t_cost, p_cost, Some(32))
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("argon2 params: {e}"),
            })?;
        let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let mut out = [0u8; 32];
        argon2
            .hash_password_into(password, salt, &mut out)
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("argon2: {e}"),
            })?;
        Ok(out)
    }

    pub fn random_salt() -> [u8; 16] {
        rand::random()
    }

    pub fn derive_subkey(key: &VaultKey, purpose: KeyPurpose) -> SafeResult<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(None, key);
        let mut okm = [0u8; 32];
        hk.expand(purpose.info(), &mut okm)
            .map_err(|_| SafeError::InvalidVault {
                reason: "hkdf expand failed".into(),
            })?;
        Ok(okm)
    }

    pub fn mac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
        mac.update(data);
        let result = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    pub fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }

    pub fn encode_b64(b: &[u8]) -> String {
        B64.encode(b)
    }

    pub fn decode_b64(s: &str) -> SafeResult<Vec<u8>> {
        B64.decode(s).map_err(|e| SafeError::InvalidVault {
            reason: format!("base64: {e}"),
        })
    }

    pub fn encrypt_with_key_schedule(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        _cipher: CipherKind,
        pt: &[u8],
    ) -> SafeResult<(Vec<u8>, Vec<u8>)> {
        let k = resolve_key(key, schedule, purpose)?;
        aead_encrypt(&k, pt, &[])
    }

    pub fn encrypt_with_key_schedule_aad(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        _cipher: CipherKind,
        pt: &[u8],
        aad: &[u8],
    ) -> SafeResult<(Vec<u8>, Vec<u8>)> {
        let k = resolve_key(key, schedule, purpose)?;
        aead_encrypt(&k, pt, aad)
    }

    pub fn decrypt_with_key_schedule(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        _cipher: CipherKind,
        nonce: &[u8],
        ct: &[u8],
    ) -> SafeResult<Vec<u8>> {
        let k = resolve_key(key, schedule, purpose)?;
        aead_decrypt(&k, nonce, ct, &[])
    }

    pub fn decrypt_with_key_schedule_aad(
        key: &VaultKey,
        schedule: KeySchedule,
        purpose: KeyPurpose,
        _cipher: CipherKind,
        nonce: &[u8],
        ct: &[u8],
        aad: &[u8],
    ) -> SafeResult<Vec<u8>> {
        let k = resolve_key(key, schedule, purpose)?;
        aead_decrypt(&k, nonce, ct, aad)
    }

    // Legacy (no key schedule) helpers, used by tests and the legacy vault path.
    pub fn encrypt(key: &VaultKey, pt: &[u8]) -> SafeResult<(Vec<u8>, Vec<u8>)> {
        aead_encrypt(key, pt, &[])
    }

    pub fn decrypt_for_cipher(
        _cipher: CipherKind,
        key: &VaultKey,
        nonce: &[u8],
        ct: &[u8],
    ) -> SafeResult<Vec<u8>> {
        aead_decrypt(key, nonce, ct, &[])
    }

    pub fn decrypt_for_cipher_aad(
        _cipher: CipherKind,
        key: &VaultKey,
        nonce: &[u8],
        ct: &[u8],
        aad: &[u8],
    ) -> SafeResult<Vec<u8>> {
        aead_decrypt(key, nonce, ct, aad)
    }

    pub fn detect_key_schedule(
        key: &VaultKey,
        purpose: KeyPurpose,
        _cipher: CipherKind,
        nonce: &[u8],
        ct: &[u8],
        expected: &[u8],
    ) -> SafeResult<KeySchedule> {
        if let Ok(pt) = aead_decrypt(key, nonce, ct, &[]) {
            if pt == expected {
                return Ok(KeySchedule::LegacyDirect);
            }
        }
        let sub = derive_subkey(key, purpose)?;
        if let Ok(pt) = aead_decrypt(&sub, nonce, ct, &[]) {
            if pt == expected {
                return Ok(KeySchedule::HkdfSha256V1);
            }
        }
        Err(SafeError::DecryptionFailed)
    }
}

// ── snapshot submodule ────────────────────────────────────────────────────────

mod snapshot {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{SafeError, SafeResult};

    pub const DEFAULT_SNAPSHOT_KEEP: usize = 10;

    pub fn snapshot_dir_for_vault(vault_path: &Path, profile: &str) -> PathBuf {
        let parent = vault_path.parent().unwrap_or_else(|| Path::new("."));
        parent.join(format!(".snapshots/{profile}"))
    }

    pub fn take(path: &Path, profile: &str, keep: usize) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(path, profile);
        fs::create_dir_all(&dir)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("vault");
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S-%9f");
        let dest = dir.join(format!("{stem}-{ts}.snap"));
        fs::copy(path, &dest)?;
        prune(&dir, keep)?;
        Ok(dest)
    }

    pub fn take_pre_upgrade(path: &Path, profile: &str) -> SafeResult<PathBuf> {
        let dir = snapshot_dir_for_vault(path, profile);
        fs::create_dir_all(&dir)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("vault");
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S-%9f");
        let dest = dir.join(format!("{stem}-{ts}.pre-upgrade.snap"));
        fs::copy(path, &dest)?;
        Ok(dest)
    }

    pub fn restore(target: &Path, snap: &Path) -> SafeResult<()> {
        fs::copy(snap, target)?;
        Ok(())
    }

    fn prune(dir: &Path, keep: usize) -> SafeResult<()> {
        if keep == 0 {
            return Ok(());
        }
        let mut snaps: Vec<(SystemTime, PathBuf)> = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("snap") {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(mt) = meta.modified() {
                            snaps.push((mt, p));
                        }
                    }
                }
            }
        }
        snaps.sort_by(|a, b| a.0.cmp(&b.0)); // oldest first
        while snaps.len() > keep {
            let (_, old) = snaps.remove(0);
            let _ = fs::remove_file(old);
        }
        Ok(())
    }
}

// ── fsperm (owner-only file writes) ───────────────────────────────────────────

mod fsperm {
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    use super::SafeResult;

    pub fn write_owner_only(path: &Path, bytes: &[u8]) -> SafeResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?;
            f.write_all(bytes)?;
        }
        #[cfg(not(unix))]
        {
            fs::write(path, bytes)?;
        }
        Ok(())
    }

    pub fn set_owner_only(path: &Path) -> SafeResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(path, perms)?;
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
        Ok(())
    }
}

// ── Vault ─────────────────────────────────────────────────────────────────────

/// Original personal-vault format. Ciphertexts carry no associated data and the
/// file has no structural MAC, so everything outside each individual ciphertext
/// — which name a value is filed under, the set of names, tags, timestamps — is
/// unauthenticated. Still readable; upgraded to v3 on the next write.
const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
/// Team-vault format (age-wrapped DEK). Unbound, like v1.
const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
/// Current format. Each ciphertext is bound to the name and slot it is stored
/// under via AEAD associated data, and the whole file structure is covered by a
/// keyed MAC (`integrity`) over a canonical encoding. Applies to both personal
/// and team vaults — team membership is determined by `age_recipients` /
/// `wrapped_dek`, not by the schema string.
const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
/// Schema written for newly created vaults.
const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
const VAULT_KDF_ALGORITHM: &str = "argon2id";
// Argon2id cost parameters for vault key derivation (m_cost in KiB, t_cost passes,
// p_cost parallelism). 64 MiB / 3 passes / 1 lane is a conservative default.
const VAULT_KDF_M_COST: u32 = 64 * 1024;
const VAULT_KDF_T_COST: u32 = 3;
const VAULT_KDF_P_COST: u32 = 1;
/// Known plaintext used to verify the vault key on open.
pub(crate) const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

/// Whether a vault's contents are cryptographically bound to their structure.
///
/// The AEAD tag on a secret proves the *bytes* were not altered. It says
/// nothing about where those bytes are filed, because the name is a JSON object
/// key and the nonce/ciphertext pair is just a value. Without binding, an
/// attacker with file-write access and no password can swap two entries'
/// nonce+ciphertext pairs and silently redirect which credential an application
/// receives, or copy a `history` entry over the live slot to resurrect a
/// rotated-away token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureBinding {
    /// v1 / v2: no AAD, no structural MAC.
    Legacy,
    /// v3: per-entry AAD plus a whole-file MAC.
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

    /// Binding implied by a vault file's declared schema. Unknown schemas are
    /// treated as unbound; the open path rejects them separately.
    pub fn of_file(file: &VaultFile) -> Self {
        Self::for_schema(&file.schema).unwrap_or(Self::Legacy)
    }

    /// AAD for a slot under this binding — empty when unbound.
    pub fn aad(self, key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
        match self {
            Self::Legacy => Vec::new(),
            Self::Bound => secret_aad(key, slot),
        }
    }
}

/// Which slot a ciphertext occupies within a secret entry.
///
/// Bound into the AAD so a history ciphertext cannot be promoted into the live
/// slot (a targeted credential rollback) by editing the JSON. History slots are
/// identified by their timestamp rather than their index, because pruning
/// shifts indices but never rewrites timestamps.
#[derive(Debug, Clone, Copy)]
pub enum SecretSlot<'a> {
    Live,
    History(&'a DateTime<Utc>),
}

/// Build the AEAD associated data binding a ciphertext to its name and slot.
///
/// Authenticated but not encrypted, so it costs nothing in confidentiality and
/// makes the AEAD tag fail closed if the value is moved.
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

// Bounds for KDF parameters to prevent DoS (extreme memory) or downgrade (trivial brute-force).
const KDF_M_COST_MIN: u32 = 8_192; // 8 MiB — schema minimum
const KDF_M_COST_MAX: u32 = 131_072; // 128 MiB
const KDF_T_COST_MIN: u32 = 1;
const KDF_T_COST_MAX: u32 = 20;
const KDF_P_COST_MIN: u32 = 1;
const KDF_P_COST_MAX: u32 = 16;

/// Validate a secret key name.
///
/// Rules:
/// - 1–256 characters, ASCII only
/// - Must start with a letter or underscore
/// - Remaining chars: letters, digits, `_`, `-`, `.`, `/`
/// - No consecutive separators (`..`, `--`, `//`, `.-`, `/-`, etc.)
/// - Must not end with a separator
///
/// This allows namespaced keys like `myapp/DB_PASSWORD`, `github.com.password`,
/// `db-prod.PASSWORD`.
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
                reason: format!(
                    "secret key '{key}' contains invalid character '{c}' — use letters, digits, _, -, ., /"
                ),
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

// ── on-disk types (serde) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
}

/// A previous version of a secret's encrypted value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub updated_at: DateTime<Utc>,
}

/// How many previous versions to retain per secret.
pub const DEFAULT_HISTORY_KEEP: usize = 5;

/// An encrypted secret entry stored in the vault file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    /// Previous versions of this secret (oldest first). Backward-compatible via serde default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

/// The vault's key-verification challenge (encrypted known plaintext).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultChallenge {
    pub nonce: String,
    pub ciphertext: String,
}

/// The full vault file as written to disk.
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
    /// age X25519 recipient public keys (team vaults only). Backward-compatible via serde default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub age_recipients: Vec<String>,
    /// age-wrapped data encryption key (team vaults only). Backward-compatible via serde default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_dek: Option<String>,
    /// Monotonic write counter, incremented on every successful save.
    #[serde(default)]
    pub version: u64,
    /// Keyed MAC over the canonical encoding of everything above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

// ── in-memory vault (unlocked) ───────────────────────────────────────────────

/// An open, authenticated vault. `key` is held in memory and zeroed on drop.
/// Holds an advisory lock file (`<name>.vault.lock`) to prevent concurrent access.
/// The lock is acquired before any read or snapshot-restore work begins and is
/// only removed by the process that created it.
pub struct Vault {
    path: PathBuf,
    pub(crate) file: VaultFile,
    key: VaultKey,
    cipher: CipherKind,
    key_schedule: KeySchedule,
    /// Whether ciphertexts in this vault are bound to their name and slot, and
    /// whether the file carries a structural MAC. Legacy vaults are upgraded to
    /// [`StructureBinding::Bound`] on their next write.
    binding: StructureBinding,
    /// Set when the opened file's version counter is lower than the highest we
    /// have previously written for this path — see [`detect_rollback`].
    rollback_suspected: bool,
    access_profile: RbacProfile,
    /// Advisory lock owned by this vault handle.
    _lock: Option<LockGuard>,
}

struct LockGuard {
    path: PathBuf,
    contents: String,
    _file: std::fs::File,
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

/// Acquire an advisory lock for the vault at `path`.
/// Creates `<path>.vault.lock`; returns an ownership-aware guard.
/// New-format lock files can be recovered when their owning process is gone;
/// legacy opaque lock files remain manual-only to avoid accidental lock stealing.
fn acquire_lock(path: &Path) -> SafeResult<LockGuard> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..2 {
        let contents = serde_json::to_string(&LockFileContents::new())
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("lock serialize: {e}"),
            })?;
        match std::fs::OpenOptions::new()
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

/// PID of a live process currently holding the vault lock, if any.
///
/// For operations that mutate the vault file without opening it — snapshot
/// restore in particular — which would otherwise write straight over a vault
/// another process is in the middle of updating.
///
/// A lock whose owner has died, or one tsafe cannot parse, returns `None`: the
/// former is reclaimable and the latter is handled by the normal open path,
/// which reports it properly rather than blocking a recovery operation.
pub fn live_lock_holder(vault_path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(lock_path_for(vault_path)).ok()?;
    let lock: LockFileContents = serde_json::from_str(&contents).ok()?;
    process_is_running(lock.pid).then_some(lock.pid)
}

/// Why a vault lock file will block commands, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// A well-formed lock whose owning process is still alive.
    HeldBy(u32),
    /// A lock file tsafe cannot parse, or one it can parse but whose owner is
    /// gone and which therefore blocks until reclaimed.
    Unparseable,
}

/// Describe the lock file blocking `vault_path`, if any.
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
    // kill(pid, 0) performs an existence/permission probe without sending a signal.
    // SAFETY: kill(2) with signal 0 is a pure existence/permission probe — no signal
    // is delivered, no memory is read or written through pointers, and the FFI takes
    // only by-value scalar arguments.
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

    // SAFETY: OpenProcess takes by-value scalar arguments only.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();
        return code != windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32;
    }

    let mut exit_code = 0u32;
    // SAFETY: `handle` is non-null and has the required access.
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    // SAFETY: `handle` is the same owning HANDLE and has not been closed yet.
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
    // ── constructors ─────────────────────────────────────────────────────────

    /// Create a new empty vault at `path`, protected by `password`.
    #[instrument(skip(password, path))]
    pub fn create(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::create_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    /// Create a new vault with an explicit access profile.
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
        let salt = crypto::random_salt();
        let key = crypto::derive_key(
            password,
            &salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let now = Utc::now();
        let cipher = crypto::default_vault_cipher();
        let key_schedule = KeySchedule::HkdfSha256V1;

        let (ch_nonce, ch_ct) = crypto::encrypt_with_key_schedule(
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
                salt: crypto::encode_b64(&salt),
            },
            cipher: cipher.as_str().to_string(),
            vault_challenge: VaultChallenge {
                nonce: crypto::encode_b64(&ch_nonce),
                ciphertext: crypto::encode_b64(&ch_ct),
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

    /// Open and authenticate an existing vault.
    #[instrument(skip(password, path))]
    pub fn open(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    /// Open an existing vault in read-only mode.
    #[instrument(skip(password, path))]
    pub fn open_read_only(path: &Path, password: &[u8]) -> SafeResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadOnly)
    }

    /// Open and authenticate an existing vault with an explicit access profile.
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
        let cipher = crypto::parse_cipher_kind(&file.cipher)?;
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
        let salt = crypto::decode_b64(&file.kdf.salt)?;
        if salt.len() != 16 {
            return Err(SafeError::InvalidVault {
                reason: "KDF salt has wrong length".into(),
            });
        }
        let mut salt_arr = [0u8; 16];
        salt_arr.copy_from_slice(&salt);
        let key = crypto::derive_key(
            password,
            &salt_arr,
            file.kdf.m_cost,
            file.kdf.t_cost,
            file.kdf.p_cost,
        )?;

        let ch_nonce = crypto::decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = crypto::decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = crypto::detect_key_schedule(
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

    /// Open a vault with a pre-derived key (e.g. a DEK unwrapped from an age header).
    pub fn open_with_key(path: &Path, key: VaultKey) -> SafeResult<Self> {
        Self::open_with_key_with_access_profile(path, key, RbacProfile::ReadWrite)
    }

    /// Open a vault with a pre-derived key in read-only mode.
    pub fn open_with_key_read_only(path: &Path, key: VaultKey) -> SafeResult<Self> {
        Self::open_with_key_with_access_profile(path, key, RbacProfile::ReadOnly)
    }

    /// Open a vault with a pre-derived key and an explicit access profile.
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
        let cipher = crypto::parse_cipher_kind(&file.cipher)?;
        let ch_nonce = crypto::decode_b64(&file.vault_challenge.nonce)?;
        let ch_ct = crypto::decode_b64(&file.vault_challenge.ciphertext)?;
        let key_schedule = crypto::detect_key_schedule(
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

    /// Check if a vault file on disk is a team vault (has age recipients).
    pub fn is_team_vault(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<VaultFile>(&json).ok())
            .map(|f| !f.age_recipients.is_empty() && f.wrapped_dek.is_some())
            .unwrap_or(false)
    }

    // ── persistence ───────────────────────────────────────────────────────────

    /// Atomically write vault to disk (write-to-tmp then rename).
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
        self.file.integrity = Some(crypto::encode_b64(&Self::integrity_tag(
            &self.file, &self.key,
        )?));
        let json = serde_json::to_string_pretty(&self.file)
            .map_err(|e| SafeError::InvalidVault {
                reason: format!("serialize vault: {e}"),
            })?;
        let tmp = self.path.with_extension("vault.tmp");
        fsperm::write_owner_only(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        fsperm::set_owner_only(&self.path)?;
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

    /// Whether the file on disk at `path` declares a pre-v3 (unbound) schema.
    fn on_disk_schema_is_unbound(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<VaultFile>(&json).ok())
            .map(|f| !schema_is_bound(&f.schema))
            .unwrap_or(false)
    }

    /// Re-encrypt every ciphertext in a legacy vault with its name and slot
    /// bound in as associated data, and mark the handle as bound.
    fn upgrade_to_bound(&mut self) -> SafeResult<()> {
        let mut rebound: HashMap<String, SecretEntry> =
            HashMap::with_capacity(self.file.secrets.len());

        for (name, entry) in &self.file.secrets {
            let live_pt = crypto::decrypt_with_key_schedule(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &crypto::decode_b64(&entry.nonce)?,
                &crypto::decode_b64(&entry.ciphertext)?,
            )?;
            let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &live_pt,
                &secret_aad(name, SecretSlot::Live),
            )?;

            let mut history = Vec::with_capacity(entry.history.len());
            for h in &entry.history {
                let pt = crypto::decrypt_with_key_schedule(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &crypto::decode_b64(&h.nonce)?,
                    &crypto::decode_b64(&h.ciphertext)?,
                )?;
                let (hn, hct) = crypto::encrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &pt,
                    &secret_aad(name, SecretSlot::History(&h.updated_at)),
                )?;
                history.push(HistoryEntry {
                    nonce: crypto::encode_b64(&hn),
                    ciphertext: crypto::encode_b64(&hct),
                    updated_at: h.updated_at,
                });
            }

            rebound.insert(
                name.clone(),
                SecretEntry {
                    nonce: crypto::encode_b64(&nonce),
                    ciphertext: crypto::encode_b64(&ct),
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

    // ── structural integrity ─────────────────────────────────────────────────

    /// Compute the keyed MAC over everything the AEAD tags do not cover.
    fn integrity_tag(file: &VaultFile, key: &VaultKey) -> SafeResult<[u8; 32]> {
        let mac_key = crypto::derive_subkey(key, KeyPurpose::VaultStructure)?;
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

        Ok(crypto::mac_sha256(&mac_key, &buf))
    }

    /// Verify a bound vault's structural tag.
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
        let stored_bytes = crypto::decode_b64(stored).map_err(|_| SafeError::VaultTampered {
            reason: "integrity tag is not valid base64".into(),
        })?;
        let expected = Self::integrity_tag(file, key)?;
        if !crypto::ct_eq_bytes(&stored_bytes, &expected) {
            return Err(SafeError::VaultTampered {
                reason: "structure MAC mismatch — a secret name, value, tag, timestamp or the \
                         version counter was modified outside tsafe"
                    .into(),
            });
        }
        Ok(())
    }

    /// AAD for a secret slot, empty on legacy vaults that predate binding.
    fn aad_for(&self, key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
        match self.binding {
            StructureBinding::Legacy => Vec::new(),
            StructureBinding::Bound => secret_aad(key, slot),
        }
    }

    /// Move a live ciphertext into the history stack.
    fn demote_to_history(&self, key: &str, existing: &SecretEntry) -> SafeResult<HistoryEntry> {
        if self.binding == StructureBinding::Legacy {
            return Ok(HistoryEntry {
                nonce: existing.nonce.clone(),
                ciphertext: existing.ciphertext.clone(),
                updated_at: existing.updated_at,
            });
        }
        let pt = crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &crypto::decode_b64(&existing.nonce)?,
            &crypto::decode_b64(&existing.ciphertext)?,
            &secret_aad(key, SecretSlot::Live),
        )?;
        let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &pt,
            &secret_aad(key, SecretSlot::History(&existing.updated_at)),
        )?;
        Ok(HistoryEntry {
            nonce: crypto::encode_b64(&nonce),
            ciphertext: crypto::encode_b64(&ct),
            updated_at: existing.updated_at,
        })
    }

    /// Whether this vault's contents are bound to their structure.
    pub fn structure_binding(&self) -> StructureBinding {
        self.binding
    }

    // ── secret operations ────────────────────────────────────────────────────

    /// Insert or update a secret.
    #[instrument(skip(self, value, tags, key))]
    pub fn set(
        &mut self,
        key: &str,
        value: &str,
        tags: HashMap<String, String>,
    ) -> SafeResult<()> {
        self.ensure_write_allowed()?;
        validate_secret_key(key)?;
        if self.binding == StructureBinding::Legacy {
            self.upgrade_to_bound()?;
        }
        let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
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
                nonce: crypto::encode_b64(&nonce),
                ciphertext: crypto::encode_b64(&ct),
                created_at,
                updated_at: now,
                tags,
                history,
            },
        );
        self.file.updated_at = now;
        self.save()
    }

    /// Decrypt and return a secret value wrapped in `Zeroizing`.
    #[instrument(skip(self, key))]
    pub fn get(&self, key: &str) -> SafeResult<Zeroizing<String>> {
        let entry = self
            .file
            .secrets
            .get(key)
            .ok_or_else(|| SafeError::SecretNotFound {
                key: key.to_string(),
            })?;
        let nonce = crypto::decode_b64(&entry.nonce)?;
        let ct = crypto::decode_b64(&entry.ciphertext)?;
        let pt = crypto::decrypt_with_key_schedule_aad(
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

    /// Remove a secret.
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

    /// Rename / move a secret key within this vault.
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

    /// Re-encrypt an entry's live and history ciphertexts from `from_key`'s
    /// binding to `to_key`'s.
    fn rebind_entry(
        &self,
        from_key: &str,
        to_key: &str,
        entry: &SecretEntry,
    ) -> SafeResult<SecretEntry> {
        if self.binding == StructureBinding::Legacy {
            return Ok(entry.clone());
        }
        let live_pt = crypto::decrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &crypto::decode_b64(&entry.nonce)?,
            &crypto::decode_b64(&entry.ciphertext)?,
            &secret_aad(from_key, SecretSlot::Live),
        )?;
        let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            &live_pt,
            &secret_aad(to_key, SecretSlot::Live),
        )?;

        let mut history = Vec::with_capacity(entry.history.len());
        for h in &entry.history {
            let pt = crypto::decrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &crypto::decode_b64(&h.nonce)?,
                &crypto::decode_b64(&h.ciphertext)?,
                &secret_aad(from_key, SecretSlot::History(&h.updated_at)),
            )?;
            let (hn, hct) = crypto::encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &pt,
                &secret_aad(to_key, SecretSlot::History(&h.updated_at)),
            )?;
            history.push(HistoryEntry {
                nonce: crypto::encode_b64(&hn),
                ciphertext: crypto::encode_b64(&hct),
                updated_at: h.updated_at,
            });
        }

        Ok(SecretEntry {
            nonce: crypto::encode_b64(&nonce),
            ciphertext: crypto::encode_b64(&ct),
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            tags: entry.tags.clone(),
            history,
        })
    }

    /// List all secret key names in sorted order.
    pub fn list(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.file.secrets.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// Decrypt and return all secrets as a plain map.
    pub fn export_all(&self) -> SafeResult<HashMap<String, String>> {
        self.list()
            .iter()
            .map(|k| {
                let val = self.get(k)?;
                Ok((k.to_string(), (*val).clone()))
            })
            .collect()
    }

    // ── versioning ─────────────────────────────────────────────────────────

    /// Decrypt a specific version of a secret. Version 0 is current, 1 is most recent previous.
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
        let nonce = crypto::decode_b64(&h.nonce)?;
        let ct = crypto::decode_b64(&h.ciphertext)?;
        let pt = crypto::decrypt_with_key_schedule_aad(
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

    /// List version metadata for a key.
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

    /// Revert a secret to a previous version.
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

        let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            target_value.as_bytes(),
            &self.aad_for(key, SecretSlot::Live),
        )?;
        let entry = self.file.secrets.get_mut(key).unwrap();
        entry.nonce = crypto::encode_b64(&nonce);
        entry.ciphertext = crypto::encode_b64(&ct);
        entry.updated_at = now;
        self.file.updated_at = now;
        self.save()
    }

    /// Prune the version history for a secret to keep at most `keep_n` previous versions.
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

    // ── key rotation ─────────────────────────────────────────────────────────

    /// Re-encrypt all secrets under a new master password.
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
                let nonce = crypto::decode_b64(&h.nonce)?;
                let ct = crypto::decode_b64(&h.ciphertext)?;
                let pt = crypto::decrypt_with_key_schedule_aad(
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

        let new_salt = crypto::random_salt();
        let new_key = crypto::derive_key(
            new_password,
            &new_salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )?;
        let new_cipher = crypto::default_vault_cipher();
        let new_key_schedule = KeySchedule::HkdfSha256V1;

        let now = Utc::now();
        let mut new_secrets = HashMap::with_capacity(all.len());
        for (key, value) in &all {
            let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
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
                    let (hn, hct) = crypto::encrypt_with_key_schedule_aad(
                        &new_key,
                        new_key_schedule,
                        KeyPurpose::SecretData,
                        new_cipher,
                        pt.as_bytes(),
                        &secret_aad(key, SecretSlot::History(updated_at)),
                    )?;
                    new_history.push(HistoryEntry {
                        nonce: crypto::encode_b64(&hn),
                        ciphertext: crypto::encode_b64(&hct),
                        updated_at: *updated_at,
                    });
                }
            }

            new_secrets.insert(
                key.clone(),
                SecretEntry {
                    nonce: crypto::encode_b64(&nonce),
                    ciphertext: crypto::encode_b64(&ct),
                    created_at,
                    updated_at: now,
                    tags: tags.clone(),
                    history: new_history,
                },
            );
        }

        let (ch_nonce, ch_ct) = crypto::encrypt_with_key_schedule(
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
            salt: crypto::encode_b64(&new_salt),
        };
        self.file.vault_challenge = VaultChallenge {
            nonce: crypto::encode_b64(&ch_nonce),
            ciphertext: crypto::encode_b64(&ch_ct),
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

    /// Whether this vault's write counter went backwards relative to the
    /// highest value tsafe has previously written for this path.
    pub fn rollback_suspected(&self) -> bool {
        self.rollback_suspected
    }

    // ── accessors ────────────────────────────────────────────────────────────

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

// ── whole-file rollback detection ───────────────────────────────────────────

/// Whether tsafe can open a vault declaring this schema.
pub fn is_supported_schema(schema: &str) -> bool {
    StructureBinding::for_schema(schema).is_some()
}

/// Whether a vault declaring this schema binds its ciphertexts to their key
/// names and carries a structure MAC.
pub fn schema_is_bound(schema: &str) -> bool {
    StructureBinding::for_schema(schema) == Some(StructureBinding::Bound)
}

/// Re-seal a `VaultFile` that was mutated outside a [`Vault`] handle.
pub fn seal_file(file: &mut VaultFile, key: &VaultKey) -> SafeResult<()> {
    file.schema = VAULT_SCHEMA_V3.to_string();
    file.version = file.version.saturating_add(1);
    file.integrity = Some(crypto::encode_b64(&Vault::integrity_tag(file, key)?));
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
    let _ = fsperm::write_owner_only(&path, highest.to_string().as_bytes());
}

/// True if this file's version counter went backwards since we last wrote it.
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

/// Move a corrupt vault aside so it is not overwritten by later writes.
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

// ── rotation policies ───────────────────────────────────────────────────────

/// Parse a rotation policy duration string like `"90d"` into days.
pub fn parse_rotation_days(policy: &str) -> Option<i64> {
    let s = policy.trim();
    s.strip_suffix('d')
        .and_then(|prefix| prefix.parse::<i64>().ok())
        .filter(|&d| d > 0)
}

/// Return secrets that are overdue for rotation based on their `rotate_policy` tag.
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

// ── KeyManager (moved from crypto.rs) ────────────────────────────────────────

#[cfg(feature = "crypto")]
pub struct KeyManager {
    git_dir: PathBuf,
}

#[cfg(feature = "crypto")]
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

    /// Create the `crypt` + `crypt/keys` directories.
    pub fn init_dirs(&self) -> crate::crypto::CryptoResult<()> {
        let crypt_dir = self.git_crypt_dir();
        if crypt_dir.exists() {
            return Err(crate::crypto::CryptoError::AlreadyInitialized);
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
    pub fn generate_key(&self) -> crate::crypto::CryptoResult<crate::crypto::SymmetricCryptoKey> {
        let key = crate::crypto::SymmetricCryptoKey::generate_random();
        self.save_key(&key)?;
        Ok(key)
    }

    /// Persist `key` to the default path with `0600` permissions on Unix.
    pub fn save_key(
        &self,
        key: &crate::crypto::SymmetricCryptoKey,
    ) -> crate::crypto::CryptoResult<()> {
        let key_path = self.default_key_path();
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&key_path)?;
        file.write_all(key.as_bytes().as_ref())?;
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
    pub fn load_key(&self) -> crate::crypto::CryptoResult<crate::crypto::SymmetricCryptoKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(crate::crypto::CryptoError::KeyNotFound("default".into()));
        }
        let mut file = std::fs::File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        Ok(crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)?)
    }

    /// Export the default key to `output_path` (with `0600` on Unix).
    pub fn export_key(&self, output_path: &Path) -> crate::crypto::CryptoResult<()> {
        let key = self.load_key()?;
        let mut file = std::fs::File::create(output_path)?;
        file.write_all(key.as_bytes().as_ref())?;
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
    pub fn import_key(&self, input_path: &Path) -> crate::crypto::CryptoResult<()> {
        let mut file = std::fs::File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

// ── IdentityManager (moved from crypto.rs, ssh subsystem) ────────────────────

#[cfg(all(feature = "crypto", feature = "ssh"))]
pub struct IdentityManager;

#[cfg(all(feature = "crypto", feature = "ssh"))]
mod ssh_keys {
    use crate::crypto::{CryptoError, CryptoResult, SymmetricCryptoKey};
    use curve25519_dalek::edwards::CompressedEdwardsY;
    use curve25519_dalek::montgomery::MontgomeryPoint;
    use curve25519_dalek::x25519::{X25519PublicKey, X25519Secret};
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey};
    use ring::rand::SystemRandom;
    use sha2::{Digest, Sha256};

    use ssh_key::private::KeypairData;
    use ssh_key::public::KeyData;
    use ssh_key::{PrivateKey, PublicKey};

    pub const NONCE_SIZE: usize = 12;
    pub const MAGIC: &[u8; 8] = b"RINGSSH1";
    pub const VAULT_MAGIC: &[u8; 8] = b"KEYVAULT";

    pub fn ssh_ed25519_public_bytes(recipient: &str) -> CryptoResult<[u8; 32]> {
        if let Ok(pubkey) = PublicKey::from_openssh(recipient.trim()) {
            if let Ok(bytes) = ed25519_pub_from_ssh_public(&pubkey) {
                return Ok(bytes);
            }
        }
        let privkey = PrivateKey::from_openssh(recipient.trim())
            .map_err(|e| CryptoError::Ssh(format!("invalid SSH key: {e}")))?;
        ed25519_pub_from_ssh_private(&privkey)
    }

    pub fn ed25519_pub_from_ssh_public(pubkey: &PublicKey) -> CryptoResult<[u8; 32]> {
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

    pub fn ed25519_pub_from_ssh_private(privkey: &PrivateKey) -> CryptoResult<[u8; 32]> {
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

    pub fn ssh_ed25519_seed(identity_content: &str, passphrase: Option<&str>) -> CryptoResult<[u8; 32]> {
        let privkey = if let Some(pw) = passphrase {
            PrivateKey::from_openssh(identity_content.trim())
                .and_then(|k| k.decrypt(pw))
                .map_err(|e| CryptoError::Ssh(format!("failed to decrypt SSH identity: {e}")))
        } else {
            PrivateKey::from_openssh(identity_content.trim()).map_err(|e| {
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

    pub fn ed25519_to_x25519_public(edwards: &[u8; 32]) -> CryptoResult<X25519PublicKey> {
        let compressed = CompressedEdwardsY::from_slice(edwards)
            .map_err(|_| CryptoError::Ssh("invalid ed25519 public point".into()))?;
        let point = compressed
            .decompress()
            .ok_or_else(|| CryptoError::Ssh("invalid ed25519 public point".into()))?;
        let mont: MontgomeryPoint = point.to_montgomery();
        Ok(X25519PublicKey::from(mont.to_bytes()))
    }

    pub fn ed25519_seed_to_x25519_scalar(seed: &[u8; 32]) -> [u8; 32] {
        let hash = Sha512::digest(seed);
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&hash[..32]);
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        scalar
    }

    pub fn derive_aead_key(shared: &[u8]) -> CryptoResult<[u8; 32]> {
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

    pub fn aead_seal(
        key: &[u8; 32],
        nonce: &[u8; NONCE_SIZE],
        plaintext: &[u8],
    ) -> CryptoResult<Vec<u8>> {
        let unbound = UnboundKey::new(&ring::aead::AES_256_GCM, key)
            .map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
        let key = LessSafeKey::new(unbound);
        let nonce = Nonce::try_assume_unique_for_key(nonce)
            .map_err(|_| CryptoError::Ssh("invalid nonce".into()))?;
        let mut in_out = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| CryptoError::Ssh("encryption failed".into()))?;
        Ok(in_out)
    }

    pub fn aead_open(
        key: &[u8; 32],
        nonce: &[u8; NONCE_SIZE],
        ciphertext: &[u8],
    ) -> CryptoResult<Vec<u8>> {
        let unbound = UnboundKey::new(&ring::aead::AES_256_GCM, key)
            .map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
        let key = LessSafeKey::new(unbound);
        let nonce = Nonce::try_assume_unique_for_key(nonce)
            .map_err(|_| CryptoError::Ssh("invalid nonce".into()))?;
        let mut in_out = ciphertext.to_vec();
        let plain = key
            .open_in_place(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| CryptoError::Ssh("decryption failed (wrong key or tampered)".into()))?;
        Ok(plain.to_vec())
    }

    pub fn vault_seal(key: &[u8; 32], plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        let mut nonce = [0u8; NONCE_SIZE];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;
        let ct = aead_seal(key, &nonce, plaintext)?;
        let mut out = Vec::with_capacity(VAULT_MAGIC.len() + NONCE_SIZE + ct.len());
        out.extend_from_slice(VAULT_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn vault_open(key: &[u8; 32], blob: &[u8]) -> CryptoResult<Vec<u8>> {
        let min = VAULT_MAGIC.len() + NONCE_SIZE;
        if blob.len() < min || &blob[..VAULT_MAGIC.len()] != VAULT_MAGIC {
            return Err(CryptoError::Ssh("invalid key envelope".into()));
        }
        let (_, rest) = blob.split_at(VAULT_MAGIC.len());
        let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(nonce_bytes);
        aead_open(key, &nonce, ct)
    }
}

#[cfg(all(feature = "crypto", feature = "ssh"))]
impl IdentityManager {
    /// Encrypt the repo's symmetric key so only the holder of the matching
    /// ed25519 SSH private key can recover it.
    pub fn encrypt_key_for_ssh_recipient(
        key: &crate::crypto::SymmetricCryptoKey,
        recipient: &str,
    ) -> crate::crypto::CryptoResult<Vec<u8>> {
        use ssh_keys::{aead_seal, derive_aead_key, ed25519_to_x25519_public, ssh_ed25519_public_bytes, MAGIC, NONCE_SIZE};
        let recipient_pk = ssh_ed25519_public_bytes(recipient)?;
        let x25519_pk = ed25519_to_x25519_public(&recipient_pk)?;

        let mut ephemeral = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut ephemeral)
            .map_err(|_| crate::crypto::CryptoError::Ssh("RNG failure".into()))?;
        let esk = curve25519_dalek::x25519::X25519Secret::from(ephemeral);
        let epk = curve25519_dalek::x25519::X25519PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; NONCE_SIZE];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| crate::crypto::CryptoError::Ssh("RNG failure".into()))?;

        let ciphertext = aead_seal(&aead_key, &nonce, key.as_bytes())?;

        let mut out = Vec::with_capacity(8 + 32 + NONCE_SIZE + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(epk.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt an `IdentityManager` envelope using an OpenSSH ed25519 private key.
    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        passphrase: Option<&str>,
    ) -> crate::crypto::CryptoResult<crate::crypto::SymmetricCryptoKey> {
        use ssh_keys::{aead_open, derive_aead_key, ed25519_seed_to_x25519_scalar, ssh_ed25519_seed, MAGIC, NONCE_SIZE};
        if encrypted.len() < 8 + 32 + NONCE_SIZE {
            return Err(crate::crypto::CryptoError::Ssh("envelope too short".into()));
        }
        if &encrypted[..8] != MAGIC {
            return Err(crate::crypto::CryptoError::Ssh("invalid envelope magic".into()));
        }
        let (_, rest) = encrypted.split_at(8);
        let (epk_bytes, rest) = rest.split_at(32);
        let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);

        let mut epk_arr = [0u8; 32];
        epk_arr.copy_from_slice(epk_bytes);
        let epk = curve25519_dalek::x25519::X25519PublicKey::from(epk_arr);

        let seed = ssh_ed25519_seed(identity_content, passphrase)?;
        let scalar = ed25519_seed_to_x25519_scalar(&seed);
        let sk = curve25519_dalek::x25519::X25519Secret::from(scalar);
        let shared = sk.diffie_hellman(&epk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce_arr = [0u8; NONCE_SIZE];
        nonce_arr.copy_from_slice(nonce_bytes);

        let key_bytes = aead_open(&aead_key, &nonce_arr, ct)?;
        crate::crypto::SymmetricCryptoKey::from_bytes(&key_bytes)
    }
}

// ── napi bindings ─────────────────────────────────────────────────────────────

/// JS-facing handle to an open [`Vault`]. All methods lock internally so the
/// JS object can be shared.
#[napi(js_name = "Vault")]
pub struct VaultHandle {
    inner: Mutex<Vault>,
}

#[napi]
impl VaultHandle {
    /// Create a new vault at `path` encrypted with `password`.
    #[napi(constructor)]
    pub fn new(path: String, password: Buffer) -> napi::Result<Self> {
        let vault = Vault::create(Path::new(&path), password.as_ref())?;
        Ok(Self {
            inner: Mutex::new(vault),
        })
    }

    /// Open an existing vault. Pass `readOnly: true` for a read-only handle.
    #[napi]
    pub fn open(path: String, password: Buffer, read_only: Option<bool>) -> napi::Result<Self> {
        let vault = if read_only.unwrap_or(false) {
            Vault::open_read_only(Path::new(&path), password.as_ref())
        } else {
            Vault::open(Path::new(&path), password.as_ref())
        }?;
        Ok(Self {
            inner: Mutex::new(vault),
        })
    }

    /// Set (insert or update) a secret. `tags` is an optional string→string map.
    #[napi]
    pub fn set(
        &self,
        key: String,
        value: String,
        tags: Option<HashMap<String, String>>,
    ) -> napi::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .set(&key, &value, tags.unwrap_or_default())?;
        Ok(())
    }

    /// Get a secret's plaintext value.
    #[napi]
    pub fn get(&self, key: String) -> napi::Result<String> {
        let v = self.inner.lock().unwrap().get(&key)?;
        Ok((*v).clone())
    }

    /// Delete a secret.
    #[napi]
    pub fn delete(&self, key: String) -> napi::Result<()> {
        self.inner.lock().unwrap().delete(&key)?;
        Ok(())
    }

    /// List all secret keys (sorted).
    #[napi]
    pub fn list(&self) -> napi::Result<Vec<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .list()
            .iter()
            .map(|s| s.to_string())
            .collect())
    }

    /// Rename a secret.
    #[napi]
    pub fn rename_key(
        &self,
        old_key: String,
        new_key: String,
        overwrite: Option<bool>,
    ) -> napi::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .rename_key(&old_key, &new_key, overwrite.unwrap_or(false))?;
        Ok(())
    }

    /// Re-encrypt the whole vault under a new master password.
    #[napi]
    pub fn rotate(&self, new_password: Buffer) -> napi::Result<()> {
        self.inner.lock().unwrap().rotate(new_password.as_ref())?;
        Ok(())
    }

    /// Number of secrets in the vault.
    #[napi]
    pub fn secret_count(&self) -> napi::Result<u32> {
        Ok(self.inner.lock().unwrap().secret_count() as u32)
    }

    /// Persist buffered changes to disk.
    ///
    /// `set`, `delete`, and `rotate` only update the in-memory copy until this is
    /// called; without it, mutations are lost when the handle is dropped.
    #[napi]
    pub fn save(&self) -> napi::Result<()> {
        self.inner.lock().unwrap().save()?;
        Ok(())
    }

    /// Open a vault with a raw 32-byte key (e.g. a DEK unwrapped from an age header).
    #[napi]
    pub fn open_with_key(path: String, key: Buffer, read_only: Option<bool>) -> napi::Result<Self> {
        let arr: [u8; 32] = key.as_ref().try_into().map_err(|_| {
            napi::Error::new(napi::Status::InvalidArg, "key must be exactly 32 bytes")
        })?;
        let vault = if read_only.unwrap_or(false) {
            Vault::open_with_key_read_only(Path::new(&path), arr)
        } else {
            Vault::open_with_key(Path::new(&path), arr)
        }?;
        Ok(Self {
            inner: Mutex::new(vault),
        })
    }

    /// Decrypt a specific historical version of a secret (0 = current).
    #[napi]
    pub fn get_version(&self, key: String, version: u32) -> napi::Result<String> {
        let v = self
            .inner
            .lock()
            .unwrap()
            .get_version(&key, version as usize)?;
        Ok((*v).clone())
    }

    /// List version metadata for a secret (newest last).
    #[napi]
    pub fn history(&self, key: String) -> napi::Result<Vec<VaultHistoryEntry>> {
        let entries = self.inner.lock().unwrap().history(&key)?;
        Ok(entries
            .into_iter()
            .map(|(v, dt)| VaultHistoryEntry {
                version: v as u32,
                timestamp: dt.to_rfc3339(),
            })
            .collect())
    }

    /// Revert a secret to a previous version.
    #[napi]
    pub fn revert_to_version(&self, key: String, version: u32) -> napi::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .revert_to_version(&key, version as usize)?;
        Ok(())
    }

    /// Prune history for a secret down to `keep_n` previous versions.
    #[napi]
    pub fn prune_history(&self, key: String, keep_n: u32) -> napi::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .prune_history(&key, keep_n as usize)?;
        Ok(())
    }

    /// Decrypt and return all secrets as a plain key→value map.
    #[napi]
    pub fn export_all(&self) -> napi::Result<HashMap<String, String>> {
        Ok(self.inner.lock().unwrap().export_all()?)
    }

    /// On-disk path of this vault.
    #[napi]
    pub fn path(&self) -> napi::Result<String> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .path()
            .to_string_lossy()
            .into_owned())
    }

    /// Access profile of this handle.
    #[napi]
    pub fn access_profile(&self) -> napi::Result<VaultAccessProfile> {
        Ok(self.inner.lock().unwrap().access_profile().into())
    }

    /// Whether the vault's contents are bound to their structure (AAD + MAC).
    #[napi]
    pub fn structure_binding(&self) -> napi::Result<VaultStructureBinding> {
        Ok(self.inner.lock().unwrap().structure_binding().into())
    }

    /// Whether a rollback was detected when the vault was opened.
    #[napi]
    pub fn rollback_suspected(&self) -> napi::Result<bool> {
        Ok(self.inner.lock().unwrap().rollback_suspected())
    }

    /// Secrets whose `rotate_policy` tag is overdue for rotation.
    #[napi]
    pub fn rotation_due(&self) -> napi::Result<Vec<VaultRotationDue>> {
        let due = {
            let g = self.inner.lock().unwrap();
            rotation_due(g.file())
        };
        Ok(due
            .into_iter()
            .map(|(k, d, p)| VaultRotationDue {
                key: k,
                overdue_days: d,
                policy: p,
            })
            .collect())
    }

    /// Whether the vault file at `path` is a team vault (has age recipients).
    #[napi]
    pub fn is_team_vault(path: String) -> napi::Result<bool> {
        Ok(Vault::is_team_vault(Path::new(&path)))
    }

    /// Parse a rotation policy duration like `"90d"` into days.
    #[napi]
    pub fn parse_rotation_days(policy: String) -> Option<i64> {
        parse_rotation_days(&policy)
    }
}

#[cfg(feature = "crypto")]
#[napi(js_name = "KeyManager")]
pub struct KeyManagerHandle {
    inner: KeyManager,
}

#[cfg(feature = "crypto")]
#[napi]
impl KeyManagerHandle {
    #[napi(constructor)]
    pub fn new(git_dir: String) -> Self {
        Self {
            inner: KeyManager::new(git_dir),
        }
    }

    /// Generate a fresh random key and persist it; returns the raw key bytes.
    #[napi]
    pub fn generate_key(&self) -> napi::Result<Buffer> {
        let key = self.inner.generate_key()?;
        Ok(Buffer::from(key.as_bytes().to_vec()))
    }

    /// Load the default key from disk; returns the raw key bytes.
    #[napi]
    pub fn load_key(&self) -> napi::Result<Buffer> {
        let key = self.inner.load_key()?;
        Ok(Buffer::from(key.as_bytes().to_vec()))
    }

    /// Save raw key bytes as the default key.
    #[napi]
    pub fn save_key(&self, key: Buffer) -> napi::Result<()> {
        let key = crate::crypto::SymmetricCryptoKey::from_bytes(key.as_ref())?;
        self.inner.save_key(&key)?;
        Ok(())
    }

    /// Import a key file as the default key.
    #[napi]
    pub fn import_key(&self, input_path: String) -> napi::Result<()> {
        self.inner.save_key(&crate::crypto::SymmetricCryptoKey::from_bytes(
            std::fs::read(&input_path)
                .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?
                .as_slice(),
        )?)?;
        Ok(())
    }

    /// Path of the default key file.
    #[napi]
    pub fn default_key_path(&self) -> String {
        self.inner.default_key_path().to_string_lossy().into_owned()
    }
}

// ── NAPI result types for the Vault bindings ────────────────────────────────────

/// Whether a vault's contents are bound to their structure.
#[napi(string_enum)]
pub enum VaultStructureBinding {
    /// v1 / v2: no AAD, no structural MAC.
    Legacy,
    /// v3: per-entry AAD plus a whole-file MAC.
    Bound,
}

/// Access profile of a vault handle.
#[napi(string_enum)]
pub enum VaultAccessProfile {
    /// Writes allowed.
    ReadWrite,
    /// Writes rejected.
    ReadOnly,
}

/// One version of a secret's history.
#[napi(object)]
pub struct VaultHistoryEntry {
    /// Version index (0 = current).
    pub version: u32,
    /// RFC3339 timestamp of when this version was written.
    pub timestamp: String,
}

/// A secret whose rotation policy is overdue.
#[napi(object)]
pub struct VaultRotationDue {
    /// Secret key.
    pub key: String,
    /// Days past the policy's due date.
    pub overdue_days: i64,
    /// The `rotate_policy` tag value (e.g. `"90d"`).
    pub policy: String,
}

impl From<StructureBinding> for VaultStructureBinding {
    fn from(b: StructureBinding) -> Self {
        match b {
            StructureBinding::Legacy => VaultStructureBinding::Legacy,
            StructureBinding::Bound => VaultStructureBinding::Bound,
        }
    }
}

impl From<RbacProfile> for VaultAccessProfile {
    fn from(p: RbacProfile) -> Self {
        match p {
            RbacProfile::ReadWrite => VaultAccessProfile::ReadWrite,
            RbacProfile::ReadOnly => VaultAccessProfile::ReadOnly,
        }
    }
}

// ── KvVault: local embedded KV store (fjall) as an opaque blob backend ──────────

/// A synchronous, local key–value store backed by `fjall`. Values are opaque
/// blobs — e.g. a serialized encrypted `VaultFile` — and the caller wires the
/// bytes to/from a [`Vault`]. No network, no external services.
///
/// This is intentionally separate from the file-backed [`Vault`]; it only
/// provides durable put/get/delete/list of strings so the encrypted vault blob
/// can live in a local KV store instead of a single JSON file.
#[napi(js_name = "KvVault")]
pub struct KvVault {
    partition: fjall::Keyspace,
}

#[napi]
impl KvVault {
    /// Open (creating if needed) a KV store at `path` with a single partition.
    #[napi(constructor)]
    pub fn new(path: String) -> napi::Result<Self> {
        let db = fjall::Database::builder(&path)
            .open()
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        let partition = db
            .keyspace("vault", fjall::KeyspaceCreateOptions::default)
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        Ok(Self { partition })
    }

    /// Store `value` under `key` (opaque blob).
    #[napi]
    pub fn put(&self, key: String, value: String) -> napi::Result<()> {
        self.partition
            .insert(key, value)
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        Ok(())
    }

    /// Fetch the value stored under `key`, or `null` if absent.
    #[napi]
    pub fn get(&self, key: String) -> napi::Result<Option<String>> {
        let v = self
            .partition
            .get(key)
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        match v {
            Some(val) => {
                let s = String::from_utf8((*val).to_vec()).map_err(|_| {
                    napi::Error::new(napi::Status::GenericFailure, "value is not valid UTF-8")
                })?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// Remove `key`.
    #[napi]
    pub fn delete(&self, key: String) -> napi::Result<()> {
        self.partition
            .remove(key)
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        Ok(())
    }

    /// List keys sharing `prefix` (an empty prefix lists everything). Sorted.
    #[napi]
    pub fn list(&self, prefix: String) -> napi::Result<Vec<String>> {
        let mut keys = Vec::new();
        for guard in self.partition.prefix(prefix) {
            let guard = guard;
            let key = guard
                .key()
                .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
            let s = String::from_utf8((*key).to_vec()).map_err(|_| {
                napi::Error::new(napi::Status::GenericFailure, "key is not valid UTF-8")
            })?;
            keys.push(s);
        }
        keys.sort_unstable();
        Ok(keys)
    }
}

/// Tests for the self-contained `Vault` implementation.
#[cfg(test)]
mod tests {
    use super::*;
    use super::crypto;
    use super::snapshot;
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
        let salt = crypto::decode_b64(&file.kdf.salt).unwrap();
        let mut salt_arr = [0u8; 16];
        salt_arr.copy_from_slice(&salt);
        crypto::derive_key(
            password,
            &salt_arr,
            file.kdf.m_cost,
            file.kdf.t_cost,
            file.kdf.p_cost,
        )
        .unwrap()
    }

    fn legacy_vault_file(password: &[u8], value: &str) -> VaultFile {
        let salt = crypto::random_salt();
        let key = crypto::derive_key(
            password,
            &salt,
            VAULT_KDF_M_COST,
            VAULT_KDF_T_COST,
            VAULT_KDF_P_COST,
        )
        .unwrap();
        let now = Utc::now();
        let (ch_nonce, ch_ct) = crypto::encrypt(&key, VAULT_CHALLENGE_PLAINTEXT).unwrap();
        let (nonce, ciphertext) = crypto::encrypt(&key, value.as_bytes()).unwrap();
        let mut secrets = HashMap::new();
        secrets.insert(
            "LEGACY".into(),
            SecretEntry {
                nonce: crypto::encode_b64(&nonce),
                ciphertext: crypto::encode_b64(&ciphertext),
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
                salt: crypto::encode_b64(&salt),
            },
            cipher: CipherKind::XChaCha20Poly1305.as_str().to_string(),
            vault_challenge: VaultChallenge {
                nonce: crypto::encode_b64(&ch_nonce),
                ciphertext: crypto::encode_b64(&ch_ct),
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
        let challenge_nonce = crypto::decode_b64(&vault.file.vault_challenge.nonce).unwrap();
        let challenge_ct = crypto::decode_b64(&vault.file.vault_challenge.ciphertext).unwrap();
        assert!(matches!(
            crypto::decrypt_for_cipher(vault.cipher, &root_key, &challenge_nonce, &challenge_ct),
            Err(SafeError::DecryptionFailed)
        ));
        assert_eq!(
            crypto::decrypt_with_key_schedule(
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
        let secret_nonce = crypto::decode_b64(&entry.nonce).unwrap();
        let secret_ct = crypto::decode_b64(&entry.ciphertext).unwrap();
        assert!(matches!(
            crypto::decrypt_for_cipher(vault.cipher, &root_key, &secret_nonce, &secret_ct),
            Err(SafeError::DecryptionFailed)
        ));
        assert!(matches!(
            crypto::decrypt_with_key_schedule(
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
            crypto::decrypt_with_key_schedule_aad(
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
        let nonce = crypto::decode_b64(&entry.nonce).unwrap();
        let ciphertext = crypto::decode_b64(&entry.ciphertext).unwrap();
        assert_eq!(
            crypto::decrypt_for_cipher_aad(
                vault.cipher,
                &root_key,
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
        std::fs::write(dir.path().join(".snapshots"), b"in the way").unwrap();

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
            v.set("TOKEN", "new-rotated-token", HashMap::new())
                .unwrap();
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
}
