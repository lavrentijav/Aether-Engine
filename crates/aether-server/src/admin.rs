//! Vanilla player and access management: `/op`, `/deop`, `/ban`, `/ban-ip`,
//! `/banlist`, `/pardon`, `/pardon-ip`, `/kick` and `/whitelist` (CMD-08).
//!
//! Every target list goes through the CMD-06b selector parser: a plain player
//! name is accepted where vanilla accepts a game profile (operators, bans,
//! pardons, whitelist), and `@a`/`@p`/... resolve against the live registry.
//! A target that does not resolve is an error (`No player was found`), never a
//! silent skip. The command nodes carry the CMD-01 permission gate at vanilla
//! op level 3.
//!
//! # Operators
//!
//! The permission store is the source of truth (CMD-01): `config.operators` is
//! only the startup seed. `/op` adds the *bare name* to the built-in `op`
//! group, and `/deop` removes it. An IP-bound seed (`name@address`) is not
//! widened: [`crate::permissions::Store::add_member`] never clears an existing
//! binding, so `/op name` leaves `name@address` exactly as bound as it was.
//!
//! # Bans and the whitelist
//!
//! Both live in small sidecar JSON files under `[server] world_dir`
//! (`banned.json`, `whitelist.json`). They are not world-KV keys, so they do
//! not claim a T5 key prefix; the files are the atomic, hand-editable source
//! that `/whitelist reload` re-reads. `whitelist.json` also carries the on/off
//! switch. A player entry stores both name and UUID (offline UUID when the
//! target is not online); a login matches either, and operators bypass the
//! whitelist as they do in vanilla.
//!
//! The login gate is [`admit`], called from `session::login_and_play` before
//! authentication (by claimed name) and again after it (by the authenticated
//! UUID), next to the max-players/operator admission.
//!
//! # Format
//!
//! `banned.json`:
//! ```json
//! {"players":[{"name":"alice","reason":"griefing","source":"Server"}],
//!  "ips":[{"name":"10.0.0.7","reason":"griefing","source":"Server"}]}
//! ```
//! `whitelist.json`:
//! ```json
//! {"enabled":true,
//!  "players":[{"name":"alice","uuid":"…"}]}
//! ```

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use aether_api::Vector3;

use crate::commands::coordinates::Rotation;
use crate::commands::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use crate::commands::tree::{self, Ctx, Node, Requirement, Source};
use crate::commands::Reply;
use crate::config::Config;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::nbt::Nbt;

/// The file a ban list is persisted to, under the world directory.
pub const BAN_FILE: &str = "banned.json";
/// The file the whitelist is persisted to, under the world directory.
pub const WHITELIST_FILE: &str = "whitelist.json";

/// The reason vanilla stores when `/ban`/`/ban-ip` is used without one.
pub const DEFAULT_BAN_REASON: &str = "Banned by an operator.";
/// The reason vanilla stores when `/kick` is used without one.
const DEFAULT_KICK_REASON: &str = "Kicked by an operator";

// ---------------------------------------------------------------------------
// Persisted model
// ---------------------------------------------------------------------------

/// One player or IP ban.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanEntry {
    /// The banned player name, or the banned IP address in text form.
    pub name: String,
    /// Why they were banned.
    #[serde(default)]
    pub reason: String,
    /// Who ran the command.
    #[serde(default)]
    pub source: String,
}

/// The two vanilla ban lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bans {
    /// Bans keyed by player name (case-insensitive on lookup).
    #[serde(default)]
    pub players: Vec<BanEntry>,
    /// Bans keyed by IP address.
    #[serde(default)]
    pub ips: Vec<BanEntry>,
}

impl Bans {
    /// Read `path`, or `None` when it does not exist.
    fn load(path: &Path) -> Result<Option<Bans>, String> {
        read_json(path)
    }

    /// Write `path` atomically.
    fn save(&self, path: &Path) -> Result<(), String> {
        write_json(path, self)
    }

    /// The player ban for `name`, case-insensitively.
    fn player(&self, name: &str) -> Option<&BanEntry> {
        self.players
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(name))
    }

    /// The ban for `ip`.
    fn ip(&self, ip: IpAddr) -> Option<&BanEntry> {
        self.ips.iter().find(|b| b.name.parse::<IpAddr>() == Ok(ip))
    }

    /// The ban that applies to `(name, ip)`, name first like vanilla.
    fn matching(&self, name: &str, ip: Option<IpAddr>) -> Option<&BanEntry> {
        self.player(name).or_else(|| ip.and_then(|ip| self.ip(ip)))
    }

    /// Add a player ban; `false` when that name is already banned.
    fn add_player(&mut self, name: &str, reason: &str, source: &str) -> bool {
        if self.player(name).is_some() {
            return false;
        }
        self.players.push(BanEntry {
            name: name.to_string(),
            reason: reason.to_string(),
            source: source.to_string(),
        });
        true
    }

    /// Remove a player ban; `false` when it was not there.
    fn remove_player(&mut self, name: &str) -> bool {
        let before = self.players.len();
        self.players.retain(|b| !b.name.eq_ignore_ascii_case(name));
        self.players.len() != before
    }

    /// Add an IP ban; `false` when it is already banned.
    fn add_ip(&mut self, ip: IpAddr, reason: &str, source: &str) -> bool {
        if self.ip(ip).is_some() {
            return false;
        }
        self.ips.push(BanEntry {
            name: ip.to_string(),
            reason: reason.to_string(),
            source: source.to_string(),
        });
        true
    }

    /// Remove an IP ban; `false` when it was not there.
    fn remove_ip(&mut self, ip: IpAddr) -> bool {
        let before = self.ips.len();
        self.ips.retain(|b| b.name.parse::<IpAddr>() != Ok(ip));
        self.ips.len() != before
    }
}

