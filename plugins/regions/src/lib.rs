//! The WorldGuard-style **regions** plugin (CMD-34a): the region model and the
//! `/rg` commands on top of the core region index and protection check.
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
//! never breaks or places anything. The bounds are normalized on every axis, so
//! the order of the clicks does not matter. The selection is per session; only
//! the regions themselves are persisted.
//!
//! `before-block-interact` is a **native-only** event: the frozen WASM
//! `@0.1.0` world has no export for a right-click at a block, and the plugin
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
use aether_region::{same_world, BlockPos, Region, RegionId, RegionStore, OVERWORLD};
use serde::{Deserialize, Serialize};

/// The plugin id: its KV namespace and log tag.
pub const ID: &str = "regions";

/// The two command names (`/rg` is the short form of `/region`).
pub const COMMAND_SHORT: &str = "rg";
/// The long form.
pub const COMMAND_LONG: &str = "region";

/// The KV key holding the serialized region set.
pub const KV_REGIONS: &str = "regions";

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
            "world.read".to_string(),
            "players".to_string(),
        ],
        subscribe: vec![
            "before-block-break".to_string(),
            "before-block-interact".to_string(),
            "after-player-join".to_string(),
            "on-command".to_string(),
        ],
        commands: vec![
            CommandDecl::Detailed {
                name: COMMAND_SHORT.to_string(),
                node: NODE_INFO.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_LONG.to_string(),
                node: NODE_INFO.to_string(),
                op: 0,
            },
        ],
        tick_period: 1,
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
    flags: BTreeMap<String, bool>,
}

impl StoredRegion {
    fn region(&self) -> Region {
        let mut region = Region::new(
            RegionId(self.id),
            self.world.clone(),
            BlockPos::new(self.min[0], self.min[1], self.min[2]),
            BlockPos::new(self.max[0], self.max[1], self.max[2]),
        )
        .with_priority(self.priority);
        for (name, allowed) in &self.flags {
            region.flags.set(name.clone(), *allowed);
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

    fn load(&mut self, ctx: &mut NativeContext) {
        self.wand_item = ctx
            .config
            .get("wand-item")
            .filter(|item| !item.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_WAND.to_string());
        if let Some(bytes) = ctx.backend.kv_get(ctx.id, KV_REGIONS) {
            match serde_json::from_slice::<StoredState>(&bytes) {
                Ok(state) => self.state = state,
                Err(e) => ctx.log(
                    LogLevel::Warn,
                    &format!("ignoring unreadable region state: {e}"),
                ),
            }
        }
        // A record written by an older/hand-edited state must not make the
        // next define collide with a live id.
        let highest = self.state.regions.values().map(|r| r.id).max().unwrap_or(0);
        self.state.next_id = self.state.next_id.max(highest + 1).max(1);
        self.selections.clear();
        self.rebuild();
    }

    fn save(&self, ctx: &NativeContext) {
        match serde_json::to_vec(&self.state) {
            Ok(bytes) => ctx.backend.kv_put(ctx.id, KV_REGIONS, &bytes),
            Err(e) => ctx.log(LogLevel::Error, &format!("cannot encode region state: {e}")),
        }
    }

    /// Publish the whole model into the core store. Used on load; edits publish
    /// one region at a time, through the same store.
    fn rebuild(&self) {
        self.store
            .replace_all(self.state.regions.values().map(|r| r.region().handle()));
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
        let id = self.state.next_id;
        self.state.next_id += 1;
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
        self.state
            .regions
            .insert(region.name.clone(), region.clone());
        self.publish(&region);
        self.save(ctx);
        Ok(region)
    }

    fn redefine(
        &mut self,
        ctx: &mut NativeContext,
        caller: Caller<'_>,
        name: &str,
        world: &str,
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
            world: world.to_string(),
            min: xyz(min),
            max: xyz(max),
            ..previous
        };
        // One record, one core upsert, one KV write: an observer sees either
        // the old box or the new one, never a mix of the two.
        self.state
            .regions
            .insert(updated.name.clone(), updated.clone());
        self.publish(&updated);
        self.save(ctx);
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
        self.state.regions.remove(name);
        self.store.remove(RegionId(region.id));
        self.save(ctx);
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
        self.state
            .regions
            .insert(updated.name.clone(), updated.clone());
        self.publish(&updated);
        self.save(ctx);
        Ok(updated)
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
                match self.regions.define(ctx, caller, name, OVERWORLD, a, b) {
                    Ok(region) => vec![format!(
                        "Created region `{}` (id {}).",
                        region.name, region.id
                    )],
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
                match self.regions.redefine(ctx, caller, name, OVERWORLD, a, b) {
                    Ok(region) => vec![format!(
                        "Moved region `{}` to [{}, {}, {}]..[{}, {}, {}].",
                        region.name,
                        region.min[0],
                        region.min[1],
                        region.min[2],
                        region.max[0],
                        region.max[1],
                        region.max[2]
                    )],
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
                    Ok(region) => vec![format!(
                        "Removed region `{}`; the next protection check no longer sees it.",
                        region.name
                    )],
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
            "help" => usage(),
            other => vec![format!("Unknown region command `{other}`. Try /rg help.")],
        }
    }
}

fn permitted(ctx: &NativeContext, p: &Player, node: &str) -> bool {
    p.operator || ctx.backend.permission_check(ctx.id, p.id, node, 0)
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
        "  /rg info [<id>]        show one region, or the one you stand in".to_string(),
        "  /rg list [<player>]    list regions, optionally only one owner's".to_string(),
        "Wand: hold the configured item and left-click point 1, right-click point 2.".to_string(),
    ]
}

impl NativePlugin for RegionsPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.regions.load(ctx);
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

    fn before_block_break(
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
