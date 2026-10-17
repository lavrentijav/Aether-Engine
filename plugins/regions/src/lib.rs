//! The WorldGuard-style **regions** plugin (CMD-34a/34b): the region model, the
//! `/rg` commands, typed flags, membership and per-world bypass on top of the
//! core region index and protection check.
//!
//! This is a native (tier N) plugin: it is a Rust crate linked into the server
//! in-process, so it can hold the shared [`RegionStore`] directly and edit the
//! very index the server consults on every block event. It does **not** keep a
//! second spatial index — the names, owners and persisted fields live here,
//! the cuboids and the hot-path lookup live in `aether-region`.
//!
//! ```text
//! /rg define <id>              from the current wand selection
//! /rg redefine <id>            atomically move an existing region's bounds
//! /rg remove <id>              removed at the next snapshot (safe point)
//! /rg priority <id> <n>        change the priority used by the check
//! /rg flag <id> <flag> [value] set/clear a typed flag (allow/deny, text, int)
//! /rg addmember|removemember <id> <player>
//! /rg addowner|removeowner <id> <player>
//! /rg info [<id>]              one region, or the region under the player
//! /rg list [<player>]          every region, grouped by world
//! ```
//!
//! ## Selection: a minimal internal wand
//!
//! WorldEdit (CMD-36) is not in the tree yet, so this plugin ships the smallest
//! useful selection tool itself. Holding the configured wand item (default
//! `minecraft:golden_axe`), **left-click** a block sets point 1 and
//! **right-click** a block sets point 2. Both clicks are cancelled so the wand
//! does not break, place, open or toggle anything. Point 1 is taken at the
//! **start** of the dig (`before-block-dig`), before the region check and
//! before a survival dig begins, so a protected or unbreakable block is still
//! selectable. The bounds are normalized on every axis, so the order of the
//! clicks does not matter. The selection is per session and is cleared after a
//! successful `define`/`redefine`/`remove`, by `/rg cancel`, and on leave; only
//! the regions themselves are persisted.
//!
//! `before-block-dig` and `before-block-interact` are **native-only** events:
//! the frozen WASM `@0.1.0` world has no export for a start-of-dig or a
//! right-click at a block, and the plugin
//! declares it in `manifest().subscribe` like any other event.
//!
//! ## Persistence
//!
//! The whole region set is one JSON value under the plugin KV key `regions`.
//! A mutation updates the in-memory model, publishes one new core snapshot and
//! writes the key once; there is no window where a reader sees a half-applied
//! redefine. `init` loads the key and rebuilds the core store from it, which is
//! what makes the state survive a restart.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};
use aether_region::{
    same_world, worldguard_bypass_permission, BlockPos, FlagKind, FlagRegistry, FlagValue, Region,
    RegionId, RegionStore, BYPASS_PERMISSION, OVERWORLD,
};
use serde::{Deserialize, Serialize};

/// The plugin id: its KV namespace and log tag.
pub const ID: &str = "regions";

/// The two command names (`/rg` is the short form of `/region`).
pub const COMMAND_SHORT: &str = "rg";
/// The long form.
pub const COMMAND_LONG: &str = "region";

/// The KV key holding the serialized region set.
pub const KV_REGIONS: &str = "regions";

/// The core region `source` this plugin stamps on every region it writes, so
/// one plugin's load/edit cannot touch another plugin's regions.
pub const SOURCE: &str = "regions";

/// Permission nodes, mirroring WorldGuard's.
pub const NODE_DEFINE: &str = "worldguard.region.define";
/// Permission node for `/rg redefine`.
pub const NODE_REDEFINE: &str = "worldguard.region.redefine";
/// Permission node for `/rg remove`.
pub const NODE_REMOVE: &str = "worldguard.region.remove";
/// Permission node for `/rg priority`.
pub const NODE_PRIORITY: &str = "worldguard.region.priority";
/// Permission node for `/rg info`.
pub const NODE_INFO: &str = "worldguard.region.info";
/// Permission node for `/rg list`.
pub const NODE_LIST: &str = "worldguard.region.list";
/// Permission node for `/rg flag`.
pub const NODE_FLAG: &str = "worldguard.region.flag";
/// Permission node for `/rg addmember` and `/rg removemember`.
pub const NODE_MEMBER: &str = "worldguard.region.member";
/// Permission node for `/rg addowner` and `/rg removeowner`.
pub const NODE_OWNER: &str = "worldguard.region.owner";

/// The per-world bypass node prefix (`worldguard.region.bypass.<world>`).
pub const BYPASS_PREFIX: &str = "worldguard.region.bypass";

/// The default wand item.
pub const DEFAULT_WAND: &str = "minecraft:golden_axe";

