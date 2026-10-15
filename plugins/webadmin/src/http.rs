//! The embedded HTTP server, the live-ops pages and the map page.
//!
//! No framework, no external asset and no CDN: one blocking handler over
//! `std::net`, serving plain server-rendered HTML (with a little inline JS) and
//! JSON. The request handler is a pure function [`WebAdmin::handle`], so the
//! routes are tested without opening a socket; [`WebAdmin::serve`] only moves
//! bytes.
//!
//! It is the single HTTP router for the whole admin plugin: WEB2's map and
//! time machine (map tiles, journal replay, per-block and per-player history)
//! and WEB1's live ops (dashboard, heaviest regions, players, profiles,
//! inventories), behind one token login.
//!
//! Everything is read-only. There is no route that writes a block or a file.
//! Auth is a static `[plugins.webadmin] token` or a one-time `/webadmin token`,
//! carried as a cookie or `?token=`; the server binds `127.0.0.1` by default.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aether_ops::{OpsHandle, OpsService};
use aether_world::journal::ActorId;

use crate::auth::{self, Auth};
use crate::map::{MapRenderer, TileKey, MAX_ZOOM};
use crate::pages;
use crate::playerhist::{self, HistoryQuery};
use crate::source::{block_name_of, HistorySource, MapSource};
use crate::timemachine::{AtSeq, TimeMachine};

/// The most body bytes read (a login form is tiny; a hostile client is not).
pub const MAX_BODY: usize = 64 * 1024;

/// A point of interest drawn over the map (a player or an entity).
#[derive(Debug, Clone)]
pub struct Mark {
    pub kind: &'static str,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// The live overlay: who is where. Empty by default.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    pub players: Vec<Mark>,
    pub entities: Vec<Mark>,
}

/// A source of overlay marks. The server (WEB1) implements this when live
/// player/entity positions are available; the default is empty.
pub trait OverlaySource: Send + Sync {
    fn overlay(&self) -> Overlay;
}

/// An overlay that is always empty.
pub struct EmptyOverlay;

impl OverlaySource for EmptyOverlay {
    fn overlay(&self) -> Overlay {
        Overlay::default()
    }
}

/// A dynamic player-name resolver, so a UUID in the history reads as a name
/// when the server knows one. Optional; UUIDs are shown as-is otherwise.
pub trait NameResolver: Send + Sync {
    fn name(&self, actor: ActorId) -> Option<String>;
}

/// Operator configuration for the web server.
#[derive(Debug, Clone)]
pub struct WebAdminConfig {
    /// Tile edge in pixels, shared with the page.
    pub tile_px: u32,
    /// Vertical band the map scans.
    pub y_min: i32,
    pub y_max: i32,
    /// When set, every request must carry `?token=` (or `X-Token`).
    pub token: Option<String>,
    /// How many history rows a page request returns at most.
    pub history_limit: usize,
}

impl Default for WebAdminConfig {
    fn default() -> Self {
        Self {
            tile_px: crate::map::DEFAULT_TILE_PX,
            y_min: -64,
            y_max: 319,
            token: None,
            history_limit: 500,
        }
    }
}

/// A parsed HTTP request.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    /// A token from `X-Token` or `?token=`, filled by the socket reader.
    pub token: Option<String>,
    /// The raw `Cookie:` header value, if any.
    pub cookie: Option<String>,
    /// The request body, for `POST /login` (a small form).
    pub body: String,
}

impl Request {
    fn get(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(String::as_str)
    }
    fn i32(&self, key: &str) -> Option<i32> {
        self.get(key).and_then(|v| v.parse().ok())
    }
    fn u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.parse().ok())
    }

    /// The token to authenticate with: the explicit one, else a cookie, else
    /// the query string.
    fn auth_token(&self) -> Option<String> {
        if let Some(t) = &self.token {
            return Some(t.clone());
        }
        if let Some(cookie) = &self.cookie {
            if let Some(t) = auth::token_from_cookie(cookie) {
                return Some(t);
            }
        }
        self.get("token").map(str::to_string)
    }

    /// The token from a login form body or the query string.
    fn login_token(&self) -> Option<String> {
        for pair in self.body.split('&') {
            if let Some(v) = pair.strip_prefix("token=") {
                return Some(percent_decode(&v.replace('+', " ")));
            }
        }
        self.get("token").map(str::to_string)
    }
}

/// An HTTP response. The body is bytes so a tile can be raw RGBA.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body,
        }
    }
    fn html(body: String) -> Self {
        Self::new(200, "text/html; charset=utf-8", body.into_bytes())
    }
    fn js(body: &str) -> Self {
        Self::new(
            200,
            "text/javascript; charset=utf-8",
            body.as_bytes().to_vec(),
        )
    }
    fn json(body: String) -> Self {
        Self::new(200, "application/json; charset=utf-8", body.into_bytes())
    }
    fn text(status: u16, body: String) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body.into_bytes())
    }
}