/// One whitelist row: the name and the UUID a later login is matched against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhitelistEntry {
    /// The player name as it was added.
    pub name: String,
    /// The player's UUID in 32 lowercase hex digits (no hyphens).
    pub uuid: String,
}

/// The whitelist file: the on/off switch and its entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Whitelist {
    /// Whether the whitelist gates logins at all.
    #[serde(default)]
    pub enabled: bool,
    /// The whitelisted players.
    #[serde(default)]
    pub players: Vec<WhitelistEntry>,
}

impl Whitelist {
    /// Build the seed from the legacy `config.whitelist` list.
    fn from_config(names: &[String]) -> Whitelist {
        Whitelist {
            enabled: !names.is_empty(),
            players: names
                .iter()
                .map(|name| WhitelistEntry {
                    name: name.clone(),
                    uuid: uuid_hex(aether_api::player::offline_uuid(name)),
                })
                .collect(),
        }
    }

    /// Read `path`, or `None` when it does not exist.
    fn load(path: &Path) -> Result<Option<Whitelist>, String> {
        read_json(path)
    }

    /// Write `path` atomically.
    fn save(&self, path: &Path) -> Result<(), String> {
        write_json(path, self)
    }

    /// Whether `(name, uuid)` is on the list.
    fn contains(&self, name: &str, uuid: u128) -> bool {
        let uuid = uuid_hex(uuid);
        self.players
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name) || uuid_key(&p.uuid) == uuid)
    }

    /// Add `(name, uuid)`; `false` when an equivalent row already exists.
    fn add(&mut self, name: &str, uuid: u128) -> bool {
        if self.contains(name, uuid) {
            return false;
        }
        self.players.push(WhitelistEntry {
            name: name.to_string(),
            uuid: uuid_hex(uuid),
        });
        true
    }

    /// Remove `(name, uuid)`; `false` when it was not there.
    fn remove(&mut self, name: &str, uuid: u128) -> bool {
        let uuid = uuid_hex(uuid);
        let before = self.players.len();
        self.players
            .retain(|p| !p.name.eq_ignore_ascii_case(name) && uuid_key(&p.uuid) != uuid);
        self.players.len() != before
    }
}

/// 32 lowercase hex digits, the spelling both files use.
fn uuid_hex(uuid: u128) -> String {
    format!("{uuid:032x}")
}

/// A file UUID with any hyphens removed and case folded, so a hand-edited
/// vanilla-style (`8-4-4-4-12`) entry still matches.
fn uuid_key(text: &str) -> String {
    text.chars()
        .filter(|c| *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("cannot parse `{}`: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read `{}`: {e}", path.display())),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("cannot encode `{}`: {e}", path.display()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|e| format!("cannot write `{}`: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace `{}`: {e}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Process-wide store
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct AdminData {
    bans: Bans,
    whitelist: Whitelist,
}

#[derive(Clone, Debug, Default)]
struct State {
    /// Set by [`install`]; before that the legacy `config.whitelist` is used.
    dir: Option<PathBuf>,
    data: AdminData,
}

fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(State::default()))
}

/// Load the ban list and whitelist from `cfg.server.world_dir`, seeding the
/// whitelist from `config.whitelist` on the first start. Called once at
/// startup, before the listener accepts a connection.
pub fn install(cfg: &Config) {
    install_in(Path::new(&cfg.server.world_dir), cfg);
}

fn install_in(dir: &Path, cfg: &Config) {
    let ban_path = dir.join(BAN_FILE);
    let white_path = dir.join(WHITELIST_FILE);
    let mut data = AdminData::default();
    match Bans::load(&ban_path) {
        Ok(Some(bans)) => data.bans = bans,
        Ok(None) => {
            if let Err(e) = data.bans.save(&ban_path) {
                crate::log::warn(&format!("bans: {e}"));
            }
        }
        Err(e) => crate::log::warn(&format!("bans: starting empty: {e}")),
    }
    match Whitelist::load(&white_path) {
        Ok(Some(whitelist)) => data.whitelist = whitelist,
        Ok(None) => {
            // The legacy config list is the first-start seed only. Once the
            // file exists it is the source of truth, exactly as the permission
            // file is after CMD-01's seed.
            data.whitelist = Whitelist::from_config(&cfg.whitelist);
            if let Err(e) = data.whitelist.save(&white_path) {
                crate::log::warn(&format!("whitelist: {e}"));
            }
        }
        Err(e) => crate::log::warn(&format!("whitelist: starting empty: {e}")),
    }
    let mut guard = state().write().unwrap_or_else(|e| e.into_inner());
    guard.dir = Some(dir.to_path_buf());
    guard.data = data;
}

/// Re-read `whitelist.json` from disk, replacing the in-memory list.
pub fn reload_whitelist() -> Result<(), String> {
    let dir = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        guard
            .dir
            .clone()
            .ok_or("the admin store is not installed")?
    };
    let path = dir.join(WHITELIST_FILE);
    let whitelist =
        Whitelist::load(&path)?.ok_or_else(|| format!("`{}` does not exist", path.display()))?;
    let mut guard = state().write().unwrap_or_else(|e| e.into_inner());
    guard.data.whitelist = whitelist;
    Ok(())
}

