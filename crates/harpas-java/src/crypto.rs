//! Login encryption: the server's RSA key, the shared secret, and the AES/CFB8 stream
//!
//! The client encrypts the shared secret and the verify token with the server's public key using RSA PKCS#1
//! v1.5. The `rsa` crate's PKCS#1 v1.5 decryption leaks timing (RUSTSEC-2023-0071, the Marvin attack), so it
//! is not used: the server does the raw RSA operation with random blinding, removes the padding in constant
//! time, and on a bad padding goes on with pseudo-random bytes instead of failing (implicit rejection, as TLS
//! stacks do). A bad padding and a wrong verify token then end the same way, at the same point.

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use getrandom::SysRng;
use rsa::hazmat::rsa_decrypt;
use rsa::pkcs8::EncodePublicKey;
use rsa::rand_core::UnwrapErr;
use rsa::traits::PublicKeyParts;
use rsa::{BoxedUint, PrivateKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::{ConditionallySelectable, ConstantTimeEq};

/// Size of the server's RSA key, as in vanilla.
const KEY_BITS: usize = 1024;
/// Shortest PKCS#1 v1.5 padding string.
const MIN_PADDING: usize = 8;

/// The server's RSA key pair, generated at startup as vanilla does.
pub struct ServerKey {
    private: PrivateKey,
    public_der: Vec<u8>,
    rejection_key: [u8; 32],
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey").finish_non_exhaustive()
    }
}

impl ServerKey {
    /// Generates a 1024-bit key pair.
    pub fn generate() -> Self {
        let mut rng = UnwrapErr(SysRng);
        let private = PrivateKey::new(&mut rng, KEY_BITS).expect("an RSA key can be generated");
        let public_der = private
            .to_public_key()
            .to_public_key_der()
            .expect("an RSA public key encodes to DER")
            .into_vec();
        let mut rejection_key = [0; 32];
        getrandom::fill(&mut rejection_key).expect("the system random number generator works");
        Self {
            private,
            public_der,
            rejection_key,
        }
    }

    /// The public key, DER encoded as an X.509 SubjectPublicKeyInfo, as Encryption Request sends it.
    pub fn public_der(&self) -> &[u8] {
        &self.public_der
    }

    /// Decrypts a PKCS#1 v1.5 message that must be `N` bytes long. A ciphertext of the wrong size is
    /// refused; any other invalid ciphertext gives `N` pseudo-random bytes derived from it.
    pub(crate) fn decrypt<const N: usize>(&self, ciphertext: &[u8]) -> Option<[u8; N]> {
        let k = self.private.size();
        // Sizes are public: a ciphertext is exactly the key's size.
        if ciphertext.len() != k || N + 3 + MIN_PADDING > k {
            return None;
        }
        let bits = self.private.n().as_ref().bits_precision();
        let c = BoxedUint::from_be_slice(ciphertext, bits).ok()?;
        // Ciphertexts not below the modulus are rejected by the public comparison inside `rsa_decrypt`;
        // those are invalid whatever the key, so they reveal nothing about it.
        let mut rng = UnwrapErr(SysRng);
        let em = rsa_decrypt(Some(&mut rng), &self.private, &c)
            .ok()?
            .to_be_bytes();
        let em = &em[em.len() - k..];

        // EM = 0x00 || 0x02 || PS (at least 8 non-zero bytes) || 0x00 || M, with M exactly N bytes.
        let separator = k - N - 1;
        let mut valid = em[0].ct_eq(&0) & em[1].ct_eq(&2) & em[separator].ct_eq(&0);
        for byte in &em[2..separator] {
            valid &= !byte.ct_eq(&0);
        }
        let fallback = self.rejection_bytes::<N>(ciphertext);
        let mut message = [0; N];
        for (i, out) in message.iter_mut().enumerate() {
            *out = u8::conditional_select(&fallback[i], &em[separator + 1 + i], valid);
        }
        Some(message)
    }

    /// Pseudo-random bytes for an invalid ciphertext: the same ciphertext always gives the same bytes.
    fn rejection_bytes<const N: usize>(&self, ciphertext: &[u8]) -> [u8; N] {
        let mut hasher = Sha256::new();
        hasher.update(self.rejection_key);
        hasher.update(ciphertext);
        let digest = hasher.finalize();
        let mut out = [0; N];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = digest[i % digest.len()];
        }
        out
    }
}

