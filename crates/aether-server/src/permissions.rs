//! The permission core: op levels, permission nodes, groups and inheritance,
//! per-world context and the one check every command and hot-path gate goes
//! through.
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
//! A check resolves the **explicit** nodes first, then the default:
//!
//! 1. the subject's own nodes (per-player grants/denies), then
//! 2. each group the subject belongs to, directly or by inheritance, in a total
//!    order: highest group [`priority`](Group::priority) first, then the
//!    shallowest inheritance depth (child before parent), then the group id —
//!    see [`Store::holders`]. The first holder with a node for the key decides.
//! 3. the node's declared default:
//!    * `Allow` — the node adds no operator requirement; the command's own op
//!      level is the only remaining gate;
//!    * `Deny`  — no one, operator or not;
//!    * `Op`    — an operator (level ≥ 1), and still at least the op level;
//! 4. nothing declared — the node is unknown, and defaults to `op`: only an
//!    operator, never `true`. A typo therefore fails closed.
//!
//! The command's own **op level** is a floor that always applies — a command
//! entry naming `op = 3` is not runnable below level 3 whatever its node says,
//! explicit grants included — and an unknown or `op` node additionally requires
//! an operator. Command nodes are registered with that rule (op 0 → `Allow`,
//! op > 0 → `Op`), so a command's effective requirement is exactly its op
//! level, as it was before this module existed.
//!
//! ## Context
//!
//! A node may carry a **context** — today a dimension/world key. A context of
//! `None` is "everywhere" (a global node); `Some("overworld")` only applies in
//! that world. Within one holder a context-specific node beats the global one,
//! and a node whose context does not match the query is skipped. Callers that
//! have no world pass `None`; [`check_in`] carries the context when they do.
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
//! ## Snapshot reads, off-tick writes
//!
//! The store is an immutable snapshot behind an `Arc`. A check clones the
//! current `Arc` ([`snapshot`]) and resolves against it; a write builds a fresh
//! copy of the store off the tick, then swaps the pointer ([`update`]). A
//! concurrent reader therefore always sees a whole, consistent store — never a
//! half-applied edit — and there is no `Mutex` on the hot path. The store is
//! the only thing the check touches; the lock is a brief `RwLock` around the
//! `Arc` clone.
//!
//! ## On disk
//!
//! The store is **persisted** to `<world_dir>/permissions.toml`, written
//! atomically (temp file + rename), so the add-only rule holds **across a
//! restart**: the file is loaded first, the config is then merged in, and a
//! name the file already holds survives even when the config no longer lists
//! it. The format is described in `docs/design/permissions.md`.
//!
//! Groups, inheritance and per-world context are CMD-02; CMD-01 is the level /
//! node model and the store. The server is the only consumer today, so the
//! module lives here rather than in `aether-api`.
//!
//! The CMD-02 brief proposed making the store a **native plugin** called from
//! the core check. It is kept in the core instead: the check is on the hot path
//! for every command, the block gates, `is_operator` and the SDK service, and
//! the native loader has no synchronous per-node check hook a plugin could
//! answer yet (the same limit that leaves `aether.staff.vanish.see`
//! unresolved). The store, its write API and its file are shaped so the move
//! behind a service is mechanical once that hook exists; see Known Issues 26.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

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

/// The value a node resolves to when no explicit grant/deny applies.
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

/// Where an explicit node came from, for the `/lp ... check` and `list` UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeSource {
    /// The subject's own node.
    Player(String),
    /// A group in the subject's inheritance chain.
    Group(String),
}

impl NodeSource {
    /// The LuckPerms-style source label.
    pub fn label(&self) -> String {
        match self {
            NodeSource::Player(name) => format!("user {name}"),
            NodeSource::Group(id) => format!("group {id}"),
        }
    }
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
///
/// A **wildcard** is a legal stored node too: `*` on its own, or a trailing
/// `.*` (`aether.command.*`). It may only be the last segment, so
/// `a.*.b` and `foo*` are rejected rather than silently matching nothing.
pub fn valid_node(node: &str) -> bool {
    if node.is_empty() {
        return false;
    }
    let mut segments = node.split('.').peekable();
    while let Some(seg) = segments.next() {
        if seg == "*" {
            // A wildcard is only meaningful as the final segment.
            return segments.peek().is_none();
        }
        if !valid_segment(seg) {
            return false;
        }
    }
    true
}

fn valid_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Whether `id` is a legal group id: non-empty ASCII letters, digits, `_` or
/// `-`. Group ids are not permission nodes, so they are case-sensitive and may
/// not contain `.` (a dot would read as a node segment in a UI).
pub fn valid_group_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether the stored node pattern `pattern` grants/denies `node`.
///
/// `*` matches every legal node; `a.*` matches `a.b` and every deeper
/// descendant, but not `a` itself or `ab`.
fn wildcard_matches(pattern: &str, node: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => {
            node.len() > prefix.len()
                && node.starts_with(prefix)
                && node.as_bytes()[prefix.len()] == b'.'
        }
        None => false,
    }
}

/// How specific a wildcard pattern is: a longer prefix is more specific, and
/// `*` is the least specific of all.
fn wildcard_len(pattern: &str) -> usize {
    if pattern == "*" {
        0
    } else {
        pattern.len()
    }
}

/// The map key for a node plus its context. A global node is the bare name; a
/// context-specific one is `node#context`. A node can never contain `#`
/// ([`valid_node`]), so the split is unambiguous, and an empty context is the
/// global one.
fn node_key(node: &str, context: Option<&str>) -> String {
    match context {
        Some(c) if !c.is_empty() => format!("{node}#{c}"),
        _ => node.to_string(),
    }
}

/// Split an encoded key back into `(node, context)`.
fn split_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('#') {
        Some((node, ctx)) => (node, Some(ctx)),
        None => (key, None),
    }
}

/// Look `node` up in one holder's map.
///
/// A matching context-specific node beats the global one, and an exact node
/// beats a wildcard. Among wildcards the most specific pattern wins; at equal
/// specificity a context-specific entry beats a global one. The search is
/// confined to one holder, so a child group's grant still beats its parent's
/// deny under the holder ordering.
fn lookup(nodes: &BTreeMap<String, bool>, node: &str, context: Option<&str>) -> Option<bool> {
    let ctx = context.filter(|c| !c.is_empty());
    if let Some(ctx) = ctx {
        if let Some(v) = nodes.get(&node_key(node, Some(ctx))) {
            return Some(*v);
        }
    }
    if let Some(v) = nodes.get(&node_key(node, None)) {
        return Some(*v);
    }

    // No exact match: the longest matching wildcard decides. Context-specific
    // is preferred to global at the same length.
    let mut best: Option<(bool, usize, bool)> = None;
    for (key, value) in nodes {
        let (pattern, key_ctx) = split_key(key);
        if !wildcard_matches(pattern, node) {
            continue;
        }
        let contextual = key_ctx.is_some() && key_ctx == ctx;
        if key_ctx.is_some() && !contextual {
            continue;
        }
        let rank = (contextual, wildcard_len(pattern), *value);
        let better = match best {
            None => true,
            Some((best_ctx, best_len, _)) => match (rank.0, best_ctx) {
                (true, false) => true,
                (false, true) => false,
                _ => rank.1 > best_len,
            },
        };
        if better {
            best = Some(rank);
        }
    }
    best.map(|(_, _, value)| value)
}

/// One group member on the wire: an optional required address, omitted for a
/// bare name. TOML has no null, so `None` is an absent `addr` key.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Member {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    addr: Option<String>,
}

