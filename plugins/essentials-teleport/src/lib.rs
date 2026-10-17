//! The EssentialsX-style **homes, warps and plugin spawn** plugin (CMD-37a).
//!
//! A native (tier N) plugin: it holds no gameplay mechanism of its own. Every
//! move goes through the core teleport service (CMD-31) via
//! [`HostBackend::teleport_player`], which searches for a safe spot, so a home
//! or warp saved above lava or in the void cannot drop a player into it.
//! Homes, warps and the plugin spawn live in plugin KV (P1) and are replayed on
//! `init`, so they survive a restart.
//!
//! ```text
//! /sethome [<name>]   save the current position (no bed or anchor needed)
//! /home [<name>]      teleport to a saved home, after the configured warmup
//! /delhome <name>     remove one home
//! /homes              list the caller's homes and the limit
//! /setwarp <name>     create or replace a server warp
//! /warp <name>        teleport to a warp, after the configured warmup
//! /delwarp <name>     remove a warp
//! /warps              list the warps
//! /setspawn           store the plugin's spawn point
//! ```
//!
//! **`/spawn` stays core.** This plugin never registers or overrides it; the
//! umbrella brief's ownership rule reserves vanilla/core names for the engine.
//! `/setspawn` only records the point the core (or a future spawn provider)
//! may read.
//!
//! ## Warmup
//!
//! `warmup-ticks` (or `warmup`/`warmup-seconds`, converted at 20 ticks per
//! second) delays a move. A pending move is cancelled when the player moves
//! more than a tiny epsilon, takes damage, dies or leaves; this is the
//! EssentialsX warmup contract. The delay is plugin policy, so it is config,
//! not core.
//!
//! ## Home limit
//!
//! The default limit is `max-homes` (1). A permission raises it:
//! `essentials.sethome.multiple.<n>` grants a cap of `n`, while the bare
//! `essentials.sethome.multiple` (and `essentials.sethome.unlimited`) removes
//! the cap. Operators bypass the plugin-side gates, like the other native
//! plugins. The limit only applies to a **new** name; replacing an existing
//! home is always allowed.
//!
//! ## Persistence and ownership
//!
//! Homes are keyed by the player's UUID when the host exposes one, otherwise
//! by the lower-cased login name, so one player can never see, replace or
//! delete another player's home even when both use the name `home`. Warps and
//! the spawn are server-wide. Every mutation writes the complete JSON value
//! first and only then adopts it in memory: a failed KV write changes nothing
//! and is reported to the caller.

