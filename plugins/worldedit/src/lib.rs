mod brush;
mod clipboard;
mod pattern;
mod schematic;
mod selection;

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::worldedit::{
    self, valid_schematic_name, EditOutcome, PlayerView, WorldEditHost,
};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};
use brush::{Brush, BrushKind};
use clipboard::{is_air, Clipboard};
use pattern::{parse_filter, parse_pattern, Filter, Pattern};
use selection::{contract, expand, volume, Direction, Selection};

pub const ID: &str = "worldedit";
pub const NODE_WAND: &str = "worldedit.wand";
pub const NODE_SELECTION: &str = "worldedit.selection";
pub const NODE_REGION_SET: &str = "worldedit.region.set";
pub const NODE_REGION_REPLACE: &str = "worldedit.region.replace";
pub const NODE_REGION_WALLS: &str = "worldedit.region.walls";
pub const NODE_REGION_OUTLINE: &str = "worldedit.region.outline";
pub const NODE_REGION_MOVE: &str = "worldedit.region.move";
pub const NODE_CLIPBOARD_COPY: &str = "worldedit.clipboard.copy";
pub const NODE_CLIPBOARD_CUT: &str = "worldedit.clipboard.cut";
pub const NODE_CLIPBOARD_PASTE: &str = "worldedit.clipboard.paste";
pub const NODE_CLIPBOARD_ROTATE: &str = "worldedit.clipboard.rotate";
pub const NODE_CLIPBOARD_FLIP: &str = "worldedit.clipboard.flip";
pub const NODE_CLIPBOARD_STACK: &str = "worldedit.clipboard.stack";
pub const NODE_SCHEMATIC: &str = "worldedit.schematic";
pub const NODE_BRUSH: &str = "worldedit.brush";
pub const NODE_BRUSH_MASK: &str = "worldedit.brush.mask";
pub const NODE_HISTORY_UNDO: &str = "worldedit.history.undo";
pub const NODE_HISTORY_REDO: &str = "worldedit.history.redo";
pub const DEFAULT_WAND: &str = "minecraft:wooden_axe";
pub const DEFAULT_BRUSH_ITEM: &str = "minecraft:blaze_rod";
const DEFAULT_HISTORY: usize = 20;

/// A command with its node and required op level.
///
/// Selection and the wand are op 0 (public). Commands that write the world
/// (`worldedit.region.*`) and the history that can rewrite it
/// (`worldedit.history.*`) are op 2, the same level as the core `/fill`,
/// `/setblock` and `/clone`: op 0 declares a public node, so an op-0 `//set`
/// would let any player bulk-edit unprotected terrain. An operator can still
/// grant the node to a non-op explicitly (CMD-01/02).
fn detailed(name: &str, node: &str, op: u8) -> CommandDecl {
    CommandDecl::Detailed {
        name: name.to_string(),
        node: node.to_string(),
        op,
    }
}

pub fn manifest() -> Manifest {
    Manifest {
        id: ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "WorldEdit".to_string(),
        priority: 20,
        // Only what the plugin uses: logs and config, the online player's
        // world/held item, and the world read/write the service performs.
        // Selection, clipboard, brushes and history are in memory and
        // schematics go through the host service, so no `storage.kv`; nothing
        // probes features, so no `supports`.
        capabilities: vec![
            "log".to_string(),
            "config".to_string(),
            "world.read".to_string(),
            "world.write".to_string(),
            "players".to_string(),
        ],
        subscribe: vec![
            "before-block-dig".to_string(),
            "before-block-interact".to_string(),
            "after-player-leave".to_string(),
            "on-command".to_string(),
        ],
        commands: vec![
            // Selection and the wand do not write the world: public.
            detailed("wand", NODE_WAND, 0),
            detailed("pos1", NODE_SELECTION, 0),
            detailed("pos2", NODE_SELECTION, 0),
            detailed("sel", NODE_SELECTION, 0),
            detailed("expand", NODE_SELECTION, 0),
            detailed("contract", NODE_SELECTION, 0),
            // Bulk writes and the history that can replay them: operator
            // level, like the core world commands (`/fill`, `/setblock`).
            detailed("set", NODE_REGION_SET, 2),
            detailed("replace", NODE_REGION_REPLACE, 2),
            detailed("walls", NODE_REGION_WALLS, 2),
            detailed("outline", NODE_REGION_OUTLINE, 2),
            detailed("copy", NODE_CLIPBOARD_COPY, 0),
            detailed("cut", NODE_CLIPBOARD_CUT, 2),
            detailed("paste", NODE_CLIPBOARD_PASTE, 2),
            detailed("rotate", NODE_CLIPBOARD_ROTATE, 0),
            detailed("flip", NODE_CLIPBOARD_FLIP, 0),
            detailed("stack", NODE_CLIPBOARD_STACK, 2),
            detailed("move", NODE_REGION_MOVE, 2),
            detailed("schem", NODE_SCHEMATIC, 2),
            detailed("brush", NODE_BRUSH, 2),
            detailed("mask", NODE_BRUSH_MASK, 0),
            detailed("undo", NODE_HISTORY_UNDO, 2),
            detailed("redo", NODE_HISTORY_REDO, 2),
        ],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: vec![format!(
            "{}@{}",
            worldedit::WORLDEDIT_SERVICE,
            worldedit::WORLDEDIT_SERVICE_VERSION
        )],
    }
}

pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(WorldEditPlugin::default()),
    }
}

#[derive(Default)]
struct History {
    undo: Vec<u64>,
    redo: Vec<u64>,
}

#[derive(Clone, Copy)]
enum Shape {
    Box,
    Walls,
    Outline,
}

pub struct WorldEditPlugin {
    wand: String,
    brush_item: String,
    history_limit: usize,
    selections: HashMap<u64, Selection>,
    histories: HashMap<u64, History>,
    clipboards: HashMap<u64, Clipboard>,
    brushes: HashMap<u64, Brush>,
    masks: HashMap<u64, Filter>,
}

