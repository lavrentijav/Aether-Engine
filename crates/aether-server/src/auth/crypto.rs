//! The cryptographic half of online mode.
//!
//! Vanilla's login encryption is: the server holds a 1024-bit RSA keypair,
//! sends the public key with a verify token, the client answers with the
//! same token and a fresh AES key (both RSA-encrypted), and from then on the
//! stream is AES/CFB8 with the shared secret as both key and IV. This module
//! is that, in RustCrypto crates only.

use aes::Aes128;
use cfb8::{Decryptor, Encryptor};
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};

/// The length of the shared secret, and of the AES key and IV.
pub const SECRET_LEN: usize = 16;
/// The length of the verify token, in bytes. Vanilla uses four.
pub const TOKEN_LEN: usize = 4;
/// The RSA key size vanilla uses. Bigger works but older clients send a
/// fixed-size response, so this is not a knob worth turning.
pub const KEY_BITS: usize = 1024;

/// The server's RSA keypair, generated once at startup.
pub struct ServerKey {
    private: RsaPrivateKey,
    public_der: Vec<u8>,
}

impl ServerKey {
    /// Generate a fresh 1024-bit keypair.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, KEY_BITS).expect("RSA keygen");
        let public = RsaPublicKey::from(&private);
        let public_der = public
            .to_public_key_der()
            .expect("encode public key")
            .as_bytes()
            .to_vec();
        Self {
            private,
            public_der,
        }
    }

    /// The DER-encoded `SubjectPublicKeyInfo` the client hashes and parses.
    pub fn public_der(&self) -> &[u8] {
        &self.public_der
    }

    /// RSA-decrypt a `PKCS#1 v1.5` block, as the Encryption Response carries
    /// it.
    pub fn decrypt(&self, data: &[u8]) -> Option<Vec<u8>> {
        self.private.decrypt(Pkcs1v15Encrypt, data).ok()
    }

    /// Encrypt with the public half. Only for tests that stand in for the
    /// client's side of the handshake.
    #[cfg(test)]
    pub fn encrypt_for_test(&self, data: &[u8]) -> Vec<u8> {
        let public = RsaPublicKey::from(&self.private);
        public
            .encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, data)
            .expect("encrypt")
    }
}

/// The AES/CFB8 stream cipher, one direction each way.
///
/// Minecraft uses the same 16-byte shared secret as both the AES key and the
/// IV, and CFB8 so a partial read (a frame boundary in the middle of a TCP
/// segment) does not desynchronise the stream.
pub struct Crypt {
    enc: Encryptor<Aes128>,
    dec: Decryptor<Aes128>,
}

impl Crypt {
    /// Build both directions from the shared secret.
    pub fn new(secret: &[u8; SECRET_LEN]) -> Self {
        Self {
            enc: Encryptor::<Aes128>::new(secret.into(), secret.into()),
            dec: Decryptor::<Aes128>::new(secret.into(), secret.into()),
        }
    }

    /// Encrypt `buf` in place (outbound).
    ///
    /// CFB8's block size is one byte, so the stream advances a byte at a time
    /// — which is exactly why Minecraft chose it: a frame split anywhere,
    /// including mid-length-prefix, still decrypts.
    pub fn encrypt(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            let mut block = cipher::Block::<Encryptor<Aes128>>::default();
            block[0] = *b;
            self.enc.encrypt_block_mut(&mut block);
            *b = block[0];
        }
    }

    /// Decrypt `buf` in place (inbound).
    pub fn decrypt(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            let mut block = cipher::Block::<Decryptor<Aes128>>::default();
            block[0] = *b;
            self.dec.decrypt_block_mut(&mut block);
            *b = block[0];
        }
    }
}

/// Minecraft's "server id" hash: `SHA-1(server_id + secret + public_key)`,
/// read as a signed big-endian integer and rendered as hex.
///
/// The sign matters: `hasJoined` must be given the exact string the client
/// computed, and a digest whose top bit is set is negative and carries a
/// leading `-`.
pub fn server_id_hash(server_id: &str, shared_secret: &[u8], public_key: &[u8]) -> String {
    let mut sha = Sha1::new();
    sha.update(server_id.as_bytes());
    sha.update(shared_secret);
    sha.update(public_key);
    let digest = sha.finalize();
    signed_hex(&digest)
}

/// Render a big-endian digest as the signed hex Minecraft's `toString(16)`
/// would produce.
fn signed_hex(digest: &[u8]) -> String {
    let negative = digest.first().is_some_and(|b| b & 0x80 != 0);
    if !negative {
        return strip_leading_zeros(digest);
    }
    // Two's complement: invert and add one, then prefix `-`.
    let mut mag = digest.to_vec();
    for b in &mut mag {
        *b = !*b;
    }
    for b in mag.iter_mut().rev() {
        let (v, carry) = b.overflowing_add(1);
        *b = v;
        if !carry {
            break;
        }
    }
    format!("-{}", strip_leading_zeros(&mag))
}

