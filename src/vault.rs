use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fjall::{Config, Keyspace, PartitionHandle};
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ssh")]
use ring::rand::{SecureRandom, SystemRandom};
#[cfg(feature = "ssh")]
use ring::signature::Ed25519KeyPair;
#[cfg(feature = "ssh")]
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519Secret};

use crate::crypto::{
    self, aead_open, aead_seal, derive_aead_key, ed25519_seed_to_x25519_scalar,
    ed25519_to_x25519_public, ssh_ed25519_seed, CipherKind, CryptoError, CryptoResult, KeyPurpose,
    KeySchedule, RbacProfile, SymmetricCryptoKey, VaultKey, NONCE_SIZE,
};
use crate::error::err_str;

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
// IdentityManager (SSH key sharing)
// ---------------------------------------------------------------------------

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
        SystemRandom::new()
            .fill(&mut ephemeral)
            .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;
        let esk = X25519Secret::from(ephemeral);
        let epk = X25519PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; NONCE_SIZE];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;

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
        let epk = X25519PublicKey::from(epk_arr);

        let seed = ssh_ed25519_seed(identity_content, passphrase)?;
        let scalar = ed25519_seed_to_x25519_scalar(&seed);
        let sk = X25519Secret::from(scalar);
        let shared = sk.diffie_hellman(&epk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce_arr = [0u8; NONCE_SIZE];
        nonce_arr.copy_from_slice(nonce_bytes);

        let key_bytes = aead_open(&aead_key, &nonce_arr, ct)?;
        SymmetricCryptoKey::from_bytes(&key_bytes)
    }
}

#[cfg(feature = "ssh")]
fn ssh_ed25519_public_bytes(recipient: &str) -> CryptoResult<[u8; 32]> {
    if let Ok(pubkey) = ssh_key::PublicKey::from_openssh(recipient.trim()) {
        if let Some(ed25519_key) = pubkey.key_data().ed25519() {
            return Ok(ed25519_key.0);
        }
    }
    if let Ok(privkey) = ssh_key::PrivateKey::from_openssh(recipient.trim()) {
        if let Some(ed25519_key) = privkey.public_key().key_data().ed25519() {
            return Ok(ed25519_key.0);
        }
    }
    Err(CryptoError::Ssh(
        "invalid ed25519 public key or unsupported format".into(),
    ))
}

// ---------------------------------------------------------------------------
// Vault schema & Types
// ---------------------------------------------------------------------------

const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
const VAULT_KDF_ALGORITHM: &str = "argon2id";
pub(crate) const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

const KDF_M_COST_MIN: u32 = 8_192;
const KDF_M_COST_MAX: u32 = 131_072;
const KDF_T_COST_MIN: u32 = 1;
const KDF_T_COST_MAX: u32 = 20;
const KDF_P_COST_MIN: u32 = 1;
const KDF_P_COST_MAX: u32 = 16;

const VAULT_KDF_M_COST: u32 = 65_536;
const VAULT_KDF_T_COST: u32 = 3;
const VAULT_KDF_P_COST: u32 = 4;

pub const DEFAULT_HISTORY_KEEP: usize = 5;

#[napi(string_enum)]
pub enum VaultStructureBinding {
    Legacy,
    Bound,
}

#[napi(string_enum)]
pub enum VaultAccessProfile {
    ReadWrite,
    ReadOnly,
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

pub fn validate_secret_key(key: &str) -> CryptoResult<()> {
    if key.is_empty() {
        return Err(CryptoError::Crypto("secret key must not be empty".into()));
    }
    if key.len() > 256 {
        return Err(CryptoError::Crypto(format!(
            "secret key too long ({} chars, max 256)",
            key.len()
        )));
    }
    let mut chars = key.chars().peekable();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(CryptoError::Crypto(format!(
            "secret key '{key}' must start with a letter or underscore"
        )));
    }
    let is_sep = |c: char| c == '.' || c == '-' || c == '/';
    let mut prev = first;
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || is_sep(c)) {
            return Err(CryptoError::Crypto(format!(
                "secret key '{key}' contains invalid character '{c}' — use letters, digits, _, -, ., /"
            )));
        }
        if is_sep(c) && is_sep(prev) {
            return Err(CryptoError::Crypto(format!(
                "secret key '{key}' has consecutive separators"
            )));
        }
        prev = c;
    }
    if is_sep(prev) {
        return Err(CryptoError::Crypto(format!(
            "secret key '{key}' must not end with a separator"
        )));
    }
    Ok(())
}

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