/// Compares two byte strings of the same length in constant time.
pub(crate) fn ct_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// The hash the client and the server both send to Mojang's session server: SHA-1 of the server ID (empty),
/// the shared secret and the public key, as a signed hexadecimal number the way Java's `BigInteger` prints it.
pub fn server_hash(server_id: &str, shared_secret: &[u8], public_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(shared_secret);
    hasher.update(public_der);
    minecraft_hex(hasher.finalize().into())
}

fn minecraft_hex(mut digest: [u8; 20]) -> String {
    let negative = digest[0] & 0x80 != 0;
    if negative {
        // Two's complement: invert and add one.
        let mut carry = true;
        for byte in digest.iter_mut().rev() {
            *byte = !*byte;
            if carry {
                let (sum, overflow) = byte.overflowing_add(1);
                *byte = sum;
                carry = overflow;
            }
        }
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    if negative {
        format!("-{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// The AES/CFB8 stream of an encrypted connection, one cipher per direction, with the shared secret as both
/// the key and the IV.
pub(crate) struct Encryption {
    encryptor: cfb8::Encryptor<Aes128>,
    decryptor: cfb8::Decryptor<Aes128>,
}

impl std::fmt::Debug for Encryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encryption").finish_non_exhaustive()
    }
}

impl Encryption {
    pub(crate) fn new(shared_secret: &[u8; 16]) -> Self {
        let key = Array::from(*shared_secret);
        Self {
            encryptor: cfb8::Encryptor::new(&key, &key),
            decryptor: cfb8::Decryptor::new(&key, &key),
        }
    }

    /// Encrypts bytes going to the client, continuing the stream.
    pub(crate) fn encrypt(&mut self, data: &mut [u8]) {
        let (blocks, rest) = Array::slice_as_chunks_mut(data);
        debug_assert!(rest.is_empty(), "CFB8 blocks are one byte");
        self.encryptor.encrypt_blocks(blocks);
    }

    /// Decrypts bytes from the client, continuing the stream.
    pub(crate) fn decrypt(&mut self, data: &mut [u8]) {
        let (blocks, rest) = Array::slice_as_chunks_mut(data);
        debug_assert!(rest.is_empty(), "CFB8 blocks are one byte");
        self.decryptor.decrypt_blocks(blocks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::Pkcs1v15Encrypt;
    use std::sync::OnceLock;

    pub(crate) fn key() -> &'static ServerKey {
        static KEY: OnceLock<ServerKey> = OnceLock::new();
        KEY.get_or_init(ServerKey::generate)
    }

    #[test]
    fn server_hashes_match_the_documented_examples() {
        // minecraft.wiki's examples hash only the name, to show the signed hexadecimal format.
        let hash = |name: &str| minecraft_hex(Sha1::digest(name.as_bytes()).into());
        assert_eq!(hash("Notch"), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(hash("jeb_"), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(hash("simon"), "88e16a1019277b15d58faf0541e11910eb756f6");
    }

    #[test]
    fn decrypts_what_the_public_key_encrypts() {
        let key = key();
        let public = key.private.to_public_key();
        let mut rng = UnwrapErr(SysRng);
        let secret = [7u8; 16];
        let ciphertext = public.encrypt(&mut rng, Pkcs1v15Encrypt, &secret).unwrap();
        assert_eq!(key.decrypt::<16>(&ciphertext), Some(secret));
        // A message of another length is not the expected one, and gives the same bytes every time.
        let token = public
            .encrypt(&mut rng, Pkcs1v15Encrypt, &[1, 2, 3, 4])
            .unwrap();
        let rejected = key.decrypt::<16>(&token).unwrap();
        assert_ne!(rejected, secret);
        assert_eq!(key.decrypt::<16>(&token), Some(rejected));
        assert_eq!(key.decrypt::<4>(&token), Some([1, 2, 3, 4]));
        // Garbage of the right size is rejected implicitly; the wrong size is refused.
        assert!(key.decrypt::<4>(&[0x55; 128]).is_some());
        assert_eq!(key.decrypt::<4>(&[0x55; 127]), None);
    }

    #[test]
    fn cfb8_streams_continue_across_calls() {
        let mut server = Encryption::new(&[3; 16]);
        let mut client = Encryption::new(&[3; 16]);
        let mut data = *b"hello, encrypted world";
        server.encrypt(&mut data[..5]);
        server.encrypt(&mut data[5..]);
        assert_ne!(&data, b"hello, encrypted world");
        client.decrypt(&mut data[..10]);
        client.decrypt(&mut data[10..]);
        assert_eq!(&data, b"hello, encrypted world");
    }
}
