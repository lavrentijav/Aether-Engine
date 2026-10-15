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
//!
//! # Hardening
//!
//! A local operator console still must not be trivially DoS-able by one client
//! on the loopback (or by anyone if the operator binds wider). The socket layer
//! therefore bounds:
//!
//! * **connections** — a global cap and a per-IP cap ([`ConnGate`]), served by
//!   a fixed [`ThreadPool`], so a flood of sockets cannot spawn unbounded
//!   threads;
//! * **request size** — a maximum request line, a maximum number of headers and
//!   a maximum total header size ([`MAX_REQUEST_LINE`] etc.), read through a
//!   bounded reader;
//! * **request time** — an absolute per-request deadline, so a slowloris that
//!   dribbles one byte per read is still cut off;
//! * **expensive queries** — a token-bucket rate limit ([`RateLimiter`]) over
//!   the tile/history/area endpoints, plus clamped radii and result caps inside
//!   the time machine.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aether_ops::{OpsHandle, OpsService};
use aether_world::journal::ActorId;

use crate::auth::{self, Auth};
use crate::map::{MapRenderer, TileKey, MAX_TILE_COORD, MAX_ZOOM, MIN_TILE_COORD};
use crate::pages;
use crate::playerhist::{self, HistoryQuery};
use crate::source::{block_name_of, HistorySource, MapSource};
use crate::timemachine::{AtSeq, TimeMachine};

/// The most body bytes read (a login form is tiny; a hostile client is not).
pub const MAX_BODY: usize = 64 * 1024;
/// The most bytes in the request line.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;
/// The most header bytes read in total.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// The most headers read.
pub const MAX_HEADERS: usize = 64;

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
    /// When set, every request must carry `?token=` (or `X-Token` or cookie).
    pub token: Option<String>,
    /// How many history rows a page request returns at most.
    pub history_limit: usize,
    /// The most simultaneous connections served.
    pub max_conns: usize,
    /// The most simultaneous connections from one address.
    pub max_per_ip: usize,
    /// The absolute per-request deadline (anti-slowloris).
    pub request_deadline: Duration,
    /// Expensive-endpoint rate: requests per second (`0` disables the limit).
    pub rate_per_sec: f64,
    /// Expensive-endpoint burst allowance.
    pub rate_burst: f64,
}

