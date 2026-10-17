//! The EssentialsX-style **teleport** plugin: homes, warps, requests, `/back`,
//! vertical helpers and random teleport (CMD-37a/b).
//!
//! A native (tier N) plugin: it holds no gameplay mechanism of its own. Every
//! move goes through the core teleport service (CMD-31) via
//! [`HostBackend::teleport_player`], which searches for a safe spot, so a home,
//! warp, request or coordinate above lava or in the void cannot drop a player
//! into it. `/top`, `/jump`, `/up` and `/rtp` call the same core search through
//! the `world.read` host hooks instead of re-deriving collision.
//! Homes, warps, the plugin spawn and `/back` locations live in plugin KV (P1)
//! and are replayed on `init`, so they survive a restart.
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
//! /tpa <player>       ask to teleport to another player
//! /tpahere <player>   ask another player to teleport to you
//! /tpaccept [<name>]  accept the newest (or a named) incoming request
//! /tpdeny [<name>]    deny every (or one) incoming request
//! /tpcancel           cancel your outgoing requests
//! /tpall              move every online player to you
//! /tphere <player>    move one player to you
//! /tppos <x> <y> <z>  move to coordinates (`~` is relative)
//! /back               return to the last death or pre-teleport position
//! /top                highest safe spot in your column
//! /jump               safe spot above the block you are looking at
//! /up [<n>]           climb up to n blocks, stopping at the first blockage
//! /rtp [<radius>]     random safe spot from CMD-31's search
//! ```
//!
//! **`/spawn` stays core.** This plugin never registers or overrides it; the
//! umbrella brief's ownership rule reserves vanilla/core names for the engine.
//! `/setspawn` only records the point the core (or a future spawn provider)
//! may read.
//!
//! ## Requests and `/back`
//!
//! Requests are per-target and per-requester, expire after
//! `request-timeout` and are dropped when either side leaves. `/tpaccept`
//! without a name takes the newest; with a name it takes that requester's.
//! `/back` is written on death and before every successful teleport, persists
//! in KV, and toggles between the last two positions when used repeatedly.
//!
//! ## Cooldowns
//!
//! `cooldown`/`cooldown-ticks` is a per-player, per-command delay; a command's
//! cooldown and warmup are both bypassed by `essentials.<command>.bypass`.
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
//! Homes and back locations are keyed by the player's UUID when the host exposes one, otherwise
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
/// `/tpa`.
pub const COMMAND_TPA: &str = "tpa";
/// `/tpahere`.
pub const COMMAND_TPA_HERE: &str = "tpahere";
/// `/tpaccept`.
pub const COMMAND_TP_ACCEPT: &str = "tpaccept";
/// `/tpdeny`.
pub const COMMAND_TP_DENY: &str = "tpdeny";
/// `/tpcancel`.
pub const COMMAND_TP_CANCEL: &str = "tpcancel";
/// `/tpall`.
pub const COMMAND_TP_ALL: &str = "tpall";
/// `/tphere`.
pub const COMMAND_TP_HERE: &str = "tphere";
/// `/tppos`.
pub const COMMAND_TP_POS: &str = "tppos";
/// `/back`.
pub const COMMAND_BACK: &str = "back";
/// `/top`.
pub const COMMAND_TOP: &str = "top";
/// `/jump`.
pub const COMMAND_JUMP: &str = "jump";
/// `/up`.
pub const COMMAND_UP: &str = "up";
/// `/rtp`.
pub const COMMAND_RTP: &str = "rtp";

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
/// Permission node for `/tpa`.
pub const NODE_TPA: &str = "essentials.tpa";
/// Permission node for `/tpahere`.
pub const NODE_TPA_HERE: &str = "essentials.tpahere";
/// Permission node for `/tpaccept`.
pub const NODE_TP_ACCEPT: &str = "essentials.tpaccept";
/// Permission node for `/tpdeny`.
pub const NODE_TP_DENY: &str = "essentials.tpdeny";
/// Permission node for `/tpcancel`.
pub const NODE_TP_CANCEL: &str = "essentials.tpcancel";
/// Permission node for `/tpall`.
pub const NODE_TP_ALL: &str = "essentials.tpall";
/// Permission node for `/tphere`.
pub const NODE_TP_HERE: &str = "essentials.tphere";
/// Permission node for `/tppos`.
pub const NODE_TP_POS: &str = "essentials.tppos";
/// Permission node for `/back`.
pub const NODE_BACK: &str = "essentials.back";
/// Permission node for `/top`.
pub const NODE_TOP: &str = "essentials.top";
/// Permission node for `/jump`.
pub const NODE_JUMP: &str = "essentials.jump";
/// Permission node for `/up`.
pub const NODE_UP: &str = "essentials.up";
/// Permission node for `/rtp`.
pub const NODE_RTP: &str = "essentials.rtp";

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
/// The KV key holding every player's last `/back` location.
pub const KV_BACK: &str = "back";

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