use std::collections::{BTreeMap, HashMap};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::sdk::TeleportCause;
use aether_plugin::{
    CommandResult, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The plugin id: its KV namespace and log tag.
pub const ID: &str = "essentials-teleport";

/// `/sethome`.
pub const COMMAND_SET_HOME: &str = "sethome";
/// `/home`.
pub const COMMAND_HOME: &str = "home";
/// `/delhome`.
pub const COMMAND_DEL_HOME: &str = "delhome";
/// `/homes`.
pub const COMMAND_HOMES: &str = "homes";
/// `/setwarp`.
pub const COMMAND_SET_WARP: &str = "setwarp";
/// `/warp`.
pub const COMMAND_WARP: &str = "warp";
/// `/delwarp`.
pub const COMMAND_DEL_WARP: &str = "delwarp";
/// `/warps`.
pub const COMMAND_WARPS: &str = "warps";
/// `/setspawn`.
pub const COMMAND_SET_SPAWN: &str = "setspawn";

/// Permission node for `/sethome`.
pub const NODE_SET_HOME: &str = "essentials.sethome";
/// Permission node for `/home`.
pub const NODE_HOME: &str = "essentials.home";
/// Permission node for `/delhome`.
pub const NODE_DEL_HOME: &str = "essentials.delhome";
/// Permission node for `/homes`.
pub const NODE_HOMES: &str = "essentials.homes";
/// Permission node for `/setwarp`.
pub const NODE_SET_WARP: &str = "essentials.setwarp";
/// Permission node for `/warp`.
pub const NODE_WARP: &str = "essentials.warp";
/// Permission node for `/delwarp`.
pub const NODE_DEL_WARP: &str = "essentials.delwarp";
/// Permission node for `/warps`.
pub const NODE_WARPS: &str = "essentials.warps";
/// Permission node for `/setspawn`.
pub const NODE_SET_SPAWN: &str = "essentials.setspawn";

/// The prefix of the numbered home-limit nodes.
pub const NODE_HOME_LIMIT_PREFIX: &str = "essentials.sethome.multiple";
/// A bare `essentials.sethome.multiple` grants an unlimited number of homes.
pub const NODE_MULTIPLE_HOMES: &str = "essentials.sethome.multiple";
/// Alias for an unlimited number of homes.
pub const NODE_UNLIMITED_HOMES: &str = "essentials.sethome.unlimited";

/// The KV key holding every player's homes.
pub const KV_HOMES: &str = "homes";
/// The KV key holding the server warps.
pub const KV_WARPS: &str = "warps";
/// The KV key holding the plugin spawn.
pub const KV_SPAWN: &str = "spawn";

/// The default home name used by `/sethome` and `/home` without an argument.
pub const DEFAULT_HOME: &str = "home";

/// The longest sanitised home/warp name.
pub const MAX_NAME: usize = 32;

/// How many numbered limit nodes a single check probes. A server that wants a
/// larger cap uses the bare `essentials.sethome.multiple`.
const LIMIT_PROBE: usize = 64;

/// How far a player may drift (in blocks) before a warmup is cancelled.
const MOVE_EPSILON: f64 = 0.05;

/// Ticks in a second, for the warmup-seconds spelling.
const TICKS_PER_SECOND: u64 = 20;

/// The manifest declared to the host.
pub fn manifest() -> Manifest {
    Manifest {
        id: ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Essentials Teleport".to_string(),
        // Before cosmetic plugins; there is no competing command owner because
        // `/spawn` is deliberately absent from this list.
        priority: 30,
        capabilities: vec![
            "log".to_string(),
            "config".to_string(),
            "supports".to_string(),
            "storage.kv".to_string(),
            "players".to_string(),
            "players.teleport".to_string(),
            "scheduler".to_string(),
        ],
        subscribe: vec![
            "on-command".to_string(),
            "on-tick".to_string(),
            "after-damage".to_string(),
            "after-entity-death".to_string(),
            "after-player-leave".to_string(),
        ],
        commands: vec![
            detailed(COMMAND_SET_HOME, NODE_SET_HOME),
            detailed(COMMAND_HOME, NODE_HOME),
            detailed(COMMAND_DEL_HOME, NODE_DEL_HOME),
            detailed(COMMAND_HOMES, NODE_HOMES),
            // Creating/removing warps and setting the spawn are admin actions.
            // The command registry node is public (so the core routes the
            // command), while the plugin's own `essentials.*` gate below is an
            // undeclared node: an operator by default, grantable to a non-op.
            dispatch(COMMAND_SET_WARP),
            detailed(COMMAND_WARP, NODE_WARP),
            dispatch(COMMAND_DEL_WARP),
            detailed(COMMAND_WARPS, NODE_WARPS),
            dispatch(COMMAND_SET_SPAWN),
        ],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

fn detailed(name: &str, node: &str) -> CommandDecl {
    CommandDecl::Detailed {
        name: name.to_string(),
        node: node.to_string(),
        op: 0,
    }
}

/// A command the core may route to anyone; the plugin then checks its own
/// `essentials.*` node. Used by the gated admin commands, because the command
/// registry's own op level is a hard floor that a non-op permission grant
/// cannot lower.
fn dispatch(name: &str) -> CommandDecl {
    detailed(name, &format!("essentials.command.{name}"))
}

/// The build-time registration the server links in.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(EssentialsTeleportPlugin::default()),
    }
}

/// A saved world position. The look direction is not persisted: the core
/// teleport API's plugin entry point takes a position only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Location {
    world: String,
    x: f64,
    y: f64,
    z: f64,
}

