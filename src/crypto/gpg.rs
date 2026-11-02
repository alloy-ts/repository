use std::io::{Cursor, Write};
use std::process::{Command, Stdio};

use napi::Result as NapiResult;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use pgp::composed::{
    ArmorOptions, Deserializable, Message, MessageBuilder, SignedPublicKey, SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::types::{KeyDetails, KeyId, Password};

/// Protocol options supported for GPG operations.
#[napi]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    OpenPgp,
    Cms,
}

#[napi]
impl Protocol {
    #[napi(getter)]
    pub fn name(&self) -> &'static str {
        match self {
            Protocol::OpenPgp => "OpenPGP",
            Protocol::Cms => "CMS",
        }
    }
}

/// Representation of data passed to cryptographic operations.
#[napi(js_name = "GpgData")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub(crate) bytes: Vec<u8>,
    pub(crate) armored: bool,
}

#[napi]
impl Data {
    #[napi(factory, js_name = "fromBytes")]
    pub fn from_bytes(bytes: Buffer) -> Self {
        Self {
            bytes: bytes.as_ref().to_vec(),
            armored: false,
        }
    }

    #[napi(factory, js_name = "fromArmoredString")]
    pub fn from_armored_string(armored_str: String) -> Self {
        Self {
            bytes: armored_str.as_bytes().to_vec(),
            armored: true,
        }
    }

    #[napi(js_name = "bytes")]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi(getter, js_name = "isArmored")]
    pub fn is_armored(&self) -> bool {
        self.armored
    }

    #[napi(js_name = "intoBytes")]
    pub fn into_bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }
}

/// Representation of OpenPGP Public or Secret keys.
#[napi(js_name = "GpgKey")]
#[derive(Debug, Clone)]
pub struct Key {
    pub(crate) inner: KeyInner,
}

#[derive(Debug, Clone)]
pub(crate) enum KeyInner {
    Public(SignedPublicKey),
    Secret(SignedSecretKey),
}

#[napi]
impl Key {
    #[napi(js_name = "fromPublicBytes")]
    pub fn from_public_bytes(bytes: Buffer) -> NapiResult<Key> {
        let b = bytes.as_ref();
        if let Ok((key, _)) = SignedPublicKey::from_armor_single(Cursor::new(b)) {
            return Ok(Key {
                inner: KeyInner::Public(key),
            });
        }
        if let Ok(key) = SignedPublicKey::from_bytes(Cursor::new(b)) {
            return Ok(Key {
                inner: KeyInner::Public(key),
            });
        }
        Err(napi::Error::from_reason(
            "Failed to parse public key from bytes or armor".to_string(),
        ))
    }

    #[napi(js_name = "fromSecretBytes")]
    pub fn from_secret_bytes(bytes: Buffer) -> NapiResult<Key> {
        let b = bytes.as_ref();
        if let Ok((key, _)) = SignedSecretKey::from_armor_single(Cursor::new(b)) {
            return Ok(Key {
                inner: KeyInner::Secret(key),
            });
        }
        if let Ok(key) = SignedSecretKey::from_bytes(Cursor::new(b)) {
            return Ok(Key {
                inner: KeyInner::Secret(key),
            });
        }
        Err(napi::Error::from_reason(
            "Failed to parse secret key from bytes or armor".to_string(),
        ))
    }

    #[napi(getter, js_name = "isSecret")]
    pub fn is_secret(&self) -> bool {
        matches!(self.inner, KeyInner::Secret(_))
    }

    #[napi(getter, js_name = "keyId")]
    pub fn key_id(&self) -> String {
        let fp = match &self.inner {
            KeyInner::Public(k) => k.fingerprint(),
            KeyInner::Secret(k) => k.fingerprint(),
        };
        let bytes = fp.as_ref();
        if bytes.len() >= 8 {
            let last8: [u8; 8] = bytes[bytes.len() - 8..].try_into().unwrap();
            hex::encode(KeyId::from(last8).as_ref())
        } else {
            hex::encode(bytes)
        }
    }

