//! Vanilla world-editing commands: `/setblock`, `/fill` and `/clone` (CMD-12a).
//!
//! Every write goes through the core bulk-edit transaction ([`crate::bulk`]):
//! the whole command is staged first, checked against the region index, and
//! committed as **one** journal revision with the caller as its actor. Nothing
//! reaches the world until every check has passed, so a refused command leaves
//! no blocks and no journal entry behind. Publishing (column resends, shape
//! settle and after-block hooks) is the normal bulk publisher, which is why a
//! client sees the same result as a player placing the blocks by hand. The
//! `strict` modes skip the shape/update pass and only re-send the touched
//! columns, matching vanilla's "place as-is without block updates".
//!
//! The commands follow vanilla 1.21.11's grammar and limits:
//!
//! * `/setblock <pos> <block> [destroy|keep|replace|strict]`
//! * `/fill <from> <to> <block> [destroy|hollow|keep|outline|replace|strict]`
//! * `/fill <from> <to> <block> replace <filter> [destroy|hollow|keep|outline|strict]`
//! * `/clone [from <dimension>] <begin> <end> [to <dimension>] <destination>
//!   [strict] [replace|masked|filtered <filter>] [force|move|normal]`
//!
//! Both `/fill` and `/clone` are capped at 32768 blocks, the cap is checked
//! before a single cell is read, and block positions outside the engine's
//! vertical range are refused. A block filter is a block state (properties
//! optional and matched partially) or a `#tag`, resolved through the pack's
//! block tags when one is configured and through the built-in name rules
//! otherwise. A tag the built-in rules cannot represent is refused rather than
//! silently matching a subset.
//!
//! The region check covers **every cell a command can touch**, not only the
//! cells whose state changes: `/fill … destroy` drops an unchanged block just
//! as it drops a changed one, and a filtered command still reads (and is
//! capable of modifying) its whole destination box.

use std::collections::BTreeSet;

use aether_api::{JournalActor, Vector3};
use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use super::args::BlockState;
use super::coordinates::{CoordinateSource, Coordinates, Rotation};
use super::tree::{self, ArgKind, Ctx, Node, Requirement, Value};
use super::Reply;
use crate::bulk::{self, BulkEdit};
use crate::commands::coordinates::BlockPos;
use crate::game::{containers, entities, tables, window};
use crate::inventory::Stack;
use crate::protocol::ServerEvent;
use crate::session::{DemoWorld, WORLD_BOTTOM, WORLD_TOP};

/// Vanilla's per-command cap for `/fill` and `/clone` (the default
/// `commandModificationBlockLimit`).
pub const MAX_VOLUME: u128 = 32_768;

const NODE_SETBLOCK: &str = "minecraft.command.setblock";
const NODE_FILL: &str = "minecraft.command.fill";
const NODE_CLONE: &str = "minecraft.command.clone";

const ERR_OUTSIDE: &str = "That position is out of this world!";
const ERR_OVERLAP: &str = "The source and destination areas cannot overlap";
const ERR_FILL_FAILED: &str = "No blocks were filled";
const ERR_CLONE_FAILED: &str = "No blocks were cloned";
const ERR_SETBLOCK_FAILED: &str = "Could not set the block";
const ERR_BLOCK_ENTITY: &str = "Block entity data is not supported";

fn too_many(volume: u128) -> String {
    format!(
        "Too many blocks in the specified area (maximum {}, but specified {})",
        MAX_VOLUME, volume
    )
}

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
    Strict,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillOp {
    Replace,
    Destroy,
    Hollow,
    Keep,
    Outline,
    Strict,
}

#[derive(Clone, Copy)]
struct FillArgs {
    op: FillOp,
    filtered: bool,
    strict: bool,
}

impl FillArgs {
    const fn new(op: FillOp) -> FillArgs {
        FillArgs {
            op,
            filtered: false,
            strict: false,
        }
    }

    const fn filtered(op: FillOp) -> FillArgs {
        FillArgs {
            op,
            filtered: true,
            strict: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneMask {
    Replace,
    Masked,
}

/// How an overlapping source/destination is treated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneMode {
    Normal,
    Force,
    Move,
}

/// Where the clone arguments sit after `from`/`to` have been placed in the
/// tree. `None` means the dimension was not written and defaults to the
/// execution dimension.
#[derive(Clone, Copy)]
struct CloneForm {
    source_dim: Option<usize>,
    begin: usize,
    end: usize,
    target_dim: Option<usize>,
    destination: usize,
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
        )
        .then(
            tree::Builder::literal("strict").executable(|ctx| exec_setblock(ctx, SetMode::Strict)),
        );

    tree::Builder::literal("setblock")
        .requires(gate(NODE_SETBLOCK))
        .usage("<pos> <block> [destroy|keep|replace|strict]")
        .executable(|ctx| ctx.fail("/setblock <pos> <block> [destroy|keep|replace|strict]"))
        .then(tree::Builder::arg("pos", ArgKind::BlockPos).then(block))
        .build()
}

fn exec_fill(ctx: &mut Ctx, args: FillArgs) -> Reply {
    match fill(ctx, args) {
        Ok(line) => Reply::one(line),
        Err(e) => ctx.fail(e),
    }
}

/// The modes that may follow a `replace <filter>` predicate.
fn filtered_fill_tail() -> tree::Builder {
    tree::Builder::arg("filter", ArgKind::String)
        .executable(|ctx| exec_fill(ctx, FillArgs::filtered(FillOp::Replace)))
        .then(
            tree::Builder::literal("destroy")
                .executable(|ctx| exec_fill(ctx, FillArgs::filtered(FillOp::Destroy))),
        )
        .then(
            tree::Builder::literal("hollow")
                .executable(|ctx| exec_fill(ctx, FillArgs::filtered(FillOp::Hollow))),
        )
        .then(
            tree::Builder::literal("keep")
                .executable(|ctx| exec_fill(ctx, FillArgs::filtered(FillOp::Keep))),
        )
        .then(
            tree::Builder::literal("outline")
                .executable(|ctx| exec_fill(ctx, FillArgs::filtered(FillOp::Outline))),
        )
        .then(tree::Builder::literal("strict").executable(|ctx| {
            exec_fill(
                ctx,
                FillArgs {
                    op: FillOp::Strict,
                    filtered: true,
                    strict: true,
                },
            )
        }))
}

fn fill_mode(op: FillOp) -> tree::Builder {
    tree::Builder::literal(match op {
        FillOp::Replace => "replace",
        FillOp::Destroy => "destroy",
        FillOp::Hollow => "hollow",
        FillOp::Keep => "keep",
        FillOp::Outline => "outline",
        FillOp::Strict => "strict",
    })
    .executable(move |ctx| exec_fill(ctx, FillArgs::new(op)))
}

fn fill_node() -> Node {
    let replace = fill_mode(FillOp::Replace).then(filtered_fill_tail());

    let block = tree::Builder::arg("block", ArgKind::BlockState)
        .executable(|ctx| exec_fill(ctx, FillArgs::new(FillOp::Replace)))
        .then(fill_mode(FillOp::Destroy))
        .then(fill_mode(FillOp::Hollow))
        .then(fill_mode(FillOp::Keep))
        .then(fill_mode(FillOp::Outline))
        .then(replace)
        .then(tree::Builder::literal("strict").executable(|ctx| {
            exec_fill(
                ctx,
                FillArgs {
                    op: FillOp::Strict,
                    filtered: false,
                    strict: true,
                },
            )
        }));

    tree::Builder::literal("fill")
        .requires(gate(NODE_FILL))
        .usage("<from> <to> <block> [destroy|hollow|keep|outline|replace|strict]")
        .executable(|ctx| {
            ctx.fail(
                "/fill <from> <to> <block> [destroy|hollow|keep|outline|replace|strict] | \
                 <from> <to> <block> replace <filter> [destroy|hollow|keep|outline|strict]",
            )
        })
        .then(
            tree::Builder::arg("from", ArgKind::BlockPos)
                .then(tree::Builder::arg("to", ArgKind::BlockPos).then(block)),
        )
        .build()
}

fn exec_clone(
    ctx: &mut Ctx,
    form: CloneForm,
    mask: CloneMask,
    mode: CloneMode,
    filtered: bool,
    strict: bool,
) -> Reply {
    match clone(ctx, form, mask, mode, filtered, strict) {
        Ok(line) => Reply::one(line),
        Err(e) => ctx.fail(e),
    }
}

fn clone_mode_word(mode: CloneMode) -> &'static str {
    match mode {
        CloneMode::Normal => "normal",
        CloneMode::Force => "force",
        CloneMode::Move => "move",
    }
}

