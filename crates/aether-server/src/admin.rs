//! Vanilla player and access management: `/op`, `/deop`, `/ban`, `/ban-ip`,
//! `/banlist`, `/pardon`, `/pardon-ip`, `/kick` and `/whitelist` (CMD-08).
//!
//! Every target list goes through the CMD-06b selector parser: a plain player
//! name is accepted where vanilla accepts a game profile (operators, bans,
//! pardons, whitelist), and `@a`/`@p`/... resolve against the live registry.
//! A target that does not resolve is an error (`No player was found`), never a
//! silent skip. The command nodes carry the CMD-01 permission gate at vanilla
//! op level 3 and mirror the vanilla grammar (`game_profile`, `entity`,
//! `message`, literals), so an extra argument is a parse error.
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
//! A file that exists but cannot be read is a startup **error**, not an empty
//! list: starting with a broken whitelist would silently disable it, and
//! starting with broken bans would silently lift every ban. The file is left
//! untouched for the operator to repair.
//!
//! # Login gate
//!
//! The gate is split so the whitelist can be decided by the *authenticated*
//! UUID: `session::login_and_play` calls [`ban_admission`] before
//! authentication (the claimed name and address are enough for a ban) and
//! [`admit`] after it, when the real UUID is known. A whitelist row added by
//! UUID therefore admits a premium player after a rename; the pre-auth step
//! never rejects on the whitelist.
//!
//! IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are canonicalised to plain
//! IPv4 both when stored and when compared, so a ban matches however the
//! socket layer spells the peer address.
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
use crate::commands::tree::{self, Ctx, Node, Requirement, Source, Value};
use crate::commands::Reply;
use crate::config::Config;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::nbt::Nbt;
use crate::session::DemoWorld;

/// The file a ban list is persisted to, under the world directory.
pub const BAN_FILE: &str = "banned.json";
/// The file the whitelist is persisted to, under the world directory.
pub const WHITELIST_FILE: &str = "whitelist.json";

/// The reason vanilla stores when `/ban`/`/ban-ip` is used without one.
pub const DEFAULT_BAN_REASON: &str = "Banned by an operator.";
/// The reason vanilla stores when `/kick` is used without one.
pub const DEFAULT_KICK_REASON: &str = "Kicked by an operator";

/// Vanilla 1.21.11 `en_us` strings this module emits, verbatim.
mod lang {
    pub const OP_FAILED: &str = "Nothing changed. The player already is an operator";
    pub const DEOP_FAILED: &str = "Nothing changed. The player isn't an operator";
    pub const BAN_FAILED: &str = "Nothing changed. The player is already banned";
    pub const BANIP_FAILED: &str = "Nothing changed. That IP is already banned";
    pub const BANIP_INVALID: &str = "Invalid IP address or unknown player";
    pub const PARDON_FAILED: &str = "Nothing changed. The player isn't banned";
    pub const PARDONIP_FAILED: &str = "Nothing changed. That IP isn't banned";
    pub const WHITELIST_ALREADY_ON: &str = "Whitelist is already turned on";
    pub const WHITELIST_ALREADY_OFF: &str = "Whitelist is already turned off";
    pub const WHITELIST_ADD_FAILED: &str = "Player is already whitelisted";
    pub const WHITELIST_REMOVE_FAILED: &str = "Player is not whitelisted";
}

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

    /// The ban for `ip`, comparing canonical addresses.
    fn ip(&self, ip: IpAddr) -> Option<&BanEntry> {
        let want = canonical_ip(ip);
        self.ips.iter().find(|b| {
            b.name
                .parse::<IpAddr>()
                .map(canonical_ip)
                .is_ok_and(|have| have == want)
        })
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
            name: canonical_ip(ip).to_string(),
            reason: reason.to_string(),
            source: source.to_string(),
        });
        true
    }

    /// Remove an IP ban; `false` when it was not there.
    fn remove_ip(&mut self, ip: IpAddr) -> bool {
        let want = canonical_ip(ip);
        let before = self.ips.len();
        self.ips.retain(|b| {
            b.name
                .parse::<IpAddr>()
                .map(canonical_ip)
                .is_ok_and(|have| have != want)
        });
        self.ips.len() != before
    }
}

