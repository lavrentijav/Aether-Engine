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
use crate::auth::bounce::BounceCache;
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
    /// The client left during authentication. Nothing to admit and nothing
    /// to say: the connection is over.
    Ended,
}

/// The result of the online handshake alone.
enum OnlineOutcome {
    /// The client proved the account.
    Ok(Profile),
    /// The client answered the Encryption Request but Mojang would not
    /// confirm the session — a cracked or modified client.
    NoSession,
    /// The client sent something that is not a valid Encryption Response.
    BadResponse,
    /// Mojang could not be reached, so the session is unverified.
    ApiDown,
    /// The client went away before answering.
    Dropped,
}

/// Resolve a login under the configured policy.
///
/// `store` is the world's lock table; `key`, `api`, `cache` and `bounce` are
/// the process-wide server key, HTTP client and caches (see [`super`]).
/// `name` is the client's claimed username.
#[allow(clippy::too_many_arguments)]
pub fn authenticate(
    s: &mut Conn,
    codec: &dyn ProtocolCodec,
    cfg: &AuthConfig,
    store: &dyn MetaStore,
    key: &ServerKey,
    api: &dyn ProfileApi,
    cache: &LookupCache,
    bounce: &BounceCache,
    name: &str,
    ip: Option<IpAddr>,
) -> std::io::Result<Admission> {
    match cfg.mode {
        AuthMode::Offline => Ok(Admission::Offline),
        AuthMode::Online => {
            let outcome = online_login(s, codec, key, api, name, ip)?;
            Ok(online_only(store, name, outcome))
        }
        AuthMode::Hybrid => hybrid(s, codec, cfg, store, key, api, cache, bounce, name, ip),
    }
}

/// Every login in `online` mode must authenticate; there is no offline
/// fallback.
fn online_only(store: &dyn MetaStore, name: &str, outcome: OnlineOutcome) -> Admission {
    match outcome {
        OnlineOutcome::Ok(profile) => lock_profile(store, name, profile),
        OnlineOutcome::Dropped => {
            log::info(&format!("[auth] {name} left during online auth"));
            Admission::Ended
        }
        OnlineOutcome::ApiDown => {
            log::warn(&format!(
                "[auth] {name} refused: session server unreachable in online mode"
            ));
            Admission::Refused("The authentication service is unavailable; try again later.".into())
        }
        _ => {
            log::info(&format!("[auth] {name} refused: online auth failed"));
            Admission::Refused(
                "Online-mode login failed. Log in with the official Minecraft launcher.".into(),
            )
        }
    }
}