impl Default for WorldEditPlugin {
    fn default() -> Self {
        WorldEditPlugin {
            wand: DEFAULT_WAND.to_string(),
            brush_item: DEFAULT_BRUSH_ITEM.to_string(),
            history_limit: DEFAULT_HISTORY,
            selections: HashMap::new(),
            histories: HashMap::new(),
            clipboards: HashMap::new(),
            brushes: HashMap::new(),
            masks: HashMap::new(),
        }
    }
}

fn normalize_item(item: &str) -> String {
    if item.contains(':') {
        item.to_string()
    } else {
        format!("minecraft:{item}")
    }
}

fn host(ctx: &NativeContext) -> Result<Arc<dyn WorldEditHost>, String> {
    worldedit::host(ctx.services)
        .ok_or_else(|| "the worldedit host service is not available".to_string())
}

fn view(host: &dyn WorldEditHost, caller: &str, p: &Player) -> Result<PlayerView, String> {
    host.player_view(caller, p.id)
        .ok_or_else(|| "player is not online".to_string())
}

fn parse_count(args: &[String]) -> Result<usize, String> {
    match args {
        [] => Ok(1),
        [n] => n
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| "expected a positive number".to_string()),
        _ => Err("expected at most one number".to_string()),
    }
}

fn parse_amount(args: &[String]) -> Result<u32, String> {
    let text = args
        .first()
        .ok_or_else(|| "Usage: //expand|//contract <n> [direction]".to_string())?;
    text.parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "expected a positive number".to_string())
}

fn direction(
    args: &[String],
    host: &dyn WorldEditHost,
    caller: &str,
    p: &Player,
) -> Result<Direction, String> {
    match args.get(1) {
        Some(word) => Direction::parse(word).ok_or_else(|| format!("unknown direction `{word}`")),
        None => {
            let view = view(host, caller, p)?;
            Ok(Direction::from_yaw(view.yaw))
        }
    }
}

fn format_pos(pos: [i32; 3]) -> String {
    format!("{} {} {}", pos[0], pos[1], pos[2])
}

fn player_block(view: &PlayerView) -> [i32; 3] {
    [
        view.x.floor() as i32,
        view.y.floor() as i32,
        view.z.floor() as i32,
    ]
}

fn shift(pos: [i32; 3], delta: [i64; 3]) -> Result<[i32; 3], String> {
    let mut out = [0; 3];
    for axis in 0..3 {
        out[axis] = i64::from(pos[axis])
            .checked_add(delta[axis])
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| "the operation would leave the world coordinate range".to_string())?;
    }
    Ok(out)
}

fn scale(delta: [i64; 3], times: i64) -> Result<[i64; 3], String> {
    let mut out = [0; 3];
    for axis in 0..3 {
        out[axis] = delta[axis]
            .checked_mul(times)
            .ok_or_else(|| "the operation is too large".to_string())?;
    }
    Ok(out)
}

fn direction_unit(direction: Direction) -> [i64; 3] {
    match direction {
        Direction::North => [0, 0, -1],
        Direction::South => [0, 0, 1],
        Direction::East => [1, 0, 0],
        Direction::West => [-1, 0, 0],
        Direction::Up => [0, 1, 0],
        Direction::Down => [0, -1, 0],
    }
}

fn parse_degrees(text: &str) -> Result<i32, String> {
    let degrees = text
        .parse::<i32>()
        .map_err(|_| format!("`{text}` is not a whole number of degrees"))?;
    if degrees % 90 != 0 {
        return Err("rotation must be a multiple of 90 degrees".to_string());
    }
    Ok(degrees)
}

#[derive(Clone, Copy, Default)]
struct PasteFlags {
    skip_air: bool,
    overwrite: bool,
    select: bool,
}

impl PasteFlags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = PasteFlags::default();
        for arg in args {
            match arg.as_str() {
                "-a" => flags.skip_air = true,
                "-o" => flags.overwrite = true,
                "-s" => flags.select = true,
                other => return Err(format!("unknown paste option `{other}`")),
            }
        }
        Ok(flags)
    }
}

impl WorldEditPlugin {
    fn wand_matches(&self, item: Option<String>) -> bool {
        item.is_some_and(|item| normalize_item(&item) == self.wand)
    }

    fn brush_matches(&self, player: u64, item: Option<String>) -> bool {
        let Some(brush) = self.brushes.get(&player) else {
            return false;
        };
        item.is_some_and(|item| normalize_item(&item) == brush.item)
    }

    fn history(&mut self, player: u64) -> &mut History {
        self.histories.entry(player).or_default()
    }

    fn record_commit(&mut self, player: u64, id: u64) {
        if id == 0 {
            return;
        }
        let limit = self.history_limit;
        let history = self.history(player);
        history.redo.clear();
        history.undo.push(id);
        while history.undo.len() > limit {
            history.undo.remove(0);
        }
    }

    fn selection_world(
        &self,
        host: &dyn WorldEditHost,
        caller: &str,
        p: &Player,
    ) -> Result<(), String> {
        let view = view(host, caller, p)?;
        if let Some(selected) = self.selections.get(&p.id).and_then(|s| s.world.as_deref()) {
            if selected != view.world {
                return Err(format!(
                    "the selection belongs to `{selected}`, not `{}`",
                    view.world
                ));
            }
        }
        Ok(())
    }