/// The plugin manifest.
///
/// A native plugin has no runtime `plugin.toml`; the server collects this
/// value from [`registration`] at build time.
pub fn manifest() -> Manifest {
    Manifest {
        id: ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Regions".to_string(),
        // Run before the core's own command handling sees nothing; the value
        // only orders plugins that subscribe to the same event. A protection
        // plugin wants to see a break before a cosmetic one.
        priority: 20,
        capabilities: vec![
            "log".to_string(),
            "config".to_string(),
            "supports".to_string(),
            "storage.kv".to_string(),
            "players".to_string(),
        ],
        subscribe: vec![
            "before-block-dig".to_string(),
            "before-block-interact".to_string(),
            "after-player-leave".to_string(),
            "on-command".to_string(),
        ],
        // The top-level command is public so the per-subcommand
        // `worldguard.region.*` nodes are the only gate. Declaring it under
        // `worldguard.region.info` would make a deny on `info` block `define`
        // too, before the plugin ever sees the arguments.
        commands: vec![
            CommandDecl::Plain(COMMAND_SHORT.to_string()),
            CommandDecl::Plain(COMMAND_LONG.to_string()),
        ],
        tick_period: 1,
        // The protection itself is core; a fault in this plugin's wand must not
        // veto every block break. A trap only disables the plugin's own hooks.
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server links in.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(RegionsPlugin::default()),
    }
}

/// One persisted typed flag value.
///
/// `untagged` keeps the file readable and, more importantly, loads a region set
/// written by CMD-34a where every flag was a JSON boolean. A boolean flag now
/// round-trips as `Bool`; text (greeting/farewell) and integer flags are the
/// CMD-34b additions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredFlag {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl StoredFlag {
    fn core(&self) -> FlagValue {
        match self {
            StoredFlag::Bool(value) => FlagValue::Bool(*value),
            StoredFlag::Int(value) => FlagValue::Int(*value),
            StoredFlag::Text(value) => FlagValue::Text(value.clone()),
        }
    }

    fn display(&self) -> String {
        match self {
            StoredFlag::Bool(true) => "allow".to_string(),
            StoredFlag::Bool(false) => "deny".to_string(),
            StoredFlag::Int(value) => value.to_string(),
            StoredFlag::Text(value) => value.clone(),
        }
    }
}

impl From<FlagValue> for StoredFlag {
    fn from(value: FlagValue) -> Self {
        match value {
            FlagValue::Bool(value) => StoredFlag::Bool(value),
            FlagValue::Int(value) => StoredFlag::Int(value),
            FlagValue::Text(value) => StoredFlag::Text(value),
        }
    }
}

/// One persisted region. This is the plugin's data record: the core [`Region`]
/// is derived from it on every publish, so there is only one spatial index.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredRegion {
    /// Numeric id assigned once and stable across restarts. It is the core
    /// index's tie-break for equal priorities.
    id: u64,
    name: String,
    world: String,
    min: [i32; 3],
    max: [i32; 3],
    priority: i32,
    #[serde(default)]
    owners: BTreeSet<String>,
    #[serde(default)]
    members: BTreeSet<String>,
    #[serde(default)]
    flags: BTreeMap<String, StoredFlag>,
}

impl StoredRegion {
    fn region(&self) -> Region {
        let mut region = Region::new(
            RegionId(self.id),
            self.world.clone(),
            BlockPos::new(self.min[0], self.min[1], self.min[2]),
            BlockPos::new(self.max[0], self.max[1], self.max[2]),
        )
        .with_priority(self.priority)
        .with_source(SOURCE);
        for (name, value) in &self.flags {
            region.flags.set_value(name.clone(), value.core());
        }
        region.owners = self.owners.clone();
        region.members = self.members.clone();
        region
    }

    fn summary(&self) -> String {
        format!(
            "  {} (id {}) [{}, {}, {}]..[{}, {}, {}] priority {}",
            self.name,
            self.id,
            self.min[0],
            self.min[1],
            self.min[2],
            self.max[0],
            self.max[1],
            self.max[2],
            self.priority
        )
    }