/// The hybrid policy, as revised by the owner:
///
/// 1. a **locked** name (one that has completed online auth on *this* server)
///    must pass online auth; a locked name's owner arriving under a new name
///    moves the lock (handled where the response's uuid is compared — the
///    lock follows the uuid, see [`store::put`]);
/// 2. offline play is open to everyone. An **unlocked** name is looked up: a
///    premium name is *offered* encryption, and
///    * if `hasJoined` succeeds → online and the name is locked;
///    * if it fails → offline in the same connection;
///    * if the client drops mid-handshake → offline next attempt from the
///      same address and name within [`bounce::TTL`], with no offer;
///    * a non-premium name → offline directly;
/// 3. a lookup failure follows [`LookupFailure`].
#[allow(clippy::too_many_arguments)]
fn hybrid(
    s: &mut Conn,
    codec: &dyn ProtocolCodec,
    cfg: &AuthConfig,
    store: &dyn MetaStore,
    key: &ServerKey,
    api: &dyn ProfileApi,
    cache: &LookupCache,
    bounce: &BounceCache,
    name: &str,
    ip: Option<IpAddr>,
) -> std::io::Result<Admission> {
    if let Some(lock) = store::get_by_name(store, name) {
        // Locked: the owner must authenticate. This is the only place a name
        // may be refused for auth reasons.
        log::info(&format!(
            "[auth] {name} is locked to {} (completed online auth here before): online auth required",
            crate::auth::api::hyphenated_uuid(lock.uuid)
        ));
        let outcome = online_login(s, codec, key, api, name, ip)?;
        return Ok(match outcome {
            OnlineOutcome::Ok(profile) => lock_profile(store, name, profile),
            OnlineOutcome::Dropped => {
                log::info(&format!("[auth] {name} left during online auth"));
                Admission::Ended
            }
            OnlineOutcome::ApiDown => {
                log::warn(&format!(
                    "[auth] {name} refused: session server unreachable, cannot verify a locked name"
                ));
                Admission::Refused(
                    "The authentication service is unavailable; this name is locked to an \
                     account and cannot join offline."
                        .into(),
                )
            }
            _ => {
                log::info(&format!(
                    "[auth] {name} refused: an offline attempt on a locked name"
                ));
                Admission::Refused(premium_message(name))
            }
        });
    }

    // A client that dropped during a recent auth from this address is not
    // offered encryption again for a while; admit it offline.
    if bounce.bounced(ip, name) {
        log::info(&format!(
            "[auth] {name} dropped during a recent online auth from this address; \
             offline without an encryption offer"
        ));
        return Ok(Admission::Offline);
    }

    match cache.get(api, name) {
        Ok(Some(_uuid)) => {
            // A premium name that has never authenticated here: offer to
            // verify it. A failure is not a refusal — offline stays open.
            match online_login(s, codec, key, api, name, ip)? {
                OnlineOutcome::Ok(profile) => Ok(lock_profile(store, name, profile)),
                OnlineOutcome::NoSession | OnlineOutcome::BadResponse => {
                    log::info(&format!(
                        "[auth] {name} is premium but did not complete online auth; \
                         admitting offline"
                    ));
                    Ok(Admission::Offline)
                }
                OnlineOutcome::ApiDown => match cfg.on_lookup_failure {
                    LookupFailure::Offline => {
                        log::warn(&format!(
                            "[auth] {name}: session server unreachable; admitting offline \
                             (on_lookup_failure = offline)"
                        ));
                        Ok(Admission::Offline)
                    }
                    LookupFailure::Deny => {
                        log::info(&format!(
                            "[auth] {name} refused: session server unreachable \
                             (on_lookup_failure = deny)"
                        ));
                        Ok(Admission::Refused(
                            "The authentication service is unavailable; try again later.".into(),
                        ))
                    }
                },
                OnlineOutcome::Dropped => {
                    bounce.record(ip, name);
                    log::info(&format!(
                        "[auth] {name} dropped during online auth; will admit offline from \
                         this address for the next {}s",
                        crate::auth::bounce::TTL.as_secs()
                    ));
                    Ok(Admission::Ended)
                }
            }
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

/// Admit an authenticated player and lock the name to their uuid.
fn lock_profile(store: &dyn MetaStore, name: &str, profile: Profile) -> Admission {
    let now = super::now_secs();
    // A name change at Mojang keeps the lock by uuid: the first-seen time
    // carries over from whichever entry already exists.
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

/// The message shown to an offline client that claimed a locked name.
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
    if req.send(s).is_err() {
        return Ok(OnlineOutcome::Dropped);
    }

    let p = match read_packet(s) {
        Ok(Some(p)) => p,
        // EOF, a timeout or a socket error: the client is gone. A vanilla
        // client with an invalid session does exactly this.
        Ok(None) | Err(_) => return Ok(OnlineOutcome::Dropped),
    };
    if p.id != ENCRYPTION_RESPONSE {
        return Ok(OnlineOutcome::BadResponse);
    }
    let mut pin = PacketIn::new(&p.data);
    let (Ok(secret_enc), Ok(token_enc)) = (read_byte_array(&mut pin), read_byte_array(&mut pin))
    else {
        return Ok(OnlineOutcome::BadResponse);
    };
    let (Some(secret), Some(tok)) = (key.decrypt(&secret_enc), key.decrypt(&token_enc)) else {
        return Ok(OnlineOutcome::BadResponse);
    };
    if tok != token || secret.len() != crypto::SECRET_LEN {
        return Ok(OnlineOutcome::BadResponse);
    }
    let mut secret16 = [0u8; crypto::SECRET_LEN];
    secret16.copy_from_slice(&secret);

    // From here the client is already encrypting, so every byte we send must
    // be encrypted too — including a refusal, and including the rest of an
    // offline login when `hasJoined` says no.
    s.enable_encryption(&secret16);

    let hash = crypto::server_id_hash(server_id, &secret16, public_key);
    match api.has_joined(name, &hash, ip) {
        Ok(Some(profile)) => Ok(OnlineOutcome::Ok(profile)),
        Ok(None) => Ok(OnlineOutcome::NoSession),
        Err(ApiError::Unreachable) => Ok(OnlineOutcome::ApiDown),
        Err(_) => Ok(OnlineOutcome::NoSession),
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

    /// What the test's "client" does when it gets the Encryption Request.
    enum Answer {
        /// Answer correctly with a matching verify token.
        Good,
        /// Answer with the wrong verify token.
        BadToken,
        /// Don't answer at all and disconnect — what a vanilla client with an
        /// invalid session does.
        Drop,
    }

    /// A `Conn` pair over a socket, with the client side speaking enough of
    /// the handshake to let `online_login` proceed. The "client" thread reads
    /// the Encryption Request and answers it (or drops).
    fn handshake_pair(key: &ServerKey, answer: Answer) -> (Conn, std::net::TcpStream) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let mut peer = client.try_clone().unwrap();
        let (server, _) = listener.accept().unwrap();
        let key_der = key.public_der().to_vec();

        std::thread::spawn(move || {
            // Read the Encryption Request frame; if there is none (the offer
            // was skipped, so the server moved straight to login), just leave.
            let Some(req) = read_client_frame(&mut peer) else {
                return;
            };
            if matches!(answer, Answer::Drop) {
                // Disconnect without answering. Shutting down both directions
                // is what makes the server see EOF: dropping `peer` alone
                // leaves the test's own clone of the socket open.
                let _ = peer.shutdown(std::net::Shutdown::Both);
                return;
            }
            // Recover the verify token, encrypt a fresh secret and the token
            // with the server's public key.
            let token = req_token(&req);
            let secret = [9u8; crypto::SECRET_LEN];
            let enc_secret = rsa_encrypt(&key_der, &secret);
            let enc_token = match answer {
                Answer::Good => rsa_encrypt(&key_der, &token),
                Answer::BadToken => rsa_encrypt(&key_der, &[0u8; TOKEN_LEN]),
                Answer::Drop => unreachable!(),
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

    /// Read one frame, or `None` on EOF (the server sent nothing).
    fn read_client_frame(s: &mut std::net::TcpStream) -> Option<Vec<u8>> {
        use std::io::Read;
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let mut b = [0u8; 1];
            if s.read_exact(&mut b).is_err() {
                return None;
            }
            len |= ((b[0] & 0x7f) as usize) << shift;
            if b[0] & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let mut buf = vec![0u8; len];
        if s.read_exact(&mut buf).is_err() {
            return None;
        }
        Some(buf)
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
            &BounceCache::new(),
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
            &BounceCache::new(),
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
            &BounceCache::new(),
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
            &BounceCache::new(),
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
            &BounceCache::new(),
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
            &BounceCache::new(),
            "Notch",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Online(_)));
        assert_eq!(store::get_by_name(&store, "notch").unwrap().uuid, 0xAAAA);
    }

    #[test]
    fn hybrid_admits_offline_when_hasjoined_fails_for_an_unlocked_premium_name() {
        // A cracked client that answered the Encryption Request without a
        // Mojang session: the name is premium but has never authenticated
        // here, so offline stays open. Not a refusal.
        let store = MapStore::default();
        let mock = Mock::new(Ok(None), Ok(Some(0xAAAA)));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            &BounceCache::new(),
            "Notch",
            Some("10.0.0.7".parse().unwrap()),
        )
        .unwrap();
        assert!(matches!(a, Admission::Offline));
        assert!(
            store::get_by_name(&store, "notch").is_none(),
            "a failed hasJoined must not lock the name"
        );
    }

    #[test]
    fn hybrid_drop_during_auth_records_a_bounce_and_the_next_attempt_goes_offline() {
        let store = MapStore::default();
        let bounce = BounceCache::new();
        let ip = Some("10.0.0.7".parse().unwrap());

        // First attempt: the client drops without answering (a vanilla client
        // with an invalid session).
        let mock = Mock::new(Ok(Some(Mock::profile(0xAAAA, "Notch"))), Ok(Some(0xAAAA)));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Drop);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            &bounce,
            "Notch",
            ip,
        )
        .unwrap();
        assert!(matches!(a, Admission::Ended));
        assert!(bounce.bounced(ip, "Notch"), "the drop is remembered");

        // The next attempt from the same address and name within the TTL is
        // admitted offline, with no encryption offer and no API calls.
        let mock2 = Mock::new(Ok(Some(Mock::profile(0xAAAA, "Notch"))), Ok(Some(0xAAAA)));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock2,
            &LookupCache::new(),
            &bounce,
            "Notch",
            ip,
        )
        .unwrap();
        assert!(matches!(a, Admission::Offline));
        assert_eq!(*mock2.joined_calls.lock().unwrap(), 0, "no hasJoined offer");
        assert_eq!(*mock2.lookup_calls.lock().unwrap(), 0, "no lookup either");
    }

    #[test]
    fn hybrid_bounce_from_another_address_does_not_skip_the_offer() {
        // The fallback is pinned to the same address, so a bounce at one
        // address must not let another address skip verification.
        let store = MapStore::default();
        let bounce = BounceCache::new();
        bounce.record(Some("10.0.0.1".parse().unwrap()), "Notch");

        let mock = Mock::new(Ok(Some(Mock::profile(0xAAAA, "Notch"))), Ok(Some(0xAAAA)));
        let (mut s, _c) = handshake_pair(super::super::test_key(), Answer::Good);
        let a = authenticate(
            &mut s,
            crate::protocol::codec_for(47).unwrap(),
            &cfg(AuthMode::Hybrid, LookupFailure::Offline),
            &store,
            super::super::test_key(),
            &mock,
            &LookupCache::new(),
            &bounce,
            "Notch",
            Some("10.0.0.2".parse().unwrap()),
        )
        .unwrap();
        assert!(matches!(a, Admission::Online(_)));
        assert_eq!(*mock.joined_calls.lock().unwrap(), 1);
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
            &BounceCache::new(),
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
            &BounceCache::new(),
            "Alice",
            None,
        )
        .unwrap();
        assert!(matches!(a, Admission::Refused(_)));
    }
}
