//! The admin web interface, as a native plugin (tier N).
//!
//! One read-only HTTP server inside the server process, serving two stages of
//! [`docs/tasks/QUEUE-WEB-admin.md`]:
//!
//! * **WEB1 — live ops**: a dashboard (TPS/MSPT, memory, players), the
//!   heaviest regions (a sampled per-region tick-cost ranking and a heat
//!   grid), a players list with live/saved inventories and per-player
//!   profiles, behind a token login (`/webadmin token`).
//! * **WEB2 — map + time machine**: top-down block-colour tiles cached and
//!   re-rendered on change, a journal "time slider" (per-block history, an
//!   area's changes over a window, and tiles *as they were*), and per-player
//!   place/break/take/put history.
//!
//! The crate is split so the interesting parts do not need the plugin host or
//! the server: [`color`] (the map palette), [`source`] (the two read-only world
//! views), [`map`], [`timemachine`], [`playerhist`], [`pages`] and
//! [`http`] are self-contained. The plugin wrapper ([`WebAdminPlugin`]) owns
//! only the server lifecycle, the token command and tile invalidation.
//!
//! Everything is **read-only**: no route, no function and no plugin hook here
//! writes a block, an item or a file.
//!
//! ## Wiring
//!
//! The server attaches the live [`aether_api::World`] at construction
//! ([`WebAdminPlugin::with_world`]) and publishes the live-ops service
//! (`ops@0.1`, the [`aether_ops`] trait) which the plugin resolves in `init`.
//! See `crates/aether-server/src/plugins.rs`.

pub mod auth;
pub mod color;
pub mod http;
pub mod map;
pub mod pages;
pub mod playerhist;
pub mod source;
pub mod timemachine;

use std::sync::Arc;

use aether_api::World;
use aether_ops::{OpsHandle, SERVICE, VERSION};
use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};
use aether_world::KvBackend;
use aether_worldgen::ChunkGenerator;

pub use auth::{Auth, COOKIE, ONE_TIME_TTL};
pub use http::{Mark, Overlay, OverlaySource, Request, Response, Server, WebAdmin, WebAdminConfig};
pub use map::{MapRenderer, Surface, Tile, TileKey};
pub use source::{HistorySource, HistoryView, MapSource, MapView};
pub use timemachine::{AtSeq, BlockChange, ChangeRecord, TimeMachine};

/// The default bind address, matching the WEB brief.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// The plugin manifest. Native tier: no `.wasm` entry, compiled in.
///
/// It requires the live-ops service (`ops@0.1`) the server publishes, so the
/// dashboard is present; it subscribes to the block events so the map cache
/// invalidates, and to `on-command` for `/webadmin token`.
pub fn manifest() -> Manifest {
    Manifest {
        id: "webadmin".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Admin web".to_string(),
        // After gameplay plugins: the web only observes.
        priority: 10,
        capabilities: Vec::new(),
        subscribe: vec![
            "on-command".to_string(),
            "after-block-break".to_string(),
            "before-block-place".to_string(),
        ],
        commands: vec![CommandDecl::Detailed {
            name: "webadmin".to_string(),
            node: "webadmin.command".to_string(),
            op: 3,
        }],
        requires: vec![format!("{SERVICE}@{VERSION}")],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
    }
}

/// A build-time registration with no world attached. A server with a live
/// world should build a [`WebAdminPlugin::with_world`] instead.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(WebAdminPlugin::default()),
    }
}

/// The native plugin wrapper: server lifecycle, the `/webadmin token` command
/// and, when a world is attached, the running [`WebAdmin`].
///
/// The host gives a native plugin no world handle, by design, so the server
/// attaches the world at construction and this wrapper forwards block-change
/// events into the tile cache.
pub struct WebAdminPlugin {
    map_source: Option<Arc<dyn MapSource>>,
    history_source: Option<Arc<dyn HistorySource>>,
    server: Option<Server>,
    map: Option<Arc<MapRenderer>>,
    auth: Option<Arc<Auth>>,
    bind: String,
    tile_px: u32,
    y_min: i32,
    y_max: i32,
    history_limit: usize,
}

impl Default for WebAdminPlugin {
    fn default() -> Self {
        Self {
            map_source: None,
            history_source: None,
            server: None,
            map: None,
            auth: None,
            bind: DEFAULT_BIND.to_string(),
            tile_px: map::DEFAULT_TILE_PX,
            y_min: -64,
            y_max: 319,
            history_limit: 500,
        }
    }
}