fn update_bans<T>(f: impl FnOnce(&mut Bans) -> Result<T, String>) -> Result<T, String> {
    let mut guard = state().write().unwrap_or_else(|e| e.into_inner());
    let mut data = guard.data.clone();
    let out = f(&mut data.bans)?;
    if let Some(dir) = &guard.dir {
        data.bans.save(&dir.join(BAN_FILE))?;
    }
    guard.data = data;
    Ok(out)
}

fn update_whitelist<T>(f: impl FnOnce(&mut Whitelist) -> Result<T, String>) -> Result<T, String> {
    let mut guard = state().write().unwrap_or_else(|e| e.into_inner());
    let mut data = guard.data.clone();
    let out = f(&mut data.whitelist)?;
    if let Some(dir) = &guard.dir {
        data.whitelist.save(&dir.join(WHITELIST_FILE))?;
    }
    guard.data = data;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Login gate
// ---------------------------------------------------------------------------

/// Why a login was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Ban by name or address, with the stored reason.
    Banned(String),
    /// The whitelist is on and the player is neither listed nor an operator.
    NotWhitelisted,
}

impl Admission {
    /// The login-phase disconnect text, as vanilla words it.
    pub fn message(&self) -> String {
        match self {
            Admission::Banned(reason) if !reason.trim().is_empty() => {
                format!("You are banned from this server! {reason}")
            }
            Admission::Banned(_) => "You are banned from this server!".to_string(),
            Admission::NotWhitelisted => "You are not on this server's whitelist.".to_string(),
        }
    }

    /// A short reason for the server log.
    pub fn log_reason(&self) -> String {
        match self {
            Admission::Banned(reason) if !reason.trim().is_empty() => {
                format!("banned ({reason})")
            }
            Admission::Banned(_) => "banned".to_string(),
            Admission::NotWhitelisted => "not on the whitelist".to_string(),
        }
    }
}

/// The ban/whitelist admission gate used by `session::login_and_play`.
///
/// Before authentication the caller passes the offline UUID derived from the
/// claimed name; after authentication it passes the real UUID. Operators
/// bypass the whitelist (vanilla), but never a ban.
pub fn admit(name: &str, uuid: u128, ip: Option<IpAddr>, cfg: &Config) -> Result<(), Admission> {
    let guard = state().read().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = guard.data.bans.matching(name, ip) {
        return Err(Admission::Banned(entry.reason.clone()));
    }
    let (enabled, listed) = match guard.dir {
        Some(_) => (
            guard.data.whitelist.enabled,
            guard.data.whitelist.contains(name, uuid),
        ),
        // Before `install` (tests and tiny tools) the legacy config list keeps
        // its old meaning: empty lets everyone in.
        None => (
            !cfg.whitelist.is_empty(),
            cfg.whitelist.iter().any(|w| w.eq_ignore_ascii_case(name)),
        ),
    };
    if enabled && !listed && !crate::permissions::is_operator(name, ip) {
        return Err(Admission::NotWhitelisted);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Selector target resolution
// ---------------------------------------------------------------------------

/// A resolved command target: an online handle, or an offline profile.
enum Target {
    Online(Arc<PlayerHandle>),
    Offline { name: String, uuid: u128 },
}

impl Target {
    fn name(&self) -> &str {
        match self {
            Target::Online(handle) => &handle.name,
            Target::Offline { name, .. } => name,
        }
    }

    fn uuid(&self) -> u128 {
        match self {
            Target::Online(handle) => handle.uuid,
            Target::Offline { uuid, .. } => *uuid,
        }
    }
}

/// The CMD-06b selector source over the live player registry.
struct OnlineSource<'a> {
    registry: &'a SharedRegistry,
    executor: Option<EntityRef>,
}

fn player_ref(handle: &PlayerHandle) -> EntityRef {
    let pos = handle.pos();
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: Rotation::new(pos.yaw, pos.pitch),
        tags: Vec::new(),
        team: None,
        gamemode: Some(Gamemode::from(handle.game().mode)),
        level: 0,
        scores: Vec::new(),
        advancements: Vec::new(),
        predicates: Vec::new(),
        nbt: Nbt::Compound(Vec::new()),
    }
}

impl SelectorSource for OnlineSource<'_> {
    fn executor(&self) -> Option<EntityRef> {
        self.executor.clone()
    }

    fn players(&self) -> Vec<EntityRef> {
        self.registry
            .snapshot()
            .iter()
            .map(|handle| player_ref(handle))
            .collect()
    }
}

fn online_source<'a>(registry: &'a SharedRegistry, source: Source<'a>) -> OnlineSource<'a> {
    OnlineSource {
        registry,
        executor: source.player().map(player_ref),
    }
}

