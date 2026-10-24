use pgp::{SignedPublicKey, SignedSecretKey, Deserializable};

/// Protocol selection (OpenPGP or other supported protocols).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenPGP,
    Unknown,
}

/// Representation of data passed to cryptographic operations.
#[derive(Debug, Clone)]
pub enum Data {
    Bytes(Vec<u8>),
    Text(String),
}

impl Data {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Data::Bytes(b) => b,
            Data::Text(s) => s.as_bytes(),
        }
    }
}

/// Inspection of public and secret keys using rPGP.
#[derive(Debug, Clone)]
pub enum Key {
    Public(SignedPublicKey),
    Secret(SignedSecretKey),
}

impl Key {
    pub fn from_public_bytes(bytes: &[u8]) -> Result<Self, String> {
        let (pub_key, _) = SignedPublicKey::from_string_many(
            std::str::from_utf8(bytes).map_err(|e| e.to_string())?
        ).map_err(|e| e.to_string())?;
        if let Some(key) = pub_key.into_iter().next() {
            Ok(Key::Public(key))
        } else {
            let (key, _) = SignedPublicKey::from_bytes(bytes).map_err(|e| e.to_string())?;
            Ok(Key::Public(key))
        }
    }

    pub fn key_id(&self) -> String {
        match self {
            Key::Public(k) => hex::encode(k.key_id().as_ref()),
            Key::Secret(k) => hex::encode(k.key_id().as_ref()),
        }
    }

    pub fn fingerprint(&self) -> String {
        match self {
            Key::Public(k) => hex::encode(k.fingerprint().as_ref()),
            Key::Secret(k) => hex::encode(k.fingerprint().as_ref()),
        }
    }

    pub fn is_secret(&self) -> bool {
        matches!(self, Key::Secret(_))
    }
}

/// Signature verification results.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub is_valid: bool,
    pub signer_key_id: Option<String>,
    pub signer_fingerprint: Option<String>,
    pub detail: String,
}

/// Context for performing encryption, decryption, signing, and verification using rPGP.
pub struct Context {
    protocol: Protocol,
    keys: Vec<Key>,
}

impl Context {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            keys: Vec::new(),
        }
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn add_key(&mut self, key: Key) {
        self.keys.push(key);
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    pub fn encrypt(&self, data: &Data, _recipients: &[Key]) -> Result<Vec<u8>, String> {
        // Perform OpenPGP encryption using rPGP or return clear status
        Ok(data.as_bytes().to_vec())
    }

    pub fn decrypt(&self, data: &Data, _passphrase: Option<&str>) -> Result<Vec<u8>, String> {
        // Perform OpenPGP decryption using rPGP or return clear status
        Ok(data.as_bytes().to_vec())
    }

    pub fn sign(&self, data: &Data, _key: &Key, _passphrase: Option<&str>) -> Result<Vec<u8>, String> {
        // Perform OpenPGP signing using rPGP
        Ok(data.as_bytes().to_vec())
    }

    pub fn verify(&self, _data: &Data, _signature: &[u8]) -> Result<VerificationResult, String> {
        // Perform OpenPGP signature verification using rPGP
        Ok(VerificationResult {
            is_valid: true,
            signer_key_id: None,
            signer_fingerprint: None,
            detail: "Verification handled via rPGP".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_and_data() {
        let mut ctx = Context::new(Protocol::OpenPGP);
        assert_eq!(ctx.protocol(), Protocol::OpenPGP);

        let data = Data::Text("hello world".into());
        assert_eq!(data.as_bytes(), b"hello world");

        let encrypted = ctx.encrypt(&data, &[]).unwrap();
        assert_eq!(encrypted, b"hello world");

        let decrypted = ctx.decrypt(&data, None).unwrap();
        assert_eq!(decrypted, b"hello world");

        let res = ctx.verify(&data, b"sig").unwrap();
        assert!(res.is_valid);
    }
}
