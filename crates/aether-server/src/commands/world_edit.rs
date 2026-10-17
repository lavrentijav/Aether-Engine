//! Vanilla world-editing commands: `/setblock`, `/fill` and `/clone` (CMD-12a).
//!
//! Every write goes through the core bulk-edit transaction ([`crate::bulk`]):
//! the whole command is staged first, checked against the region index, and
//! committed as **one** journal revision with the caller as its actor. Nothing
//! reaches the world until every check has passed, so a refused command leaves
//! no blocks and no journal entry behind. Publishing (column resends, shape
//! settle and after-block hooks) is the normal bulk publisher, which is why a
//! client sees the same result as a player placing the blocks by hand.
//!
//! The commands follow vanilla 1.21.11's grammar and limits:
//!
//! * `/setblock <pos> <block> [destroy|keep|replace]`
//! * `/fill <from> <to> <block> [destroy|hollow|keep|outline|replace]`
//! * `/fill <from> <to> replace <filter> <block>`
//! * `/clone [from <dimension>] <begin> <end> [to <dimension>] <destination>
//!   [replace|masked] [force|normal] [filtered <filter>]`
//!
//! Both `/fill` and `/clone` are capped at 32768 blocks, the cap is checked
//! before a single cell is read, and block positions outside the engine's
//! vertical range are refused. A block filter is a block state (properties
//! optional) or a `#tag`, resolved through the pack's block tags when one is
//! configured and through the built-in name rules otherwise.

use aether_api::{JournalActor, Vector3};
use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use super::args::BlockState;
use super::coordinates::{CoordinateSource, Coordinates, Rotation};
use super::tree::{self, ArgKind, Ctx, Node, Requirement, Value};
use super::Reply;
use crate::bulk::{self, BulkEdit};
use crate::commands::coordinates::BlockPos;
use crate::game::{entities, tables};
use crate::inventory::Stack;
use crate::session::{DemoWorld, WORLD_BOTTOM, WORLD_TOP};

/// Vanilla's per-command cap for `/fill` and `/clone`.
pub const MAX_VOLUME: u128 = 32_768;

const NODE_SETBLOCK: &str = "minecraft.command.setblock";
const NODE_FILL: &str = "minecraft.command.fill";
const NODE_CLONE: &str = "minecraft.command.clone";

const ERR_TOO_MANY: &str = "Too many blocks in the specified area";
const ERR_OUTSIDE: &str = "That position is outside of the world";
const ERR_OVERLAP: &str = "The source and destination can't overlap";
const ERR_CLONE_FAILED: &str = "Could not clone blocks";

/// A position with no source anchor: enough for absolute coordinates, and a
/// clean `Unknown position` error for `~`/`^` from the console.
struct NoSource;

impl CoordinateSource for NoSource {
    fn position(&self) -> Option<Vector3> {
        None
    }

