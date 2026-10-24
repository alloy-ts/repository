use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

#[cfg(feature = "crypto")]
use crate::crypto::{
    CryptoError, CryptoResult, KeyPurpose, KeySchedule, SymmetricCryptoKey,
    aead_open, aead_seal, derive_aead_key, ed25519_seed_to_x25519_scalar, ed25519_to_x25519_public,
    NONCE_SIZE, ssh_ed25519_public_bytes, ssh_ed25519_seed, X25519PublicKey, X25519Secret,
};

// ---------------------------------------------------------------------------
// Enums & Structs for Vault NAPI Parity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[napi(string_enum)]
pub enum VaultStructureBinding {
    Legacy,
    Bound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[napi(string_enum)]
pub enum VaultAccessProfile {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultHistoryEntry {
    pub version: u32,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultRotationDue {
    pub key: String,
    pub overdue_days: i64,
    pub policy: String,
}

// ---------------------------------------------------------------------------
// KeyManager & IdentityManager
// ---------------------------------------------------------------------------

/// Stores and manages the repository's crypt key on disk.
#[napi]
pub struct KeyManager {
    git_dir: PathBuf,
}

#[napi]
impl KeyManager {
    /// Create a manager rooted at `git_dir` (the `.git` directory).
    #[napi(constructor)]
    pub fn new(git_dir: String) -> Self {
        Self {
            git_dir: PathBuf::from(git_dir),
        }
    }

    /// Path to the `<git-dir>/crypt` directory.
    #[napi]
    pub fn git_crypt_dir(&self) -> String {
        self.git_dir.join("crypt").to_string_lossy().into_owned()
    }

    /// Path to the default symmetric key file.
    #[napi]
    pub fn default_key_path(&self) -> String {
        if let Some(configured) = self.config_default_key_path() {
            return configured.to_string_lossy().into_owned();
        }
        self.git_dir
            .join("crypt")
            .join("keys")
            .join("default")
            .to_string_lossy()
            .into_owned()
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
    #[napi]
    pub fn init_dirs(&self) -> napi::Result<()> {
        let crypt_dir = self.git_dir.join("crypt");
        if crypt_dir.exists() {
            return Err(napi::Error::from_reason("Already initialized"));
        }
        std::fs::create_dir_all(&crypt_dir)?;
        std::fs::create_dir(crypt_dir.join("keys"))?;
        Ok(())
    }

    /// Whether the repository has been initialized for crypt.
    #[napi]
    pub fn is_initialized(&self) -> bool {
        self.git_dir.join("crypt").exists()
    }

    #[cfg(feature = "crypto")]
    pub fn load_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let key_path = PathBuf::from(self.default_key_path());
        if !key_path.exists() {
            return Err(CryptoError::KeyNotFound("default".into()));
        }
        let mut file = File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        SymmetricCryptoKey::from_bytes(&key_bytes)
    }

    #[cfg(feature = "crypto")]
    pub fn save_key(&self, key: &SymmetricCryptoKey) -> CryptoResult<()> {
        let key_path = PathBuf::from(self.default_key_path());
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

    #[cfg(feature = "crypto")]
    pub fn generate_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let key = SymmetricCryptoKey::generate_random();
        self.save_key(&key)?;
        Ok(key)
    }

    #[cfg(feature = "crypto")]
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

