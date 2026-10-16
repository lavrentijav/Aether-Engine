//! The RCON remote-console protocol.
//!
//! RCON is the standard remote-admin channel: a client authenticates with a
//! shared password, then sends command lines and reads their output. This
//! speaks the vanilla protocol exactly — little-endian `int32` sized packets
//! of `id`, `type`, body and two trailing NULs — and dispatches each command
//! through the same tree the console and players use, as the RCON source (op
//! level 4, no entity, no position).
//!
//! The listener is off unless `[rcon] enabled = true`, and refuses to start
//! with an empty password. It binds loopback by default and the caller warns
//! when it is exposed.
//!
//! Packet types:
//! * `AUTH` (3) — client → server, the password.
//! * `AUTH_RESPONSE` (2) — server → client; the request id on success, `-1`
//!   on failure.
//! * `EXECCOMMAND` (2) — client → server, a command line.
//! * `RESPONSE_VALUE` (0) — server → client, the command's output.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::commands::{dispatch_source, tree::ServerKind, tree::Source};
use crate::config::RconConfig;
use crate::players::SharedRegistry;
use crate::session::DemoWorld;

/// `RESPONSE_VALUE`: a command's output.
pub const TYPE_RESPONSE: i32 = 0;
/// `EXECCOMMAND` (client) and `AUTH_RESPONSE` (server) share a type id in the
/// vanilla protocol.
pub const TYPE_COMMAND: i32 = 2;
/// `AUTH`: the client's password.
pub const TYPE_AUTH: i32 = 3;

/// The most body bytes a packet may carry; vanilla caps RCON at 4096.
pub const MAX_BODY: usize = 4096;
/// A whole packet's `SIZE` includes the two leading ints and the two NULs.
const MAX_PACKET: i32 = (MAX_BODY + 10) as i32;

/// How long a connection may sit idle (including before authenticating)
/// before it is closed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The most connections one address may open per minute. Each connection is a
/// potential authentication attempt, so this is the brute-force gate.
pub const AUTH_PER_MIN: u32 = 30;

/// One decoded RCON packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// The request id, echoed back in the response.
    pub id: i32,
    /// The packet type.
    pub kind: i32,
    /// The body, decoded as text with trailing NULs stripped.
    pub body: String,
}

/// Read one packet: `SIZE` then `ID`, `TYPE`, body and two NULs.
///
/// Errors on a truncated, malformed or oversized `SIZE` so a hostile client
/// cannot make the server allocate without bound.
pub fn read_packet(r: &mut impl Read) -> io::Result<Packet> {
    let len = read_i32(r)?;
    if !(10..=MAX_PACKET).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("rcon packet size out of range: {len}"),
        ));
    }
    let id = read_i32(r)?;
    let kind = read_i32(r)?;
    let body_len = (len - 10) as usize;
    let mut raw = vec![0u8; body_len + 2];
    r.read_exact(&mut raw)?;
    raw.truncate(body_len);
    while raw.last() == Some(&0) {
        raw.pop();
    }
    Ok(Packet {
        id,
        kind,
        body: String::from_utf8_lossy(&raw).into_owned(),
    })
}

/// Write one packet: `SIZE`, `ID`, `TYPE`, body and two trailing NULs.
pub fn write_packet(w: &mut impl Write, id: i32, kind: i32, body: &str) -> io::Result<()> {
    let bytes = body.as_bytes();
    let len = (bytes.len() + 10) as i32;
    let mut out = Vec::with_capacity(bytes.len() + 14);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(bytes);
    out.push(0);
    out.push(0);
    w.write_all(&out)?;
    w.flush()
}

/// Frame one packet without writing it, for a client that writes in pieces.
pub fn frame_packet(id: i32, kind: i32, body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    // The writer above cannot fail on a `Vec`, so this is total.
    let _ = write_packet(&mut out, id, kind, body);
    out
}

fn read_i32(r: &mut impl Read) -> io::Result<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}