/// The read-only admin web app over one world.
pub struct WebAdmin {
    history: Arc<dyn HistorySource>,
    map: Arc<MapRenderer>,
    time: TimeMachine,
    overlay: Arc<dyn OverlaySource>,
    names: Option<Arc<dyn NameResolver>>,
    config: WebAdminConfig,
    /// The live-ops counters (dashboard, regions, players, inventories), when
    /// the server attached them; `None` disables those pages.
    ops: Option<OpsHandle>,
    /// Token auth, shared with the plugin so `/webadmin token` can mint one.
    auth: Arc<Auth>,
}

impl WebAdmin {
    /// Build the app from a world's map and history views.
    ///
    /// For an `aether_api::World<_, _>` both views are the same `Arc`, coerced
    /// to each trait: `WebAdmin::new(world.clone(), world)`.
    pub fn new(source: Arc<dyn MapSource>, history: Arc<dyn HistorySource>) -> Self {
        let config = WebAdminConfig {
            y_min: source.y_min(),
            y_max: source.y_max(),
            ..WebAdminConfig::default()
        };
        Self::with_config(source, history, config)
    }

    /// Build with explicit operator configuration.
    pub fn with_config(
        source: Arc<dyn MapSource>,
        history: Arc<dyn HistorySource>,
        config: WebAdminConfig,
    ) -> Self {
        let map = Arc::new(
            MapRenderer::new(Arc::clone(&source))
                .with_tile_px(config.tile_px)
                .with_height_range(config.y_min, config.y_max),
        );
        let time = TimeMachine::new(Arc::clone(&history), Arc::clone(&source))
            .with_height_range(config.y_min, config.y_max);
        let auth = Arc::new(Auth::new(config.token.clone()));
        Self {
            history,
            map,
            time,
            overlay: Arc::new(EmptyOverlay),
            names: None,
            config,
            ops: None,
            auth,
        }
    }

    /// Attach the live-ops service, enabling the dashboard/regions/players
    /// pages. Without it those routes answer `503`.
    pub fn with_ops(mut self, ops: OpsHandle) -> Self {
        self.ops = Some(ops);
        self
    }

    /// The shared token state, so a plugin can mint one-time tokens.
    pub fn auth(&self) -> &Arc<Auth> {
        &self.auth
    }

    /// The live-ops service, if attached.
    fn ops(&self) -> Option<&dyn OpsService> {
        self.ops.as_ref().map(|o| o.get())
    }

    /// Attach a live overlay source.
    pub fn with_overlay(mut self, overlay: Arc<dyn OverlaySource>) -> Self {
        self.overlay = overlay;
        self
    }

    /// Attach a player-name resolver.
    pub fn with_names(mut self, names: Arc<dyn NameResolver>) -> Self {
        self.names = Some(names);
        self
    }

    /// The tile renderer, for the plugin to invalidate on block changes.
    pub fn map(&self) -> &MapRenderer {
        &self.map
    }

    /// A shared handle to the tile renderer, so a plugin can invalidate it
    /// after the app has been moved into [`WebAdmin::serve`].
    pub fn map_handle(&self) -> Arc<MapRenderer> {
        Arc::clone(&self.map)
    }

    /// The time machine.
    pub fn time(&self) -> &TimeMachine {
        &self.time
    }

    /// Route a request to a response. Pure: no sockets, no globals.
    pub fn handle(&self, req: &Request) -> Response {
        // Login is the one unauthenticated route (it sets the cookie).
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/login") => return Response::html(pages::login("")),
            ("POST", "/login") => return self.login(req),
            _ => {}
        }

        if !self.authenticated(req) {
            // A browser asking for a page gets the login form; anything the
            // page's own JS fetches (JSON, tiles) gets a plain 401.
            let wants_html = matches!(
                req.path.as_str(),
                "/" | "/regions" | "/players" | "/profile" | "/inventory" | "/map"
            );
            return match (req.method.as_str(), wants_html) {
                ("GET", true) => Response::html(pages::login("")),
                _ => Response::text(401, "unauthorized\n".to_string()),
            };
        }