    fn detail(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Region `{}` (id {})", self.name, self.id),
            format!("  world: {}", self.world),
            format!(
                "  bounds: [{}, {}, {}] .. [{}, {}, {}]",
                self.min[0], self.min[1], self.min[2], self.max[0], self.max[1], self.max[2]
            ),
            format!("  priority: {}", self.priority),
            format!(
                "  owners: {}",
                if self.owners.is_empty() {
                    "(none)".to_string()
                } else {
                    self.owners.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            ),
        ];
        if !self.members.is_empty() {
            lines.push(format!(
                "  members: {}",
                self.members.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if !self.flags.is_empty() {
            lines.push(format!(
                "  flags: {}",
                self.flags
                    .iter()
                    .map(|(name, value)| format!("{name}={}", value.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines
    }
}

/// The serialized state under [`KV_REGIONS`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoredState {
    next_id: u64,
    #[serde(default)]
    regions: BTreeMap<String, StoredRegion>,
}

/// Who is asking for an edit.
#[derive(Clone, Copy, Debug)]
struct Caller<'a> {
    name: &'a str,
    op: bool,
}

impl Caller<'_> {
    fn may_edit(&self, region: &StoredRegion) -> bool {
        self.op
            || region
                .owners
                .iter()
                .any(|owner| owner.eq_ignore_ascii_case(self.name))
    }
}

/// One player's two wand points.
#[derive(Clone, Copy, Debug, Default)]
struct Selection {
    first: Option<BlockPos>,
    second: Option<BlockPos>,
}

impl Selection {
    fn bounds(self) -> Option<(BlockPos, BlockPos)> {
        Some(normalize(self.first?, self.second?))
    }
}

/// The region model and its commands, independent of the host event plumbing
/// so the tests can drive it directly.
struct Regions {
    store: Arc<RegionStore>,
    state: StoredState,
    selections: HashMap<u64, Selection>,
    wand_item: String,
}

impl Regions {
    fn new(store: Arc<RegionStore>) -> Self {
        Regions {
            store,
            state: StoredState {
                next_id: 1,
                ..StoredState::default()
            },
            selections: HashMap::new(),
            wand_item: DEFAULT_WAND.to_string(),
        }
    }

    /// Load the persisted set and rebuild the core index from it.
    ///
    /// A failed read or a corrupt value **refuses the load**: silently
    /// starting empty would be fail-open for a protection feature and the
    /// next mutation would overwrite the still-recoverable bytes.
    fn load(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.wand_item = ctx
            .config
            .get("wand-item")
            .filter(|item| !item.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_WAND.to_string());
        // Starting empty after a failed read is fail-open, so it is only
        // allowed when the operator says so explicitly. Without that setting
        // the load is refused (the server logs an error and does not load the
        // plugin); the bytes are never overwritten.
        let allow_empty = ctx
            .config
            .get("allow-empty-on-error")
            .is_some_and(|v| matches!(v.as_str(), "true" | "1" | "yes" | "on"));
        match ctx.backend.kv_try_get(ctx.id, KV_REGIONS) {
            Ok(Some(bytes)) => match serde_json::from_slice::<StoredState>(&bytes) {
                Ok(state) => self.state = state,
                Err(e) => {
                    let message = format!("region state is unreadable: {e}");
                    ctx.log(LogLevel::Error, &message);
                    if allow_empty {
                        ctx.log(
                            LogLevel::Error,
                            "allow-empty-on-error is set: starting with no regions, protection is OFF",
                        );
                        self.state = StoredState::default();
                    } else {
                        return Err(message);
                    }
                }
            },
            Ok(None) => {}
            Err(e) => {
                let message = format!("cannot read region state: {e}");
                ctx.log(LogLevel::Error, &message);
                if allow_empty {
                    ctx.log(
                        LogLevel::Error,
                        "allow-empty-on-error is set: starting with no regions, protection is OFF",
                    );
                    self.state = StoredState::default();
                } else {
                    return Err(message);
                }
            }
        }
        // A record written by an older/hand-edited state must not make the
        // next define collide with a live id.
        let highest = self.state.regions.values().map(|r| r.id).max().unwrap_or(0);
        self.state.next_id = self.state.next_id.max(highest + 1).max(1);
        self.selections.clear();
        self.rebuild();
        Ok(())
    }

    /// Write `next` and only then adopt it in memory.
    ///
    /// Persistence-first means a failed write leaves both the in-memory model
    /// and the core index on the previous, still-persisted state, and the
    /// command reports the error instead of claiming success.
    fn persist(&mut self, ctx: &NativeContext, next: StoredState) -> Result<(), String> {
        let bytes =
            serde_json::to_vec(&next).map_err(|e| format!("cannot encode region state: {e}"))?;
        ctx.backend
            .kv_try_put(ctx.id, KV_REGIONS, &bytes)
            .map_err(|e| format!("cannot write region state: {e}"))?;
        self.state = next;
        Ok(())
    }

    /// Publish this plugin's model into the core store on load.
    ///
    /// The store is shared with every other region plugin (claims), so a load
    /// must not replace the whole index: it removes only this plugin's stale
    /// regions (ones no longer in the persisted state) and upserts its own.
    fn rebuild(&self) {
        let index = self.store.snapshot();
        for handle in index.regions() {
            if handle.source.as_deref() == Some(SOURCE) && self.by_id(handle.id.get()).is_none() {
                self.store.remove(handle.id);
            }
        }
        for region in self.state.regions.values() {
            self.store.upsert(region.region());
        }
    }

    fn publish(&self, region: &StoredRegion) {
        self.store.upsert(region.region());
    }

    fn by_name(&self, name: &str) -> Option<&StoredRegion> {
        self.state.regions.get(name)
    }

    fn by_id(&self, id: u64) -> Option<&StoredRegion> {
        self.state.regions.values().find(|r| r.id == id)
    }

    fn region_at(&self, world: &str, pos: BlockPos) -> Option<&StoredRegion> {
        // The snapshot is already in `(priority desc, id asc)` order and the
        // query is the core's own; this only maps the id back to the name.
        self.store
            .snapshot()
            .overlapping(pos)
            .find(|region| same_world(&region.world, world))
            .and_then(|region| self.by_id(region.id.get()))
    }

    fn define(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        world: &str,
        a: BlockPos,
        b: BlockPos,
    ) -> Result<StoredRegion, String> {
        validate_id(name)?;
        if self.state.regions.contains_key(name) {
            return Err(format!("region `{name}` already exists"));
        }
        let (min, max) = normalize(a, b);
        let mut next = self.state.clone();
        let id = next.next_id;
        next.next_id += 1;
        let mut owners = BTreeSet::new();
        if !caller.name.is_empty() {
            owners.insert(caller.name.to_string());
        }
        let region = StoredRegion {
            id,
            name: name.to_string(),
            world: world.to_string(),
            min: xyz(min),
            max: xyz(max),
            priority: 0,
            owners,
            members: BTreeSet::new(),
            flags: BTreeMap::new(),
        };
        next.regions.insert(region.name.clone(), region.clone());
        self.persist(ctx, next)?;
        self.publish(&region);
        Ok(region)
    }

    /// Move a region's bounds. The world is **kept** from the stored region:
    /// the selection carries block coordinates but no dimension, so taking the
    /// caller's current world would silently move a region created elsewhere.
    fn redefine(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        a: BlockPos,
        b: BlockPos,
    ) -> Result<StoredRegion, String> {
        let previous = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&previous) {
            return Err(format!("you do not own region `{name}`"));
        }
        let (min, max) = normalize(a, b);
        let updated = StoredRegion {
            min: xyz(min),
            max: xyz(max),
            ..previous
        };
        // One record, one core upsert, one KV write: an observer sees either
        // the old box or the new one, never a mix of the two.
        let mut next = self.state.clone();
        next.regions.insert(updated.name.clone(), updated.clone());
        self.persist(ctx, next)?;
        self.publish(&updated);
        Ok(updated)
    }

    fn remove(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
    ) -> Result<StoredRegion, String> {
        let region = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&region) {
            return Err(format!("you do not own region `{name}`"));
        }
        let mut next = self.state.clone();
        next.regions.remove(name);
        self.persist(ctx, next)?;
        self.store.remove(RegionId(region.id));
        Ok(region)
    }

    fn set_priority(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        priority: i32,
    ) -> Result<StoredRegion, String> {
        let previous = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&previous) {
            return Err(format!("you do not own region `{name}`"));
        }
        let updated = StoredRegion {
            priority,
            ..previous
        };
        let mut next = self.state.clone();
        next.regions.insert(updated.name.clone(), updated.clone());
        self.persist(ctx, next)?;
        self.publish(&updated);
        Ok(updated)
    }

    /// Set (`Some`) or clear (`None`) one typed flag.
    ///
    /// The core index is republished with the whole updated region, so the
    /// change is visible to the next check snapshot and a caller already
    /// holding one keeps the old, consistent answer until it asks again.
    fn set_flag(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        flag: &str,
        value: Option<StoredFlag>,
    ) -> Result<StoredRegion, String> {
        let previous = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&previous) {
            return Err(format!("you do not own region `{name}`"));
        }
        let mut updated = previous;
        match value {
            Some(value) => {
                updated.flags.insert(flag.to_string(), value);
            }
            None => {
                updated.flags.remove(flag);
            }
        }
        let mut next = self.state.clone();
        next.regions.insert(updated.name.clone(), updated.clone());
        self.persist(ctx, next)?;
        self.publish(&updated);
        Ok(updated)
    }

    /// Add or remove a member. Returns the updated region and whether the set
    /// actually changed (an add of an existing name is a no-op).
    fn set_member(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        player: &str,
        add: bool,
    ) -> Result<(StoredRegion, bool), String> {
        let previous = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&previous) {
            return Err(format!("you do not own region `{name}`"));
        }
        let mut updated = previous;
        let changed = if add {
            insert_name(&mut updated.members, player)
        } else {
            remove_name(&mut updated.members, player).is_some()
        };
        if !changed {
            return Ok((updated, false));
        }
        let mut next = self.state.clone();
        next.regions.insert(updated.name.clone(), updated.clone());
        self.persist(ctx, next)?;
        self.publish(&updated);
        Ok((updated, true))
    }

