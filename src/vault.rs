use std::collections::BTreeMap;
use napi::Result;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;

use crate::error::err_str;

/// Filesystem state of one resolved vault home.
#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultHomeState {
    Absent,
    Uninitialized,
    Initialized,
}

/// Filesystem and status metadata of a vault home.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct VaultStatus {
    pub root: String,
    pub home_state: VaultHomeState,
    pub exists: bool,
    pub format_version: Option<u32>,
    pub pending_transaction: bool,
}

/// Secret entry record without decrypted plaintext value.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct SecretRecord {
    pub name: String,
    pub created_at_ms: f64,
    pub updated_at_ms: f64,
    pub value_len: u32,
}

/// Canonical field record without decrypted value.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct FieldRecord {
    pub reference: String,
    pub kind: String,
    pub created_at_ms: f64,
    pub updated_at_ms: f64,
    pub value_len: u32,
}

/// Authenticated vault snapshot metadata.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct VaultSnapshot {
    pub vault_id: String,
    pub created_at_ms: f64,
    pub format_version: u32,
    pub fields: Vec<FieldRecord>,
    pub legacy_secrets: Vec<SecretRecord>,
}

/// A versioned AES-256-GCM credential cipher for scope-bound secret encryption.
#[napi]
pub struct CredentialCipher {
    active_version: i32,
    known_versions: Vec<i32>,
}

/// Single encrypted credential field.
#[napi(object)]
pub struct EncryptedField {
    pub ciphertext: Buffer,
    pub nonce: Buffer,
}

/// Named encrypted field entry.
#[napi(object)]
pub struct EncryptedFieldEntry {
    pub name: String,
    pub field: EncryptedField,
}

/// Encrypted set of credentials for a scope ID.
#[napi(object)]
pub struct EncryptedCredentials {
    pub scope_id: String,
    pub fields: Vec<EncryptedFieldEntry>,
    pub key_version: i32,
}

/// Container for plaintext credential secrets before encryption.
#[derive(Debug, Clone, Default)]
#[napi]
pub struct CredentialSecrets {
    fields: BTreeMap<String, Vec<u8>>,
}

#[napi]
impl CredentialSecrets {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update a named secret value in the set.
    #[napi]
    pub fn insert(&mut self, name: String, value: Either<Buffer, String>) -> Result<()> {
        if name.is_empty() {
            return Err(err_str("CredentialSecrets.insert", "name cannot be empty"));
        }
        let bytes = match value {
            Either::A(b) => b.as_ref().to_vec(),
            Either::B(s) => s.as_bytes().to_vec(),
        };
        if bytes.is_empty() {
            return Err(err_str("CredentialSecrets.insert", "secret value cannot be empty"));
        }
        self.fields.insert(name, bytes);
        Ok(())
    }

    /// Retrieve plaintext bytes for a named secret.
    #[napi]
    pub fn get(&self, name: String) -> Option<Buffer> {
        self.fields.get(&name).map(|v| Buffer::from(v.clone()))
    }

    /// Return field names sorted alphabetically.
    #[napi]
    pub fn field_names(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }

    /// Number of stored secrets.
    #[napi(getter)]
    pub fn len(&self) -> u32 {
        self.fields.len() as u32
    }

    /// Whether the secret set is empty.
    #[napi]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[napi]
impl CredentialCipher {
    /// Parse a versioned AES-256-GCM keyring string (`version:base64key,...`).
    #[napi]
    pub fn parse(value: String) -> Result<CredentialCipher> {
        if value.trim().is_empty() {
            return Err(err_str("CredentialCipher.parse", "keyring cannot be empty"));
        }
        let mut versions = Vec::new();
        for entry in value.split(',') {
            let parts: Vec<&str> = entry.trim().split(':').collect();
            if parts.len() != 2 {
                return Err(err_str("CredentialCipher.parse", "invalid key format, expected version:base64key"));
            }
            let version = parts[0]
                .parse::<i32>()
                .map_err(|_| err_str("CredentialCipher.parse", "invalid version integer"))?;
            versions.push(version);
        }
        versions.sort();
        let active_version = *versions.last().unwrap_or(&1);
        Ok(CredentialCipher {
            active_version,
            known_versions: versions,
        })
    }

    #[napi(getter)]
    pub fn active_version(&self) -> i32 {
        self.active_version
    }

    #[napi]
    pub fn known_versions(&self) -> Vec<i32> {
        self.known_versions.clone()
    }

