use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::crypto::{CryptoError, CryptoResult, SymmetricCryptoKey};

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

    /// Load the default key from disk.
    pub fn load_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(CryptoError::KeyNotFound("default".into()));
        }
        let mut file = File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        SymmetricCryptoKey::from_bytes(&key_bytes)
    }

    /// Export the default key to `output_path` (with `0600` on Unix).
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

    /// Import a key from `input_path` and save it as the default key.
    pub fn import_key(&self, input_path: &Path) -> CryptoResult<()> {
        let mut file = File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = SymmetricCryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Vault Schemas & Associated Data
// ---------------------------------------------------------------------------

pub const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
pub const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
pub const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
pub const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
pub const VAULT_KDF_ALGORITHM: &str = "argon2id";
pub(crate) const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

#[napi]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureBinding {
    Legacy,
    Bound,
}

impl StructureBinding {
    pub fn for_schema(schema: &str) -> Option<Self> {
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
    History(&'a str),
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
            aad.extend_from_slice(ts.as_bytes());
        }
    }
    aad
}

pub fn validate_secret_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(napi::Error::from_reason("secret key must not be empty"));
    }
    if key.len() > 256 {
        return Err(napi::Error::from_reason(format!(
            "secret key too long ({} chars, max 256)",
            key.len()
        )));
    }
    let mut chars = key.chars().peekable();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(napi::Error::from_reason(format!(
            "secret key '{key}' must start with a letter or underscore"
        )));
    }
    let is_sep = |c: char| c == '.' || c == '-' || c == '/';
    let mut prev = first;
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || is_sep(c)) {
            return Err(napi::Error::from_reason(format!(
                "secret key '{key}' contains invalid character '{c}'"
            )));
        }
        if is_sep(c) && is_sep(prev) {
            return Err(napi::Error::from_reason(format!(
                "secret key '{key}' has consecutive separators"
            )));
        }
        prev = c;
    }
    if is_sep(prev) {
        return Err(napi::Error::from_reason(format!(
            "secret key '{key}' must not end with a separator"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Serde Data Models
// ---------------------------------------------------------------------------

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
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: String,
    pub updated_at: String,
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
    pub created_at: String,
    pub updated_at: String,
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
// NAPI Types & Vault Surface
// ---------------------------------------------------------------------------

#[napi]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultWriteMode {
    Create,
    Replace,
    Upsert,
}

#[napi(object)]
#[derive(Clone, Debug, PartialEq)]
pub struct SecretRecord {
    pub name: String,
    pub created_at_ms: f64,
    pub updated_at_ms: f64,
    pub value_len: u32,
}

#[napi(object)]
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRecord {
    pub reference: String,
    pub kind: String,
    pub created_at_ms: f64,
    pub updated_at_ms: f64,
    pub value_len: u32,
}

#[napi(object)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRevision {
    pub vault_id: String,
    pub state_nonce_b64: String,
}

#[napi(object)]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultSnapshot {
    pub vault_id: String,
    pub revision: VaultRevision,
    pub created_at_ms: f64,
    pub format_version: u32,
    pub fields: Vec<FieldRecord>,
    pub legacy_secrets: Vec<SecretRecord>,
}

#[napi(object)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldKindChangeResult {
    pub reference: String,
    pub previous_kind: String,
    pub kind: String,
    pub changed: bool,
}

#[napi(object)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyConversionResult {
    pub secret_name: String,
    pub reference: String,
    pub kind: String,
}

#[napi(object)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevealResult {
    pub bytes_written: u32,
}

#[napi(object)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultStatus {
    pub root: String,
    pub exists: bool,
    pub format_version: Option<u32>,
    pub pending_transaction: bool,
}

#[napi]
pub struct Vault {
    path: PathBuf,
    file: VaultFile,
}

#[napi]
impl Vault {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        let path = PathBuf::from(path);
        let file = if path.exists() {
            let json = std::fs::read_to_string(&path)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
            serde_json::from_str(&json)
                .map_err(|e| napi::Error::from_reason(format!("parse vault file: {e}")))?
        } else {
            let now = Utc::now().to_rfc3339();
            VaultFile {
                schema: VAULT_SCHEMA.to_string(),
                kdf: KdfParams {
                    algorithm: VAULT_KDF_ALGORITHM.to_string(),
                    m_cost: 8192,
                    t_cost: 1,
                    p_cost: 1,
                    salt: String::new(),
                },
                cipher: "xchacha20poly1305".to_string(),
                vault_challenge: VaultChallenge {
                    nonce: String::new(),
                    ciphertext: String::new(),
                },
                created_at: now.clone(),
                updated_at: now,
                secrets: HashMap::new(),
                age_recipients: Vec::new(),
                wrapped_dek: None,
                version: 0,
                integrity: None,
            }
        };
        Ok(Vault { path, file })
    }

    #[napi(getter)]
    pub fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    #[napi]
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    #[napi]
    pub fn secret_count(&self) -> u32 {
        self.file.secrets.len() as u32
    }

    #[napi]
    pub fn list_secrets(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.file.secrets.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[napi]
    pub fn has_secret(&self, key: String) -> bool {
        self.file.secrets.contains_key(&key)
    }

    #[napi]
    pub fn save(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        }
        self.file.version = self.file.version.saturating_add(1);
        self.file.updated_at = Utc::now().to_rfc3339();
        let json = serde_json::to_string_pretty(&self.file)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        std::fs::write(&self.path, json).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }

    #[napi]
    pub fn remove_secret(&mut self, key: String) -> Result<bool> {
        let removed = self.file.secrets.remove(&key).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }
}

// ---------------------------------------------------------------------------
// Credential Secrets / Cipher NAPI Surface
// ---------------------------------------------------------------------------

#[napi(js_name = "CredentialSecrets")]
#[derive(Clone, Default)]
pub struct CredentialSecrets {
    fields: HashMap<String, Vec<u8>>,
}

#[napi]
impl CredentialSecrets {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            fields: HashMap::new(),
        }
    }

    #[napi]
    pub fn insert(&mut self, name: String, value: Buffer) -> Result<()> {
        validate_secret_key(&name)?;
        if value.is_empty() {
            return Err(napi::Error::from_reason("value cannot be empty"));
        }
        self.fields.insert(name, value.as_ref().to_vec());
        Ok(())
    }

    #[napi]
    pub fn get(&self, name: String) -> Option<Buffer> {
        self.fields.get(&name).map(|v| Buffer::from(v.clone()))
    }

    #[napi]
    pub fn field_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.fields.keys().cloned().collect();
        names.sort();
        names
    }

    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.fields.len() as u32
    }

    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[napi(object)]
pub struct EncryptedField {
    pub ciphertext: Buffer,
    pub nonce: Buffer,
}

#[napi(object)]
pub struct EncryptedCredentials {
    pub scope_id: String,
    pub fields: HashMap<String, EncryptedField>,
    pub key_version: i32,
}

#[napi(js_name = "CredentialCipher")]
pub struct CredentialCipher {
    active_version: i32,
}

#[napi]
impl CredentialCipher {
    #[napi(constructor)]
    pub fn new(active_version: i32) -> Self {
        Self { active_version }
    }

    #[napi(getter)]
    pub fn active_version(&self) -> i32 {
        self.active_version
    }
}
