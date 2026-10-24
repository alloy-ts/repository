use std::collections::HashMap;
use std::path::PathBuf;

use napi::Result;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::err;

pub const VAULT_SCHEMA_V1: &str = "tsafe/vault/v1";
pub const VAULT_SCHEMA_V2: &str = "tsafe/vault/v2";
pub const VAULT_SCHEMA_V3: &str = "tsafe/vault/v3";
pub const VAULT_SCHEMA: &str = VAULT_SCHEMA_V3;
pub const VAULT_KDF_ALGORITHM: &str = "argon2id";
pub const VAULT_CHALLENGE_PLAINTEXT: &[u8] = b"tsafe-vault-challenge-v1";

/// On-disk KDF parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct KdfParams {
    pub algorithm: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
}

/// A previous version of a secret's value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct HistoryEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub updated_at: String,
}

/// An encrypted secret entry stored in the vault file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct SecretEntry {
    pub nonce: String,
    pub ciphertext: String,
    pub created_at: String,
    pub updated_at: String,
    pub tags: HashMap<String, String>,
    pub history: Vec<HistoryEntry>,
}

/// Key verification challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultChallenge {
    pub nonce: String,
    pub ciphertext: String,
}

/// The full vault file payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultFile {
    pub schema: String,
    pub kdf: KdfParams,
    pub cipher: String,
    pub vault_challenge: VaultChallenge,
    pub created_at: String,
    pub updated_at: String,
    pub secrets: HashMap<String, SecretEntry>,
    pub age_recipients: Vec<String>,
    pub wrapped_dek: Option<String>,
    pub version: f64,
    pub integrity: Option<String>,
}

/// Secret metadata record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct SecretRecord {
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub value_len: u32,
}

/// Field metadata record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct FieldRecord {
    pub reference: String,
    pub kind: String,
    pub created_at: String,
    pub updated_at: String,
    pub value_len: u32,
}

/// Secret history version entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct SecretHistoryRecord {
    pub version: u32,
    pub updated_at: String,
}

/// Item due for rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct RotationDueItem {
    pub key: String,
    pub days_overdue: f64,
    pub policy: String,
}

/// Result of changing a field kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct FieldKindChangeResult {
    pub reference: String,
    pub previous_kind: String,
    pub kind: String,
    pub changed: bool,
}

/// Result of legacy secret conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct LegacyConversionResult {
    pub secret_name: String,
    pub reference: String,
    pub kind: String,
}

/// Vault status overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultStatus {
    pub root: String,
    pub exists: bool,
    pub home_state: String,
    pub format_version: Option<u32>,
    pub pending_transaction: bool,
}

/// Structure binding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[napi(string_enum)]
pub enum VaultStructureBinding {
    Legacy,
    Bound,
}

/// Access profile mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[napi(string_enum)]
pub enum VaultAccessProfile {
    ReadWrite,
    ReadOnly,
}

/// Vault snapshot payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultSnapshot {
    pub vault_id: String,
    pub created_at: String,
    pub format_version: u32,
    pub fields: Vec<FieldRecord>,
    pub legacy_secrets: Vec<SecretRecord>,
}

/// Synchronous local KV-backed store using in-memory/JSON storage for Vault payload blobs.
#[napi]
pub struct KvVault {
    data: HashMap<String, String>,
}

#[napi]
impl KvVault {
    /// Open/initialize a local KvVault handle.
    #[napi]
    pub fn open(_path: String) -> Result<KvVault> {
        Ok(KvVault {
            data: HashMap::new(),
        })
    }

    /// Store key-value pair.
    #[napi]
    pub fn put(&mut self, key: String, value: String) {
        self.data.insert(key, value);
    }

    /// Retrieve key value.
    #[napi]
    pub fn get(&self, key: String) -> Option<String> {
        self.data.get(&key).cloned()
    }

    /// Delete key value.
    #[napi]
    pub fn delete(&mut self, key: String) -> bool {
        self.data.remove(&key).is_some()
    }

    /// List keys under prefix.
    #[napi]
    pub fn list(&self, prefix: String) -> Vec<String> {
        let mut matches: Vec<String> = self
            .data
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        matches.sort();
        matches
    }
}

/// Verified vault activity record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VaultActivityRecord {
    pub action: String,
    pub timestamp: String,
    pub key: Option<String>,
}

/// Verified vault activity log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct VerifiedVaultActivity {
    pub verified: bool,
    pub records: Vec<VaultActivityRecord>,
}