    /// Add or remove an owner, with the same persistence and safe-point rules
    /// as [`Regions::set_member`].
    fn set_owner(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        player: &str,
        add: bool,
    ) -> Result<(StoredRegion, bool), String> {
        let previous = self
            .by_name(name)
            .cloned()
            .ok_or_else(|| format!("region `{name}` does not exist"))?;
        if !caller.may_edit(&previous) {
            return Err(format!("you do not own region `{name}`"));
        }
        let mut updated = previous;
        let changed = if add {
            insert_name(&mut updated.owners, player)
        } else {
            remove_name(&mut updated.owners, player).is_some()
        };
        if !changed {
            return Ok((updated, false));
        }
        let mut next = self.state.clone();
        next.regions.insert(updated.name.clone(), updated.clone());
        self.persist(ctx, next)?;
        self.publish(&updated);
        Ok((updated, true))
    }

    /// Forget a player's two points (after a define/redefine/remove, on leave,
    /// or via `/rg cancel`).
    fn clear_selection(&mut self, player: u64) {
        self.selections.remove(&player);
    }

    fn set_point(&mut self, player: u64, first: bool, pos: BlockPos) {
        let selection = self.selections.entry(player).or_default();
        if first {
            selection.first = Some(pos);
        } else {
            selection.second = Some(pos);
        }
    }

