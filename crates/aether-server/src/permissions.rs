//! The permission core: op levels, permission nodes, and the one check every
//! command and hot-path gate goes through.
//!
//! Before CMD-01 the only gate was [`crate::is_operator`] — a name looked up in
//! `config.operators`. That boolean is now a thin wrapper over this module, and
//! the model behind it is the vanilla one:
//!
//! * a **level** is `0..=4` (vanilla `CommandSourceStack`: 0 everyone, 1 bypass
//!   spawn protection, 2 gamemasters, 3 admins, 4 owners). It is clamped on the
//!   way in, so a configured `9` behaves as `4`.
//! * a **node** is a lower-case, dot-separated name (`aether.command.ban`,
//!   `worldguard.flag.pvp`) drawn from `[a-z0-9_.-]`. An upper-case or empty
//!   segment is rejected: a malformed node is a deny, never a match.
//! * a node's **default** is `true` / `false` / `op`.
//!
//! A check resolves in this order:
//!
//! 1. an explicit per-player grant or deny (empty today; CMD-02 fills it),
//! 2. the node's declared default:
//!    * `Allow` — the node adds no operator requirement; the command's own op
//!      level is the only remaining gate;
//!    * `Deny`  — no one, operator or not;
//!    * `Op`    — an operator (level ≥ 1), and still at least the op level;
//! 3. nothing declared — the node is unknown, and defaults to `op`: only an
//!    operator, never `true`. A typo therefore fails closed.
//!
//! The command's own **op level** is a floor that always applies — a command
//! entry naming `op = 3` is not runnable below level 3 whatever its node says —
//! and an unknown or `op` node additionally requires an operator. Command nodes
//! are registered with that rule (op 0 → `Allow`, op > 0 → `Op`), so a
//! command's effective requirement is exactly its op level, as it was before
//! this module existed.
//!
//! ## `config.operators` is a seed, not the source of truth
//!
//! [`install`] applies every `config.operators` entry as a member of the
//! built-in group [`OP_GROUP`] (level 4). The store is authoritative from then
//! on; a later `/op`/`/deop` (CMD-08) or group edit (CMD-02) changes the store,
//! not the config. Reconciliation at startup is deliberately **add-only**: an
//! entry already in the store is not duplicated, and removing a name from the
//! config does **not** silently deop — deoping is an explicit action, so a
//! fat-fingered config edit cannot quietly strip the owner. The `op` group
//! cannot be deleted.
//!
//! The store is **persisted** to `<world_dir>/permissions.toml`, written
//! atomically (temp file + rename), so the add-only rule holds **across a
//! restart**: the file is loaded first, the config is then merged in, and a
//! name the file already holds survives even when the config no longer lists
//! it. Without persistence the rule would only hold within one process, which
//! is why CMD-01 owns the file.
//!
//! Groups, inheritance and per-world context are CMD-02; CMD-01 is the level /
//! node model, the store and the [`check`] API. The server is the only consumer
//! today, so the module lives here rather than in `aether-api`.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// The highest vanilla permission level: an owner.
const MAX_LEVEL: u8 = 4;

/// A vanilla permission level, clamped to `0..=4`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Level(u8);

impl Level {
    /// Level 0: everyone.
    pub const NONE: Level = Level(0);
    /// Level 4: an owner.
    pub const OWNER: Level = Level(MAX_LEVEL);

    /// A level, clamped: a configured `9` is an owner, and there is no level
    /// below none.
    pub const fn new(raw: u8) -> Level {
        Level(if raw > MAX_LEVEL { MAX_LEVEL } else { raw })
    }

    /// The raw level, already in range.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for Level {
    fn from(raw: u8) -> Level {
        Level::new(raw)
    }
}

/// The value a node resolves to when no per-player grant/deny applies.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeDefault {
    /// Everyone, regardless of level.
    Allow,
    /// No one, not even an operator.
    Deny,
    /// An operator only (level ≥ 1).
    Op,
}

/// The built-in group every operator belongs to.
pub const OP_GROUP: &str = "op";

/// Its level: an operator is a vanilla owner.
pub const OP_LEVEL: Level = Level::OWNER;

/// The file the store is persisted to, under the world directory. Atomic
/// replace means a crash mid-write leaves the previous file intact.
pub const STORE_FILE: &str = "permissions.toml";