    #[napi(getter)]
    pub fn fingerprint(&self) -> String {
        let fp = match &self.inner {
            KeyInner::Public(k) => k.fingerprint(),
            KeyInner::Secret(k) => k.fingerprint(),
        };
        hex::encode(fp.as_ref())
    }

    #[napi(getter, js_name = "userIds")]
    pub fn user_ids(&self) -> Vec<String> {
        match &self.inner {
            KeyInner::Public(k) => k
                .details
                .users
                .iter()
                .map(|u| String::from_utf8_lossy(u.id.id().as_ref()).to_string())
                .collect(),
            KeyInner::Secret(k) => k
                .details
                .users
                .iter()
                .map(|u| String::from_utf8_lossy(u.id.id().as_ref()).to_string())
                .collect(),
        }
    }

    pub fn public_key(&self) -> NapiResult<SignedPublicKey> {
        match &self.inner {
            KeyInner::Public(k) => Ok(k.clone()),
            KeyInner::Secret(k) => Ok(k.to_public_key()),
        }
    }
}

/// Inspection of signature verification results.
#[napi(object)]
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationResult {
    pub is_valid: bool,
    pub signer_key_id: Option<String>,
    pub creation_time: Option<f64>,
    pub message: String,
}

impl VerificationResult {
    pub fn success(signer_key_id: String, creation_time: Option<u64>) -> Self {
        Self {
            is_valid: true,
            signer_key_id: Some(signer_key_id),
            creation_time: creation_time.map(|t| t as f64),
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
#[napi(js_name = "GpgContext")]
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

#[napi]
impl Context {
    #[napi(constructor)]
    pub fn new(protocol: Option<Protocol>) -> Self {
        Self {
            protocol: protocol.unwrap_or(Protocol::OpenPgp),
            ..Default::default()
        }
    }

    #[napi(js_name = "setArmor")]
    pub fn set_armor(&mut self, armor: bool) {
        self.armor = armor;
    }

    #[napi(getter)]
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    #[napi]
    pub fn encrypt(&self, recipients: Vec<&Key>, data: &Data) -> NapiResult<Data> {
        if self.protocol != Protocol::OpenPgp {
            return Err(napi::Error::from_reason(format!(
                "Protocol {:?} not supported for encryption",
                self.protocol
            )));
        }

        let pub_keys: NapiResult<Vec<SignedPublicKey>> =
            recipients.iter().map(|k| k.public_key()).collect();
        let pub_keys = pub_keys?;

        let mut rng = rand::thread_rng();
        let builder = MessageBuilder::from_bytes("data", data.bytes.clone());
        let mut enc_builder = builder.seipd_v1(&mut rng, self.symmetric_algorithm);

        for k in &pub_keys {
            enc_builder
                .encrypt_to_key(&mut rng, k)
                .map_err(|e| napi::Error::from_reason(format!("Encryption error: {e}")))?;
        }

        if self.armor {
            let armored = enc_builder
                .to_armored_string(&mut rng, ArmorOptions::default())
                .map_err(|e| napi::Error::from_reason(format!("Armoring error: {e}")))?;
            Ok(Data::from_armored_string(armored))
        } else {
            let bytes = enc_builder
                .to_vec(&mut rng)
                .map_err(|e| napi::Error::from_reason(format!("Serialization error: {e}")))?;
            Ok(Data::from_bytes(Buffer::from(bytes)))
        }
    }

    #[napi]
    pub fn decrypt(
        &self,
        secret_key: &Key,
        passphrase: Option<String>,
        data: &Data,
    ) -> NapiResult<Data> {
        if self.protocol != Protocol::OpenPgp {
            return Err(napi::Error::from_reason(format!(
                "Protocol {:?} not supported for decryption",
                self.protocol
            )));
        }

        let KeyInner::Secret(sk) = &secret_key.inner else {
            return Err(napi::Error::from_reason(
                "Decryption requires a secret key".to_string(),
            ));
        };

        let bytes = data.bytes.clone();
        let (msg, _) = Message::from_armor(Cursor::new(bytes.clone()))
            .or_else(|_| {
                Message::from_bytes(Cursor::new(bytes.clone())).map(|m| (m, Default::default()))
            })
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse message: {e}")))?;

        let pass = passphrase.unwrap_or_default();
        let pw = Password::from(pass.as_str());
        let mut decrypted = msg
            .decrypt(&pw, sk)
            .map_err(|e| napi::Error::from_reason(format!("Decryption error: {e}")))?;

        let literal_bytes = decrypted.as_data_vec().map_err(|e| {
            napi::Error::from_reason(format!("Failed to extract decrypted data: {e}"))
        })?;

        Ok(Data::from_bytes(Buffer::from(literal_bytes)))
    }

    #[napi]
    pub fn sign(
        &self,
        secret_key: &Key,
        passphrase: Option<String>,
        data: &Data,
    ) -> NapiResult<Data> {
        if self.protocol != Protocol::OpenPgp {
            return Err(napi::Error::from_reason(format!(
                "Protocol {:?} not supported for signing",
                self.protocol
            )));
        }

        let KeyInner::Secret(sk) = &secret_key.inner else {
            return Err(napi::Error::from_reason(
                "Signing requires a secret key".to_string(),
            ));
        };

        let mut rng = rand::thread_rng();
        let mut builder = MessageBuilder::from_bytes("data", data.bytes.clone());
        let pass = passphrase.unwrap_or_default();
        let pw = Password::from(pass.as_str());
        let _ = builder.sign(&sk.primary_key, pw, HashAlgorithm::Sha256);

        if self.armor {
            let armored = builder
                .to_armored_string(&mut rng, ArmorOptions::default())
                .map_err(|e| napi::Error::from_reason(format!("Armoring error: {e}")))?;
            Ok(Data::from_armored_string(armored))
        } else {
            let bytes = builder
                .to_vec(&mut rng)
                .map_err(|e| napi::Error::from_reason(format!("Serialization error: {e}")))?;
            Ok(Data::from_bytes(Buffer::from(bytes)))
        }
    }

    #[napi]
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

        let bytes = signed_data.bytes.clone();
        let (msg, _) = match Message::from_armor(Cursor::new(bytes.clone())).or_else(|_| {
            Message::from_bytes(Cursor::new(bytes.clone())).map(|m| (m, Default::default()))
        }) {
            Ok(m) => m,
            Err(e) => {
                return VerificationResult::failure(format!("Failed to parse signed data: {e}"));
            }
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

#[napi(js_name = "gpgGetExecutable")]
pub fn gpg_get_executable() -> String {
    if let Ok(repo) = gix::open(".") {
        if let Some(config) = repo.config_snapshot().string("gpg.program") {
            return config.to_string();
        }
    }
    "gpg".to_string()
}

#[napi(js_name = "gpgNthColumn")]
pub fn gpg_nth_column(line: String, col: u32) -> NapiResult<String> {
    let mut pos = 0;
    for _ in 0..col {
        match line[pos..].find(':') {
            Some(p) => pos += p + 1,
            None => {
                return Err(napi::Error::from_reason(
                    "Malformed output from gpg".to_string(),
                ));
            }
        }
    }
    let end_pos = line[pos..].find(':').map(|p| pos + p);
    match end_pos {
        Some(end) => Ok(line[pos..end].to_string()),
        None => Ok(line[pos..].to_string()),
    }
}

#[napi(js_name = "gpgGetUid")]
pub fn gpg_get_uid(fingerprint: String) -> String {
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
            if let Ok(col) = gpg_nth_column(line.to_string(), 9) {
                return col;
            }
        }
    }

    String::new()
}

#[napi(js_name = "gpgLookupKey")]
pub fn gpg_lookup_key(query: String) -> Vec<String> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args([
            "--batch",
            "--with-colons",
            "--fingerprint",
            "--list-keys",
            &query,
        ])
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
            if let Ok(col) = gpg_nth_column(line.to_string(), 9) {
                fingerprints.push(col);
            }
        }
    }

