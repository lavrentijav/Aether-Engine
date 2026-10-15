//! The sidebar plugin: the panel on the right of every player's screen.
//!
//! A native plugin (tier N, per `docs/design/plugins.md`). It composes a
//! [`SidebarBoard`] for each online player from `config.toml` and hands the
//! board to the core, which draws it in that client's own protocol and resends
//! only the lines that changed. The plugin never builds packets and never
//! touches the scoreboard store.
//!
//! **Where the pieces live.** The *mechanism* — the scoreboard packets, the
//! per-protocol differences and the flicker-free diff — is core
//! (`crates/aether-server`). The *policy* — the layouts, their conditions and
//! the update rate — is here. The `%placeholders%` themselves are shared with
//! menus and chat, so their registry is the SDK's `sdk.placeholders` service,
//! which this plugin **requires**; it resolves each line through that service.
//!
//! It publishes a `sidebar@0.1` service, so another plugin can override a line
//! or toggle a player's panel.

pub mod config;
pub mod text;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::placeholders::{placeholders, PlaceholdersService};
use aether_plugin::sdk::{self, PermissionsService};
use aether_plugin::sidebar::{self, SidebarBoard, SidebarService};
use aether_plugin::{LogLevel, NativeContext, NativePlugin, NativeRegistration, Player};

use config::SidebarConfig;

/// The most lines a sidebar can show; mirrored by the core.
pub const MAX_LINES: usize = 15;

/// The plugin manifest. It subscribes to the tick (rate-limited internally to
/// `update_hz`) and to joins and leaves so per-player state does not outlive a
/// session. It requires the SDK's placeholders service.
pub fn manifest() -> Manifest {
    Manifest {
        id: "sidebar".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether Sidebar".to_string(),
        priority: 0,
        capabilities: Vec::new(),
        subscribe: vec![
            "on-tick".to_string(),
            "after-player-join".to_string(),
            "after-player-leave".to_string(),
        ],
        commands: vec![CommandDecl::Detailed {
            name: "sidebar".to_string(),
            node: String::new(),
            op: 0,
        }],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: vec!["sdk.placeholders@0.1".to_string()],
    }
}

/// The build-time registration the server adds.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(SidebarPlugin::default()),
    }
}

/// The state the plugin and its service share.
struct State {
    config: RwLock<SidebarConfig>,
    /// Per-player line overrides set through the service.
    overrides: Mutex<HashMap<u64, HashMap<usize, String>>>,
    /// Players who turned their sidebar off with `/sidebar toggle`.
    hidden: Mutex<HashSet<u64>>,
    /// The shared placeholder registry, resolved once at `init` (the manifest
    /// requires it, so it is present).
    placeholders: OnceLock<Arc<dyn PlaceholdersService>>,
    /// The permission service, resolved once at `init`; absent until the SDK
    /// (or another provider) is present.
    permissions: OnceLock<Option<Arc<dyn PermissionsService>>>,
}

impl Default for State {
    fn default() -> Self {
        State {
            config: RwLock::new(SidebarConfig::builtin()),
            overrides: Mutex::new(HashMap::new()),
            hidden: Mutex::new(HashSet::new()),
            placeholders: OnceLock::new(),
            permissions: OnceLock::new(),
        }
    }
}

/// The sidebar plugin.
#[derive(Default)]
pub struct SidebarPlugin {
    state: Arc<State>,
}