/// Default request lifetime: 60 seconds, as EssentialsX.
const DEFAULT_REQUEST_TIMEOUT_TICKS: u64 = 60 * TICKS_PER_SECOND;

/// Default `/rtp` radius.
const DEFAULT_RTP_RADIUS: u32 = 1000;

/// Default `/up` limit.
const DEFAULT_MAX_UP: u32 = 64;

/// Default `/jump` reach, in blocks, as EssentialsX's target-block range.
const DEFAULT_JUMP_DISTANCE: f64 = 20.0;

/// How far `/jump` advances along its look ray per sample.
const JUMP_STEP: f64 = 0.1;

/// The eye height above the feet, for the `/jump` ray.
const EYE_HEIGHT: f64 = 1.62;

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
            "world.read".to_string(),
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
            detailed(COMMAND_TPA, NODE_TPA),
            detailed(COMMAND_TPA_HERE, NODE_TPA_HERE),
            detailed(COMMAND_TP_ACCEPT, NODE_TP_ACCEPT),
            detailed(COMMAND_TP_DENY, NODE_TP_DENY),
            detailed(COMMAND_TP_CANCEL, NODE_TP_CANCEL),
            detailed(COMMAND_BACK, NODE_BACK),
            detailed(COMMAND_TOP, NODE_TOP),
            detailed(COMMAND_JUMP, NODE_JUMP),
            detailed(COMMAND_UP, NODE_UP),
            detailed(COMMAND_RTP, NODE_RTP),
            // Operator moves: the core routes the public command name, then
            // the plugin checks the `essentials.*` node itself (a non-op grant
            // cannot lower a command-registry op floor).
            dispatch(COMMAND_TP_ALL),
            dispatch(COMMAND_TP_HERE),
            dispatch(COMMAND_TP_POS),
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
    request_timeout_ticks: u64,
    cooldown_ticks: u64,
    rtp_radius: u32,
    max_up: u32,
    jump_distance: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_homes: 1,
            warmup_ticks: 0,
            request_timeout_ticks: DEFAULT_REQUEST_TIMEOUT_TICKS,
            cooldown_ticks: 0,
            rtp_radius: DEFAULT_RTP_RADIUS,
            max_up: DEFAULT_MAX_UP,
            jump_distance: DEFAULT_JUMP_DISTANCE,
        }
    }
}

/// One pending teleport request.
#[derive(Clone, Debug)]
struct Request {
    requester: u64,
    target: u64,
    requester_name: String,
    target_name: String,
    /// `true` for `/tpahere`: accepting moves the target to the requester.
    here: bool,
    created_tick: u64,
    due_tick: u64,
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
    /// Every player's last death or pre-teleport position, keyed like homes.
    back: BTreeMap<String, Location>,
    pending: HashMap<u64, Pending>,
    /// Keyed by `(requester, target)`; one live request per pair.
    requests: BTreeMap<(u64, u64), Request>,
    /// `(player, command)` -> the tick its cooldown was armed.
    last_use: HashMap<(u64, &'static str), u64>,
    now: u64,
}

impl NativePlugin for EssentialsTeleportPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.settings = load_settings(ctx);
        self.homes = load_json(ctx, KV_HOMES)?.unwrap_or_default();
        self.warps = load_json(ctx, KV_WARPS)?.unwrap_or_default();
        self.spawn = load_json(ctx, KV_SPAWN)?;
        self.back = load_json(ctx, KV_BACK)?.unwrap_or_default();
        self.pending.clear();
        self.requests.clear();
        self.last_use.clear();
        self.now = 0;