/// Whether `node` is a legal permission node.
///
/// Lower-case, dot-separated segments over `[a-z0-9_.-]`, none empty: `true`
/// for `minecraft.command.ban`, `false` for `Minecraft.Command.Ban`, `""`,
/// `.leading`, `trailing.` or `a..b`. A rejected name is denied by [`check`],
/// so a typo cannot match a node nobody declared.
pub fn valid_node(node: &str) -> bool {
    !node.is_empty() && node.split('.').all(valid_segment)
}

fn valid_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// One group member on the wire: an optional required address, omitted for a
/// bare name. TOML has no null, so `None` is an absent `addr` key.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Member {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    addr: Option<String>,
}

/// A group of players at one permission level.
///
/// CMD-01 has only the built-in [`OP_GROUP`]; inheritance and meta are CMD-02.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    id: String,
    level: Level,
    builtin: bool,
    /// Member name (lower-cased) → its binding. A bare entry has no address.
    members: BTreeMap<String, Member>,
}

impl Group {
    /// The group id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The level every member gets.
    pub fn level(&self) -> Level {
        self.level
    }

    /// Whether this group is built in — and therefore not deletable.
    pub fn is_builtin(&self) -> bool {
        self.builtin
    }

    /// The members, lower-cased, with their required address if any.
    pub fn members(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.members
            .iter()
            .map(|(name, m)| (name.as_str(), m.addr.as_deref()))
    }

    /// Whether `name`, connecting from `ip`, is a member.
    pub fn contains(&self, name: &str, ip: Option<IpAddr>) -> bool {
        match self.members.get(&name.to_ascii_lowercase()) {
            // A bare name: any address.
            Some(Member { addr: None }) => true,
            // A bound name: only that address. An address that does not parse
            // is no address, exactly as `Config::is_operator` treats it.
            Some(Member { addr: Some(addr) }) => {
                ip.is_some_and(|ip| addr.trim().parse::<IpAddr>() == Ok(ip))
            }
            None => false,
        }
    }
}

/// The permission store: node defaults and groups.
///
/// The server holds one process-wide instance behind a lock (see [`store`]);
/// [`Store`] is public so the model can be unit-tested and reused by CMD-02.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    nodes: BTreeMap<String, NodeDefault>,
    groups: BTreeMap<String, Group>,
}

impl Default for Store {
    /// The same as [`Store::new`]: the invariant that `op` exists must hold
    /// however the store is built, so a derived empty default is not allowed.
    fn default() -> Self {
        Store::new()
    }
}

impl Store {
    /// A store with the built-in `op` group and no members.
    pub fn new() -> Store {
        let mut store = Store {
            nodes: BTreeMap::new(),
            groups: BTreeMap::new(),
        };
        store.normalize();
        store
    }

    /// Restore the invariants the store must always hold: every level is in
    /// range, and the built-in `op` group exists, is level 4, and cannot be
    /// deleted. Applied to a freshly built store and to one read from disk, so
    /// a corrupt or hand-edited file cannot push a level past 4 or weaken `op`.
    fn normalize(&mut self) {
        for group in self.groups.values_mut() {
            group.level = Level::new(group.level.get());
        }
        let op = self
            .groups
            .entry(OP_GROUP.to_string())
            .or_insert_with(|| Group {
                id: OP_GROUP.to_string(),
                level: OP_LEVEL,
                builtin: true,
                members: BTreeMap::new(),
            });
        op.id = OP_GROUP.to_string();
        op.level = OP_LEVEL;
        op.builtin = true;
    }

