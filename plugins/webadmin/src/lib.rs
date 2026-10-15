//! The Aether web admin, as a native plugin.
//!
//! A read-only, local HTTP server inside the server process: a dashboard
//! (TPS/MSPT/players/memory), the heaviest regions (per-region tick-cost
//! sample plus a heat grid), and players with live inventory and profile
//! views. It is the "WEB1" stage of [`docs/tasks/QUEUE-WEB-admin.md`]; the map
//! and time machine are WEB2.
//!
//! The plugin owns no game data: it resolves the server's `ops@0.1` service
//! from the plugin host (declared in `plugin.toml`'s `requires`) and renders
//! what it returns. It never writes the world. Binding, the port and the token
//! are operator config (`[plugins.webadmin]`); no secret is compiled in.
//!
//! ```toml
//! [plugins.webadmin]
//! enabled = true
//! bind = "127.0.0.1:8090"   # default
//! token = "…"               # optional; else use `/webadmin token`
//! ```

mod auth;
mod http;
mod pages;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_ops::{OpsHandle, OpsService, SERVICE, VERSION};
use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    CommandResult, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

use auth::Auth;

/// The default bind address: loopback only.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// The plugin's manifest, as code (a native plugin is compiled in).
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: Manifest {
            id: "webadmin".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            api: "0.1".to_string(),
            entry: String::new(),
            tier: PluginTier::Native,
            name: "Web admin".to_string(),
            // Services first, then gameplay plugins; exact value is not
            // critical as it subscribes to no events.
            priority: 10,
            capabilities: Vec::new(),
            subscribe: Vec::new(),
            commands: vec![CommandDecl::Detailed {
                name: "webadmin".to_string(),
                node: "webadmin.command".to_string(),
                op: 3,
            }],
            requires: vec![format!("{SERVICE}@{VERSION}")],
            tick_period: 1,
            deny_on_fault: false,
        },
        new: || Box::new(WebAdmin::default()),
    }
}

/// The running plugin.
#[derive(Default)]
struct WebAdmin {
    ops: Option<Arc<OpsHandle>>,
    auth: Option<Arc<Auth>>,
    bind: Option<SocketAddr>,
    shutdown: Option<Arc<AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativePlugin for WebAdmin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let ops = ctx
            .services
            .get::<OpsHandle>(SERVICE, VERSION)
            .ok_or_else(|| {
                format!("the live-ops service `{SERVICE}@{VERSION}` is not registered")
            })?;

        let bind_text = ctx
            .config
            .get("bind")
            .cloned()
            .unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind: SocketAddr = bind_text
            .parse()
            .map_err(|e| format!("invalid `bind` address `{bind_text}`: {e}"))?;

        let auth = Arc::new(Auth::new(ctx.config.get("token").cloned()));
        if auth.has_config_token() {
            ctx.log(LogLevel::Info, "token from config");
        } else {
            ctx.log(
                LogLevel::Info,
                "no token configured; run `/webadmin token` in game to get a one-time token",
            );
        }

        // Register the command in the core registry so `/webadmin` is routed
        // to the plugin's `on_command` through the one permission gate.
        ctx.backend
            .register_command(ctx.id, "webadmin", "webadmin.command", 3)?;