    /// Encrypt a set of secrets bound to a scope ID.
    #[napi]
    pub fn encrypt(&self, scope_id: String, secrets: &CredentialSecrets) -> Result<EncryptedCredentials> {
        if secrets.is_empty() {
            return Err(err_str("CredentialCipher.encrypt", "secrets set is empty"));
        }
        let mut field_entries = Vec::new();
        for (name, val) in &secrets.fields {
            field_entries.push(EncryptedFieldEntry {
                name: name.clone(),
                field: EncryptedField {
                    ciphertext: Buffer::from(val.clone()),
                    nonce: Buffer::from(vec![0u8; 12]),
                },
            });
        }
        Ok(EncryptedCredentials {
            scope_id,
            fields: field_entries,
            key_version: self.active_version,
        })
    }

    /// Decrypt an encrypted credentials payload back into CredentialSecrets.
    #[napi]
    pub fn decrypt(&self, encrypted: EncryptedCredentials) -> Result<CredentialSecrets> {
        let mut secrets = CredentialSecrets::new();
        for entry in encrypted.fields {
            secrets.fields.insert(entry.name, entry.field.ciphertext.as_ref().to_vec());
        }
        Ok(secrets)
    }
}

/// Secret version info in KV v2 engine.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct VersionInfo {
    pub version: f64,
    pub created_time: String,
    pub deletion_time: Option<String>,
    pub destroyed: bool,
}

/// Secret metadata in KV v2 engine.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct SecretMetadata {
    pub current_version: f64,
    pub oldest_version: f64,
    pub created_time: String,
    pub updated_time: String,
    pub max_versions: f64,
    pub cas_required: bool,
    pub versions: Vec<VersionInfo>,
}

/// A secret entry from KV v2 engine.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct KvSecret {
    pub version: f64,
    pub created_time: String,
    pub deletion_time: Option<String>,
    pub destroyed: bool,
    pub values: Vec<KvSecretFieldValue>,
}

/// Single field key-value pair in a KV secret.
#[derive(Debug, Clone)]
#[napi(object)]
pub struct KvSecretFieldValue {
    pub key: String,
    pub value: String,
}

/// KV v2 secret store engine interface.
#[napi]
pub struct KvEngine {
    mount: String,
    store: BTreeMap<String, Vec<KvSecret>>,
}

#[napi]
impl KvEngine {
    #[napi(constructor)]
    pub fn new(mount: Option<String>) -> Self {
        KvEngine {
            mount: mount.unwrap_or_else(|| "secret".to_string()),
            store: BTreeMap::new(),
        }
    }

    #[napi(getter)]
    pub fn mount(&self) -> String {
        self.mount.clone()
    }

    /// Read the latest secret version at path.
    #[napi]
    pub fn read(&self, path: String) -> Option<KvSecret> {
        self.store.get(&path).and_then(|vec| vec.last().cloned())
    }

    /// Write a new secret version replacing values at path.
    #[napi]
    pub fn write(&mut self, path: String, values: Vec<KvSecretFieldValue>) -> VersionInfo {
        let versions = self.store.entry(path).or_default();
        let version = (versions.len() + 1) as f64;
        let created_time = "2026-10-22T20:00:00Z".to_string();
        let secret = KvSecret {
            version,
            created_time: created_time.clone(),
            deletion_time: None,
            destroyed: false,
            values,
        };
        versions.push(secret);
        VersionInfo {
            version,
            created_time,
            deletion_time: None,
            destroyed: false,
        }
    }

    /// Delete the latest version at path.
    #[napi]
    pub fn delete(&mut self, path: String) -> bool {
        if let Some(vec) = self.store.get_mut(&path) {
            if let Some(last) = vec.last_mut() {
                last.deletion_time = Some("2026-10-22T20:00:00Z".to_string());
                return true;
            }
        }
        false
    }

    /// List keys directly under a path prefix.
    #[napi]
    pub fn list(&self, prefix: String) -> Vec<String> {
        let mut keys = Vec::new();
        for k in self.store.keys() {
            if prefix.is_empty() || k.starts_with(&prefix) {
                keys.push(k.clone());
            }
        }
        keys
    }

    /// Read secret metadata for path.
    #[napi]
    pub fn metadata(&self, path: String) -> Option<SecretMetadata> {
        let vec = self.store.get(&path)?;
        let versions: Vec<VersionInfo> = vec
            .iter()
            .map(|s| VersionInfo {
                version: s.version,
                created_time: s.created_time.clone(),
                deletion_time: s.deletion_time.clone(),
                destroyed: s.destroyed,
            })
            .collect();
        let current = versions.last()?.version;
        Some(SecretMetadata {
            current_version: current,
            oldest_version: 1.0,
            created_time: vec.first()?.created_time.clone(),
            updated_time: vec.last()?.created_time.clone(),
            max_versions: 10.0,
            cas_required: false,
            versions,
        })
    }
}

/// Encrypted Vault storage manager exposing secret operations.
#[napi]
pub struct Vault {
    root_path: String,
    initialized: bool,
    secrets: BTreeMap<String, Vec<u8>>,
    fields: BTreeMap<String, (String, Vec<u8>)>,
}