/// Per-address authentication-failure limiter.
///
/// A sliding one-minute window, like [`crate::auth::ratelimit`], recording
/// only *failed* passwords: a legitimate client that authenticates is never
/// counted. A blocked address is refused before its password is even read.
struct AuthLimiter {
    max: u32,
    hits: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl AuthLimiter {
    fn new(max: u32) -> Self {
        Self {
            max,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `ip` has already failed too often in the window. Read-only: a
    /// blocked check never grows the map.
    fn blocked(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip else { return false };
        let cutoff = Instant::now() - Duration::from_secs(60);
        let map = self.hits.lock().unwrap();
        match map.get(&ip) {
            None => false,
            Some(attempts) => attempts.iter().filter(|t| **t > cutoff).count() as u32 >= self.max,
        }
    }

    /// Record one failed attempt from `ip`.
    fn record(&self, ip: Option<IpAddr>) {
        let Some(ip) = ip else { return };
        let mut map = self.hits.lock().unwrap();
        if map.len() > 4096 {
            let cutoff = Instant::now() - Duration::from_secs(60);
            map.retain(|_, v| v.iter().any(|t| *t > cutoff));
        }
        map.entry(ip).or_default().push(Instant::now());
    }
}

/// A running RCON endpoint. Cheap to clone into a per-connection thread.
#[derive(Clone)]
pub struct Server {
    password: String,
    registry: SharedRegistry,
    world: Arc<DemoWorld>,
    auth: Arc<AuthLimiter>,
    timeout: Duration,
}

impl Server {
    /// A server authenticating against `password`.
    pub fn new(password: String, registry: SharedRegistry, world: Arc<DemoWorld>) -> Self {
        Self {
            password,
            registry,
            world,
            auth: Arc::new(AuthLimiter::new(AUTH_PER_MIN)),
            timeout: READ_TIMEOUT,
        }
    }

    /// Override the idle/auth timeout (tests use a short one).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the per-address failure cap (tests use a small one).
    pub fn with_auth_limit(mut self, max: u32) -> Self {
        self.auth = Arc::new(AuthLimiter::new(max));
        self
    }

    /// Whether `presented` is the configured password, compared in constant
    /// time so a timing side channel does not leak it.
    fn password_matches(&self, presented: &str) -> bool {
        let a = self.password.as_bytes();
        let b = presented.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b) {
            diff |= x ^ y;
        }
        diff == 0
    }

    /// Serve one connection: authenticate, then run commands until the peer
    /// leaves or goes idle.
    pub fn serve_conn(&self, mut stream: TcpStream, peer: Option<IpAddr>) -> io::Result<()> {
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        if self.auth.blocked(peer) {
            crate::log::warn(&format!(
                "rcon: refusing {} — too many authentication failures",
                peer_str(peer)
            ));
            return Ok(());
        }

        // The first packet must be an AUTH.
        let packet = match read_packet(&mut stream) {
            Ok(p) => p,
            Err(e) if is_timeout(&e) => {
                crate::log::info(&format!(
                    "rcon: {} never authenticated; closing",
                    peer_str(peer)
                ));
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if packet.kind != TYPE_AUTH {
            let _ = write_packet(&mut stream, -1, TYPE_COMMAND, "");
            return Ok(());
        }
        if !self.password_matches(&packet.body) {
            self.auth.record(peer);
            crate::log::warn(&format!(
                "rcon: authentication failed for {}",
                peer_str(peer)
            ));
            // Vanilla answers a failed auth with id -1.
            let _ = write_packet(&mut stream, -1, TYPE_COMMAND, "");
            return Ok(());
        }
        // Success: echo the request id with an empty AUTH_RESPONSE.
        write_packet(&mut stream, packet.id, TYPE_COMMAND, "")?;
        crate::log::info(&format!("rcon: {} authenticated", peer_str(peer)));

        // Commands, one or many over this connection.
        loop {
            let packet = match read_packet(&mut stream) {
                Ok(p) => p,
                Err(e) if is_timeout(&e) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    crate::log::warn(&format!(
                        "rcon: {} sent a malformed packet: {e}",
                        peer_str(peer)
                    ));
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if packet.kind != TYPE_COMMAND {
                // A type we do not handle after auth; ignore it rather than
                // dropping an otherwise healthy session.
                continue;
            }
            let reply = dispatch_source(
                &packet.body,
                Source::Server(ServerKind::Rcon),
                &self.registry,
                &self.world,
            );
            // A multi-line reply is one response body, lines joined by `\n`,
            // exactly as vanilla concatenates them.
            let body = reply.0.join("\n");
            write_packet(&mut stream, packet.id, TYPE_RESPONSE, &body)?;
        }
    }
}

/// Bind and serve until the process ends. Never returns while the listener is
/// healthy; a bind failure is returned so the caller can refuse to start.
pub fn listen(cfg: &RconConfig, registry: SharedRegistry, world: Arc<DemoWorld>) -> io::Result<()> {
    let addr = cfg.address();
    let listener = TcpListener::bind(&addr)?;
    let server = Arc::new(Server::new(cfg.password.clone(), registry, world));
    crate::log::info(&format!("rcon       : listening on {addr}"));
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let peer = s.peer_addr().ok().map(|a| a.ip());
                let server = Arc::clone(&server);
                std::thread::Builder::new()
                    .name("rcon-conn".into())
                    .spawn(move || {
                        if let Err(e) = server.serve_conn(s, peer) {
                            if !is_timeout(&e) {
                                crate::log::info(&format!(
                                    "rcon: {} connection ended: {e}",
                                    peer_str(peer)
                                ));
                            }
                        }
                    })
                    .ok();
            }
            Err(e) => crate::log::warn(&format!("rcon: accept failed: {e}")),
        }
    }
    Ok(())
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn peer_str(peer: Option<IpAddr>) -> String {
    peer.map(|p| p.to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::Registry;
    use std::net::{Shutdown, TcpListener};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A throwaway world on a temp store, unique per call.
    pub(super) fn test_world() -> DemoWorld {
        use aether_api::{FjallStore, World};
        static N: AtomicU64 = AtomicU64::new(0);
        let tag = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aether-rcon-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", tag).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, tag, revision, &cfg);
        World::new(store, generator)
    }

    /// A connected socket pair driven by the test; the server half runs on
    /// its own thread.
    struct Fixture {
        client: TcpStream,
    }

    fn fixture(password: &str) -> Fixture {
        let world = Arc::new(test_world());
        let registry: SharedRegistry = Arc::new(Registry::default());
        let server = Server::new(password.to_string(), registry, world)
            .with_timeout(Duration::from_millis(500));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server_side, _) = listener.accept().unwrap();
        let handle = server.clone();
        std::thread::spawn(move || {
            let _ = handle.serve_conn(server_side, Some("127.0.0.1".parse().unwrap()));
        });
        Fixture { client }
    }

    impl Fixture {
        fn send(&mut self, id: i32, kind: i32, body: &str) {
            let bytes = frame_packet(id, kind, body);
            self.client.write_all(&bytes).unwrap();
            self.client.flush().unwrap();
        }

        fn send_raw(&mut self, bytes: &[u8]) {
            self.client.write_all(bytes).unwrap();
            self.client.flush().unwrap();
        }

        fn recv(&mut self) -> Packet {
            self.client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            read_packet(&mut self.client).expect("a response packet")
        }

        fn auth(&mut self) {
            self.send(1, TYPE_AUTH, "secret");
            let resp = self.recv();
            assert_eq!(resp.id, 1, "auth echoes the request id");
            assert_eq!(resp.kind, TYPE_COMMAND);
        }
    }

    #[test]
    fn a_packet_round_trips_through_the_framing() {
        let mut buf = Vec::new();
        write_packet(&mut buf, 7, TYPE_RESPONSE, "hello").unwrap();
        // id + type + "hello" + two NULs + size prefix.
        assert_eq!(buf.len(), 4 + 4 + 4 + 5 + 2);
        assert_eq!(
            i32::from_le_bytes(buf[0..4].try_into().unwrap()),
            4 + 4 + 5 + 2
        );
        let mut cur = std::io::Cursor::new(buf);
        assert_eq!(
            read_packet(&mut cur).unwrap(),
            Packet {
                id: 7,
                kind: TYPE_RESPONSE,
                body: "hello".into()
            }
        );
    }

    #[test]
    fn auth_success_and_failure_are_reported_by_id() {
        let mut ok = fixture("secret");
        ok.send(5, TYPE_AUTH, "secret");
        let resp = ok.recv();
        assert_eq!(resp.id, 5, "success echoes the id");
        assert_eq!(resp.kind, TYPE_COMMAND, "AUTH_RESPONSE");
        assert!(resp.body.is_empty());

        let mut bad = fixture("secret");
        bad.send(6, TYPE_AUTH, "wrong");
        let resp = bad.recv();
        assert_eq!(resp.id, -1, "failure is id -1");
    }

    #[test]
    fn a_wrong_packet_type_before_auth_is_closed() {
        let mut fx = fixture("secret");
        fx.send(1, TYPE_RESPONSE, "not auth");
        let resp = fx.recv();
        assert_eq!(resp.id, -1);
        assert!(resp.body.is_empty());
    }

    #[test]
    fn an_oversized_packet_is_rejected() {
        let mut fx = fixture("secret");
        // Claim a body far past the cap.
        let bogus = (MAX_PACKET + 1).to_le_bytes();
        fx.send_raw(&bogus);
        // The server closes without a response; the next read sees EOF.
        fx.client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 1];
        let got = fx.client.read(&mut buf);
        assert!(
            matches!(got, Ok(0)) || got.is_err(),
            "expected EOF, got {got:?}"
        );
    }

    #[test]
    fn fragmented_writes_are_reassembled() {
        let mut fx = fixture("secret");
        let bytes = frame_packet(9, TYPE_AUTH, "secret");
        // Feed the packet one byte at a time.
        for b in bytes {
            fx.send_raw(&[b]);
        }
        let resp = fx.recv();
        assert_eq!(resp.id, 9, "the whole packet was read");
    }

    #[test]
    fn several_commands_share_one_connection_each_echoing_its_id() {
        let mut fx = fixture("secret");
        fx.auth();
        fx.send(10, TYPE_COMMAND, "help");
        let first = fx.recv();
        assert_eq!(first.id, 10);
        assert_eq!(first.kind, TYPE_RESPONSE);
        assert!(
            first.body.contains("commands pending"),
            "help output: {:?}",
            first.body
        );
        fx.send(11, TYPE_COMMAND, "definitely_not_a_command");
        let second = fx.recv();
        assert_eq!(second.id, 11);
        assert_eq!(second.body, "Unknown or incomplete command");
    }

    #[test]
    fn a_multi_line_reply_is_one_response_body() {
        let mut fx = fixture("secret");
        fx.auth();
        fx.send(20, TYPE_COMMAND, "help");
        let resp = fx.recv();
        assert_eq!(resp.kind, TYPE_RESPONSE);
        // Every line is in one body, separated by newlines, not one packet per
        // line.
        assert!(
            resp.body.contains('\n'),
            "expected newlines: {:?}",
            resp.body
        );
        // And the connection is quiet afterwards (no trailing packets).
        fx.client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut buf = [0u8; 4];
        assert!(
            fx.client.read(&mut buf).is_err(),
            "no extra response packet follows"
        );
    }

    #[test]
    fn a_connection_that_never_authenticates_times_out() {
        let mut fx = fixture("secret");
        // Send nothing: the server's short read timeout closes the socket.
        fx.client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 1];
        let got = fx.client.read(&mut buf);
        assert!(
            matches!(got, Ok(0)) || got.is_err(),
            "expected the idle socket to close, got {got:?}"
        );
    }

    #[test]
    fn repeated_auth_failures_are_rate_limited() {
        let world = Arc::new(test_world());
        let registry: SharedRegistry = Arc::new(Registry::default());
        // A tiny cap makes the block observable in a unit test.
        let server = Server::new("secret".into(), registry, world)
            .with_auth_limit(2)
            .with_timeout(Duration::from_millis(300));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = {
            let server = server.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().take(3) {
                    let Ok(stream) = stream else { break };
                    let server = server.clone();
                    std::thread::spawn(move || {
                        let _ = server.serve_conn(stream, Some("127.0.0.1".parse().unwrap()));
                    });
                }
            })
        };

