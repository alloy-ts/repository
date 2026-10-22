use napi::Result;
use napi_derive::napi;

/// Key format options for WebCrypto import/export operations.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct KeyFormatSupport {
    pub format: String,
    pub algorithm: String,
    pub supports_import: bool,
    pub supports_export: bool,
}

#[napi]
pub fn is_format_supported(format: String, algorithm: String, is_import: bool) -> Result<bool> {
    let fmt = format.to_lowercase();
    let alg = algorithm.to_uppercase();

    let supported = match alg.as_str() {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "HMAC" => {
            matches!(fmt.as_str(), "jwk" | "raw" | "raw-secret")
        }
        "AES-OCB" | "CHACHA20-POLY1305" | "KMAC128" | "KMAC256" => {
            matches!(fmt.as_str(), "jwk" | "raw-secret")
        }
        "ARGON2D" | "ARGON2I" | "ARGON2ID" | "HKDF" | "PBKDF2" => {
            is_import && matches!(fmt.as_str(), "raw" | "raw-secret")
        }
        "ECDH" | "ECDSA" | "ED25519" | "ED448" | "X25519" | "X448" => {
            matches!(fmt.as_str(), "spki" | "pkcs8" | "jwk" | "raw" | "raw-public")
        }
        "RSA-OAEP" | "RSA-PSS" | "RSASSA-PKCS1-V1_5" => {
            matches!(fmt.as_str(), "spki" | "pkcs8" | "jwk")
        }
        _ => false,
    };

    Ok(supported)
}

#[napi]
pub fn check_key_format_support(format: String, algorithm: String) -> Result<KeyFormatSupport> {
    let supports_import = is_format_supported(format.clone(), algorithm.clone(), true)?;
    let supports_export = is_format_supported(format.clone(), algorithm.clone(), false)?;
    Ok(KeyFormatSupport {
        format,
        algorithm,
        supports_import,
        supports_export,
    })
}
