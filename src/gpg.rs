use std::io::{Cursor, Write};
use std::process::{Command, Stdio};

use pgp::composed::{ArmorOptions, Deserializable, Message, MessageBuilder, SignedPublicKey, SignedSecretKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::types::{KeyDetails, KeyId, Password};

/// Protocol options supported for GPG operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    OpenPgp,
    Cms,
}

impl Protocol {
    pub fn name(&self) -> &'static str {
        match self {
            Protocol::OpenPgp => "OpenPGP",
            Protocol::Cms => "CMS",
        }
    }
}

/// Representation of data passed to cryptographic operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    bytes: Vec<u8>,
    armored: bool,
}

impl Data {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            armored: false,
        }
    }

    pub fn from_armored_string(armored_str: &str) -> Self {
        Self {
            bytes: armored_str.as_bytes().to_vec(),
            armored: true,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn is_armored(&self) -> bool {
        self.armored
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Representation of OpenPGP Public or Secret keys.
#[derive(Debug, Clone)]
pub enum Key {
    Public(SignedPublicKey),
    Secret(SignedSecretKey),
}

impl Key {
    pub fn from_public_bytes(bytes: &[u8]) -> Result<Self, String> {
        if let Ok((key, _)) = SignedPublicKey::from_armor_single(Cursor::new(bytes)) {
            return Ok(Key::Public(key));
        }
        if let Ok(key) = SignedPublicKey::from_bytes(Cursor::new(bytes)) {
            return Ok(Key::Public(key));
        }
        Err("Failed to parse public key from bytes or armor".into())
    }

    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self, String> {
        if let Ok((key, _)) = SignedSecretKey::from_armor_single(Cursor::new(bytes)) {
            return Ok(Key::Secret(key));
        }
        if let Ok(key) = SignedSecretKey::from_bytes(Cursor::new(bytes)) {
            return Ok(Key::Secret(key));
        }
        Err("Failed to parse secret key from bytes or armor".into())
    }

    pub fn is_secret(&self) -> bool {
        matches!(self, Key::Secret(_))
    }

    pub fn key_id(&self) -> String {
        let fp = match self {
            Key::Public(k) => k.fingerprint(),
            Key::Secret(k) => k.fingerprint(),
        };
        let bytes = fp.as_ref();
        if bytes.len() >= 8 {
            let last8: [u8; 8] = bytes[bytes.len() - 8..].try_into().unwrap();
            hex::encode(KeyId::from(last8).as_ref())
        } else {
            hex::encode(bytes)
        }
    }

    pub fn fingerprint(&self) -> String {
        let fp = match self {
            Key::Public(k) => k.fingerprint(),
            Key::Secret(k) => k.fingerprint(),
        };
        hex::encode(fp.as_ref())
    }

    pub fn user_ids(&self) -> Vec<String> {
        match self {
            Key::Public(k) => k
                .details
                .users
                .iter()
                .map(|u| String::from_utf8_lossy(u.id.id().as_ref()).to_string())
                .collect(),
            Key::Secret(k) => k
                .details
                .users
                .iter()
                .map(|u| String::from_utf8_lossy(u.id.id().as_ref()).to_string())
                .collect(),
        }
    }

    pub fn public_key(&self) -> Result<SignedPublicKey, String> {
        match self {
            Key::Public(k) => Ok(k.clone()),
            Key::Secret(k) => Ok(k.to_public_key()),
        }
    }
}

/// Inspection of signature verification results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationResult {
    pub is_valid: bool,
    pub signer_key_id: Option<String>,
    pub creation_time: Option<u64>,
    pub message: String,
}

impl VerificationResult {
    pub fn success(signer_key_id: String, creation_time: Option<u64>) -> Self {
        Self {
            is_valid: true,
            signer_key_id: Some(signer_key_id),
            creation_time,
            message: "Signature verified successfully".to_string(),
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            is_valid: false,
            signer_key_id: None,
            creation_time: None,
            message: message.into(),
        }
    }
}

/// Context for executing cryptographic operations (encryption, decryption, signing, verification).
#[derive(Debug, Clone)]
pub struct Context {
    protocol: Protocol,
    armor: bool,
    symmetric_algorithm: SymmetricKeyAlgorithm,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            protocol: Protocol::OpenPgp,
            armor: true,
            symmetric_algorithm: SymmetricKeyAlgorithm::AES256,
        }
    }
}