    /// Declare (or redeclare) a node's default.
    pub fn declare_node(&mut self, node: &str, default: NodeDefault) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        self.nodes.insert(node.to_string(), default);
        Ok(())
    }

    /// A node's declared default, if any.
    pub fn node_default(&self, node: &str) -> Option<NodeDefault> {
        self.nodes.get(node).copied()
    }

    /// The named group, if any.
    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.get(id)
    }

    /// Every group.
    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.values()
    }

    /// The ids of every group `name`, from `ip`, belongs to.
    pub fn groups_of(&self, name: &str, ip: Option<IpAddr>) -> Vec<String> {
        self.groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.id.clone())
            .collect()
    }

    /// Create a non-built-in group. CMD-01 only needs this for tests and for
    /// the shape CMD-02 will extend; the built-in `op` group already exists.
    pub fn create_group(&mut self, id: &str, level: Level) -> Result<(), String> {
        if id.is_empty() {
            return Err("a group id is empty".to_string());
        }
        if self.groups.contains_key(id) {
            return Err(format!("group `{id}` already exists"));
        }
        self.groups.insert(
            id.to_string(),
            Group {
                id: id.to_string(),
                level,
                builtin: false,
                members: BTreeMap::new(),
            },
        );
        Ok(())
    }

    /// Delete a group. Refuses the built-in `op` group: it is the group
    /// `is_operator` maps to, and an operator must always have a home.
    pub fn delete_group(&mut self, id: &str) -> Result<(), String> {
        match self.groups.get(id) {
            None => Err(format!("no group `{id}`")),
            Some(g) if g.builtin => Err(format!("the built-in group `{id}` cannot be deleted")),
            Some(_) => {
                self.groups.remove(id);
                Ok(())
            }
        }
    }

    /// Add `name` to `group`, optionally requiring `addr`. Returns whether the
    /// member is new.
    ///
    /// Add-only in the sense that a member already present is never
    /// **weakened**: binding an address applies, but a later bare entry never
    /// clears an existing binding — a config edit cannot turn `admin@addr`
    /// into an operator from anywhere. Two different addresses reseat the
    /// binding to the later one.
    pub fn add_member(
        &mut self,
        group: &str,
        name: &str,
        addr: Option<&str>,
    ) -> Result<bool, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a member name is empty".to_string());
        }
        let group = self
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        let key = name.to_ascii_lowercase();
        let new_addr = addr.map(str::to_string);
        match group.members.get_mut(&key) {
            Some(member) => {
                if new_addr.is_some() {
                    member.addr = new_addr;
                }
                Ok(false)
            }
            None => {
                group.members.insert(key, Member { addr: new_addr });
                Ok(true)
            }
        }
    }

    /// Apply `config.operators` entries as members of `op`. `name` or
    /// `name@address`, exactly as the config reads them. Returns how many were
    /// newly added — zero when the store already knows every entry.
    pub fn seed_operators(&mut self, entries: &[String]) -> usize {
        let mut added = 0;
        for entry in entries {
            let (name, addr) = match entry.split_once('@') {
                Some((n, a)) => (n.trim(), Some(a.trim())),
                None => (entry.trim(), None),
            };
            if name.is_empty() {
                continue;
            }
            if let Ok(true) = self.add_member(OP_GROUP, name, addr) {
                added += 1;
            }
        }
        added
    }

    /// A player's effective level: the highest level among the groups they
    /// belong to, or none.
    pub fn level_of(&self, name: &str, ip: Option<IpAddr>) -> Level {
        self.groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.level)
            .max()
            .unwrap_or(Level::NONE)
    }

    /// The decision: may a player at `level` use `node`, for a command needing
    /// `op`?
    ///
    /// A malformed node is denied outright. [`NodeDefault::Allow`] removes the
    /// node's own operator requirement, but the command's `op` level still
    /// applies — that is what lets one node default serve a public op-0 command
    /// and a gated op-3 command. [`NodeDefault::Deny`] permits no one.
    /// [`NodeDefault::Op`] and an undeclared node are alike: operators only, never
    /// `true`, so a typo fails closed. This is the one place the three sources
    /// (node default, op level, unknown) are combined; every caller goes
    /// through it.
    pub fn check(&self, level: Level, node: &str, op: u8) -> bool {
        if !valid_node(node) {
            return false;
        }
        let op = Level::new(op).get();
        match self.nodes.get(node) {
            Some(NodeDefault::Allow) => level.get() >= op,
            Some(NodeDefault::Deny) => false,
            Some(NodeDefault::Op) | None => level.get() >= op.max(1),
        }
    }

    /// Encode the store as TOML.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| format!("cannot encode permissions: {e}"))
    }

    /// Decode a store, restoring the invariants a hand-edited file could break.
    pub fn from_toml(text: &str) -> Result<Store, String> {
        let mut store: Store =
            toml::from_str(text).map_err(|e| format!("cannot parse permissions: {e}"))?;
        store.normalize();
        Ok(store)
    }

    /// Read the store from `path`, or `None` when the file does not exist (a
    /// first start). A file that exists but does not parse is an error, so the
    /// caller can choose to keep the previous store rather than silently reset.
    pub fn load(path: &Path) -> Result<Option<Store>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Store::from_toml(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read `{}`: {e}", path.display())),
        }
    }

    /// Write the store to `path` atomically: the bytes land in a sibling temp
    /// file, then a rename replaces the target, so a crash mid-write leaves
    /// either the old file or the new one, never a half one.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
            }
        }
        let text = self.to_toml()?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| format!("cannot write `{}`: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("cannot replace `{}`: {e}", path.display()))?;
        Ok(())
    }
}