impl Default for WebAdminConfig {
    fn default() -> Self {
        Self {
            tile_px: crate::map::DEFAULT_TILE_PX,
            y_min: -64,
            y_max: 319,
            token: None,
            history_limit: 500,
            max_conns: 32,
            max_per_ip: 8,
            request_deadline: Duration::from_secs(10),
            rate_per_sec: 50.0,
            rate_burst: 100.0,
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
    fn text(status: u16, body: impl Into<String>) -> Self {
        Self::new(
            status,
            "text/plain; charset=utf-8",
            body.into().into_bytes(),
        )
    }
}

/// A small token-bucket rate limiter. `None` (rate `0`) disables it.
struct RateLimiter {
    rate: f64,
    burst: f64,
    state: Mutex<(f64, Instant)>,
}

impl RateLimiter {
    fn new(rate_per_sec: f64, burst: f64) -> Option<RateLimiter> {
        (rate_per_sec > 0.0 && burst > 0.0).then(|| RateLimiter {
            rate: rate_per_sec,
            burst,
            state: Mutex::new((burst, Instant::now())),
        })
    }

    /// Consume one token, refilling by elapsed time. `false` means "slow down".
    fn allow(&self) -> bool {
        let now = Instant::now();
        let mut s = self.state.lock().unwrap();
        let elapsed = now.saturating_duration_since(s.1).as_secs_f64();
        s.0 = (s.0 + elapsed * self.rate).min(self.burst);
        s.1 = now;
        if s.0 >= 1.0 {
            s.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A bounded connection gate: a global cap and a per-IP cap. A permit releases
/// its slots on drop.
struct ConnGate {
    max_conns: usize,
    max_per_ip: usize,
    active: Mutex<usize>,
    by_ip: Mutex<HashMap<IpAddr, usize>>,
}

impl ConnGate {
    fn new(max_conns: usize, max_per_ip: usize) -> Arc<ConnGate> {
        Arc::new(ConnGate {
            max_conns: max_conns.max(1),
            max_per_ip: max_per_ip.max(1),
            active: Mutex::new(0),
            by_ip: Mutex::new(HashMap::new()),
        })
    }

    /// Admit a connection from `ip`, or `None` when a limit is reached.
    fn try_acquire(self: &Arc<ConnGate>, ip: IpAddr) -> Option<ConnPermit> {
        // Lock order, always: active, then by_ip.
        let mut active = self.active.lock().unwrap();
        if *active >= self.max_conns {
            return None;
        }
        let mut by_ip = self.by_ip.lock().unwrap();
        let count = by_ip.entry(ip).or_insert(0);
        if *count >= self.max_per_ip {
            return None;
        }
        *count += 1;
        *active += 1;
        Some(ConnPermit {
            gate: Arc::clone(self),
            ip,
        })
    }
}

struct ConnPermit {
    gate: Arc<ConnGate>,
    ip: IpAddr,
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        let mut active = self.gate.active.lock().unwrap();
        *active = active.saturating_sub(1);
        let mut by_ip = self.gate.by_ip.lock().unwrap();
        if let Some(c) = by_ip.get_mut(&self.ip) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                by_ip.remove(&self.ip);
            }
        }
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

struct PoolInner {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    cap: usize,
    shutdown: AtomicBool,
}

/// A fixed worker pool with a bounded queue. Reuses threads instead of one per
/// connection, so a burst of sockets cannot grow the thread count without
/// bound.
struct ThreadPool {
    inner: Arc<PoolInner>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl ThreadPool {
    fn new(workers: usize, cap: usize) -> ThreadPool {
        let inner = Arc::new(PoolInner {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            cap: cap.max(1),
            shutdown: AtomicBool::new(false),
        });
        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers.max(1) {
            let inner = Arc::clone(&inner);
            let h = thread::Builder::new()
                .name(format!("webadmin-{i}"))
                .spawn(move || worker(inner))
                .expect("failed to start web admin worker");
            handles.push(h);
        }
        ThreadPool {
            inner,
            workers: Mutex::new(handles),
        }
    }

    /// Stop the workers once the queue drains.
    fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
        if let Ok(mut workers) = self.workers.lock() {
            for h in workers.drain(..) {
                let _ = h.join();
            }
        }
    }
}

/// Whether `inner`'s queue has room for another job.
///
/// Single-producer (the accept loop), so a `true` here cannot become `false`
/// before the matching [`pool_submit`].
fn pool_has_room(inner: &Arc<PoolInner>) -> bool {
    inner.queue.lock().unwrap().len() < inner.cap
}

/// Queue a job on `inner`; callers must have checked [`pool_has_room`].
fn pool_submit(inner: &Arc<PoolInner>, job: Job) {
    let mut q = inner.queue.lock().unwrap();
    q.push_back(job);
    inner.cv.notify_one();
}

fn worker(inner: Arc<PoolInner>) {
    loop {
        let job = {
            let mut q = inner.queue.lock().unwrap();
            loop {
                if let Some(job) = q.pop_front() {
                    break Some(job);
                }
                if inner.shutdown.load(Ordering::SeqCst) {
                    break None;
                }
                q = inner.cv.wait(q).unwrap();
            }
        };
        match job {
            Some(job) => job(),
            None => return,
        }
    }
}

/// The read-only admin web app over one world.
pub struct WebAdmin {
    history: Arc<dyn HistorySource>,
    source: Arc<dyn MapSource>,
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
    rate: Option<RateLimiter>,
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
        let rate = RateLimiter::new(config.rate_per_sec, config.rate_burst);
        Self {
            history,
            source,
            map,
            time,
            overlay: Arc::new(EmptyOverlay),
            names: None,
            config,
            ops: None,
            auth,
            rate,
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

    /// Attach a live overlay source (players/entities on the map).
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

        // A token presented only in the query string is turned into a cookie so
        // the page's own JS fetches (which carry cookies but not the query) are
        // authenticated too.
        let set_cookie = req.token.is_none() && req.cookie.is_none() && req.get("token").is_some();

        let mut res = self.route(req);
        if set_cookie {
            if let Some(t) = req.get("token") {
                res.headers.push((
                    "Set-Cookie",
                    format!(
                        "{}={t}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                        auth::COOKIE
                    ),
                ));
            }
        }
        res
    }

    /// Dispatch an authenticated request.
    fn route(&self, req: &Request) -> Response {
        // Everything is a GET except the login handled by the caller.
        if req.method != "GET" {
            return Response::text(405, "method not allowed\n".to_string());
        }

        // Rate-limit the expensive endpoints (tiles and journal scans).
        if matches!(
            req.path.as_str(),
            "/tile" | "/tile_at" | "/surface" | "/history" | "/changes" | "/player"
        ) {
            if let Some(rate) = &self.rate {
                if !rate.allow() {
                    return Response::text(429, "rate limited\n".to_string());
                }
            }
        }

        match req.path.as_str() {
            // Live ops (WEB1): the dashboard is the home page; the map (WEB2)
            // lives at `/map`.
            "/" => self.ops_page(|o| Some(pages::dashboard(o))),
            "/regions" => self.ops_page(|o| Some(pages::regions(o))),
            "/players" => self.ops_page(|o| Some(pages::players(o))),
            "/profile" => match req.get("id") {
                Some(id) => self.ops_page(move |o| o.profile(id).map(|p| pages::profile(&p))),
                None => Response::text(400, "id required\n".to_string()),
            },
            "/inventory" => match req.get("id") {
                Some(id) => self.ops_page(move |o| o.inventory(id).map(|v| pages::inventory(&v))),
                None => Response::text(400, "id required\n".to_string()),
            },
            "/api/dashboard" => self.ops_json(|o| serde_json::to_value(o.dashboard()).ok()),
            "/api/regions" => self.ops_json(|o| serde_json::to_value(o.regions(100)).ok()),
            "/api/players" => self.ops_json(|o| serde_json::to_value(o.players()).ok()),
            "/api/player" => match req.get("id") {
                Some(id) => {
                    self.ops_json(move |o| o.profile(id).and_then(|p| serde_json::to_value(p).ok()))
                }
                None => Response::text(400, "id required\n".to_string()),
            },
            "/api/inventory" => match req.get("id") {
                Some(id) => self
                    .ops_json(move |o| o.inventory(id).and_then(|v| serde_json::to_value(v).ok())),
                None => Response::text(400, "id required\n".to_string()),
            },

            // Map and time machine (WEB2).
            "/map" => Response::html(INDEX_HTML.to_string()),
            "/app.js" => Response::js(APP_JS),
            "/info" => self.info(),
            "/head" => Response::json(format!("{{\"head\":{}}}", self.history.head())),
            "/tile" => self.tile(req),
            "/tile_at" => self.tile_at(req),
            "/overlay" => self.overlay(),
            "/surface" => self.surface(req),
            "/history" => self.history(req),
            "/changes" => self.changes(req),
            "/player" => self.player(req),
            _ if req.path.starts_with("/tile") => {
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

    /// A tile key from the query, with `zoom` clamped to [`MAX_ZOOM`] and the
    /// tile coordinates checked so a huge `tx`/`tz` cannot overflow.
    fn tile_key(req: &Request) -> Option<TileKey> {
        let zoom = req.u64("zoom").unwrap_or(0).min(MAX_ZOOM as u64) as u8;
        let tx = req.i32("tx")?;
        let tz = req.i32("tz")?;
        if !(MIN_TILE_COORD..=MAX_TILE_COORD).contains(&tx)
            || !(MIN_TILE_COORD..=MAX_TILE_COORD).contains(&tz)
        {
            return None;
        }
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
        let r = req.u64("r").unwrap_or(16).min(u32::MAX as u64) as u32;
        let since = req.u64("since").unwrap_or(0);
        let until = req.u64("until").unwrap_or(u64::MAX);
        let limit = req
            .u64("limit")
            .map(|l| l as usize)
            .unwrap_or(self.config.history_limit);
        match self.time.changes_in_area(x, z, r, since, until, limit) {
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
            q = q.near(x, z, req.u64("r").unwrap_or(48).min(u32::MAX as u64) as u32);
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

    /// The name of a block state, from the **world's** registry (the single
    /// name authority), falling back to the global table only when the world
    /// does not know the id.
    fn name_of(&self, id: aether_world::BlockStateId) -> String {
        self.source
            .block_name(id)
            .unwrap_or_else(|| block_name_of(id))
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
        let gate = ConnGate::new(admin.config.max_conns, admin.config.max_per_ip);
        let workers = admin.config.max_conns.clamp(2, 16);
        let pool = ThreadPool::new(workers, admin.config.max_conns);
        let inner = Arc::clone(&pool.inner);
        let stop_thread = Arc::clone(&stop);
        let admin_thread = Arc::clone(&admin);
        let handle = thread::Builder::new()
            .name("webadmin".to_string())
            .spawn(move || {
                while !stop_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, peer)) => {
                            let Some(permit) = gate.try_acquire(peer.ip()) else {
                                let _ = write_response(
                                    stream,
                                    &Response::text(503, "server busy\n".to_string()),
                                );
                                continue;
                            };
                            if !pool_has_room(&inner) {
                                drop(permit);
                                let _ = write_response(
                                    stream,
                                    &Response::text(503, "server busy\n".to_string()),
                                );
                                continue;
                            }
                            let admin = Arc::clone(&admin_thread);
                            pool_submit(
                                &inner,
                                Box::new(move || {
                                    let _permit = permit;
                                    let _ = admin.serve_conn(stream);
                                }),
                            );
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
            pool,
        })
    }

    fn serve_conn(&self, stream: TcpStream) -> io::Result<()> {
        let deadline = Instant::now() + self.config.request_deadline;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let set_read_deadline = |stream: &TcpStream| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = stream.set_read_timeout(Some(remaining.min(Duration::from_secs(5))));
        };
        set_read_deadline(&stream);
        let mut reader = BufReader::new(stream.try_clone()?);

        // Request line.
        let mut line = Vec::new();
        match read_deadline_line(&mut reader, &mut line, MAX_REQUEST_LINE, deadline) {
            Ok(Line::Eof) => return Ok(()),
            Ok(Line::Ok) => {}
            Err(LineError::TooLong) => {
                return write_response(stream, &Response::text(431, "request line too long\n"))
            }
            Err(LineError::Deadline) => {
                return write_response(stream, &Response::text(408, "request timeout\n"))
            }
            Err(LineError::Io(e)) => return Err(e),
        }
        let Some(mut req) = parse_request(&String::from_utf8_lossy(&line)) else {
            return write_response(stream, &Response::text(400, "bad request\n"));
        };

        // Headers: bounded in count and total size.
        let mut length = 0usize;
        let mut header_bytes = 0usize;
        for _ in 0..MAX_HEADERS {
            set_read_deadline(&stream);
            let mut h = Vec::new();
            match read_deadline_line(&mut reader, &mut h, MAX_HEADER_BYTES, deadline) {
                Ok(Line::Eof) | Ok(Line::Ok) => {}
                Err(LineError::TooLong) => {
                    return write_response(stream, &Response::text(431, "headers too large\n"))
                }
                Err(LineError::Deadline) => {
                    return write_response(stream, &Response::text(408, "request timeout\n"))
                }
                Err(LineError::Io(e)) => return Err(e),
            }
            header_bytes += h.len();
            if header_bytes > MAX_HEADER_BYTES {
                return write_response(stream, &Response::text(431, "headers too large\n"));
            }
            let text = String::from_utf8_lossy(&h);
            let t = text.trim_end();
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
            if length > MAX_BODY {
                return write_response(stream, &Response::text(413, "body too large\n"));
            }
            set_read_deadline(&stream);
            let mut buf = vec![0u8; length];
            read_deadline_exact(&mut reader, &mut buf, deadline)?;
            req.body = String::from_utf8_lossy(&buf).into_owned();
        }

        let res = self.handle(&req);
        write_response(stream, &res)
    }
}

/// A running server; dropping it stops the accept loop and joins the threads.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pool: ThreadPool,
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

    /// Stop the server, waiting for the accept loop and the workers to exit.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop on the next poll rather than after the sleep.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.pool.shutdown();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The outcome of a bounded line read.
enum Line {
    Ok,
    Eof,
}

enum LineError {
    TooLong,
    Deadline,
    Io(io::Error),
}

/// Read one `\n`-terminated line, at most `max` bytes, before `deadline`.
///
/// Unlike `BufRead::read_line` this never grows the buffer without bound: a
/// client that never sends a newline is cut off at `max` (or at the deadline),
/// not at the process's memory limit.
fn read_deadline_line<R: BufRead>(
    r: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
    deadline: Instant,
) -> Result<Line, LineError> {
    loop {
        if Instant::now() >= deadline {
            return Err(LineError::Deadline);
        }
        let available = r.fill_buf().map_err(LineError::Io)?;
        if available.is_empty() {
            return Ok(Line::Eof);
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if buf.len() + take > max {
                return Err(LineError::TooLong);
            }
            buf.extend_from_slice(&available[..take]);
            r.consume(take);
            return Ok(Line::Ok);
        }
        let take = available.len();
        if buf.len() + take > max {
            return Err(LineError::TooLong);
        }
        buf.extend_from_slice(available);
        r.consume(take);
    }
}

/// Read exactly `buf.len()` bytes before `deadline`.
fn read_deadline_exact<R: BufRead>(r: &mut R, buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    let mut read = 0;
    while read < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "request timeout"));
        }
        let available = r.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "body truncated",
            ));
        }
        let take = available.len().min(buf.len() - read);
        buf[read..read + take].copy_from_slice(&available[..take]);
        r.consume(take);
        read += take;
    }
    Ok(())
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
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
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

