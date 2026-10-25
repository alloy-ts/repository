use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum GpgProtocol {
    OpenPgp,
    Cms,
    GpgConf,
    Assuan,
    Unknown,
}

impl From<gpgme::Protocol> for GpgProtocol {
    fn from(p: gpgme::Protocol) -> Self {
        match p {
            gpgme::Protocol::OpenPgp => GpgProtocol::OpenPgp,
            gpgme::Protocol::Cms => GpgProtocol::Cms,
            gpgme::Protocol::GpgConf => GpgProtocol::GpgConf,
            gpgme::Protocol::Assuan => GpgProtocol::Assuan,
            _ => GpgProtocol::Unknown,
        }
    }
}

impl From<GpgProtocol> for gpgme::Protocol {
    fn from(p: GpgProtocol) -> Self {
        match p {
            GpgProtocol::OpenPgp => gpgme::Protocol::OpenPgp,
            GpgProtocol::Cms => gpgme::Protocol::Cms,
            GpgProtocol::GpgConf => gpgme::Protocol::GpgConf,
            GpgProtocol::Assuan => gpgme::Protocol::Assuan,
            GpgProtocol::Unknown => gpgme::Protocol::OpenPgp,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[napi]
pub enum GpgSignMode {
    Normal,
    Detached,
    Clear,
}

impl From<GpgSignMode> for gpgme::SignMode {
    fn from(m: GpgSignMode) -> Self {
        match m {
            GpgSignMode::Normal => gpgme::SignMode::Normal,
            GpgSignMode::Detached => gpgme::SignMode::Detached,
            GpgSignMode::Clear => gpgme::SignMode::Clear,
        }
    }
}

/// Buffer or file data wrapped for GPG operations.
#[napi(js_name = "GpgData")]
pub struct GpgData {
    pub(crate) inner: gpgme::Data<'static>,
}

#[napi]
impl GpgData {
    /// Create an empty in-memory GpgData buffer.
    #[napi(constructor)]
    pub fn new() -> Result<Self> {
        let data = gpgme::Data::new().map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner: data })
    }

    /// Create GpgData from a byte buffer.
    #[napi(factory)]
    pub fn from_buffer(buf: Buffer) -> Result<Self> {
        let data = gpgme::Data::from_bytes(buf.as_ref().to_vec())
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner: data })
    }

    /// Create GpgData from a UTF-8 string.
    #[napi(factory)]
    pub fn from_string(text: String) -> Result<Self> {
        let data = gpgme::Data::from_bytes(text.into_bytes())
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner: data })
    }

    /// Create GpgData referencing a file path.
    #[napi(factory)]
    pub fn from_file(path: String) -> Result<Self> {
        let data = gpgme::Data::load(path)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner: data })
    }

    /// Read all accumulated data into a Buffer.
    #[napi]
    pub fn to_buffer(&mut self) -> Result<Buffer> {
        let mut bytes = Vec::new();
        self.inner
            .read_to_end(&mut bytes)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Buffer::from(bytes))
    }

    /// Read all accumulated data as a UTF-8 String.
    #[napi]
    pub fn to_string(&mut self) -> Result<String> {
        let buf = self.to_buffer()?;
        String::from_utf8(buf.as_ref().to_vec())
            .map_err(|e| napi::Error::from_reason(format!("Invalid UTF-8 data: {e}")))
    }
}

#[napi(object)]
pub struct GpgUserId {
    pub id: String,
    pub name: String,
    pub email: String,
    pub comment: String,
    pub validity: String,
    pub is_revoked: bool,
    pub is_invalid: bool,
}

fn convert_user_id(uid: &gpgme::UserId) -> GpgUserId {
    GpgUserId {
        id: uid.id().unwrap_or("").to_string(),
        name: uid.name().unwrap_or("").to_string(),
        email: uid.email().unwrap_or("").to_string(),
        comment: uid.comment().unwrap_or("").to_string(),
        validity: format!("{:?}", uid.validity()),
        is_revoked: uid.is_revoked(),
        is_invalid: uid.is_invalid(),
    }
}