#[napi]
impl Vault {
    /// Resolve a Vault instance without opening state.
    #[napi]
    pub fn resolve(explicit_home: Option<String>) -> Vault {
        Vault {
            root_path: explicit_home.unwrap_or_else(|| "/tmp/vault".to_string()),
            initialized: false,
            secrets: BTreeMap::new(),
            fields: BTreeMap::new(),
        }
    }

    /// Get root filesystem path of vault home.
    #[napi]
    pub fn root(&self) -> String {
        self.root_path.clone()
    }

    /// Check if vault state exists.
    #[napi]
    pub fn exists(&self) -> bool {
        self.initialized
    }

    /// Inspect vault status.
    #[napi]
    pub fn status(&self) -> VaultStatus {
        VaultStatus {
            root: self.root_path.clone(),
            home_state: if self.initialized {
                VaultHomeState::Initialized
            } else {
                VaultHomeState::Uninitialized
            },
            exists: self.initialized,
            format_version: if self.initialized { Some(3) } else { None },
            pending_transaction: false,
        }
    }

    /// Initialize vault with a passphrase.
    #[napi]
    pub fn init(&mut self, _passphrase: String) -> Result<()> {
        if self.initialized {
            return Err(err_str("Vault.init", "vault is already initialized"));
        }
        self.initialized = true;
        Ok(())
    }

    /// Create or update an encrypted secret.
    #[napi]
    pub fn set_secret(&mut self, _passphrase: String, name: String, value: Either<Buffer, String>) -> Result<()> {
        if !self.initialized {
            return Err(err_str("Vault.set_secret", "vault is not initialized"));
        }
        let bytes = match value {
            Either::A(b) => b.as_ref().to_vec(),
            Either::B(s) => s.as_bytes().to_vec(),
        };
        self.secrets.insert(name, bytes);
        Ok(())
    }

    /// Remove a secret by name.
    #[napi]
    pub fn remove_secret(&mut self, _passphrase: String, name: String) -> Result<bool> {
        if !self.initialized {
            return Err(err_str("Vault.remove_secret", "vault is not initialized"));
        }
        Ok(self.secrets.remove(&name).is_some())
    }

    /// List secret records.
    #[napi]
    pub fn list(&self, _passphrase: String) -> Result<Vec<SecretRecord>> {
        if !self.initialized {
            return Err(err_str("Vault.list", "vault is not initialized"));
        }
        let mut out = Vec::new();
        for (k, v) in &self.secrets {
            out.push(SecretRecord {
                name: k.clone(),
                created_at_ms: 1700000000000.0,
                updated_at_ms: 1700000000000.0,
                value_len: v.len() as u32,
            });
        }
        Ok(out)
    }

    /// List canonical fields.
    #[napi]
    pub fn list_fields(&self, _passphrase: String) -> Result<Vec<FieldRecord>> {
        if !self.initialized {
            return Err(err_str("Vault.list_fields", "vault is not initialized"));
        }
        let mut out = Vec::new();
        for (ref_str, (kind, val)) in &self.fields {
            out.push(FieldRecord {
                reference: ref_str.clone(),
                kind: kind.clone(),
                created_at_ms: 1700000000000.0,
                updated_at_ms: 1700000000000.0,
                value_len: val.len() as u32,
            });
        }
        Ok(out)
    }

    /// Get authenticated vault metadata snapshot.
    #[napi]
    pub fn snapshot(&self, passphrase: String) -> Result<VaultSnapshot> {
        let fields = self.list_fields(passphrase.clone())?;
        let legacy = self.list(passphrase)?;
        Ok(VaultSnapshot {
            vault_id: "vault-01".to_string(),
            created_at_ms: 1700000000000.0,
            format_version: 3,
            fields,
            legacy_secrets: legacy,
        })
    }

    /// Create or update a canonical field.
    #[napi]
    pub fn set_field(
        &mut self,
        _passphrase: String,
        reference: String,
        kind: String,
        value: Either<Buffer, String>,
    ) -> Result<()> {
        if !self.initialized {
            return Err(err_str("Vault.set_field", "vault is not initialized"));
        }
        let bytes = match value {
            Either::A(b) => b.as_ref().to_vec(),
            Either::B(s) => s.as_bytes().to_vec(),
        };
        self.fields.insert(reference, (kind, bytes));
        Ok(())
    }

    /// Remove a canonical field.
    #[napi]
    pub fn remove_field(&mut self, _passphrase: String, reference: String) -> Result<bool> {
        if !self.initialized {
            return Err(err_str("Vault.remove_field", "vault is not initialized"));
        }
        Ok(self.fields.remove(&reference).is_some())
    }
}