        let fail = |password: &str| {
            let mut c = TcpStream::connect(addr).unwrap();
            write_packet(&mut c, 1, TYPE_AUTH, password).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            read_packet(&mut c)
        };
        // Two recorded failures...
        assert_eq!(fail("bad").unwrap().id, -1);
        assert_eq!(fail("wrong").unwrap().id, -1);
        // ...and the third connection is refused without a reply (EOF).
        let mut c = TcpStream::connect(addr).unwrap();
        write_packet(&mut c, 1, TYPE_AUTH, "secret").unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 1];
        assert!(
            matches!(c.read(&mut buf), Ok(0) | Err(_)),
            "the address is blocked"
        );
        let _ = accept.join();
    }

    #[test]
    fn stop_over_rcon_requests_a_clean_shutdown() {
        let _guard = crate::ops::stop_test_lock();
        crate::ops::clear_stop();
        let mut fx = fixture("secret");
        fx.auth();
        fx.send(30, TYPE_COMMAND, "stop");
        let resp = fx.recv();
        assert_eq!(resp.id, 30);
        assert!(
            crate::ops::stopping(),
            "the shared stop path was taken: {:?}",
            resp.body
        );
        crate::ops::clear_stop();
    }

    #[test]
    fn disabled_by_default_and_an_empty_password_is_refused() {
        let cfg = RconConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.validate().is_ok());
        let on = RconConfig {
            enabled: true,
            ..RconConfig::default()
        };
        assert!(on.validate().is_err());
    }

    #[test]
    fn a_client_that_disconnects_mid_packet_is_handled() {
        let mut fx = fixture("secret");
        // Half a size prefix, then close.
        fx.send_raw(&[1, 0]);
        fx.client.shutdown(Shutdown::Both).unwrap();
    }
}