impl Location {
    fn from_position((world, x, y, z): (String, f64, f64, f64)) -> Location {
        Location { world, x, y, z }
    }

    fn display(&self) -> String {
        format!(
            "{}, {:.1}, {:.1}, {:.1}",
            self.world, self.x, self.y, self.z
        )
    }
}

/// One player's named homes, persisted as one JSON value.
type Homes = BTreeMap<String, BTreeMap<String, Location>>;
/// The server warps, persisted as one JSON value.
type Warps = BTreeMap<String, Location>;

/// Operator-tunable settings.
#[derive(Clone, Debug)]
struct Settings {
    max_homes: usize,
    warmup_ticks: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_homes: 1,
            warmup_ticks: 0,
        }
    }
}

/// A teleport waiting for its warmup to finish.
#[derive(Clone, Debug)]
struct Pending {
    due_tick: u64,
    start: (String, f64, f64, f64),
    location: Location,
    label: String,
}

/// The essentials-teleport plugin.
#[derive(Default)]
pub struct EssentialsTeleportPlugin {
    settings: Settings,
    homes: Homes,
    warps: Warps,
    spawn: Option<Location>,
    pending: HashMap<u64, Pending>,
    now: u64,
}

impl NativePlugin for EssentialsTeleportPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.settings = load_settings(ctx);
        self.homes = load_json(ctx, KV_HOMES)?.unwrap_or_default();
        self.warps = load_json(ctx, KV_WARPS)?.unwrap_or_default();
        self.spawn = load_json(ctx, KV_SPAWN)?;
        self.pending.clear();
        self.now = 0;

        ctx.log(
            LogLevel::Info,
            &format!(
                "essentials-teleport ready: {} home owner(s), {} warp(s), spawn {}",
                self.homes.len(),
                self.warps.len(),
                if self.spawn.is_some() { "set" } else { "unset" }
            ),
        );
        Ok(())
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        let result = match name {
            COMMAND_SET_HOME => self.set_home(ctx, p, args),
            COMMAND_HOME => self.home(ctx, p, args),
            COMMAND_DEL_HOME => self.del_home(ctx, p, args),
            COMMAND_HOMES => self.list_homes(ctx, p, args),
            COMMAND_SET_WARP => self.set_warp(ctx, p, args),
            COMMAND_WARP => self.warp(ctx, p, args),
            COMMAND_DEL_WARP => self.del_warp(ctx, p, args),
            COMMAND_WARPS => self.list_warps(ctx, p, args),
            COMMAND_SET_SPAWN => self.set_spawn(ctx, p, args),
            _ => return None,
        };
        Some(CommandResult {
            handled: true,
            lines: result,
        })
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, n: u64) {
        self.now = n;
        let waiting: Vec<u64> = self.pending.keys().copied().collect();
        for player in waiting {
            let Some(pending) = self.pending.get(&player) else {
                continue;
            };
            if self.has_moved(ctx, player, &pending.start) {
                self.pending.remove(&player);
                notify(ctx, player, "Teleport cancelled: you moved.");
                continue;
            }
            if n < pending.due_tick {
                continue;
            }
            let Some(pending) = self.pending.remove(&player) else {
                continue;
            };
            let lines = self.teleport(ctx, player, &pending.location, &pending.label);
            for line in lines {
                notify(ctx, player, &line);
            }
        }
    }

    fn after_damage(&mut self, ctx: &mut NativeContext, entity: u64, _amount: f64, _source: &str) {
        self.cancel_pending(ctx, entity, "Teleport cancelled: you took damage.");
    }

    fn after_entity_death(&mut self, ctx: &mut NativeContext, entity: u64, _killer: Option<u64>) {
        self.cancel_pending(ctx, entity, "Teleport cancelled: you died.");
    }

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        self.pending.remove(&p.id);
    }
}