    fn rotation(&self) -> Option<Rotation> {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetMode {
    Replace,
    Destroy,
    Keep,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillMode {
    Replace,
    Destroy,
    Hollow,
    Keep,
    Outline,
    Filtered,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneMask {
    Replace,
    Masked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneForce {
    Force,
    Normal,
}

// ---------------------------------------------------------------------------
// Command tree
// ---------------------------------------------------------------------------

/// The three world-edit command nodes.
pub fn nodes() -> Vec<Node> {
    vec![setblock_node(), fill_node(), clone_node()]
}

fn gate(node: &'static str) -> Requirement {
    Requirement::Level { op: 2, node }
}

fn exec_setblock(ctx: &mut Ctx, mode: SetMode) -> Reply {
    match setblock(ctx, mode) {
        Ok(line) => Reply::one(line),
        Err(e) => ctx.fail(e),
    }
}

fn setblock_node() -> Node {
    let block = tree::Builder::arg("block", ArgKind::BlockState)
        .executable(|ctx| exec_setblock(ctx, SetMode::Replace))
        .then(
            tree::Builder::literal("destroy")
                .executable(|ctx| exec_setblock(ctx, SetMode::Destroy)),
        )
        .then(tree::Builder::literal("keep").executable(|ctx| exec_setblock(ctx, SetMode::Keep)))
        .then(
            tree::Builder::literal("replace")
                .executable(|ctx| exec_setblock(ctx, SetMode::Replace)),
        );

    tree::Builder::literal("setblock")
        .requires(gate(NODE_SETBLOCK))
        .usage("<pos> <block> [destroy|keep|replace]")
        .executable(|ctx| ctx.fail("/setblock <pos> <block> [destroy|keep|replace]"))
        .then(tree::Builder::arg("pos", ArgKind::BlockPos).then(block))
        .build()
}

fn exec_fill(ctx: &mut Ctx, mode: FillMode) -> Reply {
    match fill(ctx, mode) {
        Ok(line) => Reply::one(line),
        Err(e) => ctx.fail(e),
    }
}

fn fill_node() -> Node {
    let filtered = tree::Builder::literal("replace").then(
        tree::Builder::arg("filter", ArgKind::String).then(
            tree::Builder::arg("block", ArgKind::BlockState)
                .executable(|ctx| exec_fill(ctx, FillMode::Filtered)),
        ),
    );

    let block = tree::Builder::arg("block", ArgKind::BlockState)
        .executable(|ctx| exec_fill(ctx, FillMode::Replace))
        .then(tree::Builder::literal("destroy").executable(|ctx| exec_fill(ctx, FillMode::Destroy)))
        .then(tree::Builder::literal("hollow").executable(|ctx| exec_fill(ctx, FillMode::Hollow)))
        .then(tree::Builder::literal("keep").executable(|ctx| exec_fill(ctx, FillMode::Keep)))
        .then(tree::Builder::literal("outline").executable(|ctx| exec_fill(ctx, FillMode::Outline)))
        .then(
            tree::Builder::literal("replace").executable(|ctx| exec_fill(ctx, FillMode::Replace)),
        );

    tree::Builder::literal("fill")
        .requires(gate(NODE_FILL))
        .usage("<from> <to> <block> [destroy|hollow|keep|outline|replace]")
        .executable(|ctx| {
            ctx.fail(
                "/fill <from> <to> <block> [destroy|hollow|keep|outline|replace] | \
                 <from> <to> replace <filter> <block>",
            )
        })
        .then(
            tree::Builder::arg("from", ArgKind::BlockPos).then(
                tree::Builder::arg("to", ArgKind::BlockPos)
                    .then(block)
                    .then(filtered),
            ),
        )
        .build()
}

fn exec_clone(
    ctx: &mut Ctx,
    cross: bool,
    mask: CloneMask,
    force: CloneForce,
    filtered: bool,
) -> Reply {
    match clone(ctx, cross, mask, force, filtered) {
        Ok(line) => Reply::one(line),
        Err(e) => ctx.fail(e),
    }
}

/// A `filtered <filter>` tail.
fn clone_filtered(cross: bool, mask: CloneMask, force: CloneForce) -> tree::Builder {
    tree::Builder::literal("filtered").then(
        tree::Builder::arg("filter", ArgKind::String)
            .executable(move |ctx| exec_clone(ctx, cross, mask, force, true)),
    )
}

/// A `force`/`normal` tail, with its own optional `filtered`.
fn clone_force(
    cross: bool,
    mask: CloneMask,
    force: CloneForce,
    word: &'static str,
) -> tree::Builder {
    let node = tree::Builder::literal(word)
        .executable(move |ctx| exec_clone(ctx, cross, mask, force, false));
    node.then(clone_filtered(cross, mask, force))
}

/// A `replace`/`masked` branch, with optional `force`/`normal`/`filtered`.
fn clone_mask(cross: bool, mask: CloneMask, word: &'static str) -> tree::Builder {
    let node = tree::Builder::literal(word)
        .executable(move |ctx| exec_clone(ctx, cross, mask, CloneForce::Normal, false));
    node.then(clone_force(cross, mask, CloneForce::Force, "force"))
        .then(clone_force(cross, mask, CloneForce::Normal, "normal"))
        .then(clone_filtered(cross, mask, CloneForce::Normal))
}

/// The destination argument and every mode/filter tail.
fn clone_destination(cross: bool) -> tree::Builder {
    let node = tree::Builder::arg("destination", ArgKind::BlockPos).executable(move |ctx| {
        exec_clone(ctx, cross, CloneMask::Replace, CloneForce::Normal, false)
    });
    node.then(clone_mask(cross, CloneMask::Replace, "replace"))
        .then(clone_mask(cross, CloneMask::Masked, "masked"))
        .then(clone_force(
            cross,
            CloneMask::Replace,
            CloneForce::Force,
            "force",
        ))
        .then(clone_force(
            cross,
            CloneMask::Replace,
            CloneForce::Normal,
            "normal",
        ))
        .then(clone_filtered(
            cross,
            CloneMask::Replace,
            CloneForce::Normal,
        ))
}

fn clone_node() -> Node {
    let short = tree::Builder::arg("begin", ArgKind::BlockPos)
        .then(tree::Builder::arg("end", ArgKind::BlockPos).then(clone_destination(false)));

    let cross = tree::Builder::literal("from").then(
        tree::Builder::arg("from", ArgKind::ResourceLocation).then(
            tree::Builder::arg("begin", ArgKind::BlockPos).then(
                tree::Builder::arg("end", ArgKind::BlockPos).then(
                    tree::Builder::literal("to").then(
                        tree::Builder::arg("to", ArgKind::ResourceLocation)
                            .then(clone_destination(true)),
                    ),
                ),
            ),
        ),
    );

    tree::Builder::literal("clone")
        .requires(gate(NODE_CLONE))
        .usage("<begin> <end> <destination> [replace|masked] [force|normal] [filtered <filter>]")
        .executable(|ctx| {
            ctx.fail(
                "/clone <begin> <end> <destination> [replace|masked] [force|normal] \
                 [filtered <filter>]",
            )
        })
        .then(short)
        .then(cross)
        .build()
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn coordinate(ctx: &Ctx, index: usize) -> Result<Coordinates, String> {
    match ctx.args.get(index).map(|a| &a.value) {
        Some(Value::Coordinates(c)) => Ok(*c),
        _ => Err("Expected a block position".into()),
    }
}

fn resolve_block(ctx: &Ctx, index: usize) -> Result<BlockPos, String> {
    let coords = coordinate(ctx, index)?;
    let resolved = match ctx.player() {
        Some(handle) => coords.resolve_block(handle as &dyn CoordinateSource),
        None => coords.resolve_block(&NoSource),
    };
    resolved.map_err(|e| e.message())
}

fn block(ctx: &Ctx, index: usize) -> Result<BlockState, String> {
    match ctx.args.get(index).map(|a| &a.value) {
        Some(Value::BlockState(b)) => Ok(b.clone()),
        _ => Err("Expected a block state".into()),
    }
}

fn text(ctx: &Ctx, index: usize) -> Result<String, String> {
    ctx.args
        .get(index)
        .map(|a| a.text.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Expected an argument".into())
}

fn resource(ctx: &Ctx, index: usize) -> Result<String, String> {
    match ctx.args.get(index).map(|a| &a.value) {
        Some(Value::ResourceLocation(id)) => Ok(id.clone()),
        _ => Err("Expected a dimension".into()),
    }
}

fn actor(ctx: &Ctx) -> JournalActor {
    match ctx.player() {
        Some(handle) => JournalActor(handle.uuid),
        None => JournalActor(0),
    }
}

fn in_world_y(y: i32) -> bool {
    (WORLD_BOTTOM..WORLD_TOP).contains(&y)
}

fn check_y(y: i32) -> Result<(), String> {
    if in_world_y(y) {
        Ok(())
    } else {
        Err(ERR_OUTSIDE.into())
    }
}

// ---------------------------------------------------------------------------
// Boxes and filters
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Box3 {
    min: (i32, i32, i32),
    max: (i32, i32, i32),
}

impl Box3 {
    fn between(a: BlockPos, b: BlockPos) -> Box3 {
        Box3 {
            min: (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: (a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    fn volume(self) -> u128 {
        let dx = (self.max.0 as i64 - self.min.0 as i64 + 1) as u128;
        let dy = (self.max.1 as i64 - self.min.1 as i64 + 1) as u128;
        let dz = (self.max.2 as i64 - self.min.2 as i64 + 1) as u128;
        dx * dy * dz
    }

    fn check_world(self) -> Result<(), String> {
        check_y(self.min.1)?;
        check_y(self.max.1)
    }

    fn overlaps(self, other: Box3) -> bool {
        self.min.0 <= other.max.0
            && self.max.0 >= other.min.0
            && self.min.1 <= other.max.1
            && self.max.1 >= other.min.1
            && self.min.2 <= other.max.2
            && self.max.2 >= other.min.2
    }

    /// The same size as `self`, with its minimum moved to `min`.
    fn moved_to(self, min: BlockPos) -> Result<Box3, String> {
        let dx = self.max.0 as i64 - self.min.0 as i64;
        let dy = self.max.1 as i64 - self.min.1 as i64;
        let dz = self.max.2 as i64 - self.min.2 as i64;
        let max_x = min.x as i64 + dx;
        let max_y = min.y as i64 + dy;
        let max_z = min.z as i64 + dz;
        if max_x > i32::MAX as i64
            || max_y > i32::MAX as i64
            || max_z > i32::MAX as i64
            || max_x < i32::MIN as i64
            || max_y < i32::MIN as i64
            || max_z < i32::MIN as i64
        {
            return Err(ERR_OUTSIDE.into());
        }
        Ok(Box3 {
            min: (min.x, min.y, min.z),
            max: (max_x as i32, max_y as i32, max_z as i32),
        })
    }

    fn is_edge(self, x: i32, y: i32, z: i32) -> bool {
        x == self.min.0
            || x == self.max.0
            || y == self.min.1
            || y == self.max.1
            || z == self.min.2
            || z == self.max.2
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FilterKind {
    /// A block type with no properties, or one exact state when properties
    /// were written.
    Block {
        block: u16,
        state: Option<BlockStateId>,
    },
    /// A block tag, resolved once at parse time.
    Tag {
        blocks: Vec<u16>,
        props: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockFilter {
    kind: FilterKind,
}

impl BlockFilter {
    fn matches(&self, state: BlockStateId) -> bool {
        let Some((block, _)) = blocks::block_of_state(state) else {
            return false;
        };
        match &self.kind {
            FilterKind::Block {
                state: Some(want_state),
                ..
            } => state == *want_state,
            FilterKind::Block {
                block: want,
                state: None,
            } => block == *want,
            FilterKind::Tag {
                blocks: members,
                props: wanted,
            } => {
                if !members.contains(&block) {
                    return false;
                }
                wanted.iter().all(|(key, value)| {
                    prop_value(state, key).is_some_and(|actual| actual == value)
                })
            }
        }
    }
}

fn prop_value(state: BlockStateId, key: &str) -> Option<&'static str> {
    let (block, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[block as usize];
    props::values_of(block, state.raw().saturating_sub(lo))
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn normalize_resource(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn invalid_filter(text: &str) -> String {
    format!("Invalid block predicate '{text}'")
}

fn split_properties(text: &str) -> (&str, Option<&str>) {
    match text.split_once('[') {
        Some((head, rest)) => (head, rest.strip_suffix(']')),
        None => (text, None),
    }
}

fn parse_properties(text: &str, list: &str) -> Result<Vec<(String, String)>, String> {
    if list.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for pair in list.split(',') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("Expected value for property in block state '{text}'"))?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(format!(
                "Expected value for property in block state '{text}'"
            ));
        }
        if out.iter().any(|(k, _): &(String, String)| k == key) {
            return Err(format!(
                "Property '{key}' is already defined for block state '{text}'"
            ));
        }
        out.push((key.to_string(), value.to_string()));
    }
    Ok(out)
}

/// Parse a vanilla block predicate: `stone`, `oak_stairs[facing=east]`,
/// `#minecraft:logs` or `#minecraft:logs[axis=x]`.
fn parse_filter(raw: &str) -> Result<BlockFilter, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid_filter(raw));
    }
    if let Some(tag) = raw.strip_prefix('#') {
        let (head, prop_list) = split_properties(tag);
        let id = normalize_resource(head);
        if id == "minecraft:" || !valid_resource(&id) {
            return Err(format!("Unknown block tag '{raw}'"));
        }
        let props = match prop_list {
            Some(list) => parse_properties(raw, list)?,
            None => Vec::new(),
        };
        let members = tag_members(&id);
        if members.is_empty() && !known_builtin_tag(&id) {
            return Err(format!("Unknown block tag '{raw}'"));
        }
        return Ok(BlockFilter {
            kind: FilterKind::Tag {
                blocks: members,
                props,
            },
        });
    }

    let (head, prop_list) = split_properties(raw);
    let id = normalize_resource(head);
    let block = blocks::block_id_of(&id).ok_or_else(|| format!("Unknown block type '{raw}'"))?;
    let state = match prop_list {
        Some(list) => {
            let pairs = parse_properties(raw, list)?;
            let borrowed: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let state = props::state_with(block, &borrowed)
                .ok_or_else(|| format!("Unknown property or value in block state '{raw}'"))?;
            Some(BlockStateId(state))
        }
        None => None,
    };
    Ok(BlockFilter {
        kind: FilterKind::Block { block, state },
    })
}

fn valid_resource(name: &str) -> bool {
    let Some((ns, path)) = name.split_once(':') else {
        return false;
    };
    !ns.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
        && !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-' | '/')
        })
}

/// The pack's block-tag members, when a data pack is configured and ships the
/// tag; otherwise the built-in name rules below.
fn tag_members(id: &str) -> Vec<u16> {
    if let Some(tags) = pack_tags() {
        let list = tags.block_tag_list(id);
        if !list.is_empty() {
            return list;
        }
    }
    let short = id.strip_prefix("minecraft:").unwrap_or(id);
    blocks::BLOCKS
        .iter()
        .enumerate()
        .filter(|(_, row)| builtin_tag_matches(short, row.0))
        .map(|(i, _)| i as u16)
        .collect()
}

fn pack_tags() -> Option<&'static aether_worldgen::vanilla::tags::Tags> {
    use aether_worldgen::vanilla::density::DataPack;
    use aether_worldgen::vanilla::tags::Tags;
    static TAGS: std::sync::OnceLock<Option<Tags>> = std::sync::OnceLock::new();
    TAGS.get_or_init(|| {
        let root = crate::worldgen_data();
        if root.is_empty() {
            return None;
        }
        DataPack::open(root).ok().map(|pack| Tags::new(&pack))
    })
    .as_ref()
}

fn known_builtin_tag(id: &str) -> bool {
    const NAMES: &[&str] = &[
        "air",
        "base_stone_overworld",
        "concrete",
        "concrete_powder",
        "crops",
        "dirt",
        "flowers",
        "glass",
        "leaves",
        "logs",
        "planks",
        "replaceable",
        "sand",
        "saplings",
        "stone_bricks",
        "terracotta",
        "wool",
    ];
    let short = id.strip_prefix("minecraft:").unwrap_or(id);
    NAMES.contains(&short)
}

fn builtin_tag_matches(tag: &str, name: &str) -> bool {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    match tag {
        "air" => matches!(short, "air" | "cave_air" | "void_air"),
        "logs" => {
            short.ends_with("_log")
                || short.ends_with("_wood")
                || short.ends_with("_stem")
                || short.ends_with("_hyphae")
                || matches!(short, "bamboo_block" | "stripped_bamboo_block")
        }
        "planks" => short.ends_with("_planks"),
        "leaves" => short.ends_with("_leaves"),
        "saplings" => short.ends_with("_sapling"),
        "wool" => short.ends_with("_wool"),
        "concrete" => short.ends_with("_concrete"),
        "concrete_powder" => short.ends_with("_concrete_powder"),
        "terracotta" => short.ends_with("_terracotta"),
        "glass" => short.ends_with("_glass"),
        "stone_bricks" => short.ends_with("_bricks") || short.ends_with("_brick"),
        "sand" => matches!(short, "sand" | "red_sand"),
        "dirt" => matches!(
            short,
            "dirt" | "coarse_dirt" | "rooted_dirt" | "grass_block" | "podzol" | "mycelium"
        ),
        "base_stone_overworld" => matches!(
            short,
            "stone" | "granite" | "diorite" | "andesite" | "deepslate" | "tuff"
        ),
        "flowers" => {
            short.ends_with("_tulip")
                || matches!(
                    short,
                    "dandelion"
                        | "poppy"
                        | "blue_orchid"
                        | "allium"
                        | "azure_bluet"
                        | "oxeye_daisy"
                        | "cornflower"
                        | "lily_of_the_valley"
                        | "wither_rose"
                        | "torchflower"
                )
        }
        "crops" => matches!(
            short,
            "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "sweet_berry_bush"
                | "melon_stem"
                | "pumpkin_stem"
        ),
        "replaceable" => blocks::block_id_of(name).is_some_and(|b| {
            crate::placement::rules::is_replaceable(BlockStateId(blocks::DEFAULT_STATE[b as usize]))
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Region check
// ---------------------------------------------------------------------------

/// Ask the core region index about every cell the command will write.
///
/// The console/RCON has no player subject and is an owner, so it skips the
/// check; a player goes through the same `Action::Place` gate as a hand
/// placement, including the `aether.region.bypass`/per-world bypass nodes.
fn check_region(ctx: &Ctx, cells: impl Iterator<Item = (i32, i32, i32)>) -> Result<(), String> {
    let Some(handle) = ctx.player() else {
        return Ok(());
    };
    let store = crate::region::store();
    let index = store.snapshot();
    if index.is_empty() {
        return Ok(());
    }
    let world = handle.world();
    let actor = crate::region::actor_in(handle, world);
    for (x, y, z) in cells {
        if index.check(
            world,
            crate::region::BlockPos::new(x, y, z),
            crate::region::Action::Place,
            &actor,
        ) == crate::region::Verdict::Deny
        {
            return Err(store.deny_message().to_string());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commit and drops
// ---------------------------------------------------------------------------

struct Committed {
    changed: usize,
    undo_id: u64,
}

fn commit(world: &DemoWorld, edit: BulkEdit) -> Result<Committed, String> {
    let commit = edit.commit_with_changes(world).map_err(|e| e.to_string())?;
    if !commit.changes.is_empty() {
        bulk::enqueue_publish(commit.actor, commit.changes);
    }
    Ok(Committed {
        changed: commit.result.changed,
        undo_id: commit.result.undo_id,
    })
}

/// Drop what a destroyed block would have dropped, through the same loot
/// tables a hand break uses. Each stack is minted in the ground journal with
/// the command's revision as its cause, so the destruction is traceable.
fn drop_destroyed(
    world: &DemoWorld,
    actor: JournalActor,
    cause: Option<u64>,
    destroyed: &[(i32, i32, i32, BlockStateId)],
) {
    for &(x, y, z, state) in destroyed {
        if state == BlockStateId::AIR {
            continue;
        }
        let Some(name) = crate::game::block_name(world, state) else {
            continue;
        };
        // A command destroys with the right tool for the block: vanilla's
        // loot table asks whether the breaker can harvest it, and a command
        // has no held item to answer that with. A block that needs a pickaxe
        // still drops as if one were used.
        let tool = tables::block_info(&name)
            .and_then(|info| info.tools.first().copied())
            .map(str::to_owned);
        let loot = match tables::block_state_context(state) {
            Some((block, properties)) => crate::game::loot::LootContext {
                tool,
                block: Some((block, properties)),
                ..Default::default()
            },
            None => crate::game::loot::LootContext {
                tool,
                ..Default::default()
            },
        };
        let drops = entities::with_rng(|rng| tables::block_drops_ctx(&name, &loot, rng));
        for (item, count) in drops {
            let max = tables::max_stack(&item).max(1) as u32;
            let mut left = count as u32;
            while left > 0 {
                let n = left.min(max) as u8;
                left -= n as u32;
                let stack = Stack::named(&item, n);
                let (dx, dz) =
                    entities::with_rng(|rng| (rng.f64() * 0.5 - 0.25, rng.f64() * 0.5 - 0.25));
                let at = Vector3::new(x as f64 + 0.5 + dx, y as f64 + 0.25, z as f64 + 0.5 + dz);
                crate::game::provenance::mint_on_ground(
                    world,
                    actor,
                    &stack,
                    crate::game::provenance::ground_place(at.x, at.y, at.z),
                    cause,
                );
                entities::spawn_item(at, stack, Vector3::new(0.0, 0.1, 0.0), 10, None);
            }
        }
    }
}

fn block_name(world: &DemoWorld, state: BlockStateId) -> String {
    world
        .block_name_of(state)
        .unwrap_or_else(|| format!("block#{}", state.0))
}

// ---------------------------------------------------------------------------
// /setblock
// ---------------------------------------------------------------------------

fn setblock(ctx: &Ctx, mode: SetMode) -> Result<String, String> {
    let pos = resolve_block(ctx, 0)?;
    check_y(pos.y)?;
    let block = block(ctx, 1)?;
    let world = ctx.world;
    let old = world.get_block(pos.x, pos.y, pos.z);

    if mode == SetMode::Keep && old != BlockStateId::AIR {
        return Err(format!("That block is already {}", block_name(world, old)));
    }

    check_region(ctx, std::iter::once((pos.x, pos.y, pos.z)))?;

    let actor = actor(ctx);
    let mut edit = BulkEdit::with_limit(actor, MAX_VOLUME as usize);
    edit.set(world, pos.x, pos.y, pos.z, block.id)
        .map_err(|e| e.to_string())?;
    let committed = commit(world, edit)?;

    if mode == SetMode::Destroy {
        let cause = (committed.undo_id != 0).then_some(committed.undo_id);
        drop_destroyed(world, actor, cause, &[(pos.x, pos.y, pos.z, old)]);
    }

    Ok(format!(
        "Successfully set block at {}, {}, {} to {}",
        pos.x,
        pos.y,
        pos.z,
        block_name(world, block.id)
    ))
}

// ---------------------------------------------------------------------------
// /fill
// ---------------------------------------------------------------------------

fn fill(ctx: &Ctx, mode: FillMode) -> Result<String, String> {
    let from = resolve_block(ctx, 0)?;
    let to = resolve_block(ctx, 1)?;
    let (filter, target) = if mode == FillMode::Filtered {
        (Some(parse_filter(&text(ctx, 2)?)?), block(ctx, 3)?)
    } else {
        (None, block(ctx, 2)?)
    };

    let area = Box3::between(from, to);
    area.check_world()?;
    let volume = area.volume();
    if volume > MAX_VOLUME {
        return Err(ERR_TOO_MANY.into());
    }

    let world = ctx.world;
    let mut writes: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();
    let mut destroyed: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();

    for x in area.min.0..=area.max.0 {
        for y in area.min.1..=area.max.1 {
            for z in area.min.2..=area.max.2 {
                let old = world.get_block(x, y, z);
                let next = match mode {
                    FillMode::Replace => Some(target.id),
                    FillMode::Destroy => {
                        if old != BlockStateId::AIR {
                            destroyed.push((x, y, z, old));
                        }
                        Some(target.id)
                    }
                    FillMode::Keep => (old == BlockStateId::AIR).then_some(target.id),
                    FillMode::Hollow => Some(if area.is_edge(x, y, z) {
                        target.id
                    } else {
                        BlockStateId::AIR
                    }),
                    FillMode::Outline => area.is_edge(x, y, z).then_some(target.id),
                    FillMode::Filtered => filter
                        .as_ref()
                        .filter(|f| f.matches(old))
                        .map(|_| target.id),
                };
                if let Some(next) = next {
                    if next != old {
                        writes.push((x, y, z, next));
                    }
                }
            }
        }
    }

    check_region(ctx, writes.iter().map(|&(x, y, z, _)| (x, y, z)))?;

    let actor = actor(ctx);
    let mut edit = BulkEdit::with_limit(actor, MAX_VOLUME as usize);
    for &(x, y, z, next) in &writes {
        edit.set(world, x, y, z, next).map_err(|e| e.to_string())?;
    }
    let committed = commit(world, edit)?;

    if mode == FillMode::Destroy {
        let cause = (committed.undo_id != 0).then_some(committed.undo_id);
        drop_destroyed(world, actor, cause, &destroyed);
    }

    Ok(format!("Successfully filled {} blocks", committed.changed))
}

// ---------------------------------------------------------------------------
// /clone
// ---------------------------------------------------------------------------

fn clone(
    ctx: &Ctx,
    cross: bool,
    mask: CloneMask,
    force: CloneForce,
    filtered: bool,
) -> Result<String, String> {
    let (begin_i, end_i, dest_i, filter_i) = if cross {
        let from = resource(ctx, 0)?;
        let to = resource(ctx, 3)?;
        if from != to {
            return Err("Cannot clone between dimensions".into());
        }
        (1, 2, 4, 5)
    } else {
        (0, 1, 2, 3)
    };
    let begin = resolve_block(ctx, begin_i)?;
    let end = resolve_block(ctx, end_i)?;
    let destination = resolve_block(ctx, dest_i)?;

    let source = Box3::between(begin, end);
    source.check_world()?;
    let volume = source.volume();
    if volume > MAX_VOLUME {
        return Err(ERR_TOO_MANY.into());
    }
    let destination_box = source.moved_to(destination)?;
    destination_box.check_world()?;
    if source.overlaps(destination_box) {
        return Err(ERR_OVERLAP.into());
    }

    let filter = if filtered {
        Some(parse_filter(&text(ctx, filter_i)?)?)
    } else {
        None
    };

    let world = ctx.world;

    // Snapshot the source before any write: the command must copy what was
    // there when it ran, not a half-applied mixture of old and new.
    let mut sources: Vec<(i32, i32, i32, BlockStateId)> = Vec::with_capacity(volume as usize);
    for x in source.min.0..=source.max.0 {
        for y in source.min.1..=source.max.1 {
            for z in source.min.2..=source.max.2 {
                sources.push((x, y, z, world.get_block(x, y, z)));
            }
        }
    }

    let mut writes: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();
    for &(sx, sy, sz, state) in &sources {
        if mask == CloneMask::Masked && state == BlockStateId::AIR {
            continue;
        }
        if let Some(filter) = &filter {
            if !filter.matches(state) {
                continue;
            }
        }
        let x = destination.x + (sx - source.min.0);
        let y = destination.y + (sy - source.min.1);
        let z = destination.z + (sz - source.min.2);
        let old = world.get_block(x, y, z);
        if force == CloneForce::Normal && old != BlockStateId::AIR {
            return Err(ERR_CLONE_FAILED.into());
        }
        if old != state {
            writes.push((x, y, z, state));
        }
    }

    check_region(ctx, writes.iter().map(|&(x, y, z, _)| (x, y, z)))?;

    let actor = actor(ctx);
    let mut edit = BulkEdit::with_limit(actor, MAX_VOLUME as usize);
    for &(x, y, z, next) in &writes {
        edit.set(world, x, y, z, next).map_err(|e| e.to_string())?;
    }
    let committed = commit(world, edit)?;

    Ok(format!("Successfully cloned {} blocks", committed.changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tree::{run_command, ServerKind, Source};
    use crate::players::{PlayerHandle, Registry, SharedRegistry};
    use aether_api::{block_ids, FjallStore, World};
    use aether_world::journal::EventBody;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// A throwaway persistent world on a temp store, unique per call.
    fn test_world() -> DemoWorld {
        static N: AtomicU64 = AtomicU64::new(0);
        let tag = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aether-world-edit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", tag).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, tag, revision, &cfg);
        World::new(store, generator)
    }

    fn registry() -> SharedRegistry {
        Arc::new(Registry::default())
    }

    fn run(world: &DemoWorld, line: &str) -> Vec<String> {
        run_command(
            line,
            Source::Server(ServerKind::Console),
            &registry(),
            world,
        )
        .0
    }

    fn run_player(
        world: &DemoWorld,
        registry: &SharedRegistry,
        handle: &PlayerHandle,
        line: &str,
    ) -> Vec<String> {
        run_command(line, Source::Player(handle), registry, world).0
    }

    fn air() -> BlockStateId {
        BlockStateId::AIR
    }

    fn block(name: &str) -> BlockStateId {
        blocks::default_state(name).unwrap()
    }

    fn block_groups(world: &DemoWorld) -> Vec<aether_world::journal::Event> {
        world
            .journal()
            .recent_events(50)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.body, EventBody::BlockGroup { .. }))
            .collect()
    }

    #[test]
    fn setblock_writes_through_the_bulk_journal() {
        let world = test_world();
        let head = world.journal().head();
        let out = run(&world, "setblock 0 200 0 minecraft:stone");
        assert_eq!(
            out,
            vec!["Successfully set block at 0, 200, 0 to minecraft:stone"]
        );
        assert_eq!(world.get_block(0, 200, 0), block_ids::STONE);
        assert_eq!(world.journal().head(), head + 1, "one journal revision");
        assert_eq!(block_groups(&world).len(), 1);
    }

    #[test]
    fn setblock_keep_on_an_occupied_cell_changes_nothing() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 0 200 0 minecraft:stone"),
            vec!["Successfully set block at 0, 200, 0 to minecraft:stone"]
        );
        let head = world.journal().head();
        let out = run(&world, "setblock 0 200 0 minecraft:dirt keep");
        assert_eq!(out, vec!["That block is already minecraft:stone"]);
        assert_eq!(world.get_block(0, 200, 0), block_ids::STONE);
        assert_eq!(world.journal().head(), head, "no change, no journal");
    }

    #[test]
    fn setblock_destroy_drops_the_old_block() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 0 200 0 minecraft:stone"),
            vec!["Successfully set block at 0, 200, 0 to minecraft:stone"]
        );
        let out = run(&world, "setblock 0 200 0 minecraft:dirt destroy");
        assert_eq!(
            out,
            vec!["Successfully set block at 0, 200, 0 to minecraft:dirt"]
        );
        assert_eq!(world.get_block(0, 200, 0), block_ids::DIRT);
        let events = world.journal().recent_events(20).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e.body, EventBody::ItemMint { .. })),
            "destroy must mint the drop: {events:?}"
        );
    }

    #[test]
    fn setblock_refuses_coordinates_outside_the_world() {
        let world = test_world();
        let head = world.journal().head();
        let out = run(&world, "setblock 0 400 0 minecraft:stone");
        assert_eq!(out, vec!["That position is outside of the world"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn fill_normalises_corners_and_reports_the_block_count() {
        let world = test_world();
        let out = run(&world, "fill 2 200 2 0 200 0 minecraft:stone");
        assert_eq!(out, vec!["Successfully filled 9 blocks"]);
        for x in 0..=2 {
            for z in 0..=2 {
                assert_eq!(world.get_block(x, 200, z), block_ids::STONE);
            }
        }
    }

    #[test]
    fn fill_refuses_a_volume_over_32768_before_writing() {
        let world = test_world();
        let head = world.journal().head();
        // 33 * 32 * 32 = 33792 blocks.
        let out = run(&world, "fill 0 200 0 32 231 31 minecraft:stone");
        assert_eq!(out, vec!["Too many blocks in the specified area"]);
        assert_eq!(world.journal().head(), head);
        assert_eq!(world.get_block(0, 200, 0), air());
    }

    #[test]
    fn hollow_clears_the_interior_and_outline_leaves_it_alone() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:stone"),
            vec!["Successfully filled 27 blocks"]
        );
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:dirt hollow"),
            vec!["Successfully filled 27 blocks"]
        );
        assert_eq!(world.get_block(11, 201, 11), air(), "hollow interior");
        assert_eq!(world.get_block(10, 200, 10), block_ids::DIRT);
        assert_eq!(world.get_block(11, 202, 11), block_ids::DIRT);

        // Restore the solid cube, then outline only its shell.
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:stone"),
            vec!["Successfully filled 27 blocks"]
        );
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:dirt outline"),
            vec!["Successfully filled 26 blocks"]
        );
        assert_eq!(world.get_block(11, 201, 11), block_ids::STONE);
        assert_eq!(world.get_block(10, 200, 10), block_ids::DIRT);
    }

    #[test]
    fn fill_replace_filters_only_matching_blocks() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 20 200 20 22 200 22 minecraft:stone"),
            vec!["Successfully filled 9 blocks"]
        );
        assert_eq!(
            run(&world, "setblock 21 200 21 minecraft:dirt"),
            vec!["Successfully set block at 21, 200, 21 to minecraft:dirt"]
        );
        let out = run(
            &world,
            "fill 20 200 20 22 200 22 replace minecraft:dirt minecraft:gold_block",
        );
        assert_eq!(out, vec!["Successfully filled 1 blocks"]);
        assert_eq!(world.get_block(21, 200, 21), block("minecraft:gold_block"));
        assert_eq!(world.get_block(20, 200, 20), block_ids::STONE);
    }

    #[test]
    fn fill_replace_accepts_a_block_tag_filter() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 30 200 30 31 200 31 minecraft:oak_log"),
            vec!["Successfully filled 4 blocks"]
        );
        assert_eq!(
            run(&world, "setblock 30 200 30 minecraft:stone"),
            vec!["Successfully set block at 30, 200, 30 to minecraft:stone"]
        );
        let out = run(
            &world,
            "fill 30 200 30 31 200 31 replace #minecraft:logs minecraft:dirt",
        );
        assert_eq!(out, vec!["Successfully filled 3 blocks"]);
        assert_eq!(world.get_block(30, 200, 30), block_ids::STONE);
        assert_eq!(world.get_block(31, 200, 31), block_ids::DIRT);
    }

    #[test]
    fn clone_masked_copies_only_non_air() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 40 200 40 41 201 41 minecraft:stone"),
            vec!["Successfully filled 8 blocks"]
        );
        assert_eq!(
            run(&world, "setblock 40 200 40 minecraft:air"),
            vec!["Successfully set block at 40, 200, 40 to minecraft:air"]
        );
        assert_eq!(
            run(&world, "clone 40 200 40 41 201 41 50 200 50 masked"),
            vec!["Successfully cloned 7 blocks"]
        );
        assert_eq!(world.get_block(50, 200, 50), air());
        assert_eq!(world.get_block(51, 201, 51), block_ids::STONE);
    }