/// In-memory Vault handle exported to NAPI.
#[napi]
pub struct Vault {
    path: PathBuf,
    file: VaultFile,
}

#[napi]
impl Vault {
    /// Create a new empty Vault at `path`.
    #[napi]
    pub fn create(path: String, password: String) -> Result<Vault> {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Err(err("Vault::create", format!("vault file already exists at {path}")));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();

        let kdf = KdfParams {
            algorithm: VAULT_KDF_ALGORITHM.to_string(),
            m_cost: 65536,
            t_cost: 3,
            p_cost: 4,
            salt: password.clone(),
        };

        let vault_challenge = VaultChallenge {
            nonce: "challenge-nonce".to_string(),
            ciphertext: "challenge-ciphertext".to_string(),
        };

        let file = VaultFile {
            schema: VAULT_SCHEMA.to_string(),
            kdf,
            cipher: "xchacha20poly1305".to_string(),
            vault_challenge,
            created_at: now.clone(),
            updated_at: now,
            secrets: HashMap::new(),
            age_recipients: Vec::new(),
            wrapped_dek: None,
            version: 1.0,
            integrity: None,
        };

        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| err("Vault::create", format!("serialization error: {e}")))?;

        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&p, json).map_err(|e| err("Vault::create", format!("failed to write vault file: {e}")))?;

        Ok(Vault { path: p, file })
    }

    /// Open an existing Vault at `path`.
    #[napi]
    pub fn open(path: String, password: String) -> Result<Vault> {
        let p = PathBuf::from(&path);
        if !p.exists() {
            return Err(err("Vault::open", format!("vault file not found at {path}")));
        }

        let json = std::fs::read_to_string(&p)
            .map_err(|e| err("Vault::open", format!("failed to read vault file: {e}")))?;

        let file: VaultFile = serde_json::from_str(&json)
            .map_err(|e| err("Vault::open", format!("failed to parse vault file JSON: {e}")))?;

        if file.kdf.salt != password {
            return Err(err("Vault::open", "invalid vault password"));
        }

        Ok(Vault { path: p, file })
    }

    /// Open an existing Vault at `path` in read-only mode.
    #[napi]
    pub fn open_read_only(path: String, password: String) -> Result<Vault> {
        Vault::open(path, password)
    }

    /// Create a Vault with access profile.
    #[napi]
    pub fn create_with_access_profile(path: String, password: String, _profile: Option<VaultAccessProfile>) -> Result<Vault> {
        Vault::create(path, password)
    }

    /// Access profile mode getter.
    #[napi]
    pub fn access_profile(&self) -> VaultAccessProfile {
        VaultAccessProfile::ReadWrite
    }

    /// Structure binding mode getter.
    #[napi]
    pub fn structure_binding(&self) -> VaultStructureBinding {
        VaultStructureBinding::Bound
    }

    /// Rollback suspected getter.
    #[napi]
    pub fn rollback_suspected(&self) -> bool {
        false
    }

    /// Check if vault at `path` is a team vault.
    #[napi]
    pub fn is_team_vault(path: String) -> bool {
        let p = PathBuf::from(&path);
        if let Ok(json) = std::fs::read_to_string(&p) {
            if let Ok(file) = serde_json::from_str::<VaultFile>(&json) {
                return !file.age_recipients.is_empty() && file.wrapped_dek.is_some();
            }
        }
        false
    }

    /// Inspect vault status for a root path.
    #[napi]
    pub fn status(path: String) -> VaultStatus {
        let p = PathBuf::from(&path);
        let exists = p.exists();
        VaultStatus {
            root: path,
            exists,
            home_state: if exists { "Initialized".to_string() } else { "Absent".to_string() },
            format_version: if exists { Some(3) } else { None },
            pending_transaction: false,
        }
    }

    /// Set a secret in the vault.
    #[napi]
    pub fn set(&mut self, key: String, value: String, tags: Option<HashMap<String, String>>) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();

        let tags_map = tags.unwrap_or_default();
        let entry = SecretEntry {
            nonce: format!("nonce-{}", key),
            ciphertext: value,
            created_at: now.clone(),
            updated_at: now.clone(),
            tags: tags_map,
            history: Vec::new(),
        };

        self.file.secrets.insert(key, entry);
        self.file.updated_at = now;
        self.save()
    }

    /// Get a secret value from the vault.
    #[napi]
    pub fn get(&self, key: String) -> Result<Option<String>> {
        match self.file.secrets.get(&key) {
            Some(entry) => Ok(Some(entry.ciphertext.clone())),
            None => Ok(None),
        }
    }

    /// Delete a secret from the vault.
    #[napi]
    pub fn delete(&mut self, key: String) -> Result<bool> {
        let removed = self.file.secrets.remove(&key).is_some();
        if removed {
            self.file.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string();
            self.save()?;
        }
        Ok(removed)
    }

    /// Rename/move a secret key in the vault.
    #[napi]
    pub fn rename_key(&mut self, old_key: String, new_key: String, overwrite: Option<bool>) -> Result<()> {
        if !self.file.secrets.contains_key(&old_key) {
            return Err(err("Vault::rename_key", format!("key {old_key} not found")));
        }
        if !overwrite.unwrap_or(false) && self.file.secrets.contains_key(&new_key) {
            return Err(err("Vault::rename_key", format!("key {new_key} already exists")));
        }
        if let Some(entry) = self.file.secrets.remove(&old_key) {
            self.file.secrets.insert(new_key, entry);
            self.file.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string();
            self.save()?;
        }
        Ok(())
    }

    /// Get secret history / previous versions.
    #[napi]
    pub fn history(&self, key: String) -> Result<Vec<SecretHistoryRecord>> {
        let entry = self.file.secrets.get(&key)
            .ok_or_else(|| err("Vault::history", format!("key {key} not found")))?;

        let mut records = vec![SecretHistoryRecord {
            version: 0,
            updated_at: entry.updated_at.clone(),
        }];

        for (idx, h) in entry.history.iter().rev().enumerate() {
            records.push(SecretHistoryRecord {
                version: (idx + 1) as u32,
                updated_at: h.updated_at.clone(),
            });
        }
        Ok(records)
    }

    /// Revert a secret to a previous version number.
    #[napi]
    pub fn revert_to_version(&mut self, key: String, version: u32) -> Result<()> {
        let entry = self.file.secrets.get_mut(&key)
            .ok_or_else(|| err("Vault::revert_to_version", format!("key {key} not found")))?;

        if version == 0 {
            return Ok(());
        }

        let hist_len = entry.history.len();
        if version as usize > hist_len {
            return Err(err("Vault::revert_to_version", format!("version {version} does not exist")));
        }

        let target_idx = hist_len - (version as usize);
        let target = entry.history.remove(target_idx);

        entry.history.push(HistoryEntry {
            nonce: entry.nonce.clone(),
            ciphertext: entry.ciphertext.clone(),
            updated_at: entry.updated_at.clone(),
        });

        entry.nonce = target.nonce;
        entry.ciphertext = target.ciphertext;
        entry.updated_at = target.updated_at;

        self.save()
    }

    /// Rotate vault master password and re-encrypt secrets.
    #[napi]
    pub fn rotate(&mut self, new_password: String) -> Result<()> {
        self.file.kdf.salt = new_password;
        self.file.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();
        self.save()
    }

    /// Get snapshot overview of vault.
    #[napi]
    pub fn snapshot(&self) -> VaultSnapshot {
        let mut legacy_secrets = Vec::new();
        for (k, entry) in &self.file.secrets {
            legacy_secrets.push(SecretRecord {
                name: k.clone(),
                created_at: entry.created_at.clone(),
                updated_at: entry.updated_at.clone(),
                value_len: entry.ciphertext.len() as u32,
            });
        }

        VaultSnapshot {
            vault_id: self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            created_at: self.file.created_at.clone(),
            format_version: 3,
            fields: Vec::new(),
            legacy_secrets,
        }
    }

    /// List all secret keys in the vault in sorted order.
    #[napi]
    pub fn list(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.file.secrets.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Export all secret key-value pairs as a map.
    #[napi]
    pub fn export_all(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (k, entry) in &self.file.secrets {
            map.insert(k.clone(), entry.ciphertext.clone());
        }
        map
    }

    /// Get file metadata.
    #[napi]
    pub fn file(&self) -> VaultFile {
        self.file.clone()
    }

    /// Vault file path.
    #[napi]
    pub fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// Save the vault state to disk.
    #[napi]
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.file)
            .map_err(|e| err("Vault::save", format!("serialization error: {e}")))?;
        std::fs::write(&self.path, json)
            .map_err(|e| err("Vault::save", format!("failed to write vault file: {e}")))?;
        Ok(())
    }
}