/// A `force`/`move`/`normal` tail.
fn clone_mode_tail(
    form: CloneForm,
    mask: CloneMask,
    mode: CloneMode,
    filtered: bool,
    strict: bool,
) -> tree::Builder {
    tree::Builder::literal(clone_mode_word(mode))
        .executable(move |ctx| exec_clone(ctx, form, mask, mode, filtered, strict))
}

/// Append `force`/`move`/`normal` as siblings of the current node.
fn add_clone_force_tails(
    parent: tree::Builder,
    form: CloneForm,
    mask: CloneMask,
    filtered: bool,
    strict: bool,
) -> tree::Builder {
    parent
        .then(clone_mode_tail(
            form,
            mask,
            CloneMode::Force,
            filtered,
            strict,
        ))
        .then(clone_mode_tail(
            form,
            mask,
            CloneMode::Move,
            filtered,
            strict,
        ))
        .then(clone_mode_tail(
            form,
            mask,
            CloneMode::Normal,
            filtered,
            strict,
        ))
}

fn clone_mask_node(form: CloneForm, mask: CloneMask, strict: bool) -> tree::Builder {
    let word = match mask {
        CloneMask::Replace => "replace",
        CloneMask::Masked => "masked",
    };
    add_clone_force_tails(
        tree::Builder::literal(word)
            .executable(move |ctx| exec_clone(ctx, form, mask, CloneMode::Normal, false, strict)),
        form,
        mask,
        false,
        strict,
    )
}

fn clone_filtered_node(form: CloneForm, strict: bool) -> tree::Builder {
    let filter = tree::Builder::arg("filter", ArgKind::String).executable(move |ctx| {
        exec_clone(
            ctx,
            form,
            CloneMask::Replace,
            CloneMode::Normal,
            true,
            strict,
        )
    });
    tree::Builder::literal("filtered").then(add_clone_force_tails(
        filter,
        form,
        CloneMask::Replace,
        true,
        strict,
    ))
}

fn clone_strict_node(form: CloneForm) -> tree::Builder {
    let node = tree::Builder::literal("strict")
        .executable(move |ctx| {
            exec_clone(
                ctx,
                form,
                CloneMask::Replace,
                CloneMode::Normal,
                false,
                true,
            )
        })
        .then(clone_mask_node(form, CloneMask::Replace, true))
        .then(clone_mask_node(form, CloneMask::Masked, true))
        .then(clone_filtered_node(form, true));
    add_clone_force_tails(node, form, CloneMask::Replace, false, true)
}

/// The destination argument and every mode/filter tail.
fn clone_destination(form: CloneForm) -> tree::Builder {
    let node = tree::Builder::arg("destination", ArgKind::BlockPos)
        .executable(move |ctx| {
            exec_clone(
                ctx,
                form,
                CloneMask::Replace,
                CloneMode::Normal,
                false,
                false,
            )
        })
        .then(clone_strict_node(form))
        .then(clone_mask_node(form, CloneMask::Replace, false))
        .then(clone_mask_node(form, CloneMask::Masked, false))
        .then(clone_filtered_node(form, false));
    add_clone_force_tails(node, form, CloneMask::Replace, false, false)
}

/// `to <dimension> <destination>`: an alternative to the plain destination.
fn clone_to(form: CloneForm) -> tree::Builder {
    let with_to = CloneForm {
        target_dim: Some(form.end + 1),
        destination: form.end + 2,
        ..form
    };
    tree::Builder::literal("to")
        .then(tree::Builder::arg("to", ArgKind::ResourceLocation).then(clone_destination(with_to)))
}

/// The end corner, followed by either a plain destination or `to <dim> …`.
fn clone_end(form: CloneForm) -> tree::Builder {
    tree::Builder::arg("end", ArgKind::BlockPos)
        .then(clone_destination(form))
        .then(clone_to(form))
}

