//! Online mode and hybrid authentication.
//!
//! The server has always run offline: a username is a claim anyone may make,
//! and an "operator" bound by name alone is takeable by anyone who types it.
//! This module adds Mojang's encryption handshake and the hybrid policy that
//! locks a name that has ever authenticated to its Mojang uuid.
//!
//! Only names that have **completed online auth on this server** are locked;
//! offline play stays open to everyone, including names registered at Mojang.
//! An unlocked premium name is only *offered* encryption — a client that
//! fails `hasJoined` is admitted offline in the same connection, and a client
//! that drops mid-handshake suppresses the offer for its next attempt. See
//! [`policy::authenticate`].
//!
//! The pieces:
//!
//! * [`store`] — the durable premium-name lock table;
//! * `crypto` — the server RSA keypair, AES/CFB8 stream cipher and the
//!   signed server-id hash;
//! * `api` — the session server (`hasJoined`) and profile API (`name →
//!   uuid`) clients, behind a trait so tests can mock them;
//! * `bounce` — the same-address, same-name "dropped during auth" memory;
//! * `policy` — the hybrid decision table.

pub mod api;
pub mod bounce;
pub mod crypto;
pub mod policy;
pub mod store;

pub use policy::Admission;
pub use store::Lock;

use std::sync::OnceLock;

/// The server's RSA keypair, generated once at startup.
static SERVER_KEY: OnceLock<crypto::ServerKey> = OnceLock::new();
/// The Mojang client, built once at startup.
static API: OnceLock<Box<dyn api::ProfileApi>> = OnceLock::new();
/// The name-lookup cache, shared by every connection.
static CACHE: OnceLock<api::LookupCache> = OnceLock::new();
/// Clients that dropped during online auth, shared by every connection.
static BOUNCE: OnceLock<bounce::BounceCache> = OnceLock::new();

/// Build the key, HTTP client and cache for online or hybrid mode.
///
/// A no-op under [`AuthMode::Offline`](crate::config::AuthMode::Offline),
/// so an offline server never generates a key or opens a socket to Mojang.
/// Called at startup; a failure to build the HTTP client stops the server,
/// because hybrid mode that cannot reach Mojang would refuse every premium
/// name.
pub fn install(cfg: &crate::config::AuthConfig) -> Result<(), String> {
    use crate::config::AuthMode;
    if cfg.mode == AuthMode::Offline {
        return Ok(());
    }
    let key = crypto::ServerKey::generate();
    let timeout = std::time::Duration::from_millis(cfg.lookup_timeout_ms.max(1));
    let api = api::MojangApi::new(&cfg.session_server, &cfg.profile_api, timeout)
        .map_err(|e| format!("cannot build the Mojang HTTP client: {e:?}"))?;
    let _ = SERVER_KEY.set(key);
    let _ = API.set(Box::new(api));
    let _ = CACHE.set(api::LookupCache::new());
    let _ = BOUNCE.set(bounce::BounceCache::new());
    Ok(())
}

/// Resolve a login for the session layer, using the installed key/API/cache.
///
/// `store` is the world's lock table.
pub fn authenticate(
    s: &mut crate::proto::Conn,
    codec: &dyn crate::protocol::ProtocolCodec,
    cfg: &crate::config::AuthConfig,
    store: &dyn store::MetaStore,
    name: &str,
    ip: Option<std::net::IpAddr>,
) -> std::io::Result<Admission> {
    use crate::config::AuthMode;
    if cfg.mode == AuthMode::Offline {
        return Ok(Admission::Offline);
    }
    let (Some(key), Some(api), Some(cache), Some(bounce)) =
        (SERVER_KEY.get(), API.get(), CACHE.get(), BOUNCE.get())
    else {
        // Startup installs these; reaching here means a wiring bug, and
        // admitting anyone would silently drop the policy.
        return Ok(Admission::Refused(
            "Authentication is not initialised on this server.".into(),
        ));
    };
    policy::authenticate(
        s,
        codec,
        cfg,
        store,
        key,
        api.as_ref(),
        cache,
        bounce,
        name,
        ip,
    )
}

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