#[napi(object)]
pub struct GpgSubkey {
    pub id: String,
    pub fingerprint: String,
    pub can_encrypt: bool,
    pub can_sign: bool,
    pub can_certify: bool,
    pub can_authenticate: bool,
    pub is_disabled: bool,
    pub is_invalid: bool,
    pub is_revoked: bool,
    pub is_expired: bool,
    pub is_secret: bool,
    pub pubkey_algo: String,
    pub length: u32,
    pub creation_time: Option<f64>,
    pub expires: Option<f64>,
}

fn convert_subkey(sub: &gpgme::Subkey) -> GpgSubkey {
    let creation_time = sub
        .creation_time()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    let expires = sub
        .expiration_time()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    GpgSubkey {
        id: sub.id().unwrap_or("").to_string(),
        fingerprint: sub.fingerprint().unwrap_or("").to_string(),
        can_encrypt: sub.can_encrypt(),
        can_sign: sub.can_sign(),
        can_certify: sub.can_certify(),
        can_authenticate: sub.can_authenticate(),
        is_disabled: sub.is_disabled(),
        is_invalid: sub.is_invalid(),
        is_revoked: sub.is_revoked(),
        is_expired: sub.is_expired(),
        is_secret: sub.is_secret(),
        pubkey_algo: format!("{:?}", sub.algorithm()),
        length: sub.length() as u32,
        creation_time,
        expires,
    }
}

#[napi(object)]
pub struct GpgKey {
    pub id: String,
    pub fingerprint: String,
    pub can_encrypt: bool,
    pub can_sign: bool,
    pub can_certify: bool,
    pub can_authenticate: bool,
    pub is_disabled: bool,
    pub is_invalid: bool,
    pub is_revoked: bool,
    pub is_expired: bool,
    pub has_secret: bool,
    pub protocol: GpgProtocol,
    pub issuer_serial: Option<String>,
    pub issuer_name: Option<String>,
    pub chain_id: Option<String>,
    pub user_ids: Vec<GpgUserId>,
    pub subkeys: Vec<GpgSubkey>,
}

fn convert_key(k: &gpgme::Key) -> GpgKey {
    let subkeys: Vec<GpgSubkey> = k.subkeys().map(|s| convert_subkey(&s)).collect();
    let user_ids: Vec<GpgUserId> = k.user_ids().map(|u| convert_user_id(&u)).collect();

    let primary_id = k.id().unwrap_or("").to_string();
    let primary_fp = k.fingerprint().unwrap_or("").to_string();

    GpgKey {
        id: primary_id,
        fingerprint: primary_fp,
        can_encrypt: k.can_encrypt(),
        can_sign: k.can_sign(),
        can_certify: k.can_certify(),
        can_authenticate: k.can_authenticate(),
        is_disabled: k.is_disabled(),
        is_invalid: k.is_invalid(),
        is_revoked: k.is_revoked(),
        is_expired: k.is_expired(),
        has_secret: k.has_secret(),
        protocol: k.protocol().into(),
        issuer_serial: k.issuer_serial().ok().map(|s| s.to_string()),
        issuer_name: k.issuer_name().ok().map(|s| s.to_string()),
        chain_id: k.chain_id().ok().map(|s| s.to_string()),
        user_ids,
        subkeys,
    }
}

#[napi(object)]
pub struct GpgNotation {
    pub name: String,
    pub value: String,
    pub is_human_readable: bool,
    pub is_critical: bool,
}

#[napi(object)]
pub struct GpgSignature {
    pub summary: String,
    pub fingerprint: String,
    pub status: String,
    pub creation_time: Option<f64>,
    pub exp_time: Option<f64>,
    pub is_wrong_key_usage: bool,
    pub pka_trust: u32,
    pub pka_address: Option<String>,
    pub notations: Vec<GpgNotation>,
}

