//! The crypto the two voice protocols use, built on the workspace's `aes`
//! block cipher.
//!
//! - **Simple Voice Chat** wraps every UDP payload in `AES/GCM/NoPadding`: a
//!   12-byte random IV, the ciphertext and a 128-bit tag, all with the
//!   per-player 16-byte secret key. [`Gcm`] implements it.
//! - **Plasmo Voice** hands each client the server's AES key RSA-encrypted and
//!   the clients then protect audio with `AES/CBC/PKCS5Padding`. [`Cbc`]
//!   implements it.
//!
//! GCM and CBC are implemented here rather than pulled in as extra crates so
//! the plugin adds nothing to the dependency tree beyond the AES cipher the
//! workspace already builds. Both are checked against the published NIST test
//! vectors in the tests below.

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

/// AES-128-GCM, the mode Simple Voice Chat encrypts every UDP payload with.
pub struct Gcm {
    cipher: Aes128,
}

impl Gcm {
    /// A GCM instance under a 16-byte key.
    pub fn new(key: &[u8; 16]) -> Gcm {
        Gcm {
            cipher: Aes128::new(key.into()),
        }
    }

    fn block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut b = aes::cipher::Block::<Aes128>::clone_from_slice(input);
        self.cipher.encrypt_block(&mut b);
        b.into()
    }

    fn ghash(&self, h: u128, aad: &[u8], ciphertext: &[u8]) -> u128 {
        let mut y: u128 = 0;
        let absorb = |chunk: &[u8], y: &mut u128| {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            *y = gf_mul(*y ^ u128::from_be_bytes(block), h);
        };
        for chunk in aad.chunks(16) {
            absorb(chunk, &mut y);
        }
        for chunk in ciphertext.chunks(16) {
            absorb(chunk, &mut y);
        }
        let mut len = [0u8; 16];
        len[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
        len[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
        gf_mul(y ^ u128::from_be_bytes(len), h)
    }

    fn gctr(&self, mut counter: u128, data: &[u8], encrypt: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let keystream = self.block(&counter.to_be_bytes());
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            for i in 0..chunk.len() {
                block[i] ^= keystream[i];
            }
            out.extend_from_slice(&block[..chunk.len()]);
            counter = inc32(counter);
        }
        let _ = encrypt;
        out
    }

    /// Encrypt `plaintext` with a 12-byte `iv`, returning `iv` unchanged is the
    /// caller's job; this returns `ciphertext || tag`.
    pub fn encrypt(&self, iv: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let h = u128::from_be_bytes(self.block(&[0u8; 16]));
        let j0 = j0(iv);
        let ciphertext = self.gctr(inc32(j0), plaintext, true);
        let s = self.ghash(h, aad, &ciphertext);
        let tag = u128::from_be_bytes(self.block(&j0.to_be_bytes())) ^ s;
        let mut out = ciphertext;
        out.extend_from_slice(&tag.to_be_bytes());
        out
    }

    /// Decrypt `ciphertext || tag`, checking the tag in constant time. `None`
    /// on a bad tag, a short input or a length mismatch.
    pub fn decrypt(&self, iv: &[u8; 12], aad: &[u8], input: &[u8]) -> Option<Vec<u8>> {
        if input.len() < 16 {
            return None;
        }
        let (ciphertext, tag) = input.split_at(input.len() - 16);
        let h = u128::from_be_bytes(self.block(&[0u8; 16]));
        let j0 = j0(iv);
        let s = self.ghash(h, aad, ciphertext);
        let expected = u128::from_be_bytes(self.block(&j0.to_be_bytes())) ^ s;
        if !constant_time_eq(&expected.to_be_bytes(), tag) {
            return None;
        }
        Some(self.gctr(inc32(j0), ciphertext, false))
    }
}

/// AES-128-CBC with PKCS#5/7 padding, Plasmo Voice's audio mode.
pub struct Cbc {
    cipher: Aes128,
}

impl Cbc {
    pub fn new(key: &[u8; 16]) -> Cbc {
        Cbc {
            cipher: Aes128::new(key.into()),
        }
    }

