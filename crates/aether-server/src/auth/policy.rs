//! The hybrid decision table, and the online encryption/`hasJoined`
//! handshake that feeds it.
//!
//! The handshake itself is version-independent apart from one boolean: the
//! Encryption Request and Response are login packets `0x01` in every version
//! this server speaks. A version that carries the 1.19.x
//! `should_authenticate` field says so through
//! [`ProtocolCodec::encryption_request_has_should_authenticate`].
//!
//! Every login logs which mode it used ([`Admission`]), so an operator can
//! see who is online and who was turned away.

use std::net::IpAddr;

use crate::auth::api::{ApiError, LookupCache, Profile, ProfileApi};
use crate::auth::crypto::{self, ServerKey, TOKEN_LEN};
use crate::auth::store::{self, Lock, MetaStore};
use crate::config::{AuthConfig, AuthMode, LookupFailure};
use crate::log;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut};
use crate::protocol::ProtocolCodec;

/// The clientbound Encryption Request, in the login state.
const ENCRYPTION_REQUEST: i32 = 0x01;
/// The serverbound Encryption Response, in the login state.
const ENCRYPTION_RESPONSE: i32 = 0x01;

/// How a login was resolved.
#[derive(Debug)]
pub enum Admission {
    /// Admitted without Mojang auth; the caller derives the offline uuid.
    Offline,
    /// Authenticated with Mojang; the caller uses this uuid, name and skin.
    Online(Profile),
    /// Refused, with the message to show.
    Refused(String),
}

/// The result of the online handshake alone.
enum OnlineOutcome {
    Ok(Profile),
    /// The client failed to prove the account.
    Refused,
    /// Mojang could not be reached, so the claim is unverified.
    ApiDown,
}

/// Resolve a login under the configured policy.
///
/// `store` is the world's lock table; `key`, `api` and `cache` are the
/// process-wide server key and HTTP client (see [`super`]). `name` is the
/// client's claimed username.
#[allow(clippy::too_many_arguments)]
pub fn authenticate(
    s: &mut Conn,
    codec: &dyn ProtocolCodec,
    cfg: &AuthConfig,
    store: &dyn MetaStore,
    key: &ServerKey,
    api: &dyn ProfileApi,
    cache: &LookupCache,
    name: &str,
    ip: Option<IpAddr>,
) -> std::io::Result<Admission> {
    match cfg.mode {
        AuthMode::Offline => Ok(Admission::Offline),
        AuthMode::Online => {
            let outcome = online_login(s, codec, key, api, name, ip)?;
            Ok(settle_online(store, name, outcome))
        }
        AuthMode::Hybrid => hybrid(s, codec, cfg, store, key, api, cache, name, ip),
    }
}

/// The hybrid policy:
///
/// 1. a locked name must pass online auth;
/// 2. an unlocked name is looked up; premium names must pass online auth (so
///    they cannot be squatted offline first) and are then locked;
/// 3. a non-premium name joins offline;
/// 4. if the lookup fails, [`LookupFailure`] decides.
#[allow(clippy::too_many_arguments)]
fn hybrid(
    s: &mut Conn,
    codec: &dyn ProtocolCodec,
    cfg: &AuthConfig,
    store: &dyn MetaStore,
    key: &ServerKey,
    api: &dyn ProfileApi,
    cache: &LookupCache,
    name: &str,
    ip: Option<IpAddr>,
) -> std::io::Result<Admission> {
    if let Some(lock) = store::get_by_name(store, name) {
        log::info(&format!(
            "[auth] {name} is locked to {} (premium): online auth required",
            crate::auth::api::hyphenated_uuid(lock.uuid)
        ));
        let outcome = online_login(s, codec, key, api, name, ip)?;
        return Ok(settle_online(store, name, outcome));
    }

    match cache.get(api, name) {
        Ok(Some(_uuid)) => {
            // A premium name we have not locked yet: verify, then lock.
            let outcome = online_login(s, codec, key, api, name, ip)?;
            Ok(settle_online(store, name, outcome))
        }
        Ok(None) => {
            log::info(&format!("[auth] {name} is not premium: offline login"));
            Ok(Admission::Offline)
        }
        Err(e) => match cfg.on_lookup_failure {
            LookupFailure::Offline => {
                log::warn(&format!(
                    "[auth] profile lookup for {name} failed ({e:?}); admitting offline \
                     (on_lookup_failure = offline)"
                ));
                Ok(Admission::Offline)
            }
            LookupFailure::Deny => {
                let msg = "Could not check whether this name is premium; try again later.";
                log::info(&format!("[auth] {name} refused: lookup failed ({e:?})"));
                Ok(Admission::Refused(msg.into()))
            }
        },
    }
}