/// A holder's display/chat meta. Every field is optional; a missing value is
/// inherited from the next holder in priority order ([`Store::meta_of`]).
///
/// `weight` is the LuckPerms-style sort weight, kept as `Option` so an unset
/// value does not shadow a group's: a player with no explicit weight inherits
/// the highest-precedence group's.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// Chat prefix, raw (the chat layer renders colour codes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Chat suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    /// Sort weight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i32>,
}

impl Meta {
    /// Whether every field is unset.
    pub fn is_empty(&self) -> bool {
        self.prefix.is_none() && self.suffix.is_none() && self.weight.is_none()
    }

    /// A meta value by the SDK's key spelling (`prefix`/`suffix`/`weight`).
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "prefix" => self.prefix.clone(),
            "suffix" => self.suffix.clone(),
            "weight" => self.weight.map(|w| w.to_string()),
            _ => None,
        }
    }

    /// Set a meta value by key, rejecting an unknown key or a non-integer
    /// weight. An empty value clears the key. Prefix/suffix keep their exact
    /// spelling (a trailing space is part of a prefix).
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "prefix" => self.prefix = keep_if_nonempty(value),
            "suffix" => self.suffix = keep_if_nonempty(value),
            "weight" => {
                let value = value.trim();
                self.weight = match value {
                    "" => None,
                    v => Some(
                        v.parse()
                            .map_err(|_| format!("weight `{v}` is not an integer"))?,
                    ),
                }
            }
            other => return Err(format!("unknown meta key `{other}`")),
        }
        Ok(())
    }
}

fn keep_if_nonempty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// A group of players at one permission level, with an inheritance parent, a
/// priority, explicit nodes and meta.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    id: String,
    level: Level,
    builtin: bool,
    /// Sort priority: a higher value resolves first. Ties break by id.
    #[serde(default)]
    pub priority: i32,
    /// Parent groups, inherited child-then-parent. Sorted for a stable file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Explicit nodes, encoded by [`node_key`] (`node` or `node#context`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    nodes: BTreeMap<String, bool>,
    /// Prefix/suffix/weight meta.
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    meta: Meta,
    /// Member name (lower-cased) → its binding. A bare entry has no address.
    #[serde(default)]
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

    /// The group's parents.
    pub fn parents(&self) -> &[String] {
        &self.parents
    }

    /// The group's explicit nodes, as `(node, context, value)`.
    pub fn nodes(&self) -> impl Iterator<Item = (&str, Option<&str>, bool)> {
        self.nodes
            .iter()
            .map(|(k, v)| (split_key(k).0, split_key(k).1, *v))
    }

    /// The group's meta.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// A node on this group, context-aware.
    pub fn node(&self, node: &str, context: Option<&str>) -> Option<bool> {
        lookup(&self.nodes, node, context)
    }

    /// Set (or clear, with `None`) a node on this group.
    pub fn set_node(&mut self, node: &str, context: Option<&str>, value: Option<bool>) {
        let key = node_key(node, context);
        match value {
            Some(v) => {
                self.nodes.insert(key, v);
            }
            None => {
                self.nodes.remove(&key);
            }
        }
    }

    /// Set this group's meta by key.
    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.meta.set(key, value)
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

/// A subject's explicit per-player state: nodes and meta.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Player {
    /// Explicit nodes, encoded by [`node_key`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    nodes: BTreeMap<String, bool>,
    /// The player's own meta, which outranks every group's.
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    meta: Meta,
}

impl Player {
    /// A node on this player, context-aware.
    pub fn node(&self, node: &str, context: Option<&str>) -> Option<bool> {
        lookup(&self.nodes, node, context)
    }

    /// The player's meta.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The player's explicit nodes, as `(node, context, value)`.
    pub fn nodes(&self) -> impl Iterator<Item = (&str, Option<&str>, bool)> {
        self.nodes
            .iter()
            .map(|(k, v)| (split_key(k).0, split_key(k).1, *v))
    }

