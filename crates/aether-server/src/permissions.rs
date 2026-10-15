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
//! Groups, inheritance and per-world context are CMD-02; CMD-01 is the level /
//! node model, the store and the [`check`] API. The server is the only consumer
//! today, so the module lives here rather than in `aether-api`.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::config::Config;

/// The highest vanilla permission level: an owner.
const MAX_LEVEL: u8 = 4;

/// A vanilla permission level, clamped to `0..=4`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
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
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Default {
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

/// A group of players at one permission level.
///
/// CMD-01 has only the built-in [`OP_GROUP`]; inheritance and meta are CMD-02.
pub struct Group {
    id: String,
    level: Level,
    builtin: bool,
    /// Member name (lower-cased) → an optional required address. `None` means
    /// any address, mirroring `Config::is_operator`'s bare-name entry.
    members: BTreeMap<String, Option<String>>,
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
            .map(|(name, addr)| (name.as_str(), addr.as_deref()))
    }

    /// Whether `name`, connecting from `ip`, is a member.
    pub fn contains(&self, name: &str, ip: Option<IpAddr>) -> bool {
        match self.members.get(&name.to_ascii_lowercase()) {
            // A bare name: any address.
            Some(None) => true,
            // A bound name: only that address. An address that does not parse
            // is no address, exactly as `Config::is_operator` treats it.
            Some(Some(addr)) => ip.is_some_and(|ip| addr.trim().parse::<IpAddr>() == Ok(ip)),
            None => false,
        }
    }
}

/// The permission store: node defaults and groups.
///
/// The server holds one process-wide instance behind a lock (see [`store`]);
/// [`Store`] is public so the model can be unit-tested and reused by CMD-02.
#[derive(Default)]
pub struct Store {
    nodes: BTreeMap<String, Default>,
    groups: BTreeMap<String, Group>,
}

impl Store {
    /// A store with the built-in `op` group and no members.
    pub fn new() -> Store {
        let mut store = Store::default();
        store.groups.insert(
            OP_GROUP.to_string(),
            Group {
                id: OP_GROUP.to_string(),
                level: OP_LEVEL,
                builtin: true,
                members: BTreeMap::new(),
            },
        );
        store
    }

    /// Declare (or redeclare) a node's default.
    pub fn declare_node(&mut self, node: &str, default: Default) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        self.nodes.insert(node.to_string(), default);
        Ok(())
    }

    /// A node's declared default, if any.
    pub fn node_default(&self, node: &str) -> Option<Default> {
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
    /// member was new: an existing entry is left untouched (no duplication),
    /// which is the add-only reconciliation the seed relies on.
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
        if group.members.contains_key(&key) {
            return Ok(false);
        }
        group.members.insert(key, addr.map(str::to_string));
        Ok(true)
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
    /// A malformed node is denied outright. [`Default::Allow`] removes the
    /// node's own operator requirement, but the command's `op` level still
    /// applies — that is what lets one node default serve a public op-0 command
    /// and a gated op-3 command. [`Default::Deny`] permits no one.
    /// [`Default::Op`] and an undeclared node are alike: operators only, never
    /// `true`, so a typo fails closed. This is the one place the three sources
    /// (node default, op level, unknown) are combined; every caller goes
    /// through it.
    pub fn check(&self, level: Level, node: &str, op: u8) -> bool {
        if !valid_node(node) {
            return false;
        }
        let op = Level::new(op).get();
        match self.nodes.get(node) {
            Some(Default::Allow) => level.get() >= op,
            Some(Default::Deny) => false,
            Some(Default::Op) | None => level.get() >= op.max(1),
        }
    }
}

/// The process-wide store, created empty on first use.
fn store() -> &'static RwLock<Store> {
    static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(Store::new()))
}

fn read() -> RwLockReadGuard<'static, Store> {
    store().read().unwrap_or_else(|e| e.into_inner())
}

fn write() -> RwLockWriteGuard<'static, Store> {
    store().write().unwrap_or_else(|e| e.into_inner())
}

/// Install the permission core from the config: seed the `op` group from
/// `config.operators` and declare every core command's node.
///
/// Called once at startup, before the listener accepts a connection. Add-only,
/// so calling it again reconciles without deopping anyone.
pub fn install(cfg: &Config) {
    let mut store = write();
    let added = store.seed_operators(&cfg.operators);
    for entry in crate::commands::registry::CORE {
        let _ = declare_command_node_in(&mut store, entry.node, entry.op);
    }
    if added > 0 {
        crate::log::info(&format!(
            "permissions: seeded {added} operator(s) into the `{OP_GROUP}` group"
        ));
    }
}

/// Register a command's node with the default its op level implies: a public
/// (op 0) command allows everyone, a gated one is operator-only (its op level
/// is still enforced by [`check`]).
///
/// Idempotent and non-destructive: an existing declaration is kept, so a
/// command registration never clobbers a node an operator set explicitly.
pub fn declare_command_node(node: &str, op: u8) -> Result<(), String> {
    declare_command_node_in(&mut write(), node, op)
}

fn declare_command_node_in(store: &mut Store, node: &str, op: u8) -> Result<(), String> {
    if store.node_default(node).is_some() {
        return Ok(());
    }
    let default = if op == 0 { Default::Allow } else { Default::Op };
    store.declare_node(node, default)
}

/// A player's effective level in the process-wide store.
pub fn level(name: &str, ip: Option<IpAddr>) -> Level {
    read().level_of(name, ip)
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
pub fn declare_node(node: &str, default: Default) -> Result<(), String> {
    write().declare_node(node, default)
}

/// A node's declared default in the process-wide store.
pub fn node_default(node: &str) -> Option<Default> {
    read().node_default(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
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
        s.declare_node("test.allow", Default::Allow).unwrap();
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
        s.declare_node("test.allow", Default::Allow).unwrap();
        s.declare_node("test.deny", Default::Deny).unwrap();
        assert!(s.check(Level::NONE, "test.allow", 0));
        // A deny is absolute: level 4 does not override it.
        assert!(!s.check(Level::OWNER, "test.deny", 0));
        assert!(!s.check(Level::OWNER, "test.deny", 4));
    }

    #[test]
    fn an_op_node_is_operator_only() {
        let mut s = Store::new();
        s.declare_node("test.op", Default::Op).unwrap();
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
        // Unique names so this cannot collide with another test sharing the
        // process-wide store.
        let cfg = Config {
            operators: vec!["perm_core_test_op".into()],
            ..Config::default()
        };
        install(&cfg);
        assert!(is_operator("perm_core_test_op", None));
        assert!(!is_operator("perm_core_test_other", None));
        assert_eq!(level("perm_core_test_op", None), Level::OWNER);
    }
}