impl NativePlugin for SidebarPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Load the config from the file named by `config`, defaulting to the
        // plugin's own `config.toml`; fall back to the built-in board if it is
        // missing, so a fresh install still shows something useful.
        let path = ctx
            .config
            .get("config")
            .cloned()
            .unwrap_or_else(|| "plugins/sidebar/config.toml".to_string());
        let cfg = match std::fs::read_to_string(&path) {
            Ok(text) => SidebarConfig::parse(&text).map_err(|e| format!("sidebar: {path}: {e}"))?,
            Err(_) => SidebarConfig::builtin(),
        };
        // An operator may override a few knobs directly under
        // `[plugins.sidebar]`: the update rate and the tab-list mirror.
        let mut cfg = cfg;
        if let Some(hz) = ctx.config.get("update_hz").and_then(|v| v.parse().ok()) {
            cfg.update_hz = hz;
        }
        if let Some(tab) = ctx.config.get("tab").and_then(|v| v.parse().ok()) {
            cfg.tab = tab;
        }
        *self.state.config.write().unwrap() = cfg;

        // The placeholders service is required (the host refused the load if
        // it were absent); a missing provider here is a contract violation.
        let svc = placeholders(ctx.services)
            .ok_or("sidebar requires `sdk.placeholders@0.1`, but it is not registered")?;
        let _ = self.state.placeholders.set(svc);

        // The permission service is optional: the SDK fronts the permission
        // model and fails closed until CMD-01/02 land, so a permission-gated
        // layout simply does not match while it is absent.
        let _ = self.state.permissions.set(sdk::permissions(ctx.services));

        let service: Arc<dyn SidebarService> = Arc::new(SidebarSvc(Arc::clone(&self.state)));
        sidebar::register_sidebar(ctx.services, service)?;

        let cfg = self.state.config.read().unwrap();
        ctx.log(
            LogLevel::Info,
            &format!(
                "sidebar ready: {} layout(s), {} Hz, title {:?}",
                cfg.layout.len(),
                cfg.update_hz,
                cfg.title
            ),
        );
        Ok(())
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, n: u64) {
        let (hz, tab) = {
            let cfg = self.state.config.read().unwrap();
            (cfg.update_hz.max(1), cfg.tab)
        };
        let period = (20 / hz).max(1) as u64;
        if n % period != 0 {
            return;
        }
        let Some(svc) = self.state.placeholders.get().cloned() else {
            return;
        };
        let players = ctx.backend.online_players(ctx.id);
        for p in &players {
            if self.state.hidden.lock().unwrap().contains(&p.id) {
                // Hide once; the core's diff makes a repeat a no-op.
                ctx.backend.clear_sidebar(ctx.id, p.id);
                continue;
            }
            let Some(board) = self.board_for(ctx, p, &*svc) else {
                continue;
            };
            ctx.backend.set_sidebar(ctx.id, p.id, &board);
            if tab {
                // The same board in the tab list: the title is the header, the
                // lines are the footer.
                let (header, footer) = tab_parts(&board);
                ctx.backend
                    .set_tab_header_footer(ctx.id, p.id, &header, &footer);
            }
        }
    }

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        self.state.overrides.lock().unwrap().remove(&p.id);
        self.state.hidden.lock().unwrap().remove(&p.id);
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<aether_plugin::CommandResult> {
        if name != "sidebar" {
            return None;
        }
        let sub = args.first().map(|s| s.as_str()).unwrap_or("toggle");
        let mut lines = Vec::new();
        match sub {
            "toggle" => {
                let showing = {
                    let mut hidden = self.state.hidden.lock().unwrap();
                    if hidden.remove(&p.id) {
                        true
                    } else {
                        hidden.insert(p.id);
                        false
                    }
                };
                lines.push(if showing {
                    "&aSidebar shown.".to_string()
                } else {
                    "&7Sidebar hidden.".to_string()
                });
            }
            "show" => {
                self.state.hidden.lock().unwrap().remove(&p.id);
                lines.push("&aSidebar shown.".to_string());
            }
            "hide" => {
                self.state.hidden.lock().unwrap().insert(p.id);
                lines.push("&7Sidebar hidden.".to_string());
            }
            "reload" => match self.reload(ctx) {
                Ok(()) => lines.push("&aSidebar config reloaded.".to_string()),
                Err(e) => lines.push(format!("&cReload failed: {e}")),
            },
            other => {
                lines.push(format!("&cUnknown subcommand `{other}`."));
                lines.push("&7/sidebar toggle|show|hide|reload".to_string());
            }
        }
        Some(aether_plugin::CommandResult {
            handled: true,
            lines,
        })
    }
}