    /// The number of explicit nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the player has no explicit nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// A holder of nodes and meta: a player or a group. The resolution walks both
/// through this one lens.
trait Holder {
    fn holder_nodes(&self) -> &BTreeMap<String, bool>;
    fn holder_meta(&self) -> &Meta;
}

impl Holder for Player {
    fn holder_nodes(&self) -> &BTreeMap<String, bool> {
        &self.nodes
    }
    fn holder_meta(&self) -> &Meta {
        &self.meta
    }
}

impl Holder for Group {
    fn holder_nodes(&self) -> &BTreeMap<String, bool> {
        &self.nodes
    }
    fn holder_meta(&self) -> &Meta {
        &self.meta
    }
}

/// The permission store: node defaults, groups and per-player nodes.
///
/// Immutable in practice: a write clones the whole store, edits the copy, and
/// swaps it in ([`update`]). The public methods take `&mut self` so the model
/// can be built and unit-tested directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    nodes: BTreeMap<String, NodeDefault>,
    groups: BTreeMap<String, Group>,
    players: BTreeMap<String, Player>,
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
            players: BTreeMap::new(),
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
            group.parents.sort();
            group.parents.dedup();
        }
        let op = self
            .groups
            .entry(OP_GROUP.to_string())
            .or_insert_with(|| Group {
                id: OP_GROUP.to_string(),
                level: OP_LEVEL,
                builtin: true,
                priority: 0,
                parents: Vec::new(),
                nodes: BTreeMap::new(),
                meta: Meta::default(),
                members: BTreeMap::new(),
            });
        op.id = OP_GROUP.to_string();
        op.level = OP_LEVEL;
        op.builtin = true;
    }

    /// Reject an inheritance cycle (or a parent that does not exist) before the
    /// store is used. Called on load, and by [`Store::set_parent`], so a cycle
    /// is a load error rather than an infinite recursion.
    pub fn validate(&self) -> Result<(), String> {
        // 0 unvisited, 1 on the stack, 2 done.
        let mut state: BTreeMap<&str, u8> = BTreeMap::new();
        for id in self.groups.keys() {
            let mut stack = Vec::new();
            self.visit(id, &mut state, &mut stack)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        id: &'a str,
        state: &mut BTreeMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Result<(), String> {
        match state.get(id).copied().unwrap_or(0) {
            2 => return Ok(()),
            1 => {
                let start = stack.iter().position(|s| *s == id).unwrap_or(0);
                let mut path: Vec<&str> = stack[start..].to_vec();
                path.push(id);
                return Err(format!("inheritance cycle: {}", path.join(" -> ")));
            }
            _ => {}
        }
        state.insert(id, 1);
        stack.push(id);
        if let Some(group) = self.groups.get(id) {
            for parent in &group.parents {
                if self.groups.contains_key(parent) {
                    self.visit(parent, state, stack)?;
                }
            }
        }
        stack.pop();
        state.insert(id, 2);
        Ok(())
    }

    /// Whether `from` reaches `target` through parent links. Used to reject a
    /// parent edge that would close a cycle.
    fn reaches(&self, from: &str, target: &str) -> bool {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = vec![from];
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if !seen.insert(id) {
                continue;
            }
            if let Some(group) = self.groups.get(id) {
                stack.extend(group.parents.iter().map(|p| p.as_str()));
            }
        }
        false
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

    /// The named player's explicit state, if any.
    pub fn player(&self, name: &str) -> Option<&Player> {
        self.players.get(&name.to_ascii_lowercase())
    }

    /// The ids of every group `name`, from `ip`, belongs to. Direct membership
    /// only; inheritance is folded in by [`Store::holders`].
    pub fn groups_of(&self, name: &str, ip: Option<IpAddr>) -> Vec<String> {
        self.groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.id.clone())
            .collect()
    }

    /// Every group id, sorted, for completion and `/lp group info`.
    pub fn group_ids(&self) -> Vec<String> {
        self.groups.keys().cloned().collect()
    }

    /// Every player name the store knows, sorted.
    pub fn player_names(&self) -> Vec<String> {
        self.players.keys().cloned().collect()
    }

    /// Every node name the store knows: declared defaults plus explicit
    /// player/group patterns. Contexts are stripped, sorted and de-duplicated,
    /// so this is exactly the candidate list for tab-completion.
    pub fn known_nodes(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = self.nodes.keys().cloned().collect();
        for player in self.players.values() {
            out.extend(player.nodes.keys().map(|k| split_key(k).0.to_string()));
        }
        for group in self.groups.values() {
            out.extend(group.nodes.keys().map(|k| split_key(k).0.to_string()));
        }
        out.into_iter().collect()
    }

    /// Create a non-built-in group. The built-in `op` group already exists.
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
                priority: 0,
                parents: Vec::new(),
                nodes: BTreeMap::new(),
                meta: Meta::default(),
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
                // No dangling parent edges: a deleted group is no longer an
                // ancestor.
                for group in self.groups.values_mut() {
                    group.parents.retain(|p| p != id);
                }
                Ok(())
            }
        }
    }

    /// Set a group's priority.
    pub fn set_priority(&mut self, id: &str, priority: i32) -> Result<(), String> {
        let group = self
            .groups
            .get_mut(id)
            .ok_or_else(|| format!("no group `{id}`"))?;
        group.priority = priority;
        Ok(())
    }

    /// Add `parent` as a parent of `child`, refusing a missing group or an edge
    /// that would create a cycle.
    pub fn set_parent(&mut self, child: &str, parent: &str) -> Result<(), String> {
        if !self.groups.contains_key(child) {
            return Err(format!("no group `{child}`"));
        }
        if !self.groups.contains_key(parent) {
            return Err(format!("no group `{parent}`"));
        }
        if child == parent || self.reaches(parent, child) {
            return Err(format!(
                "adding parent `{parent}` to `{child}` would create an inheritance cycle"
            ));
        }
        let group = self.groups.get_mut(child).expect("checked above");
        if !group.parents.iter().any(|p| p == parent) {
            group.parents.push(parent.to_string());
            group.parents.sort();
        }
        Ok(())
    }

    /// Remove `parent` from `child`'s parents.
    pub fn remove_parent(&mut self, child: &str, parent: &str) -> Result<(), String> {
        let group = self
            .groups
            .get_mut(child)
            .ok_or_else(|| format!("no group `{child}`"))?;
        group.parents.retain(|p| p != parent);
        Ok(())
    }

    /// Replace `child`'s whole parent set atomically: validates every parent,
    /// de-duplicates and **rejects a cycle** before the store changes, so a
    /// failed `parent set`/`switch`/`clear` leaves the previous edges intact.
    pub fn set_group_parents(&mut self, child: &str, parents: &[String]) -> Result<(), String> {
        if !self.groups.contains_key(child) {
            return Err(format!("no group `{child}`"));
        }
        let mut next = self.clone();
        let mut unique: Vec<String> = Vec::new();
        for parent in parents {
            if !next.groups.contains_key(parent) {
                return Err(format!("no group `{parent}`"));
            }
            if !unique.iter().any(|p| p == parent) {
                unique.push(parent.clone());
            }
        }
        unique.sort();
        next.groups.get_mut(child).expect("checked above").parents = unique;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Set a user's direct groups, replacing the previous set. Validates every
    /// group before touching membership, so a bad name changes nothing.
    pub fn set_user_groups(&mut self, name: &str, groups: &[String]) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a player name is empty".to_string());
        }
        for id in groups {
            if !self.groups.contains_key(id) {
                return Err(format!("no group `{id}`"));
            }
        }
        let key = name.to_ascii_lowercase();
        for group in self.groups.values_mut() {
            group.members.remove(&key);
        }
        let mut unique: Vec<&String> = Vec::new();
        for id in groups {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        for id in unique {
            self.groups
                .get_mut(id)
                .expect("validated above")
                .members
                .insert(key.clone(), Member { addr: None });
        }
        Ok(())
    }

    /// Add `name` to a group (a user `parent add`), keeping any other groups.
    pub fn add_user_group(&mut self, name: &str, group: &str) -> Result<bool, String> {
        self.add_member(group, name, None)
    }

    /// Remove `name` from `group` (a user `parent remove`), keeping the rest.
    pub fn remove_user_group(&mut self, name: &str, group: &str) -> Result<bool, String> {
        self.remove_member(group, name)
    }

    /// Set or clear an explicit node on a group.
    pub fn set_group_node(
        &mut self,
        group: &str,
        node: &str,
        context: Option<&str>,
        value: Option<bool>,
    ) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let group = self
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        group.set_node(node, context, value);
        Ok(())
    }

    /// Set a group's meta by key.
    pub fn set_group_meta(&mut self, group: &str, key: &str, value: &str) -> Result<(), String> {
        let group = self
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        group.set_meta(key, value)
    }

    /// Set or clear an explicit node on a player.
    pub fn set_player_node(
        &mut self,
        name: &str,
        node: &str,
        context: Option<&str>,
        value: Option<bool>,
    ) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err("a player name is empty".to_string());
        }
        let player = self.players.entry(name.to_ascii_lowercase()).or_default();
        match value {
            Some(v) => {
                player.nodes.insert(node_key(node, context), v);
            }
            None => {
                player.nodes.remove(&node_key(node, context));
            }
        }
        if player.is_empty() && player.meta.is_empty() {
            self.players.remove(&name.to_ascii_lowercase());
        }
        Ok(())
    }

    /// Set a player's meta by key.
    pub fn set_player_meta(&mut self, name: &str, key: &str, value: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a player name is empty".to_string());
        }
        self.players
            .entry(name.to_ascii_lowercase())
            .or_default()
            .meta
            .set(key, value)
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

    /// Remove `name` from `group`. Returns whether it was a member.
    ///
    /// This is the `/deop` write (CMD-08): removal is whole, so a bound entry
    /// (`name@address`) is removed regardless of the address.
    pub fn remove_member(&mut self, group: &str, name: &str) -> Result<bool, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a member name is empty".to_string());
        }
        let group = self
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        Ok(group.members.remove(&name.to_ascii_lowercase()).is_some())
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
    /// belong to directly, or none.
    pub fn level_of(&self, name: &str, ip: Option<IpAddr>) -> Level {
        self.groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.level)
            .max()
            .unwrap_or(Level::NONE)
    }

    /// The holders a resolution walks, highest precedence first: the player
    /// first, then every reachable group ordered by `(priority desc, depth asc,
    /// id asc)`.
    ///
    /// `depth` is `0` for a group the player is in directly, `1` for its
    /// parent, and so on: a child's node therefore beats a parent's at the same
    /// priority, while a higher-priority group anywhere in the tree beats a
    /// lower one. The order is total (`depth` then `id` break every tie), so a
    /// resolution is reproducible.
    fn holders<'a>(&'a self, name: &str, ip: Option<IpAddr>) -> Vec<&'a dyn Holder> {
        let mut out: Vec<&dyn Holder> = Vec::new();
        if let Some(player) = self.player(name) {
            out.push(player);
        }
        let roots: Vec<String> = self
            .groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.id.clone())
            .collect();
        out.extend(
            self.group_order(roots)
                .into_iter()
                .map(|g| g as &dyn Holder),
        );
        out
    }

    /// The reachable groups from `roots`, highest precedence first:
    /// `(priority desc, depth asc, id asc)`.
    ///
    /// `depth` is `0` for a root, `1` for its parent, and so on. Breadth-first
    /// keeps the shallowest depth at which a group is reached, so a child
    /// beats a parent at equal priority; priority outranks depth across the
    /// whole tree. The order is total, so a resolution is reproducible.
    fn group_order(&self, roots: impl IntoIterator<Item = String>) -> Vec<&Group> {
        let mut depth: BTreeMap<String, usize> = BTreeMap::new();
        let mut level: Vec<String> = roots.into_iter().collect();
        let mut d = 0usize;
        while !level.is_empty() {
            let mut next: Vec<String> = Vec::new();
            for id in level {
                if let Some(&seen) = depth.get(&id) {
                    if seen <= d {
                        continue;
                    }
                }
                depth.insert(id.clone(), d);
                if let Some(group) = self.groups.get(&id) {
                    next.extend(group.parents.iter().cloned());
                }
            }
            level = next;
            d += 1;
            // `validate` rejects cycles, but never loop if a hand-built store
            // slipped one through.
            if d > self.groups.len() + 1 {
                break;
            }
        }

        let mut order: Vec<(i32, usize, &str)> = depth
            .iter()
            .filter_map(|(id, &dep)| self.groups.get(id).map(|g| (g.priority, dep, id.as_str())))
            .collect();
        order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
        order
            .into_iter()
            .filter_map(|(_, _, id)| self.groups.get(id))
            .collect()
    }

    /// The explicit node a `(name, ip)` subject holds for `node` in `context`,
    /// or `None` when no holder (player or group) has one.
    pub fn explicit(
        &self,
        name: &str,
        ip: Option<IpAddr>,
        node: &str,
        context: Option<&str>,
    ) -> Option<bool> {
        for holder in self.holders(name, ip) {
            if let Some(value) = lookup(holder.holder_nodes(), node, context) {
                return Some(value);
            }
        }
        None
    }

    /// Like [`Store::explicit`], but names the holder that decided.
    pub fn explicit_source(
        &self,
        name: &str,
        ip: Option<IpAddr>,
        node: &str,
        context: Option<&str>,
    ) -> Option<(bool, NodeSource)> {
        if let Some(player) = self.player(name) {
            if let Some(value) = lookup(&player.nodes, node, context) {
                return Some((value, NodeSource::Player(name.to_string())));
            }
        }
        let roots: Vec<String> = self
            .groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.id.clone())
            .collect();
        for group in self.group_order(roots) {
            if let Some(value) = lookup(&group.nodes, node, context) {
                return Some((value, NodeSource::Group(group.id.clone())));
            }
        }
        None
    }

    /// The explicit node a **group** holds for `node` in `context`, walking the
    /// group itself and then its ancestors.
    pub fn group_explicit(
        &self,
        group: &str,
        node: &str,
        context: Option<&str>,
    ) -> Option<(bool, NodeSource)> {
        let group = self.groups.get(group)?;
        for holder in self.group_order(vec![group.id.clone()]) {
            if let Some(value) = lookup(&holder.nodes, node, context) {
                return Some((value, NodeSource::Group(holder.id.clone())));
            }
        }
        None
    }

    /// The groups `name`, from `ip`, belongs to **by inheritance**, including
    /// the direct ones, in resolution order.
    pub fn inherited_groups(&self, name: &str, ip: Option<IpAddr>) -> Vec<String> {
        let roots: Vec<String> = self
            .groups
            .values()
            .filter(|g| g.contains(name, ip))
            .map(|g| g.id.clone())
            .collect();
        self.group_order(roots)
            .into_iter()
            .map(|g| g.id.clone())
            .collect()
    }

    /// The parents of `id`, or every reachable ancestor excluding `id` itself.
    pub fn group_parents(&self, id: &str) -> Vec<String> {
        self.groups
            .get(id)
            .map(|g| g.parents.clone())
            .unwrap_or_default()
    }

    /// The decision: may a player at `level` use `node`, in `context`, for a
    /// command needing `op`?
    ///
    /// A malformed node is denied outright. An explicit node (player or group,
    /// in [`Store::holders`] order) decides next, but only at or above the
    /// command's `op` floor — an explicit grant cannot lower a command's op
    /// level. With no explicit node the declared default resolves as in
    /// [`Store::check`].
    pub fn resolve(
        &self,
        level: Level,
        name: &str,
        ip: Option<IpAddr>,
        node: &str,
        context: Option<&str>,
        op: u8,
    ) -> bool {
        if !valid_node(node) {
            return false;
        }
        // The command's op level is a floor whatever the node says.
        if level.get() < Level::new(op).get() {
            return false;
        }
        if let Some(value) = self.explicit(name, ip, node, context) {
            return value;
        }
        self.check(level, node, op)
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

    /// A meta value for `(name, ip)`, resolved the way nodes are: the player
    /// first, then groups in precedence order.
    pub fn meta_of(&self, name: &str, ip: Option<IpAddr>, key: &str) -> Option<String> {
        for holder in self.holders(name, ip) {
            if let Some(value) = holder.holder_meta().get(key) {
                return Some(value);
            }
        }
        None
    }

    /// The effective prefix for `(name, ip)`.
    pub fn prefix_of(&self, name: &str, ip: Option<IpAddr>) -> Option<String> {
        self.meta_of(name, ip, "prefix")
    }

    /// The effective suffix for `(name, ip)`.
    pub fn suffix_of(&self, name: &str, ip: Option<IpAddr>) -> Option<String> {
        self.meta_of(name, ip, "suffix")
    }

    /// Encode the store as TOML.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| format!("cannot encode permissions: {e}"))
    }

    /// Decode a store, restoring the invariants a hand-edited file could break
    /// and rejecting an inheritance cycle.
    pub fn from_toml(text: &str) -> Result<Store, String> {
        let mut store: Store =
            toml::from_str(text).map_err(|e| format!("cannot parse permissions: {e}"))?;
        store.normalize();
        store.validate()?;
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

/// The process-wide snapshot, created with the default store on first use.
fn snapshot() -> &'static RwLock<Arc<Store>> {
    static SNAPSHOT: OnceLock<RwLock<Arc<Store>>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| RwLock::new(Arc::new(Store::new())))
}