    fn selection(&self, player: u64) -> Option<(BlockPos, BlockPos)> {
        self.selections.get(&player).and_then(|s| s.bounds())
    }

    fn is_wand(&self, ctx: &NativeContext, p: &Player) -> bool {
        ctx.backend
            .held_item(ctx.id, p.id)
            .is_some_and(|held| same_item(&held, &self.wand_item))
    }

    fn list(&self, owner: Option<&str>) -> Vec<String> {
        let mut by_world: BTreeMap<&str, Vec<&StoredRegion>> = BTreeMap::new();
        for region in self.state.regions.values() {
            if let Some(owner) = owner {
                if !region
                    .owners
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(owner))
                {
                    continue;
                }
            }
            by_world.entry(&region.world).or_default().push(region);
        }
        if by_world.is_empty() {
            return vec!["No regions defined.".to_string()];
        }
        let mut lines = Vec::new();
        for (world, mut regions) in by_world {
            regions.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
            lines.push(format!("Regions in {world} ({}):", regions.len()));
            lines.extend(regions.into_iter().map(StoredRegion::summary));
        }
        lines
    }
}

/// Normalize the two corners so `min <= max` on every axis.
fn normalize(a: BlockPos, b: BlockPos) -> (BlockPos, BlockPos) {
    (
        BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

fn xyz(pos: BlockPos) -> [i32; 3] {
    [pos.x, pos.y, pos.z]
}

/// Insert `name` unless an existing entry matches case-insensitively.
fn insert_name(set: &mut BTreeSet<String>, name: &str) -> bool {
    if set
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(name))
    {
        return false;
    }
    set.insert(name.to_string())
}

/// Remove the entry matching `name` case-insensitively, returning its stored
/// spelling.
fn remove_name(set: &mut BTreeSet<String>, name: &str) -> Option<String> {
    let existing = set
        .iter()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()?;
    set.remove(&existing);
    Some(existing)
}

/// A member/owner name: a Minecraft-style name (`[A-Za-z0-9_]`, 1..=32).
fn validate_player(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("player name must not be empty".to_string());
    }
    if name.len() > 32 {
        return Err("player name must be at most 32 characters".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "invalid player name `{name}`: use letters, digits or `_`"
        ));
    }
    Ok(())
}

/// Parse the text after `/rg flag <id> <flag>` for the flag's declared type.
///
/// Booleans accept WorldGuard's `allow`/`deny` as well as the usual
/// `true`/`false`/`yes`/`no`/`on`/`off`; anything else is an error, never a
/// silent default.
fn parse_flag_value(kind: FlagKind, raw: &str) -> Result<StoredFlag, String> {
    match kind {
        FlagKind::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
            "allow" | "true" | "yes" | "on" | "1" => Ok(StoredFlag::Bool(true)),
            "deny" | "false" | "no" | "off" | "0" => Ok(StoredFlag::Bool(false)),
            _ => Err(format!(
                "`{raw}` is not a valid value for a boolean flag (use allow or deny)"
            )),
        },
        FlagKind::Integer => raw
            .trim()
            .parse::<i64>()
            .map(StoredFlag::Int)
            .map_err(|_| format!("`{raw}` is not a valid integer flag value")),
        FlagKind::Text => {
            let value = raw.trim();
            if value.is_empty() {
                return Err("a text flag value must not be empty".to_string());
            }
            if value.len() > 256 {
                return Err("a text flag value must be at most 256 characters".to_string());
            }
            Ok(StoredFlag::Text(value.to_string()))
        }
    }
}

/// The flag name a WorldGuard user expects for a core [`aether_region::Action`].
///
/// Kept next to the registry so a test can pin the mapping the hot path uses.
pub fn flag_for_action(action: aether_region::Action) -> &'static str {
    action.flag()
}

/// The per-world WorldGuard bypass node (`worldguard.region.bypass.<world>`).
pub fn bypass_permission(world: &str) -> String {
    debug_assert!(worldguard_bypass_permission(world).starts_with(BYPASS_PREFIX));
    worldguard_bypass_permission(world)
}

/// Whether `p` may bypass region protection in `world`.
///
/// An operator always may (the node is undeclared, so it defaults to op), and a
/// non-operator may when granted either the WorldGuard per-world node or the
/// core [`BYPASS_PERMISSION`]. The check itself is core; this is the same
/// resolution the server uses for its hooks.
pub fn bypass(ctx: &NativeContext<'_>, p: &Player, world: &str) -> bool {
    p.operator
        || ctx
            .backend
            .permission_check(ctx.id, p.id, &worldguard_bypass_permission(world), 0)
        || ctx
            .backend
            .permission_check(ctx.id, p.id, BYPASS_PERMISSION, 0)
}