fn convert_signature(s: &gpgme::Signature) -> GpgSignature {
    let notations: Vec<GpgNotation> = s
        .notations()
        .map(|n| GpgNotation {
            name: n.name().unwrap_or("").to_string(),
            value: n.value().unwrap_or("").to_string(),
            is_human_readable: n.is_human_readable(),
            is_critical: n.is_critical(),
        })
        .collect();

    let creation_time = s
        .creation_time()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    let exp_time = s
        .expiration_time()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());

    GpgSignature {
        summary: format!("{:?}", s.summary()),
        fingerprint: s.fingerprint().unwrap_or("").to_string(),
        status: format!("{:?}", s.status()),
        creation_time,
        exp_time,
        is_wrong_key_usage: s.is_wrong_key_usage(),
        pka_trust: format!("{:?}", s.pka_trust()).len() as u32,
        pka_address: s.pka_address().ok().map(|a| a.to_string()),
        notations,
    }
}

#[napi(object)]
pub struct GpgVerificationResult {
    pub file_name: Option<String>,
    pub signatures: Vec<GpgSignature>,
}

fn convert_verification_result(res: &gpgme::VerificationResult) -> GpgVerificationResult {
    let signatures: Vec<GpgSignature> = res.signatures().map(|s| convert_signature(&s)).collect();

    GpgVerificationResult {
        file_name: res.filename().ok().map(|s| s.to_string()),
        signatures,
    }
}

/// NAPI wrapper around `gpgme::Context`.
#[napi(js_name = "GpgContext")]
pub struct GpgContext {
    inner: gpgme::Context,
}