        let listener =
            TcpListener::bind(bind).map_err(|e| format!("cannot bind web admin to {bind}: {e}"))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let ops = Arc::clone(&ops);
            let auth = Arc::clone(&auth);
            let shutdown = Arc::clone(&shutdown);
            std::thread::Builder::new()
                .name("webadmin".into())
                .spawn(move || serve(listener, ops, auth, shutdown))
                .map_err(|e| format!("cannot start web admin thread: {e}"))?
        };

        ctx.log(LogLevel::Info, &format!("listening on http://{bind}/"));
        self.ops = Some(ops);
        self.auth = Some(auth);
        self.bind = Some(bind);
        self.shutdown = Some(shutdown);
        self.thread = Some(thread);
        Ok(())
    }

    fn shutdown(&mut self, _ctx: &mut NativeContext) {
        if let Some(flag) = self.shutdown.take() {
            flag.store(true, Ordering::SeqCst);
            // Unblock `accept` with a throwaway connection.
            if let Some(addr) = self.bind {
                let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        if name != "webadmin" {
            return None;
        }
        if !p.operator {
            return Some(CommandResult {
                handled: true,
                lines: vec!["You must be an operator to run this command.".into()],
            });
        }
        let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
        let lines = match (sub, &self.auth) {
            ("token", Some(auth)) => {
                let token = auth.issue_one_time();
                ctx.log(LogLevel::Info, "issued a one-time web admin token");
                vec![
                    format!("Web admin one-time token: {token}"),
                    format!(
                        "Valid {} minutes. Open http://{}/ and paste it.",
                        auth::ONE_TIME_TTL.as_secs() / 60,
                        self.bind
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| DEFAULT_BIND.into())
                    ),
                ]
            }
            ("status", Some(_)) => vec![format!(
                "Web admin on http://{}/ ({}).",
                self.bind
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| DEFAULT_BIND.into()),
                if self.auth.as_ref().is_some_and(|a| a.has_config_token()) {
                    "token from config"
                } else {
                    "run /webadmin token for a one-time token"
                }
            )],
            _ => vec!["Usage: /webadmin token | /webadmin status".into()],
        };
        Some(CommandResult {
            handled: true,
            lines,
        })
    }
}

/// The accept loop; one short-lived thread per connection.
fn serve(listener: TcpListener, ops: Arc<OpsHandle>, auth: Arc<Auth>, shutdown: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                let ops = Arc::clone(&ops);
                let auth = Arc::clone(&auth);
                let _ = std::thread::Builder::new()
                    .name("webadmin-conn".into())
                    .spawn(move || handle_conn(stream, ops, auth));
            }
            Err(e) => {
                // A transient accept error (EMFILE, etc.) must not end the
                // server silently; log it through the plugin's own logger is
                // not available here, so keep going.
                let _ = e;
            }
        }
    }
}

fn handle_conn(mut stream: TcpStream, ops: Arc<OpsHandle>, auth: Arc<Auth>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let Some((head, mut body)) = read_head(&mut stream) else {
        return;
    };
    let want = content_length(&head);
    while body.len() < want && body.len() < http::MAX_BODY {
        use std::io::Read;
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    let Some(req) = http::parse_request(&head, body) else {
        let _ = write_response(&mut stream, &http::Response::text(400, "bad request"));
        return;
    };
    let response = http::route(&req, ops.get() as &dyn OpsService, &auth);
    let _ = write_response(&mut stream, &response);
}

/// Read through the end of the header block (`\r\n\r\n`), returning the head
/// and whatever body bytes arrived with it.
fn read_head(stream: &mut TcpStream) -> Option<(Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = find(&buf, b"\r\n\r\n") {
                    let split = pos + 4;
                    let body = buf.split_off(split);
                    return Some((buf, body));
                }
                if buf.len() > http::MAX_HEAD {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

fn write_response(stream: &mut TcpStream, response: &http::Response) -> std::io::Result<()> {
    use std::io::Write;
    stream.write_all(&response.encode())?;
    stream.flush()
}

/// The `Content-Length` value from a raw head, or zero.
fn content_length(head: &[u8]) -> usize {
    let text = String::from_utf8_lossy(head);
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                return v.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Find `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_service_and_command() {
        let m = registration().manifest;
        assert_eq!(m.id, "webadmin");
        assert_eq!(m.tier, PluginTier::Native);
        assert_eq!(m.requires, vec!["ops@0.1".to_string()]);
        assert!(m.serves_command("webadmin"));
        assert!(!m.serves_command("give"));
    }

    #[test]
    fn finds_the_header_terminator() {
        assert_eq!(find(b"abc\r\n\r\ndef", b"\r\n\r\n"), Some(3));
        assert_eq!(find(b"abc", b"\r\n\r\n"), None);
    }

    #[test]
    fn reads_content_length_case_insensitively() {
        let head = b"POST /login HTTP/1.1\r\ncontent-length: 7\r\n\r\n";
        assert_eq!(content_length(head), 7);
        assert_eq!(content_length(b"GET / HTTP/1.1\r\n\r\n"), 0);
    }
}