/// Accept `[A-Za-z0-9_-]`, 1..=32 characters. This keeps an id safe as a KV
/// field and as a command argument, and gives a clear error for `bad id`.
fn validate_id(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("region id must not be empty".to_string());
    }
    if name.len() > 32 {
        return Err("region id must be at most 32 characters".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid region id `{name}`: use letters, digits, `_` or `-`"
        ));
    }
    Ok(())
}

/// Compare item names ignoring the `minecraft:` namespace.
fn same_item(a: &str, b: &str) -> bool {
    fn bare(item: &str) -> &str {
        let item = item.trim();
        match item.get(..10) {
            Some(prefix) if prefix.eq_ignore_ascii_case("minecraft:") => &item[10..],
            _ => item,
        }
    }
    bare(a).eq_ignore_ascii_case(bare(b))
}

/// The plugin.
pub struct RegionsPlugin {
    regions: Regions,
}

impl Default for RegionsPlugin {
    fn default() -> Self {
        RegionsPlugin {
            regions: Regions::new(aether_region::global()),
        }
    }
}

impl RegionsPlugin {
    /// A plugin over a specific store. The server uses [`Default`] (the
    /// process-wide store the protection hooks read); tests use this to keep
    /// their regions off the global index.
    pub fn with_store(store: Arc<RegionStore>) -> Self {
        RegionsPlugin {
            regions: Regions::new(store),
        }
    }

