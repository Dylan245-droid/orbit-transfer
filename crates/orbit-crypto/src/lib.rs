use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use thiserror::Error;

const SALT: &[u8] = b"orbit-transfer-v1-salt";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("decryption failed: {0}")]
    Decrypt(String),
}

/// AEAD session cipher: each symbol is sealed under a nonce derived from
/// (session_id, esi), so the relay can route but never read the payload.
#[derive(Clone)]
pub struct SessionCipher {
    cipher: ChaCha20Poly1305,
}

impl SessionCipher {
    /// Derives a 256-bit key from a passphrase using Argon2id.
    pub fn new(secret: &str) -> Self {
        let mut key = [0u8; KEY_LEN];
        Argon2::default()
            .hash_password_into(secret.as_bytes(), SALT, &mut key)
            .expect("argon2 key derivation");
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
        }
    }

    fn nonce(session_id: u64, esi: u32) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        n[..8].copy_from_slice(&session_id.to_le_bytes());
        n[8..].copy_from_slice(&esi.to_le_bytes());
        n
    }

    /// Seals one encoded symbol. Output = ciphertext || 16-byte tag.
    pub fn seal_symbol(&self, session_id: u64, esi: u32, data: &[u8]) -> Vec<u8> {
        let nonce_bytes = Self::nonce(session_id, esi);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let aad = nonce_bytes;
        self.cipher
            .encrypt(
                nonce,
                Payload {
                    msg: data,
                    aad: &aad,
                },
            )
            .expect("chacha20poly1305 encryption")
    }

    /// Opens one encoded symbol. Fails on tampering or wrong key.
    pub fn open_symbol(
        &self,
        session_id: u64,
        esi: u32,
        data: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce_bytes = Self::nonce(session_id, esi);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let aad = nonce_bytes;
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: data,
                    aad: &aad,
                },
            )
            .map_err(|e| CryptoError::Decrypt(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let cipher = SessionCipher::new("hunter2");
        let data = vec![0xAB; 4096];
        let sealed = cipher.seal_symbol(42, 7, &data);
        assert_ne!(sealed, data);
        assert_eq!(sealed.len(), data.len() + 16);
        let opened = cipher.open_symbol(42, 7, &sealed).unwrap();
        assert_eq!(opened, data);
    }

    #[test]
    fn wrong_key_fails() {
        let a = SessionCipher::new("correct horse");
        let b = SessionCipher::new("battery staple");
        let sealed = a.seal_symbol(1, 2, b"secret");
        assert!(b.open_symbol(1, 2, &sealed).is_err());
    }

    #[test]
    fn wrong_esi_fails() {
        let cipher = SessionCipher::new("x");
        let sealed = cipher.seal_symbol(9, 3, b"data");
        assert!(cipher.open_symbol(9, 4, &sealed).is_err());
        assert!(cipher.open_symbol(8, 3, &sealed).is_err());
    }
}