/// `::ffff:a.b.c.d` → `a.b.c.d`, so a dual-stack listener's mapped peer
/// address matches a ban written from a plain IPv4 spelling (and the reverse).
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
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
///
/// A file that exists but cannot be parsed is an error: the server refuses to
/// start rather than lifting bans or silently disabling the whitelist.
pub fn install(cfg: &Config) -> Result<(), String> {
    install_in(Path::new(&cfg.server.world_dir), cfg)
}

fn install_in(dir: &Path, cfg: &Config) -> Result<(), String> {
    let ban_path = dir.join(BAN_FILE);
    let white_path = dir.join(WHITELIST_FILE);
    let bans = match Bans::load(&ban_path) {
        Ok(Some(bans)) => bans,
        Ok(None) => {
            let bans = Bans::default();
            bans.save(&ban_path)?;
            bans
        }
        Err(e) => {
            return Err(format!(
                "{e}; refusing to start with the ban list unavailable (repair or remove `{}`)",
                ban_path.display()
            ))
        }
    };
    let whitelist = match Whitelist::load(&white_path) {
        Ok(Some(whitelist)) => whitelist,
        Ok(None) => {
            // The legacy config list is the first-start seed only. Once the
            // file exists it is the source of truth, exactly as the permission
            // file is after CMD-01's seed.
            let whitelist = Whitelist::from_config(&cfg.whitelist);
            whitelist.save(&white_path)?;
            whitelist
        }
        Err(e) => {
            return Err(format!(
                "{e}; refusing to start with the whitelist unavailable (repair or remove `{}`)",
                white_path.display()
            ))
        }
    };
    let mut guard = state().write().unwrap_or_else(|e| e.into_inner());
    guard.dir = Some(dir.to_path_buf());
    guard.data = AdminData { bans, whitelist };
    Ok(())
}

/// Re-read `whitelist.json` from disk, replacing the in-memory list. A parse
/// error leaves the previous list in place.
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

/// Whether the whitelist currently gates logins (from `whitelist.json` once
/// [`install`] has run, from `config.whitelist` otherwise).
pub fn whitelist_enabled(cfg: &Config) -> bool {
    let guard = state().read().unwrap_or_else(|e| e.into_inner());
    match guard.dir {
        Some(_) => guard.data.whitelist.enabled,
        None => !cfg.whitelist.is_empty(),
    }
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

/// The **ban** half of the login gate. Safe before authentication: the
/// claimed name and the peer address are all a ban needs.
pub fn ban_admission(name: &str, ip: Option<IpAddr>) -> Result<(), Admission> {
    let guard = state().read().unwrap_or_else(|e| e.into_inner());
    match guard.data.bans.matching(name, ip) {
        Some(entry) => Err(Admission::Banned(entry.reason.clone())),
        None => Ok(()),
    }
}

/// The **whitelist** half of the login gate, by the authenticated UUID. The
/// pre-auth step must not call this: a whitelist row keyed only by UUID can
/// admit a premium player under a new name, which is unknowable before auth.
pub fn whitelist_admission(
    name: &str,
    uuid: u128,
    ip: Option<IpAddr>,
    cfg: &Config,
) -> Result<(), Admission> {
    let guard = state().read().unwrap_or_else(|e| e.into_inner());
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

/// Both halves of the login gate, for a caller that already knows the real
/// UUID (post-authentication, and tests).
pub fn admit(name: &str, uuid: u128, ip: Option<IpAddr>, cfg: &Config) -> Result<(), Admission> {
    ban_admission(name, ip)?;
    whitelist_admission(name, uuid, ip, cfg)
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
    world: &'a DemoWorld,
    executor: Option<EntityRef>,
}

fn player_ref(handle: &PlayerHandle, world: &DemoWorld) -> EntityRef {
    let pos = handle.pos();
    let st = handle.game();
    let tags = st.tags.iter().cloned().collect();
    let level = st.xp_total;
    let mode = st.mode;
    drop(st);
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: Rotation::new(pos.yaw, pos.pitch),
        tags,
        team: crate::team::team_name_of(world, &handle.name),
        gamemode: Some(Gamemode::from(mode)),
        level,
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
            .map(|handle| player_ref(handle, self.world))
            .collect()
    }
}

fn online_source<'a>(
    registry: &'a SharedRegistry,
    world: &'a DemoWorld,
    source: Source<'a>,
) -> OnlineSource<'a> {
    OnlineSource {
        registry,
        world,
        executor: source.player().map(|p| player_ref(p, world)),
    }
}