  function el(tag, text) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    return node;
  }

  // All remote text is written with textContent, never innerHTML: the names
  // and actor labels come from the world and must not be able to inject markup.
  function renderHistory(wx, wz, surf, rows) {
    hist.replaceChildren();
    hist.append(el('b', `Block (${wx}, ${surf.y}, ${wz})`));
    hist.append(document.createElement('br'));
    const name = el('span', surf.name);
    name.className = 'mono muted';
    hist.append(name);
    const table = document.createElement('table');
    const head = document.createElement('tr');
    for (const h of ['seq', 'when', 'who', 'from → to']) head.append(el('th', h));
    table.append(head);
    if (rows.length === 0) {
      const tr = document.createElement('tr');
      const td = el('td', 'never edited (worldgen)');
      td.colSpan = 4; td.className = 'muted';
      tr.append(td);
      table.append(tr);
    }
    for (const r of rows) {
      const tr = document.createElement('tr');
      const seq = el('td', String(r.seq)); seq.className = 'mono';
      const time = el('td', new Date(r.at_ms).toLocaleString());
      const who = el('td', r.actor.slice(0, 8));
      who.className = 'mono'; who.title = r.actor;
      const move = el('td', `${r.from_name} → ${r.to_name}`);
      tr.append(seq, time, who, move);
      table.append(tr);
    }
    hist.append(table);
  }

  async function showHistory(wx, wz) {
    const atq = at === null ? '' : `&at=${at}`;
    const surf = await fetch(`/surface?x=${wx}&z=${wz}${atq}`).then(r => r.json());
    if (surf.y === null) {
      hist.replaceChildren();
      const muted = el('span', 'No block there.');
      muted.className = 'muted';
      hist.append(muted);
      return;
    }
    const rows = await fetch(`/history?x=${wx}&y=${surf.y}&z=${wz}`).then(r => r.json());
    renderHistory(wx, wz, surf, rows);
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
            ..WebAdminConfig::default()
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
    fn tile_route_rejects_overflowing_coordinates() {
        let a = admin();
        // A coordinate that would overflow `tx * span` is a 400, not a panic.
        let r = get(
            &a,
            "/tile",
            &[("zoom", "6"), ("tx", &i32::MAX.to_string()), ("tz", "0")],
        );
        assert_eq!(r.status, 400);
        let r = get(&a, "/tile", &[("zoom", "0"), ("tx", "12"), ("tz", "-7")]);
        assert_eq!(r.status, 200, "in-range coordinates still work");
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
    fn changes_radius_is_clamped_and_bounded() {
        let a = admin();
        // A huge radius does not hang: it is clamped and returns promptly.
        let r = get(
            &a,
            "/changes",
            &[("x", "0"), ("z", "0"), ("r", &i32::MAX.to_string())],
        );
        assert_eq!(r.status, 200);
        let body = String::from_utf8(r.body).unwrap();
        // The one edit from setup is inside the (clamped) area.
        assert!(body.contains("\"x\":0"), "{body}");
        assert!(body.len() < 4096, "bounded response");
    }

    #[test]
    fn non_get_is_method_not_allowed() {
        let a = admin();
        let mut req = request("/info", &[], Some("test"));
        req.method = "DELETE".to_string();
        assert_eq!(a.handle(&req).status, 405);
        let mut req = request("/players", &[], Some("test"));
        req.method = "PUT".to_string();
        assert_eq!(a.handle(&req).status, 405);
    }

    #[test]
    fn a_query_token_sets_a_cookie_for_the_page_js() {
        let a = admin();
        // No token header, `?token=` present: authenticated, and the response
        // carries a cookie so the page's own fetches are authenticated too.
        let mut req = request("/map", &[("token", "test")], None);
        req.token = None;
        let res = a.handle(&req);
        assert_eq!(res.status, 200);
        assert!(
            res.headers.iter().any(|(k, _)| *k == "Set-Cookie"),
            "a query token is upgraded to a cookie"
        );
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

    /// A single client opening more than `max_per_ip` connections at once is
    /// shed with 503 rather than admitted.
    #[test]
    fn per_ip_connection_limit_is_enforced() {
        let w = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
        let cfg = WebAdminConfig {
            token: Some("test".to_string()),
            max_conns: 4,
            max_per_ip: 2,
            ..WebAdminConfig::default()
        };
        let a = WebAdmin::with_config(w.clone(), w, cfg);
        let mut server = a.serve("127.0.0.1:0".parse().unwrap()).unwrap();
        // Two idle connections hold the per-IP permits without finishing.
        let c1 = TcpStream::connect(server.addr()).unwrap();
        let c2 = TcpStream::connect(server.addr()).unwrap();
        // Give the accept loop a moment to register them.
        std::thread::sleep(Duration::from_millis(150));
        // The third from the same IP is refused at the gate.
        let mut c3 = TcpStream::connect(server.addr()).unwrap();
        let body = read_all(&mut c3);
        assert!(
            body.contains("503") || body.contains("busy"),
            "third connection shed: {body:?}"
        );
        drop(c1);
        drop(c2);
        server.stop();
    }

    /// A request line with no newline is cut off at the limit, not buffered
    /// forever.
    #[test]
    fn overlong_request_line_is_refused() {
        let a = admin();
        let mut server = a.serve("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        // One huge line, no terminator.
        let big = vec![b'A'; MAX_REQUEST_LINE + 100];
        let _ = stream.write_all(&big);
        let _ = stream.flush();
        let body = read_all(&mut stream);
        assert!(
            body.contains("431") || body.contains("400"),
            "overlong line refused: {body:?}"
        );
        server.stop();
    }

    /// A slowloris that sends one byte at a time is cut off by the absolute
    /// request deadline rather than holding a worker forever.
    #[test]
    fn a_slow_client_hits_the_request_deadline() {
        let w = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
        let cfg = WebAdminConfig {
            token: Some("test".to_string()),
            request_deadline: Duration::from_millis(300),
            ..WebAdminConfig::default()
        };
        let a = WebAdmin::with_config(w.clone(), w, cfg);
        let mut server = a.serve("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        let started = Instant::now();
        // Dribble the request line a byte at a time; never finish it.
        let mut sent = 0;
        while started.elapsed() < Duration::from_secs(3) {
            if stream.write_all(b"G").is_err() {
                break;
            }
            let _ = stream.flush();
            sent += 1;
            std::thread::sleep(Duration::from_millis(40));
        }
        let body = read_all(&mut stream);
        assert!(
            body.contains("408"),
            "slowloris cut off (sent {sent} bytes): {body:?}"
        );
        server.stop();
    }

    /// The token-bucket limiter sheds a burst above the configured rate.
    #[test]
    fn the_rate_limiter_sheds_a_burst() {
        // 1/s with a burst of 2: the third immediate call is refused.
        let rl = RateLimiter::new(1.0, 2.0).unwrap();
        assert!(rl.allow());
        assert!(rl.allow());
        assert!(!rl.allow(), "burst exhausted");
    }

    fn read_all(stream: &mut TcpStream) -> String {
        use std::io::Read;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
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