    fn block(&self, input: &[u8; 16], decrypt: bool) -> [u8; 16] {
        let mut b = aes::cipher::Block::<Aes128>::clone_from_slice(input);
        if decrypt {
            self.cipher.decrypt_block(&mut b);
        } else {
            self.cipher.encrypt_block(&mut b);
        }
        b.into()
    }

    /// Encrypt with PKCS#7 padding. `iv` is 16 bytes.
    pub fn encrypt(&self, iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let mut padded = plaintext.to_vec();
        let pad = 16 - (plaintext.len() % 16);
        padded.extend(std::iter::repeat(pad as u8).take(pad));
        let mut prev = *iv;
        let mut out = Vec::with_capacity(padded.len());
        for chunk in padded.chunks(16) {
            let mut block = [0u8; 16];
            for i in 0..16 {
                block[i] = chunk[i] ^ prev[i];
            }
            let enc = self.block(&block, false);
            out.extend_from_slice(&enc);
            prev = enc;
        }
        out
    }

    /// Decrypt and strip PKCS#7 padding. `None` on a length not a multiple of
    /// the block size or bad padding.
    pub fn decrypt(&self, iv: &[u8; 16], input: &[u8]) -> Option<Vec<u8>> {
        if input.is_empty() || input.len() % 16 != 0 {
            return None;
        }
        let mut prev = *iv;
        let mut out = Vec::with_capacity(input.len());
        for chunk in input.chunks(16) {
            let dec = self.block(chunk.try_into().unwrap(), true);
            for i in 0..16 {
                out.push(dec[i] ^ prev[i]);
            }
            prev = chunk.try_into().unwrap();
        }
        let pad = *out.last()? as usize;
        if pad == 0 || pad > 16 || pad > out.len() {
            return None;
        }
        if !out[out.len() - pad..].iter().all(|&b| b as usize == pad) {
            return None;
        }
        out.truncate(out.len() - pad);
        Some(out)
    }
}

/// Increment the rightmost 32 bits of the counter, big-endian.
fn inc32(counter: u128) -> u128 {
    let low = counter as u32;
    (counter & !0xffff_ffff) | (low.wrapping_add(1) as u128)
}

/// The GCM initial counter block: for a 12-byte IV it is `IV || 0x00000001`.
fn j0(iv: &[u8; 12]) -> u128 {
    let mut block = [0u8; 16];
    block[..12].copy_from_slice(iv);
    block[15] = 1;
    u128::from_be_bytes(block)
}