fn clone_node() -> Node {
    let short = CloneForm {
        source_dim: None,
        begin: 0,
        end: 1,
        target_dim: None,
        destination: 2,
    };
    let with_source = CloneForm {
        source_dim: Some(0),
        begin: 1,
        end: 2,
        target_dim: None,
        destination: 3,
    };

    let plain = tree::Builder::arg("begin", ArgKind::BlockPos).then(clone_end(short));

    let from = tree::Builder::literal("from").then(
        tree::Builder::arg("from", ArgKind::ResourceLocation)
            .then(tree::Builder::arg("begin", ArgKind::BlockPos).then(clone_end(with_source))),
    );

    tree::Builder::literal("clone")
        .requires(gate(NODE_CLONE))
        .usage(
            "[from <dimension>] <begin> <end> [to <dimension>] <destination> \
             [strict] [replace|masked|filtered <filter>] [force|move|normal]",
        )
        .executable(|ctx| {
            ctx.fail(
                "/clone [from <dimension>] <begin> <end> [to <dimension>] <destination> \
                 [strict] [replace|masked|filtered <filter>] [force|move|normal]",
            )
        })
        .then(plain)
        .then(from)
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

/// Vanilla's `isAir()`: plain air, cave air and void air are all air.
fn is_air(state: BlockStateId) -> bool {
    state == BlockStateId::AIR
        || blocks::block_of_state(state).is_some_and(|(_, name)| {
            matches!(name.rsplit(':').next(), Some("cave_air") | Some("void_air"))
        })
}

/// The dimension key a clone argument names, validated against the one world
/// the engine serves. `minecraft:overworld` and `overworld` are the same
/// dimension, as in the command's own dimension registry.
fn check_dimension(id: &str) -> Result<(), String> {
    if crate::region::same_world(id, crate::WORLD) {
        Ok(())
    } else {
        Err(format!("Unknown dimension '{id}'"))
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

    fn cells(self) -> impl Iterator<Item = (i32, i32, i32)> {
        (self.min.0..=self.max.0).flat_map(move |x| {
            (self.min.1..=self.max.1)
                .flat_map(move |y| (self.min.2..=self.max.2).map(move |z| (x, y, z)))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FilterKind {
    /// A block type. Written properties are matched partially — every one
    /// must agree, and an unwritten property is a wildcard.
    Block {
        block: u16,
        props: Vec<(String, String)>,
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
            FilterKind::Block { block: want, props } => {
                *want == block
                    && props.iter().all(|(key, value)| {
                        prop_value(state, key).is_some_and(|actual| actual == value)
                    })
            }
            FilterKind::Tag {
                blocks: members,
                props,
            } => {
                if !members.contains(&block) {
                    return false;
                }
                props.iter().all(|(key, value)| {
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
    let mut out: Vec<(String, String)> = Vec::new();
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
        if out.iter().any(|(k, _)| k == key) {
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
        if members.is_empty() {
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
    let props = match prop_list {
        Some(list) => parse_properties(raw, list)?,
        None => Vec::new(),
    };
    // Validate the property names and values once, against the block's own
    // state schema; matching itself stays partial.
    if !props.is_empty() {
        let borrowed: Vec<(&str, &str)> = props
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        props::state_with(block, &borrowed)
            .ok_or_else(|| format!("Unknown property or value in block state '{raw}'"))?;
    }
    Ok(BlockFilter {
        kind: FilterKind::Block { block, props },
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
    if !known_builtin_tag(short) {
        return Vec::new();
    }
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

/// Every tag the built-in fallback knows **exactly**.
///
/// The set is deliberately small: each entry is either a fixed member list or
/// a family rule that matches the whole 1.21.11 tag and nothing outside it. A
/// tag outside this set is refused (`Unknown block tag`) rather than matched
/// approximately, so a filtered command can never touch cells vanilla would
/// leave alone because the built-in table guessed.
fn known_builtin_tag(id: &str) -> bool {
    const NAMES: &[&str] = &[
        "air",
        "banners",
        "base_stone_overworld",
        "beds",
        "buttons",
        "candles",
        "coral_blocks",
        "coral_plants",
        "crops",
        "dirt",
        "doors",
        "fence_gates",
        "fences",
        "flowers",
        "leaves",
        "logs",
        "planks",
        "pressure_plates",
        "rails",
        "replaceable",
        "sand",
        "saplings",
        "signs",
        "slabs",
        "small_flowers",
        "stairs",
        "standing_signs",
        "stone_bricks",
        "terracotta",
        "trapdoors",
        "wall_corals",
        "wall_signs",
        "walls",
        "wooden_buttons",
        "wooden_doors",
        "wooden_fences",
        "wooden_pressure_plates",
        "wooden_slabs",
        "wooden_stairs",
        "wooden_trapdoors",
        "wool",
        "wool_carpets",
    ];
    NAMES.contains(&id)
}

/// The sixteen dye colours, for the tags that are "every coloured variant".
const DYE_COLOURS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

fn is_dye_colour(short: &str, suffix: &str) -> bool {
    short
        .strip_suffix(suffix)
        .is_some_and(|prefix| DYE_COLOURS.contains(&prefix))
}

/// Whether `name` belongs to a wood family, for the `wooden_*` tags.
///
/// The family is the longest matching prefix, so `dark_oak_stairs` is dark oak
/// and `pale_oak_stairs` pale oak. The remainder must be a plain item suffix:
/// bamboo has an extra `bamboo_mosaic` family that is *not* wooden, so a
/// `_mosaic_…` remainder is excluded.
fn is_wood(name: &str) -> bool {
    const WOODS: &[&str] = &[
        "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry", "bamboo",
        "crimson", "warped", "pale_oak",
    ];
    WOODS.iter().any(|wood| {
        name.strip_prefix(wood).is_some_and(|rest| {
            rest.starts_with('_') && !rest.trim_start_matches('_').starts_with("mosaic")
        })
    })
}

/// The vanilla `#minecraft:small_flowers` set.
fn is_small_flower(short: &str) -> bool {
    (short.ends_with("_tulip") && !short.starts_with("potted_"))
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
                | "closed_eyeblossom"
                | "open_eyeblossom"
        )
}

/// The tall two-block flowers in `#minecraft:flowers`.
fn is_tall_flower(short: &str) -> bool {
    matches!(
        short,
        "sunflower" | "lilac" | "rose_bush" | "peony" | "pitcher_plant"
    )
}

fn builtin_tag_matches(tag: &str, name: &str) -> bool {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let suffix = |s: &str| short.strip_suffix(s).is_some();
    // Signs do not include the hanging variants: vanilla's `#signs` is the
    // standing and wall signs only, and `#standing_signs` likewise excludes
    // the ceiling-hung ones.
    let is_sign = suffix("_sign") && !suffix("_hanging_sign");
    let is_wall_sign = suffix("_wall_sign") && !suffix("_wall_hanging_sign");
    match tag {
        "air" => matches!(short, "air" | "cave_air" | "void_air"),
        "logs" => {
            suffix("_log")
                || suffix("_wood")
                || suffix("_stem")
                || suffix("_hyphae")
                || matches!(short, "bamboo_block" | "stripped_bamboo_block")
        }
        "planks" => suffix("_planks"),
        "leaves" => suffix("_leaves"),
        "saplings" => suffix("_sapling"),
        "wool" => suffix("_wool"),
        "terracotta" => short == "terracotta" || suffix("_terracotta"),
        "stone_bricks" => matches!(
            short,
            "stone_bricks"
                | "mossy_stone_bricks"
                | "cracked_stone_bricks"
                | "chiseled_stone_bricks"
        ),
        "sand" => matches!(short, "sand" | "red_sand"),
        "dirt" => matches!(
            short,
            "dirt"
                | "coarse_dirt"
                | "rooted_dirt"
                | "grass_block"
                | "podzol"
                | "mycelium"
                | "moss_block"
                | "mud"
                | "muddy_mangrove_roots"
        ),
        "base_stone_overworld" => matches!(
            short,
            "stone" | "granite" | "diorite" | "andesite" | "deepslate" | "tuff"
        ),
        "flowers" => {
            is_small_flower(short)
                || is_tall_flower(short)
                || matches!(
                    short,
                    "flowering_azalea"
                        | "flowering_azalea_leaves"
                        | "mangrove_propagule"
                        | "cherry_leaves"
                        | "chorus_flower"
                        | "spore_blossom"
                        | "pink_petals"
                        | "wildflowers"
                        | "cactus_flower"
                )
        }
        "small_flowers" => is_small_flower(short),
        "crops" => matches!(
            short,
            "wheat" | "carrots" | "potatoes" | "beetroots" | "torchflower_crop" | "pitcher_crop"
        ),
        "rails" => short == "rail" || suffix("_rail"),
        "banners" => suffix("_banner"),
        "beds" => suffix("_bed"),
        "buttons" => suffix("_button"),
        "candles" => short == "candle" || suffix("_candle"),
        "wool_carpets" => is_dye_colour(short, "_carpet"),
        "doors" => suffix("_door") && !suffix("_trapdoor"),
        "fence_gates" => suffix("_fence_gate"),
        "fences" => suffix("_fence"),
        "pressure_plates" => suffix("_pressure_plate"),
        "signs" => is_sign,
        "standing_signs" => is_sign && !is_wall_sign,
        "wall_signs" => is_wall_sign,
        "slabs" => suffix("_slab"),
        "stairs" => suffix("_stairs"),
        "trapdoors" => suffix("_trapdoor"),
        "walls" => suffix("_wall") && !suffix("_wall_sign") && !suffix("_wall_torch"),
        "coral_blocks" => suffix("_coral_block"),
        "coral_plants" => suffix("_coral_plant"),
        "wall_corals" => suffix("_wall_fan"),
        "wooden_buttons" => suffix("_button") && is_wood(short),
        "wooden_doors" => suffix("_door") && !suffix("_trapdoor") && is_wood(short),
        "wooden_fences" => suffix("_fence") && is_wood(short),
        "wooden_pressure_plates" => suffix("_pressure_plate") && is_wood(short),
        "wooden_slabs" => suffix("_slab") && is_wood(short),
        "wooden_stairs" => suffix("_stairs") && is_wood(short),
        "wooden_trapdoors" => suffix("_trapdoor") && is_wood(short),
        "replaceable" => blocks::block_id_of(name).is_some_and(|b| {
            crate::placement::rules::is_replaceable(BlockStateId(blocks::DEFAULT_STATE[b as usize]))
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Region check
// ---------------------------------------------------------------------------

/// Ask the core region index about every cell a command can touch.
///
/// The console/RCON has no player subject and is an owner, so it skips the
/// check; a player goes through the same `Action::Place` gate as a hand
/// placement, including the `aether.region.bypass`/per-world bypass nodes.
/// The caller passes the *whole* touched box, not only the cells whose state
/// will change: `/fill … destroy` drops unchanged blocks too, and a filtered
/// command must not be able to farm a protected cell that happens to already
/// hold the filter's block.
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

/// Commit the staged edit and publish it.
///
/// The normal path is the CMD-30b publisher: settle derived shapes, re-send
/// the touched columns and fire the after-* hooks over the next ticks. A
/// `strict` edit skips the settle/hooks (vanilla's "place as-is without block
/// updates") and re-sends the touched columns directly.
fn commit(ctx: &Ctx, edit: BulkEdit, strict: bool) -> Result<Committed, String> {
    let commit = edit
        .commit_with_changes(ctx.world)
        .map_err(|e| e.to_string())?;
    if !commit.changes.is_empty() {
        if strict {
            let mut columns = BTreeSet::new();
            for change in &commit.changes {
                columns.insert((change.x >> 4, change.z >> 4));
            }
            for (cx, cz) in columns {
                ctx.registry
                    .broadcast(&ServerEvent::ChunkColumn { cx, cz }, ctx.world);
            }
        } else {
            bulk::enqueue_publish(commit.actor, commit.changes);
        }
    }
    Ok(Committed {
        changed: commit.result.changed,
        undo_id: commit.result.undo_id,
    })
}

fn container_kind(world: &DemoWorld, state: BlockStateId) -> Option<containers::ContainerKind> {
    let name = crate::game::block_name(world, state)?;
    containers::ContainerKind::of_block(&name)
}

/// Drop a destroyed container's contents, exactly as a hand break spills them.
fn spill_container(
    world: &DemoWorld,
    actor: JournalActor,
    x: i32,
    y: i32,
    z: i32,
    state: BlockStateId,
) {
    let Some(kind) = container_kind(world, state) else {
        return;
    };
    window::close_viewers((x, y, z), world);
    let items = containers::remove(world, (x, y, z), kind);
    if !items.is_empty() {
        entities::spill(x, y, z, items, actor);
    }
}

/// Forget a container that a write replaced with a different block, so a later
/// block at the same position cannot resurrect its items.
fn clear_replaced_container(
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
    old: BlockStateId,
    new: BlockStateId,
) {
    let Some(kind) = container_kind(world, old) else {
        return;
    };
    if blocks::block_of_state(old).map(|(b, _)| b) == blocks::block_of_state(new).map(|(b, _)| b) {
        return;
    }
    window::close_viewers((x, y, z), world);
    let _ = containers::remove(world, (x, y, z), kind);
}

/// A block that a command will write, with the state it replaces.
#[derive(Clone, Copy)]
struct Write {
    x: i32,
    y: i32,
    z: i32,
    old: BlockStateId,
    new: BlockStateId,
}

fn stage(world: &DemoWorld, actor: JournalActor, writes: &[Write]) -> Result<BulkEdit, String> {
    let mut edit = BulkEdit::with_limit(actor, MAX_VOLUME as usize);
    for w in writes {
        edit.set(world, w.x, w.y, w.z, w.new)
            .map_err(|e| e.to_string())?;
    }
    Ok(edit)
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
        if is_air(state) {
            continue;
        }
        let Some(name) = crate::game::block_name(world, state) else {
            continue;
        };
        spill_container(world, actor, x, y, z, state);
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

fn require_no_block_entity(block: &BlockState) -> Result<(), String> {
    if block.nbt.is_some() {
        Err(ERR_BLOCK_ENTITY.into())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// /setblock
// ---------------------------------------------------------------------------

fn setblock(ctx: &Ctx, mode: SetMode) -> Result<String, String> {
    let pos = resolve_block(ctx, 0)?;
    check_y(pos.y)?;
    let block = block(ctx, 1)?;
    require_no_block_entity(&block)?;
    let world = ctx.world;
    let old = world.get_block(pos.x, pos.y, pos.z);

    if mode == SetMode::Keep && !is_air(old) {
        return Err(ERR_SETBLOCK_FAILED.into());
    }

    check_region(ctx, std::iter::once((pos.x, pos.y, pos.z)))?;

    let actor = actor(ctx);
    let writes = [Write {
        x: pos.x,
        y: pos.y,
        z: pos.z,
        old,
        new: block.id,
    }];
    let edit = stage(world, actor, &writes)?;
    let committed = commit(ctx, edit, mode == SetMode::Strict)?;

    if mode == SetMode::Destroy {
        let cause = (committed.undo_id != 0).then_some(committed.undo_id);
        drop_destroyed(world, actor, cause, &[(pos.x, pos.y, pos.z, old)]);
    } else {
        clear_replaced_container(world, pos.x, pos.y, pos.z, old, block.id);
    }

    Ok(format!(
        "Changed the block at {}, {}, {}",
        pos.x, pos.y, pos.z
    ))
}

// ---------------------------------------------------------------------------
// /fill
// ---------------------------------------------------------------------------

fn fill(ctx: &Ctx, args: FillArgs) -> Result<String, String> {
    let from = resolve_block(ctx, 0)?;
    let to = resolve_block(ctx, 1)?;
    let target = block(ctx, 2)?;
    require_no_block_entity(&target)?;
    let filter = if args.filtered {
        Some(parse_filter(&text(ctx, 3)?)?)
    } else {
        None
    };

    let area = Box3::between(from, to);
    area.check_world()?;
    let volume = area.volume();
    if volume > MAX_VOLUME {
        return Err(too_many(volume));
    }

    // The whole box, before a cell is read: `destroy` drops unchanged blocks,
    // and a filtered fill must not be able to touch a protected cell that
    // already holds the filter's block.
    check_region(ctx, area.cells())?;

    let world = ctx.world;
    let mut writes: Vec<Write> = Vec::new();
    let mut destroyed: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();

    for x in area.min.0..=area.max.0 {
        for y in area.min.1..=area.max.1 {
            for z in area.min.2..=area.max.2 {
                let old = world.get_block(x, y, z);
                let selected = match &filter {
                    Some(f) => f.matches(old),
                    None => true,
                };
                if !selected {
                    continue;
                }
                let next = match args.op {
                    FillOp::Replace | FillOp::Strict => Some(target.id),
                    FillOp::Destroy => {
                        if !is_air(old) {
                            destroyed.push((x, y, z, old));
                        }
                        Some(target.id)
                    }
                    FillOp::Keep => is_air(old).then_some(target.id),
                    FillOp::Hollow => Some(if area.is_edge(x, y, z) {
                        target.id
                    } else {
                        BlockStateId::AIR
                    }),
                    FillOp::Outline => area.is_edge(x, y, z).then_some(target.id),
                };
                if let Some(next) = next {
                    if next != old {
                        writes.push(Write {
                            x,
                            y,
                            z,
                            old,
                            new: next,
                        });
                    }
                }
            }
        }
    }

    let actor = actor(ctx);
    let edit = stage(world, actor, &writes)?;
    let committed = commit(ctx, edit, args.strict || args.op == FillOp::Strict)?;

    if args.op == FillOp::Destroy {
        let cause = (committed.undo_id != 0).then_some(committed.undo_id);
        drop_destroyed(world, actor, cause, &destroyed);
    } else {
        for w in &writes {
            clear_replaced_container(world, w.x, w.y, w.z, w.old, w.new);
        }
    }

    let changed = if args.op == FillOp::Destroy {
        committed.changed.max(destroyed.len())
    } else {
        committed.changed
    };
    if changed == 0 {
        return Err(ERR_FILL_FAILED.into());
    }
    Ok(format!("Successfully filled {changed} block(s)"))
}

// ---------------------------------------------------------------------------
// /clone
// ---------------------------------------------------------------------------

fn clone(
    ctx: &Ctx,
    form: CloneForm,
    mask: CloneMask,
    mode: CloneMode,
    filtered: bool,
    strict: bool,
) -> Result<String, String> {
    if let Some(i) = form.source_dim {
        check_dimension(&resource(ctx, i)?)?;
    }
    if let Some(i) = form.target_dim {
        check_dimension(&resource(ctx, i)?)?;
    }

    let begin = resolve_block(ctx, form.begin)?;
    let end = resolve_block(ctx, form.end)?;
    let destination = resolve_block(ctx, form.destination)?;

    let source = Box3::between(begin, end);
    source.check_world()?;
    let volume = source.volume();
    if volume > MAX_VOLUME {
        return Err(too_many(volume));
    }
    let destination_box = source.moved_to(destination)?;
    destination_box.check_world()?;
    if mode == CloneMode::Normal && source.overlaps(destination_box) {
        return Err(ERR_OVERLAP.into());
    }

    let filter = if filtered {
        Some(parse_filter(&text(ctx, form.destination + 1)?)?)
    } else {
        None
    };

    // The destination is written; `move` also rewrites the source. Check both
    // before reading a cell.
    check_region(ctx, destination_box.cells())?;
    if mode == CloneMode::Move {
        check_region(ctx, source.cells())?;
    }

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

    // Every selected source block is a destination write, even when the
    // destination already holds that state: `move` clears the source first,
    // and a self-move (or any overlap) must have the destination write
    // override its own clear. BulkEdit keeps the last staged state per
    // position, so appending the destination writes after the clears is what
    // makes the block survive.
    let mut destination_writes: Vec<Write> = Vec::new();
    let mut copied_sources: Vec<(i32, i32, i32, BlockStateId)> = Vec::new();
    for &(sx, sy, sz, state) in &sources {
        if mask == CloneMask::Masked && is_air(state) {
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
        copied_sources.push((sx, sy, sz, state));
        destination_writes.push(Write {
            x,
            y,
            z,
            old,
            new: state,
        });
    }

    if copied_sources.is_empty() {
        return Err(ERR_CLONE_FAILED.into());
    }

    // `move` clears the copied source cells; the destination writes are
    // staged afterwards so an overlapping destination is written, not erased.
    let mut source_clears: Vec<Write> = Vec::new();
    if mode == CloneMode::Move {
        for &(x, y, z, _) in &copied_sources {
            let old = world.get_block(x, y, z);
            if !is_air(old) {
                source_clears.push(Write {
                    x,
                    y,
                    z,
                    old,
                    new: BlockStateId::AIR,
                });
            }
        }
    }
    let mut staged = source_clears.clone();
    staged.extend(destination_writes.iter().copied());

    let actor = actor(ctx);
    let edit = stage(world, actor, &staged)?;
    commit(ctx, edit, strict)?;

    let destination_cells: BTreeSet<(i32, i32, i32)> =
        destination_writes.iter().map(|w| (w.x, w.y, w.z)).collect();
    for w in &destination_writes {
        clear_replaced_container(world, w.x, w.y, w.z, w.old, w.new);
    }
    for w in &source_clears {
        // A cell that is also a destination was overwritten above and keeps
        // its block (and its container record).
        if destination_cells.contains(&(w.x, w.y, w.z)) {
            continue;
        }
        // `/clone` copies block states only, so a moved container's contents
        // cannot follow it. Drop them at the source rather than deleting them.
        spill_container(world, actor, w.x, w.y, w.z, w.old);
    }

    // Vanilla reports the number of source blocks it copied, not the number of
    // destination states that happened to differ; a successful clone that
    // overwrites identical blocks still says how many it cloned.
    Ok(format!(
        "Successfully cloned {} block(s)",
        copied_sources.len()
    ))
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
            .recent_events(100)
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
        assert_eq!(out, vec!["Changed the block at 0, 200, 0"]);
        assert_eq!(world.get_block(0, 200, 0), block_ids::STONE);
        assert_eq!(world.journal().head(), head + 1, "one journal revision");
        assert_eq!(block_groups(&world).len(), 1);
    }

    #[test]
    fn setblock_keep_on_an_occupied_cell_changes_nothing() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 0 200 0 minecraft:stone"),
            vec!["Changed the block at 0, 200, 0"]
        );
        let head = world.journal().head();
        let out = run(&world, "setblock 0 200 0 minecraft:dirt keep");
        assert_eq!(out, vec!["Could not set the block"]);
        assert_eq!(world.get_block(0, 200, 0), block_ids::STONE);
        assert_eq!(world.journal().head(), head, "no change, no journal");
    }

    #[test]
    fn cave_and_void_air_count_as_air_for_keep_and_masked() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 0 201 0 minecraft:cave_air"),
            vec!["Changed the block at 0, 201, 0"]
        );
        assert_eq!(
            run(&world, "setblock 0 201 0 minecraft:stone keep"),
            vec!["Changed the block at 0, 201, 0"]
        );
        assert_eq!(world.get_block(0, 201, 0), block_ids::STONE);

        assert_eq!(
            run(&world, "fill 10 201 0 11 201 0 minecraft:cave_air"),
            vec!["Successfully filled 2 block(s)"]
        );
        assert_eq!(
            run(&world, "fill 10 201 0 11 201 0 minecraft:dirt keep"),
            vec!["Successfully filled 2 block(s)"]
        );
        assert_eq!(world.get_block(10, 201, 0), block_ids::DIRT);

        assert_eq!(
            run(&world, "fill 20 201 0 21 201 0 minecraft:cave_air"),
            vec!["Successfully filled 2 block(s)"]
        );
        assert_eq!(
            run(&world, "clone 20 201 0 21 201 0 22 201 0 masked"),
            vec!["No blocks were cloned"]
        );
        assert_eq!(world.get_block(22, 201, 0), air());
    }

    #[test]
    fn setblock_destroy_drops_the_old_block() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 0 200 0 minecraft:stone"),
            vec!["Changed the block at 0, 200, 0"]
        );
        let out = run(&world, "setblock 0 200 0 minecraft:dirt destroy");
        assert_eq!(out, vec!["Changed the block at 0, 200, 0"]);
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
        assert_eq!(out, vec!["That position is out of this world!"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn setblock_rejects_block_entity_data_instead_of_silently_dropping_it() {
        let world = test_world();
        let out = run(&world, "setblock 0 200 0 minecraft:chest{Items:[]}");
        assert_eq!(out, vec!["Block entity data is not supported"]);
        assert_eq!(world.get_block(0, 200, 0), air());
    }

    #[test]
    fn fill_normalises_corners_and_reports_the_block_count() {
        let world = test_world();
        let out = run(&world, "fill 2 200 2 0 200 0 minecraft:stone");
        assert_eq!(out, vec!["Successfully filled 9 block(s)"]);
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
        assert_eq!(
            out,
            vec!["Too many blocks in the specified area (maximum 32768, but specified 33792)"]
        );
        assert_eq!(world.journal().head(), head);
        assert_eq!(world.get_block(0, 200, 0), air());
    }

    #[test]
    fn fill_that_changes_nothing_is_a_failure() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 0 200 0 1 200 1 minecraft:stone"),
            vec!["Successfully filled 4 block(s)"]
        );
        let out = run(&world, "fill 0 200 0 1 200 1 minecraft:stone");
        assert_eq!(out, vec!["No blocks were filled"]);
    }

    #[test]
    fn hollow_clears_the_interior_and_outline_leaves_it_alone() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:stone"),
            vec!["Successfully filled 27 block(s)"]
        );
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:dirt hollow"),
            vec!["Successfully filled 27 block(s)"]
        );
        assert_eq!(world.get_block(11, 201, 11), air(), "hollow interior");
        assert_eq!(world.get_block(10, 200, 10), block_ids::DIRT);
        assert_eq!(world.get_block(11, 202, 11), block_ids::DIRT);

        // Restore the solid cube, then outline only its shell.
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:stone"),
            vec!["Successfully filled 27 block(s)"]
        );
        assert_eq!(
            run(&world, "fill 10 200 10 12 202 12 minecraft:dirt outline"),
            vec!["Successfully filled 26 block(s)"]
        );
        assert_eq!(world.get_block(11, 201, 11), block_ids::STONE);
        assert_eq!(world.get_block(10, 200, 10), block_ids::DIRT);
    }

    #[test]
    fn fill_replace_filter_uses_the_vanilla_argument_order() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 20 200 20 22 200 22 minecraft:stone"),
            vec!["Successfully filled 9 block(s)"]
        );
        assert_eq!(
            run(&world, "setblock 21 200 21 minecraft:dirt"),
            vec!["Changed the block at 21, 200, 21"]
        );
        let out = run(
            &world,
            "fill 20 200 20 22 200 22 minecraft:gold_block replace minecraft:dirt",
        );
        assert_eq!(out, vec!["Successfully filled 1 block(s)"]);
        assert_eq!(world.get_block(21, 200, 21), block("minecraft:gold_block"));
        assert_eq!(world.get_block(20, 200, 20), block_ids::STONE);
    }

    #[test]
    fn fill_replace_filter_matches_written_properties_partially() {
        let world = test_world();
        let east_top = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:oak_stairs").unwrap(),
                &[("facing", "east"), ("half", "top")],
            )
            .unwrap(),
        );
        assert_eq!(
            run(
                &world,
                "setblock 30 200 30 minecraft:oak_stairs[facing=east,half=top]"
            ),
            vec!["Changed the block at 30, 200, 30"]
        );
        assert_eq!(world.get_block(30, 200, 30), east_top);
        let out = run(
            &world,
            "fill 30 200 30 30 200 30 minecraft:stone replace oak_stairs[facing=east]",
        );
        assert_eq!(out, vec!["Successfully filled 1 block(s)"]);
        assert_eq!(world.get_block(30, 200, 30), block_ids::STONE);
    }

    #[test]
    fn fill_replace_accepts_a_block_tag_filter() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 40 200 40 41 200 41 minecraft:oak_log"),
            vec!["Successfully filled 4 block(s)"]
        );
        assert_eq!(
            run(&world, "setblock 40 200 40 minecraft:stone"),
            vec!["Changed the block at 40, 200, 40"]
        );
        let out = run(
            &world,
            "fill 40 200 40 41 200 41 minecraft:dirt replace #minecraft:logs",
        );
        assert_eq!(out, vec!["Successfully filled 3 block(s)"]);
        assert_eq!(world.get_block(40, 200, 40), block_ids::STONE);
        assert_eq!(world.get_block(41, 200, 41), block_ids::DIRT);
    }

    #[test]
    fn fill_destroy_checks_the_whole_box_including_unchanged_cells() {
        let _guard = crate::permissions::test_lock();
        let mut perms = crate::permissions::Store::new();
        perms
            .add_member(crate::permissions::OP_GROUP, "Alice", None)
            .unwrap();
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
        assert_eq!(
            run(&world, "setblock 5 200 5 minecraft:stone"),
            vec!["Changed the block at 5, 200, 5"]
        );
        let registry = registry();
        let alice = PlayerHandle::for_test(1, "Alice");
        registry.insert_for_test(Arc::clone(&alice));

        let head = world.journal().head();
        let out = run_player(
            &world,
            &registry,
            &alice,
            "fill 5 200 5 5 200 5 minecraft:stone destroy",
        );
        assert_eq!(out, vec!["That area is protected."]);
        assert_eq!(world.journal().head(), head, "no drop, no revision");
        assert_eq!(world.get_block(5, 200, 5), block_ids::STONE);
        assert!(
            !world
                .journal()
                .recent_events(20)
                .unwrap()
                .iter()
                .any(|e| matches!(e.body, EventBody::ItemMint { .. })),
            "a refused destroy must not mint loot"
        );
    }

    #[test]
    fn clone_masked_copies_only_non_air() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 50 200 50 51 201 51 minecraft:stone"),
            vec!["Successfully filled 8 block(s)"]
        );
        assert_eq!(
            run(&world, "setblock 50 200 50 minecraft:air"),
            vec!["Changed the block at 50, 200, 50"]
        );
        assert_eq!(
            run(&world, "clone 50 200 50 51 201 51 60 200 60 masked"),
            vec!["Successfully cloned 7 block(s)"]
        );
        assert_eq!(world.get_block(60, 200, 60), air());
        assert_eq!(world.get_block(61, 201, 61), block_ids::STONE);
    }

    #[test]
    fn clone_normal_overwrites_a_non_air_destination_but_refuses_overlap() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 70 200 70 71 200 71 minecraft:stone"),
            vec!["Successfully filled 4 block(s)"]
        );
        assert_eq!(
            run(&world, "setblock 80 200 80 minecraft:dirt"),
            vec!["Changed the block at 80, 200, 80"]
        );
        let out = run(&world, "clone 70 200 70 71 200 71 80 200 80");
        assert_eq!(out, vec!["Successfully cloned 4 block(s)"]);
        assert_eq!(world.get_block(80, 200, 80), block_ids::STONE);

        let head = world.journal().head();
        let out = run(&world, "clone 70 200 70 71 200 71 70 200 71");
        assert_eq!(out, vec!["The source and destination areas cannot overlap"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn clone_force_and_move_may_overlap() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 90 200 90 92 200 90 minecraft:stone"),
            vec!["Successfully filled 3 block(s)"]
        );
        let out = run(&world, "clone 90 200 90 92 200 90 91 200 90 force");
        assert_eq!(out, vec!["Successfully cloned 3 block(s)"]);
        assert_eq!(world.get_block(93, 200, 90), block_ids::STONE);

        assert_eq!(
            run(&world, "fill 100 200 100 101 200 100 minecraft:dirt"),
            vec!["Successfully filled 2 block(s)"]
        );
        let out = run(&world, "clone 100 200 100 101 200 100 110 200 100 move");
        assert_eq!(out, vec!["Successfully cloned 2 block(s)"]);
        assert_eq!(world.get_block(110, 200, 100), block_ids::DIRT);
        assert_eq!(world.get_block(100, 200, 100), air());
        assert_eq!(world.get_block(101, 200, 100), air());
    }

    #[test]
    fn clone_move_into_itself_keeps_the_block() {
        // The brief's "clone into itself" edge case. A plain clone refuses the
        // overlap, but `move` may overlap: clearing the source and then writing
        // the copied block back must leave the cell occupied.
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 1 230 1 minecraft:stone"),
            vec!["Changed the block at 1, 230, 1"]
        );
        let out = run(&world, "clone 1 230 1 1 230 1 1 230 1 move");
        assert_eq!(out, vec!["Successfully cloned 1 block(s)"]);
        assert_eq!(
            world.get_block(1, 230, 1),
            block_ids::STONE,
            "a self-move must not delete the block it copied"
        );

        // A partially overlapping move behaves the same on the shared cells.
        assert_eq!(
            run(&world, "fill 2 230 1 4 230 1 minecraft:dirt"),
            vec!["Successfully filled 3 block(s)"]
        );
        assert_eq!(
            run(&world, "clone 2 230 1 4 230 1 3 230 1 move"),
            vec!["Successfully cloned 3 block(s)"]
        );
        assert_eq!(
            world.get_block(2, 230, 1),
            air(),
            "the uncovered source clears"
        );
        for x in 3..=5 {
            assert_eq!(
                world.get_block(x, 230, 1),
                block_ids::DIRT,
                "x={x} must survive the overlapping move"
            );
        }
    }

    #[test]
    fn clone_move_clears_the_source_container_record() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 5 230 5 minecraft:chest"),
            vec!["Changed the block at 5, 230, 5"]
        );
        containers::with(&world, (5, 230, 5), containers::ContainerKind::Chest, |c| {
            c.slots[0] = Some(Stack::named("minecraft:stone", 4));
        });
        let out = run(&world, "clone 5 230 5 5 230 5 6 230 5 move");
        assert_eq!(out, vec!["Successfully cloned 1 block(s)"]);
        assert_eq!(world.get_block(6, 230, 5), block("minecraft:chest"));
        let source = containers::snapshot(&world, (5, 230, 5), containers::ContainerKind::Chest);
        assert!(
            source.slots.iter().all(Option::is_none),
            "a moved-from container must not keep its old record"
        );
    }

    #[test]
    fn clone_filtered_copies_only_matching_source_blocks() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 120 200 120 121 200 121 minecraft:stone"),
            vec!["Successfully filled 4 block(s)"]
        );
        assert_eq!(
            run(&world, "setblock 120 200 120 minecraft:dirt"),
            vec!["Changed the block at 120, 200, 120"]
        );
        let out = run(
            &world,
            "clone 120 200 120 121 200 121 130 200 130 filtered minecraft:dirt",
        );
        assert_eq!(out, vec!["Successfully cloned 1 block(s)"]);
        assert_eq!(world.get_block(130, 200, 130), block_ids::DIRT);
        assert_eq!(world.get_block(131, 200, 131), air());
    }

    #[test]
    fn clone_accepts_and_validates_the_dimension_forms() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 140 200 140 140 200 140 minecraft:stone"),
            vec!["Successfully filled 1 block(s)"]
        );
        assert_eq!(
            run(
                &world,
                "clone from minecraft:overworld 140 200 140 140 200 140 \
                 to minecraft:overworld 141 200 140"
            ),
            vec!["Successfully cloned 1 block(s)"]
        );
        assert_eq!(world.get_block(141, 200, 140), block_ids::STONE);

        assert_eq!(
            run(
                &world,
                "clone from overworld 140 200 140 140 200 140 142 200 140"
            ),
            vec!["Successfully cloned 1 block(s)"]
        );
        assert_eq!(
            run(
                &world,
                "clone 140 200 140 140 200 140 to minecraft:overworld 143 200 140"
            ),
            vec!["Successfully cloned 1 block(s)"]
        );
        let out = run(
            &world,
            "clone from minecraft:the_nether 140 200 140 140 200 140 144 200 140",
        );
        assert_eq!(out, vec!["Unknown dimension 'minecraft:the_nether'"]);
        assert_eq!(world.get_block(144, 200, 140), air());
    }

    #[test]
    fn clone_into_itself_is_refused_without_a_journal_entry() {
        let world = test_world();
        assert_eq!(
            run(&world, "fill 150 200 150 151 201 151 minecraft:stone"),
            vec!["Successfully filled 8 block(s)"]
        );
        let head = world.journal().head();
        let out = run(&world, "clone 150 200 150 151 201 151 150 200 150");
        assert_eq!(out, vec!["The source and destination areas cannot overlap"]);
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn clone_refuses_a_volume_over_32768_before_snapshotting() {
        let world = test_world();
        let head = world.journal().head();
        let out = run(&world, "clone 0 200 0 32 231 31 40 200 40");
        assert_eq!(
            out,
            vec!["Too many blocks in the specified area (maximum 32768, but specified 33792)"]
        );
        assert_eq!(world.journal().head(), head);
    }

    #[test]
    fn clone_that_copies_nothing_is_a_failure() {
        let world = test_world();
        let out = run(&world, "clone 160 200 160 161 200 161 170 200 170 masked");
        assert_eq!(out, vec!["No blocks were cloned"]);
    }

    #[test]
    fn strict_modes_parse_and_still_write() {
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 180 200 180 minecraft:stone strict"),
            vec!["Changed the block at 180, 200, 180"]
        );
        assert_eq!(
            run(&world, "fill 180 200 180 181 200 180 minecraft:dirt strict"),
            vec!["Successfully filled 2 block(s)"]
        );
        assert_eq!(
            run(&world, "clone 180 200 180 181 200 180 182 200 180 strict"),
            vec!["Successfully cloned 2 block(s)"]
        );
        assert_eq!(world.get_block(182, 200, 180), block_ids::DIRT);
    }

    #[test]
    fn destroy_spills_a_container_and_replacing_it_clears_the_record() {
        // Distinct coordinates: the container store is process-wide and tests
        // run in parallel.
        let world = test_world();
        assert_eq!(
            run(&world, "setblock 190 220 190 minecraft:chest"),
            vec!["Changed the block at 190, 220, 190"]
        );
        containers::with(
            &world,
            (190, 220, 190),
            containers::ContainerKind::Chest,
            |c| {
                c.slots[0] = Some(Stack::named("minecraft:stone", 3));
            },
        );

        let out = run(&world, "setblock 190 220 190 minecraft:dirt destroy");
        assert_eq!(out, vec!["Changed the block at 190, 220, 190"]);
        let chest = containers::snapshot(&world, (190, 220, 190), containers::ContainerKind::Chest);
        assert!(
            chest.slots.iter().all(Option::is_none),
            "destroy must take the container's contents"
        );

        // Replacing a container discards its record so a later chest is empty.
        assert_eq!(
            run(&world, "setblock 191 220 190 minecraft:chest"),
            vec!["Changed the block at 191, 220, 190"]
        );
        containers::with(
            &world,
            (191, 220, 190),
            containers::ContainerKind::Chest,
            |c| {
                c.slots[0] = Some(Stack::named("minecraft:dirt", 2));
            },
        );
        assert_eq!(
            run(&world, "setblock 191 220 190 minecraft:stone"),
            vec!["Changed the block at 191, 220, 190"]
        );
        assert_eq!(
            run(&world, "setblock 191 220 190 minecraft:chest"),
            vec!["Changed the block at 191, 220, 190"]
        );
        let chest = containers::snapshot(&world, (191, 220, 190), containers::ContainerKind::Chest);
        assert!(chest.slots.iter().all(Option::is_none), "stale contents");
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
        assert_eq!(out, vec!["Changed the block at 0, 200, 0"]);
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
    fn a_filter_parses_block_states_partially_and_tags() {
        let stairs = parse_filter("oak_stairs[facing=east]").unwrap();
        let east = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:oak_stairs").unwrap(),
                &[("facing", "east")],
            )
            .unwrap(),
        );
        let east_top = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:oak_stairs").unwrap(),
                &[("facing", "east"), ("half", "top")],
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
        assert!(
            stairs.matches(east_top),
            "unwritten properties are wildcards"
        );
        assert!(!stairs.matches(west));

        let logs = parse_filter("#minecraft:logs").unwrap();
        assert!(logs.matches(block("minecraft:oak_log")));
        assert!(!logs.matches(block_ids::STONE));

        let wooden_stairs = parse_filter("#minecraft:wooden_stairs").unwrap();
        assert!(wooden_stairs.matches(east));
        let dark_oak = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:dark_oak_stairs").unwrap(),
                &[("facing", "east")],
            )
            .unwrap(),
        );
        let pale_oak = BlockStateId(
            props::state_with(
                blocks::block_id_of("minecraft:pale_oak_stairs").unwrap(),
                &[("facing", "east")],
            )
            .unwrap(),
        );
        assert!(
            wooden_stairs.matches(dark_oak) && wooden_stairs.matches(pale_oak),
            "two-word wood families must be recognised"
        );
        assert!(wooden_stairs.matches(block("minecraft:bamboo_stairs")));
        assert!(
            !wooden_stairs.matches(block("minecraft:bamboo_mosaic_stairs")),
            "bamboo mosaic is not a wooden family"
        );
        let wooden_slabs = parse_filter("#minecraft:wooden_slabs").unwrap();
        assert!(wooden_slabs.matches(block("minecraft:oak_slab")));
        assert!(!wooden_slabs.matches(block("minecraft:bamboo_mosaic_slab")));

        // Flowers: the potted variants are not in either flower tag, and
        // `pink_petals`/`wildflowers`/`cactus_flower` are in `#flowers` only.
        let flowers = parse_filter("#minecraft:flowers").unwrap();
        assert!(flowers.matches(block("minecraft:poppy")));
        assert!(flowers.matches(block("minecraft:red_tulip")));
        assert!(flowers.matches(block("minecraft:pink_petals")));
        assert!(!flowers.matches(block("minecraft:potted_red_tulip")));
        assert!(!flowers.matches(block("minecraft:potted_poppy")));
        let small_flowers = parse_filter("#minecraft:small_flowers").unwrap();
        assert!(small_flowers.matches(block("minecraft:red_tulip")));
        assert!(!small_flowers.matches(block("minecraft:pink_petals")));
        assert!(!small_flowers.matches(block("minecraft:potted_red_tulip")));

        // Crops: the two crop blocks added after 1.20 are in, the berry bush
        // and the stems are not.
        let crops = parse_filter("#minecraft:crops").unwrap();
        for crop in [
            "minecraft:wheat",
            "minecraft:carrots",
            "minecraft:potatoes",
            "minecraft:beetroots",
            "minecraft:torchflower_crop",
            "minecraft:pitcher_crop",
        ] {
            assert!(crops.matches(block(crop)), "{crop}");
        }
        for not_crop in [
            "minecraft:sweet_berry_bush",
            "minecraft:melon_stem",
            "minecraft:pumpkin_stem",
        ] {
            assert!(!crops.matches(block(not_crop)), "{not_crop}");
        }

        let signs = parse_filter("#minecraft:signs").unwrap();
        assert!(signs.matches(block("minecraft:oak_sign")));
        assert!(signs.matches(block("minecraft:oak_wall_sign")));
        assert!(!signs.matches(block("minecraft:oak_hanging_sign")));
        assert!(!signs.matches(block("minecraft:oak_wall_hanging_sign")));

        assert!(parse_filter("not_a_block").is_err());
        assert!(parse_filter("oak_stairs[facing=upside]").is_err());
        // Tags the built-in table cannot represent exactly fail closed,
        // including names that do not exist in 1.21.11 at all.
        for unknown in [
            "#minecraft:definitely_not_a_tag",
            "#minecraft:wooden_fence_gates",
            "#minecraft:concrete",
            "#minecraft:concrete_powder",
            "#minecraft:glass",
            "#minecraft:glass_panes",
            "#minecraft:carpets",
            "#minecraft:coral",
            "#minecraft:wooden_shelves",
        ] {
            assert!(parse_filter(unknown).is_err(), "{unknown}");
        }
    }
}