/// The process-wide store, created empty on first use.
fn store() -> &'static RwLock<Store> {
    static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(Store::new()))
}

/// Where the process-wide store persists, set by [`install`]. Absent when the
/// core was never installed (tests, tools), in which case [`save`] is a no-op.
fn path() -> &'static OnceLock<PathBuf> {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    &PATH
}

fn read() -> RwLockReadGuard<'static, Store> {
    store().read().unwrap_or_else(|e| e.into_inner())
}

fn write() -> RwLockWriteGuard<'static, Store> {
    store().write().unwrap_or_else(|e| e.into_inner())
}

/// Install the permission core from the config: load the persisted store, seed
/// the `op` group from `config.operators`, declare every core command's node,
/// and write the result back.
///
/// Called once at startup, before the listener accepts a connection. Loading
/// before seeding is what makes "removing a name from the config does not
/// deop" true across a restart. A file that exists but cannot be read is a
/// warning, not a refusal — the server still starts, but it starts from the
/// seed, so the operator is told.
pub fn install(cfg: &Config) {
    let path = Path::new(&cfg.server.world_dir).join(STORE_FILE);
    let _ = self::path().set(path.clone());
    let mut store = match Store::load(&path) {
        Ok(Some(s)) => {
            crate::log::info(&format!("permissions: loaded `{}`", path.display()));
            s
        }
        Ok(None) => Store::new(),
        Err(e) => {
            crate::log::warn(&format!(
                "permissions: `{}` is unreadable ({e}); starting from the config seed",
                path.display()
            ));
            Store::new()
        }
    };
    let added = store.seed_operators(&cfg.operators);
    for entry in crate::commands::registry::CORE {
        let _ = declare_command_node_in(&mut store, entry.node, entry.op);
    }
    *write() = store;
    if added > 0 {
        crate::log::info(&format!(
            "permissions: seeded {added} operator(s) into the `{OP_GROUP}` group"
        ));
    }
    if let Err(e) = save() {
        crate::log::warn(&format!("permissions: cannot persist the store: {e}"));
    }
}

/// Persist the process-wide store to its file. A no-op before [`install`].
pub fn save() -> Result<(), String> {
    match path().get() {
        Some(p) => read().save(p),
        None => Ok(()),
    }
}

/// Register a command's node with the default its op level implies: a public
/// (op 0) command allows everyone, a gated one is operator-only (its op level
/// is still enforced by [`check`]).
///
/// Idempotent and non-destructive: an existing declaration is kept, so a
/// command registration never clobbers a node an operator set explicitly. A
/// malformed node is an error, so a caller can refuse the command rather than
/// register a dead one.
pub fn declare_command_node(node: &str, op: u8) -> Result<(), String> {
    declare_command_node_in(&mut write(), node, op)
}

fn declare_command_node_in(store: &mut Store, node: &str, op: u8) -> Result<(), String> {
    if store.node_default(node).is_some() {
        return Ok(());
    }
    let default = if op == 0 {
        NodeDefault::Allow
    } else {
        NodeDefault::Op
    };
    store.declare_node(node, default)
}

/// Add `config.operators`-style entries to the `op` group of the process-wide
/// store, without touching node defaults or other groups.
///
/// This is the seeding half of [`install`], split out so a caller that already
/// has a store — including a test — can grant operator rights without resetting
/// everything else the store holds. Add-only, exactly like [`Store::seed_operators`].
pub fn seed_operators(entries: &[String]) -> usize {
    write().seed_operators(entries)
}