    fn command(&mut self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        let Some(sub) = args.first().map(String::as_str) else {
            return usage();
        };
        match sub {
            "define" => {
                if !permitted(ctx, p, NODE_DEFINE) {
                    return vec![denied(NODE_DEFINE)];
                }
                let Some(name) = args.get(1) else {
                    return vec!["Usage: /rg define <id>".to_string()];
                };
                if args.len() != 2 {
                    return vec!["Usage: /rg define <id>".to_string()];
                }
                let Some((a, b)) = self.regions.selection(p.id) else {
                    return vec![
                        "Select two points with the wand first (left click, then right click)."
                            .to_string(),
                    ];
                };
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                let world = world_of(ctx, p);
                match self.regions.define(ctx, caller, name, &world, a, b) {
                    Ok(region) => {
                        self.regions.clear_selection(p.id);
                        vec![format!(
                            "Created region `{}` (id {}).",
                            region.name, region.id
                        )]
                    }
                    Err(e) => vec![format!("Cannot define region: {e}")],
                }
            }
            "redefine" => {
                if !permitted(ctx, p, NODE_REDEFINE) {
                    return vec![denied(NODE_REDEFINE)];
                }
                let Some(name) = args.get(1) else {
                    return vec!["Usage: /rg redefine <id>".to_string()];
                };
                if args.len() != 2 {
                    return vec!["Usage: /rg redefine <id>".to_string()];
                }
                let Some((a, b)) = self.regions.selection(p.id) else {
                    return vec![
                        "Select two points with the wand first (left click, then right click)."
                            .to_string(),
                    ];
                };
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                match self.regions.redefine(ctx, caller, name, a, b) {
                    Ok(region) => {
                        self.regions.clear_selection(p.id);
                        vec![format!(
                            "Moved region `{}` to [{}, {}, {}]..[{}, {}, {}].",
                            region.name,
                            region.min[0],
                            region.min[1],
                            region.min[2],
                            region.max[0],
                            region.max[1],
                            region.max[2]
                        )]
                    }
                    Err(e) => vec![format!("Cannot redefine region: {e}")],
                }
            }
            "remove" => {
                if !permitted(ctx, p, NODE_REMOVE) {
                    return vec![denied(NODE_REMOVE)];
                }
                let Some(name) = args.get(1) else {
                    return vec!["Usage: /rg remove <id>".to_string()];
                };
                if args.len() != 2 {
                    return vec!["Usage: /rg remove <id>".to_string()];
                }
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                match self.regions.remove(ctx, caller, name) {
                    Ok(region) => {
                        self.regions.clear_selection(p.id);
                        vec![format!(
                            "Removed region `{}`; the next protection check no longer sees it.",
                            region.name
                        )]
                    }
                    Err(e) => vec![format!("Cannot remove region: {e}")],
                }
            }
            "priority" => {
                if !permitted(ctx, p, NODE_PRIORITY) {
                    return vec![denied(NODE_PRIORITY)];
                }
                let (Some(name), Some(raw)) = (args.get(1), args.get(2)) else {
                    return vec!["Usage: /rg priority <id> <n>".to_string()];
                };
                if args.len() != 3 {
                    return vec!["Usage: /rg priority <id> <n>".to_string()];
                }
                let priority = match raw.parse::<i32>() {
                    Ok(value) => value,
                    Err(_) => return vec![format!("`{raw}` is not a valid priority")],
                };
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                match self.regions.set_priority(ctx, caller, name, priority) {
                    Ok(region) => vec![format!(
                        "Region `{}` priority is now {}.",
                        region.name, region.priority
                    )],
                    Err(e) => vec![format!("Cannot set priority: {e}")],
                }
            }
            "flag" => {
                if !permitted(ctx, p, NODE_FLAG) {
                    return vec![denied(NODE_FLAG)];
                }
                let (Some(name), Some(flag)) = (args.get(1), args.get(2)) else {
                    return vec!["Usage: /rg flag <id> <flag> [value]".to_string()];
                };
                let flag = flag.to_ascii_lowercase();
                let registry = FlagRegistry::builtin();
                let Some(kind) = registry.kind(&flag) else {
                    return vec![format!(
                        "Unknown flag `{flag}`. Try one of: {}",
                        registry
                            .iter()
                            .map(|(name, _)| name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )];
                };
                if args.len() == 3 {
                    let Some(region) = self.regions.by_name(name) else {
                        return vec![format!("Region `{name}` does not exist.")];
                    };
                    return match region.flags.get(&flag) {
                        Some(value) => vec![format!(
                            "Flag `{flag}` on region `{}` is {}.",
                            region.name,
                            value.display()
                        )],
                        None => vec![format!(
                            "Flag `{flag}` on region `{}` is not set.",
                            region.name
                        )],
                    };
                }
                let raw = match kind {
                    FlagKind::Text => args[3..].join(" "),
                    _ if args.len() == 4 => args[3].clone(),
                    _ => return vec!["Usage: /rg flag <id> <flag> [value]".to_string()],
                };
                let value = if raw.eq_ignore_ascii_case("none") || raw.eq_ignore_ascii_case("unset")
                {
                    None
                } else {
                    match parse_flag_value(kind, &raw) {
                        Ok(value) => Some(value),
                        Err(e) => return vec![format!("Cannot set flag: {e}")],
                    }
                };
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                let value_for_line = value.as_ref().map(StoredFlag::display);
                match self.regions.set_flag(ctx, caller, name, &flag, value) {
                    Ok(region) => match value_for_line {
                        Some(value) => vec![format!(
                            "Set flag `{flag}` on region `{}` to {value}.",
                            region.name
                        )],
                        None => vec![format!(
                            "Cleared flag `{flag}` on region `{}`.",
                            region.name
                        )],
                    },
                    Err(e) => vec![format!("Cannot set flag: {e}")],
                }
            }
            "addmember" | "removemember" => {
                if !permitted(ctx, p, NODE_MEMBER) {
                    return vec![denied(NODE_MEMBER)];
                }
                let (Some(name), Some(player)) = (args.get(1), args.get(2)) else {
                    return vec![format!("Usage: /rg {sub} <id> <player>")];
                };
                if args.len() != 3 {
                    return vec![format!("Usage: /rg {sub} <id> <player>")];
                }
                if let Err(e) = validate_player(player) {
                    return vec![format!("Cannot change members: {e}")];
                }
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                let add = sub == "addmember";
                match self.regions.set_member(ctx, caller, name, player, add) {
                    Ok((region, true)) if add => vec![format!(
                        "Added `{player}` as a member of region `{}`.",
                        region.name
                    )],
                    Ok((region, false)) if add => vec![format!(
                        "`{player}` is already a member of region `{}`.",
                        region.name
                    )],
                    Ok((region, true)) => vec![format!(
                        "Removed `{player}` from region `{}` members.",
                        region.name
                    )],
                    Ok((region, false)) => vec![format!(
                        "`{player}` is not a member of region `{}`.",
                        region.name
                    )],
                    Err(e) => vec![format!("Cannot change members: {e}")],
                }
            }
            "addowner" | "removeowner" => {
                if !permitted(ctx, p, NODE_OWNER) {
                    return vec![denied(NODE_OWNER)];
                }
                let (Some(name), Some(player)) = (args.get(1), args.get(2)) else {
                    return vec![format!("Usage: /rg {sub} <id> <player>")];
                };
                if args.len() != 3 {
                    return vec![format!("Usage: /rg {sub} <id> <player>")];
                }
                if let Err(e) = validate_player(player) {
                    return vec![format!("Cannot change owners: {e}")];
                }
                let caller = Caller {
                    name: &p.name,
                    op: p.operator,
                };
                let add = sub == "addowner";
                match self.regions.set_owner(ctx, caller, name, player, add) {
                    Ok((region, true)) if add => vec![format!(
                        "Added `{player}` as an owner of region `{}`.",
                        region.name
                    )],
                    Ok((region, false)) if add => vec![format!(
                        "`{player}` is already an owner of region `{}`.",
                        region.name
                    )],
                    Ok((region, true)) => vec![format!(
                        "Removed `{player}` from region `{}` owners.",
                        region.name
                    )],
                    Ok((region, false)) => vec![format!(
                        "`{player}` is not an owner of region `{}`.",
                        region.name
                    )],
                    Err(e) => vec![format!("Cannot change owners: {e}")],
                }
            }
            "info" => {
                if !permitted(ctx, p, NODE_INFO) {
                    return vec![denied(NODE_INFO)];
                }
                match args.get(1) {
                    Some(name) => match self.regions.by_name(name) {
                        Some(region) => region.detail(),
                        None => vec![format!("Region `{name}` does not exist.")],
                    },
                    None => {
                        let Some((world, x, y, z)) = ctx.backend.player_position(ctx.id, p.id)
                        else {
                            return vec!["No region here.".to_string()];
                        };
                        let pos =
                            BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                        match self.regions.region_at(&world, pos) {
                            Some(region) => region.detail(),
                            None => vec![format!(
                                "No region at {}, {}, {} in {world}.",
                                pos.x, pos.y, pos.z
                            )],
                        }
                    }
                }
            }
            "list" => {
                if !permitted(ctx, p, NODE_LIST) {
                    return vec![denied(NODE_LIST)];
                }
                if args.len() > 2 {
                    return vec!["Usage: /rg list [<player>]".to_string()];
                }
                self.regions.list(args.get(1).map(String::as_str))
            }
            "cancel" => {
                if args.len() != 1 {
                    return vec!["Usage: /rg cancel".to_string()];
                }
                self.regions.clear_selection(p.id);
                vec!["Wand selection cleared.".to_string()]
            }
            "help" => usage(),
            other => vec![format!("Unknown region command `{other}`. Try /rg help.")],
        }
    }
}

fn permitted(ctx: &NativeContext, p: &Player, node: &str) -> bool {
    p.operator || ctx.backend.permission_check(ctx.id, p.id, node, 0)
}

/// The dimension the player is in, for a region created now. Falling back to
/// the overworld keeps a host without `player_position` (a test) working.
fn world_of(ctx: &NativeContext, p: &Player) -> String {
    ctx.backend
        .player_position(ctx.id, p.id)
        .map(|(world, _, _, _)| world)
        .unwrap_or_else(|| OVERWORLD.to_string())
}

fn denied(node: &str) -> String {
    format!("You do not have permission ({node}).")
}

fn usage() -> Vec<String> {
    vec![
        "Regions:".to_string(),
        "  /rg define <id>        create a region from the wand selection".to_string(),
        "  /rg redefine <id>      move its bounds to the wand selection".to_string(),
        "  /rg remove <id>        remove it".to_string(),
        "  /rg priority <id> <n>  change its priority".to_string(),
        "  /rg flag <id> <flag> [value]  set/clear a typed flag (allow|deny)".to_string(),
        "  /rg addmember <id> <player>    grant build/use rights".to_string(),
        "  /rg removemember <id> <player>".to_string(),
        "  /rg addowner <id> <player>     grant full region rights".to_string(),
        "  /rg removeowner <id> <player>".to_string(),
        "  /rg info [<id>]        show one region, or the one you stand in".to_string(),
        "  /rg list [<player>]    list regions, optionally only one owner's".to_string(),
        "  /rg cancel             clear the wand selection".to_string(),
        "Wand: hold the configured item and left-click point 1, right-click point 2.".to_string(),
    ]
}

impl NativePlugin for RegionsPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.regions.load(ctx)?;
        ctx.log(
            LogLevel::Info,
            &format!(
                "regions ready: {} region(s), wand={}",
                self.regions.state.regions.len(),
                self.regions.wand_item
            ),
        );
        Ok(())
    }

    fn shutdown(&mut self, ctx: &mut NativeContext) {
        ctx.log(LogLevel::Info, "regions plugin stopping");
    }

    fn before_block_dig(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        _block: &str,
    ) -> Decision {
        if !self.regions.is_wand(ctx, p) {
            return Decision::Allow;
        }
        let pos = BlockPos::new(x, y, z);
        self.regions.set_point(p.id, true, pos);
        ctx.backend.send_message(
            ctx.id,
            p.id,
            &format!("First position set to ({}, {}, {}).", pos.x, pos.y, pos.z),
        );
        Decision::Cancel(String::new())
    }

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        // Entity ids are reused; a new player must not inherit a selection.
        self.regions.clear_selection(p.id);
    }

    fn before_block_interact(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        _block: &str,
    ) -> Decision {
        if !self.regions.is_wand(ctx, p) {
            return Decision::Allow;
        }
        let pos = BlockPos::new(x, y, z);
        self.regions.set_point(p.id, false, pos);
        ctx.backend.send_message(
            ctx.id,
            p.id,
            &format!("Second position set to ({}, {}, {}).", pos.x, pos.y, pos.z),
        );
        Decision::Cancel(String::new())
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        if name != COMMAND_SHORT && name != COMMAND_LONG {
            return None;
        }
        Some(CommandResult {
            handled: true,
            lines: self.command(ctx, p, args),
        })
    }
}

#[cfg(test)]
mod tests;
