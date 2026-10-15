//! WEB2 — the admin web interface: a top-down map and a journal time machine.
//!
//! This crate is a **native** plugin (tier N): trusted, in-process Rust that
//! the server links in. It is deliberately split so the interesting parts do
//! not need the plugin host:
//!
//! * [`color`] — block name → RGB, the map palette.
//! * [`source`] — the two read-only views of a world (`MapSource`,
//!   `HistorySource`), with blanket impls for [`aether_api::World`].
//! * [`map`] — tile rendering, caching and re-render-on-change.
//! * [`timemachine`] — replay the journal to any past sequence number:
//!   per-block history, an area's changes over a time window, and map tiles
//!   *as they were*.
//! * [`playerhist`] — a player's place/break/take/put history, filtered by
//!   time and area.
//! * [`http`] — a small embedded server that serves the map page and its JSON,
//!   with no framework and no CDN.
//!
//! Everything is **read-only**: no route, no function and no plugin hook here
//! writes a block, an item or a file. The time machine computes past states; it
//! never restores them.
//!
//! ## Wiring
//!
//! WEB2 depends on **PA2** (the read-only History API) and **WEB1** (the HTTP
//! server and login). Both landed on separate branches; neither is in `main`
//! yet, so this stage ships the map and time machine as a self-contained
//! module with its own minimal server, and a native-plugin wrapper
//! ([`WebAdminPlugin`]) that the server bootstrap attaches to a live world once
//! WEB1's bootstrap exists. See `docs/tasks/WEB2-notes.md`.

pub mod color;
pub mod http;
pub mod map;
pub mod playerhist;
pub mod source;
pub mod timemachine;

use aether_plugin::manifest::{Manifest, PluginTier};
use aether_plugin::{Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player};

pub use http::{Mark, Overlay, OverlaySource, Request, Response, Server, WebAdmin, WebAdminConfig};
pub use map::{MapRenderer, Surface, Tile, TileKey};
pub use source::{HistorySource, HistoryView, MapSource, MapView};
pub use timemachine::{AtSeq, BlockChange, ChangeRecord, TimeMachine};

/// The default bind address, matching the WEB brief.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// The plugin manifest. Native tier: no `.wasm` entry, compiled in.
pub fn manifest() -> Manifest {
    Manifest {
        id: "webadmin".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Admin web map".to_string(),
        // After gameplay plugins: the map only observes.
        priority: 10,
        capabilities: Vec::new(),
        subscribe: vec![
            "after-block-break".to_string(),
            "before-block-place".to_string(),
        ],
        commands: Vec::new(),
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// A build-time registration with no world attached. A server that has a live
/// world should build a [`WebAdmin`] and pass it to [`WebAdminPlugin::new`]
/// instead of using this.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(WebAdminPlugin::default()),
    }
}

/// The native plugin wrapper around a [`WebAdmin`].
///
/// The host gives a native plugin no world handle, by design — a plugin sees
/// its config, its services and the log, nothing more. So the world is
/// attached at construction time by the server bootstrap, and this wrapper
/// only owns the running server's lifecycle and forwards block-change events
/// into the tile cache.
pub struct WebAdminPlugin {
    admin: Option<WebAdmin>,
    server: Option<Server>,
    map: Option<std::sync::Arc<MapRenderer>>,
    bind: String,
}

impl Default for WebAdminPlugin {
    fn default() -> Self {
        Self {
            admin: None,
            server: None,
            map: None,
            bind: DEFAULT_BIND.to_string(),
        }
    }
}

impl WebAdminPlugin {
    /// Wrap an admin app; it is served on `bind` when the plugin initialises.
    pub fn new(admin: WebAdmin, bind: impl Into<String>) -> Self {
        let map = admin.map_handle();
        Self {
            admin: Some(admin),
            server: None,
            map: Some(map),
            bind: bind.into(),
        }
    }
}

impl NativePlugin for WebAdminPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Operator config overrides the constructor default.
        if let Some(bind) = ctx.config.get("bind") {
            self.bind = bind.clone();
        }
        let Some(admin) = self.admin.take() else {
            // Without a world there is nothing to draw. This is the state a
            // `registration()` install is in until WEB1's bootstrap attaches
            // one; it is not an error, so the plugin still loads.
            ctx.log(
                LogLevel::Warn,
                "webadmin loaded without a world: the map is disabled until the server \
                 attaches one (WEB1 bootstrap)",
            );
            return Ok(());
        };
        let addr = self
            .bind
            .parse()
            .map_err(|e| format!("invalid webadmin bind `{}`: {e}", self.bind))?;
        let server = admin
            .serve(addr)
            .map_err(|e| format!("cannot bind webadmin on {}: {e}", self.bind))?;
        ctx.log(
            LogLevel::Info,
            &format!("admin map listening on {}", server.url()),
        );
        self.server = Some(server);
        Ok(())
    }

    fn shutdown(&mut self, _ctx: &mut NativeContext) {
        if let Some(mut server) = self.server.take() {
            server.stop();
        }
    }

    fn after_block_break(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        x: i32,
        _y: i32,
        z: i32,
        _block: &str,
    ) {
        self.invalidate(x, z);
    }

    fn before_block_place(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        x: i32,
        _y: i32,
        z: i32,
        _block: &str,
    ) -> Decision {
        // Invalidate before the write lands: the next tile request rebuilds
        // from the world, which by then holds the new block. A cancelled place
        // merely costs one unnecessary rebuild.
        self.invalidate(x, z);
        Decision::Allow
    }
}

impl WebAdminPlugin {
    /// Drop the map tiles that contain `(x, z)` so the next request rebuilds
    /// them from the live world.
    fn invalidate(&mut self, x: i32, z: i32) {
        if let Some(map) = &self.map {
            map.invalidate(x, z);
        }
    }
}