        ctx.log(
            LogLevel::Info,
            &format!(
                "essentials-teleport ready: {} home owner(s), {} warp(s), {} back location(s), spawn {}",
                self.homes.len(),
                self.warps.len(),
                self.back.len(),
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
            COMMAND_TPA => self.tpa(ctx, p, args, false),
            COMMAND_TPA_HERE => self.tpa(ctx, p, args, true),
            COMMAND_TP_ACCEPT => self.tp_accept(ctx, p, args),
            COMMAND_TP_DENY => self.tp_deny(ctx, p, args),
            COMMAND_TP_CANCEL => self.tp_cancel(ctx, p, args),
            COMMAND_TP_ALL => self.tp_all(ctx, p, args),
            COMMAND_TP_HERE => self.tp_here(ctx, p, args),
            COMMAND_TP_POS => self.tp_pos(ctx, p, args),
            COMMAND_BACK => self.back(ctx, p, args),
            COMMAND_TOP => self.top(ctx, p, args),
            COMMAND_JUMP => self.jump(ctx, p, args),
            COMMAND_UP => self.up(ctx, p, args),
            COMMAND_RTP => self.rtp(ctx, p, args),
            _ => return None,
        };
        Some(CommandResult {
            handled: true,
            lines: result,
        })
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, n: u64) {
        self.now = n;
        self.expire_requests(ctx, n);
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
            let (_, lines) = self.teleport(ctx, player, &pending.location, &pending.label);
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
        // A death is a `/back` target. Non-player entities answer `None` from
        // `online_by_id`, so a mob death never writes a player record.
        if self.online_by_id(ctx, entity).is_some() {
            if let Some(location) = location_of(ctx, entity) {
                self.remember_back(ctx, entity, location);
            }
        }
    }