// ---------------------------------------------------------------------------
// LockGuard
// ---------------------------------------------------------------------------

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
            id: uuid::Uuid::new_v4().to_string(),
            pid: std::process::id(),
            created_at: Utc::now(),
        }
    }
}

fn acquire_lock(path: &Path) -> CryptoResult<LockGuard> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..2 {
        let contents = serde_json::to_string(&LockFileContents::new())
            .map_err(|e| CryptoError::Crypto(e.to_string()))?;
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
            Err(e) => return Err(CryptoError::Io(e)),
        }
    }
    Err(lock_held_error(&lock_path))
}

fn lock_path_for(vault_path: &Path) -> PathBuf {
    vault_path.with_extension("vault.lock")
}

fn lock_held_error(lock_path: &Path) -> CryptoError {
    CryptoError::Crypto(format!(
        "vault is locked by another process: {}",
        lock_path.display()
    ))
}

fn try_recover_dead_lock(lock_path: &Path) -> CryptoResult<bool> {
    let original_contents = match std::fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(CryptoError::Io(e)),
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
        Err(e) => return Err(CryptoError::Io(e)),
    };
    if current_contents != original_contents {
        return Ok(false);
    }

    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(CryptoError::Io(e)),
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

// ---------------------------------------------------------------------------
// Vault Struct & NAPI Bindings
// ---------------------------------------------------------------------------

#[napi]
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