/// Resolve one target token: a CMD-06b selector, or a plain player name.
///
/// A selector only names online players; a plain name may name an offline
/// profile when `allow_offline` (operators, bans, pardons, whitelist).
fn resolve_targets(
    registry: &SharedRegistry,
    source: Source<'_>,
    text: &str,
    allow_offline: bool,
) -> Result<Vec<Target>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No player was found".into());
    }
    if text.starts_with('@') {
        let selector = Selector::parse_players(text).map_err(|e| e.message())?;
        let source = online_source(registry, source);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let found = selector
            .select(&source, &mut LcgRng::new(seed))
            .map_err(|e| e.message())?;
        let mut out = Vec::with_capacity(found.len());
        for entity in found {
            let handle = registry
                .by_uuid(entity.uuid)
                .or_else(|| {
                    let name = entity.name.clone().unwrap_or_default();
                    registry
                        .snapshot()
                        .into_iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&name))
                })
                .ok_or_else(|| "No player was found".to_string())?;
            out.push(Target::Online(handle));
        }
        if out.is_empty() {
            return Err("No player was found".into());
        }
        return Ok(out);
    }
    let online = registry
        .snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(text));
    match online {
        Some(handle) => Ok(vec![Target::Online(handle)]),
        None if allow_offline => Ok(vec![Target::Offline {
            name: text.to_string(),
            uuid: aether_api::player::offline_uuid(text),
        }]),
        None => Err("No player was found".into()),
    }
}

/// Split `<targets> [<reason>]`: one target token, the rest is the reason.
fn target_and_reason(args: &[String]) -> (String, Option<String>) {
    let target = args.first().cloned().unwrap_or_default();
    let reason = if args.len() > 1 {
        Some(args[1..].join(" "))
    } else {
        None
    };
    (target, reason)
}

// ---------------------------------------------------------------------------
// Command nodes
// ---------------------------------------------------------------------------

fn command(name: &'static str, node: &'static str, usage: &'static str) -> tree::Builder {
    tree::Builder::literal(name)
        .requires(Requirement::Level { op: 3, node })
        .usage(usage)
}

/// The admin command nodes.
pub fn nodes() -> Vec<Node> {
    vec![
        command("op", "minecraft.command.op", "<targets>")
            .then(tree::Builder::arg("targets", tree::ArgKind::GreedyString).executable(exec_op))
            .executable(exec_op)
            .build(),
        command("deop", "minecraft.command.deop", "<targets>")
            .then(tree::Builder::arg("targets", tree::ArgKind::GreedyString).executable(exec_deop))
            .executable(exec_deop)
            .build(),
        command("ban", "minecraft.command.ban", "<targets> [<reason>]")
            .then(tree::Builder::arg("args", tree::ArgKind::GreedyString).executable(exec_ban))
            .executable(exec_ban)
            .build(),
        command("ban-ip", "minecraft.command.ban-ip", "<targets> [<reason>]")
            .then(tree::Builder::arg("args", tree::ArgKind::GreedyString).executable(exec_ban_ip))
            .executable(exec_ban_ip)
            .build(),
        command("banlist", "minecraft.command.banlist", "[ips|players]")
            .then(tree::Builder::arg("args", tree::ArgKind::GreedyString).executable(exec_banlist))
            .executable(exec_banlist)
            .build(),
        command("pardon", "minecraft.command.pardon", "<targets>")
            .then(
                tree::Builder::arg("targets", tree::ArgKind::GreedyString).executable(exec_pardon),
            )
            .executable(exec_pardon)
            .build(),
        command("pardon-ip", "minecraft.command.pardon-ip", "<targets>")
            .then(
                tree::Builder::arg("targets", tree::ArgKind::GreedyString)
                    .executable(exec_pardon_ip),
            )
            .executable(exec_pardon_ip)
            .build(),
        command("kick", "minecraft.command.kick", "<targets> [<reason>]")
            .then(tree::Builder::arg("args", tree::ArgKind::GreedyString).executable(exec_kick))
            .executable(exec_kick)
            .build(),
        command(
            "whitelist",
            "minecraft.command.whitelist",
            "<on|off|list|add|remove|reload> [<targets>]",
        )
        .then(tree::Builder::arg("args", tree::ArgKind::GreedyString).executable(exec_whitelist))
        .executable(exec_whitelist)
        .build(),
    ]
}

fn args(ctx: &Ctx) -> Vec<String> {
    ctx.refs().into_iter().map(str::to_string).collect()
}