impl SidebarPlugin {
    /// Compose one player's board, or `None` when no layout matches.
    fn board_for(
        &self,
        ctx: &NativeContext,
        p: &Player,
        svc: &dyn PlaceholdersService,
    ) -> Option<SidebarBoard> {
        let cfg = self.state.config.read().unwrap();
        let layout = self.pick_layout(&cfg, ctx.id, p, svc)?;
        let overrides = self.state.overrides.lock().unwrap();
        let player_overrides = overrides.get(&p.id);
        let title = text::colorize(&svc.resolve(ctx.id, p, &cfg.title));
        let mut lines = Vec::with_capacity(layout.lines.len());
        for (i, line) in layout.lines.iter().enumerate() {
            let raw = player_overrides.and_then(|m| m.get(&i)).unwrap_or(line);
            lines.push(text::colorize(&svc.resolve(ctx.id, p, raw)));
        }
        Some(SidebarBoard { title, lines })
    }

    /// The highest-priority layout whose conditions hold for `p`.
    fn pick_layout<'a>(
        &self,
        cfg: &'a SidebarConfig,
        caller: &str,
        p: &Player,
        svc: &dyn PlaceholdersService,
    ) -> Option<&'a config::Layout> {
        let perms = self.state.permissions.get().and_then(|p| p.clone());
        let mut best: Option<&config::Layout> = None;
        for layout in &cfg.layout {
            if !layout.permission.is_empty() {
                let allowed = perms
                    .as_ref()
                    .map(|s| s.check(caller, p, &layout.permission))
                    .unwrap_or(false);
                if !allowed {
                    continue;
                }
            }
            if !layout.world.is_empty() {
                // `%world%` stays literal when the host cannot answer, so the
                // condition simply fails to match.
                if svc.resolve(caller, p, "%world%") != layout.world {
                    continue;
                }
            }
            if !layout.placeholder.is_empty()
                && svc.resolve(caller, p, &layout.placeholder) != layout.equals
            {
                continue;
            }
            match best {
                Some(b) if b.priority >= layout.priority => {}
                _ => best = Some(layout),
            }
        }
        best
    }

    /// Reload the `config.toml`, keeping the previous config on a parse error.
    fn reload(&self, ctx: &mut NativeContext) -> Result<(), String> {
        let path = ctx
            .config
            .get("config")
            .cloned()
            .unwrap_or_else(|| "plugins/sidebar/config.toml".to_string());
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let cfg = SidebarConfig::parse(&text)?;
        *self.state.config.write().unwrap() = cfg;
        Ok(())
    }
}

/// Split a board into a tab-list header and footer: the title is the header,
/// the lines are the footer.
fn tab_parts(board: &SidebarBoard) -> (String, String) {
    (board.title.clone(), board.lines.join("\n"))
}

/// The `sidebar@0.1` service, over the plugin's shared state.
struct SidebarSvc(Arc<State>);

impl SidebarService for SidebarSvc {
    fn set_line(&self, player: u64, index: usize, text: String) {
        if index >= MAX_LINES {
            return;
        }
        self.0
            .overrides
            .lock()
            .unwrap()
            .entry(player)
            .or_default()
            .insert(index, text);
    }

    fn clear_line(&self, player: u64, index: usize) {
        let mut overrides = self.0.overrides.lock().unwrap();
        if let Some(lines) = overrides.get_mut(&player) {
            lines.remove(&index);
        }
    }

    fn show(&self, player: u64) {
        self.0.hidden.lock().unwrap().remove(&player);
    }

    fn hide(&self, player: u64) {
        self.0.hidden.lock().unwrap().insert(player);
    }

    fn toggle(&self, player: u64) -> bool {
        let mut hidden = self.0.hidden.lock().unwrap();
        if hidden.remove(&player) {
            true
        } else {
            hidden.insert(player);
            false
        }
    }
}
