use napi_derive::napi;
#[napi]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyFormat {
    /// Raw key material: symmetric keys, or the raw public/private point for
    /// asymmetric keys.
    Raw,
    /// Raw secret bytes: symmetric secrets, HMAC keys, Argon2/PBKDF2 secrets.
    RawSecret,
    /// Raw public-key bytes (e.g. the 32-byte X25519 / Ed25519 public key).
    RawPublic,
    /// Raw seed bytes (post-quantum ML-DSA / ML-KEM seeds).
    RawSeed,
    /// `SubjectPublicKeyInfo` DER (asymmetric public keys).
    Spki,
    /// `PrivateKeyInfo` / PKCS#8 DER (asymmetric private keys).
    Pkcs8,
    /// JSON Web Key.
    Jwk,
}

impl KeyFormat {
    /// The canonical string identifier for this format.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyFormat::Raw => "raw",
            KeyFormat::RawSecret => "raw-secret",
            KeyFormat::RawPublic => "raw-public",
            KeyFormat::RawSeed => "raw-seed",
            KeyFormat::Spki => "spki",
            KeyFormat::Pkcs8 => "pkcs8",
            KeyFormat::Jwk => "jwk",
        }
    }
}

/// The set of formats an algorithm supports for import and export.
#[napi(object)]
pub struct KeyFormatSet {
    /// Formats accepted by `importKey`.
    pub import: Vec<String>,
    /// Formats accepted by `exportKey`.
    pub export: Vec<String>,
}

/// Every format this implementation understands.
#[napi]
pub fn all_key_formats() -> Vec<String> {
    use KeyFormat::*;
    [Raw, RawSecret, RawPublic, RawSeed, Spki, Pkcs8, Jwk]
        .iter()
        .map(KeyFormat::as_str)
        .map(str::to_string)
        .collect()
}

/// Returns the import/export formats supported for `algorithm`, or `None` if the
/// algorithm is unknown to this implementation.
///
/// Mirrors the Node.js Web Crypto "Key Formats" table.
#[napi]
pub fn key_formats(algorithm: String) -> Option<KeyFormatSet> {
    let (import, export): (&[KeyFormat], &[KeyFormat]) = match algorithm.as_str() {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "HMAC" => (
            &[KeyFormat::Jwk, KeyFormat::Raw, KeyFormat::RawSecret],
            &[KeyFormat::Jwk, KeyFormat::Raw, KeyFormat::RawSecret],
        ),
        "AES-OCB" | "ChaCha20-Poly1305" | "KMAC128" | "KMAC256" => (
            &[KeyFormat::Jwk, KeyFormat::RawSecret],
            &[KeyFormat::Jwk, KeyFormat::RawSecret],
        ),
        "Argon2d" | "Argon2i" | "Argon2id" => (&[KeyFormat::RawSecret], &[]),
        "ECDH" | "ECDSA" | "Ed25519" | "Ed448" | "X25519" | "X448" => (
            &[
                KeyFormat::Spki,
                KeyFormat::Pkcs8,
                KeyFormat::Jwk,
                KeyFormat::Raw,
                KeyFormat::RawPublic,
            ],
            &[
                KeyFormat::Spki,
                KeyFormat::Pkcs8,
                KeyFormat::Jwk,
                KeyFormat::Raw,
                KeyFormat::RawPublic,
            ],
        ),
        "HKDF" | "PBKDF2" => (&[KeyFormat::Raw, KeyFormat::RawSecret], &[]),
        "ML-DSA-44" | "ML-DSA-65" | "ML-DSA-87" | "ML-KEM-512" | "ML-KEM-768" | "ML-KEM-1024" => (
            &[
                KeyFormat::Spki,
                KeyFormat::Pkcs8,
                KeyFormat::Jwk,
                KeyFormat::RawPublic,
                KeyFormat::RawSeed,
            ],
            &[
                KeyFormat::Spki,
                KeyFormat::Pkcs8,
                KeyFormat::Jwk,
                KeyFormat::RawPublic,
                KeyFormat::RawSeed,
            ],
        ),
        "MLKEM768-P256" | "MLKEM768-X25519" | "MLKEM1024-P384" => (
            &[KeyFormat::Jwk, KeyFormat::RawPublic, KeyFormat::RawSeed],
            &[KeyFormat::Jwk, KeyFormat::RawPublic, KeyFormat::RawSeed],
        ),
        "RSA-OAEP" | "RSA-PSS" | "RSASSA-PKCS1-v1_5" => (
            &[KeyFormat::Spki, KeyFormat::Pkcs8, KeyFormat::Jwk],
            &[KeyFormat::Spki, KeyFormat::Pkcs8, KeyFormat::Jwk],
        ),
        _ => return None,
    };
    Some(KeyFormatSet {
        import: import.iter().map(KeyFormat::as_str).map(str::to_string).collect(),
        export: export.iter().map(KeyFormat::as_str).map(str::to_string).collect(),
    })
}