    #[cfg(feature = "crypto")]
    pub fn import_key(&self, input_path: &Path) -> CryptoResult<()> {
        let mut file = File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = SymmetricCryptoKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

#[napi]
pub struct IdentityManager;

#[napi]
impl IdentityManager {
    #[cfg(feature = "crypto")]
    pub fn encrypt_key_for_ssh_recipient(
        key: &SymmetricCryptoKey,
        recipient: &str,
    ) -> CryptoResult<Vec<u8>> {
        let recipient_pk = ssh_ed25519_public_bytes(recipient)?;
        let x25519_pk = ed25519_to_x25519_public(&recipient_pk)?;

        let mut ephemeral = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut ephemeral)
            .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;
        let esk = X25519Secret::from(ephemeral);
        let epk = X25519PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; NONCE_SIZE];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;

        let ciphertext = aead_seal(&aead_key, &nonce, key.as_bytes())?;

        let mut out = Vec::with_capacity(8 + 32 + NONCE_SIZE + ciphertext.len());
        out.extend_from_slice(b"RINGSSH1");
        out.extend_from_slice(epk.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    #[cfg(feature = "crypto")]
    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        passphrase: Option<&str>,
    ) -> CryptoResult<SymmetricCryptoKey> {
        if encrypted.len() < 8 + 32 + NONCE_SIZE {
            return Err(CryptoError::Ssh("envelope too short".into()));
        }
        if &encrypted[..8] != b"RINGSSH1" {
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

// ---------------------------------------------------------------------------
// Vault Format Data Types
// ---------------------------------------------------------------------------

const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct KdfParams {
    pub algorithm: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct HistoryEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
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
#[napi(object)]
pub struct VaultChallenge {
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
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
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultStatus {
    pub root: String,
    pub exists: bool,
    pub format_version: Option<u32>,
    pub secret_count: u32,
    pub is_team_vault: bool,
}

// ---------------------------------------------------------------------------
// Vault NAPI Implementation
// ---------------------------------------------------------------------------

#[napi]
pub struct Vault {
    path: PathBuf,
    file: VaultFile,
    access_profile: VaultAccessProfile,
    structure_binding: VaultStructureBinding,
    rollback_suspected: bool,
}

#[napi]
impl Vault {
    #[napi(factory)]
    pub fn open(path: String) -> napi::Result<Self> {
        let p = PathBuf::from(path);
        if !p.exists() {
            return Err(napi::Error::from_reason(format!(
                "Vault not found: {}",
                p.display()
            )));
        }
        let json = std::fs::read_to_string(&p)?;
        let file: VaultFile = serde_json::from_str(&json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid vault JSON: {e}")))?;

        let structure_binding = match file.schema.as_str() {
            VAULT_SCHEMA_V1 | VAULT_SCHEMA_V2 => VaultStructureBinding::Legacy,
            _ => VaultStructureBinding::Bound,
        };

        Ok(Self {
            path: p,
            file,
            access_profile: VaultAccessProfile::ReadWrite,
            structure_binding,
            rollback_suspected: false,
        })
    }

    #[napi(factory)]
    pub fn open_with_key(path: String, _key_buffer: Buffer) -> napi::Result<Self> {
        Self::open(path)
    }

    #[napi(factory)]
    pub fn status(path: String) -> napi::Result<VaultStatus> {
        let p = PathBuf::from(&path);
        let exists = p.exists();
        if !exists {
            return Ok(VaultStatus {
                root: path,
                exists: false,
                format_version: None,
                secret_count: 0,
                is_team_vault: false,
            });
        }
        let json = std::fs::read_to_string(&p)?;
        let file: VaultFile = serde_json::from_str(&json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid vault JSON: {e}")))?;

        let format_version = match file.schema.as_str() {
            VAULT_SCHEMA_V1 => Some(1),
            VAULT_SCHEMA_V2 => Some(2),
            VAULT_SCHEMA_V3 => Some(3),
            _ => None,
        };
        let is_team_vault = !file.age_recipients.is_empty() && file.wrapped_dek.is_some();
        Ok(VaultStatus {
            root: path,
            exists: true,
            format_version,
            secret_count: file.secrets.len() as u32,
            is_team_vault,
        })
    }

    #[napi]
    pub fn save(&mut self) -> napi::Result<()> {
        let json = serde_json::to_string_pretty(&self.file)
            .map_err(|e| napi::Error::from_reason(format!("Serialization error: {e}")))?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    #[napi]
    pub fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    #[napi]
    pub fn schema(&self) -> String {
        self.file.schema.clone()
    }

    #[napi]
    pub fn access_profile(&self) -> VaultAccessProfile {
        self.access_profile
    }

    #[napi]
    pub fn structure_binding(&self) -> VaultStructureBinding {
        self.structure_binding
    }

    #[napi]
    pub fn rollback_suspected(&self) -> bool {
        self.rollback_suspected
    }

    #[napi]
    pub fn secret_count(&self) -> u32 {
        self.file.secrets.len() as u32
    }

    #[napi]
    pub fn list(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.file.secrets.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[napi]
    pub fn is_team_vault(&self) -> bool {
        !self.file.age_recipients.is_empty() && self.file.wrapped_dek.is_some()
    }

    #[napi(js_name = "isTeamVault")]
    pub static fn is_team_vault_static(path: String) -> bool {
        let p = PathBuf::from(path);
        if let Ok(json) = std::fs::read_to_string(p) {
            if let Ok(file) = serde_json::from_str::<VaultFile>(&json) {
                return !file.age_recipients.is_empty() && file.wrapped_dek.is_some();
            }
        }
        false
    }

    #[napi]
    pub fn get_secret_entry(&self, key: String) -> Option<SecretEntry> {
        self.file.secrets.get(&key).cloned()
    }

    #[napi]
    pub fn get_version(&self, key: String, version: u32) -> napi::Result<String> {
        let entry = self
            .file
            .secrets
            .get(&key)
            .ok_or_else(|| napi::Error::from_reason(format!("Secret '{key}' not found")))?;
        if version == 0 {
            return Ok(entry.ciphertext.clone());
        }
        let hist_idx = (entry.history.len() as i64) - (version as i64);
        if hist_idx < 0 || hist_idx as usize >= entry.history.len() {
            return Err(napi::Error::from_reason(format!(
                "Version {version} does not exist for secret '{key}'"
            )));
        }
        Ok(entry.history[hist_idx as usize].ciphertext.clone())
    }

    #[napi]
    pub fn history(&self, key: String) -> napi::Result<Vec<VaultHistoryEntry>> {
        let entry = self
            .file
            .secrets
            .get(&key)
            .ok_or_else(|| napi::Error::from_reason(format!("Secret '{key}' not found")))?;

        let mut res = vec![VaultHistoryEntry {
            version: 0,
            timestamp: entry.updated_at.clone(),
        }];

        for (i, h) in entry.history.iter().rev().enumerate() {
            res.push(VaultHistoryEntry {
                version: (i + 1) as u32,
                timestamp: h.updated_at.clone(),
            });
        }

        Ok(res)
    }

    #[napi]
    pub fn revert_to_version(&mut self, key: String, version: u32) -> napi::Result<()> {
        if version == 0 {
            return Ok(());
        }
        let target_ct = self.get_version(key.clone(), version)?;
        let entry = self
            .file
            .secrets
            .get_mut(&key)
            .ok_or_else(|| napi::Error::from_reason(format!("Secret '{key}' not found")))?;

        let now = Utc::now().to_rfc3339();
        entry.history.push(HistoryEntry {
            nonce: entry.nonce.clone(),
            ciphertext: entry.ciphertext.clone(),
            updated_at: entry.updated_at.clone(),
        });
        entry.ciphertext = target_ct;
        entry.updated_at = now.clone();
        self.file.updated_at = now;
        self.save()
    }

    #[napi]
    pub fn prune_history(&mut self, key: String, keep_n: u32) -> napi::Result<()> {
        let entry = self
            .file
            .secrets
            .get_mut(&key)
            .ok_or_else(|| napi::Error::from_reason(format!("Secret '{key}' not found")))?;
        let keep = keep_n as usize;
        if entry.history.len() > keep {
            entry.history.drain(..entry.history.len() - keep);
        }
        self.file.updated_at = Utc::now().to_rfc3339();
        self.save()
    }

    #[napi]
    pub fn export_all(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (k, v) in &self.file.secrets {
            map.insert(k.clone(), v.ciphertext.clone());
        }
        map
    }
}

// ---------------------------------------------------------------------------
// KvVault (fjall-backed local KV store)
// ---------------------------------------------------------------------------

#[napi]
pub struct KvVault {
    _keyspace: fjall::Keyspace,
    partition: fjall::PartitionHandle,
}

#[napi]
impl KvVault {
    #[napi(factory)]
    pub fn open(path: String) -> napi::Result<Self> {
        let keyspace = fjall::Config::new(path)
            .open()
            .map_err(|e| napi::Error::from_reason(format!("Failed to open KvVault keyspace: {e}")))?;
        let partition = keyspace
            .open_partition("vault", Default::default())
            .map_err(|e| napi::Error::from_reason(format!("Failed to open KvVault partition: {e}")))?;
        Ok(Self {
            _keyspace: keyspace,
            partition,
        })
    }

    #[napi]
    pub fn put(&self, key: String, value: String) -> napi::Result<()> {
        self.partition
            .insert(key, value)
            .map_err(|e| napi::Error::from_reason(format!("KvVault put error: {e}")))?;
        Ok(())
    }

    #[napi]
    pub fn get(&self, key: String) -> napi::Result<Option<String>> {
        let slice = self
            .partition
            .get(key)
            .map_err(|e| napi::Error::from_reason(format!("KvVault get error: {e}")))?;
        match slice {
            Some(s) => Ok(Some(String::from_utf8_lossy(&s).into_owned())),
            None => Ok(None),
        }
    }

    #[napi]
    pub fn delete(&self, key: String) -> napi::Result<()> {
        self.partition
            .remove(key)
            .map_err(|e| napi::Error::from_reason(format!("KvVault delete error: {e}")))?;
        Ok(())
    }

    #[napi]
    pub fn list(&self, prefix: String) -> napi::Result<Vec<String>> {
        let mut result = Vec::new();
        for item in self.partition.prefix(prefix) {
            let (k, _) = item.map_err(|e| napi::Error::from_reason(format!("KvVault list error: {e}")))?;
            result.push(String::from_utf8_lossy(&k).into_owned());
        }
        result.sort();
        Ok(result)
    }
}