/// Where the process-wide store persists, set by [`install`]. Absent when the
/// core was never installed (tests, tools), in which case [`save`] is a no-op.
fn path() -> &'static OnceLock<PathBuf> {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    &PATH
}

/// Clone the current snapshot. This is the only lock a reader takes, and it is
/// held just long enough to bump the `Arc` refcount — never across a check.
pub fn snapshot_store() -> Arc<Store> {
    snapshot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Apply `f` to a fresh copy of the store and swap it in. A writer's clone and
/// edit happen under the write lock (writes are admin-rare, never the hot
/// path); readers only ever see a complete snapshot.
fn update<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut guard = snapshot().write().unwrap_or_else(|e| e.into_inner());
    let mut next = (**guard).clone();
    let result = f(&mut next);
    *guard = Arc::new(next);
    result
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
    // The pending core commands are node/op stubs, so their nodes are declared
    // here (the implemented ones gate through the legacy operator check until
    // CMD-05/CMD-01 wire their nodes).
    for c in crate::commands::catalogue::CATALOGUE
        .iter()
        .filter(|c| c.status == crate::commands::catalogue::Status::Pending)
    {
        let _ = declare_command_node_in(&mut store, c.node, c.op);
    }
    *snapshot().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(store);
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
        Some(p) => snapshot_store().save(p),
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
    update(|store| declare_command_node_in(store, node, op))
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
    update(|store| store.seed_operators(entries))
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

/// Replace the process-wide store for a test, without touching the persisted
/// file path the way [`install`] does.
#[cfg(test)]
pub(crate) fn install_store_for_test(store: Store) {
    *snapshot().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(store);
}

/// A player's effective level in the process-wide store.
pub fn level(name: &str, ip: Option<IpAddr>) -> Level {
    snapshot_store().level_of(name, ip)
}

/// The groups `name`, from `ip`, belongs to, in the process-wide store.
pub fn groups_of(name: &str, ip: Option<IpAddr>) -> Vec<String> {
    snapshot_store().groups_of(name, ip)
}

/// Whether `name`, from `ip`, is an operator (level ≥ 1).
pub fn is_operator(name: &str, ip: Option<IpAddr>) -> bool {
    level(name, ip).get() >= 1
}

/// The process-wide [`Store::check`]: the default/op-level half of resolution,
/// with no explicit nodes. Kept for callers that only have a level.
pub fn check(level: Level, node: &str, op: u8) -> bool {
    snapshot_store().check(level, node, op)
}

/// The process-wide resolved check for a subject: explicit nodes, groups,
/// inheritance and the declared default, for a global (context-free) node.
pub fn check_for(name: &str, ip: Option<IpAddr>, node: &str, op: u8) -> bool {
    check_in(name, ip, node, None, op)
}

/// The process-wide resolved check in a context (a dimension/world key).
pub fn check_in(name: &str, ip: Option<IpAddr>, node: &str, context: Option<&str>, op: u8) -> bool {
    let store = snapshot_store();
    let level = store.level_of(name, ip);
    store.resolve(level, name, ip, node, context, op)
}

/// A meta value (prefix/suffix/weight) for `(name, ip)`.
pub fn meta(name: &str, ip: Option<IpAddr>, key: &str) -> Option<String> {
    snapshot_store().meta_of(name, ip, key)
}

/// Grant or deny `node` to a player, persist, and swap the snapshot in.
pub fn set_node(
    subject: &str,
    node: &str,
    context: Option<&str>,
    value: bool,
) -> Result<(), String> {
    update(|store| store.set_player_node(subject, node, context, Some(value)))?;
    save()
}

/// Remove an explicit `node` from a player.
pub fn unset_node(subject: &str, node: &str, context: Option<&str>) -> Result<(), String> {
    update(|store| store.set_player_node(subject, node, context, None))?;
    save()
}

/// Add a member to a group in the process-wide store and persist it.
///
/// `/op` (CMD-08) calls this with [`OP_GROUP`]. A bare add never clears an
/// existing address binding; `false` means the member was already there.
pub fn add_member(group: &str, name: &str, addr: Option<&str>) -> Result<bool, String> {
    let added = update(|store| store.add_member(group, name, addr))?;
    save()?;
    Ok(added)
}

/// Remove a member from a group in the process-wide store and persist it.
pub fn remove_member(group: &str, name: &str) -> Result<bool, String> {
    let removed = update(|store| store.remove_member(group, name))?;
    save()?;
    Ok(removed)
}

/// Declare a node's default in the process-wide store.
pub fn declare_node(node: &str, default: NodeDefault) -> Result<(), String> {
    update(|store| store.declare_node(node, default))
}

/// A node's declared default in the process-wide store.
pub fn node_default(node: &str) -> Option<NodeDefault> {
    snapshot_store().node_default(node)
}

/// The server side of the `/lp` plugin's `permissions.admin` service.
///
/// It is a thin adapter: every read clones the current immutable snapshot and
/// every write goes through [`update`] + [`save`], exactly like the core's own
/// `/op` write model. It is registered in [`crate::plugins::install`] before
/// native plugins load, so the `permissions` plugin resolves it in `init`.
#[cfg(feature = "plugins")]
pub struct AdminService;

#[cfg(feature = "plugins")]
fn node_entries<'a>(
    nodes: impl Iterator<Item = (&'a str, Option<&'a str>, bool)>,
) -> Vec<aether_plugin::permissions::NodeEntry> {
    nodes
        .map(
            |(node, context, value)| aether_plugin::permissions::NodeEntry {
                node: node.to_string(),
                context: context.map(str::to_string),
                value,
            },
        )
        .collect()
}