/// A lock that serialises tests which mutate or depend on the process-wide
/// store.
///
/// [`install`] replaces the whole store (its job: the persisted file is the
/// source of truth), so a test that reads operator membership must not run
/// while an install test does — otherwise the operator it just seeded is
/// wiped. Production never hits this: `install` runs once, before any reader.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A player's effective level in the process-wide store.
pub fn level(name: &str, ip: Option<IpAddr>) -> Level {
    read().level_of(name, ip)
}

/// The groups `name`, from `ip`, belongs to, in the process-wide store.
pub fn groups_of(name: &str, ip: Option<IpAddr>) -> Vec<String> {
    read().groups_of(name, ip)
}

/// Whether `name`, from `ip`, is an operator (level ≥ 1).
pub fn is_operator(name: &str, ip: Option<IpAddr>) -> bool {
    level(name, ip).get() >= 1
}

/// The process-wide [`Store::check`].
pub fn check(level: Level, node: &str, op: u8) -> bool {
    read().check(level, node, op)
}

/// Declare a node's default in the process-wide store.
pub fn declare_node(node: &str, default: NodeDefault) -> Result<(), String> {
    write().declare_node(node, default)
}

/// A node's declared default in the process-wide store.
pub fn node_default(node: &str) -> Option<NodeDefault> {
    read().node_default(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    /// A unique file under the temp dir, so parallel tests cannot collide.
    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aether-perm-{tag}-{}.toml", std::process::id()))
    }

    #[test]
    fn levels_are_clamped_to_the_vanilla_range() {
        assert_eq!(Level::new(0).get(), 0);
        assert_eq!(Level::new(2).get(), 2);
        assert_eq!(Level::new(4).get(), 4);
        // A configured 9 behaves as an owner, not as a level nothing can meet.
        assert_eq!(Level::new(9).get(), 4);
        assert_eq!(Level::from(200), Level::OWNER);
        assert!(Level::new(3) < Level::OWNER);
    }

    #[test]
    fn node_names_are_validated() {
        for ok in [
            "aether.command.ban",
            "worldguard.flag.pvp",
            "a",
            "a-b.c_d.1",
            "minecraft.command.ban-ip",
        ] {
            assert!(valid_node(ok), "`{ok}` should be a legal node");
        }
        for bad in [
            "",
            "Aether.Command",
            "a..b",
            ".a",
            "a.",
            "a b",
            "a/b",
            "a@b",
        ] {
            assert!(!valid_node(bad), "`{bad}` should be rejected");
        }
    }

    #[test]
    fn an_operator_passes_every_level_and_a_non_op_only_zero() {
        let mut s = Store::new();
        s.declare_node("test.allow", NodeDefault::Allow).unwrap();
        // The node is permissive; the op level is the only gate left.
        for op in 0..=4 {
            assert!(s.check(Level::OWNER, "test.allow", op), "op level {op}");
        }
        assert!(s.check(Level::NONE, "test.allow", 0));
        for op in 1..=4 {
            assert!(!s.check(Level::NONE, "test.allow", op), "op level {op}");
        }
    }

    #[test]
    fn allow_passes_a_non_op_and_deny_fails_an_operator() {
        let mut s = Store::new();
        s.declare_node("test.allow", NodeDefault::Allow).unwrap();
        s.declare_node("test.deny", NodeDefault::Deny).unwrap();
        assert!(s.check(Level::NONE, "test.allow", 0));
        // A deny is absolute: level 4 does not override it.
        assert!(!s.check(Level::OWNER, "test.deny", 0));
        assert!(!s.check(Level::OWNER, "test.deny", 4));
    }

    #[test]
    fn an_op_node_is_operator_only() {
        let mut s = Store::new();
        s.declare_node("test.op", NodeDefault::Op).unwrap();
        assert!(!s.check(Level::NONE, "test.op", 0));
        assert!(s.check(Level::new(1), "test.op", 0));
        assert!(s.check(Level::new(1), "test.op", 1));
        assert!(s.check(Level::OWNER, "test.op", 4));
    }

    #[test]
    fn unknown_and_malformed_nodes_fail_closed() {
        let s = Store::new();
        // An undeclared node is operator-only, never `true`: a typo is denied
        // to a non-op whatever op level the caller names.
        assert!(!s.check(Level::NONE, "definitely.unknown", 0));
        assert!(!s.check(Level::NONE, "definitely.unknown", 4));
        assert!(s.check(Level::OWNER, "definitely.unknown", 0));
        assert!(s.check(Level::new(1), "definitely.unknown", 0));
        // A gated unknown node still enforces the op level.
        assert!(!s.check(Level::new(2), "definitely.unknown", 3));
        assert!(s.check(Level::new(3), "definitely.unknown", 3));
        // A malformed name is a deny even for an owner.
        assert!(!s.check(Level::OWNER, "Bad.Node", 0));
        assert!(!s.check(Level::OWNER, "", 0));
    }

    #[test]
    fn the_op_group_levels_its_members_and_honours_address_binding() {
        let mut s = Store::new();
        let added = s.seed_operators(&["Root".into(), "admin@10.0.0.7".into()]);
        assert_eq!(added, 2);
        let op = s.group(OP_GROUP).expect("the op group exists");
        assert_eq!(op.level(), OP_LEVEL);
        assert!(op.is_builtin());
        // A bare name is an operator anywhere; the case does not matter.
        assert_eq!(s.level_of("root", None), Level::OWNER);
        assert_eq!(s.level_of("ROOT", ip("1.2.3.4")), Level::OWNER);
        // A bound name is an operator only from its address.
        assert_eq!(s.level_of("admin", ip("10.0.0.7")), Level::OWNER);
        assert_eq!(s.level_of("admin", None), Level::NONE);
        assert_eq!(s.level_of("admin", ip("10.0.0.8")), Level::NONE);
        assert_eq!(s.level_of("nobody", None), Level::NONE);
    }

    #[test]
    fn the_default_store_still_has_the_op_group() {
        // `Store::default()` must not produce a store without `op`: the
        // invariant is restored by the manual `Default`.
        let s = Store::default();
        let op = s.group(OP_GROUP).expect("default store has `op`");
        assert!(op.is_builtin());
        assert_eq!(op.level(), OP_LEVEL);
        // And a derive-shaped default is gone: `nodes`/`groups` are the same as
        // `Store::new`'s.
        assert_eq!(s.groups().count(), Store::new().groups().count());
    }

    #[test]
    fn adding_a_binding_strengthens_and_a_bare_entry_never_weakens() {
        let mut s = Store::new();
        // A bare member first...
        assert!(s.add_member(OP_GROUP, "admin", None).unwrap());
        assert_eq!(s.level_of("admin", None), Level::OWNER);
        // ...then a binding: applied, not treated as a duplicate.
        assert!(!s.add_member(OP_GROUP, "admin", Some("10.0.0.7")).unwrap());
        assert_eq!(s.level_of("admin", None), Level::NONE);
        assert_eq!(s.level_of("admin", ip("10.0.0.7")), Level::OWNER);
        // A later bare entry must not clear the binding.
        assert!(!s.add_member(OP_GROUP, "admin", None).unwrap());
        assert_eq!(s.level_of("admin", None), Level::NONE);
        assert_eq!(s.level_of("admin", ip("10.0.0.7")), Level::OWNER);
    }

    #[test]
    fn seeding_is_add_only_and_never_deops_on_restart() {
        let mut s = Store::new();
        assert_eq!(s.seed_operators(&["root".into(), "root".into()]), 1);
        // A restart with the same list adds nothing.
        assert_eq!(s.seed_operators(&["root".into()]), 0);
        // Removing a name from the config does not remove the member: deoping
        // is an explicit `/deop`, not a side effect of editing the file.
        s.seed_operators(&[]);
        assert_eq!(s.level_of("root", None), Level::OWNER);
    }

    #[test]
    fn a_persisted_store_survives_a_restart_without_the_config_seed() {
        let path = temp_path("restart");
        let _ = std::fs::remove_file(&path);
        let mut first = Store::new();
        first.seed_operators(&["root".into(), "admin@10.0.0.7".into()]);
        first
            .declare_node("worldguard.flag.pvp", NodeDefault::Deny)
            .unwrap();
        first.create_group("moderator", Level::new(3)).unwrap();
        first.add_member("moderator", "mod", None).unwrap();
        first.save(&path).unwrap();

        // A restart: the config no longer lists `root`, but the file does.
        let mut second = Store::load(&path).unwrap().expect("the file exists");
        assert_eq!(second.level_of("root", None), Level::OWNER);
        assert_eq!(second.level_of("admin", ip("10.0.0.7")), Level::OWNER);
        assert_eq!(second.level_of("mod", None), Level::new(3));
        assert_eq!(
            second.node_default("worldguard.flag.pvp"),
            Some(NodeDefault::Deny)
        );
        // Seeding an empty config after the restart does not deop.
        second.seed_operators(&[]);
        assert_eq!(second.level_of("root", None), Level::OWNER);
        // The built-in group came back too.
        assert!(second.group(OP_GROUP).unwrap().is_builtin());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("toml.tmp"));
    }

    #[test]
    fn a_file_cannot_weaken_the_op_group() {
        // A hand-edited file that renames or lowers `op` is corrected on load.
        let text = r#"
[groups.op]
id = "op"
level = 1
builtin = false

[groups.op.members]
root = {}
"#;
        let s = Store::from_toml(text).unwrap();
        let op = s.group(OP_GROUP).unwrap();
        assert!(op.is_builtin(), "op must stay built-in");
        assert_eq!(op.level(), OP_LEVEL, "op must stay level 4");
        assert_eq!(s.level_of("root", None), Level::OWNER);
        // A missing `op` group is recreated.
        let s = Store::from_toml("[nodes]\n").unwrap();
        assert!(s.group(OP_GROUP).is_some());
    }

    #[test]
    fn a_level_past_four_in_a_file_is_clamped() {
        let text = r#"
[groups.owner]
id = "owner"
level = 200
builtin = false

[groups.owner.members]
root = {}
"#;
        let s = Store::from_toml(text).unwrap();
        assert_eq!(s.level_of("root", None), Level::OWNER);
        assert_eq!(s.group("owner").unwrap().level().get(), 4);
    }

    #[test]
    fn the_file_round_trips_through_toml() {
        let mut s = Store::new();
        s.seed_operators(&["root".into()]);
        s.create_group("moderator", Level::new(3)).unwrap();
        s.add_member("moderator", "mod", Some("10.0.0.9")).unwrap();
        let text = s.to_toml().unwrap();
        let back = Store::from_toml(&text).unwrap();
        assert_eq!(back.level_of("root", None), Level::OWNER);
        assert_eq!(back.level_of("mod", ip("10.0.0.9")), Level::new(3));
        assert_eq!(back.level_of("mod", None), Level::NONE);
    }

    #[test]
    fn the_op_group_cannot_be_deleted_but_an_ordinary_one_can() {
        let mut s = Store::new();
        assert!(s.delete_group(OP_GROUP).is_err());
        assert!(s.group(OP_GROUP).is_some());
        s.create_group("moderator", Level::new(3)).unwrap();
        assert_eq!(s.group("moderator").unwrap().level().get(), 3);
        assert!(s.delete_group("moderator").is_ok());
        assert!(s.group("moderator").is_none());
        assert!(s.delete_group("missing").is_err());
        assert!(s.add_member("missing", "x", None).is_err());
    }

    #[test]
    fn a_member_name_is_never_duplicated_across_case() {
        let mut s = Store::new();
        assert!(s.add_member(OP_GROUP, "Steve", None).unwrap());
        assert!(!s.add_member(OP_GROUP, "STEVE", None).unwrap());
        assert_eq!(s.group(OP_GROUP).unwrap().members().count(), 1);
    }

    #[test]
    fn the_process_wide_store_seeds_operators_from_config() {
        // `install` replaces the whole store, so take the shared lock: another
        // test that seeded an operator into the process-wide store must not
        // have it wiped from under it.
        let _guard = test_lock();
        // Unique names so this cannot collide with another test sharing the
        // process-wide store, and a temp world dir so `install`'s persistence
        // does not touch the source tree.
        let dir = std::env::temp_dir().join(format!("aether-perm-install-{}", std::process::id()));
        let mut cfg = Config::default();
        cfg.operators = vec!["perm_core_test_op".into()];
        cfg.server.world_dir = dir.to_string_lossy().into_owned();
        install(&cfg);
        assert!(is_operator("perm_core_test_op", None));
        assert!(!is_operator("perm_core_test_other", None));
        assert_eq!(level("perm_core_test_op", None), Level::OWNER);
        assert!(groups_of("perm_core_test_op", None).contains(&OP_GROUP.to_string()));
        // The file was written.
        assert!(dir.join(STORE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
