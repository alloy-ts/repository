use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use sequoia_openpgp::cert::Cert;
use sequoia_openpgp::crypto::{KeyPair, Password, SessionKey};
use sequoia_openpgp::parse::stream::{
    DecryptorBuilder, DecryptionHelper, MessageLayer, MessageStructure, VerificationError,
    VerificationHelper,
};
use sequoia_openpgp::parse::Parse;
use sequoia_openpgp::policy::StandardPolicy;
use sequoia_openpgp::serialize::stream::{Encryptor2, LiteralWriter, Message, Recipient, Signer};
use sequoia_openpgp::types::SymmetricAlgorithm;
use sequoia_openpgp::Fingerprint;

#[derive(Debug)]
pub enum PgpError {
    Seq(sequoia_openpgp::Error),
    Io(std::io::Error),
    Anyhow(String),
    UnsupportedHash(String),
    Verify(String),
}

impl From<sequoia_openpgp::Error> for PgpError {
    fn from(e: sequoia_openpgp::Error) -> Self {
        PgpError::Seq(e)
    }
}

impl From<std::io::Error> for PgpError {
    fn from(e: std::io::Error) -> Self {
        PgpError::Io(e)
    }
}

impl From<anyhow::Error> for PgpError {
    fn from(e: anyhow::Error) -> Self {
        PgpError::Anyhow(e.to_string())
    }
}

impl From<PgpError> for napi::Error {
    fn from(e: PgpError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

impl std::fmt::Display for PgpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgpError::Seq(e) => write!(f, "openpgp error: {e}"),
            PgpError::Io(e) => write!(f, "io error: {e}"),
            PgpError::Anyhow(s) => write!(f, "openpgp error: {s}"),
            PgpError::UnsupportedHash(a) => write!(f, "unsupported hash algorithm: {a}"),
            PgpError::Verify(s) => write!(f, "signature verification failed: {s}"),
        }
    }
}

fn parse_certs(buffers: &[Buffer]) -> Result<Vec<Cert>, PgpError> {
    let mut certs = Vec::with_capacity(buffers.len());
    for b in buffers {
        certs.push(Cert::from_bytes(b).map_err(PgpError::from)?);
    }
    Ok(certs)
}