    fingerprints
}

#[napi(js_name = "gpgListSecretKeys")]
pub fn gpg_list_secret_keys() -> NapiResult<Vec<String>> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args([
            "--batch",
            "--with-colons",
            "--list-secret-keys",
            "--fingerprint",
        ])
        .output()
        .map_err(|e| napi::Error::from_reason(format!("Failed to execute gpg: {e}")))?;

    if !output.status.success() {
        return Err(napi::Error::from_reason(
            "gpg --list-secret-keys failed".to_string(),
        ));
    }

    let mut secret_keys = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.starts_with("fpr:") {
            if let Ok(col) = gpg_nth_column(line.to_string(), 9) {
                secret_keys.push(col);
            }
        }
    }

    Ok(secret_keys)
}

#[napi(js_name = "gpgEncryptToFile")]
pub fn gpg_encrypt_to_file(
    filename: String,
    recipient_fingerprint: String,
    key_is_trusted: bool,
    input: Buffer,
) -> NapiResult<()> {
    let exe = gpg_get_executable();
    let mut cmd = Command::new(exe);
    cmd.arg("--batch");
    if key_is_trusted {
        cmd.args(["--trust-model", "always"]);
    }
    cmd.args([
        "-o",
        &filename,
        "-r",
        &format!("0x{recipient_fingerprint}"),
        "-e",
    ]);
    cmd.stdin(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| napi::Error::from_reason(format!("Failed to spawn gpg: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_ref())
            .map_err(|e| napi::Error::from_reason(format!("Failed to write stdin: {e}")))?;
    }

    let status = child
        .wait()
        .map_err(|e| napi::Error::from_reason(format!("Failed to wait for gpg: {e}")))?;
    if !status.success() {
        return Err(napi::Error::from_reason("Failed to encrypt".to_string()));
    }

    Ok(())
}

#[napi(js_name = "gpgDecryptFromFile")]
pub fn gpg_decrypt_from_file(filename: String) -> NapiResult<Buffer> {
    let exe = gpg_get_executable();
    let output = Command::new(exe)
        .args(["-q", "-d", &filename])
        .output()
        .map_err(|e| napi::Error::from_reason(format!("Failed to execute gpg: {e}")))?;

    if !output.status.success() {
        return Err(napi::Error::from_reason("Failed to decrypt".to_string()));
    }

    Ok(Buffer::from(output.stdout))
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
        let d1 = Data::from_bytes(Buffer::from(b"hello".to_vec()));
        assert_eq!(d1.bytes().as_ref(), b"hello");
        assert!(!d1.is_armored());

        let d2 = Data::from_armored_string(
            "-----BEGIN PGP MESSAGE-----\n-----END PGP MESSAGE-----".to_string(),
        );
        assert!(d2.is_armored());
    }

    #[test]
    fn test_gpg_nth_column() {
        let line = "uid:u::::1395975462::AB97D6E3E5D8789988CA55E5F77D9E7397D05229::John Smith <jsmith@example.com>:";
        let col9 = gpg_nth_column(line.to_string(), 9).unwrap();
        assert_eq!(col9, "John Smith <jsmith@example.com>");

        let fpr_line = "fpr:::::::::7A399B2DB06D039020CD1CE1D0F3702D61489532:";
        let col9_fpr = gpg_nth_column(fpr_line.to_string(), 9).unwrap();
        assert_eq!(col9_fpr, "7A399B2DB06D039020CD1CE1D0F3702D61489532");
    }

    #[test]
    fn test_context_default() {
        let ctx = Context::default();
        assert_eq!(ctx.protocol(), Protocol::OpenPgp);
    }
}
