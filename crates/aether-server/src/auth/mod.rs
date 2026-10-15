//! Online mode and hybrid authentication.
//!
//! The server has always run offline: a username is a claim anyone may make,
//! and an "operator" bound by name alone is takeable by anyone who types it.
//! This module adds Mojang's encryption handshake and the hybrid policy that
//! locks a name that has ever authenticated to its Mojang uuid.
//!
//! The pieces:
//!
//! * [`store`] — the durable premium-name lock table;
//! * `crypto` — the server RSA keypair, AES/CFB8 stream cipher and the
//!   signed server-id hash;
//! * `api` — the session server (`hasJoined`) and profile API (`name →
//!   uuid`) clients, behind a trait so tests can mock them;
//! * `policy` — the hybrid decision table.

pub mod crypto;
pub mod store;

pub use store::Lock;

/// A shared RSA key for tests.
///
/// Generating a 1024-bit key is slow and the tests would do it per case, so
/// one is generated lazily and reused for the whole test binary.
#[cfg(test)]
pub(crate) fn test_key() -> &'static crypto::ServerKey {
    use std::sync::OnceLock;
    static KEY: OnceLock<crypto::ServerKey> = OnceLock::new();
    KEY.get_or_init(crypto::ServerKey::generate)
}

/// Seconds since the Unix epoch, saturating at the edges.
pub fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // A clock before 1970 is not worth a special case; "now" is 0.
        Err(_) => 0,
    }
}

impl store::MetaStore for crate::session::DemoWorld {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.get_meta(key).ok().flatten()
    }
    fn put(&self, key: &[u8], value: &[u8]) {
        let _ = self.put_meta(key, value);
    }
    fn del(&self, key: &[u8]) {
        let _ = self.delete_meta(key);
    }
}