    fn capture_clipboard(
        &self,
        caller: &str,
        p: &Player,
        host: &dyn WorldEditHost,
    ) -> Result<Clipboard, String> {
        let view = view(host, caller, p)?;
        self.selection_world(host, caller, p)?;
        let (min, max) = self.selection_bounds(p)?;
        self.check_volume(host, caller, min, max)?;
        let size = [
            (i64::from(max[0]) - i64::from(min[0]) + 1) as usize,
            (i64::from(max[1]) - i64::from(min[1]) + 1) as usize,
            (i64::from(max[2]) - i64::from(min[2]) + 1) as usize,
        ];
        let capacity = size[0]
            .checked_mul(size[1])
            .and_then(|n| n.checked_mul(size[2]))
            .ok_or_else(|| "the selection is too large".to_string())?;
        let mut blocks = Vec::with_capacity(capacity);
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                for x in min[0]..=max[0] {
                    blocks.push(
                        host.block_name(caller, x, y, z)
                            .ok_or_else(|| format!("no block at {x} {y} {z}"))?,
                    );
                }
            }
        }
        let pos = player_block(&view);
        let mut offset = [0; 3];
        for axis in 0..3 {
            offset[axis] = i32::try_from(i64::from(min[axis]) - i64::from(pos[axis]))
                .map_err(|_| "the clipboard offset is out of range".to_string())?;
        }
        Ok(Clipboard {
            world: view.world,
            offset,
            size,
            blocks,
        })
    }

    fn commit_writes(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        host: &dyn WorldEditHost,
        writes: &[([i32; 3], String)],
    ) -> Result<EditOutcome, String> {
        if writes.is_empty() {
            return Ok(EditOutcome {
                changed: 0,
                undo_id: 0,
            });
        }
        let cells: Vec<[i32; 3]> = writes.iter().map(|(pos, _)| *pos).collect();
        host.check_cells(ctx.id, p.id, &cells)?;
        let edit = host.begin_edit(ctx.id, p.id, writes.len())?;
        for (pos, block) in writes {
            if let Err(e) = host.set_block(ctx.id, edit, pos[0], pos[1], pos[2], block) {
                host.abort(ctx.id, edit);
                return Err(e);
            }
        }
        let outcome = match host.commit(ctx.id, edit) {
            Ok(outcome) => outcome,
            Err(e) => {
                host.abort(ctx.id, edit);
                return Err(e);
            }
        };
        self.record_commit(p.id, outcome.undo_id);
        Ok(outcome)
    }

    fn select_box(&mut self, p: &Player, world: &str, min: [i32; 3], max: [i32; 3]) {
        let selection = self.selections.entry(p.id).or_default();
        selection.set_point(true, world, min);
        selection.set_point(false, world, max);
    }

    fn selection_bounds(&self, p: &Player) -> Result<([i32; 3], [i32; 3]), String> {
        self.selections
            .get(&p.id)
            .and_then(Selection::bounds)
            .ok_or_else(|| "No selection; set //pos1 and //pos2 first".to_string())
    }

    fn set_position(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
        first: bool,
    ) -> Result<Vec<String>, String> {
        let host = host(ctx)?;
        let view = view(host.as_ref(), ctx.id, p)?;
        let pos = if args.is_empty() {
            [
                view.x.floor() as i32,
                view.y.floor() as i32,
                view.z.floor() as i32,
            ]
        } else {
            let text = args.join(" ");
            host.resolve_position(ctx.id, p.id, &text)?
        };
        self.selections
            .entry(p.id)
            .or_default()
            .set_point(first, &view.world, pos);
        Ok(vec![format!(
            "Position {} set to {}",
            if first { 1 } else { 2 },
            format_pos(pos)
        )])
    }

    fn show_selection(&self, p: &Player) -> Result<Vec<String>, String> {
        let selection = self
            .selections
            .get(&p.id)
            .ok_or_else(|| "No selection".to_string())?;
        match selection.bounds() {
            Some((min, max)) => Ok(vec![format!(
                "Selection: {} .. {} ({} blocks, {})",
                format_pos(min),
                format_pos(max),
                volume(min, max),
                selection.world.as_deref().unwrap_or("unknown")
            )]),
            None => {
                let mut lines = vec!["Selection is incomplete".to_string()];
                if let Some(p1) = selection.p1 {
                    lines.push(format!("Position 1: {}", format_pos(p1)));
                }
                if let Some(p2) = selection.p2 {
                    lines.push(format!("Position 2: {}", format_pos(p2)));
                }
                Ok(lines)
            }
        }
    }

    fn change_selection(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
        grow: bool,
    ) -> Result<Vec<String>, String> {
        let amount = parse_amount(args)?;
        let host = host(ctx)?;
        let dir = direction(args, host.as_ref(), ctx.id, p)?;
        let selection = self
            .selections
            .get(&p.id)
            .ok_or_else(|| "No selection".to_string())?;
        let (min, max) = selection
            .bounds()
            .ok_or_else(|| "No complete selection".to_string())?;
        let (min, max) = if grow {
            expand(min, max, amount, dir)?
        } else {
            contract(min, max, amount, dir)?
        };
        let selection = self.selections.entry(p.id).or_default();
        selection.p1 = Some(min);
        selection.p2 = Some(max);
        Ok(vec![format!(
            "Selection: {} .. {} ({} blocks)",
            format_pos(min),
            format_pos(max),
            volume(min, max)
        )])
    }

    fn check_volume(
        &self,
        host: &dyn WorldEditHost,
        caller: &str,
        min: [i32; 3],
        max: [i32; 3],
    ) -> Result<usize, String> {
        let volume = volume(min, max);
        let max_volume = host.max_volume(caller);
        if volume > max_volume as u128 {
            return Err(format!(
                "selection of {volume} blocks exceeds the limit of {max_volume}"
            ));
        }
        Ok(volume as usize)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_edit(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        host: &dyn WorldEditHost,
        min: [i32; 3],
        max: [i32; 3],
        shape: Shape,
        filter: Filter,
        pattern: &Pattern,
    ) -> Result<Vec<String>, String> {
        self.selection_world(host, ctx.id, p)?;
        let clipboard = if matches!(pattern, Pattern::Clipboard) {
            Some(
                self.clipboards
                    .get(&p.id)
                    .cloned()
                    .ok_or_else(|| "the clipboard is empty".to_string())?,
            )
        } else {
            None
        };
        let expected = self.check_volume(host, ctx.id, min, max)?;
        host.check_build(ctx.id, p.id, min, max)?;
        let edit = host.begin_edit(ctx.id, p.id, expected)?;
        let single = match pattern {
            Pattern::Weighted { choices, .. } if choices.len() == 1 => Some(choices[0].1.clone()),
            _ => None,
        };
        let staged =
            if matches!(shape, Shape::Box) && matches!(&filter, Filter::Any) && single.is_some() {
                host.fill(
                    ctx.id,
                    edit,
                    min,
                    max,
                    single.as_deref().unwrap_or_default(),
                )
            } else {
                let mut result = Ok(());
                for x in min[0]..=max[0] {
                    for y in min[1]..=max[1] {
                        for z in min[2]..=max[2] {
                            let keep = match shape {
                                Shape::Box => true,
                                Shape::Walls => {
                                    x == min[0] || x == max[0] || z == min[2] || z == max[2]
                                }
                                Shape::Outline => {
                                    x == min[0]
                                        || x == max[0]
                                        || y == min[1]
                                        || y == max[1]
                                        || z == min[2]
                                        || z == max[2]
                                }
                            };
                            if !keep {
                                continue;
                            }
                            if !matches!(&filter, Filter::Any) {
                                let Some(name) = host.block_name(ctx.id, x, y, z) else {
                                    continue;
                                };
                                if !filter.matches(&name) {
                                    continue;
                                }
                            }
                            let picked = match clipboard.as_ref() {
                                Some(clipboard) => clipboard
                                    .get(
                                        (x - min[0]) as usize,
                                        (y - min[1]) as usize,
                                        (z - min[2]) as usize,
                                    )
                                    .map(str::to_string)
                                    .ok_or_else(|| {
                                        "the selection is larger than the clipboard".to_string()
                                    }),
                                None => pattern.pick(host, ctx.id, x, y, z),
                            };
                            match picked {
                                Ok(block) => {
                                    if let Err(e) = host.set_block(ctx.id, edit, x, y, z, &block) {
                                        result = Err(e);
                                        break;
                                    }
                                }
                                Err(e) => {
                                    result = Err(e);
                                    break;
                                }
                            }
                        }
                        if result.is_err() {
                            break;
                        }
                    }
                    if result.is_err() {
                        break;
                    }
                }
                result
            };
        if let Err(e) = staged {
            host.abort(ctx.id, edit);
            return Err(e);
        }
        let outcome = match host.commit(ctx.id, edit) {
            Ok(outcome) => outcome,
            Err(e) => {
                host.abort(ctx.id, edit);
                return Err(e);
            }
        };
        self.record_commit(p.id, outcome.undo_id);
        Ok(vec![format!("{} block(s) changed", outcome.changed)])
    }

    fn command_set(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.is_empty() {
            return Err("Usage: //set <pattern>".to_string());
        }
        let host = host(ctx)?;
        let (min, max) = self.selection_bounds(p)?;
        let pos1 = self.selections.get(&p.id).and_then(|s| s.p1);
        let pattern = parse_pattern(&args.join(" "), host.as_ref(), ctx.id, p.id, pos1)?;
        self.run_edit(
            ctx,
            p,
            host.as_ref(),
            min,
            max,
            Shape::Box,
            Filter::Any,
            &pattern,
        )
    }

    fn command_replace(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.is_empty() {
            return Err("Usage: //replace [<filter>] <pattern>".to_string());
        }
        let host = host(ctx)?;
        let (min, max) = self.selection_bounds(p)?;
        let pos1 = self.selections.get(&p.id).and_then(|s| s.p1);
        let (filter, pattern_text) = if args.len() == 1 {
            (Filter::Any, args[0].clone())
        } else {
            (
                parse_filter(&args[0], host.as_ref(), ctx.id)?,
                args[1..].join(" "),
            )
        };
        let pattern = parse_pattern(&pattern_text, host.as_ref(), ctx.id, p.id, pos1)?;
        self.run_edit(
            ctx,
            p,
            host.as_ref(),
            min,
            max,
            Shape::Box,
            filter,
            &pattern,
        )
    }

    fn command_shape(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
        shape: Shape,
        usage: &str,
    ) -> Result<Vec<String>, String> {
        if args.is_empty() {
            return Err(format!("Usage: {usage}"));
        }
        let host = host(ctx)?;
        let (min, max) = self.selection_bounds(p)?;
        let pos1 = self.selections.get(&p.id).and_then(|s| s.p1);
        let pattern = parse_pattern(&args.join(" "), host.as_ref(), ctx.id, p.id, pos1)?;
        self.run_edit(
            ctx,
            p,
            host.as_ref(),
            min,
            max,
            shape,
            Filter::Any,
            &pattern,
        )
    }

    fn command_copy(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        _args: &[String],
    ) -> Result<Vec<String>, String> {
        let host = host(ctx)?;
        let clipboard = self.capture_clipboard(ctx.id, p, host.as_ref())?;
        let volume = clipboard.volume();
        self.clipboards.insert(p.id, clipboard);
        Ok(vec![format!("Copied {volume} block(s) to the clipboard")])
    }

    fn command_cut(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        _args: &[String],
    ) -> Result<Vec<String>, String> {
        let host = host(ctx)?;
        let clipboard = self.capture_clipboard(ctx.id, p, host.as_ref())?;
        let (min, max) = self.selection_bounds(p)?;
        let expected = self.check_volume(host.as_ref(), ctx.id, min, max)?;
        host.check_build(ctx.id, p.id, min, max)?;
        let edit = host.begin_edit(ctx.id, p.id, expected)?;
        if let Err(e) = host.fill(ctx.id, edit, min, max, "minecraft:air") {
            host.abort(ctx.id, edit);
            return Err(e);
        }
        let outcome = match host.commit(ctx.id, edit) {
            Ok(outcome) => outcome,
            Err(e) => {
                host.abort(ctx.id, edit);
                return Err(e);
            }
        };
        let volume = clipboard.volume();
        self.clipboards.insert(p.id, clipboard);
        self.record_commit(p.id, outcome.undo_id);
        Ok(vec![format!(
            "Cut {volume} block(s); {} changed",
            outcome.changed
        )])
    }

    fn paste_clipboard(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        host: &dyn WorldEditHost,
        clipboard: &Clipboard,
        base: [i32; 3],
        flags: PasteFlags,
        world: &str,
    ) -> Result<Vec<String>, String> {
        let max = host.max_volume(ctx.id);
        let volume = clipboard.volume();
        if volume > max as u128 {
            return Err(format!(
                "clipboard of {volume} blocks exceeds the limit of {max}"
            ));
        }
        let mut writes = Vec::new();
        for y in 0..clipboard.size[1] {
            for z in 0..clipboard.size[2] {
                for x in 0..clipboard.size[0] {
                    let block = clipboard
                        .get(x, y, z)
                        .ok_or_else(|| "the clipboard is corrupt".to_string())?;
                    if flags.skip_air && is_air(block) {
                        continue;
                    }
                    let target = shift(base, [x as i64, y as i64, z as i64])?;
                    if !flags.overwrite {
                        let existing = host
                            .block_name(ctx.id, target[0], target[1], target[2])
                            .ok_or_else(|| format!("no block at {}", format_pos(target)))?;
                        if !is_air(&existing) {
                            continue;
                        }
                    }
                    writes.push((target, block.to_string()));
                }
            }
        }
        let pasted = writes.len();
        let outcome = self.commit_writes(ctx, p, host, &writes)?;
        if flags.select {
            let extent = [
                clipboard.size[0] as i64 - 1,
                clipboard.size[1] as i64 - 1,
                clipboard.size[2] as i64 - 1,
            ];
            let corner = shift(base, extent)?;
            self.select_box(p, world, base, corner);
        }
        Ok(vec![format!(
            "Pasted {pasted} block(s); {} changed",
            outcome.changed
        )])
    }

    fn command_paste(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let flags = PasteFlags::parse(args)?;
        let clipboard = self
            .clipboards
            .get(&p.id)
            .cloned()
            .ok_or_else(|| "the clipboard is empty".to_string())?;
        let host = host(ctx)?;
        let view = view(host.as_ref(), ctx.id, p)?;
        let pos = player_block(&view);
        let base = shift(
            pos,
            [
                i64::from(clipboard.offset[0]),
                i64::from(clipboard.offset[1]),
                i64::from(clipboard.offset[2]),
            ],
        )?;
        self.paste_clipboard(ctx, p, host.as_ref(), &clipboard, base, flags, &view.world)
    }

    fn command_rotate(
        &mut self,
        _ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.is_empty() || args.len() > 3 {
            return Err("Usage: //rotate <y> [x] [z]".to_string());
        }
        let mut angles = [0i32; 3];
        for (axis, arg) in args.iter().enumerate() {
            angles[axis] = parse_degrees(arg)?;
        }
        let clipboard = self
            .clipboards
            .get(&p.id)
            .cloned()
            .ok_or_else(|| "the clipboard is empty".to_string())?;
        let rotated = clipboard.rotate(angles[0], angles[1], angles[2])?;
        self.clipboards.insert(p.id, rotated);
        Ok(vec![format!(
            "Rotated the clipboard by {} {} {} degrees",
            angles[0], angles[1], angles[2]
        )])
    }

    fn command_flip(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.len() > 1 {
            return Err("Usage: //flip [direction]".to_string());
        }
        let host = host(ctx)?;
        let dir = match args.first() {
            Some(word) => {
                Direction::parse(word).ok_or_else(|| format!("unknown direction `{word}`"))?
            }
            None => {
                let view = view(host.as_ref(), ctx.id, p)?;
                Direction::from_yaw(view.yaw)
            }
        };
        let clipboard = self
            .clipboards
            .get(&p.id)
            .cloned()
            .ok_or_else(|| "the clipboard is empty".to_string())?;
        self.clipboards.insert(p.id, clipboard.flip(dir));
        Ok(vec![format!("Flipped the clipboard {dir:?}")])
    }

    fn command_stack(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.is_empty() || args.len() > 2 {
            return Err("Usage: //stack <n> [direction]".to_string());
        }
        let copies = args[0]
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| "expected a positive number of copies".to_string())?;
        let host = host(ctx)?;
        let dir = match args.get(1) {
            Some(word) => {
                Direction::parse(word).ok_or_else(|| format!("unknown direction `{word}`"))?
            }
            None => {
                let view = view(host.as_ref(), ctx.id, p)?;
                Direction::from_yaw(view.yaw)
            }
        };
        let clipboard = self.capture_clipboard(ctx.id, p, host.as_ref())?;
        let max = host.max_volume(ctx.id);
        let blocks = clipboard.volume();
        let expected = blocks.saturating_mul(copies as u128 + 1);
        if expected > max as u128 {
            return Err(format!(
                "stack of {expected} blocks exceeds the limit of {max}"
            ));
        }
        let (min, source_max) = self.selection_bounds(p)?;
        let step = clipboard.step(dir);
        let mut writes = std::collections::BTreeMap::new();
        for copy in 1..=copies {
            let times = i64::try_from(copy).map_err(|_| "the stack is too large".to_string())?;
            let base = shift(min, scale(step, times)?)?;
            for y in 0..clipboard.size[1] {
                for z in 0..clipboard.size[2] {
                    for x in 0..clipboard.size[0] {
                        let block = clipboard
                            .get(x, y, z)
                            .ok_or_else(|| "the clipboard is corrupt".to_string())?;
                        let target = shift(base, [x as i64, y as i64, z as i64])?;
                        writes.insert(target, block.to_string());
                    }
                }
            }
        }
        if writes.len() > max {
            return Err(format!(
                "stack of {} blocks exceeds the limit of {max}",
                writes.len()
            ));
        }
        let ordered: Vec<([i32; 3], String)> = writes.into_iter().collect();
        let outcome = self.commit_writes(ctx, p, host.as_ref(), &ordered)?;
        let times = i64::try_from(copies).map_err(|_| "the stack is too large".to_string())?;
        let base = shift(min, scale(step, times)?)?;
        let corner = shift(
            base,
            [
                clipboard.size[0] as i64 - 1,
                clipboard.size[1] as i64 - 1,
                clipboard.size[2] as i64 - 1,
            ],
        )?;
        let union_min = [
            min[0].min(base[0]),
            min[1].min(base[1]),
            min[2].min(base[2]),
        ];
        let union_max = [
            source_max[0].max(corner[0]),
            source_max[1].max(corner[1]),
            source_max[2].max(corner[2]),
        ];
        let view = view(host.as_ref(), ctx.id, p)?;
        self.select_box(p, &view.world, union_min, union_max);
        Ok(vec![format!(
            "Stacked {copies} copy/copies; {} changed",
            outcome.changed
        )])
    }

    fn command_move(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let (amount, word) = match args {
            [count] => (
                count
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| "expected a positive number".to_string())?,
                None,
            ),
            [first, second] => {
                if let Some(count) = first.parse::<usize>().ok().filter(|n| *n > 0) {
                    (count, Some(second.as_str()))
                } else if let Some(count) = second.parse::<usize>().ok().filter(|n| *n > 0) {
                    (count, Some(first.as_str()))
                } else {
                    return Err("Usage: //move <n> [direction]".to_string());
                }
            }
            _ => return Err("Usage: //move <n> [direction]".to_string()),
        };
        let host = host(ctx)?;
        let dir = match word {
            Some(word) => {
                Direction::parse(word).ok_or_else(|| format!("unknown direction `{word}`"))?
            }
            None => {
                let view = view(host.as_ref(), ctx.id, p)?;
                Direction::from_yaw(view.yaw)
            }
        };
        let clipboard = self.capture_clipboard(ctx.id, p, host.as_ref())?;
        let (min, _) = self.selection_bounds(p)?;
        let times = i64::try_from(amount).map_err(|_| "the move is too large".to_string())?;
        let base = shift(min, scale(direction_unit(dir), times)?)?;
        let max = host.max_volume(ctx.id);
        let mut writes = std::collections::BTreeMap::new();
        for y in 0..clipboard.size[1] {
            for z in 0..clipboard.size[2] {
                for x in 0..clipboard.size[0] {
                    let source = shift(min, [x as i64, y as i64, z as i64])?;
                    writes.insert(source, "minecraft:air".to_string());
                }
            }
        }
        for y in 0..clipboard.size[1] {
            for z in 0..clipboard.size[2] {
                for x in 0..clipboard.size[0] {
                    let block = clipboard
                        .get(x, y, z)
                        .ok_or_else(|| "the clipboard is corrupt".to_string())?;
                    let target = shift(base, [x as i64, y as i64, z as i64])?;
                    writes.insert(target, block.to_string());
                }
            }
        }
        if writes.len() > max {
            return Err(format!(
                "move of {} blocks exceeds the limit of {max}",
                writes.len()
            ));
        }
        let ordered: Vec<([i32; 3], String)> = writes.into_iter().collect();
        let outcome = self.commit_writes(ctx, p, host.as_ref(), &ordered)?;
        let corner = shift(
            base,
            [
                clipboard.size[0] as i64 - 1,
                clipboard.size[1] as i64 - 1,
                clipboard.size[2] as i64 - 1,
            ],
        )?;
        let view = view(host.as_ref(), ctx.id, p)?;
        self.select_box(p, &view.world, base, corner);
        Ok(vec![format!(
            "Moved {} block(s) {dir:?}; {} changed",
            clipboard.volume(),
            outcome.changed
        )])
    }

    fn command_schem(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let Some(sub) = args.first() else {
            return Err("Usage: //schem save|load|list|paste <name>".to_string());
        };
        let host = host(ctx)?;
        match sub.as_str() {
            "save" => {
                let name = args
                    .get(1)
                    .ok_or_else(|| "Usage: //schem save <name>".to_string())?;
                if args.len() != 2 {
                    return Err("Usage: //schem save <name>".to_string());
                }
                if !valid_schematic_name(name) {
                    return Err(format!("invalid schematic name `{name}`"));
                }
                let clipboard = self
                    .clipboards
                    .get(&p.id)
                    .ok_or_else(|| "the clipboard is empty".to_string())?;
                let bytes = schematic::to_bytes(clipboard)?;
                host.save_schematic(ctx.id, name, &bytes)?;
                Ok(vec![format!("Saved schematic `{name}`")])
            }
            "load" => {
                let name = args
                    .get(1)
                    .ok_or_else(|| "Usage: //schem load <name>".to_string())?;
                if args.len() != 2 {
                    return Err("Usage: //schem load <name>".to_string());
                }
                if !valid_schematic_name(name) {
                    return Err(format!("invalid schematic name `{name}`"));
                }
                let bytes = host
                    .load_schematic(ctx.id, name)?
                    .ok_or_else(|| format!("no schematic named `{name}`"))?;
                let clipboard = schematic::from_bytes(&bytes)?;
                let size = clipboard.size;
                self.clipboards.insert(p.id, clipboard);
                Ok(vec![format!(
                    "Loaded schematic `{name}` ({}x{}x{})",
                    size[0], size[1], size[2]
                )])
            }
            "list" => {
                if args.len() != 1 {
                    return Err("Usage: //schem list".to_string());
                }
                let names = host.list_schematics(ctx.id);
                if names.is_empty() {
                    Ok(vec!["No schematics".to_string()])
                } else {
                    Ok(vec![format!("Schematics: {}", names.join(", "))])
                }
            }
            "paste" => {
                let mut flags = PasteFlags::default();
                let mut name = None;
                for arg in &args[1..] {
                    match arg.as_str() {
                        "-a" => flags.skip_air = true,
                        "-o" => flags.overwrite = true,
                        "-s" => flags.select = true,
                        other if name.is_none() && !other.starts_with('-') => {
                            name = Some(other.to_string());
                        }
                        other => return Err(format!("unknown option `{other}`")),
                    }
                }
                let name = name.ok_or_else(|| "Usage: //schem paste <name>".to_string())?;
                if !valid_schematic_name(&name) {
                    return Err(format!("invalid schematic name `{name}`"));
                }
                let bytes = host
                    .load_schematic(ctx.id, &name)?
                    .ok_or_else(|| format!("no schematic named `{name}`"))?;
                let clipboard = schematic::from_bytes(&bytes)?;
                let view = view(host.as_ref(), ctx.id, p)?;
                let pos = player_block(&view);
                let base = shift(
                    pos,
                    [
                        i64::from(clipboard.offset[0]),
                        i64::from(clipboard.offset[1]),
                        i64::from(clipboard.offset[2]),
                    ],
                )?;
                self.paste_clipboard(ctx, p, host.as_ref(), &clipboard, base, flags, &view.world)
            }
            other => Err(format!("unknown schematic command `{other}`")),
        }
    }

    fn command_brush(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let Some(kind) = args.first() else {
            return Ok(match self.brushes.get(&p.id) {
                Some(brush) => vec![format!(
                    "Brush: {:?} radius {} height {}",
                    brush.kind, brush.radius, brush.height
                )],
                None => vec!["No brush bound".to_string()],
            });
        };
        if kind.as_str() == "clear" {
            self.brushes.remove(&p.id);
            return Ok(vec!["Brush cleared".to_string()]);
        }
        let host = host(ctx)?;
        let held = ctx
            .backend
            .held_item(ctx.id, p.id)
            .map(|item| normalize_item(&item))
            .unwrap_or_else(|| self.brush_item.clone());
        let brush = match kind.as_str() {
            "sphere" | "cyl" => {
                let rest = &args[1..];
                if rest.is_empty() {
                    return Err(format!("Usage: //brush {kind} <pattern> [radius]"));
                }
                let mut end = rest.len();
                let mut radius = 2u32;
                let mut height = 1u32;
                if kind.as_str() == "cyl"
                    && end >= 2
                    && rest[end - 1].parse::<u32>().is_ok()
                    && rest[end - 2].parse::<u32>().is_ok()
                {
                    height = rest[end - 1]
                        .parse::<u32>()
                        .map_err(|_| "invalid height".to_string())?;
                    radius = rest[end - 2]
                        .parse::<u32>()
                        .map_err(|_| "invalid radius".to_string())?;
                    end -= 2;
                } else if end >= 1 && rest[end - 1].parse::<u32>().is_ok() {
                    radius = rest[end - 1]
                        .parse::<u32>()
                        .map_err(|_| "invalid radius".to_string())?;
                    end -= 1;
                }
                if end == 0 {
                    return Err("expected a pattern".to_string());
                }
                if kind.as_str() == "cyl" && height == 0 {
                    return Err("the cylinder height must be positive".to_string());
                }
                let pos1 = self.selections.get(&p.id).and_then(|s| s.p1);
                let pattern =
                    parse_pattern(&rest[..end].join(" "), host.as_ref(), ctx.id, p.id, pos1)?;
                Brush {
                    kind: if kind.as_str() == "cyl" {
                        BrushKind::Cylinder
                    } else {
                        BrushKind::Sphere
                    },
                    pattern: Some(pattern),
                    radius,
                    height,
                    item: held,
                }
            }
            "smooth" => {
                if args.len() > 2 {
                    return Err("Usage: //brush smooth [radius]".to_string());
                }
                let radius = match args.get(1) {
                    Some(text) => text
                        .parse::<u32>()
                        .map_err(|_| "invalid radius".to_string())?,
                    None => 3,
                };
                Brush {
                    kind: BrushKind::Smooth,
                    pattern: None,
                    radius,
                    height: 1,
                    item: held,
                }
            }
            other => return Err(format!("unknown brush `{other}`")),
        };
        let max = host.max_volume(ctx.id);
        if brush.estimated_volume() > max as u128 {
            return Err(format!(
                "brush of {} blocks exceeds the limit of {max}",
                brush.estimated_volume()
            ));
        }
        let summary = format!("Brush: {:?} radius {}", brush.kind, brush.radius);
        self.brushes.insert(p.id, brush);
        Ok(vec![summary])
    }

    fn command_mask(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        if args.is_empty() {
            return Ok(match self.masks.get(&p.id) {
                Some(filter) => vec![format!("Brush mask: {filter:?}")],
                None => vec!["No brush mask".to_string()],
            });
        }
        if args.len() == 1 && args[0] == "clear" {
            self.masks.remove(&p.id);
            return Ok(vec!["Brush mask cleared".to_string()]);
        }
        let host = host(ctx)?;
        let filter = parse_filter(&args.join(" "), host.as_ref(), ctx.id)?;
        self.masks.insert(p.id, filter);
        Ok(vec!["Brush mask set".to_string()])
    }

    fn apply_brush(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<usize, String> {
        let brush = self
            .brushes
            .get(&p.id)
            .cloned()
            .ok_or_else(|| "no brush bound".to_string())?;
        let host = host(ctx)?;
        let max = host.max_volume(ctx.id);
        if brush.estimated_volume() > max as u128 {
            return Err(format!(
                "brush of {} blocks exceeds the limit of {max}",
                brush.estimated_volume()
            ));
        }
        let mask = self.masks.get(&p.id).cloned();
        let mut writes = Vec::new();
        for pos in brush.cells([x, y, z])? {
            if let Some(mask) = &mask {
                let Some(name) = host.block_name(ctx.id, pos[0], pos[1], pos[2]) else {
                    continue;
                };
                if !mask.matches(&name) {
                    continue;
                }
            }
            let block = match &brush.pattern {
                Some(pattern) => pattern.pick(host.as_ref(), ctx.id, pos[0], pos[1], pos[2])?,
                None => "minecraft:air".to_string(),
            };
            writes.push((pos, block));
        }
        if writes.len() > max {
            return Err(format!(
                "brush of {} blocks exceeds the limit of {max}",
                writes.len()
            ));
        }
        let outcome = self.commit_writes(ctx, p, host.as_ref(), &writes)?;
        Ok(outcome.changed)
    }

    fn command_undo(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let count = parse_count(args)?;
        let host = host(ctx)?;
        let history = self.history(p.id);
        let mut done = 0;
        let mut changed = 0;
        for _ in 0..count {
            let Some(id) = history.undo.pop() else {
                break;
            };
            match host.undo(ctx.id, p.id, id) {
                Ok(EditOutcome {
                    changed: n,
                    undo_id,
                }) => {
                    changed += n;
                    done += 1;
                    if undo_id != 0 {
                        history.redo.push(undo_id);
                    }
                }
                Err(e) => {
                    history.undo.push(id);
                    return Err(e);
                }
            }
        }
        if done == 0 {
            return Err("Nothing to undo".to_string());
        }
        Ok(vec![format!(
            "Undid {done} edit(s); {changed} block(s) changed"
        )])
    }

    fn command_redo(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Result<Vec<String>, String> {
        let count = parse_count(args)?;
        let host = host(ctx)?;
        let limit = self.history_limit;
        let history = self.history(p.id);
        let mut done = 0;
        let mut changed = 0;
        for _ in 0..count {
            let Some(id) = history.redo.pop() else {
                break;
            };
            match host.undo(ctx.id, p.id, id) {
                Ok(EditOutcome {
                    changed: n,
                    undo_id,
                }) => {
                    changed += n;
                    done += 1;
                    if undo_id != 0 {
                        history.undo.push(undo_id);
                        while history.undo.len() > limit {
                            history.undo.remove(0);
                        }
                        while history.redo.len() > limit {
                            history.redo.remove(0);
                        }
                    }
                }
                Err(e) => {
                    history.redo.push(id);
                    return Err(e);
                }
            }
        }
        if done == 0 {
            return Err("Nothing to redo".to_string());
        }
        Ok(vec![format!(
            "Redid {done} edit(s); {changed} block(s) changed"
        )])
    }

    fn command_wand(&self, ctx: &mut NativeContext, p: &Player) -> Result<Vec<String>, String> {
        let leftover = ctx.backend.give_item(ctx.id, p.id, &self.wand, 1);
        if leftover > 0 {
            return Err("the inventory is full".to_string());
        }
        Ok(vec![format!("Given {}", self.wand)])
    }
}

impl NativePlugin for WorldEditPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        if let Some(wand) = ctx.backend.config(ctx.id, "wand-item") {
            let wand = wand.trim();
            if !wand.is_empty() {
                self.wand = normalize_item(wand);
            }
        }
        if let Some(item) = ctx.backend.config(ctx.id, "brush-item") {
            let item = item.trim();
            if !item.is_empty() {
                self.brush_item = normalize_item(item);
            }
        }
        if let Some(limit) = ctx.backend.config(ctx.id, "history-size") {
            if let Ok(limit) = limit.trim().parse::<usize>() {
                self.history_limit = limit.clamp(1, 1_000);
            }
        }
        ctx.log(LogLevel::Info, "worldedit plugin ready");
        Ok(())
    }

    fn shutdown(&mut self, ctx: &mut NativeContext) {
        ctx.log(LogLevel::Info, "worldedit plugin stopping");
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
        let held = ctx.backend.held_item(ctx.id, p.id);
        if self.wand_matches(held.clone()) {
            if let Ok(host) = host(ctx) {
                if let Ok(view) = view(host.as_ref(), ctx.id, p) {
                    self.selections.entry(p.id).or_default().set_point(
                        true,
                        &view.world,
                        [x, y, z],
                    );
                }
            }
            return Decision::Cancel(String::new());
        }
        if self.brush_matches(p.id, held) {
            return match self.apply_brush(ctx, p, x, y, z) {
                Ok(_) => Decision::Cancel(String::new()),
                Err(e) => Decision::Cancel(e),
            };
        }
        Decision::Allow
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
        let held = ctx.backend.held_item(ctx.id, p.id);
        if self.wand_matches(held.clone()) {
            if let Ok(host) = host(ctx) {
                if let Ok(view) = view(host.as_ref(), ctx.id, p) {
                    self.selections.entry(p.id).or_default().set_point(
                        false,
                        &view.world,
                        [x, y, z],
                    );
                }
            }
            return Decision::Cancel(String::new());
        }
        if self.brush_matches(p.id, held) {
            return match self.apply_brush(ctx, p, x, y, z) {
                Ok(_) => Decision::Cancel(String::new()),
                Err(e) => Decision::Cancel(e),
            };
        }
        Decision::Allow
    }

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        self.selections.remove(&p.id);
        self.histories.remove(&p.id);
        self.clipboards.remove(&p.id);
        self.brushes.remove(&p.id);
        self.masks.remove(&p.id);
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        let result = match name {
            "wand" => self.command_wand(ctx, p),
            "pos1" => self.set_position(ctx, p, args, true),
            "pos2" => self.set_position(ctx, p, args, false),
            "sel" => self.show_selection(p),
            "expand" => self.change_selection(ctx, p, args, true),
            "contract" => self.change_selection(ctx, p, args, false),
            "set" => self.command_set(ctx, p, args),
            "replace" => self.command_replace(ctx, p, args),
            "walls" => self.command_shape(ctx, p, args, Shape::Walls, "//walls <pattern>"),
            "outline" => self.command_shape(ctx, p, args, Shape::Outline, "//outline <pattern>"),
            "copy" => self.command_copy(ctx, p, args),
            "cut" => self.command_cut(ctx, p, args),
            "paste" => self.command_paste(ctx, p, args),
            "rotate" => self.command_rotate(ctx, p, args),
            "flip" => self.command_flip(ctx, p, args),
            "stack" => self.command_stack(ctx, p, args),
            "move" => self.command_move(ctx, p, args),
            "schem" => self.command_schem(ctx, p, args),
            "brush" => self.command_brush(ctx, p, args),
            "mask" => self.command_mask(ctx, p, args),
            "undo" => self.command_undo(ctx, p, args),
            "redo" => self.command_redo(ctx, p, args),
            _ => return None,
        };
        Some(match result {
            Ok(lines) => CommandResult {
                handled: true,
                lines,
            },
            Err(e) => CommandResult {
                handled: true,
                lines: vec![format!("Error: {e}")],
            },
        })
    }
}

#[cfg(test)]
mod tests;
