mod pattern;
mod selection;

use std::collections::HashMap;
use std::sync::Arc;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::worldedit::{self, EditOutcome, PlayerView, WorldEditHost};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};
use pattern::{parse_filter, parse_pattern, Filter, Pattern};
use selection::{contract, expand, volume, Direction, Selection};

pub const ID: &str = "worldedit";
pub const NODE_WAND: &str = "worldedit.wand";
pub const NODE_SELECTION: &str = "worldedit.selection";
pub const NODE_REGION_SET: &str = "worldedit.region.set";
pub const NODE_REGION_REPLACE: &str = "worldedit.region.replace";
pub const NODE_REGION_WALLS: &str = "worldedit.region.walls";
pub const NODE_REGION_OUTLINE: &str = "worldedit.region.outline";
pub const NODE_HISTORY_UNDO: &str = "worldedit.history.undo";
pub const NODE_HISTORY_REDO: &str = "worldedit.history.redo";
pub const DEFAULT_WAND: &str = "minecraft:wooden_axe";
const DEFAULT_HISTORY: usize = 20;

fn detailed(name: &str, node: &str) -> CommandDecl {
    CommandDecl::Detailed {
        name: name.to_string(),
        node: node.to_string(),
        op: 0,
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
        capabilities: vec![
            "log".to_string(),
            "config".to_string(),
            "supports".to_string(),
            "storage.kv".to_string(),
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
            detailed("wand", NODE_WAND),
            detailed("pos1", NODE_SELECTION),
            detailed("pos2", NODE_SELECTION),
            detailed("sel", NODE_SELECTION),
            detailed("expand", NODE_SELECTION),
            detailed("contract", NODE_SELECTION),
            detailed("set", NODE_REGION_SET),
            detailed("replace", NODE_REGION_REPLACE),
            detailed("walls", NODE_REGION_WALLS),
            detailed("outline", NODE_REGION_OUTLINE),
            detailed("undo", NODE_HISTORY_UNDO),
            detailed("redo", NODE_HISTORY_REDO),
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
    history_limit: usize,
    selections: HashMap<u64, Selection>,
    histories: HashMap<u64, History>,
}

impl Default for WorldEditPlugin {
    fn default() -> Self {
        WorldEditPlugin {
            wand: DEFAULT_WAND.to_string(),
            history_limit: DEFAULT_HISTORY,
            selections: HashMap::new(),
            histories: HashMap::new(),
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

impl WorldEditPlugin {
    fn wand_matches(&self, item: Option<String>) -> bool {
        item.is_some_and(|item| normalize_item(&item) == self.wand)
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
                            match pattern.pick(host, ctx.id, x, y, z) {
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
        if !self.wand_matches(ctx.backend.held_item(ctx.id, p.id)) {
            return Decision::Allow;
        }
        if let Ok(host) = host(ctx) {
            if let Ok(view) = view(host.as_ref(), ctx.id, p) {
                self.selections
                    .entry(p.id)
                    .or_default()
                    .set_point(true, &view.world, [x, y, z]);
            }
        }
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
        if !self.wand_matches(ctx.backend.held_item(ctx.id, p.id)) {
            return Decision::Allow;
        }
        if let Ok(host) = host(ctx) {
            if let Ok(view) = view(host.as_ref(), ctx.id, p) {
                self.selections
                    .entry(p.id)
                    .or_default()
                    .set_point(false, &view.world, [x, y, z]);
            }
        }
        Decision::Cancel(String::new())
    }

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        self.selections.remove(&p.id);
        self.histories.remove(&p.id);
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