/// Hex, lowercase, without leading zero *digits* (`0` when the value is
/// zero).
///
/// This mirrors `BigInteger.toString(16)`, which drops leading zero bits, not
/// just whole zero bytes: a digest beginning `08 8e …` renders `88e16a…`.
fn strip_leading_zeros(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    let trimmed = out.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Fill `out` with cryptographically random bytes.
pub fn random_bytes(out: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wiki.vg vectors, which use an empty secret and public key, so they
    /// pin the digest *and* the signed rendering.
    #[test]
    fn server_id_hash_matches_the_known_vectors() {
        assert_eq!(
            server_id_hash("Notch", &[], &[]),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        assert_eq!(
            server_id_hash("jeb_", &[], &[]),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
        assert_eq!(
            server_id_hash("simon", &[], &[]),
            "88e16a1019277b15d58faf0541e11910eb756f6"
        );
    }

    #[test]
    fn the_hash_includes_secret_and_key_after_the_server_id() {
        // Any change to the tail changes the digest; this is what the ordering
        // means in practice.
        let a = server_id_hash("srv", &[1u8; 16], &[2u8; 8]);
        let b = server_id_hash("srv", &[1u8; 16], &[3u8; 8]);
        assert_ne!(a, b);
    }

    #[test]
    fn a_positive_digest_keeps_a_leading_zero_byte_out() {
        // "simon" hashes to 08 8e ...; the rendering drops the `08` entirely,
        // which is the BigInteger behaviour, not the raw hex.
        assert!(server_id_hash("simon", &[], &[]).starts_with("88e16a10"));
    }

    #[test]
    fn rsa_round_trips_a_verify_token_and_a_secret() {
        let key = super::super::test_key();
        let token = [0xDE, 0xAD, 0xBE, 0xEF];
        let secret = [7u8; SECRET_LEN];

        let enc_token = key.encrypt_for_test(&token);
        assert_eq!(key.decrypt(&enc_token).unwrap(), token);

        let enc_secret = key.encrypt_for_test(&secret);
        let got = key.decrypt(&enc_secret).unwrap();
        assert_eq!(got.as_slice(), &secret);
    }

    #[test]
    fn aes_cfb8_round_trips_across_chunk_boundaries() {
        // The whole reason for CFB8: a stream split at an arbitrary byte must
        // decrypt to the same plaintext as one shot.
        let secret = [0x11u8; SECRET_LEN];
        let plaintext: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();

        let mut enc = Crypt::new(&secret);
        let mut wire = plaintext.clone();
        enc.encrypt(&mut wire);
        assert_ne!(wire, plaintext);

        let mut dec = Crypt::new(&secret);
        // Decrypt in ragged chunks; CFB8 keeps the byte stream aligned.
        let mut out = wire.clone();
        let mut off = 0;
        for step in [1usize, 7, 130, 862] {
            let end = (off + step).min(out.len());
            dec.decrypt(&mut out[off..end]);
            off = end;
        }
        assert_eq!(out, plaintext);
    }

    #[test]
    fn the_per_byte_cfb8_matches_the_reference_vector() {
        // The `cfb8` crate's own known-answer vector, with the key and IV it
        // uses there (Minecraft uses one secret for both, but the mode is the
        // same). This pins the byte-at-a-time path, which is where a
        // streaming implementation most easily drifts from the standard.
        let key = [0x42u8; 16];
        let iv = [0x24u8; 16];
        let plaintext = b"hello world! this is my plaintext.";
        let want: &[u8] = &[
            0x33, 0xb3, 0x56, 0xce, 0x91, 0x84, 0x29, 0x0c, 0x4c, 0x8f, 0xac, 0xc1, 0xc0, 0xb1,
            0xf9, 0x18, 0xd5, 0x47, 0x5a, 0xeb, 0x75, 0xb8, 0x8c, 0x16, 0x1c, 0xa6, 0x5b, 0xdf,
            0x05, 0xc7, 0x13, 0x7f, 0xf4, 0xb0,
        ];
        let mut enc = Encryptor::<Aes128>::new(&key.into(), &iv.into());
        let mut got = plaintext.to_vec();
        for b in got.iter_mut() {
            let mut block = cipher::Block::<Encryptor<Aes128>>::default();
            block[0] = *b;
            enc.encrypt_block_mut(&mut block);
            *b = block[0];
        }
        assert_eq!(got, want);

        let mut dec = Decryptor::<Aes128>::new(&key.into(), &iv.into());
        for b in got.iter_mut() {
            let mut block = cipher::Block::<Decryptor<Aes128>>::default();
            block[0] = *b;
            dec.decrypt_block_mut(&mut block);
            *b = block[0];
        }
        assert_eq!(got, plaintext);
    }

    #[test]
    fn the_public_key_is_a_der_subjectpublickeyinfo() {
        let key = super::super::test_key();
        let der = key.public_der();
        // SEQUENCE of a 1024-bit RSA key; the prefix is stable and is what the
        // client parses.
        assert_eq!(der[0], 0x30);
        assert!(der.len() > 140 && der.len() < 200, "len {}", der.len());
    }
}