impl Vault {
    pub fn create(path: &Path, password: &[u8]) -> CryptoResult<Self> {
        Self::create_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    pub fn create_with_access_profile(
        path: &Path,
        password: &[u8],
        access_profile: RbacProfile,
    ) -> CryptoResult<Self> {
        access_profile.ensure_write_allowed()?;
        if path.exists() {
            return Err(CryptoError::Crypto(format!(
                "vault already exists at path: {}",
                path.display()
            )));
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

    pub fn open(path: &Path, password: &[u8]) -> CryptoResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadWrite)
    }

    pub fn open_read_only(path: &Path, password: &[u8]) -> CryptoResult<Self> {
        Self::open_with_access_profile(path, password, RbacProfile::ReadOnly)
    }

    pub fn open_with_access_profile(
        path: &Path,
        password: &[u8],
        access_profile: RbacProfile,
    ) -> CryptoResult<Self> {
        let lock = acquire_lock(path)?;
        if !path.exists() {
            return Err(CryptoError::Crypto(format!(
                "vault not found at path: {}",
                path.display()
            )));
        }

        let json = std::fs::read_to_string(path)?;

        let file: VaultFile = serde_json::from_str(&json)
            .map_err(|e| CryptoError::Crypto(format!("vault JSON is invalid: {e}")))?;

        let binding = StructureBinding::for_schema(&file.schema).ok_or_else(|| {
            CryptoError::Crypto(format!("unknown vault schema: {}", file.schema))
        })?;
        let cipher = crypto::parse_cipher_kind(&file.cipher)?;
        if file.kdf.algorithm != VAULT_KDF_ALGORITHM {
            return Err(CryptoError::Crypto(format!(
                "unsupported KDF algorithm: '{}' (expected '{VAULT_KDF_ALGORITHM}')",
                file.kdf.algorithm
            )));
        }
        if file.kdf.m_cost < KDF_M_COST_MIN || file.kdf.m_cost > KDF_M_COST_MAX {
            return Err(CryptoError::Crypto(format!(
                "KDF m_cost {} is outside allowed range [{KDF_M_COST_MIN}, {KDF_M_COST_MAX}]",
                file.kdf.m_cost
            )));
        }
        if file.kdf.t_cost < KDF_T_COST_MIN || file.kdf.t_cost > KDF_T_COST_MAX {
            return Err(CryptoError::Crypto(format!(
                "KDF t_cost {} is outside allowed range [{KDF_T_COST_MIN}, {KDF_T_COST_MAX}]",
                file.kdf.t_cost
            )));
        }
        if file.kdf.p_cost < KDF_P_COST_MIN || file.kdf.p_cost > KDF_P_COST_MAX {
            return Err(CryptoError::Crypto(format!(
                "KDF p_cost {} is outside allowed range [{KDF_P_COST_MIN}, {KDF_P_COST_MAX}]",
                file.kdf.p_cost
            )));
        }
        let salt = crypto::decode_b64(&file.kdf.salt)?;
        let key = crypto::derive_key(
            password,
            &salt,
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

    pub fn open_with_key(path: &Path, key: VaultKey) -> CryptoResult<Self> {
        let lock = acquire_lock(path)?;
        if !path.exists() {
            return Err(CryptoError::Crypto(format!(
                "vault not found at path: {}",
                path.display()
            )));
        }
        let json = std::fs::read_to_string(path)?;
        let file: VaultFile = serde_json::from_str(&json)
            .map_err(|e| CryptoError::Crypto(format!("vault JSON parse error: {e}")))?;
        let binding = StructureBinding::for_schema(&file.schema).ok_or_else(|| {
            CryptoError::Crypto(format!("unknown schema: {}", file.schema))
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
            access_profile: RbacProfile::ReadWrite,
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

    pub fn save(&mut self) -> CryptoResult<()> {
        self.ensure_write_allowed()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
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
            .map_err(|e| CryptoError::Crypto(e.to_string()))?;
        let tmp = self.path.with_extension("vault.tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        record_high_water_version(&self.path, self.file.version);
        Ok(())
    }

    fn upgrade_to_bound(&mut self) -> CryptoResult<()> {
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

        self.file.secrets = rebound;
        self.binding = StructureBinding::Bound;
        Ok(())
    }

    fn integrity_tag(file: &VaultFile, key: &VaultKey) -> CryptoResult<[u8; 32]> {
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

    fn verify_integrity(
        file: &VaultFile,
        key: &VaultKey,
        binding: StructureBinding,
    ) -> CryptoResult<()> {
        if binding == StructureBinding::Legacy {
            return Ok(());
        }
        let stored = file.integrity.as_deref().ok_or_else(|| {
            CryptoError::Crypto("vault declares schema v3 but carries no integrity tag".into())
        })?;
        let stored_bytes = crypto::decode_b64(stored)?;
        let expected = Self::integrity_tag(file, key)?;
        if !crypto::ct_eq_bytes(&stored_bytes, &expected) {
            return Err(CryptoError::Crypto(
                "structure MAC mismatch — vault modified outside tsafe".into(),
            ));
        }
        Ok(())
    }

    fn aad_for(&self, key: &str, slot: SecretSlot<'_>) -> Vec<u8> {
        match self.binding {
            StructureBinding::Legacy => Vec::new(),
            StructureBinding::Bound => secret_aad(key, slot),
        }
    }

    pub fn ensure_write_allowed(&self) -> CryptoResult<()> {
        self.access_profile.ensure_write_allowed()
    }

    pub fn get_version(&self, key: &str, version: usize) -> CryptoResult<String> {
        if version == 0 {
            return self.get_secret(key);
        }
        let entry = self.file.secrets.get(key).ok_or_else(|| {
            CryptoError::Crypto(format!("secret key not found: {key}"))
        })?;
        let hist_idx = entry
            .history
            .len()
            .checked_sub(version)
            .ok_or_else(|| {
                CryptoError::Crypto(format!(
                    "version {version} does not exist for '{key}' (max {})",
                    entry.history.len()
                ))
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
        String::from_utf8(pt)
            .map_err(|_| CryptoError::Crypto("secret is not valid UTF-8".into()))
    }

    pub fn get_secret(&self, key: &str) -> CryptoResult<String> {
        let entry = self.file.secrets.get(key).ok_or_else(|| {
            CryptoError::Crypto(format!("secret key not found: {key}"))
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
        String::from_utf8(pt)
            .map_err(|_| CryptoError::Crypto("secret is not valid UTF-8".into()))
    }

    pub fn history(&self, key: &str) -> CryptoResult<Vec<VaultHistoryEntry>> {
        let entry = self.file.secrets.get(key).ok_or_else(|| {
            CryptoError::Crypto(format!("secret key not found: {key}"))
        })?;
        let mut versions = vec![VaultHistoryEntry {
            version: 0,
            timestamp: entry.updated_at.to_rfc3339(),
        }];
        for (i, h) in entry.history.iter().rev().enumerate() {
            versions.push(VaultHistoryEntry {
                version: (i + 1) as u32,
                timestamp: h.updated_at.to_rfc3339(),
            });
        }
        Ok(versions)
    }

    pub fn revert_to_version(&mut self, key: &str, version: usize) -> CryptoResult<()> {
        self.ensure_write_allowed()?;
        if version == 0 {
            return Ok(());
        }
        let target_value = self.get_version(key, version)?;
        let now = Utc::now();
        let demoted = {
            let entry = self.file.secrets.get(key).ok_or_else(|| {
                CryptoError::Crypto(format!("secret key not found: {key}"))
            })?;
            if self.binding == StructureBinding::Legacy {
                HistoryEntry {
                    nonce: entry.nonce.clone(),
                    ciphertext: entry.ciphertext.clone(),
                    updated_at: entry.updated_at,
                }
            } else {
                let pt = crypto::decrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &crypto::decode_b64(&entry.nonce)?,
                    &crypto::decode_b64(&entry.ciphertext)?,
                    &secret_aad(key, SecretSlot::Live),
                )?;
                let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &pt,
                    &secret_aad(key, SecretSlot::History(&entry.updated_at)),
                )?;
                HistoryEntry {
                    nonce: crypto::encode_b64(&nonce),
                    ciphertext: crypto::encode_b64(&ct),
                    updated_at: entry.updated_at,
                }
            }
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

    pub fn prune_history(&mut self, key: &str, keep_n: usize) -> CryptoResult<()> {
        self.ensure_write_allowed()?;
        let entry = self.file.secrets.get_mut(key).ok_or_else(|| {
            CryptoError::Crypto(format!("secret key not found: {key}"))
        })?;
        if entry.history.len() > keep_n {
            entry.history.drain(..entry.history.len() - keep_n);
        }
        self.file.updated_at = Utc::now();
        self.save()
    }

    pub fn export_all(&self) -> CryptoResult<HashMap<String, String>> {
        let mut map = HashMap::new();
        for key in self.file.secrets.keys() {
            let val = self.get_secret(key)?;
            map.insert(key.clone(), val);
        }
        Ok(map)
    }

    pub fn rotate(&mut self, new_password: &[u8]) -> CryptoResult<()> {
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
                let s = String::from_utf8(pt)
                    .map_err(|_| CryptoError::Crypto("history entry is not valid UTF-8".into()))?;
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

    pub fn rename_key(&mut self, old_key: &str, new_key: &str, overwrite: bool) -> CryptoResult<()> {
        self.ensure_write_allowed()?;
        validate_secret_key(new_key)?;
        if !self.file.secrets.contains_key(old_key) {
            return Err(CryptoError::Crypto(format!("secret key not found: {old_key}")));
        }
        if !overwrite && self.file.secrets.contains_key(new_key) {
            return Err(CryptoError::Crypto(format!(
                "secret key already exists: {new_key}"
            )));
        }
        let entry = self.file.secrets.remove(old_key).unwrap();
        let rebound = if self.binding == StructureBinding::Legacy {
            entry
        } else {
            let live_pt = crypto::decrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &crypto::decode_b64(&entry.nonce)?,
                &crypto::decode_b64(&entry.ciphertext)?,
                &secret_aad(old_key, SecretSlot::Live),
            )?;
            let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
                &self.key,
                self.key_schedule,
                KeyPurpose::SecretData,
                self.cipher,
                &live_pt,
                &secret_aad(new_key, SecretSlot::Live),
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
                    &secret_aad(old_key, SecretSlot::History(&h.updated_at)),
                )?;
                let (hn, hct) = crypto::encrypt_with_key_schedule_aad(
                    &self.key,
                    self.key_schedule,
                    KeyPurpose::SecretData,
                    self.cipher,
                    &pt,
                    &secret_aad(new_key, SecretSlot::History(&h.updated_at)),
                )?;
                history.push(HistoryEntry {
                    nonce: crypto::encode_b64(&hn),
                    ciphertext: crypto::encode_b64(&hct),
                    updated_at: h.updated_at,
                });
            }

            SecretEntry {
                nonce: crypto::encode_b64(&nonce),
                ciphertext: crypto::encode_b64(&ct),
                created_at: entry.created_at,
                updated_at: entry.updated_at,
                tags: entry.tags.clone(),
                history,
            }
        };
        self.file.secrets.insert(new_key.to_string(), rebound);
        self.file.updated_at = Utc::now();
        self.save()
    }
}

#[napi]
impl Vault {
    #[napi(factory)]
    pub fn create_vault(path: String, password: Buffer) -> Result<Self> {
        Self::create(Path::new(&path), password.as_ref()).map_err(|e| err_str("Vault.create", e))
    }

    #[napi(factory)]
    pub fn open_vault(path: String, password: Buffer) -> Result<Self> {
        Self::open(Path::new(&path), password.as_ref()).map_err(|e| err_str("Vault.open", e))
    }

    #[napi(factory)]
    pub fn open_vault_with_key(path: String, key: Buffer) -> Result<Self> {
        let key_bytes: [u8; 32] = key
            .as_ref()
            .try_into()
            .map_err(|_| err_str("Vault.openWithKey", "vault key must be 32 bytes"))?;
        Self::open_with_key(Path::new(&path), key_bytes)
            .map_err(|e| err_str("Vault.openWithKey", e))
    }

    #[napi(factory)]
    pub fn is_team_vault(path: String) -> bool {
        Self::is_team_vault(Path::new(&path))
    }

    #[napi]
    pub fn save(&mut self) -> Result<()> {
        self.save().map_err(|e| err_str("Vault.save", e))
    }

    #[napi]
    pub fn set(&mut self, key: String, value: String) -> Result<()> {
        self.ensure_write_allowed()
            .map_err(|e| err_str("Vault.set", e))?;
        validate_secret_key(&key).map_err(|e| err_str("Vault.set", e))?;
        if self.binding == StructureBinding::Legacy {
            self.upgrade_to_bound()
                .map_err(|e| err_str("Vault.set", e))?;
        }
        let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
            &self.key,
            self.key_schedule,
            KeyPurpose::SecretData,
            self.cipher,
            value.as_bytes(),
            &self.aad_for(&key, SecretSlot::Live),
        )
        .map_err(|e| err_str("Vault.set", e))?;

        let now = Utc::now();
        let (created_at, history, tags) = match self.file.secrets.get(&key) {
            Some(existing) => {
                let mut h = existing.history.clone();
                let demoted = if self.binding == StructureBinding::Legacy {
                    HistoryEntry {
                        nonce: existing.nonce.clone(),
                        ciphertext: existing.ciphertext.clone(),
                        updated_at: existing.updated_at,
                    }
                } else {
                    let pt = crypto::decrypt_with_key_schedule_aad(
                        &self.key,
                        self.key_schedule,
                        KeyPurpose::SecretData,
                        self.cipher,
                        &crypto::decode_b64(&existing.nonce).map_err(|e| err_str("Vault.set", e))?,
                        &crypto::decode_b64(&existing.ciphertext).map_err(|e| err_str("Vault.set", e))?,
                        &secret_aad(&key, SecretSlot::Live),
                    )
                    .map_err(|e| err_str("Vault.set", e))?;
                    let (nonce, ct) = crypto::encrypt_with_key_schedule_aad(
                        &self.key,
                        self.key_schedule,
                        KeyPurpose::SecretData,
                        self.cipher,
                        &pt,
                        &secret_aad(&key, SecretSlot::History(&existing.updated_at)),
                    )
                    .map_err(|e| err_str("Vault.set", e))?;
                    HistoryEntry {
                        nonce: crypto::encode_b64(&nonce),
                        ciphertext: crypto::encode_b64(&ct),
                        updated_at: existing.updated_at,
                    }
                };
                h.push(demoted);
                if h.len() > DEFAULT_HISTORY_KEEP {
                    h.drain(..h.len() - DEFAULT_HISTORY_KEEP);
                }
                (existing.created_at, h, existing.tags.clone())
            }
            None => (now, Vec::new(), HashMap::new()),
        };

        self.file.secrets.insert(
            key,
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
        self.save().map_err(|e| err_str("Vault.save", e))
    }

    #[napi]
    pub fn get(&self, key: String) -> Result<String> {
        self.get_secret(&key).map_err(|e| err_str("Vault.get", e))
    }

    #[napi]
    pub fn get_version(&self, key: String, version: u32) -> Result<String> {
        self.get_version(&key, version as usize)
            .map_err(|e| err_str("Vault.getVersion", e))
    }

    #[napi]
    pub fn history(&self, key: String) -> Result<Vec<VaultHistoryEntry>> {
        self.history(&key)
            .map_err(|e| err_str("Vault.history", e))
    }

    #[napi]
    pub fn revert_to_version(&mut self, key: String, version: u32) -> Result<()> {
        self.revert_to_version(&key, version as usize)
            .map_err(|e| err_str("Vault.revertToVersion", e))
    }

    #[napi]
    pub fn prune_history(&mut self, key: String, keep_n: u32) -> Result<()> {
        self.prune_history(&key, keep_n as usize)
            .map_err(|e| err_str("Vault.pruneHistory", e))
    }

    #[napi]
    pub fn delete(&mut self, key: String) -> Result<()> {
        self.ensure_write_allowed()
            .map_err(|e| err_str("Vault.delete", e))?;
        if !self.file.secrets.contains_key(&key) {
            return Err(err_str("Vault.delete", format!("secret key not found: {key}")));
        }
        self.file.secrets.remove(&key);
        self.file.updated_at = Utc::now();
        self.save().map_err(|e| err_str("Vault.save", e))
    }

    #[napi]
    pub fn rename_key(&mut self, old_key: String, new_key: String, overwrite: bool) -> Result<()> {
        self.rename_key(&old_key, &new_key, overwrite)
            .map_err(|e| err_str("Vault.renameKey", e))
    }

    #[napi]
    pub fn rotate(&mut self, new_password: Buffer) -> Result<()> {
        self.rotate(new_password.as_ref())
            .map_err(|e| err_str("Vault.rotate", e))
    }

    #[napi]
    pub fn list(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.file.secrets.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[napi]
    pub fn export_all(&self) -> Result<HashMap<String, String>> {
        self.export_all()
            .map_err(|e| err_str("Vault.exportAll", e))
    }

    #[napi]
    pub fn path(&self) -> String {
        self.path.to_string_lossy().to_string()
    }

    #[napi]
    pub fn access_profile(&self) -> VaultAccessProfile {
        self.access_profile.into()
    }

    #[napi]
    pub fn structure_binding(&self) -> VaultStructureBinding {
        self.binding.into()
    }

    #[napi]
    pub fn rollback_suspected(&self) -> bool {
        self.rollback_suspected
    }

    #[napi]
    pub fn rotation_due(&self) -> Vec<VaultRotationDue> {
        let now = Utc::now();
        let mut due = Vec::new();
        for (key, entry) in &self.file.secrets {
            if let Some(policy) = entry.tags.get("rotate_policy") {
                let s = policy.trim();
                if let Some(days) = s
                    .strip_suffix('d')
                    .and_then(|prefix| prefix.parse::<i64>().ok())
                    .filter(|&d| d > 0)
                {
                    let age = (now - entry.updated_at).num_days();
                    if age >= days {
                        due.push(VaultRotationDue {
                            key: key.clone(),
                            overdue_days: age - days,
                            policy: policy.clone(),
                        });
                    }
                }
            }
        }
        due.sort_by(|a, b| a.key.cmp(&b.key));
        due
    }
}

// ---------------------------------------------------------------------------
// KvVault NAPI Struct & Implementation
// ---------------------------------------------------------------------------

#[napi]
pub struct KvVault {
    _keyspace: Keyspace,
    partition: PartitionHandle,
}

#[napi]
impl KvVault {
    #[napi(factory)]
    pub fn open(path: String) -> Result<Self> {
        let keyspace = Config::new(Path::new(&path))
            .open()
            .map_err(|e| err_str("KvVault.open", e))?;
        let partition = keyspace
            .open_partition("default", Default::default())
            .map_err(|e| err_str("KvVault.open", e))?;
        Ok(Self {
            _keyspace: keyspace,
            partition,
        })
    }

    #[napi]
    pub fn put(&self, key: String, value: String) -> Result<()> {
        self.partition
            .insert(key.as_bytes(), value.as_bytes())
            .map_err(|e| err_str("KvVault.put", e))
    }

    #[napi]
    pub fn get(&self, key: String) -> Result<Option<String>> {
        let res = self
            .partition
            .get(key.as_bytes())
            .map_err(|e| err_str("KvVault.get", e))?;
        match res {
            Some(slice) => {
                let s = String::from_utf8(slice.to_vec())
                    .map_err(|_| err_str("KvVault.get", "stored value is not valid UTF-8"))?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    #[napi]
    pub fn delete(&self, key: String) -> Result<()> {
        self.partition
            .remove(key.as_bytes())
            .map_err(|e| err_str("KvVault.delete", e))
    }

    #[napi]
    pub fn list(&self, prefix: String) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let range = self.partition.prefix(prefix.as_bytes());
        for item in range {
            let (k, _) = item.map_err(|e| err_str("KvVault.list", e))?;
            let key_str = String::from_utf8(k.to_vec())
                .map_err(|_| err_str("KvVault.list", "stored key is not valid UTF-8"))?;
            keys.push(key_str);
        }
        keys.sort();
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// Rollback & High-Water Mark Helpers
// ---------------------------------------------------------------------------

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
    let _ = std::fs::write(&path, highest.to_string().as_bytes());
}

fn detect_rollback(vault_path: &Path, file: &VaultFile) -> bool {
    match read_high_water_version(vault_path) {
        Some(seen) if file.version < seen => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_generate_and_load_key_manager() {
        let temp = TempDir::new().unwrap();
        let km = KeyManager::new(temp.path());
        assert!(!km.is_initialized());

        km.init_dirs().unwrap();
        assert!(km.is_initialized());
        assert!(km.init_dirs().is_err());

        let key = km.generate_key().unwrap();
        let loaded = km.load_key().unwrap();
        assert_eq!(key.as_bytes(), loaded.as_bytes());
    }

    #[test]
    fn vault_create_set_get() {
        let temp = TempDir::new().unwrap();
        let vault_path = temp.path().join("test.vault");
        let password = b"master_password";

        let mut vault = Vault::create(&vault_path, password).unwrap();
        vault.set("SECRET_KEY".to_string(), "secret_val".to_string()).unwrap();

        let loaded = Vault::open(&vault_path, password).unwrap();
        assert_eq!(loaded.get("SECRET_KEY".to_string()).unwrap(), "secret_val");
        assert_eq!(loaded.list(), vec!["SECRET_KEY".to_string()]);
    }

    #[test]
    fn kv_vault_put_get_list_delete() {
        let temp = TempDir::new().unwrap();
        let kv = KvVault::open(temp.path().to_str().unwrap().to_string()).unwrap();
        kv.put("app/db".to_string(), "pass1".to_string()).unwrap();
        kv.put("app/api".to_string(), "pass2".to_string()).unwrap();
        assert_eq!(kv.get("app/db".to_string()).unwrap(), Some("pass1".to_string()));
        assert_eq!(kv.list("app/".to_string()).unwrap(), vec!["app/api".to_string(), "app/db".to_string()]);
        kv.delete("app/db".to_string()).unwrap();
        assert_eq!(kv.get("app/db".to_string()).unwrap(), None);
    }
}