impl Context {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            ..Default::default()
        }
    }

    pub fn set_armor(&mut self, armor: bool) -> &mut Self {
        self.armor = armor;
        self
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn encrypt(&self, recipients: &[Key], data: &Data) -> Result<Data, String> {
        if self.protocol != Protocol::OpenPgp {
            return Err(format!("Protocol {:?} not supported for encryption", self.protocol));
        }

        let pub_keys: Result<Vec<SignedPublicKey>, String> =
            recipients.iter().map(|k| k.public_key()).collect();
        let pub_keys = pub_keys?;

        let mut rng = rand::thread_rng();
        let builder = MessageBuilder::from_bytes("data", data.bytes().to_vec());
        let mut enc_builder = builder.seipd_v1(&mut rng, self.symmetric_algorithm);

        for k in &pub_keys {
            enc_builder
                .encrypt_to_key(&mut rng, k)
                .map_err(|e| format!("Encryption error: {e}"))?;
        }

        if self.armor {
            let armored = enc_builder
                .to_armored_string(&mut rng, ArmorOptions::default())
                .map_err(|e| format!("Armoring error: {e}"))?;
            Ok(Data::from_armored_string(&armored))
        } else {
            let bytes = enc_builder
                .to_vec(&mut rng)
                .map_err(|e| format!("Serialization error: {e}"))?;
            Ok(Data::from_bytes(bytes))
        }
    }

    pub fn decrypt(&self, secret_key: &Key, passphrase: Option<&str>, data: &Data) -> Result<Data, String> {
        if self.protocol != Protocol::OpenPgp {
            return Err(format!("Protocol {:?} not supported for decryption", self.protocol));
        }

        let Key::Secret(sk) = secret_key else {
            return Err("Decryption requires a secret key".into());
        };

        let (msg, _) = Message::from_armor(Cursor::new(data.bytes()))
            .or_else(|_| Message::from_bytes(Cursor::new(data.bytes())).map(|m| (m, Default::default())))
            .map_err(|e| format!("Failed to parse message: {e}"))?;

        let pw = Password::from(passphrase.unwrap_or(""));
        let mut decrypted = msg
            .decrypt(&pw, sk)
            .map_err(|e| format!("Decryption error: {e}"))?;

        let literal_bytes = decrypted
            .as_data_vec()
            .map_err(|e| format!("Failed to extract decrypted data: {e}"))?;

        Ok(Data::from_bytes(literal_bytes))
    }

    pub fn sign(&self, secret_key: &Key, passphrase: Option<&str>, data: &Data) -> Result<Data, String> {
        if self.protocol != Protocol::OpenPgp {
            return Err(format!("Protocol {:?} not supported for signing", self.protocol));
        }

        let Key::Secret(sk) = secret_key else {
            return Err("Signing requires a secret key".into());
        };

        let mut rng = rand::thread_rng();
        let mut builder = MessageBuilder::from_bytes("data", data.bytes().to_vec());
        let pw = Password::from(passphrase.unwrap_or(""));
        let _ = builder.sign(&sk.primary_key, pw, HashAlgorithm::Sha256);

        if self.armor {
            let armored = builder
                .to_armored_string(&mut rng, ArmorOptions::default())
                .map_err(|e| format!("Armoring error: {e}"))?;
            Ok(Data::from_armored_string(&armored))
        } else {
            let bytes = builder
                .to_vec(&mut rng)
                .map_err(|e| format!("Serialization error: {e}"))?;
            Ok(Data::from_bytes(bytes))
        }
    }

    pub fn verify(&self, public_key: &Key, signed_data: &Data) -> VerificationResult {
        if self.protocol != Protocol::OpenPgp {
            return VerificationResult::failure(format!(
                "Protocol {:?} not supported for verification",
                self.protocol
            ));
        }

        let Ok(pub_key) = public_key.public_key() else {
            return VerificationResult::failure("Failed to obtain public key for verification");
        };

        let (msg, _) = match Message::from_armor(Cursor::new(signed_data.bytes()))
            .or_else(|_| Message::from_bytes(Cursor::new(signed_data.bytes())).map(|m| (m, Default::default())))
        {
            Ok(m) => m,
            Err(e) => return VerificationResult::failure(format!("Failed to parse signed data: {e}")),
        };

        match msg.verify(&pub_key) {
            Ok(_) => {
                let signer_id = public_key.key_id();
                VerificationResult::success(signer_id, None)
            }
            Err(e) => VerificationResult::failure(format!("Verification error: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// GPG CLI / keyring helpers
// ---------------------------------------------------------------------------

pub fn gpg_get_executable() -> String {
    if let Ok(repo) = gix::open(".") {
        if let Some(config) = repo.config_snapshot().string("gpg.program") {
            return config.to_string();
        }
    }
    "gpg".to_string()
}

pub fn gpg_nth_column(line: &str, col: usize) -> Result<String, String> {
    let mut pos = 0;
    for _ in 0..col {
        match line[pos..].find(':') {
            Some(p) => pos += p + 1,
            None => return Err("Malformed output from gpg".to_string()),
        }
    }
    let end_pos = line[pos..].find(':').map(|p| pos + p);
    match end_pos {
        Some(end) => Ok(line[pos..end].to_string()),
        None => Ok(line[pos..].to_string()),
    }
}

pub fn gpg_get_uid(fingerprint: &str) -> String {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args([
            "--batch",
            "--with-colons",
            "--fixed-list-mode",
            "--list-keys",
            &format!("0x{fingerprint}"),
        ])
        .output();

    let Ok(output) = output else {
        return String::new();
    };

    if !output.status.success() {
        return String::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.starts_with("uid:") {
            if let Ok(col) = gpg_nth_column(line, 9) {
                return col;
            }
        }
    }

    String::new()
}

pub fn gpg_lookup_key(query: &str) -> Vec<String> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args(["--batch", "--with-colons", "--fingerprint", "--list-keys", query])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    let mut fingerprints = Vec::new();
    let mut is_pubkey = false;
    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.starts_with("pub:") {
            is_pubkey = true;
        } else if line.starts_with("sub:") {
            is_pubkey = false;
        } else if is_pubkey && line.starts_with("fpr:") {
            if let Ok(col) = gpg_nth_column(line, 9) {
                fingerprints.push(col);
            }
        }
    }

    fingerprints
}

pub fn gpg_list_secret_keys() -> Result<Vec<String>, String> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args(["--batch", "--with-colons", "--list-secret-keys", "--fingerprint"])
        .output()
        .map_err(|e| format!("Failed to execute gpg: {e}"))?;

    if !output.status.success() {
        return Err("gpg --list-secret-keys failed".to_string());
    }

    let mut secret_keys = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.starts_with("fpr:") {
            if let Ok(col) = gpg_nth_column(line, 9) {
                secret_keys.push(col);
            }
        }
    }

    Ok(secret_keys)
}

pub fn gpg_encrypt_to_file(
    filename: &str,
    recipient_fingerprint: &str,
    key_is_trusted: bool,
    input: &[u8],
) -> Result<(), String> {
    let exe = gpg_get_executable();
    let mut cmd = Command::new(exe);
    cmd.arg("--batch");
    if key_is_trusted {
        cmd.args(["--trust-model", "always"]);
    }
    cmd.args(["-o", filename, "-r", &format!("0x{recipient_fingerprint}"), "-e"]);
    cmd.stdin(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn gpg: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(|e| format!("Failed to write stdin: {e}"))?;
    }

    let status = child.wait().map_err(|e| format!("Failed to wait for gpg: {e}"))?;
    if !status.success() {
        return Err("Failed to encrypt".to_string());
    }

    Ok(())
}

pub fn gpg_decrypt_from_file(filename: &str) -> Result<Vec<u8>, String> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args(["-q", "-d", filename])
        .output()
        .map_err(|e| format!("Failed to execute gpg: {e}"))?;

    if !output.status.success() {
        return Err("Failed to decrypt".to_string());
    }

    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol() {
        let p = Protocol::OpenPgp;
        assert_eq!(p.name(), "OpenPGP");
        let cms = Protocol::Cms;
        assert_eq!(cms.name(), "CMS");
    }

    #[test]
    fn test_data() {
        let d1 = Data::from_bytes(b"hello".to_vec());
        assert_eq!(d1.bytes(), b"hello");
        assert!(!d1.is_armored());

        let d2 = Data::from_armored_string("-----BEGIN PGP MESSAGE-----\n-----END PGP MESSAGE-----");
        assert!(d2.is_armored());
    }

    #[test]
    fn test_gpg_nth_column() {
        let line = "uid:u::::1395975462::AB97D6E3E5D8789988CA55E5F77D9E7397D05229::John Smith <jsmith@example.com>:";
        let col9 = gpg_nth_column(line, 9).unwrap();
        assert_eq!(col9, "John Smith <jsmith@example.com>");

        let fpr_line = "fpr:::::::::7A399B2DB06D039020CD1CE1D0F3702D61489532:";
        let col9_fpr = gpg_nth_column(fpr_line, 9).unwrap();
        assert_eq!(col9_fpr, "7A399B2DB06D039020CD1CE1D0F3702D61489532");
    }

    #[test]
    fn test_context_default() {
        let ctx = Context::default();
        assert_eq!(ctx.protocol(), Protocol::OpenPgp);
    }
}