/// Resolve one target token: a CMD-06b selector, or a plain player name.
///
/// A selector only names online players; a plain name may name an offline
/// profile when `allow_offline` (operators, bans, pardons, whitelist).
fn resolve_targets(
    registry: &SharedRegistry,
    world: &DemoWorld,
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
        let source = online_source(registry, world, source);
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

// ---------------------------------------------------------------------------
// Command nodes
// ---------------------------------------------------------------------------

fn command(name: &'static str, node: &'static str, usage: &'static str) -> tree::Builder {
    tree::Builder::literal(name)
        .requires(Requirement::Level { op: 3, node })
        .usage(usage)
}

fn literal(name: &'static str) -> tree::Builder {
    tree::Builder::literal(name)
}

/// The admin command nodes, with the vanilla grammar (CMD-08 review 1).
pub fn nodes() -> Vec<Node> {
    let banlist = command("banlist", "minecraft.command.banlist", "[ips|players]")
        .executable(exec_banlist_all)
        .then(literal("ips").executable(exec_banlist_ips))
        .then(literal("players").executable(exec_banlist_players))
        .build();

    let whitelist = command(
        "whitelist",
        "minecraft.command.whitelist",
        "<on|off|list|add|remove|reload>",
    )
    .executable(exec_whitelist_bare)
    .then(literal("on").executable(exec_whitelist_on))
    .then(literal("off").executable(exec_whitelist_off))
    .then(literal("list").executable(exec_whitelist_list))
    .then(literal("reload").executable(exec_whitelist_reload))
    .then(literal("add").then(
        tree::Builder::arg("targets", tree::ArgKind::GameProfile).executable(exec_whitelist_add),
    ))
    .then(literal("remove").then(
        tree::Builder::arg("targets", tree::ArgKind::GameProfile).executable(exec_whitelist_remove),
    ))
    .build();

    vec![
        command("op", "minecraft.command.op", "<targets>")
            .executable(exec_op)
            .then(tree::Builder::arg("targets", tree::ArgKind::GameProfile).executable(exec_op))
            .build(),
        command("deop", "minecraft.command.deop", "<targets>")
            .executable(exec_deop)
            .then(tree::Builder::arg("targets", tree::ArgKind::GameProfile).executable(exec_deop))
            .build(),
        command("ban", "minecraft.command.ban", "<targets> [<reason>]")
            .executable(exec_ban)
            .then(
                tree::Builder::arg("targets", tree::ArgKind::GameProfile)
                    .then(tree::Builder::arg("reason", tree::ArgKind::Message).executable(exec_ban))
                    .executable(exec_ban),
            )
            .build(),
        command("ban-ip", "minecraft.command.ban-ip", "<target> [<reason>]")
            .executable(exec_ban_ip)
            .then(
                tree::Builder::arg("target", tree::ArgKind::String)
                    .then(
                        tree::Builder::arg("reason", tree::ArgKind::Message)
                            .executable(exec_ban_ip),
                    )
                    .executable(exec_ban_ip),
            )
            .build(),
        banlist,
        command("pardon", "minecraft.command.pardon", "<targets>")
            .executable(exec_pardon)
            .then(tree::Builder::arg("targets", tree::ArgKind::GameProfile).executable(exec_pardon))
            .build(),
        command("pardon-ip", "minecraft.command.pardon-ip", "<target>")
            .executable(exec_pardon_ip)
            .then(tree::Builder::arg("target", tree::ArgKind::String).executable(exec_pardon_ip))
            .build(),
        command("kick", "minecraft.command.kick", "<targets> [<reason>]")
            .executable(exec_kick)
            .then(
                tree::Builder::arg("targets", tree::ArgKind::Entities)
                    .then(
                        tree::Builder::arg("reason", tree::ArgKind::Message).executable(exec_kick),
                    )
                    .executable(exec_kick),
            )
            .build(),
        whitelist,
    ]
}

/// The text of parsed argument `i` (empty when the grammar did not supply it).
fn arg_text(ctx: &Ctx, i: usize) -> String {
    match ctx.args.get(i) {
        Some(arg) => match &arg.value {
            Value::Text(text) => text.clone(),
            _ => arg.text.clone(),
        },
        None => String::new(),
    }
}

/// The optional reason argument's text.
fn reason_arg(ctx: &Ctx) -> Option<String> {
    ctx.args
        .get(1)
        .map(|arg| arg.text.trim().to_string())
        .filter(|reason| !reason.is_empty())
}

fn exec_op(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    match do_op(ctx, &target, true) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn exec_deop(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    match do_op(ctx, &target, false) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_op(ctx: &Ctx, target: &str, add: bool) -> Result<Vec<String>, String> {
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, target, true)?;
    let mut lines = Vec::new();
    let mut changed_any = false;
    for target in targets {
        let name = target.name().to_string();
        let changed = if add {
            crate::permissions::add_member(crate::permissions::OP_GROUP, &name, None)?
        } else {
            crate::permissions::remove_member(crate::permissions::OP_GROUP, &name)?
        };
        if changed {
            changed_any = true;
            lines.push(if add {
                format!("Made {name} a server operator")
            } else {
                format!("Made {name} no longer a server operator")
            });
        }
    }
    if !changed_any {
        return Err(if add {
            lang::OP_FAILED.to_string()
        } else {
            lang::DEOP_FAILED.to_string()
        });
    }
    Ok(lines)
}

fn exec_ban(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    let reason = reason_arg(ctx);
    match do_ban(ctx, &target, reason.as_deref()) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_ban(ctx: &Ctx, target: &str, reason: Option<&str>) -> Result<Vec<String>, String> {
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, target, true)?;
    let stored_reason = reason
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_BAN_REASON.to_string());
    let source = ctx.source.name().to_string();
    let mut lines = Vec::new();
    for target in targets {
        let name = target.name().to_string();
        let added = update_bans(|bans| Ok(bans.add_player(&name, &stored_reason, &source)))?;
        if !added {
            return Err(lang::BAN_FAILED.to_string());
        }
        if let Target::Online(handle) = target {
            handle.disconnect(&Admission::Banned(stored_reason.clone()).message());
        }
        lines.push(match reason {
            Some(reason) => format!("Banned {name}: {reason}"),
            None => format!("Banned {name}"),
        });
    }
    Ok(lines)
}

fn exec_ban_ip(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    let reason = reason_arg(ctx);
    match do_ban_ip(ctx, &target, reason.as_deref()) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

/// Resolve a `/ban-ip`/`/pardon-ip` target to canonical addresses: a literal
/// address, an online name, or a selector of online players.
fn target_ips(ctx: &Ctx, text: &str) -> Result<Vec<IpAddr>, String> {
    let text = text.trim();
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(vec![canonical_ip(ip)]);
    }
    if text.is_empty() {
        return Err(lang::BANIP_INVALID.into());
    }
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, text, false)
        .map_err(|_| lang::BANIP_INVALID.to_string())?;
    let mut out = Vec::new();
    for target in targets {
        match target {
            Target::Online(handle) => match handle.ip {
                Some(ip) => out.push(canonical_ip(ip)),
                None => return Err(lang::BANIP_INVALID.into()),
            },
            Target::Offline { .. } => return Err(lang::BANIP_INVALID.into()),
        }
    }
    Ok(out)
}