impl EssentialsTeleportPlugin {
    /// The plugin spawn, for diagnostics and tests.
    pub fn spawn_point(&self) -> Option<(String, f64, f64, f64)> {
        self.spawn
            .as_ref()
            .map(|l| (l.world.clone(), l.x, l.y, l.z))
    }

    fn set_home(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_SET_HOME) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /sethome [<name>]".to_string()];
        }
        let name = match home_name(args.first()) {
            Ok(name) => name,
            Err(line) => return vec![line],
        };
        let Some(location) = current_location(ctx, p) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let owner = owner_key(ctx, p);
        let existing = self
            .homes
            .get(&owner)
            .is_some_and(|homes| homes.contains_key(&name));
        if !existing {
            let limit = self.home_limit(ctx, p);
            let count = self.homes.get(&owner).map_or(0, BTreeMap::len);
            if count >= limit {
                return vec![format!("You have reached your home limit ({limit}).")];
            }
        }

        let mut next = self.homes.clone();
        next.entry(owner)
            .or_default()
            .insert(name.clone(), location);
        match save_json(ctx, KV_HOMES, &next) {
            Ok(()) => {
                self.homes = next;
                vec![format!("Home `{name}` set.")]
            }
            Err(e) => vec![format!("Could not save home `{name}`: {e}")],
        }
    }

    fn home(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_HOME) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /home [<name>]".to_string()];
        }
        let name = match home_name(args.first()) {
            Ok(name) => name,
            Err(line) => return vec![line],
        };
        let owner = owner_key(ctx, p);
        let Some(location) = self
            .homes
            .get(&owner)
            .and_then(|homes| homes.get(&name))
            .cloned()
        else {
            return vec![format!("You do not have a home named `{name}`.")];
        };
        self.begin_teleport(ctx, p, location, format!("home `{name}`"))
    }

    fn del_home(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_DEL_HOME) {
            return denied;
        }
        // Unlike `/sethome`/`/home`, `/delhome` has no default: deleting the
        // wrong home because an argument was forgotten is not recoverable.
        if args.len() != 1 {
            return vec!["Usage: /delhome <name>".to_string()];
        }
        let name = match sanitize_name(&args[0]) {
            Some(name) => name,
            None => return vec!["Invalid home name.".to_string()],
        };
        let owner = owner_key(ctx, p);
        let mut next = self.homes.clone();
        let removed = next.get_mut(&owner).and_then(|homes| homes.remove(&name));
        if removed.is_none() {
            return vec![format!("You do not have a home named `{name}`.")];
        }
        if next.get(&owner).is_some_and(BTreeMap::is_empty) {
            next.remove(&owner);
        }
        match save_json(ctx, KV_HOMES, &next) {
            Ok(()) => {
                self.homes = next;
                vec![format!("Home `{name}` deleted.")]
            }
            Err(e) => vec![format!("Could not delete home `{name}`: {e}")],
        }
    }

    fn list_homes(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_HOMES) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /homes".to_string()];
        }
        let owner = owner_key(ctx, p);
        let Some(homes) = self.homes.get(&owner) else {
            return vec!["You have no homes.".to_string()];
        };
        let limit = self.home_limit(ctx, p);
        let cap = if limit == usize::MAX {
            "unlimited".to_string()
        } else {
            limit.to_string()
        };
        let names = homes.keys().cloned().collect::<Vec<_>>().join(", ");
        vec![format!("Homes ({}/{}): {names}", homes.len(), cap)]
    }

    fn set_warp(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_SET_WARP) {
            return denied;
        }
        if args.len() != 1 {
            return vec!["Usage: /setwarp <name>".to_string()];
        }
        let name = match sanitize_name(&args[0]) {
            Some(name) => name,
            None => return vec!["Invalid warp name.".to_string()],
        };
        let Some(location) = current_location(ctx, p) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let mut next = self.warps.clone();
        next.insert(name.clone(), location);
        match save_json(ctx, KV_WARPS, &next) {
            Ok(()) => {
                self.warps = next;
                vec![format!("Warp `{name}` set.")]
            }
            Err(e) => vec![format!("Could not save warp `{name}`: {e}")],
        }
    }

    fn warp(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_WARP) {
            return denied;
        }
        if args.len() != 1 {
            return vec!["Usage: /warp <name>".to_string()];
        }
        let Some(name) = sanitize_name(&args[0]) else {
            return vec!["Invalid warp name.".to_string()];
        };
        let Some(location) = self.warps.get(&name).cloned() else {
            return vec![format!("There is no warp named `{name}`.")];
        };
        self.begin_teleport(ctx, p, location, format!("warp `{name}`"))
    }

    fn del_warp(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_DEL_WARP) {
            return denied;
        }
        if args.len() != 1 {
            return vec!["Usage: /delwarp <name>".to_string()];
        }
        let Some(name) = sanitize_name(&args[0]) else {
            return vec!["Invalid warp name.".to_string()];
        };
        let mut next = self.warps.clone();
        if next.remove(&name).is_none() {
            return vec![format!("There is no warp named `{name}`.")];
        }
        match save_json(ctx, KV_WARPS, &next) {
            Ok(()) => {
                self.warps = next;
                vec![format!("Warp `{name}` deleted.")]
            }
            Err(e) => vec![format!("Could not delete warp `{name}`: {e}")],
        }
    }

    fn list_warps(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_WARPS) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /warps".to_string()];
        }
        if self.warps.is_empty() {
            return vec!["There are no warps.".to_string()];
        }
        let names = self.warps.keys().cloned().collect::<Vec<_>>().join(", ");
        vec![format!("Warps ({}): {names}", self.warps.len())]
    }

    fn set_spawn(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_SET_SPAWN) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /setspawn".to_string()];
        }
        let Some(location) = current_location(ctx, p) else {
            return vec!["Your current position is not available.".to_string()];
        };
        match save_json(ctx, KV_SPAWN, &location) {
            Ok(()) => {
                let display = location.display();
                self.spawn = Some(location);
                vec![format!("Plugin spawn set to {display}.")]
            }
            Err(e) => vec![format!("Could not save the plugin spawn: {e}")],
        }
    }

    fn begin_teleport(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        location: Location,
        label: String,
    ) -> Vec<String> {
        if self.settings.warmup_ticks == 0 {
            return self.teleport(ctx, p.id, &location, &label);
        }
        let Some(start) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let due_tick = self.now.saturating_add(self.settings.warmup_ticks);
        let seconds = self.settings.warmup_ticks.div_ceil(TICKS_PER_SECOND).max(1);
        self.pending.insert(
            p.id,
            Pending {
                due_tick,
                start,
                location,
                label: label.clone(),
            },
        );
        vec![format!(
            "Teleporting to {label} in {seconds} second(s). Do not move or take damage."
        )]
    }

    fn teleport(
        &self,
        ctx: &NativeContext,
        player: u64,
        location: &Location,
        label: &str,
    ) -> Vec<String> {
        if let Some((world, _, _, _)) = ctx.backend.player_position(ctx.id, player) {
            if world != location.world {
                return vec![format!(
                    "Teleport to {label} failed: it is in `{}`, not `{world}`; cross-world teleport is not available.",
                    location.world
                )];
            }
        }
        match ctx.backend.teleport_player(
            ctx.id,
            player,
            location.x,
            location.y,
            location.z,
            TeleportCause::Plugin,
        ) {
            Ok(()) => vec![format!("Teleported to {label}.")],
            Err(e) => vec![format!("Teleport to {label} failed: {e}.")],
        }
    }

    fn has_moved(&self, ctx: &NativeContext, player: u64, start: &(String, f64, f64, f64)) -> bool {
        let Some((world, x, y, z)) = ctx.backend.player_position(ctx.id, player) else {
            return false;
        };
        world != start.0
            || (x - start.1).abs() > MOVE_EPSILON
            || (y - start.2).abs() > MOVE_EPSILON
            || (z - start.3).abs() > MOVE_EPSILON
    }

    fn cancel_pending(&mut self, ctx: &mut NativeContext, player: u64, message: &str) {
        if self.pending.remove(&player).is_some() {
            notify(ctx, player, message);
        }
    }

    fn home_limit(&self, ctx: &NativeContext, p: &Player) -> usize {
        if permitted(ctx, p, NODE_MULTIPLE_HOMES) || permitted(ctx, p, NODE_UNLIMITED_HOMES) {
            return usize::MAX;
        }
        let mut limit = self.settings.max_homes;
        for n in 1..=LIMIT_PROBE {
            let node = format!("{NODE_HOME_LIMIT_PREFIX}.{n}");
            if permitted(ctx, p, &node) {
                limit = limit.max(n);
            }
        }
        limit
    }
}

