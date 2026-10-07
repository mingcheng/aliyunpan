use std::fmt;

use k256::{
    ecdsa::{Signature, SigningKey, signature::hazmat::PrehashSigner},
    sha2::{Digest, Sha256},
};

use crate::{
    error::{Error, Result},
    util,
};

/// A secp256k1 key used to sign device session requests.
#[derive(Clone)]
pub struct DeviceKey {
    key: SigningKey,
}

impl DeviceKey {
    /// Generate a private key using the operating system's CSPRNG.
    pub fn generate() -> Result<Self> {
        loop {
            // Reject values outside the curve order even though they are extremely unlikely.
            if let Ok(key) = SigningKey::from_slice(&util::random_bytes::<32>()?) {
                return Ok(Self { key });
            }
        }
    }

    pub fn from_bytes(secret: &[u8]) -> Result<Self> {
        if secret.len() != 32 {
            return Err(Error::Crypto("secp256k1 secret key must be 32 bytes".into()));
        }
        SigningKey::from_slice(secret)
            .map(|key| Self { key })
            .map_err(|e| Error::Crypto(format!("invalid secp256k1 secret key: {e}")))
    }

    /// Hex encoding of `"04"` followed by the compressed 33-byte public key, matching the Go implementation.
    pub fn public_key_hex(&self) -> String {
        let point = self.key.verifying_key().to_sec1_point(true);
        format!("04{}", util::hex_lower(point.as_bytes()))
    }

    /// Return `hex(r || s) + "01"`, signing the SHA-256 of `"{app_id}:{device_id}:{user_id}:{nonce}"`.
    pub fn sign(&self, app_id: &str, device_id: &str, user_id: &str, nonce: u64) -> Result<String> {
        let digest = Sha256::digest(format!("{app_id}:{device_id}:{user_id}:{nonce}").as_bytes());
        let sig: Signature = self
            .key
            .sign_prehash(&digest)
            .map_err(|e| Error::Crypto(format!("ecdsa sign: {e}")))?;
        let sig = sig.normalize_s();
        Ok(format!("{}01", util::hex_lower(&sig.to_bytes())))
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceKey")
            .field("public_key", &self.public_key_hex())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use k256::ecdsa::{VerifyingKey, signature::hazmat::PrehashVerifier};

    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn public_key_format() {
        let key = DeviceKey::generate().unwrap();
        let pk = key.public_key_hex();
        assert_eq!(pk.len(), 2 + 66);
        assert!(pk.starts_with("0402") || pk.starts_with("0403"), "{pk}");
    }

    #[test]
    fn signature_format_and_validity() {
        let key = DeviceKey::from_bytes(&[7u8; 32]).unwrap();
        let sig = key
            .sign("25dzX3vbYqktVxyX", "0123456789abcdef0123456789abcdef", "user", 0)
            .unwrap();
        assert_eq!(sig.len(), 130);
        assert!(sig.ends_with("01"));

        let raw = unhex(&sig[..128]);
        let parsed = Signature::from_slice(&raw).unwrap();
        assert!(parsed.normalize_s() == parsed, "signature must be low-S");

        let vk = VerifyingKey::from_sec1_bytes(&unhex(&key.public_key_hex()[2..])).unwrap();
        let digest = Sha256::digest(b"25dzX3vbYqktVxyX:0123456789abcdef0123456789abcdef:user:0");
        vk.verify_prehash(&digest, &parsed).unwrap();
    }

    #[test]
    fn signature_is_deterministic() {
        let key = DeviceKey::from_bytes(&[9u8; 32]).unwrap();
        assert_eq!(key.sign("a", "b", "c", 0).unwrap(), key.sign("a", "b", "c", 0).unwrap());
        assert_ne!(key.sign("a", "b", "c", 0).unwrap(), key.sign("a", "b", "c", 1).unwrap());
    }

    #[test]
    fn rejects_invalid_secret() {
        assert!(DeviceKey::from_bytes(&[0u8; 32]).is_err());
        assert!(DeviceKey::from_bytes(&[1u8; 31]).is_err());
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let key = DeviceKey::from_bytes(&[7u8; 32]).unwrap();
        assert!(!format!("{key:?}").contains("0707070707"));
    }
}