fn exec_op(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_op(ctx, &args, true) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn exec_deop(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_op(ctx, &args, false) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_op(ctx: &Ctx, args: &[String], add: bool) -> Result<Vec<String>, String> {
    let target = args.first().cloned().unwrap_or_default();
    let targets = resolve_targets(ctx.registry, ctx.source, &target, true)?;
    let mut lines = Vec::new();
    let mut refused = Vec::new();
    for target in targets {
        let name = target.name().to_string();
        let changed = if add {
            crate::permissions::add_member(crate::permissions::OP_GROUP, &name, None)?
        } else {
            crate::permissions::remove_member(crate::permissions::OP_GROUP, &name)?
        };
        if changed {
            lines.push(if add {
                format!("Made {name} a server operator")
            } else {
                format!("Made {name} no longer a server operator")
            });
        } else {
            refused.push(if add {
                format!("Nothing changed. The player {name} already is an operator")
            } else {
                format!("Nothing changed. The player {name} isn't an operator")
            });
        }
    }
    if lines.is_empty() {
        return Err(refused.join("\n"));
    }
    Ok(lines)
}

fn exec_ban(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_ban(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_ban(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let (target, reason) = target_and_reason(args);
    let targets = resolve_targets(ctx.registry, ctx.source, &target, true)?;
    let stored_reason = reason
        .clone()
        .unwrap_or_else(|| DEFAULT_BAN_REASON.to_string());
    let source = ctx.source.name().to_string();
    let mut lines = Vec::new();
    for target in targets {
        let name = target.name().to_string();
        let added = update_bans(|bans| Ok(bans.add_player(&name, &stored_reason, &source)))?;
        if !added {
            return Err(format!("Could not ban {name}"));
        }
        if let Target::Online(handle) = target {
            handle.disconnect(&Admission::Banned(stored_reason.clone()).message());
        }
        lines.push(match &reason {
            Some(reason) => format!("Banned {name}: {reason}"),
            None => format!("Banned {name}"),
        });
    }
    Ok(lines)
}

fn exec_ban_ip(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_ban_ip(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn target_ips(ctx: &Ctx, text: &str) -> Result<Vec<(IpAddr, Option<Arc<PlayerHandle>>)>, String> {
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(vec![(ip, None)]);
    }
    if text.starts_with('@') {
        let targets = resolve_targets(ctx.registry, ctx.source, text, false)?;
        let mut out = Vec::new();
        for target in targets {
            if let Target::Online(handle) = target {
                match handle.ip {
                    Some(ip) => out.push((ip, Some(handle))),
                    None => return Err(format!("Invalid IP address or unknown player '{text}'")),
                }
            }
        }
        return Ok(out);
    }
    // A plain online name resolves to the address it is connected from; an
    // offline name has no address to ban (`/ban name` is the offline tool).
    let targets = resolve_targets(ctx.registry, ctx.source, text, false)?;
    let mut out = Vec::new();
    for target in targets {
        if let Target::Online(handle) = target {
            match handle.ip {
                Some(ip) => out.push((ip, Some(handle))),
                None => return Err(format!("Invalid IP address or unknown player '{text}'")),
            }
        }
    }
    Ok(out)
}

fn do_ban_ip(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let (target, reason) = target_and_reason(args);
    if target.trim().is_empty() {
        return Err("Invalid IP address or unknown player ''".into());
    }
    let ips = target_ips(ctx, &target)?;
    let stored_reason = reason
        .clone()
        .unwrap_or_else(|| DEFAULT_BAN_REASON.to_string());
    let source = ctx.source.name().to_string();
    let mut lines = Vec::new();
    for (ip, online) in ips {
        let added = update_bans(|bans| Ok(bans.add_ip(ip, &stored_reason, &source)))?;
        if !added {
            return Err(format!("Could not ban IP address {ip}"));
        }
        if let Some(handle) = online {
            handle.disconnect(&Admission::Banned(stored_reason.clone()).message());
        }
        lines.push(match &reason {
            Some(reason) => format!("Banned IP address {ip}: {reason}"),
            None => format!("Banned IP address {ip}"),
        });
    }
    Ok(lines)
}

fn exec_banlist(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    if args.len() > 1 {
        return ctx.fail("Incorrect argument for command");
    }
    let filter = args.first().map(String::as_str).unwrap_or("");
    let (players, ips) = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        (guard.data.bans.players.clone(), guard.data.bans.ips.clone())
    };
    match filter {
        "" => banlist_lines(
            players.len() + ips.len(),
            players.iter().chain(ips.iter()),
            None,
        ),
        "players" => banlist_lines(players.len(), players.iter(), Some("players")),
        "ips" => banlist_lines(ips.len(), ips.iter(), Some("ips")),
        other => ctx.fail(format!(
            "Incorrect argument for command: expected 'ips' or 'players', got '{other}'"
        )),
    }
}

fn banlist_lines<'a>(
    total: usize,
    entries: impl Iterator<Item = &'a BanEntry>,
    filter: Option<&str>,
) -> Reply {
    if total == 0 {
        return Reply::one("There are no bans");
    }
    let header = match filter {
        Some("players") => format!("There are {total} banned players:"),
        Some("ips") => format!("There are {total} banned IP addresses:"),
        _ => format!("There are {total} bans:"),
    };
    let mut lines = vec![header];
    for entry in entries {
        if entry.reason.trim().is_empty() {
            lines.push(format!("{} was banned by {}", entry.name, entry.source));
        } else {
            lines.push(format!(
                "{} was banned by {}: {}",
                entry.name, entry.source, entry.reason
            ));
        }
    }
    Reply(lines)
}

fn exec_pardon(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_pardon(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_pardon(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let target = args.first().cloned().unwrap_or_default();
    let targets = resolve_targets(ctx.registry, ctx.source, &target, true)?;
    let mut lines = Vec::new();
    for target in targets {
        let name = target.name().to_string();
        let removed = update_bans(|bans| Ok(bans.remove_player(&name)))?;
        if !removed {
            return Err(format!("Nothing changed. The player {name} isn't banned"));
        }
        lines.push(format!("Unbanned {name}"));
    }
    Ok(lines)
}

fn exec_pardon_ip(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_pardon_ip(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_pardon_ip(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let target = args.first().cloned().unwrap_or_default();
    let ips = target_ips(ctx, &target)?;
    let mut lines = Vec::new();
    for (ip, _) in ips {
        let removed = update_bans(|bans| Ok(bans.remove_ip(ip)))?;
        if !removed {
            return Err(format!("Nothing changed. The IP address {ip} isn't banned"));
        }
        lines.push(format!("Unbanned IP address {ip}"));
    }
    Ok(lines)
}

fn exec_kick(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_kick(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_kick(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let (target, reason) = target_and_reason(args);
    let targets = resolve_targets(ctx.registry, ctx.source, &target, false)?;
    let disconnect_reason = reason
        .clone()
        .unwrap_or_else(|| DEFAULT_KICK_REASON.to_string());
    let mut lines = Vec::new();
    for target in targets {
        let Target::Online(handle) = target else {
            return Err("No player was found".into());
        };
        handle.disconnect(&disconnect_reason);
        lines.push(match &reason {
            Some(reason) => format!("Kicked {}: {reason}", handle.name),
            None => format!("Kicked {}", handle.name),
        });
    }
    Ok(lines)
}

fn exec_whitelist(ctx: &mut Ctx) -> Reply {
    let args = args(ctx);
    match do_whitelist(ctx, &args) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_whitelist(ctx: &Ctx, args: &[String]) -> Result<Vec<String>, String> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err("Unknown or incomplete command".into());
    };
    match sub {
        "on" | "off" => {
            if args.len() != 1 {
                return Err("Incorrect argument for command".into());
            }
            let on = sub == "on";
            update_whitelist(|whitelist| {
                whitelist.enabled = on;
                Ok(())
            })?;
            Ok(vec![if on {
                "Whitelist is now turned on".to_string()
            } else {
                "Whitelist is now turned off".to_string()
            }])
        }
        "list" => {
            if args.len() != 1 {
                return Err("Incorrect argument for command".into());
            }
            let whitelist = {
                let guard = state().read().unwrap_or_else(|e| e.into_inner());
                guard.data.whitelist.clone()
            };
            if whitelist.players.is_empty() {
                return Ok(vec!["There are no whitelisted players".to_string()]);
            }
            let mut names: Vec<String> = whitelist
                .players
                .iter()
                .map(|p| p.name.clone())
                .collect();
            names.sort();
            let mut lines = vec![format!(
                "There are {} whitelisted players:",
                names.len()
            )];
            lines.extend(names);
            Ok(lines)
        }
        "reload" => {
            if args.len() != 1 {
                return Err("Incorrect argument for command".into());
            }
            reload_whitelist()?;
            Ok(vec!["Reloaded the whitelist".to_string()])
        }
        "add" | "remove" => {
            if args.len() != 2 {
                return Err("Incorrect argument for command".into());
            }
            let target = args.get(1).cloned().unwrap_or_default();
            let targets = resolve_targets(ctx.registry, ctx.source, &target, true)?;
            let add = sub == "add";
            let mut lines = Vec::new();
            for target in targets {
                let name = target.name().to_string();
                let uuid = target.uuid();
                let changed = update_whitelist(|whitelist| {
                    Ok(if add {
                        whitelist.add(&name, uuid)
                    } else {
                        whitelist.remove(&name, uuid)
                    })
                })?;
                if !changed {
                    return Err(if add {
                        format!("Player {name} is already whitelisted")
                    } else {
                        format!("Player {name} is not whitelisted")
                    });
                }
                lines.push(if add {
                    format!("Added {name} to the whitelist")
                } else {
                    format!("Removed {name} from the whitelist")
                });
            }
            Ok(lines)
        }
        other => Err(format!(
            "Incorrect argument for command: expected on, off, list, add, remove or reload, got '{other}'"
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feedback::FeedbackKind;
    use crate::commands::tree::execute_feedback_from;
    use crate::players::{PosLook, Registry};
    use crate::proto::Conn;
    use crate::protocol::codec_for;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aether-admin-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cfg() -> Config {
        Config::default()
    }

    fn world() -> crate::session::DemoWorld {
        use aether_api::{FjallStore, World};
        let dir = temp_dir("world");
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", 7).expect("noise generator");
        let revision = generator.revision();
        let cache = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, 7, revision, &cache);
        World::new(store, generator)
    }

    fn join(
        registry: &SharedRegistry,
        name: &str,
        id: i32,
        ip: Option<IpAddr>,
    ) -> Arc<PlayerHandle> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let _peer = listener.accept().unwrap();
        let handle = registry
            .join(
                id,
                id as u128,
                name.into(),
                ip,
                Vec::new(),
                codec_for(763).unwrap(),
                &client,
                PosLook {
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                },
            )
            .unwrap();
        // The writer only needs the outbox open; the peer is not read.
        std::mem::forget(client);
        handle
    }

    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _perm: std::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        world: crate::session::DemoWorld,
        registry: SharedRegistry,
        tester: Arc<PlayerHandle>,
        bob: Arc<PlayerHandle>,
    }

    fn env(tag: &str) -> Env {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _perm = crate::permissions::test_lock();
        let dir = temp_dir(tag);
        let mut cfg = cfg();
        cfg.server.world_dir = dir.to_string_lossy().into_owned();
        let mut store = crate::permissions::Store::new();
        store.seed_operators(&["tester".to_string(), "bound@10.0.0.7".to_string()]);
        crate::permissions::install_store_for_test(store);
        install_in(&dir, &cfg);
        let world = world();
        let registry: SharedRegistry = Arc::new(Registry::default());
        let tester = join(&registry, "tester", 1, Some("10.0.0.1".parse().unwrap()));
        let bob = join(&registry, "bob", 2, Some("10.0.0.2".parse().unwrap()));
        Env {
            _guard,
            _perm,
            dir,
            world,
            registry,
            tester,
            bob,
        }
    }

    fn run(env: &Env, source: Source<'_>, text: &str) -> (FeedbackKind, Vec<String>) {
        let feedback = execute_feedback_from(text, source, &env.registry, &env.world)
            .unwrap_or_else(|| panic!("`{text}` was not a command"));
        (feedback.kind, feedback.lines)
    }

    fn ok(env: &Env, text: &str) -> Vec<String> {
        let (kind, lines) = run(env, Source::Player(&env.tester), text);
        assert_eq!(kind, FeedbackKind::Success, "`{text}` => {lines:?}");
        lines
    }

    fn err(env: &Env, text: &str) -> String {
        let (kind, lines) = run(env, Source::Player(&env.tester), text);
        assert_eq!(kind, FeedbackKind::Error, "`{text}` => {lines:?}");
        lines.join("\n")
    }

    fn one(env: &Env, text: &str) -> String {
        let lines = ok(env, text);
        assert_eq!(lines.len(), 1, "`{text}` => {lines:?}");
        lines.into_iter().next().unwrap()
    }

    #[test]
    fn op_adds_a_name_and_deop_removes_it_without_widening_a_bound_entry() {
        let env = env("op");
        assert_eq!(one(&env, "/op alice"), "Made alice a server operator");
        assert!(crate::permissions::is_operator("alice", None));

        // The config seed `bound@10.0.0.7` stays bound: `/op bound` neither
        // reports a new grant nor widens it to every address.
        assert!(err(&env, "/op bound").contains("Nothing changed"));
        assert!(
            crate::permissions::is_operator("bound", Some("10.0.0.7".parse().unwrap())),
            "the bound address still works"
        );
        assert!(
            !crate::permissions::is_operator("bound", Some("10.0.0.8".parse().unwrap())),
            "`/op` must not widen an IP-bound entry"
        );

        assert_eq!(
            one(&env, "/deop alice"),
            "Made alice no longer a server operator"
        );
        assert!(!crate::permissions::is_operator("alice", None));
    }

    #[test]
    fn banning_an_online_player_kicks_them_and_persists_the_ban() {
        let env = env("ban-online");
        assert_eq!(one(&env, "/ban bob griefing"), "Banned bob: griefing");
        assert!(
            env.bob.disconnected().is_some(),
            "an online ban disconnects the target"
        );
        assert!(env
            .bob
            .disconnected()
            .unwrap()
            .contains("You are banned from this server! griefing"));
        let saved: Bans =
            serde_json::from_str(&std::fs::read_to_string(env.dir.join(BAN_FILE)).unwrap())
                .unwrap();
        assert_eq!(saved.players[0].name, "bob");
        assert_eq!(saved.players[0].reason, "griefing");
    }

    #[test]
    fn an_offline_ban_persists_and_refuses_the_next_login_with_the_reason() {
        let env = env("ban-offline");
        assert_eq!(one(&env, "/ban carol cheating"), "Banned carol: cheating");
        let refusal = admit(
            "carol",
            aether_api::player::offline_uuid("carol"),
            None,
            &cfg(),
        )
        .unwrap_err();
        assert_eq!(
            refusal.message(),
            "You are banned from this server! cheating"
        );
        // Reloading the process-wide store from the file keeps the ban.
        let mut cfg = cfg();
        cfg.server.world_dir = env.dir.to_string_lossy().into_owned();
        install_in(&env.dir, &cfg);
        assert!(admit("carol", 0, None, &cfg).is_err());
    }

    #[test]
    fn pardoning_something_that_is_not_banned_is_an_error_not_a_success() {
        let env = env("pardon");
        assert!(err(&env, "/pardon nobody").contains("Nothing changed"));
        assert_eq!(one(&env, "/ban dave"), "Banned dave");
        assert_eq!(one(&env, "/pardon dave"), "Unbanned dave");
        assert!(admit("dave", 0, None, &cfg()).is_ok());
    }

    #[test]
    fn an_ip_ban_applies_by_address_and_a_name_ban_applies_by_name() {
        let env = env("ban-ip");
        assert_eq!(
            one(&env, "/ban-ip 10.0.0.9 scanning"),
            "Banned IP address 10.0.0.9: scanning"
        );
        assert!(admit("anyone", 1, Some("10.0.0.9".parse().unwrap()), &cfg()).is_err());
        assert!(admit("anyone", 1, Some("10.0.0.8".parse().unwrap()), &cfg()).is_ok());

        assert_eq!(one(&env, "/ban erin"), "Banned erin");
        assert!(
            admit("erin", 1, Some("10.0.0.8".parse().unwrap()), &cfg()).is_err(),
            "a name ban follows the name to another address"
        );
    }

    #[test]
    fn banlist_headers_and_the_ip_player_filters_match_vanilla() {
        let env = env("banlist");
        assert_eq!(one(&env, "/banlist"), "There are no bans");
        assert_eq!(one(&env, "/ban alice"), "Banned alice");
        assert_eq!(one(&env, "/ban-ip 10.0.0.5"), "Banned IP address 10.0.0.5");
        assert_eq!(
            ok(&env, "/banlist"),
            vec![
                "There are 2 bans:".to_string(),
                "alice was banned by tester: Banned by an operator.".to_string(),
                "10.0.0.5 was banned by tester: Banned by an operator.".to_string(),
            ]
        );
        assert_eq!(
            ok(&env, "/banlist players"),
            vec![
                "There are 1 banned players:".to_string(),
                "alice was banned by tester: Banned by an operator.".to_string(),
            ]
        );
        assert_eq!(
            ok(&env, "/banlist ips"),
            vec![
                "There are 1 banned IP addresses:".to_string(),
                "10.0.0.5 was banned by tester: Banned by an operator.".to_string(),
            ]
        );
    }

    #[test]
    fn kick_works_with_and_without_a_reason() {
        let env = env("kick");
        assert_eq!(one(&env, "/kick bob because"), "Kicked bob: because");
        assert!(env.bob.disconnected().unwrap().contains("because"));
        assert_eq!(one(&env, "/kick tester"), "Kicked tester");
        assert!(env.tester.disconnected().is_some());
    }

    #[test]
    fn whitelist_on_off_add_remove_list_and_reload_are_real_gates() {
        let env = env("white");
        assert_eq!(one(&env, "/whitelist on"), "Whitelist is now turned on");
        assert!(
            admit(
                "alice",
                aether_api::player::offline_uuid("alice"),
                None,
                &cfg()
            )
            .is_err(),
            "an empty whitelist refuses everyone"
        );
        // An operator still gets in while the whitelist is on.
        assert!(admit("tester", 1, None, &cfg()).is_ok());

        assert_eq!(
            one(&env, "/whitelist add alice"),
            "Added alice to the whitelist"
        );
        let alice_uuid = aether_api::player::offline_uuid("alice");
        assert!(admit("alice", alice_uuid, None, &cfg()).is_ok());
        // The entry carries the UUID, so a later profile that presents the
        // same UUID matches even if the name casing differs.
        assert!(admit("ALICE", alice_uuid, None, &cfg()).is_ok());
        assert_eq!(
            ok(&env, "/whitelist list"),
            vec![
                "There are 1 whitelisted players:".to_string(),
                "alice".to_string(),
            ]
        );
        assert_eq!(
            one(&env, "/whitelist remove alice"),
            "Removed alice from the whitelist"
        );
        assert!(admit("alice", alice_uuid, None, &cfg()).is_err());
        assert_eq!(one(&env, "/whitelist off"), "Whitelist is now turned off");
        assert!(admit("alice", alice_uuid, None, &cfg()).is_ok());
    }

    #[test]
    fn whitelist_reload_rereads_the_file() {
        let env = env("white-reload");
        assert_eq!(one(&env, "/whitelist on"), "Whitelist is now turned on");
        // A hand-edited vanilla-style hyphenated UUID still matches.
        let text = format!(
            "{{\"enabled\":true,\"players\":[{{\"name\":\"dana\",\"uuid\":\"{}\"}}]}}",
            crate::auth::api::hyphenated_uuid(aether_api::player::offline_uuid("dana"))
        );
        std::fs::write(env.dir.join(WHITELIST_FILE), text).unwrap();
        assert_eq!(one(&env, "/whitelist reload"), "Reloaded the whitelist");
        assert!(admit(
            "dana",
            aether_api::player::offline_uuid("dana"),
            None,
            &cfg()
        )
        .is_ok());
        assert!(admit(
            "alice",
            aether_api::player::offline_uuid("alice"),
            None,
            &cfg()
        )
        .is_err());
    }

    #[test]
    fn a_selector_target_is_resolved_and_unresolved_targets_are_errors() {
        let env = env("selector");
        assert_eq!(err(&env, "/op @a[name=nobody]"), "No player was found");
        assert_eq!(err(&env, "/kick nobody"), "No player was found");
        let lines = ok(&env, "/ban @a flood");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines.iter().any(|l| l == "Banned tester: flood"),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l == "Banned bob: flood"), "{lines:?}");
        assert!(env.tester.disconnected().is_some());
        assert!(env.bob.disconnected().is_some());
    }

    #[test]
    fn level_two_cannot_run_admin_commands_but_level_three_can_and_console_is_level_four() {
        let env = env("perm");

        let mut store = (*crate::permissions::snapshot_store()).clone();
        store
            .create_group("level2", crate::permissions::Level::new(2))
            .unwrap();
        store.add_member("level2", "bob", None).unwrap();
        crate::permissions::install_store_for_test(store);

        let (kind, lines) = run(&env, Source::Player(&env.bob), "/ban alice");
        assert_eq!(kind, FeedbackKind::Error, "{lines:?}");
        assert!(lines[0].contains("permission"), "{lines:?}");

        let mut store = (*crate::permissions::snapshot_store()).clone();
        store
            .create_group("level3", crate::permissions::Level::new(3))
            .unwrap();
        store.add_member("level3", "bob", None).unwrap();
        crate::permissions::install_store_for_test(store);

        let (kind, lines) = run(&env, Source::Player(&env.bob), "/ban alice");
        assert_eq!(kind, FeedbackKind::Success, "{lines:?}");
        assert_eq!(lines, vec!["Banned alice".to_string()]);

        // The console is a level-4 owner and runs the same node.
        let (kind, lines) = run(
            &env,
            Source::Server(crate::commands::ServerKind::Console),
            "/ban dana",
        );
        assert_eq!(kind, FeedbackKind::Success, "{lines:?}");
        assert_eq!(lines, vec!["Banned dana".to_string()]);
    }
}