fn load_settings(ctx: &mut NativeContext) -> Settings {
    let mut settings = Settings::default();

    if let Some((key, raw)) = config_first(ctx, &["max-homes", "homes-limit"]) {
        match raw.trim().parse::<usize>() {
            Ok(value) => settings.max_homes = value,
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a non-negative whole number; using {}",
                    settings.max_homes
                ),
            ),
        }
    }

    if let Some(raw) = config_value(ctx, "warmup-ticks") {
        match raw.trim().parse::<u64>() {
            Ok(value) => settings.warmup_ticks = value,
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `warmup-ticks` value `{raw}`: expected a non-negative whole number; using {}",
                    settings.warmup_ticks
                ),
            ),
        }
    } else if let Some((key, raw)) = config_first(ctx, &["warmup", "warmup-seconds"]) {
        match raw.trim().parse::<u64>() {
            Ok(seconds) => {
                settings.warmup_ticks = seconds.saturating_mul(TICKS_PER_SECOND);
            }
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a non-negative whole number of seconds; using {} tick(s)",
                    settings.warmup_ticks
                ),
            ),
        }
    }

    settings
}

fn config_value(ctx: &NativeContext, key: &str) -> Option<String> {
    ctx.config
        .get(key)
        .cloned()
        .or_else(|| ctx.backend.config(ctx.id, key))
}