#[cfg(feature = "plugins")]
impl aether_plugin::permissions::PermissionsAdmin for AdminService {
    fn groups(&self) -> Vec<String> {
        snapshot_store().group_ids()
    }

    fn users(&self) -> Vec<String> {
        snapshot_store().player_names()
    }

    fn nodes(&self) -> Vec<String> {
        snapshot_store().known_nodes()
    }

    fn user(&self, name: &str) -> Option<aether_plugin::permissions::UserInfo> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let store = snapshot_store();
        Some(aether_plugin::permissions::UserInfo {
            name: name.to_ascii_lowercase(),
            level: store.level_of(name, None).get(),
            groups: store.groups_of(name, None),
            inherited_groups: store.inherited_groups(name, None),
            nodes: node_entries(store.player(name).map(|p| p.nodes()).into_iter().flatten()),
        })
    }

    fn group(&self, id: &str) -> Option<aether_plugin::permissions::GroupInfo> {
        let store = snapshot_store();
        let group = store.group(id)?;
        let inherited = store
            .group_order(vec![group.id().to_string()])
            .into_iter()
            .filter(|g| g.id() != group.id())
            .map(|g| g.id().to_string())
            .collect();
        Some(aether_plugin::permissions::GroupInfo {
            id: group.id().to_string(),
            level: group.level().get(),
            builtin: group.is_builtin(),
            priority: group.priority,
            parents: group.parents().to_vec(),
            inherited_parents: inherited,
            nodes: node_entries(group.nodes()),
            members: group.members().map(|(name, _)| name.to_string()).collect(),
        })
    }

    fn check_user(
        &self,
        name: &str,
        node: &str,
        context: Option<&str>,
    ) -> aether_plugin::permissions::Check {
        use aether_plugin::permissions::{Check, Source};
        let store = snapshot_store();
        let level = store.level_of(name, None);
        let value = store.resolve(level, name, None, node, context, 0);
        let source = match store.explicit_source(name, None, node, context) {
            Some((_, NodeSource::Player(name))) => Source::User(name),
            Some((_, NodeSource::Group(id))) => Source::Group(id),
            None => Source::Default,
        };
        Check { value, source }
    }

    fn check_group(
        &self,
        id: &str,
        node: &str,
        context: Option<&str>,
    ) -> aether_plugin::permissions::Check {
        use aether_plugin::permissions::{Check, Source};
        let store = snapshot_store();
        let Some(group) = store.group(id) else {
            return Check {
                value: false,
                source: Source::Default,
            };
        };
        let explicit = store.group_explicit(id, node, context);
        let value = explicit
            .as_ref()
            .map(|(value, _)| *value)
            .unwrap_or_else(|| store.check(group.level(), node, 0));
        let source = match explicit {
            Some((_, NodeSource::Player(name))) => Source::User(name),
            Some((_, NodeSource::Group(id))) => Source::Group(id),
            None => Source::Default,
        };
        Check { value, source }
    }

    fn set_user_node(
        &self,
        name: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String> {
        update(|store| store.set_player_node(name, node, context, Some(value)))?;
        save()
    }

    fn unset_user_node(&self, name: &str, node: &str, context: Option<&str>) -> Result<(), String> {
        update(|store| store.set_player_node(name, node, context, None))?;
        save()
    }

    fn set_group_node(
        &self,
        id: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String> {
        update(|store| store.set_group_node(id, node, context, Some(value)))?;
        save()
    }

    fn unset_group_node(&self, id: &str, node: &str, context: Option<&str>) -> Result<(), String> {
        update(|store| store.set_group_node(id, node, context, None))?;
        save()
    }

    fn set_group_parents(&self, id: &str, parents: Vec<String>) -> Result<(), String> {
        update(|store| store.set_group_parents(id, &parents))?;
        save()
    }

    fn add_group_parent(&self, id: &str, parent: &str) -> Result<(), String> {
        update(|store| store.set_parent(id, parent))?;
        save()
    }

    fn remove_group_parent(&self, id: &str, parent: &str) -> Result<(), String> {
        update(|store| store.remove_parent(id, parent))?;
        save()
    }

    fn set_user_groups(&self, name: &str, groups: Vec<String>) -> Result<(), String> {
        update(|store| store.set_user_groups(name, &groups))?;
        save()
    }

    fn add_user_group(&self, name: &str, group: &str) -> Result<(), String> {
        update(|store| store.add_user_group(name, group))?;
        save()
    }

    fn remove_user_group(&self, name: &str, group: &str) -> Result<(), String> {
        update(|store| store.remove_user_group(name, group))?;
        save()
    }

    fn create_group(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if !valid_group_id(id) {
            return Err(format!(
                "`{id}` is not a legal group name (use letters, digits, `_` or `-`)"
            ));
        }
        update(|store| store.create_group(id, Level::NONE))?;
        save()
    }

    fn delete_group(&self, id: &str) -> Result<(), String> {
        update(|store| store.delete_group(id))?;
        save()
    }

    fn save(&self) -> Result<(), String> {
        crate::permissions::save()
    }
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

    // --- CMD-02: groups, inheritance, context, meta -----------------------

    /// Build a store with a plain group, a member, and one node.
    fn fixture() -> Store {
        let mut s = Store::new();
        s.create_group("moderator", Level::new(2)).unwrap();
        s.add_member("moderator", "alice", None).unwrap();
        s.declare_node("test.node", NodeDefault::Deny).unwrap();
        s
    }

    #[test]
    fn a_group_node_grants_a_node_the_default_would_deny() {
        let mut s = fixture();
        // The default is `op`, so a level-2 member would fail `test.node`.
        assert!(!s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.node",
            None,
            0
        ));
        // An explicit grant on her group makes it pass.
        s.set_group_node("moderator", "test.node", None, Some(true))
            .unwrap();
        assert!(s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.node",
            None,
            0
        ));
        // A non-member is unaffected.
        assert!(!s.resolve(Level::NONE, "bob", None, "test.node", None, 0));
    }

    #[test]
    fn child_group_grant_is_overridden_by_a_player_level_deny() {
        let mut s = fixture();
        s.set_group_node("moderator", "test.node", None, Some(true))
            .unwrap();
        assert!(s.resolve(Level::new(2), "alice", None, "test.node", None, 0));
        // The player's own explicit deny outranks the group grant.
        s.set_player_node("alice", "test.node", None, Some(false))
            .unwrap();
        assert!(!s.resolve(Level::new(2), "alice", None, "test.node", None, 0));
        // Removing it restores the group grant.
        s.set_player_node("alice", "test.node", None, None).unwrap();
        assert!(s.resolve(Level::new(2), "alice", None, "test.node", None, 0));
    }

    #[test]
    fn priority_orders_groups_and_a_tie_breaks_by_group_id() {
        let mut s = Store::new();
        s.create_group("alpha", Level::new(1)).unwrap();
        s.create_group("beta", Level::new(1)).unwrap();
        s.add_member("alpha", "carol", None).unwrap();
        s.add_member("beta", "carol", None).unwrap();
        s.declare_node("test.node", NodeDefault::Deny).unwrap();

        // Equal priority: the lower id (`alpha`) resolves first.
        s.set_group_node("alpha", "test.node", None, Some(true))
            .unwrap();
        s.set_group_node("beta", "test.node", None, Some(false))
            .unwrap();
        assert!(
            s.resolve(Level::new(1), "carol", None, "test.node", None, 0),
            "equal priority must break by group id (alpha first)"
        );

        // Raising beta's priority makes it win despite the higher id.
        s.set_priority("beta", 10).unwrap();
        assert!(
            !s.resolve(Level::new(1), "carol", None, "test.node", None, 0),
            "the higher-priority group must win"
        );
    }

    #[test]
    fn a_child_group_node_beats_its_parent_at_equal_priority() {
        let mut s = Store::new();
        s.create_group("child", Level::new(1)).unwrap();
        s.create_group("parent", Level::new(1)).unwrap();
        s.set_parent("child", "parent").unwrap();
        s.add_member("child", "dave", None).unwrap();
        s.declare_node("test.node", NodeDefault::Deny).unwrap();

        // No node on the parent: the child's inherited member sees the default.
        s.set_group_node("parent", "test.node", None, Some(false))
            .unwrap();
        s.set_group_node("child", "test.node", None, Some(true))
            .unwrap();
        assert!(
            s.resolve(Level::new(1), "dave", None, "test.node", None, 0),
            "the child's node must beat the parent's at equal priority"
        );

        // Give the parent a higher priority and it wins.
        s.set_priority("parent", 5).unwrap();
        assert!(!s.resolve(Level::new(1), "dave", None, "test.node", None, 0));
    }

    #[test]
    fn a_per_world_node_beats_the_global_one_and_only_there() {
        let mut s = fixture();
        s.set_group_node("moderator", "test.node", None, Some(true))
            .unwrap();
        s.set_group_node("moderator", "test.node", Some("overworld"), Some(false))
            .unwrap();

        // In the overworld the context node wins...
        assert!(!s.resolve(
            Level::new(2),
            "alice",
            None,
            "test.node",
            Some("overworld"),
            0
        ));
        // ...and elsewhere the global grant applies.
        assert!(s.resolve(Level::new(2), "alice", None, "test.node", Some("nether"), 0));
        assert!(s.resolve(Level::new(2), "alice", None, "test.node", None, 0));

        // A player context deny outranks an inherited global grant.
        s.set_player_node("alice", "test.node", Some("overworld"), Some(false))
            .unwrap();
        assert!(!s.resolve(
            Level::new(2),
            "alice",
            None,
            "test.node",
            Some("overworld"),
            0
        ));
    }

    #[test]
    fn an_explicit_grant_cannot_lower_a_commands_op_floor() {
        let mut s = fixture();
        s.set_player_node("alice", "test.node", None, Some(true))
            .unwrap();
        // The command wants op 3; alice is level 2, so the grant is not enough.
        assert!(!s.resolve(Level::new(2), "alice", None, "test.node", None, 3));
        // At the floor (3) the grant decides against the `op` default.
        assert!(s.resolve(Level::new(3), "alice", None, "test.node", None, 3));
    }

    #[test]
    fn an_inheritance_cycle_is_rejected_at_load() {
        let text = r#"
[groups.a]
id = "a"
level = 1
builtin = false
parents = ["b"]

[groups.b]
id = "b"
level = 1
builtin = false
parents = ["a"]
"#;
        let err = Store::from_toml(text).unwrap_err();
        assert!(err.contains("cycle"), "{err}");
        // A self-parent is a cycle too.
        let text = r#"
[groups.a]
id = "a"
level = 1
builtin = false
parents = ["a"]
"#;
        assert!(Store::from_toml(text).is_err());
    }

    #[test]
    fn set_parent_refuses_a_cycle() {
        let mut s = Store::new();
        s.create_group("a", Level::new(1)).unwrap();
        s.create_group("b", Level::new(1)).unwrap();
        s.create_group("c", Level::new(1)).unwrap();
        s.set_parent("a", "b").unwrap();
        s.set_parent("b", "c").unwrap();
        // c -> a would close a -> b -> c -> a.
        assert!(s.set_parent("c", "a").is_err());
        // a -> c is fine (no cycle).
        assert!(s.set_parent("a", "c").is_ok());
        s.validate().unwrap();
    }

    #[test]
    fn a_missing_parent_is_skipped_not_an_error() {
        let text = r#"
[groups.a]
id = "a"
level = 1
builtin = false
parents = ["ghost"]
"#;
        let s = Store::from_toml(text).unwrap();
        assert!(s.group("a").is_some());
    }

    #[test]
    fn meta_resolves_player_then_group_by_precedence() {
        let mut s = fixture();
        s.set_group_meta("moderator", "prefix", "[mod] ").unwrap();
        s.set_group_meta("moderator", "weight", "5").unwrap();
        assert_eq!(
            s.meta_of("alice", None, "prefix").as_deref(),
            Some("[mod] ")
        );
        assert_eq!(s.meta_of("alice", None, "weight").as_deref(), Some("5"));
        assert_eq!(s.meta_of("bob", None, "prefix"), None);
        // A player prefix outranks the group's.
        s.set_player_meta("alice", "prefix", "[a] ").unwrap();
        assert_eq!(s.meta_of("alice", None, "prefix").as_deref(), Some("[a] "));
        // Weight still inherits from the group.
        assert_eq!(s.meta_of("alice", None, "weight").as_deref(), Some("5"));
        // A non-integer weight is refused.
        assert!(s.set_player_meta("alice", "weight", "heavy").is_err());
        assert!(s.set_player_meta("alice", "nonsense", "x").is_err());
    }

    #[test]
    fn the_file_round_trips_groups_inheritance_context_and_meta() {
        let mut s = Store::new();
        s.create_group("staff", Level::new(3)).unwrap();
        s.create_group("moderator", Level::new(2)).unwrap();
        s.set_parent("moderator", "staff").unwrap();
        s.set_priority("moderator", 7).unwrap();
        s.set_group_node("moderator", "aether.command.kick", None, Some(true))
            .unwrap();
        s.set_group_node(
            "staff",
            "aether.command.ban",
            Some("overworld"),
            Some(false),
        )
        .unwrap();
        s.set_group_meta("moderator", "prefix", "[mod] ").unwrap();
        s.set_player_node("alice", "aether.command.kick", None, Some(false))
            .unwrap();
        s.set_player_meta("alice", "suffix", " the brave").unwrap();

        let text = s.to_toml().unwrap();
        let back = Store::from_toml(&text).unwrap();
        assert_eq!(back.group("moderator").unwrap().priority, 7);
        assert_eq!(
            back.group("moderator").unwrap().parents(),
            &["staff".to_string()]
        );
        assert_eq!(
            back.group("moderator")
                .unwrap()
                .node("aether.command.kick", None),
            Some(true)
        );
        assert_eq!(
            back.group("staff")
                .unwrap()
                .node("aether.command.ban", Some("overworld")),
            Some(false)
        );
        assert_eq!(
            back.meta_of("alice", None, "suffix").as_deref(),
            Some(" the brave")
        );
        assert_eq!(
            back.player("alice")
                .unwrap()
                .node("aether.command.kick", None),
            Some(false)
        );
        // The inherited node still resolves for the member.
        s.add_member("moderator", "alice", None).unwrap();
        let text = s.to_toml().unwrap();
        let back = Store::from_toml(&text).unwrap();
        assert_eq!(
            back.explicit("alice", None, "aether.command.ban", Some("overworld")),
            Some(false)
        );
    }

    #[test]
    fn a_reload_preserves_a_players_explicit_overrides() {
        let path = temp_path("overrides");
        let _ = std::fs::remove_file(&path);
        let mut s = fixture();
        s.set_group_node("moderator", "test.node", None, Some(true))
            .unwrap();
        s.set_player_node("alice", "test.node", None, Some(false))
            .unwrap();
        s.save(&path).unwrap();

        let back = Store::load(&path).unwrap().expect("the file exists");
        // The group still grants, but the player's stored deny still wins.
        assert_eq!(
            back.group("moderator").unwrap().node("test.node", None),
            Some(true)
        );
        assert!(!back.resolve(
            back.level_of("alice", None),
            "alice",
            None,
            "test.node",
            None,
            0
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("toml.tmp"));
    }

    #[test]
    fn a_snapshot_swap_is_atomic() {
        let _guard = test_lock();
        let mut seeded = Store::new();
        seeded
            .set_player_node("p", "test.a", None, Some(true))
            .unwrap();
        seeded
            .set_player_node("p", "test.b", None, Some(true))
            .unwrap();
        seeded.declare_node("test.a", NodeDefault::Allow).unwrap();
        seeded.declare_node("test.b", NodeDefault::Allow).unwrap();
        *snapshot().write().unwrap() = Arc::new(seeded);

        let before = snapshot_store();
        assert!(before.player("p").unwrap().node("test.a", None).unwrap());

        // A write builds a whole new store and swaps the pointer.
        update(|store| {
            store
                .set_player_node("p", "test.a", None, Some(false))
                .unwrap();
            store
                .set_player_node("p", "test.b", None, Some(false))
                .unwrap();
        });
        let after = snapshot_store();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the snapshot must be swapped"
        );
        // The old snapshot is untouched — a reader holding it sees the old,
        // consistent state, never a half-applied edit.
        assert!(before.player("p").unwrap().node("test.a", None).unwrap());
        assert!(before.player("p").unwrap().node("test.b", None).unwrap());
        assert!(!after.player("p").unwrap().node("test.a", None).unwrap());
        assert!(!after.player("p").unwrap().node("test.b", None).unwrap());
    }

    #[test]
    fn a_concurrent_reader_never_sees_a_half_applied_edit() {
        let _guard = test_lock();
        *snapshot().write().unwrap() = Arc::new(Store::new());
        update(|store| {
            store.declare_node("test.a", NodeDefault::Allow).unwrap();
            store.declare_node("test.b", NodeDefault::Allow).unwrap();
            store
                .set_player_node("p", "test.a", None, Some(true))
                .unwrap();
            store
                .set_player_node("p", "test.b", None, Some(true))
                .unwrap();
        });

        let writer = std::thread::spawn(|| {
            for i in 0..20_000u32 {
                let on = i % 2 == 0;
                update(|store| {
                    store
                        .set_player_node("p", "test.a", None, Some(on))
                        .unwrap();
                    store
                        .set_player_node("p", "test.b", None, Some(on))
                        .unwrap();
                });
            }
        });

        for _ in 0..20_000 {
            let snap = snapshot_store();
            let player = snap
                .player("p")
                .expect("the player exists in every snapshot");
            // Both nodes are written in one `update`, so every snapshot must
            // agree on them: no reader can observe one flipped and not the
            // other.
            assert_eq!(
                player.node("test.a", None),
                player.node("test.b", None),
                "a snapshot saw a half-applied edit"
            );
        }
        writer.join().unwrap();
    }

    #[test]
    fn the_process_wide_write_model_grants_and_revokes_a_player_node() {
        let _guard = test_lock();
        // Start from a clean-ish store and take unique names.
        *snapshot().write().unwrap() = Arc::new(Store::new());
        declare_node("perm_cmd02_test", NodeDefault::Deny).unwrap();
        assert!(!check_for("perm_cmd02_subject", None, "perm_cmd02_test", 0));
        set_node("perm_cmd02_subject", "perm_cmd02_test", None, true).unwrap();
        assert!(check_for("perm_cmd02_subject", None, "perm_cmd02_test", 0));
        unset_node("perm_cmd02_subject", "perm_cmd02_test", None).unwrap();
        assert!(!check_for("perm_cmd02_subject", None, "perm_cmd02_test", 0));
    }

    // --- CMD-33a: wildcards, parent sets and user group membership ---------

    #[test]
    fn a_wildcard_is_legal_only_as_a_final_segment() {
        for ok in ["*", "a.*", "a.b.*", "a-b.c_d.*"] {
            assert!(valid_node(ok), "`{ok}` should be a legal wildcard");
        }
        for bad in ["a.*.b", "foo*", "*.a", "a..*", "*.*.b"] {
            assert!(!valid_node(bad), "`{bad}` should be rejected");
        }
    }

    #[test]
    fn wildcard_nodes_match_descendants_and_exact_nodes_win() {
        let mut s = Store::new();
        s.create_group("staff", Level::new(1)).unwrap();
        s.add_member("staff", "alice", None).unwrap();
        s.declare_node("test.wild.child", NodeDefault::Deny)
            .unwrap();
        s.declare_node("test.wild.grand.child", NodeDefault::Deny)
            .unwrap();
        s.declare_node("test.wilder.child", NodeDefault::Deny)
            .unwrap();

        s.set_group_node("staff", "test.wild.*", None, Some(true))
            .unwrap();
        assert!(s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.wild.child",
            None,
            0
        ));
        assert!(s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.wild.grand.child",
            None,
            0
        ));
        // The wildcard does not leak to a sibling prefix.
        assert!(!s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.wilder.child",
            None,
            0
        ));

        // An exact deny beats the wildcard grant within the same holder.
        s.set_group_node("staff", "test.wild.child", None, Some(false))
            .unwrap();
        assert!(!s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.wild.child",
            None,
            0
        ));
        assert!(s.resolve(
            s.level_of("alice", None),
            "alice",
            None,
            "test.wild.grand.child",
            None,
            0
        ));
    }

    #[test]
    fn the_bare_star_grants_a_node_no_group_names() {
        let mut s = Store::new();
        s.create_group("all", Level::NONE).unwrap();
        s.add_member("all", "bob", None).unwrap();
        s.set_group_node("all", "*", None, Some(true)).unwrap();
        s.declare_node("unlisted.node", NodeDefault::Deny).unwrap();
        assert!(s.resolve(
            s.level_of("bob", None),
            "bob",
            None,
            "unlisted.node",
            None,
            0
        ));
        // A player-level deny still outranks the group's `*`.
        s.set_player_node("bob", "unlisted.node", None, Some(false))
            .unwrap();
        assert!(!s.resolve(
            s.level_of("bob", None),
            "bob",
            None,
            "unlisted.node",
            None,
            0
        ));
    }

    #[test]
    fn a_context_wildcard_applies_only_in_its_world() {
        let mut s = Store::new();
        s.create_group("ctx", Level::NONE).unwrap();
        s.add_member("ctx", "carol", None).unwrap();
        s.set_group_node("ctx", "test.*", Some("overworld"), Some(true))
            .unwrap();
        s.declare_node("test.node", NodeDefault::Deny).unwrap();
        assert!(s.resolve(
            s.level_of("carol", None),
            "carol",
            None,
            "test.node",
            Some("overworld"),
            0
        ));
        assert!(!s.resolve(
            s.level_of("carol", None),
            "carol",
            None,
            "test.node",
            Some("nether"),
            0
        ));
    }

    #[test]
    fn explicit_source_names_the_group_that_decided() {
        let mut s = Store::new();
        s.create_group("source", Level::NONE).unwrap();
        s.add_member("source", "dave", None).unwrap();
        s.set_group_node("source", "test.source.*", None, Some(true))
            .unwrap();
        let (value, source) = s
            .explicit_source("dave", None, "test.source.child", None)
            .expect("the wildcard matched");
        assert!(value);
        assert_eq!(source, NodeSource::Group("source".into()));
        assert_eq!(
            s.explicit_source("nobody", None, "test.source.child", None),
            None
        );
    }

    #[test]
    fn set_group_parents_replaces_dedups_and_rejects_cycles_atomically() {
        let mut s = Store::new();
        for id in ["a", "b", "c"] {
            s.create_group(id, Level::NONE).unwrap();
        }
        s.set_group_parents("a", &["b".into(), "c".into(), "b".into()])
            .unwrap();
        assert_eq!(
            s.group("a").unwrap().parents(),
            &["b".to_string(), "c".to_string()]
        );

        // `b -> a` would close a cycle. The failed write leaves `a` untouched.
        assert!(s.set_group_parents("b", &["a".into()]).is_err());
        assert!(s.group("b").unwrap().parents().is_empty());
        assert_eq!(
            s.group("a").unwrap().parents(),
            &["b".to_string(), "c".to_string()]
        );

        // An unknown parent is refused before any edge changes.
        assert!(s.set_group_parents("b", &["missing".into()]).is_err());
        assert!(s.group("b").unwrap().parents().is_empty());

        // Clear is the empty set.
        s.set_group_parents("a", &[]).unwrap();
        assert!(s.group("a").unwrap().parents().is_empty());
    }

    #[test]
    fn set_user_groups_replaces_and_deduplicates() {
        let mut s = Store::new();
        s.create_group("one", Level::NONE).unwrap();
        s.create_group("two", Level::NONE).unwrap();
        s.set_user_groups("Eve", &["one".into(), "two".into(), "one".into()])
            .unwrap();
        let mut groups = s.groups_of("eve", None);
        groups.sort();
        assert_eq!(groups, vec!["one".to_string(), "two".to_string()]);
        s.set_user_groups("eve", &["two".into()]).unwrap();
        assert_eq!(s.groups_of("eve", None), vec!["two".to_string()]);
        s.set_user_groups("eve", &[]).unwrap();
        assert!(s.groups_of("eve", None).is_empty());
        assert!(s.set_user_groups("eve", &["missing".into()]).is_err());
    }

    #[test]
    fn wildcards_and_parent_sets_survive_a_file_round_trip() {
        let path = temp_path("wildcard");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::new();
        s.create_group("parent", Level::NONE).unwrap();
        s.create_group("child", Level::NONE).unwrap();
        s.set_group_parents("child", &["parent".into()]).unwrap();
        s.set_group_node("parent", "test.*", Some("overworld"), Some(true))
            .unwrap();
        s.save(&path).unwrap();

        let back = Store::load(&path).unwrap().expect("the file exists");
        assert_eq!(
            back.group("child").unwrap().parents(),
            &["parent".to_string()]
        );
        // Membership is not part of the group node itself: the grant belongs
        // to the group and is resolved for whoever joins it later.
        assert!(!back.resolve(
            Level::NONE,
            "someone",
            None,
            "test.anything",
            Some("overworld"),
            0
        ));
        assert_eq!(
            back.group("parent")
                .unwrap()
                .node("test.anything", Some("overworld")),
            Some(true)
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("toml.tmp"));
    }

    #[cfg(feature = "plugins")]
    #[test]
    fn the_admin_service_reads_writes_and_names_the_source() {
        use aether_plugin::permissions::{PermissionsAdmin, Source};

        let _guard = test_lock();
        install_store_for_test(Store::new());
        let admin = AdminService;
        admin.create_group("service").unwrap();
        admin
            .set_group_node("service", "service.*", None, true)
            .unwrap();
        admin.add_user_group("alice", "service").unwrap();

        let info = admin.user("alice").expect("a user view");
        assert_eq!(info.groups, vec!["service".to_string()]);
        assert_eq!(info.inherited_groups, vec!["service".to_string()]);
        let check = admin.check_user("alice", "service.child", None);
        assert!(check.value);
        assert_eq!(check.source, Source::Group("service".into()));

        admin
            .set_user_node("alice", "service.child", None, false)
            .unwrap();
        let check = admin.check_user("alice", "service.child", None);
        assert!(!check.value);
        assert_eq!(check.source, Source::User("alice".into()));

        admin
            .unset_user_node("alice", "service.child", None)
            .unwrap();
        assert!(admin.check_user("alice", "service.child", None).value);

        let group = admin.group("service").expect("a group view");
        assert!(group.members.contains(&"alice".to_string()));
        assert!(admin.group("op").expect("builtin").builtin);
        assert!(admin.delete_group("op").is_err());
        assert!(admin.delete_group("service").is_ok());
        assert!(admin.group("service").is_none());
    }
}