/// Multiply two elements of GF(2^128) in the GCM bit order (the NIST
/// "Algorithm 1" right-shift form).
fn gf_mul(x: u128, y: u128) -> u128 {
    let mut z: u128 = 0;
    let mut v = y;
    for i in 0..128 {
        if (x >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        if v & 1 == 1 {
            v = (v >> 1) ^ (0xe1u128 << 120);
        } else {
            v >>= 1;
        }
    }
    z
}

/// Compare two byte slices without an early exit on the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// MD5, only so the Plasmo Voice "name UUIDs" (`UUID.nameUUIDFromBytes`) can be
/// reproduced. Not used for anything security-sensitive.
pub fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Java's `UUID.nameUUIDFromBytes`: an MD5 name UUID with the version and
/// variant bits set, as Plasmo Voice derives source-line and activation ids.
pub fn name_uuid(name: &str) -> u128 {
    let mut h = md5(name.as_bytes());
    h[6] = (h[6] & 0x0f) | 0x30;
    h[8] = (h[8] & 0x3f) | 0x80;
    u128::from_be_bytes(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_matches_nist_empty_plaintext() {
        // NIST CAVP AES-128-GCM, key and IV all zero, no plaintext, no AAD.
        let gcm = Gcm::new(&[0u8; 16]);
        let out = gcm.encrypt(&[0u8; 12], &[], &[]);
        assert_eq!(hex(&out), "58e2fccefa7e3061367f1d57a4e7455a");
    }

    #[test]
    fn gcm_matches_nist_one_block() {
        // NIST CAVP AES-128-GCM, key and IV all zero, 16 zero plaintext bytes.
        let gcm = Gcm::new(&[0u8; 16]);
        let out = gcm.encrypt(&[0u8; 12], &[], &[0u8; 16]);
        assert_eq!(
            hex(&out),
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"
        );
    }

    #[test]
    fn gcm_matches_nist_with_aad() {
        // NIST GCM test case 4: 16-byte key, 12-byte IV, 60-byte plaintext,
        // 20-byte AAD.
        let key: [u8; 16] = [
            0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30,
            0x83, 0x08,
        ];
        let iv: [u8; 12] = [
            0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88,
        ];
        let plaintext = [
            0xd9, 0x31, 0x32, 0x25, 0xf8, 0x84, 0x06, 0xe5, 0xa5, 0x59, 0x09, 0xc5, 0xaf, 0xf5,
            0x26, 0x9a, 0x86, 0xa7, 0xa9, 0x53, 0x15, 0x34, 0xf7, 0xda, 0x2e, 0x4c, 0x30, 0x3d,
            0x8a, 0x31, 0x8a, 0x72, 0x1c, 0x3c, 0x0c, 0x95, 0x95, 0x68, 0x09, 0x53, 0x2f, 0xcf,
            0x0e, 0x24, 0x49, 0xa6, 0xb5, 0x25, 0xb1, 0x6a, 0xed, 0xf5, 0xaa, 0x0d, 0xe6, 0x57,
            0xba, 0x63, 0x7b, 0x39,
        ];
        let aad = [
            0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad,
            0xbe, 0xef, 0xab, 0xad, 0xda, 0xd2,
        ];
        let gcm = Gcm::new(&key);
        let out = gcm.encrypt(&iv, &aad, &plaintext);
        assert_eq!(
            hex(&out),
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e0915bc94fbc3221a5db94fae95ae7121a47"
        );
        // And it round-trips.
        let back = gcm.decrypt(&iv, &aad, &out).unwrap();
        assert_eq!(back, plaintext);
    }

    #[test]
    fn gcm_rejects_a_tampered_tag() {
        let gcm = Gcm::new(&[7u8; 16]);
        let mut out = gcm.encrypt(&[1u8; 12], b"aad", b"secret audio");
        let last = out.len() - 1;
        out[last] ^= 1;
        assert!(gcm.decrypt(&[1u8; 12], b"aad", &out).is_none());
    }

    #[test]
    fn cbc_round_trips_and_uses_pkcs7() {
        let cbc = Cbc::new(&[3u8; 16]);
        let iv = [9u8; 16];
        let ct = cbc.encrypt(&iv, b"hello");
        assert_eq!(ct.len() % 16, 0);
        assert_eq!(ct.len(), 16);
        assert_eq!(cbc.decrypt(&iv, &ct).unwrap(), b"hello");
    }

    #[test]
    fn cbc_matches_nist_aes128_vector() {
        // NIST SP 800-38A F.2.1 CBC-AES128, first block (IV and plaintext given,
        // no padding involved).
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let block: [u8; 16] = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        let cbc = Cbc::new(&key);
        // One plaintext block plus the mandatory padding block.
        let ct = cbc.encrypt(&iv, &block);
        assert_eq!(
            hex(&ct[..16]),
            "7649abac8119b246cee98e9b12e9197d",
            "first CBC block"
        );
    }

    #[test]
    fn cbc_decrypt_rejects_bad_padding() {
        let cbc = Cbc::new(&[5u8; 16]);
        let mut ct = cbc.encrypt(&[0u8; 16], b"x");
        // Corrupt the last ciphertext block so the padding check fails.
        let last = ct.len() - 1;
        ct[last] ^= 0xff;
        assert!(cbc.decrypt(&[0u8; 16], &ct).is_none());
    }

    fn hex(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }

    #[test]
    fn md5_matches_the_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn a_name_uuid_sets_the_version_and_variant_bits() {
        let id = name_uuid("proximity_activation");
        let bytes = id.to_be_bytes();
        assert_eq!(bytes[6] >> 4, 3, "version 3");
        assert_eq!(bytes[8] >> 6, 0b10, "RFC 4122 variant");
    }
}