fn config_first(ctx: &NativeContext, keys: &[&str]) -> Option<(String, String)> {
    keys.iter()
        .find_map(|key| config_value(ctx, key).map(|value| ((*key).to_string(), value)))
}

fn load_json<T: DeserializeOwned>(ctx: &NativeContext, key: &str) -> Result<Option<T>, String> {
    match ctx.backend.kv_try_get(ctx.id, key) {
        Ok(None) => Ok(None),
        Ok(Some(bytes)) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("cannot read plugin KV `{key}`: {e}")),
        Err(e) => Err(format!("cannot read plugin KV `{key}`: {e}")),
    }
}

fn save_json<T: Serialize>(ctx: &NativeContext, key: &str, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    ctx.backend.kv_try_put(ctx.id, key, &bytes)
}

fn current_location(ctx: &NativeContext, p: &Player) -> Option<Location> {
    ctx.backend
        .player_position(ctx.id, p.id)
        .map(Location::from_position)
}

fn owner_key(ctx: &NativeContext, p: &Player) -> String {
    ctx.backend
        .player_uuid(ctx.id, p.id)
        .map(|uuid| format!("uuid:{uuid:032x}"))
        .unwrap_or_else(|| format!("name:{}", p.name.to_lowercase()))
}

/// A home name: the default when absent, otherwise the sanitised argument.
fn home_name(raw: Option<&String>) -> Result<String, String> {
    match raw {
        None => Ok(DEFAULT_HOME.to_string()),
        Some(raw) => sanitize_name(raw).ok_or_else(|| "Invalid home name.".to_string()),
    }
}