impl WebAdminPlugin {
    /// Wrap a live world. The server calls this in `install_native`; the plugin
    /// serves it once it initialises.
    pub fn with_world<B, G>(world: Arc<World<B, G>>) -> Self
    where
        B: KvBackend + Send + Sync + 'static,
        G: ChunkGenerator + Send + Sync + 'static,
    {
        let map_source: Arc<dyn MapSource> = world.clone();
        let history_source: Arc<dyn HistorySource> = world;
        Self {
            map_source: Some(map_source),
            history_source: Some(history_source),
            ..Self::default()
        }
    }

    /// Drop the map tiles that contain `(x, z)`.
    fn invalidate(&mut self, x: i32, z: i32) {
        if let Some(map) = &self.map {
            map.invalidate(x, z);
        }
    }
}

impl NativePlugin for WebAdminPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Operator config overrides the constructor default.
        if let Some(bind) = ctx.config.get("bind") {
            self.bind = bind.clone();
        }
        let token = ctx.config.get("token").cloned().filter(|t| !t.is_empty());

        let (Some(map_source), Some(history_source)) =
            (self.map_source.take(), self.history_source.take())
        else {
            // Without a world there is nothing to draw or report. This is the
            // state a `registration()` install is in until the server attaches
            // one; it is not an error, so the plugin still loads.
            ctx.log(
                LogLevel::Warn,
                "webadmin loaded without a world: the map and live ops are disabled until \
                 the server attaches one (install_native)",
            );
            return Ok(());
        };

        let addr = self
            .bind
            .parse()
            .map_err(|e| format!("invalid webadmin bind `{}`: {e}", self.bind))?;

        // The live-ops counters, published by the server. The manifest
        // `requires` it, so it is normally present; treat absence as
        // "dashboard disabled" rather than refusing the whole plugin.
        let ops: Option<Arc<OpsHandle>> = ctx.services.get::<OpsHandle>(SERVICE, VERSION);
        if ops.is_none() {
            ctx.log(
                LogLevel::Warn,
                &format!("live-ops service `{SERVICE}@{VERSION}` missing: dashboard disabled"),
            );
        }

        let config = WebAdminConfig {
            tile_px: self.tile_px,
            y_min: self.y_min,
            y_max: self.y_max,
            token,
            history_limit: self.history_limit,
        };
        let mut admin = WebAdmin::with_config(map_source, history_source, config);
        if let Some(ops) = ops {
            admin = admin.with_ops((*ops).clone());
        }
        self.auth = Some(Arc::clone(admin.auth()));
        self.map = Some(admin.map_handle());

        // Register the command through the core registry so `/webadmin` goes
        // through the one permission gate.
        ctx.backend
            .register_command(ctx.id, "webadmin", "webadmin.command", 3)?;

        let server = admin
            .serve(addr)
            .map_err(|e| format!("cannot bind webadmin on {}: {e}", self.bind))?;
        ctx.log(
            LogLevel::Info,
            &format!("web admin listening on {}", server.url()),
        );
        self.server = Some(server);
        Ok(())
    }

    fn shutdown(&mut self, _ctx: &mut NativeContext) {
        if let Some(mut server) = self.server.take() {
            server.stop();
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
        let sub = args.first().map(String::as_str).unwrap_or("status");
        let lines = match (sub, &self.auth) {
            ("token", Some(auth)) => {
                let token = auth.issue_one_time();
                ctx.log(LogLevel::Info, "issued a one-time web admin token");
                vec![
                    format!("Web admin one-time token: {token}"),
                    format!(
                        "Valid {} minutes. Open {} and paste it.",
                        auth::ONE_TIME_TTL.as_secs() / 60,
                        self.bind,
                    ),
                ]
            }
            ("status", Some(auth)) => vec![format!(
                "Web admin on http://{} ({}).",
                self.bind,
                if auth.has_config_token() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_requires_ops_and_serves_webadmin() {
        let m = manifest();
        assert_eq!(m.id, "webadmin");
        assert_eq!(m.tier, PluginTier::Native);
        assert_eq!(m.requires, vec!["ops@0.1".to_string()]);
        assert!(m.serves_command("webadmin"));
        assert!(m.subscribes("on-command"));
        assert!(m.subscribes("after-block-break"));
        assert!(m.subscribes("before-block-place"));
    }
}