#[napi]
impl GpgContext {
    /// Create a new GPG context (defaults to OpenPGP protocol).
    #[napi(constructor)]
    pub fn new(protocol: Option<GpgProtocol>) -> Result<Self> {
        gpgme::init();
        let proto = protocol.map(Into::into).unwrap_or(gpgme::Protocol::OpenPgp);
        let ctx =
            gpgme::Context::from_protocol(proto).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner: ctx })
    }

    /// Get current protocol.
    #[napi]
    pub fn protocol(&self) -> GpgProtocol {
        self.inner.protocol().into()
    }

    /// Enable or disable ASCII armor output.
    #[napi]
    pub fn set_armor(&mut self, armor: bool) {
        self.inner.set_armor(armor);
    }

    /// Check if ASCII armor output is enabled.
    #[napi]
    pub fn armor(&self) -> bool {
        self.inner.armor()
    }

    /// Enable or disable canonical text mode.
    #[napi]
    pub fn set_textmode(&mut self, textmode: bool) {
        self.inner.set_text_mode(textmode);
    }

    /// Check if text mode is enabled.
    #[napi]
    pub fn textmode(&self) -> bool {
        self.inner.text_mode()
    }

    /// Enable or disable offline mode (disables network lookups like CRL/OCSP/WKD).
    #[napi]
    pub fn set_offline(&mut self, offline: bool) {
        self.inner.set_offline(offline);
    }

    /// Check if offline mode is enabled.
    #[napi]
    pub fn offline(&self) -> bool {
        self.inner.offline()
    }

    /// Find a key by pattern (keyid, fingerprint, email, or user id).
    #[napi]
    pub fn find_key(&mut self, pattern: String) -> Result<Option<GpgKey>> {
        match self.inner.get_key(pattern) {
            Ok(key) => Ok(Some(convert_key(&key))),
            Err(e) if e.code() == gpgme::Error::EOF.code() => Ok(None),
            Err(e) => Err(napi::Error::from_reason(e.to_string())),
        }
    }

    /// List public keys matching patterns.
    #[napi]
    pub fn find_keys(&mut self, patterns: Vec<String>) -> Result<Vec<GpgKey>> {
        let keys = self
            .inner
            .find_keys(patterns)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let mut out = Vec::new();
        for res in keys {
            let key = res.map_err(|e| napi::Error::from_reason(e.to_string()))?;
            out.push(convert_key(&key));
        }
        Ok(out)
    }

    /// List secret keys matching patterns.
    #[napi]
    pub fn find_secret_keys(&mut self, patterns: Vec<String>) -> Result<Vec<GpgKey>> {
        let keys = self
            .inner
            .find_secret_keys(patterns)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let mut out = Vec::new();
        for res in keys {
            let key = res.map_err(|e| napi::Error::from_reason(e.to_string()))?;
            out.push(convert_key(&key));
        }
        Ok(out)
    }

    /// Add a key as a signer for signing operations.
    #[napi]
    pub fn add_signer(&mut self, key_pattern: String) -> Result<()> {
        let key = self
            .inner
            .get_key(key_pattern)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        self.inner
            .add_signer(&key)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Clear signers list.
    #[napi]
    pub fn clear_signers(&mut self) {
        self.inner.clear_signers();
    }

    /// Encrypt plaintext for recipients (key fingerprints/patterns).
    #[napi]
    pub fn encrypt(
        &mut self,
        recipients: Vec<String>,
        plaintext: &mut GpgData,
        ciphertext: &mut GpgData,
    ) -> Result<()> {
        let mut keys = Vec::new();
        for pattern in recipients {
            let key = self
                .inner
                .get_key(pattern)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
            keys.push(key);
        }
        self.inner
            .encrypt(&keys, &mut plaintext.inner, &mut ciphertext.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Encrypt plaintext symmetrically using passphrase.
    #[napi]
    pub fn encrypt_symmetric(
        &mut self,
        plaintext: &mut GpgData,
        ciphertext: &mut GpgData,
    ) -> Result<()> {
        self.inner
            .encrypt_symmetric(&mut plaintext.inner, &mut ciphertext.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Decrypt ciphertext into plaintext.
    #[napi]
    pub fn decrypt(
        &mut self,
        ciphertext: &mut GpgData,
        plaintext: &mut GpgData,
    ) -> Result<()> {
        self.inner
            .decrypt(&mut ciphertext.inner, &mut plaintext.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Sign data using current signers.
    #[napi]
    pub fn sign(
        &mut self,
        mode: GpgSignMode,
        plaintext: &mut GpgData,
        signature: &mut GpgData,
    ) -> Result<()> {
        self.inner
            .sign(mode.into(), &mut plaintext.inner, &mut signature.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Verify signature for plaintext data.
    #[napi]
    pub fn verify(
        &mut self,
        signature: &mut GpgData,
        signed_text: Option<&mut GpgData>,
        output_plain: Option<&mut GpgData>,
    ) -> Result<GpgVerificationResult> {
        let mut dummy_output = gpgme::Data::new().map_err(|e| napi::Error::from_reason(e.to_string()))?;

        let res = match (signed_text, output_plain) {
            (Some(text), _) => self
                .inner
                .verify_opaque(&mut signature.inner, &mut text.inner)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?,
            (None, Some(out)) => self
                .inner
                .verify_detached(&mut signature.inner, &mut out.inner)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?,
            (None, None) => self
                .inner
                .verify_detached(&mut signature.inner, &mut dummy_output)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?,
        };

        Ok(convert_verification_result(&res))
    }

    /// Import GPG keys from a key data buffer.
    #[napi]
    pub fn import_keys(&mut self, key_data: &mut GpgData) -> Result<u32> {
        let result = self
            .inner
            .import(&mut key_data.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(result.imported() as u32)
    }

    /// Export GPG key(s) by pattern to an output buffer.
    #[napi]
    pub fn export_keys(
        &mut self,
        pattern: Option<String>,
        output_data: &mut GpgData,
    ) -> Result<()> {
        let pat: Vec<String> = pattern.into_iter().collect();
        self.inner
            .export(pat, gpgme::ExportMode::empty(), &mut output_data.inner)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Generate a new keypair using key generation parameters.
    #[napi]
    pub fn create_key(&mut self, userid: String, algo: Option<String>) -> Result<()> {
        let algorithm = algo.as_deref().unwrap_or("default");
        self.inner
            .create_key(&userid, algorithm, std::time::Duration::ZERO)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Standalone GPG Helper Functions (matching requested C++ utilities)
// ---------------------------------------------------------------------------

/// Retrieve the configured GPG executable path (`gpg.program` in git config or `"gpg"`).
#[napi(js_name = "gpgGetExecutable")]
pub fn gpg_get_executable() -> String {
    if let Ok(repo) = gix::discover(".") {
        if let Some(config) = repo.config_snapshot().string("gpg.program") {
            return String::from_utf8_lossy(&config).into_owned();
        }
    }
    "gpg".to_string()
}

/// Given a key fingerprint, return the key's primary UID (e.g. `"John Smith <jsmith@example.com>"`).
#[napi(js_name = "gpgGetUid")]
pub fn gpg_get_uid(fingerprint: String) -> Result<String> {
    gpgme::init();
    let mut ctx = gpgme::Context::from_protocol(gpgme::Protocol::OpenPgp)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let pattern = if fingerprint.starts_with("0x") || fingerprint.starts_with("0X") {
        fingerprint
    } else {
        format!("0x{fingerprint}")
    };

    match ctx.get_key(pattern) {
        Ok(key) => {
            if let Some(uid) = key.user_ids().next() {
                if let (Ok(name), Ok(email)) = (uid.name(), uid.email()) {
                    if !name.is_empty() && !email.is_empty() {
                        return Ok(format!("{name} <{email}>"));
                    }
                }
                if let Ok(id) = uid.id() {
                    return Ok(id.to_string());
                }
            }
            Ok(String::new())
        }
        Err(_) => Ok(String::new()),
    }
}

/// Return a list of fingerprints of public keys matching the given search query.
#[napi(js_name = "gpgLookupKey")]
pub fn gpg_lookup_key(query: String) -> Result<Vec<String>> {
    gpgme::init();
    let mut ctx = gpgme::Context::from_protocol(gpgme::Protocol::OpenPgp)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let keys = ctx
        .find_keys(vec![query])
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut fps = Vec::new();
    for item in keys {
        if let Ok(key) = item {
            if let Ok(fp) = key.fingerprint() {
                fps.push(fp.to_string());
            }
        }
    }
    Ok(fps)
}

/// Return a list of fingerprints of all secret keys in the keyring.
#[napi(js_name = "gpgListSecretKeys")]
pub fn gpg_list_secret_keys() -> Result<Vec<String>> {
    gpgme::init();
    let mut ctx = gpgme::Context::from_protocol(gpgme::Protocol::OpenPgp)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let keys = ctx
        .find_secret_keys(Vec::<String>::new())
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut fps = Vec::new();
    for item in keys {
        if let Ok(key) = item {
            if let Ok(fp) = key.fingerprint() {
                fps.push(fp.to_string());
            }
        }
    }
    Ok(fps)
}

/// Encrypt raw input data directly to a file for a recipient fingerprint.
#[napi(js_name = "gpgEncryptToFile")]
pub fn gpg_encrypt_to_file(
    filename: String,
    recipient_fingerprint: String,
    _key_is_trusted: bool,
    data: Buffer,
) -> Result<()> {
    gpgme::init();
    let mut ctx = gpgme::Context::from_protocol(gpgme::Protocol::OpenPgp)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let pattern = if recipient_fingerprint.starts_with("0x") || recipient_fingerprint.starts_with("0X") {
        recipient_fingerprint
    } else {
        format!("0x{recipient_fingerprint}")
    };

    let key = ctx
        .get_key(pattern)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut plain = gpgme::Data::from_bytes(data.as_ref().to_vec())
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let mut cipher = gpgme::Data::new().map_err(|e| napi::Error::from_reason(e.to_string()))?;

    ctx.encrypt(&[key], &mut plain, &mut cipher)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut out_bytes = Vec::new();
    cipher
        .read_to_end(&mut out_bytes)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut file = File::create(Path::new(&filename))
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    file.write_all(&out_bytes)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    Ok(())
}

/// Decrypt an encrypted file and return decrypted bytes.
#[napi(js_name = "gpgDecryptFromFile")]
pub fn gpg_decrypt_from_file(filename: String) -> Result<Buffer> {
    gpgme::init();
    let mut ctx = gpgme::Context::from_protocol(gpgme::Protocol::OpenPgp)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut cipher = gpgme::Data::load(filename)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let mut plain = gpgme::Data::new().map_err(|e| napi::Error::from_reason(e.to_string()))?;

    ctx.decrypt(&mut cipher, &mut plain)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    let mut out_bytes = Vec::new();
    plain
        .read_to_end(&mut out_bytes)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;

    Ok(Buffer::from(out_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpg_data_and_helpers() {
        let mut data = GpgData::from_string("hello gpg".to_string()).unwrap();
        assert_eq!(data.to_string().unwrap(), "hello gpg");

        let exe = gpg_get_executable();
        assert!(!exe.is_empty());
    }
}