        match (req.method.as_str(), req.path.as_str()) {
            // Live ops (WEB1): the dashboard is the home page; the map (WEB2)
            // lives at `/map`.
            ("GET", "/") => self.ops_page(|o| Some(pages::dashboard(o))),
            ("GET", "/regions") => self.ops_page(|o| Some(pages::regions(o))),
            ("GET", "/players") => self.ops_page(|o| Some(pages::players(o))),
            ("GET", "/profile") => match req.get("id") {
                Some(id) => self.ops_page(move |o| o.profile(id).map(|p| pages::profile(&p))),
                None => Response::text(400, "id required\n".to_string()),
            },
            ("GET", "/inventory") => match req.get("id") {
                Some(id) => self.ops_page(move |o| o.inventory(id).map(|v| pages::inventory(&v))),
                None => Response::text(400, "id required\n".to_string()),
            },
            ("GET", "/api/dashboard") => {
                self.ops_json(|o| serde_json::to_value(o.dashboard()).ok())
            }
            ("GET", "/api/regions") => self.ops_json(|o| serde_json::to_value(o.regions(100)).ok()),
            ("GET", "/api/players") => self.ops_json(|o| serde_json::to_value(o.players()).ok()),
            ("GET", "/api/player") => match req.get("id") {
                Some(id) => {
                    self.ops_json(move |o| o.profile(id).and_then(|p| serde_json::to_value(p).ok()))
                }
                None => Response::text(400, "id required\n".to_string()),
            },
            ("GET", "/api/inventory") => match req.get("id") {
                Some(id) => self
                    .ops_json(move |o| o.inventory(id).and_then(|v| serde_json::to_value(v).ok())),
                None => Response::text(400, "id required\n".to_string()),
            },

            // Map and time machine (WEB2).
            ("GET", "/map") => Response::html(INDEX_HTML.to_string()),
            ("GET", "/app.js") => Response::js(APP_JS),
            ("GET", "/info") => self.info(),
            ("GET", "/head") => Response::json(format!("{{\"head\":{}}}", self.history.head())),
            ("GET", "/tile") => self.tile(req),
            ("GET", "/tile_at") => self.tile_at(req),
            ("GET", "/overlay") => self.overlay(),
            ("GET", "/surface") => self.surface(req),
            ("GET", "/history") => self.history(req),
            ("GET", "/changes") => self.changes(req),
            ("GET", "/player") => self.player(req),
            ("GET", _) if req.path.starts_with("/tile") => {
                Response::text(400, "bad tile: need zoom, tx, tz\n".to_string())
            }
            _ => Response::text(404, "not found\n".to_string()),
        }
    }

    /// Whether `req` carries a valid token.
    fn authenticated(&self, req: &Request) -> bool {
        req.auth_token()
            .map(|t| self.auth.check(&t))
            .unwrap_or(false)
    }

    /// Handle `POST /login`: verify the form token, set the cookie, redirect.
    fn login(&self, req: &Request) -> Response {
        let provided = req.login_token().unwrap_or_default();
        if self.auth.check(&provided) {
            let mut r = Response::text(303, String::new());
            r.headers.push((
                "Set-Cookie",
                format!(
                    "{}={provided}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                    auth::COOKIE
                ),
            ));
            r.headers.push(("Location", "/".to_string()));
            r
        } else {
            Response::html(pages::login("wrong token"))
        }
    }

    /// Render a live-ops page, or `503` when no ops service is attached. `f`
    /// returns `None` for an unknown id.
    fn ops_page(&self, f: impl FnOnce(&dyn OpsService) -> Option<String>) -> Response {
        match self.ops() {
            Some(ops) => match f(ops) {
                Some(html) => Response::html(html),
                None => Response::text(404, "not found\n".to_string()),
            },
            None => Response::text(503, "live-ops service not attached\n".to_string()),
        }
    }

    /// Render a live-ops JSON value, or `503`/`404`.
    fn ops_json(&self, f: impl FnOnce(&dyn OpsService) -> Option<serde_json::Value>) -> Response {
        match self.ops() {
            Some(ops) => match f(ops) {
                Some(v) => Response::json(v.to_string()),
                None => Response::text(404, "not found\n".to_string()),
            },
            None => Response::text(503, "live-ops service not attached\n".to_string()),
        }
    }

    fn info(&self) -> Response {
        let head = self.history.head();
        Response::json(format!(
            "{{\"tile_px\":{},\"max_zoom\":{},\"y_min\":{},\"y_max\":{},\"head\":{}}}",
            self.config.tile_px, MAX_ZOOM, self.config.y_min, self.config.y_max, head
        ))
    }

    fn tile_key(req: &Request) -> Option<TileKey> {
        let zoom = req.u64("zoom")? as u8;
        let tx = req.i32("tx")?;
        let tz = req.i32("tz")?;
        Some(TileKey::new(zoom, tx, tz))
    }

    fn tile(&self, req: &Request) -> Response {
        let Some(key) = Self::tile_key(req) else {
            return Response::text(400, "need zoom, tx, tz\n".to_string());
        };
        let t = self.map.render(key);
        Self::raw_tile(t.px, &t.rgba)
    }

    fn tile_at(&self, req: &Request) -> Response {
        let Some(key) = Self::tile_key(req) else {
            return Response::text(400, "need zoom, tx, tz\n".to_string());
        };
        let at = AtSeq(req.u64("at").unwrap_or(u64::MAX));
        match self.time.tile_at(key, at, self.config.tile_px) {
            Ok(t) => Self::raw_tile(t.px, &t.rgba),
            Err(e) => Response::text(500, format!("{e}\n")),
        }
    }

    fn raw_tile(px: u32, rgba: &[u8]) -> Response {
        let mut r = Response::new(200, "application/octet-stream", rgba.to_vec());
        r.headers.push(("X-Tile-Px", px.to_string()));
        r
    }

    fn overlay(&self) -> Response {
        let o = self.overlay.overlay();
        let mut s = String::from("{\"players\":[");
        for (i, m) in o.players.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            push_mark(&mut s, m);
        }
        s.push_str("],\"entities\":[");
        for (i, m) in o.entities.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            push_mark(&mut s, m);
        }
        s.push_str("]}");
        Response::json(s)
    }

    fn surface(&self, req: &Request) -> Response {
        let (Some(x), Some(z)) = (req.i32("x"), req.i32("z")) else {
            return Response::text(400, "need x, z\n".to_string());
        };
        let at = AtSeq(req.u64("at").unwrap_or(u64::MAX));
        match self.time.surface_at(x, z, at) {
            Ok(Some(s)) => Response::json(format!(
                "{{\"x\":{x},\"z\":{z},\"y\":{},\"id\":{},\"name\":\"{}\",\"water\":{}}}",
                s.y,
                s.id.raw(),
                json_escape(&self.name_of(s.id)),
                s.is_water
            )),
            Ok(None) => Response::json(format!("{{\"x\":{x},\"z\":{z},\"y\":null}}")),
            Err(e) => Response::text(500, format!("{e}\n")),
        }
    }

    fn history(&self, req: &Request) -> Response {
        let (Some(x), Some(y), Some(z)) = (req.i32("x"), req.i32("y"), req.i32("z")) else {
            return Response::text(400, "need x, y, z\n".to_string());
        };
        match self.time.block_history(x, y, z) {
            Ok(h) => {
                let mut s = String::from("[");
                for (i, c) in h.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!(
                        "{{\"seq\":{},\"at_ms\":{},\"actor\":\"{}\",\"from\":{},\"from_name\":\"{}\",\"to\":{},\"to_name\":\"{}\"}}",
                        c.seq,
                        c.at_ms,
                        json_escape(&self.actor_label(c.actor)),
                        c.from.raw(),
                        json_escape(&self.name_of(c.from)),
                        c.to.raw(),
                        json_escape(&self.name_of(c.to)),
                    ));
                }
                s.push(']');
                Response::json(s)
            }
            Err(e) => Response::text(500, format!("{e}\n")),
        }
    }

    fn changes(&self, req: &Request) -> Response {
        let (Some(x), Some(z)) = (req.i32("x"), req.i32("z")) else {
            return Response::text(400, "need x, z\n".to_string());
        };
        let r = req.u64("r").unwrap_or(16) as u32;
        let since = req.u64("since").unwrap_or(0);
        let until = req.u64("until").unwrap_or(u64::MAX);
        match self
            .time
            .changes_in_area(x, z, r, since, until, self.config.history_limit)
        {
            Ok(cs) => {
                let mut s = String::from("[");
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!(
                        "{{\"seq\":{},\"at_ms\":{},\"actor\":\"{}\",\"x\":{},\"y\":{},\"z\":{},\"from_name\":\"{}\",\"to_name\":\"{}\"}}",
                        c.seq,
                        c.at_ms,
                        json_escape(&self.actor_label(c.actor)),
                        c.x,
                        c.y,
                        c.z,
                        json_escape(&self.name_of(c.from)),
                        json_escape(&self.name_of(c.to)),
                    ));
                }
                s.push(']');
                Response::json(s)
            }
            Err(e) => Response::text(500, format!("{e}\n")),
        }
    }

    fn player(&self, req: &Request) -> Response {
        let Some(actor) = req.get("id").and_then(parse_actor) else {
            return Response::text(400, "need id=<uuid>\n".to_string());
        };
        let mut q = HistoryQuery::default().by(actor);
        if let Some(since) = req.u64("since") {
            q = q.between(since, req.u64("until").unwrap_or(0));
        }
        if let (Some(x), Some(z)) = (req.i32("x"), req.i32("z")) {
            q = q.near(x, z, req.u64("r").unwrap_or(48) as u32);
        }
        q = q.limit(req.u64("limit").unwrap_or(self.config.history_limit as u64) as usize);
        match playerhist::query(self.history.as_ref(), &q) {
            Ok(events) => {
                let mut s = String::from("[");
                for (i, e) in events.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    let sum = playerhist::summarize(e);
                    let pos = match sum.position {
                        Some((x, y, z)) => format!("[{x},{y},{z}]"),
                        None => "null".to_string(),
                    };
                    s.push_str(&format!(
                        "{{\"seq\":{},\"at_ms\":{},\"actor\":\"{}\",\"kind\":\"{}\",\"action\":\"{}\",\"pos\":{},\"count\":{}}}",
                        sum.seq,
                        sum.at_ms,
                        json_escape(&self.actor_label(sum.actor)),
                        sum.kind,
                        sum.action,
                        pos,
                        sum.count,
                    ));
                }
                s.push(']');
                Response::json(s)
            }
            Err(e) => Response::text(500, format!("{e}\n")),
        }
    }

    fn name_of(&self, id: aether_world::BlockStateId) -> String {
        block_name_of(id)
    }

    /// The player name for an actor, when a resolver is attached.
    fn actor_name(&self, actor: ActorId) -> Option<String> {
        self.names.as_ref().and_then(|n| n.name(actor))
    }

    /// A short label for an actor: the resolved name, else the UUID hex.
    fn actor_label(&self, actor: ActorId) -> String {
        self.actor_name(actor).unwrap_or_else(|| actor_hex(actor))
    }

    /// Run the server on `bind` until the returned [`Server`] is dropped.
    pub fn serve(self, bind: SocketAddr) -> io::Result<Server> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let admin = Arc::new(self);
        let stop_thread = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("webadmin".to_string())
            .spawn(move || {
                while !stop_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let a = Arc::clone(&admin);
                            let _ = thread::Builder::new()
                                .name("webadmin-conn".to_string())
                                .spawn(move || {
                                    let _ = a.serve_conn(stream);
                                });
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Server {
            addr,
            stop,
            handle: Some(handle),
        })
    }

    fn serve_conn(&self, stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let Some(mut req) = parse_request(&line) else {
            return write_response(stream, &Response::text(400, "bad request\n".to_string()));
        };
        // Drain headers: a token may arrive as `X-Token`, and a login cookie
        // as `Cookie`. `Content-Length` bounds the body.
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 {
                break;
            }
            let t = h.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                let k = k.trim();
                let v = v.trim();
                if k.eq_ignore_ascii_case("x-token") {
                    req.token = Some(v.to_string());
                } else if k.eq_ignore_ascii_case("cookie") {
                    req.cookie = Some(v.to_string());
                } else if k.eq_ignore_ascii_case("content-length") {
                    length = v.parse().unwrap_or(0);
                }
            }
        }
        // A form body, bounded so a hostile client cannot make us allocate.
        if length > 0 {
            let take = length.min(MAX_BODY);
            let mut buf = vec![0u8; take];
            reader.read_exact(&mut buf)?;
            req.body = String::from_utf8_lossy(&buf).into_owned();
        }
        let res = self.handle(&req);
        write_response(stream, &res)
    }
}