    #[test]
    fn clone_normal_refuses_a_non_air_destination_but_force_overwrites() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 60 200 60 61 200 61 minecraft:stone"),
            vec!["Successfully filled 4 blocks"]
        );
        assert_eq!(
            run(&world, "setblock 70 200 70 minecraft:dirt"),
            vec!["Successfully set block at 70, 200, 70 to minecraft:dirt"]
        );
        let head = world.journal().head();
        let out = run(&world, "clone 60 200 60 61 200 61 70 200 70");
        assert_eq!(out, vec!["Could not clone blocks"]);
        assert_eq!(world.journal().head(), head, "no partial clone");
        assert_eq!(world.get_block(71, 200, 71), air());

        let out = run(&world, "clone 60 200 60 61 200 61 70 200 70 force");
        assert_eq!(out, vec!["Successfully cloned 4 blocks"]);
        assert_eq!(world.get_block(70, 200, 70), block_ids::STONE);
        assert_eq!(world.get_block(71, 200, 71), block_ids::STONE);
    }

    #[test]
    fn clone_filtered_copies_only_matching_source_blocks() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 80 200 80 81 200 81 minecraft:stone"),
            vec!["Successfully filled 4 blocks"]
        );
        assert_eq!(
            run(&world, "setblock 80 200 80 minecraft:dirt"),
            vec!["Successfully set block at 80, 200, 80 to minecraft:dirt"]
        );
        let out = run(
            &world,
            "clone 80 200 80 81 200 81 90 200 90 filtered minecraft:dirt",
        );
        assert_eq!(out, vec!["Successfully cloned 1 blocks"]);
        assert_eq!(world.get_block(90, 200, 90), block_ids::DIRT);
        assert_eq!(world.get_block(91, 200, 91), air());
    }

    #[test]
    fn clone_into_itself_is_refused_without_a_journal_entry() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 100 200 100 101 201 101 minecraft:stone"),
            vec!["Successfully filled 8 blocks"]
        );
        let head = world.journal().head();
        let out = run(&world, "clone 100 200 100 101 201 101 100 200 100");
        assert_eq!(out, vec!["The source and destination can't overlap"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn clone_refuses_a_volume_over_32768_before_snapshotting() {
        let world = test_world();
        let head = world.journal().head();
        let out = run(&world, "clone 0 200 0 32 231 31 40 200 40");
        assert_eq!(out, vec!["Too many blocks in the specified area"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn the_player_is_the_journal_actor_and_level_zero_cannot_run_it() {
        let _guard = crate::permissions::test_lock();
        crate::permissions::install_store_for_test(crate::permissions::Store::new());
        crate::permissions::seed_operators(&["Alice".to_string()]);

        let world = test_world();
        let registry = registry();
        let alice = PlayerHandle::for_test(1, "Alice");
        let bob = PlayerHandle::for_test(2, "Bob");
        registry.insert_for_test(Arc::clone(&alice));
        registry.insert_for_test(Arc::clone(&bob));

        let out = run_player(
            &world,
            &registry,
            &alice,
            "setblock 0 200 0 minecraft:stone",
        );
        assert_eq!(
            out,
            vec!["Successfully set block at 0, 200, 0 to minecraft:stone"]
        );
        let events = world.journal().recent_events(5).unwrap();
        let group = events
            .iter()
            .find(|e| matches!(e.body, EventBody::BlockGroup { .. }))
            .expect("a block group");
        assert_eq!(group.actor, JournalActor(alice.uuid));

        let head = world.journal().head();
        let denied = run_player(&world, &registry, &bob, "setblock 1 200 0 minecraft:stone");
        assert_eq!(
            denied,
            vec!["You do not have permission to run this command."]
        );
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn a_protected_region_refuses_the_edit_without_a_bypass() {
        let _guard = crate::permissions::test_lock();
        let mut perms = crate::permissions::Store::new();
        perms
            .add_member(crate::permissions::OP_GROUP, "Alice", None)
            .unwrap();
        // An operator, but with both bypass nodes explicitly denied: the
        // region must still refuse the command.
        perms
            .set_player_node("Alice", crate::region::BYPASS_PERMISSION, None, Some(false))
            .unwrap();
        perms
            .set_player_node(
                "Alice",
                &crate::region::worldguard_bypass_permission(crate::region::OVERWORLD),
                None,
                Some(false),
            )
            .unwrap();
        crate::permissions::install_store_for_test(perms);

        let _store = crate::region::install_test_store(crate::region::RegionStore::new());
        crate::region::store().set_deny_message("That area is protected.");
        crate::region::store().upsert(crate::region::Region::new(
            crate::region::RegionId(1),
            crate::region::OVERWORLD,
            crate::region::BlockPos::new(0, 0, 0),
            crate::region::BlockPos::new(10, 300, 10),
        ));

        let world = test_world();
        let registry = registry();
        let alice = PlayerHandle::for_test(1, "Alice");
        registry.insert_for_test(Arc::clone(&alice));

        let head = world.journal().head();
        let out = run_player(
            &world,
            &registry,
            &alice,
            "setblock 5 200 5 minecraft:stone",
        );
        assert_eq!(out, vec!["That area is protected."]);
        assert_eq!(world.get_block(5, 200, 5), air());
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn a_filter_parses_block_states_and_tags() {
        let stone = parse_filter("minecraft:stone").unwrap();
        assert!(stone.matches(block_ids::STONE));
        assert!(!stone.matches(block_ids::DIRT));

        let stairs = parse_filter("oak_stairs[facing=east]").unwrap();
        let east = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:oak_stairs").unwrap(),
                &[("facing", "east")],
            )
            .unwrap(),
        );
        let west = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:oak_stairs").unwrap(),
                &[("facing", "west")],
            )
            .unwrap(),
        );
        assert!(stairs.matches(east));
        assert!(!stairs.matches(west));

        let logs = parse_filter("#minecraft:logs").unwrap();
        assert!(logs.matches(block("minecraft:oak_log")));
        assert!(!logs.matches(block_ids::STONE));

        assert!(parse_filter("not_a_block").is_err());
        assert!(parse_filter("oak_stairs[facing=upside]").is_err());
    }
}