/// Turn an online handshake outcome into an admission, locking on success.
fn settle_online(store: &dyn MetaStore, name: &str, outcome: OnlineOutcome) -> Admission {
    match outcome {
        OnlineOutcome::Ok(profile) => {
            let now = super::now_secs();
            let first_seen = store::get_by_name(store, &profile.name)
                .or_else(|| store::get_by_uuid(store, profile.uuid))
                .map(|l| l.first_seen)
                .unwrap_or(now);
            let lock = Lock {
                uuid: profile.uuid,
                name: profile.name.to_ascii_lowercase(),
                first_seen,
                last_seen: now,
            };
            store::put(store, &lock);
            log::info(&format!(
                "[auth] {name} authenticated online as {} (uuid {}, locked)",
                profile.name,
                crate::auth::api::hyphenated_uuid(profile.uuid)
            ));
            Admission::Online(profile)
        }
        OnlineOutcome::Refused => {
            log::info(&format!("[auth] {name} refused: online auth failed"));
            Admission::Refused(premium_message(name))
        }
        OnlineOutcome::ApiDown => {
            log::warn(&format!(
                "[auth] {name} refused: session server unreachable, cannot verify a premium name"
            ));
            Admission::Refused(
                "The authentication service is unavailable; this name is premium and cannot \
                 join offline."
                    .into(),
            )
        }
    }
}

/// The message shown to an offline client that claimed a premium name.
fn premium_message(name: &str) -> String {
    format!("`{name}` is a premium name. Log in with the official Minecraft launcher to use it.")
}

/// Run the encryption handshake and `hasJoined`.
///
/// Returns an io error only when the socket itself failed; a client that
/// sends the wrong bytes, or a Mojang answer that says "no", is an
/// [`OnlineOutcome`].
fn online_login(
    s: &mut Conn,
    codec: &dyn ProtocolCodec,
    key: &ServerKey,
    api: &dyn ProfileApi,
    name: &str,
    ip: Option<IpAddr>,
) -> std::io::Result<OnlineOutcome> {
    let mut token = [0u8; TOKEN_LEN];
    crypto::random_bytes(&mut token);
    // The server id is empty on every modern server: Mojang no longer uses
    // it, and the client hashes whatever string we send.
    let server_id = "";
    let public_key = key.public_der();

    let mut req = PacketOut::new(ENCRYPTION_REQUEST);
    req.string(server_id);
    req.var_int(public_key.len() as i32).bytes(public_key);
    req.var_int(TOKEN_LEN as i32).bytes(&token);
    if codec.encryption_request_has_should_authenticate() {
        // 1.19/1.19.2 only. We do not want chat signing, and the client still
        // returns the shared secret and verify token.
        req.bool(false);
    }
    req.send(s)?;

    let Some(p) = read_packet(s)? else {
        return Ok(OnlineOutcome::Refused);
    };
    if p.id != ENCRYPTION_RESPONSE {
        return Ok(OnlineOutcome::Refused);
    }
    let mut pin = PacketIn::new(&p.data);
    let Ok(secret_enc) = read_byte_array(&mut pin) else {
        return Ok(OnlineOutcome::Refused);
    };
    let Ok(token_enc) = read_byte_array(&mut pin) else {
        return Ok(OnlineOutcome::Refused);
    };
    let (Some(secret), Some(tok)) = (key.decrypt(&secret_enc), key.decrypt(&token_enc)) else {
        return Ok(OnlineOutcome::Refused);
    };
    if tok != token || secret.len() != crypto::SECRET_LEN {
        return Ok(OnlineOutcome::Refused);
    }
    let mut secret16 = [0u8; crypto::SECRET_LEN];
    secret16.copy_from_slice(&secret);

    // From here the client is already encrypting, so every byte we send must
    // be encrypted too — including a refusal.
    s.enable_encryption(&secret16);

    let hash = crypto::server_id_hash(server_id, &secret16, public_key);
    match api.has_joined(name, &hash, ip) {
        Ok(Some(profile)) => Ok(OnlineOutcome::Ok(profile)),
        Ok(None) => Ok(OnlineOutcome::Refused),
        Err(ApiError::Unreachable) => Ok(OnlineOutcome::ApiDown),
        Err(_) => Ok(OnlineOutcome::Refused),
    }
}