/// A running server; dropping it stops the accept loop and joins the thread.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    /// The bound address (useful with port 0 in tests).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The base URL, e.g. `http://127.0.0.1:8090`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop the server, waiting for the accept loop to exit.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop on the next poll rather than after the sleep.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse_request(line: &str) -> Option<Request> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.to_string(), BTreeMap::new()),
    };
    Some(Request {
        method,
        path,
        query,
        ..Request::default()
    })
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hi = (b[i + 1] as char).to_digit(16);
                let lo = (b[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn write_response(mut stream: TcpStream, res: &Response) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        res.status,
        status_text(res.status),
        res.content_type,
        res.body.len()
    );
    for (k, v) in &res.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&res.body)?;
    stream.flush()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn push_mark(s: &mut String, m: &Mark) {
    s.push_str(&format!(
        "{{\"kind\":\"{}\",\"name\":\"{}\",\"x\":{},\"y\":{},\"z\":{}}}",
        m.kind,
        json_escape(&m.name),
        m.x,
        m.y,
        m.z
    ));
}

/// A player UUID as a stable hex string, for JSON and log lines.
pub fn actor_hex(actor: ActorId) -> String {
    format!("{:032x}", actor.0)
}

/// Parse a UUID as hex (`0x…`, dashed, or bare) or decimal.
pub fn parse_actor(s: &str) -> Option<ActorId> {
    let cleaned: String = s.chars().filter(|c| *c != '-').collect();
    let cleaned = cleaned.as_str();
    let hex = cleaned.strip_prefix("0x").unwrap_or(cleaned);
    if hex.len() == 32 {
        return u128::from_str_radix(hex, 16).ok().map(ActorId);
    }
    if let Ok(v) = hex.parse::<u128>() {
        return Some(ActorId(v));
    }
    None
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Aether map</title>
<style>
  html,body{margin:0;height:100%;background:#0b0d10;color:#ddd;font:13px/1.4 system-ui,sans-serif}
  #map{position:absolute;inset:0;image-rendering:pixelated;cursor:crosshair}
  .panel{position:absolute;background:#14181dee;border:1px solid #2a3138;border-radius:6px;padding:10px}
  #hud{top:10px;left:10px}
  #side{top:10px;right:10px;width:320px;max-height:calc(100% - 90px);overflow:auto}
  #time{bottom:12px;left:50%;transform:translateX(-50%);width:min(760px,90%)}
  button{background:#232a31;color:#ddd;border:1px solid #38414a;border-radius:4px;padding:4px 8px;cursor:pointer}
  button:hover{background:#2c343c}
  table{border-collapse:collapse;width:100%}
  td,th{border-bottom:1px solid #232a31;padding:2px 4px;text-align:left;font-size:12px}
  input[type=range]{width:100%}
  .mono{font-family:ui-monospace,monospace}
  .muted{color:#8a949e}
</style>
</head>
<body>
<canvas id="map"></canvas>
<div id="hud" class="panel">
  <b>Aether map</b>
  <a href="/" style="margin-left:10px">dashboard</a>
  <a href="/regions">regions</a>
  <a href="/players">players</a><br>
  <span id="coords" class="mono muted">-</span><br>
  <span class="muted">drag to pan · wheel to zoom · click a block for its history</span>
</div>
<div id="side" class="panel">
  <div id="hist"><span class="muted">No block selected.</span></div>
</div>
<div id="time" class="panel">
  <div style="display:flex;gap:8px;align-items:center">
    <button id="live">Live</button>
    <input id="slider" type="range" min="0" max="0" value="0">
    <span id="when" class="mono muted">now</span>
  </div>
</div>
<script src="/app.js"></script>
</body>
</html>
"#;

const APP_JS: &str = r#"(() => {
  const canvas = document.getElementById('map');
  const ctx = canvas.getContext('2d');
  const coords = document.getElementById('coords');
  const hist = document.getElementById('hist');
  const slider = document.getElementById('slider');
  const when = document.getElementById('when');
  const liveBtn = document.getElementById('live');

  let cfg = { tile_px: 128, max_zoom: 6, head: 0 };
  // view: world block at the top-left corner, and pixels per block.
  let view = { x: -256, z: -256, ppb: 1 };
  let at = null;               // null = live; otherwise a sequence number
  let overlay = { players: [], entities: [] };
  const cache = new Map();

  function resize() {
    canvas.width = window.innerWidth;
    canvas.height = window.innerHeight;
    draw();
  }

  async function loadInfo() {
    cfg = await fetch('/info').then(r => r.json());
    slider.max = String(cfg.head);
    slider.value = String(cfg.head);
    view.ppb = 1;
    draw();
    refreshOverlay();
    setInterval(refreshOverlay, 3000);
  }

  async function refreshOverlay() {
    try { overlay = await fetch('/overlay').then(r => r.json()); } catch (e) {}
    draw();
  }

  function zoom() { return Math.max(0, Math.min(cfg.max_zoom, Math.round(Math.log2(view.ppb)))); }

  function tileFor(px, py) {
    const z = zoom();
    const size = cfg.tile_px * Math.pow(2, z);
    const tx = Math.floor(px / size), tz = Math.floor(py / size);
    return { z, tx, tz, size };
  }

  function urlFor(z, tx, tz) {
    const base = at === null ? '/tile' : '/tile_at';
    const tail = at === null ? '' : `&at=${at}`;
    return `${base}?zoom=${z}&tx=${tx}&tz=${tz}${tail}`;
  }

  async function tileBitmap(z, tx, tz) {
    const key = `${z}/${tx}/${tz}` + (at === null ? '' : `@${at}`);
    const hit = cache.get(key);
    if (hit && hit.t !== at) return hit.img;
    const buf = await fetch(urlFor(z, tx, tz)).then(r => r.arrayBuffer());
    const rgba = new Uint8ClampedArray(buf);
    const img = new ImageData(rgba, cfg.tile_px, cfg.tile_px);
    cache.set(key, { img, t: at });
    if (cache.size > 256) cache.clear();
    return img;
  }

  async function draw() {
    ctx.fillStyle = '#0b0d10';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    const z = zoom();
    // world -> screen: sx = (wx - view.x) * ppb
    const size = cfg.tile_px * Math.pow(2, z);
    const tx0 = Math.floor(view.x / size), tz0 = Math.floor(view.z / size);
    const tx1 = Math.floor((view.x + canvas.width / view.ppb) / size);
    const tz1 = Math.floor((view.z + canvas.height / view.ppb) / size);
    const jobs = [];
    for (let tz = tz0; tz <= tz1; tz++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        const sx = (tx * size - view.x) * view.ppb;
        const sy = (tz * size - view.z) * view.ppb;
        const sw = size * view.ppb;
        jobs.push(tileBitmap(z, tx, tz).then(img => ctx.putImageData(img, Math.round(sx), Math.round(sy))));
      }
    }
    await Promise.all(jobs);
    drawOverlay();
    coords.textContent = `x=${Math.round(view.x)} z=${Math.round(view.z)} zoom=${z}`;
  }

  function toScreen(wx, wz) {
    return [(wx - view.x) * view.ppb, (wz - view.z) * view.ppb];
  }

  function drawOverlay() {
    for (const p of overlay.players) {
      const [sx, sy] = toScreen(p.x, p.z);
      ctx.fillStyle = p.kind === 'entity' ? '#e0a020' : '#38d66a';
      ctx.beginPath(); ctx.arc(sx, sy, 4, 0, Math.PI * 2); ctx.fill();
      ctx.fillStyle = '#fff'; ctx.fillText(p.name, sx + 6, sy - 6);
    }
    for (const e of overlay.entities) {
      const [sx, sy] = toScreen(e.x, e.z);
      ctx.fillStyle = '#e0a020';
      ctx.fillRect(sx - 2, sy - 2, 4, 4);
    }
  }

  function blockAt(ev) {
    const r = canvas.getBoundingClientRect();
    const wx = Math.floor(view.x + (ev.clientX - r.left) / view.ppb);
    const wz = Math.floor(view.z + (ev.clientY - r.top) / view.ppb);
    return [wx, wz];
  }

  async function showHistory(wx, wz) {
    const atq = at === null ? '' : `&at=${at}`;
    const surf = await fetch(`/surface?x=${wx}&z=${wz}${atq}`).then(r => r.json());
    if (surf.y === null) { hist.innerHTML = '<span class="muted">No block there.</span>'; return; }
    const rows = await fetch(`/history?x=${wx}&y=${surf.y}&z=${wz}`).then(r => r.json());
    let html = `<b>Block (${wx}, ${surf.y}, ${wz})</b><br><span class="mono muted">${surf.name}</span>`;
    html += `<table><tr><th>seq</th><th>when</th><th>who</th><th>from → to</th></tr>`;
    if (rows.length === 0) html += `<tr><td colspan=4 class="muted">never edited (worldgen)</td></tr>`;
    for (const r of rows) {
      const w = new Date(r.at_ms).toLocaleString();
      const who = r.actor.slice(0, 8);
      html += `<tr><td class="mono">${r.seq}</td><td>${w}</td><td class="mono" title="${r.actor}">${who}</td><td>${r.from_name} → ${r.to_name}</td></tr>`;
    }
    html += `</table>`;
    hist.innerHTML = html;
  }

  // --- interaction ---------------------------------------------------------
  let drag = null;
  canvas.addEventListener('mousedown', e => { drag = { x: e.clientX, y: e.clientY, ox: view.x, oz: view.z, moved: false }; });
  canvas.addEventListener('mousemove', e => {
    if (!drag) return;
    const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
    view.x = drag.ox - dx / view.ppb;
    view.z = drag.oz - dy / view.ppb;
    draw();
  });
  window.addEventListener('mouseup', async e => {
    if (!drag) return;
    const moved = drag.moved; drag = null;
    if (!moved) { const [wx, wz] = blockAt(e); await showHistory(wx, wz); }
  });
  canvas.addEventListener('wheel', e => {
    e.preventDefault();
    const r = canvas.getBoundingClientRect();
    const mx = e.clientX - r.left, my = e.clientY - r.top;
    const wx = view.x + mx / view.ppb, wz = view.z + my / view.ppb;
    const f = e.deltaY < 0 ? 2 : 0.5;
    const next = Math.max(0.5, Math.min(Math.pow(2, cfg.max_zoom), view.ppb * f));
    view.ppb = Math.max(0.25, Math.min(Math.pow(2, cfg.max_zoom), next));
    // keep the point under the cursor fixed
    view.x = wx - mx / view.ppb;
    view.z = wz - my / view.ppb;
    draw();
  }, { passive: false });

  slider.addEventListener('input', () => {
    const v = Number(slider.value);
    at = v >= cfg.head ? null : v;
    when.textContent = at === null ? 'now' : `seq ${v}`;
    cache.clear();
    draw();
  });
  liveBtn.addEventListener('click', () => {
    at = null; slider.value = String(cfg.head); when.textContent = 'now'; cache.clear(); draw();
  });

  window.addEventListener('resize', resize);
  resize();
  loadInfo();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::World;
    use aether_world::journal::ActorId;
    use aether_world::registry::ids;
    use aether_world::MemStore;
    use aether_worldgen::FlatGenerator;

    const ALICE: ActorId = ActorId(0xA11CE);

    fn admin() -> WebAdmin {
        let w = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
        let props = w.props_of(ids::STONE);
        w.set_block_by(ALICE, 0, 3, 0, ids::STONE, props);
        let cfg = WebAdminConfig {
            tile_px: 4,
            y_min: 0,
            y_max: 8,
            token: Some("test".to_string()),
            history_limit: 100,
        };
        WebAdmin::with_config(w.clone(), w, cfg)
    }

    fn request(path: &str, query: &[(&str, &str)], token: Option<&str>) -> Request {
        let mut q = BTreeMap::new();
        for (k, v) in query {
            q.insert(k.to_string(), v.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: path.to_string(),
            query: q,
            token: token.map(str::to_string),
            ..Request::default()
        }
    }

    fn get(admin: &WebAdmin, path: &str, query: &[(&str, &str)]) -> Response {
        admin.handle(&request(path, query, Some("test")))
    }

    #[test]
    fn index_and_js_are_served() {
        let a = admin();
        // The map page and its script; `/` is the dashboard (no ops here).
        assert_eq!(get(&a, "/map", &[]).status, 200);
        assert_eq!(get(&a, "/app.js", &[]).status, 200);
        assert_eq!(get(&a, "/nope", &[]).status, 404);
        assert_eq!(
            get(&a, "/", &[]).status,
            503,
            "no live-ops service attached"
        );
    }

    #[test]
    fn tile_route_returns_raw_rgba_with_size_header() {
        let a = admin();
        let r = get(&a, "/tile", &[("zoom", "0"), ("tx", "0"), ("tz", "0")]);
        assert_eq!(r.status, 200);
        assert_eq!(r.body.len(), 4 * 4 * 4);
        assert!(r.headers.iter().any(|(k, v)| *k == "X-Tile-Px" && v == "4"));
    }

    #[test]
    fn tile_at_route_replays() {
        let a = admin();
        let live = get(&a, "/tile", &[("zoom", "0"), ("tx", "0"), ("tz", "0")]);
        let past = get(
            &a,
            "/tile_at",
            &[("zoom", "0"), ("tx", "0"), ("tz", "0"), ("at", "0")],
        );
        assert_ne!(live.body, past.body, "time machine changes the pixels");
    }

    #[test]
    fn history_route_reports_the_edit() {
        let a = admin();
        let r = get(&a, "/history", &[("x", "0"), ("y", "3"), ("z", "0")]);
        let body = String::from_utf8(r.body).unwrap();
        assert!(
            body.contains("block_set") || body.contains("\"seq\""),
            "{body}"
        );
        assert!(body.contains("alice") || body.contains("a11ce"), "{body}");
    }

    #[test]
    fn surface_route_answers_top_block() {
        let a = admin();
        let r = get(&a, "/surface", &[("x", "5"), ("z", "5")]);
        let body = String::from_utf8(r.body).unwrap();
        assert!(body.contains("grass_block"), "{body}");
    }

    #[test]
    fn token_is_required_when_configured() {
        let w = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
        let cfg = WebAdminConfig {
            token: Some("secret".to_string()),
            ..WebAdminConfig::default()
        };
        let a = WebAdmin::with_config(w.clone(), w, cfg);
        assert_eq!(a.handle(&request("/info", &[], None)).status, 401);
        assert_eq!(a.handle(&request("/info", &[], Some("wrong"))).status, 401);
        assert_eq!(a.handle(&request("/info", &[], Some("secret"))).status, 200);
    }

    #[test]
    fn login_page_is_open_and_sets_a_cookie_on_success() {
        let a = admin();
        // The login page needs no token.
        let page = a.handle(&request("/login", &[], None));
        assert_eq!(page.status, 200);
        // A wrong token re-renders the form without a cookie.
        let mut post = request("/login", &[], None);
        post.method = "POST".to_string();
        post.body = "token=wrong".to_string();
        let bad = a.handle(&post);
        assert_eq!(bad.status, 200);
        assert!(!bad.headers.iter().any(|(k, _)| *k == "Set-Cookie"));
        // The right token redirects and sets the cookie.
        post.body = "token=test".to_string();
        let ok = a.handle(&post);
        assert_eq!(ok.status, 303);
        assert!(ok.headers.iter().any(|(k, _)| *k == "Set-Cookie"));
        assert!(ok.headers.iter().any(|(k, v)| *k == "Location" && v == "/"));
    }

    #[test]
    fn a_cookie_authenticates_a_request() {
        let a = admin();
        let mut req = request("/info", &[], None);
        req.cookie = Some("aether_web=test".to_string());
        assert_eq!(a.handle(&req).status, 200);
    }

    #[test]
    fn actor_hex_roundtrips() {
        let s = actor_hex(ALICE);
        assert_eq!(parse_actor(&s), Some(ALICE));
        assert_eq!(parse_actor("0"), Some(ActorId(0)));
    }

    #[test]
    fn server_answers_over_tcp() {
        let a = admin();
        let mut server = a.serve("127.0.0.1:0".parse().unwrap()).unwrap();
        let body = fetch(server.addr(), "/info?token=test");
        assert!(body.contains("\"tile_px\":4"), "{body}");
        server.stop();
    }

    fn fetch(addr: SocketAddr, path: &str) -> String {
        use std::io::Read;
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }
}