/// Lower-case a name, turn every run of other characters into one `_`, cap the
/// length and trim separators. The result is **idempotent**: sanitising it
/// again yields the same name, so every stored key can be typed back into
/// `/warp`/`/delwarp` or `/home`/`/delhome` and found. Trimming happens *after*
/// the cut, otherwise a name whose 32nd character is `_` would be stored with
/// a trailing separator that a later lookup removes.
///
/// Names are single command tokens: the server splits plugin-command arguments
/// on whitespace, so a space-containing name cannot be entered in game. The
/// sanitiser still handles them for API callers and for stored keys.
fn sanitize_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut separator = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            separator = false;
        } else if !out.is_empty() && !separator {
            out.push('_');
            separator = true;
        }
    }
    let capped = out
        .trim_matches(|c| c == '_' || c == '-')
        .chars()
        .take(MAX_NAME)
        .collect::<String>();
    let capped = capped.trim_matches(|c| c == '_' || c == '-');
    if capped.is_empty() {
        return None;
    }
    Some(capped.to_string())
}

fn permitted(ctx: &NativeContext, p: &Player, node: &str) -> bool {
    p.operator || ctx.backend.permission_check(ctx.id, p.id, node, 0)
}

fn require(ctx: &NativeContext, p: &Player, node: &str) -> Option<Vec<String>> {
    (!permitted(ctx, p, node)).then(|| vec![format!("You do not have permission ({node}).")])
}

fn notify(ctx: &NativeContext, player: u64, message: &str) {
    ctx.backend.send_message(ctx.id, player, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitised_deterministically() {
        assert_eq!(sanitize_name("  My Warp!! ").as_deref(), Some("my_warp"));
        assert_eq!(sanitize_name("HOME").as_deref(), Some("home"));
        assert_eq!(sanitize_name("a__b").as_deref(), Some("a_b"));
        assert_eq!(sanitize_name("!!!"), None);
        assert_eq!(sanitize_name(""), None);
        assert_eq!(sanitize_name(&"x".repeat(80)).unwrap().len(), MAX_NAME);
    }

    #[test]
    fn a_name_cut_on_a_separator_is_still_round_trippable() {
        // 31 letters then `-b`: the 32nd sanitised character is `_`, which a
        // trim-before-cut implementation would store and a later lookup would
        // remove, so `/warps` would list a name `/warp` cannot find.
        let raw = format!("{}-b", "a".repeat(31));
        let stored = sanitize_name(&raw).expect("a valid name");
        assert_eq!(stored, "a".repeat(31), "the cut must not leave a `_`");
        assert_eq!(
            sanitize_name(&stored).as_deref(),
            Some(stored.as_str()),
            "sanitising a sanitised name must be a fixed point"
        );
        // Truncation can also land on an internal separator run; still stable.
        let raw = format!("{}-{}", "b".repeat(40), "c".repeat(40));
        assert_eq!(
            sanitize_name(&sanitize_name(&raw).unwrap()),
            sanitize_name(&raw)
        );
    }

    #[test]
    fn the_manifest_never_claims_spawn() {
        let manifest = manifest();
        assert!(!manifest.serves_command("spawn"));
        assert!(manifest.serves_command(COMMAND_SET_SPAWN));
        for name in [
            COMMAND_SET_HOME,
            COMMAND_HOME,
            COMMAND_DEL_HOME,
            COMMAND_HOMES,
            COMMAND_SET_WARP,
            COMMAND_WARP,
            COMMAND_DEL_WARP,
            COMMAND_WARPS,
            COMMAND_SET_SPAWN,
        ] {
            assert!(manifest.serves_command(name), "{name}");
        }
    }
}