/// Symmetric encrypt `plaintext` under `pass` (mirrors `gpg -c --passphrase`).
pub fn symmetric_encrypt(pass: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, PgpError> {
    let password = Password::from(pass);
    let mut sink: Vec<u8> = Vec::new();
    {
        let message = Message::new(&mut sink);
        let encryptor = Encryptor2::with_passwords(message, vec![password]);
        let mut literal = LiteralWriter::new(Message::new(encryptor))
            .build()
            .map_err(PgpError::from)?;
        literal.write_all(plaintext).map_err(PgpError::from)?;
        literal.finalize().map_err(PgpError::from)?;
    }
    Ok(sink)
}

/// Symmetric decrypt `ciphertext` with `pass` (mirrors `gpg -d --passphrase`).
pub fn symmetric_decrypt(pass: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, PgpError> {
    let password = Password::from(pass);
    let signers = Rc::new(RefCell::new(Vec::new()));
    let helper = DecryptHelper {
        passwords: vec![password],
        secret_keys: Vec::new(),
        accept: Vec::new(),
        signers: signers.clone(),
    };
    let policy = StandardPolicy::new();
    let mut decrypted = Vec::new();
    let mut reader = DecryptorBuilder::from_bytes(ciphertext)
        .map_err(PgpError::from)?
        .with_policy(&policy, None, helper)
        .map_err(PgpError::from)?;
    reader.read_to_end(&mut decrypted).map_err(PgpError::from)?;
    Ok(decrypted)
}

/// Encrypt `plaintext` to `recipients` (public certs), optionally signing with
/// `sign_key` (a cert containing a signing-capable secret key). Mirrors
/// `gpg -se -r <recp> [-u <signer>]`.
pub fn public_encrypt(
    recipients: &[Cert],
    sign_key: Option<&Cert>,
    plaintext: &[u8],
) -> Result<Vec<u8>, PgpError> {
    let policy = StandardPolicy::new();

    // Collect recipient keys (transport/storage encryption capable).
    let mut recipient_keys: Vec<Recipient> = Vec::new();
    for cert in recipients {
        for ka in cert.keys().with_policy(&policy, None).supported() {
            if let Some(flags) = ka.key_flags() {
                if flags.for_transport_encryption() || flags.for_storage_encryption() {
                    recipient_keys.push(Recipient::from(ka.key()));
                }
            }
        }
    }

    // Pick a signing-capable secret key from `sign_key`, if any.
    let signer: Option<KeyPair> = if let Some(sk) = sign_key {
        sk.keys()
            .with_policy(&policy, None)
            .supported()
            .secret()
            .find(|ka| ka.key_flags().map_or(false, |f| f.for_signing()))
            .and_then(|ka| ka.key().clone().into_keypair().ok())
    } else {
        None
    };

    let mut sink: Vec<u8> = Vec::new();
    {
        let message = Message::new(&mut sink);
        let encryptor = Encryptor2::for_recipients(message, recipient_keys);
        match signer {
            Some(kp) => {
                let signer = Signer::new(Message::new(encryptor), kp);
                let mut literal = LiteralWriter::new(Message::new(signer))
                    .build()
                    .map_err(PgpError::from)?;
                literal.write_all(plaintext).map_err(PgpError::from)?;
                literal.finalize().map_err(PgpError::from)?;
            }
            None => {
                let mut literal = LiteralWriter::new(Message::new(encryptor))
                    .build()
                    .map_err(PgpError::from)?;
                literal.write_all(plaintext).map_err(PgpError::from)?;
                literal.finalize().map_err(PgpError::from)?;
            }
        }
    }
    Ok(sink)
}

struct DecryptHelper {
    passwords: Vec<Password>,
    secret_keys: Vec<Cert>,
    accept: Vec<String>,
    signers: Rc<RefCell<Vec<String>>>,
}

impl VerificationHelper for DecryptHelper {
    fn get_certs(
        &mut self,
        _ids: &[sequoia_openpgp::KeyHandle],
    ) -> sequoia_openpgp::Result<Vec<Cert>> {
        Ok(self.secret_keys.clone())
    }

    fn check(&mut self, structure: MessageStructure) -> sequoia_openpgp::Result<()> {
        let mut found = Vec::new();
        for layer in structure.iter() {
            if let MessageLayer::SignatureGroup { results } = layer {
                eprintln!("SIGGROUP results={}", results.len());
                for result in results {
                    match result {
                        Ok(good) => {
                            eprintln!("  GOOD sig by {}", good.ka.fingerprint().to_hex());
                            found.push(good.ka.cert().fingerprint().to_hex());
                        }
                        Err(e) => eprintln!("  SIGERR {:?}", e),
                    }
                }
            }
        }
        if !self.accept.is_empty() {
            let ok = found.iter().any(|fp| {
                self.accept
                    .iter()
                    .any(|a| fp == a || fp.ends_with(a))
            });
            if !ok {
                return Err(anyhow::anyhow!(
                    "no accepted signer found"
                ));
            }
        }
        self.signers.borrow_mut().extend(found);
        Ok(())
    }
}

impl DecryptionHelper for DecryptHelper {
    fn decrypt<D>(
        &mut self,
        pkesks: &[sequoia_openpgp::packet::PKESK],
        skesks: &[sequoia_openpgp::packet::SKESK],
        _sym_algo: Option<SymmetricAlgorithm>,
        mut decrypt: D,
    ) -> sequoia_openpgp::Result<Option<Fingerprint>>
    where
        D: FnMut(SymmetricAlgorithm, &SessionKey) -> bool,
    {
        let policy = StandardPolicy::new();
        for pkesk in pkesks {
            for cert in &self.secret_keys {
                for ka in cert
                    .keys()
                    .with_policy(&policy, None)
                    .supported()
                    .secret()
                {
                    if let Some(flags) = ka.key_flags() {
                        if flags.for_transport_encryption() || flags.for_storage_encryption() {
                            if let Ok(mut kp) = ka.key().clone().into_keypair() {
                                if let Some((algo, session_key)) =
                                    pkesk.decrypt(&mut kp, _sym_algo)
                                {
                                    if decrypt(algo, &session_key) {
                                        return Ok(Some(cert.fingerprint()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for skesk in skesks {
            for pw in &self.passwords {
                if let Ok((algo, session_key)) = skesk.decrypt(pw) {
                    if decrypt(algo, &session_key) {
                        return Ok(None);
                    }
                }
            }
        }
        // No key or passphrase matched: decryption genuinely failed.
        Err(anyhow::anyhow!("no available key or passphrase"))
    }
}

/// Decrypt `ciphertext`, using `secret_keys` and/or `passwords`. `accept`, if
/// non-empty, requires at least one verified signer fingerprint/keyid to match.
/// Returns `(plaintext, verified_signer_fingerprints)`.
pub fn private_decrypt(
    ciphertext: &[u8],
    secret_keys: &[Cert],
    passwords: &[Vec<u8>],
    accept: &[String],
) -> Result<(Vec<u8>, Vec<String>), PgpError> {
    let policy = StandardPolicy::new();
    let signers = Rc::new(RefCell::new(Vec::new()));
    let helper = DecryptHelper {
        passwords: passwords.iter().map(|p| Password::from(p.as_slice())).collect(),
        secret_keys: secret_keys.to_vec(),
        accept: accept.to_vec(),
        signers: signers.clone(),
    };
    let mut decrypted = Vec::new();
    let mut reader = DecryptorBuilder::from_bytes(ciphertext)
        .map_err(PgpError::from)?
        .with_policy(&policy, None, helper)
        .map_err(PgpError::from)?;
    reader.read_to_end(&mut decrypted).map_err(PgpError::from)?;
    Ok((decrypted, signers.borrow().clone()))
}

/// Cryptographically-strong random bytes (mirrors `gpg --gen-rand`).
pub fn random_bytes(n: usize) -> Vec<u8> {
    (0..n).map(|_| rand::random::<u8>()).collect()
}

/// Hex digest of `data` for the named algorithm (mirrors `gpg --print-md`).
pub fn hex_hash(algo: &str, data: &[u8]) -> Result<String, PgpError> {
    use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};
    let upper = algo.to_ascii_uppercase();
    let out: Vec<u8> = match upper.as_str() {
        "SHA224" => {
            let mut h = Sha224::new();
            h.update(data);
            h.finalize().to_vec()
        }
        "SHA256" => {
            let mut h = Sha256::new();
            h.update(data);
            h.finalize().to_vec()
        }
        "SHA384" => {
            let mut h = Sha384::new();
            h.update(data);
            h.finalize().to_vec()
        }
        "SHA512" => {
            let mut h = Sha512::new();
            h.update(data);
            h.finalize().to_vec()
        }
        _ => return Err(PgpError::UnsupportedHash(algo.to_string())),
    };
    Ok(hex::encode(out))
}

#[napi(object)]
pub struct PgpDecryptResult {
    pub plaintext: Buffer,
    pub signers: Vec<String>,
}

/// NAPI-exposed GPG-compatible crypto primitives.
#[napi(js_name = "Pgp")]
pub struct Pgp;

#[napi]
impl Pgp {
    #[napi(js_name = "symmetricEncrypt")]
    pub fn napi_symmetric_encrypt(
        passphrase: Buffer,
        plaintext: Buffer,
    ) -> napi::Result<Buffer> {
        Ok(Buffer::from(symmetric_encrypt(
            passphrase.as_ref(),
            plaintext.as_ref(),
        )?))
    }

    #[napi(js_name = "symmetricDecrypt")]
    pub fn napi_symmetric_decrypt(
        passphrase: Buffer,
        ciphertext: Buffer,
    ) -> napi::Result<Buffer> {
        Ok(Buffer::from(symmetric_decrypt(
            passphrase.as_ref(),
            ciphertext.as_ref(),
        )?))
    }

    /// Encrypt `plaintext` to the given recipient public keys (armored PGP
    /// certificates). If `sign_key` (a PGP cert containing a signing secret
    /// key) is supplied, the message is also signed.
    #[napi(js_name = "publicEncrypt")]
    pub fn napi_public_encrypt(
        recipients: Vec<Buffer>,
        sign_key: Option<Buffer>,
        plaintext: Buffer,
    ) -> napi::Result<Buffer> {
        let recs = parse_certs(&recipients)?;
        let sign = match sign_key {
            Some(b) => Some(Cert::from_bytes(&b).map_err(PgpError::from)?),
            None => None,
        };
        Ok(Buffer::from(public_encrypt(
            &recs,
            sign.as_ref(),
            plaintext.as_ref(),
        )?))
    }

    /// Decrypt `ciphertext` using the supplied secret keys and/or passphrases.
    /// When `accepted_signers` is non-empty, decryption fails unless at least
    /// one verified signer matches. Returns the plaintext and the list of
    /// verified signer fingerprints.
    #[napi(js_name = "privateDecrypt")]
    pub fn napi_private_decrypt(
        ciphertext: Buffer,
        secret_keys: Vec<Buffer>,
        passwords: Option<Vec<Buffer>>,
        accepted_signers: Option<Vec<String>>,
    ) -> napi::Result<PgpDecryptResult> {
        let secs = parse_certs(&secret_keys)?;
        let pws: Vec<Vec<u8>> = passwords
            .unwrap_or_default()
            .iter()
            .map(|b| b.to_vec())
            .collect();
        let accept = accepted_signers.unwrap_or_default();
        let (pt, signers) = private_decrypt(ciphertext.as_ref(), &secs, &pws, &accept)?;
        Ok(PgpDecryptResult {
            plaintext: Buffer::from(pt),
            signers,
        })
    }

    #[napi(js_name = "randomBytes")]
    pub fn napi_random_bytes(n: u32) -> Buffer {
        Buffer::from(random_bytes(n as usize))
    }

    #[napi(js_name = "hexHash")]
    pub fn napi_hex_hash(algo: String, data: Buffer) -> napi::Result<String> {
        Ok(hex_hash(&algo, data.as_ref())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sequoia_openpgp::cert::CertBuilder;

    fn make_cert(userid: &str) -> Cert {
        let (cert, _) = CertBuilder::new()
            .add_userid(userid)
            .add_subkey(
                sequoia_openpgp::types::KeyFlags::empty().set_transport_encryption(),
                None,
                None,
            )
            .add_signing_subkey()
            .generate()
            .expect("generate cert");
        cert
    }

    #[test]
    fn symmetric_roundtrip() {
        let pt = b"hello gcrypt";
        let ct = symmetric_encrypt(b"pass", pt).unwrap();
        assert_ne!(ct, pt.to_vec());
        let dec = symmetric_decrypt(b"pass", &ct).unwrap();
        assert_eq!(dec, pt);
        assert!(symmetric_decrypt(b"wrong", &ct).is_err());
    }

    #[test]
    fn public_encrypt_sign_and_decrypt_verify() {
        let cert = make_cert("alice@example.com");
        let pt = b"secret manifest data";

        // Encrypt to alice and sign as alice.
        let ct = public_encrypt(&[cert.clone()], Some(&cert), pt).unwrap();
        assert!(!ct.is_empty());

        // Decrypt with alice's secret key; verify the signature.
        let (dec, signers) = private_decrypt(&ct, &[cert.clone()], &[], &[]).unwrap();
        assert_eq!(dec, pt);
        assert!(
            !signers.is_empty(),
            "expected at least one verified signer"
        );

        // accepted_signers restricting to alice's fingerprint must succeed.
        let fp = cert.fingerprint().to_hex();
        let (dec2, _) = private_decrypt(&ct, &[cert.clone()], &[], &[fp]).unwrap();
        assert_eq!(dec2, pt);

        // A mismatch on accepted_signers must fail.
        assert!(private_decrypt(&ct, &[cert], &[], &["deadbeef".to_string()]).is_err());
    }

    #[test]
    fn random_and_hash() {
        let r = random_bytes(32);
        assert_eq!(r.len(), 32);
        let h = hex_hash("SHA256", b"abc").unwrap();
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(hex_hash("MD5", b"x").is_err());
    }
}