/// Read a VarInt-prefixed byte array.
fn read_byte_array(pin: &mut PacketIn) -> Result<Vec<u8>, std::io::Error> {
    let len = pin.var_int()?;
    if len < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "negative array length",
        ));
    }
    let mut out = vec![0u8; len as usize];
    for b in out.iter_mut() {
        *b = pin.u8()?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::api::ApiError;
    use crate::auth::store::test_store::MapStore;
    use std::sync::Mutex;

    /// A mock Mojang: fixed `hasJoined` and lookup answers, call counters.
    struct Mock {
        joined: Result<Option<Profile>, ApiError>,
        looked: Result<Option<u128>, ApiError>,
        joined_calls: Mutex<usize>,
        lookup_calls: Mutex<usize>,
    }

    impl Mock {
        fn new(
            joined: Result<Option<Profile>, ApiError>,
            looked: Result<Option<u128>, ApiError>,
        ) -> Self {
            Self {
                joined,
                looked,
                joined_calls: Mutex::new(0),
                lookup_calls: Mutex::new(0),
            }
        }
        fn profile(uuid: u128, name: &str) -> Profile {
            Profile {
                uuid,
                name: name.into(),
                properties: Vec::new(),
            }
        }
    }

    impl ProfileApi for Mock {
        fn has_joined(
            &self,
            _u: &str,
            _h: &str,
            _ip: Option<IpAddr>,
        ) -> Result<Option<Profile>, ApiError> {
            *self.joined_calls.lock().unwrap() += 1;
            self.joined.clone()
        }
        fn lookup_name(&self, _n: &str) -> Result<Option<u128>, ApiError> {
            *self.lookup_calls.lock().unwrap() += 1;
            self.looked
        }
    }

    fn cfg(mode: AuthMode, failure: LookupFailure) -> AuthConfig {
        AuthConfig {
            mode,
            on_lookup_failure: failure,
            ..Default::default()
        }
    }

    /// A `Conn` pair over a socket, with the client side speaking enough of
    /// the handshake to let `online_login` proceed. The "client" thread reads
    /// the Encryption Request and answers it.
    fn handshake_pair(key: &ServerKey, answer: Answer) -> (Conn, std::net::TcpStream) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let mut peer = client.try_clone().unwrap();
        let (server, _) = listener.accept().unwrap();
        let key_der = key.public_der().to_vec();

        std::thread::spawn(move || {
            // Read the Encryption Request frame.
            let req = read_client_frame(&mut peer);
            // Recover the verify token, encrypt a fresh secret and the token
            // with the server's public key.
            let token = req_token(&req);
            let secret = match answer {
                Answer::Good => [9u8; crypto::SECRET_LEN],
                Answer::BadToken => [9u8; crypto::SECRET_LEN],
            };
            let enc_secret = rsa_encrypt(&key_der, &secret);
            let enc_token = match answer {
                Answer::Good => rsa_encrypt(&key_der, &token),
                Answer::BadToken => rsa_encrypt(&key_der, &[0u8; TOKEN_LEN]),
            };
            let mut body = Vec::new();
            write_varint(&mut body, enc_secret.len() as i32);
            body.extend_from_slice(&enc_secret);
            write_varint(&mut body, enc_token.len() as i32);
            body.extend_from_slice(&enc_token);
            let mut frame = Vec::new();
            write_varint(&mut frame, body.len() as i32 + 1);
            write_varint(&mut frame, 0x01); // packet id
            frame.extend_from_slice(&body);
            let _ = peer.write_all(&frame);
        });
        (Conn::new(server), client)
    }

    enum Answer {
        Good,
        BadToken,
    }

    fn read_client_frame(s: &mut std::net::TcpStream) -> Vec<u8> {
        use std::io::Read;
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let mut b = [0u8; 1];
            s.read_exact(&mut b).unwrap();
            len |= ((b[0] & 0x7f) as usize) << shift;
            if b[0] & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).unwrap();
        buf
    }

    /// Pull the verify token out of an Encryption Request body (id + server
    /// id + key + token).
    fn req_token(body: &[u8]) -> [u8; TOKEN_LEN] {
        let mut p = PacketIn::new(&body[1..]);
        let _ = p.string().unwrap();
        let klen = p.var_int().unwrap() as usize;
        for _ in 0..klen {
            let _ = p.u8().unwrap();
        }
        let tlen = p.var_int().unwrap() as usize;
        let mut token = [0u8; TOKEN_LEN];
        for b in token.iter_mut() {
            *b = p.u8().unwrap();
        }
        assert_eq!(tlen, TOKEN_LEN);
        token
    }

    fn rsa_encrypt(der: &[u8], data: &[u8]) -> Vec<u8> {
        use rsa::pkcs8::DecodePublicKey;
        use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
        let key = RsaPublicKey::from_public_key_der(der).unwrap();
        key.encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, data)
            .unwrap()
    }

    fn write_varint(out: &mut Vec<u8>, v: i32) {
        let mut u = v as u32;
        loop {
            if u & !0x7f == 0 {
                out.push(u as u8);
                return;
            }
            out.push(((u & 0x7f) | 0x80) as u8);
            u >>= 7;
        }
    }

    fn online_profile(name: &str) -> Result<Option<Profile>, ApiError> {
        Ok(Some(Mock::profile(0xAAAA, name)))
    }

    #[test]
    fn offline_mode_admits_without_touching_mojang() {
        let store = MapStore::default();
        let mock = Mock::new(Ok(None), Ok(None));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Offline, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "Steve",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Offline));
        assert_eq!(*mock.joined_calls.lock().unwrap(), 0);
        assert_eq!(*mock.lookup_calls.lock().unwrap(), 0);
    }

    #[test]
    fn online_mode_authenticates_and_locks() {
        let store = MapStore::default();
        let mock = Mock::new(online_profile("Notch"), Ok(None));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Online, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "Notch",
            None,
        )
        .unwrap();
        match a {
            Admission::Online(p) => assert_eq!(p.uuid, 0xAAAA),
            other => panic!("expected online, got {other:?}"),
        }
        assert_eq!(store::get_by_name(&store, "notch").unwrap().uuid, 0xAAAA);
    }

    #[test]
    fn online_mode_refuses_a_bad_verify_token() {
        let store = MapStore::default();
        let mock = Mock::new(online_profile("x"), Ok(None));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::BadToken);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Online, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "x",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Refused(_)));
        assert_eq!(
            *mock.joined_calls.lock().unwrap(),
            0,
            "never called hasJoined"
        );
    }

    #[test]
    fn hybrid_admits_a_non_premium_name_offline() {
        let store = MapStore::default();
        let mock = Mock::new(Ok(None), Ok(None));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "_alpha_01",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Offline));
        assert_eq!(*mock.lookup_calls.lock().unwrap(), 1);
    }

    #[test]
    fn hybrid_refuses_an_offline_attempt_on_a_locked_name() {
        let store = MapStore::default();
        store::put(
            &store,
            &Lock {
                uuid: 0xAAAA,
                name: "notch".into(),
                first_seen: 1,
                last_seen: 1,
            },
        );
        // The client fails online auth (bad token), so the locked name is
        // refused.
        let mock = Mock::new(online_profile("Notch"), Ok(None));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::BadToken);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "Notch",
            None,
        )
        .unwrap();
        match a {
            Admission::Refused(msg) => assert!(msg.contains("premium")),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn hybrid_requires_online_for_a_premium_unlocked_name_and_locks_it() {
        let store = MapStore::default();
        let mock = Mock::new(online_profile("Notch"), Ok(Some(0xAAAA)));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            "Notch",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Online(_)));
        assert_eq!(store::get_by_name(&store, "notch").unwrap().uuid, 0xAAAA);
    }

    #[test]
    fn hybrid_lookup_failure_follows_the_configured_fallback() {
        let store = MapStore::default();
        let down = Mock::new(Ok(None), Err(ApiError::Unreachable));

        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &down,
            &LookupCache::new(),
            "Alice",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Offline));

        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Deny),
            &store,
            super::super::test_key(),
            &down,
            &LookupCache::new(),
            "Alice",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Refused(_)));
    }
}