    fn after_player_leave(&mut self, ctx: &mut NativeContext, p: &Player) {
        self.pending.remove(&p.id);
        self.drop_player_requests(ctx, p, "left the server");
        self.last_use.retain(|(player, _), _| *player != p.id);
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
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_HOME) {
            return lines;
        }
        self.begin_teleport(ctx, p, location, format!("home `{name}`"), COMMAND_HOME)
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
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_WARP) {
            return lines;
        }
        self.begin_teleport(ctx, p, location, format!("warp `{name}`"), COMMAND_WARP)
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

    fn tpa(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
        here: bool,
    ) -> Vec<String> {
        let node = if here { NODE_TPA_HERE } else { NODE_TPA };
        if let Some(denied) = require(ctx, p, node) {
            return denied;
        }
        if args.len() != 1 {
            let usage = if here {
                "Usage: /tpahere <player>"
            } else {
                "Usage: /tpa <player>"
            };
            return vec![usage.to_string()];
        }
        let Some(target) = self.online_by_name(ctx, &args[0]) else {
            return vec![format!("`{}` is not online.", args[0])];
        };
        if target.id == p.id {
            return vec!["You cannot send a teleport request to yourself.".to_string()];
        }
        if ctx.backend.player_ignores(ctx.id, target.id, p.id) {
            return vec![format!("{} is ignoring teleport requests.", target.name)];
        }
        let command = if here { COMMAND_TPA_HERE } else { COMMAND_TPA };
        if let Some(lines) = self.cooldown(ctx, p, command) {
            return lines;
        }

        let due_tick = if self.settings.request_timeout_ticks == 0 {
            u64::MAX
        } else {
            self.now.saturating_add(self.settings.request_timeout_ticks)
        };
        let replaced = self
            .requests
            .insert(
                (p.id, target.id),
                Request {
                    requester: p.id,
                    target: target.id,
                    requester_name: p.name.clone(),
                    target_name: target.name.clone(),
                    here,
                    created_tick: self.now,
                    due_tick,
                },
            )
            .is_some();

        if here {
            notify(
                ctx,
                target.id,
                &format!(
                    "{} has requested that you teleport to them. Use /tpaccept or /tpdeny.",
                    p.name
                ),
            );
        } else {
            notify(
                ctx,
                target.id,
                &format!(
                    "{} has requested to teleport to you. Use /tpaccept or /tpdeny.",
                    p.name
                ),
            );
        }
        let verb = if replaced { "updated" } else { "sent" };
        vec![format!("Teleport request {verb} to {}.", target.name)]
    }

    fn tp_accept(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_ACCEPT) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /tpaccept [<player>]".to_string()];
        }
        let incoming: Vec<Request> = self
            .requests
            .values()
            .filter(|request| request.target == p.id)
            .cloned()
            .collect();
        let selected = match args.first() {
            Some(name) => incoming
                .into_iter()
                .find(|request| request.requester_name.eq_ignore_ascii_case(name)),
            None => incoming
                .into_iter()
                .max_by_key(|request| request.created_tick),
        };
        let Some(request) = selected else {
            let detail = match args.first() {
                Some(name) => format!(" from `{name}`"),
                None => String::new(),
            };
            return vec![format!("You have no pending teleport request{detail}.")];
        };
        self.requests.remove(&(request.requester, request.target));

        let Some(requester) = self.online_by_id(ctx, request.requester) else {
            return vec![format!("{} is no longer online.", request.requester_name)];
        };
        if request.here {
            // `/tpahere`: the target (the accepting caller) moves to the
            // requester's current position.
            let Some(destination) = location_of(ctx, requester.id) else {
                return vec!["That player's position is not available.".to_string()];
            };
            notify(
                ctx,
                requester.id,
                &format!("{} accepted your teleport request.", p.name),
            );
            let mut lines = vec![format!("Accepted {}'s teleport request.", requester.name)];
            lines.extend(self.begin_teleport(
                ctx,
                p,
                destination,
                format!("{}'s location", requester.name),
                COMMAND_TPA_HERE,
            ));
            lines
        } else {
            // `/tpa`: the requester moves to the target (the accepting caller).
            let Some(destination) = location_of(ctx, p.id) else {
                return vec!["Your current position is not available.".to_string()];
            };
            notify(
                ctx,
                requester.id,
                &format!("{} accepted your teleport request.", p.name),
            );
            let lines = self.begin_teleport(
                ctx,
                &requester,
                destination,
                format!("{}'s location", p.name),
                COMMAND_TPA,
            );
            for line in lines {
                notify(ctx, requester.id, &line);
            }
            vec![format!("Accepted {}'s teleport request.", requester.name)]
        }
    }

    fn tp_deny(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_DENY) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /tpdeny [<player>]".to_string()];
        }
        let mut denied: Vec<Request> = self
            .requests
            .values()
            .filter(|request| request.target == p.id)
            .filter(|request| match args.first() {
                Some(name) => request.requester_name.eq_ignore_ascii_case(name),
                None => true,
            })
            .cloned()
            .collect();
        if denied.is_empty() {
            let detail = match args.first() {
                Some(name) => format!(" from `{name}`"),
                None => String::new(),
            };
            return vec![format!("You have no pending teleport request{detail}.")];
        }
        denied.sort_by_key(|request| request.created_tick);
        for request in &denied {
            self.requests.remove(&(request.requester, request.target));
            notify(
                ctx,
                request.requester,
                &format!("{} denied your teleport request.", p.name),
            );
        }
        if !args.is_empty() {
            let name = &denied[0].requester_name;
            vec![format!("Denied {name}'s teleport request.")]
        } else {
            vec![format!("Denied {} teleport request(s).", denied.len())]
        }
    }

    fn tp_cancel(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_CANCEL) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /tpcancel".to_string()];
        }
        let outgoing: Vec<Request> = self
            .requests
            .values()
            .filter(|request| request.requester == p.id)
            .cloned()
            .collect();
        if outgoing.is_empty() {
            return vec!["You have no outgoing teleport requests.".to_string()];
        }
        for request in &outgoing {
            self.requests.remove(&(request.requester, request.target));
            notify(
                ctx,
                request.target,
                &format!("{} cancelled their teleport request.", p.name),
            );
        }
        vec![format!("Cancelled {} teleport request(s).", outgoing.len())]
    }

    fn tp_all(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_ALL) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /tpall".to_string()];
        }
        let Some(destination) = location_of(ctx, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let targets = self.online_players(ctx);
        let mut moved = 0usize;
        for target in targets {
            if target.id == p.id {
                continue;
            }
            let (ok, lines) = self.teleport(ctx, target.id, &destination, p.name.as_str());
            if ok {
                moved += 1;
            }
            for line in lines {
                notify(ctx, target.id, &line);
            }
        }
        vec![format!("Teleported {moved} player(s) to you.")]
    }

    fn tp_here(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_HERE) {
            return denied;
        }
        if args.len() != 1 {
            return vec!["Usage: /tphere <player>".to_string()];
        }
        let Some(target) = self.online_by_name(ctx, &args[0]) else {
            return vec![format!("`{}` is not online.", args[0])];
        };
        if target.id == p.id {
            return vec!["You cannot teleport yourself.".to_string()];
        }
        let Some(destination) = location_of(ctx, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let lines = self.begin_teleport(
            ctx,
            &target,
            destination,
            format!("{}'s location", p.name),
            COMMAND_TP_HERE,
        );
        for line in lines {
            notify(ctx, target.id, &line);
        }
        vec![format!("Teleporting {} to you.", target.name)]
    }

    fn tp_pos(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TP_POS) {
            return denied;
        }
        if args.len() != 3 {
            return vec!["Usage: /tppos <x> <y> <z>".to_string()];
        }
        let Some((world, cx, cy, cz)) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let (Some(x), Some(y), Some(z)) = (
            parse_coordinate(&args[0], cx),
            parse_coordinate(&args[1], cy),
            parse_coordinate(&args[2], cz),
        ) else {
            return vec!["Coordinates must be numbers (or `~` / `~<offset>`).".to_string()];
        };
        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
            return vec!["Coordinates must be finite numbers.".to_string()];
        }
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_TP_POS) {
            return lines;
        }
        let location = Location { world, x, y, z };
        self.begin_teleport(
            ctx,
            p,
            location,
            format!("{}, {}, {}", args[0], args[1], args[2]),
            COMMAND_TP_POS,
        )
    }

    fn back(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_BACK) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /back".to_string()];
        }
        let owner = owner_key(ctx, p);
        let Some(location) = self.back.get(&owner).cloned() else {
            return vec!["You have no previous location.".to_string()];
        };
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_BACK) {
            return lines;
        }
        self.begin_teleport(
            ctx,
            p,
            location,
            "your previous location".to_string(),
            COMMAND_BACK,
        )
    }

    fn top(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_TOP) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /top".to_string()];
        }
        let Some((world, x, _y, z)) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let Some((_min_y, max_y)) = ctx.backend.world_height(ctx.id) else {
            return vec!["The world height is not available.".to_string()];
        };
        let around = (x.floor() as i32, max_y, z.floor() as i32);
        let Some((sx, sy, sz)) = ctx.backend.find_safe_spot(ctx.id, around, 0, false) else {
            return vec!["No safe spot was found above you.".to_string()];
        };
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_TOP) {
            return lines;
        }
        self.begin_teleport(
            ctx,
            p,
            Location {
                world,
                x: sx as f64,
                y: sy as f64,
                z: sz as f64,
            },
            "the top".to_string(),
            COMMAND_TOP,
        )
    }

    fn jump(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_JUMP) {
            return denied;
        }
        if !args.is_empty() {
            return vec!["Usage: /jump".to_string()];
        }
        let Some((world, x, y, z)) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let Some((yaw, pitch)) = ctx.backend.player_look(ctx.id, p.id) else {
            return vec!["Your facing is not available.".to_string()];
        };
        let yaw = (yaw as f64).to_radians();
        let pitch = (pitch as f64).to_radians();
        let direction = (
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        // `player_position` is the entity's feet, not a block corner; a
        // `/jump` ray must start exactly there.
        let start = (x, y + EYE_HEIGHT, z);
        let steps = (self.settings.jump_distance.max(1.0) / JUMP_STEP).ceil() as u32;
        let mut hit = None;
        for step in 1..=steps {
            let distance = step as f64 * JUMP_STEP;
            let bx = (start.0 + direction.0 * distance).floor() as i32;
            let by = (start.1 + direction.1 * distance).floor() as i32;
            let bz = (start.2 + direction.2 * distance).floor() as i32;
            let Some(block) = ctx.backend.world_block(ctx.id, bx, by, bz) else {
                return vec!["The world is not available.".to_string()];
            };
            if block.solid {
                hit = Some((bx, by, bz));
                break;
            }
        }
        let Some((bx, by, bz)) = hit else {
            return vec!["There is no block ahead to jump to.".to_string()];
        };
        let Some((sx, sy, sz)) = ctx
            .backend
            .find_safe_spot(ctx.id, (bx, by + 1, bz), 1, false)
        else {
            return vec!["No safe spot was found ahead.".to_string()];
        };
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_JUMP) {
            return lines;
        }
        self.begin_teleport(
            ctx,
            p,
            Location {
                world,
                x: sx as f64,
                y: sy as f64,
                z: sz as f64,
            },
            "the block ahead".to_string(),
            COMMAND_JUMP,
        )
    }

    fn up(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_UP) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /up [<n>]".to_string()];
        }
        let n = match args.first() {
            Some(raw) => match raw.parse::<u32>() {
                Ok(0) => return vec!["Usage: /up [<n>] with n >= 1".to_string()],
                Ok(n) => n,
                Err(_) => return vec!["Usage: /up [<n>]".to_string()],
            },
            None => 1,
        };
        if n > self.settings.max_up {
            return vec![format!(
                "You can move up at most {} block(s) at a time.",
                self.settings.max_up
            )];
        }
        let Some((world, x, y, z)) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let Some((_min_y, max_y)) = ctx.backend.world_height(ctx.id) else {
            return vec!["The world height is not available.".to_string()];
        };
        let (bx, by, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let mut best = 0u32;
        for step in 1..=n {
            let feet_y = by.saturating_add(step as i32);
            if feet_y.saturating_add(1) > max_y {
                break;
            }
            let Some(feet) = ctx.backend.world_block(ctx.id, bx, feet_y, bz) else {
                return vec!["The world is not available.".to_string()];
            };
            let Some(head) = ctx.backend.world_block(ctx.id, bx, feet_y + 1, bz) else {
                return vec!["The world is not available.".to_string()];
            };
            if feet.solid || feet.hazard || head.solid || head.hazard {
                break;
            }
            best = step;
        }
        if best == 0 {
            return vec!["There is no room to move up.".to_string()];
        }
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_UP) {
            return lines;
        }
        self.begin_teleport(
            ctx,
            p,
            Location {
                world,
                x: bx as f64,
                y: by.saturating_add(best as i32) as f64,
                z: bz as f64,
            },
            format!("up {best} block(s)"),
            COMMAND_UP,
        )
    }

    fn rtp(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if let Some(denied) = require(ctx, p, NODE_RTP) {
            return denied;
        }
        if args.len() > 1 {
            return vec!["Usage: /rtp [<radius>]".to_string()];
        }
        let max_radius = self.settings.rtp_radius.max(1);
        let radius = match args.first() {
            Some(raw) => match raw.parse::<u32>() {
                Ok(radius) if radius > 0 && radius <= max_radius => radius,
                Ok(_) => return vec![format!("The radius must be between 1 and {max_radius}.")],
                Err(_) => return vec!["Usage: /rtp [<radius>]".to_string()],
            },
            None => max_radius,
        };
        let Some((world, x, y, z)) = ctx.backend.player_position(ctx.id, p.id) else {
            return vec!["Your current position is not available.".to_string()];
        };
        let center = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let Some((sx, sy, sz)) = ctx.backend.random_safe_spot(ctx.id, center, 1, radius) else {
            return vec![format!("No safe spot was found within {radius} block(s).")];
        };
        if let Some(lines) = self.cooldown(ctx, p, COMMAND_RTP) {
            return lines;
        }
        self.begin_teleport(
            ctx,
            p,
            Location {
                world,
                x: sx as f64,
                y: sy as f64,
                z: sz as f64,
            },
            "a random location".to_string(),
            COMMAND_RTP,
        )
    }

    fn begin_teleport(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        location: Location,
        label: String,
        command: &'static str,
    ) -> Vec<String> {
        let bypass = permitted(ctx, p, &format!("essentials.{command}.bypass"));
        if self.settings.warmup_ticks == 0 || bypass {
            let (_, lines) = self.teleport(ctx, p.id, &location, &label);
            return lines;
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

    /// Perform one move through CMD-31 and remember where the player was, so
    /// `/back` can return there. The return value is `(moved, lines)`; an
    /// operator command that moves several players uses the flag to count.
    fn teleport(
        &mut self,
        ctx: &mut NativeContext,
        player: u64,
        location: &Location,
        label: &str,
    ) -> (bool, Vec<String>) {
        let from = ctx.backend.player_position(ctx.id, player);
        if let Some((world, _, _, _)) = &from {
            if *world != location.world {
                return (
                    false,
                    vec![format!(
                        "Teleport to {label} failed: it is in `{}`, not `{world}`; cross-world teleport is not available.",
                        location.world
                    )],
                );
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
            Ok(()) => {
                if let Some(from) = from {
                    self.remember_back(ctx, player, Location::from_position(from));
                }
                (true, vec![format!("Teleported to {label}.")])
            }
            Err(e) => (false, vec![format!("Teleport to {label} failed: {e}.")]),
        }
    }

    fn cooldown(
        &mut self,
        ctx: &NativeContext,
        p: &Player,
        command: &'static str,
    ) -> Option<Vec<String>> {
        if self.settings.cooldown_ticks == 0
            || permitted(ctx, p, &format!("essentials.{command}.bypass"))
        {
            return None;
        }
        let key = (p.id, command);
        if let Some(last) = self.last_use.get(&key).copied() {
            let ready = last.saturating_add(self.settings.cooldown_ticks);
            if self.now < ready {
                let seconds = (ready - self.now).div_ceil(TICKS_PER_SECOND).max(1);
                return Some(vec![format!(
                    "Wait {seconds} more second(s) before using /{command} again."
                )]);
            }
        }
        self.last_use.insert(key, self.now);
        None
    }

    fn remember_back(&mut self, ctx: &NativeContext, player: u64, location: Location) {
        let Some(owner) = owner_key_for(ctx, player) else {
            return;
        };
        let mut next = self.back.clone();
        next.insert(owner, location);
        match save_json(ctx, KV_BACK, &next) {
            Ok(()) => self.back = next,
            Err(e) => ctx.log(
                LogLevel::Warn,
                &format!("could not save the /back location: {e}"),
            ),
        }
    }

    fn expire_requests(&mut self, ctx: &NativeContext, now: u64) {
        if self.requests.is_empty() {
            return;
        }
        let expired: Vec<((u64, u64), Request)> = self
            .requests
            .iter()
            .filter(|(_, request)| request.due_tick <= now)
            .map(|(key, request)| (*key, request.clone()))
            .collect();
        for (key, request) in expired {
            self.requests.remove(&key);
            notify(
                ctx,
                request.requester,
                &format!("Teleport request to {} expired.", request.target_name),
            );
            notify(
                ctx,
                request.target,
                &format!("Teleport request from {} expired.", request.requester_name),
            );
        }
    }

    fn drop_player_requests(&mut self, ctx: &NativeContext, p: &Player, reason: &str) {
        if self.requests.is_empty() {
            return;
        }
        let keys: Vec<(u64, u64)> = self
            .requests
            .iter()
            .filter(|(_, request)| request.requester == p.id || request.target == p.id)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            let Some(request) = self.requests.remove(&key) else {
                continue;
            };
            let other = if request.requester == p.id {
                request.target
            } else {
                request.requester
            };
            notify(
                ctx,
                other,
                &format!("{} {reason}; the teleport request was cancelled.", p.name),
            );
        }
    }

    fn online_players(&self, ctx: &NativeContext) -> Vec<Player> {
        ctx.backend.online_players(ctx.id)
    }

    fn online_by_name(&self, ctx: &NativeContext, name: &str) -> Option<Player> {
        self.online_players(ctx)
            .into_iter()
            .find(|player| player.name.eq_ignore_ascii_case(name))
    }

    fn online_by_id(&self, ctx: &NativeContext, id: u64) -> Option<Player> {
        self.online_players(ctx)
            .into_iter()
            .find(|player| player.id == id)
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

/// A player's current position by session id.
fn location_of(ctx: &NativeContext, player: u64) -> Option<Location> {
    ctx.backend
        .player_position(ctx.id, player)
        .map(Location::from_position)
}

/// The persistence key for a player, resolving the name through the online
/// player list when the host exposes no UUID.
fn owner_key_for(ctx: &NativeContext, player: u64) -> Option<String> {
    if let Some(uuid) = ctx.backend.player_uuid(ctx.id, player) {
        return Some(format!("uuid:{uuid:032x}"));
    }
    ctx.backend
        .online_players(ctx.id)
        .into_iter()
        .find(|online| online.id == player)
        .map(|online| format!("name:{}", online.name.to_lowercase()))
}

/// Parse an absolute coordinate, or `~` / `~<offset>` relative to `current`.
fn parse_coordinate(raw: &str, current: f64) -> Option<f64> {
    match raw.strip_prefix('~') {
        Some("") => Some(current),
        Some(offset) => offset.parse::<f64>().ok().map(|offset| current + offset),
        None => raw.parse::<f64>().ok(),
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

    settings.request_timeout_ticks = config_ticks(
        ctx,
        "request-timeout-ticks",
        &["request-timeout", "request-timeout-seconds", "tpa-timeout"],
        settings.request_timeout_ticks,
    );
    settings.cooldown_ticks = config_ticks(
        ctx,
        "cooldown-ticks",
        &["cooldown", "cooldown-seconds"],
        settings.cooldown_ticks,
    );

    if let Some((key, raw)) = config_first(ctx, &["rtp-radius", "rtp-max-radius"]) {
        match raw.trim().parse::<u32>() {
            Ok(value) if value > 0 => settings.rtp_radius = value,
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a whole number >= 1; using {}",
                    settings.rtp_radius
                ),
            ),
        }
    }

    if let Some((key, raw)) = config_first(ctx, &["max-up", "up-limit"]) {
        match raw.trim().parse::<u32>() {
            Ok(value) if value > 0 => settings.max_up = value,
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a whole number >= 1; using {}",
                    settings.max_up
                ),
            ),
        }
    }

    if let Some(raw) = config_value(ctx, "jump-distance") {
        match raw.trim().parse::<f64>() {
            Ok(value) if value.is_finite() && value > 0.0 => settings.jump_distance = value,
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `jump-distance` value `{raw}`: expected a positive number; using {}",
                    settings.jump_distance
                ),
            ),
        }
    }

    settings
}

/// A tick setting: the `-ticks` spelling wins, else the seconds spelling is
/// converted at 20 ticks per second.
fn config_ticks(ctx: &NativeContext, ticks_key: &str, seconds_keys: &[&str], default: u64) -> u64 {
    if let Some(raw) = config_value(ctx, ticks_key) {
        match raw.trim().parse::<u64>() {
            Ok(value) => return value,
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{ticks_key}` value `{raw}`: expected a non-negative whole number; using {default}"
                ),
            ),
        }
        return default;
    }
    if let Some((key, raw)) = config_first(ctx, seconds_keys) {
        match raw.trim().parse::<u64>() {
            Ok(seconds) => return seconds.saturating_mul(TICKS_PER_SECOND),
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a non-negative number of seconds; using {default} tick(s)"
                ),
            ),
        }
    }
    default
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