fn do_ban_ip(ctx: &Ctx, target: &str, reason: Option<&str>) -> Result<Vec<String>, String> {
    let ips = target_ips(ctx, target)?;
    let stored_reason = reason
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_BAN_REASON.to_string());
    let source = ctx.source.name().to_string();
    let mut lines = Vec::new();
    for ip in ips {
        let added = update_bans(|bans| Ok(bans.add_ip(ip, &stored_reason, &source)))?;
        if !added {
            return Err(lang::BANIP_FAILED.to_string());
        }
        // A literal IP names no handle, so kick every connected player from
        // that address (vanilla's `/ban-ip` does the same).
        let message = Admission::Banned(stored_reason.clone()).message();
        for handle in ctx.registry.snapshot() {
            if handle.ip.map(canonical_ip) == Some(ip) {
                handle.disconnect(&message);
            }
        }
        lines.push(match reason {
            Some(reason) => format!("Banned IP address {ip}: {reason}"),
            None => format!("Banned IP address {ip}"),
        });
    }
    Ok(lines)
}

fn exec_banlist_all(_ctx: &mut Ctx) -> Reply {
    let (players, ips) = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        (guard.data.bans.players.clone(), guard.data.bans.ips.clone())
    };
    banlist_lines(players.len() + ips.len(), players.iter().chain(ips.iter()))
}

fn exec_banlist_players(_ctx: &mut Ctx) -> Reply {
    let players = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        guard.data.bans.players.clone()
    };
    banlist_lines(players.len(), players.iter())
}

fn exec_banlist_ips(_ctx: &mut Ctx) -> Reply {
    let ips = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        guard.data.bans.ips.clone()
    };
    banlist_lines(ips.len(), ips.iter())
}

fn banlist_lines<'a>(total: usize, entries: impl Iterator<Item = &'a BanEntry>) -> Reply {
    if total == 0 {
        return Reply::one("There are no bans");
    }
    let mut lines = vec![format!("There are {total} bans:")];
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
    let target = arg_text(ctx, 0);
    match do_pardon(ctx, &target) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_pardon(ctx: &Ctx, target: &str) -> Result<Vec<String>, String> {
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, target, true)?;
    let mut lines = Vec::new();
    let mut changed_any = false;
    for target in targets {
        let name = target.name().to_string();
        let removed = update_bans(|bans| Ok(bans.remove_player(&name)))?;
        if removed {
            changed_any = true;
            lines.push(format!("Unbanned {name}"));
        }
    }
    if !changed_any {
        return Err(lang::PARDON_FAILED.to_string());
    }
    Ok(lines)
}

fn exec_pardon_ip(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    match do_pardon_ip(ctx, &target) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_pardon_ip(ctx: &Ctx, target: &str) -> Result<Vec<String>, String> {
    let ips = target_ips(ctx, target)?;
    let mut lines = Vec::new();
    let mut changed_any = false;
    for ip in ips {
        let removed = update_bans(|bans| Ok(bans.remove_ip(ip)))?;
        if removed {
            changed_any = true;
            lines.push(format!("Unbanned IP address {ip}"));
        }
    }
    if !changed_any {
        return Err(lang::PARDONIP_FAILED.to_string());
    }
    Ok(lines)
}

fn exec_kick(ctx: &mut Ctx) -> Reply {
    let target = arg_text(ctx, 0);
    let reason = reason_arg(ctx);
    match do_kick(ctx, &target, reason) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_kick(ctx: &Ctx, target: &str, reason: Option<String>) -> Result<Vec<String>, String> {
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, target, false)?;
    let disconnect_reason = reason.unwrap_or_else(|| DEFAULT_KICK_REASON.to_string());
    let mut lines = Vec::new();
    for target in targets {
        let Target::Online(handle) = target else {
            return Err("No player was found".into());
        };
        handle.disconnect(&disconnect_reason);
        lines.push(format!("Kicked {}: {disconnect_reason}", handle.name));
    }
    Ok(lines)
}

fn exec_whitelist_bare(ctx: &mut Ctx) -> Reply {
    ctx.fail("Unknown or incomplete command")
}

fn exec_whitelist_on(ctx: &mut Ctx) -> Reply {
    whitelist_switch(ctx, true)
}

fn exec_whitelist_off(ctx: &mut Ctx) -> Reply {
    whitelist_switch(ctx, false)
}

fn whitelist_switch(ctx: &mut Ctx, on: bool) -> Reply {
    let changed = update_whitelist(|whitelist| {
        if whitelist.enabled == on {
            return Ok(false);
        }
        whitelist.enabled = on;
        Ok(true)
    });
    match changed {
        Ok(true) if on => Reply::one("Whitelist is now turned on"),
        Ok(true) => Reply::one("Whitelist is now turned off"),
        Ok(false) if on => Reply::one(lang::WHITELIST_ALREADY_ON),
        Ok(false) => Reply::one(lang::WHITELIST_ALREADY_OFF),
        Err(e) => ctx.fail(e),
    }
}

fn exec_whitelist_list(_ctx: &mut Ctx) -> Reply {
    let whitelist = {
        let guard = state().read().unwrap_or_else(|e| e.into_inner());
        guard.data.whitelist.clone()
    };
    if whitelist.players.is_empty() {
        return Reply::one("There are no whitelisted players");
    }
    let mut names: Vec<String> = whitelist.players.iter().map(|p| p.name.clone()).collect();
    names.sort();
    let mut lines = vec![format!("There are {} whitelisted players:", names.len())];
    lines.extend(names);
    Reply(lines)
}

fn exec_whitelist_reload(ctx: &mut Ctx) -> Reply {
    match reload_whitelist() {
        Ok(()) => Reply::one("Reloaded the whitelist"),
        Err(e) => ctx.fail(e),
    }
}

fn exec_whitelist_add(ctx: &mut Ctx) -> Reply {
    whitelist_target(ctx, true)
}

fn exec_whitelist_remove(ctx: &mut Ctx) -> Reply {
    whitelist_target(ctx, false)
}

fn whitelist_target(ctx: &mut Ctx, add: bool) -> Reply {
    let target = arg_text(ctx, 0);
    match do_whitelist_target(ctx, &target, add) {
        Ok(lines) => Reply(lines),
        Err(e) => ctx.fail(e),
    }
}

fn do_whitelist_target(ctx: &Ctx, target: &str, add: bool) -> Result<Vec<String>, String> {
    let targets = resolve_targets(ctx.registry, ctx.world, ctx.source, target, true)?;
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
                lang::WHITELIST_ADD_FAILED.to_string()
            } else {
                lang::WHITELIST_REMOVE_FAILED.to_string()
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
        install_in(&dir, &cfg).unwrap();
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

    fn install_cfg(env: &Env) -> Config {
        let mut cfg = cfg();
        cfg.server.world_dir = env.dir.to_string_lossy().into_owned();
        cfg
    }

    #[test]
    fn op_adds_a_name_and_deop_removes_it_without_widening_a_bound_entry() {
        let env = env("op");
        assert_eq!(one(&env, "/op alice"), "Made alice a server operator");
        assert!(crate::permissions::is_operator("alice", None));
        assert_eq!(
            err(&env, "/op alice"),
            "Nothing changed. The player already is an operator"
        );

        // The config seed `bound@10.0.0.7` stays bound: `/op bound` neither
        // reports a new grant nor widens it to every address.
        assert_eq!(
            err(&env, "/op bound"),
            "Nothing changed. The player already is an operator"
        );
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
        assert_eq!(
            err(&env, "/deop alice"),
            "Nothing changed. The player isn't an operator"
        );
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
        let cfg = install_cfg(&env);
        let refusal = admit(
            "carol",
            aether_api::player::offline_uuid("carol"),
            None,
            &cfg,
        )
        .unwrap_err();
        assert_eq!(
            refusal.message(),
            "You are banned from this server! cheating"
        );
        // Reloading the process-wide store from the file keeps the ban.
        install_in(&env.dir, &cfg).unwrap();
        assert!(admit("carol", 0, None, &cfg).is_err());
    }

    #[test]
    fn repeated_operations_use_the_vanilla_failure_strings() {
        let env = env("dupes");
        assert_eq!(one(&env, "/ban alice"), "Banned alice");
        assert_eq!(
            err(&env, "/ban alice"),
            "Nothing changed. The player is already banned"
        );
        assert_eq!(one(&env, "/ban-ip 10.0.0.5"), "Banned IP address 10.0.0.5");
        assert_eq!(
            err(&env, "/ban-ip 10.0.0.5"),
            "Nothing changed. That IP is already banned"
        );
        assert_eq!(
            err(&env, "/pardon nobody"),
            "Nothing changed. The player isn't banned"
        );
        assert_eq!(
            err(&env, "/pardon-ip 10.0.0.6"),
            "Nothing changed. That IP isn't banned"
        );
    }

    #[test]
    fn pardoning_something_that_is_not_banned_is_an_error_not_a_success() {
        let env = env("pardon");
        assert!(err(&env, "/pardon nobody").contains("Nothing changed"));
        assert_eq!(one(&env, "/ban dave"), "Banned dave");
        assert_eq!(one(&env, "/pardon dave"), "Unbanned dave");
        assert!(admit("dave", 0, None, &install_cfg(&env)).is_ok());
    }

    #[test]
    fn an_ip_ban_applies_by_address_and_a_name_ban_applies_by_name() {
        let env = env("ban-ip");
        let cfg = install_cfg(&env);
        assert_eq!(
            one(&env, "/ban-ip 10.0.0.9 scanning"),
            "Banned IP address 10.0.0.9: scanning"
        );
        assert!(admit("anyone", 1, Some("10.0.0.9".parse().unwrap()), &cfg).is_err());
        assert!(admit("anyone", 1, Some("10.0.0.8".parse().unwrap()), &cfg).is_ok());

        assert_eq!(one(&env, "/ban erin"), "Banned erin");
        assert!(
            admit("erin", 1, Some("10.0.0.8".parse().unwrap()), &cfg).is_err(),
            "a name ban follows the name to another address"
        );
    }

    #[test]
    fn a_literal_ip_ban_kicks_every_connected_player_from_that_address() {
        let env = env("ban-ip-kick");
        assert_eq!(one(&env, "/ban-ip 10.0.0.2"), "Banned IP address 10.0.0.2");
        assert!(
            env.bob.disconnected().is_some(),
            "a literal IP ban must kick bob, who is connected from it"
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_addresses_are_canonicalised() {
        let env = env("mapped");
        let cfg = install_cfg(&env);
        assert_eq!(
            one(&env, "/ban-ip ::ffff:10.0.0.9"),
            "Banned IP address 10.0.0.9"
        );
        assert!(
            admit("anyone", 1, Some("10.0.0.9".parse().unwrap()), &cfg).is_err(),
            "a mapped ban must match the plain IPv4 address"
        );
        // And a plain ban must match a mapped peer.
        assert_eq!(
            one(&env, "/ban-ip 10.0.0.10"),
            "Banned IP address 10.0.0.10"
        );
        assert!(admit("anyone", 1, Some("::ffff:10.0.0.10".parse().unwrap()), &cfg).is_err());
    }

    #[test]
    fn banlist_uses_one_header_for_every_filter() {
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
                "There are 1 bans:".to_string(),
                "alice was banned by tester: Banned by an operator.".to_string(),
            ]
        );
        assert_eq!(
            ok(&env, "/banlist ips"),
            vec![
                "There are 1 bans:".to_string(),
                "10.0.0.5 was banned by tester: Banned by an operator.".to_string(),
            ]
        );
    }

    #[test]
    fn kick_always_names_the_reason_including_the_default() {
        let env = env("kick");
        assert_eq!(one(&env, "/kick bob because"), "Kicked bob: because");
        assert!(env.bob.disconnected().unwrap().contains("because"));
        assert_eq!(
            one(&env, "/kick tester"),
            "Kicked tester: Kicked by an operator"
        );
        assert_eq!(env.tester.disconnected().unwrap(), "Kicked by an operator");
    }

    #[test]
    fn whitelist_on_off_add_remove_list_and_reload_are_real_gates() {
        let env = env("white");
        let cfg = install_cfg(&env);
        assert!(!whitelist_enabled(&cfg));
        assert_eq!(one(&env, "/whitelist on"), "Whitelist is now turned on");
        assert_eq!(one(&env, "/whitelist on"), "Whitelist is already turned on");
        assert!(
            admit(
                "alice",
                aether_api::player::offline_uuid("alice"),
                None,
                &cfg
            )
            .is_err(),
            "an empty whitelist refuses everyone"
        );
        // An operator still gets in while the whitelist is on.
        assert!(admit("tester", 1, None, &cfg).is_ok());

        assert_eq!(
            one(&env, "/whitelist add alice"),
            "Added alice to the whitelist"
        );
        assert_eq!(
            err(&env, "/whitelist add alice"),
            "Player is already whitelisted"
        );
        let alice_uuid = aether_api::player::offline_uuid("alice");
        assert!(admit("alice", alice_uuid, None, &cfg).is_ok());
        // The entry carries the UUID, so a later profile that presents the
        // same UUID matches even if the name casing differs.
        assert!(admit("ALICE", alice_uuid, None, &cfg).is_ok());
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
        assert_eq!(
            err(&env, "/whitelist remove alice"),
            "Player is not whitelisted"
        );
        assert!(admit("alice", alice_uuid, None, &cfg).is_err());
        assert_eq!(one(&env, "/whitelist off"), "Whitelist is now turned off");
        assert_eq!(
            one(&env, "/whitelist off"),
            "Whitelist is already turned off"
        );
        assert!(admit("alice", alice_uuid, None, &cfg).is_ok());
    }

    #[test]
    fn a_uuid_only_whitelist_entry_admits_a_renamed_premium_player_after_auth() {
        let env = env("white-uuid");
        let cfg = install_cfg(&env);
        // The real Mojang UUID, stored under the old name.
        let real = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00u128;
        let text = format!(
            "{{\"enabled\":true,\"players\":[{{\"name\":\"oldname\",\"uuid\":\"{}\"}}]}}",
            uuid_hex(real)
        );
        std::fs::write(env.dir.join(WHITELIST_FILE), text).unwrap();
        reload_whitelist().unwrap();

        // Pre-auth knows only the claimed name and its offline UUID: the ban
        // half must not reject, or the post-auth UUID check never runs.
        assert!(ban_admission("newname", None).is_ok());
        // After auth the real UUID matches the row despite the new name.
        assert!(whitelist_admission("newname", real, None, &cfg).is_ok());
        assert!(admit("newname", real, None, &cfg).is_ok());
        // Without it, the same name is refused.
        assert!(admit(
            "newname",
            aether_api::player::offline_uuid("newname"),
            None,
            &cfg
        )
        .is_err());
    }

    #[test]
    fn whitelist_reload_rereads_the_file() {
        let env = env("white-reload");
        let cfg = install_cfg(&env);
        assert_eq!(one(&env, "/whitelist on"), "Whitelist is now turned on");
        // A hand-edited vanilla-style hyphenated UUID still matches.
        let text = format!(
            "{{\"enabled\":true,\"players\":[{{\"name\":\"dana\",\"uuid\":\"{}\"}}]}}",
            crate::auth::api::hyphenated_uuid(aether_api::player::offline_uuid("dana"))
        );
        std::fs::write(env.dir.join(WHITELIST_FILE), text).unwrap();
        assert_eq!(one(&env, "/whitelist reload"), "Reloaded the whitelist");
        assert!(admit("dana", aether_api::player::offline_uuid("dana"), None, &cfg).is_ok());
        assert!(admit(
            "alice",
            aether_api::player::offline_uuid("alice"),
            None,
            &cfg
        )
        .is_err());
    }

    #[test]
    fn a_corrupt_whitelist_or_ban_file_refuses_to_start_instead_of_opening_the_server() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("corrupt");

        let white_path = dir.join(WHITELIST_FILE);
        std::fs::write(&white_path, "{not json").unwrap();
        let cfg = Config::default();
        assert!(install_in(&dir, &cfg).is_err());
        assert_eq!(
            std::fs::read_to_string(&white_path).unwrap(),
            "{not json",
            "a broken whitelist must not be overwritten"
        );

        let dir = temp_dir("corrupt-bans");
        let ban_path = dir.join(BAN_FILE);
        std::fs::write(&ban_path, "{\"players\": 3").unwrap();
        assert!(install_in(&dir, &Config::default()).is_err());
        assert_eq!(
            std::fs::read_to_string(&ban_path).unwrap(),
            "{\"players\": 3",
            "a broken ban list must not be overwritten"
        );
    }

    #[test]
    fn a_selector_target_is_resolved_and_unresolved_targets_are_errors() {
        let env = env("selector");
        assert_eq!(err(&env, "/op @a[name=nobody]"), "No player was found");
        assert_eq!(err(&env, "/kick nobody"), "No player was found");
        // A quoted selector filter with a space is one argument, not two.
        assert_eq!(
            err(&env, "/ban @a[name=\"Bob Smith\"]"),
            "No player was found"
        );
        assert_eq!(
            one(&env, "/ban @a[name=\"bob\"] flood"),
            "Banned bob: flood"
        );
        assert!(env.bob.disconnected().is_some());
    }

    #[test]
    fn extra_arguments_and_unknown_subcommands_are_parse_errors() {
        let env = env("grammar");
        assert_eq!(
            err(&env, "/op alice extra"),
            "Incorrect argument for command"
        );
        assert_eq!(
            err(&env, "/pardon alice extra"),
            "Incorrect argument for command"
        );
        assert_eq!(err(&env, "/banlist nope"), "Unknown or incomplete command");
        assert_eq!(
            err(&env, "/whitelist add alice extra"),
            "Incorrect argument for command"
        );
        assert_eq!(
            err(&env, "/whitelist nope"),
            "Unknown or incomplete command"
        );
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
