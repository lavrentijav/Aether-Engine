//! Player verbs: `/clear`, `/effect` and `/enchant` (CMD-10a).
//!
//! The grammar and the `en_us` strings are vanilla 1.21.11's:
//!
//! ```text
//! /clear [<targets>] [<item>] [<maxCount>]
//! /effect give <targets> <effect> [<seconds>|infinite] [<amplifier>] [<hideParticles>]
//! /effect clear <targets> [<effect>]
//! /enchant <targets> <enchantment> [<level>]
//! ```
//!
//! `/clear` works on players (the vanilla `EntityArgument.players()`), removes
//! at most `maxCount` matching items per target, and runs inside a
//! [`provenance::Scope`] so every unit it takes is journalled as a
//! `Command` destruction. `/effect` works on any living entity the selector
//! layer can find and reuses [`effects`] for the model and the packets.
//! `/enchant` follows vanilla's held-item rule: the main-hand stack of every
//! target, the enchantment's own maximum level, and an error when the item
//! cannot take it.

use std::sync::Arc;

use aether_api::Vector3;
use aether_world::journal::DestroyReason;

use super::{effects, enchantment, entities};
use crate::commands::feedback::lang;
use crate::commands::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use crate::commands::tree::{self, ArgKind, Builder, Ctx, Node, Requirement, Source};
use crate::commands::Reply;
use crate::inventory::SLOTS;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;

/// The permission gate every command in this group uses.
fn gamemaster() -> Requirement {
    Requirement::Level {
        op: 2,
        node: "minecraft.command.clear",
    }
}

fn effect_gate() -> Requirement {
    Requirement::Level {
        op: 2,
        node: "minecraft.command.effect",
    }
}

fn enchant_gate() -> Requirement {
    Requirement::Level {
        op: 2,
        node: "minecraft.command.enchant",
    }
}

/// Register the CMD-10a commands.
pub fn nodes() -> Vec<Node> {
    vec![clear_node(), effect_node(), enchant_node()]
}

fn clear_node() -> Node {
    let max = Builder::arg("maxCount", ArgKind::Integer).executable(exec_clear);
    let item = Builder::arg("item", ArgKind::String)
        .executable(exec_clear)
        .then(max);
    let targets = Builder::arg("targets", ArgKind::Entities)
        .executable(exec_clear)
        .then(item);
    Builder::literal("clear")
        .requires(gamemaster())
        .executable(exec_clear)
        .then(targets)
        .build()
}

fn amplifier_branch(infinite: bool) -> Builder {
    Builder::arg("amplifier", ArgKind::Integer)
        .executable(move |ctx| exec_effect_give_in(ctx, infinite))
        .then(
            Builder::arg("hideParticles", ArgKind::Bool)
                .executable(move |ctx| exec_effect_give_in(ctx, infinite)),
        )
}

fn effect_node() -> Node {
    // Vanilla declares `infinite` as a literal sibling of the `seconds`
    // argument; a plain word, not an enum argument.
    let infinite = Builder::literal("infinite")
        .executable(exec_effect_give_infinite)
        .then(amplifier_branch(true));
    let seconds = Builder::arg("seconds", ArgKind::Integer)
        .executable(exec_effect_give)
        .then(amplifier_branch(false));
    let effect = Builder::arg("effect", ArgKind::String)
        .executable(exec_effect_give)
        .then(seconds)
        .then(infinite);
    let give =
        Builder::literal("give").then(Builder::arg("targets", ArgKind::Entities).then(effect));
    let clear_effect = Builder::arg("effect", ArgKind::String).executable(exec_effect_clear);
    let clear = Builder::literal("clear").then(
        Builder::arg("targets", ArgKind::Entities)
            .executable(exec_effect_clear)
            .then(clear_effect),
    );
    Builder::literal("effect")
        .requires(effect_gate())
        .executable(exec_effect_usage)
        .then(give)
        .then(clear)
        .build()
}

fn enchant_node() -> Node {
    let level = Builder::arg("level", ArgKind::Integer).executable(exec_enchant);
    let enchantment = Builder::arg("enchantment", ArgKind::String)
        .executable(exec_enchant)
        .then(level);
    let targets = Builder::arg("targets", ArgKind::Entities)
        .executable(exec_enchant)
        .then(enchantment);
    Builder::literal("enchant")
        .requires(enchant_gate())
        .executable(exec_enchant)
        .then(targets)
        .build()
}

// ---------------------------------------------------------------------------
// Shared argument and target helpers
// ---------------------------------------------------------------------------

fn text_arg<'a, 'b, 'e>(ctx: &'a Ctx<'b, 'e>, i: usize) -> Option<&'a str> {
    match &ctx.args.get(i)?.value {
        tree::Value::Text(s) | tree::Value::ResourceLocation(s) => Some(s),
        _ => None,
    }
}

fn int_arg(ctx: &Ctx, i: usize) -> Option<i32> {
    match ctx.args.get(i)?.value {
        tree::Value::Int(n) => Some(n),
        _ => None,
    }
}

fn player_ref(handle: &PlayerHandle) -> EntityRef {
    let pos = handle.pos();
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: crate::commands::coordinates::Rotation::new(pos.yaw, pos.pitch),
        tags: Vec::new(),
        team: None,
        gamemode: Some(Gamemode::from(handle.game().mode)),
        level: 0,
        scores: Vec::new(),
        advancements: Vec::new(),
        predicates: Vec::new(),
        nbt: crate::protocol::nbt::Nbt::Compound(Vec::new()),
    }
}

/// The live registry and entity list, as the CMD-06b selector layer sees it.
struct LiveSource<'a> {
    registry: &'a SharedRegistry,
    executor: Option<EntityRef>,
}

impl SelectorSource for LiveSource<'_> {
    fn executor(&self) -> Option<EntityRef> {
        self.executor.clone()
    }

    fn players(&self) -> Vec<EntityRef> {
        self.registry
            .snapshot()
            .iter()
            .map(|handle| player_ref(handle))
            .collect()
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities::selector_snapshot()
            .into_iter()
            .map(|(id, uuid, kind, pos, alive)| {
                let mut e = EntityRef::other(kind, id, pos);
                e.uuid = uuid;
                e.alive = alive;
                e
            })
            .collect()
    }
}

fn live_source<'a>(registry: &'a SharedRegistry, source: Source<'a>) -> LiveSource<'a> {
    LiveSource {
        registry,
        executor: source.player().map(player_ref),
    }
}

fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// One resolved target of `/effect` or `/enchant`.
enum Target {
    Player(Arc<PlayerHandle>),
    Mob { id: i32, name: String },
    Other { name: String },
}

impl Target {
    fn name(&self) -> &str {
        match self {
            Target::Player(h) => &h.name,
            Target::Mob { name, .. } | Target::Other { name } => name,
        }
    }
}

/// Resolve a target token to entities: a selector, or a single online player.
fn resolve_entities(
    text: &str,
    source: Source<'_>,
    registry: &SharedRegistry,
) -> Result<Vec<Target>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No entity was found".into());
    }
    if !text.starts_with('@') {
        return registry
            .snapshot()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(text))
            .map(|p| vec![Target::Player(p)])
            .ok_or_else(|| "No player was found".to_string());
    }
    let selector = Selector::parse(text).map_err(|e| e.message())?;
    let live = live_source(registry, source);
    let found = selector
        .select(&live, &mut LcgRng::new(seed()))
        .map_err(|e| e.message())?;
    let mut out = Vec::with_capacity(found.len());
    for entity in found {
        if entity.is_player {
            let handle = registry.by_uuid(entity.uuid).or_else(|| {
                let name = entity.name.clone().unwrap_or_default();
                registry
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&name))
            });
            match handle {
                Some(handle) => out.push(Target::Player(handle)),
                None => return Err("No player was found".into()),
            }
        } else if entities::is_mob(entity.id) {
            out.push(Target::Mob {
                id: entity.id,
                name: entity.stringified_name(),
            });
        } else {
            out.push(Target::Other {
                name: entity.stringified_name(),
            });
        }
    }
    if out.is_empty() {
        return Err("No entity was found".into());
    }
    Ok(out)
}

/// Resolve a player-only target token (`/clear`'s `targets`).
fn resolve_players(
    text: &str,
    source: Source<'_>,
    registry: &SharedRegistry,
) -> Result<Vec<Arc<PlayerHandle>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No player was found".into());
    }
    if !text.starts_with('@') {
        return registry
            .snapshot()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(text))
            .map(|p| vec![p])
            .ok_or_else(|| "No player was found".to_string());
    }
    let selector = Selector::parse_players(text).map_err(|e| e.message())?;
    let live = live_source(registry, source);
    let found = selector
        .select(&live, &mut LcgRng::new(seed()))
        .map_err(|e| e.message())?;
    let mut out = Vec::with_capacity(found.len());
    for entity in found {
        let handle = registry.by_uuid(entity.uuid).or_else(|| {
            let name = entity.name.clone().unwrap_or_default();
            registry
                .snapshot()
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(&name))
        });
        match handle {
            Some(handle) => out.push(handle),
            None => return Err("No player was found".into()),
        }
    }
    if out.is_empty() {
        return Err("No player was found".into());
    }
    Ok(out)
}

fn int_range(n: i32, min: i32, max: Option<i32>) -> Result<(), String> {
    if n < min {
        return Err(lang::format(
            lang::ARG_INTEGER_LOW,
            &[&min.to_string(), &n.to_string()],
        ));
    }
    if let Some(max) = max {
        if n > max {
            return Err(lang::format(
                lang::ARG_INTEGER_BIG,
                &[&max.to_string(), &n.to_string()],
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// /clear
// ---------------------------------------------------------------------------

fn exec_clear(ctx: &mut Ctx) -> Reply {
    let target_text = text_arg(ctx, 0).map(str::to_owned);
    let item_text = text_arg(ctx, 1).map(str::to_owned);
    let max_count = int_arg(ctx, 2);
    if let Some(n) = max_count {
        if let Err(e) = int_range(n, 0, None) {
            return ctx.fail(e);
        }
    }

    let targets = match target_text {
        Some(text) => match resolve_players(&text, ctx.source, ctx.registry) {
            Ok(v) => v,
            Err(e) => return ctx.fail(e),
        },
        None => match ctx.player() {
            Some(handle) => match ctx.registry.by_uuid(handle.uuid) {
                Some(handle) => vec![handle],
                None => return ctx.fail("No player was found"),
            },
            None => return ctx.needs_player(),
        },
    };

    let filter = match item_text {
        Some(text) => match parse_item_filter(&text, ctx.world) {
            Ok(filter) => filter,
            Err(e) => return ctx.fail(e),
        },
        None => ItemFilter::Any,
    };
    let max = max_count.unwrap_or(-1);

    let mut total = 0u64;
    for handle in &targets {
        total += clear_one(handle, &filter, max, ctx.world);
    }
    if total == 0 {
        return ctx.fail(if targets.len() == 1 {
            lang::format(lang::CLEAR_FAILED_SINGLE, &[&targets[0].name])
        } else {
            lang::format(lang::CLEAR_FAILED_MULTIPLE, &[&targets.len().to_string()])
        });
    }
    let line = if targets.len() == 1 {
        lang::format(
            lang::CLEAR_SUCCESS_SINGLE,
            &[&total.to_string(), &targets[0].name],
        )
    } else {
        lang::format(
            lang::CLEAR_SUCCESS_MULTIPLE,
            &[&total.to_string(), &targets.len().to_string()],
        )
    };
    Reply::one(line)
}

/// The item filter of `/clear`: every item, one item, or the members of an
/// item tag (`#minecraft:…`) from the loaded data pack. A component patch
/// (after `[`/`{`) is accepted syntactically but not tested, because the stack
/// model carries no components yet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemFilter {
    Any,
    Items(Vec<aether_world::ItemTypeId>),
}

impl ItemFilter {
    fn matches(&self, item: aether_world::ItemTypeId) -> bool {
        match self {
            ItemFilter::Any => true,
            ItemFilter::Items(ids) => ids.contains(&item),
        }
    }
}

fn parse_item_filter(text: &str, world: &DemoWorld) -> Result<ItemFilter, String> {
    let stem = text.split(['[', '{']).next().unwrap_or(text).trim();
    if stem == "*" {
        return Ok(ItemFilter::Any);
    }
    if stem.starts_with('#') {
        return super::recipes::item_tag(stem)
            .map(|ids| ItemFilter::Items(ids.to_vec()))
            .ok_or_else(|| format!("Unknown item tag '{stem}'"));
    }
    let name = if stem.contains(':') {
        stem.to_string()
    } else {
        format!("minecraft:{stem}")
    };
    world
        .item_id(&name)
        .map(|id| ItemFilter::Items(vec![id]))
        .ok_or_else(|| format!("Unknown item '{text}'"))
}

/// Remove up to `max` matching items from one player, journalling the
/// destruction, and return how many units left.
fn clear_one(handle: &PlayerHandle, filter: &ItemFilter, max: i32, world: &DemoWorld) -> u64 {
    if max == 0 {
        return 0;
    }
    let scope = super::provenance::Scope::begin(handle, world);
    super::provenance::note_destroy_reason(DestroyReason::Command);
    let mut removed = 0u64;
    {
        let mut inv = handle.inventory();
        for slot in 0..SLOTS {
            if max >= 0 && removed >= max as u64 {
                break;
            }
            let Some(stack) = inv.slot(slot) else {
                continue;
            };
            if !filter.matches(stack.item) {
                continue;
            }
            let take = if max < 0 {
                stack.count as u64
            } else {
                (max as u64 - removed).min(stack.count as u64)
            };
            if take == 0 {
                break;
            }
            removed += take;
            let left = stack.count - take as u8;
            if left == 0 {
                inv.put_slot(slot, None);
            } else {
                // Split the survivors into a fresh instance. The provenance
                // layer represents a partial take as the old uid destroyed
                // and the remainder minted, so the units that `/clear` ate are
                // journalled as a `Command` destruction rather than a silent
                // same-slot shrink.
                let mut rest = stack.clone();
                rest.count = left;
                rest.uid = aether_world::journal::ledger::mint_uid();
                inv.put_slot(slot, Some(rest));
            }
        }
    }
    drop(scope);
    handle.sync_inventory(world);
    removed
}

// ---------------------------------------------------------------------------
// /effect
// ---------------------------------------------------------------------------

fn exec_effect_usage(ctx: &mut Ctx) -> Reply {
    ctx.fail("Unknown or incomplete command")
}

fn exec_effect_give(ctx: &mut Ctx) -> Reply {
    exec_effect_give_in(ctx, false)
}

fn exec_effect_give_infinite(ctx: &mut Ctx) -> Reply {
    exec_effect_give_in(ctx, true)
}

fn exec_effect_give_in(ctx: &mut Ctx, infinite: bool) -> Reply {
    let target_text = text_arg(ctx, 0).map(str::to_owned);
    let effect_text = text_arg(ctx, 1).map(str::to_owned);
    let (Some(target_text), Some(effect_text)) = (target_text, effect_text) else {
        return ctx.fail("Unknown or incomplete command");
    };
    let Some((id, display)) = effects::by_name(&effect_text) else {
        return ctx.fail(format!("Unknown effect '{effect_text}'"));
    };

    // With `infinite` the first integer after the effect is the amplifier;
    // otherwise it is `seconds` and the next one is the amplifier.
    let mut at = 2usize;
    let mut seconds: Option<i32> = None;
    if !infinite {
        if let Some(tree::Value::Int(n)) = ctx.args.get(at).map(|a| &a.value) {
            seconds = Some(*n);
            at += 1;
        }
    }
    let mut amplifier = 0i32;
    if let Some(tree::Value::Int(n)) = ctx.args.get(at).map(|a| &a.value) {
        amplifier = *n;
        at += 1;
    }
    let hide = matches!(
        ctx.args.get(at).map(|a| &a.value),
        Some(tree::Value::Bool(true))
    );
    if let Some(s) = seconds {
        if let Err(e) = int_range(s, 1, Some(1_000_000)) {
            return ctx.fail(e);
        }
    }
    if let Err(e) = int_range(amplifier, 0, Some(255)) {
        return ctx.fail(e);
    }

    let duration = if infinite {
        -1
    } else {
        match seconds {
            Some(s) => s.saturating_mul(20),
            None if effects::is_instant(id) => 1,
            None => 600,
        }
    };
    let mut effect = effects::Effect::new(id, amplifier as u8, duration);
    if hide {
        effect = effect.hidden();
    }

    let targets = match resolve_entities(&target_text, ctx.source, ctx.registry) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let mut applied = 0usize;
    for target in &targets {
        let ok = match target {
            Target::Player(handle) => effects::try_give(handle, ctx.registry, ctx.world, effect),
            Target::Mob { id, .. } => {
                if let Some(shown) = entities::apply_mob_effect(*id, effect) {
                    let ev = effects::event(*id, shown);
                    ctx.registry.broadcast_tracking(*id, &ev, ctx.world);
                    true
                } else {
                    false
                }
            }
            Target::Other { .. } => false,
        };
        if ok {
            applied += 1;
        }
    }
    if applied == 0 {
        return ctx.fail(lang::EFFECT_GIVE_FAILED);
    }
    let line = if targets.len() == 1 {
        lang::format(
            lang::EFFECT_GIVE_SUCCESS_SINGLE,
            &[display, targets[0].name()],
        )
    } else {
        lang::format(
            lang::EFFECT_GIVE_SUCCESS_MULTIPLE,
            &[display, &targets.len().to_string()],
        )
    };
    Reply::one(line)
}

fn exec_effect_clear(ctx: &mut Ctx) -> Reply {
    let target_text = text_arg(ctx, 0).map(str::to_owned);
    let effect_text = text_arg(ctx, 1).map(str::to_owned);
    let Some(target_text) = target_text else {
        return ctx.fail("Unknown or incomplete command");
    };
    let targets = match resolve_entities(&target_text, ctx.source, ctx.registry) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };

    match effect_text {
        Some(text) => {
            let Some((id, display)) = effects::by_name(&text) else {
                return ctx.fail(format!("Unknown effect '{text}'"));
            };
            let mut removed = 0usize;
            for target in &targets {
                if target_has_effect(target, id) {
                    remove_target_effect(target, id, ctx.registry, ctx.world);
                    removed += 1;
                }
            }
            if removed == 0 {
                return ctx.fail(lang::EFFECT_CLEAR_SPECIFIC_FAILED);
            }
            let line = if targets.len() == 1 {
                lang::format(
                    lang::EFFECT_CLEAR_SPECIFIC_SUCCESS_SINGLE,
                    &[display, targets[0].name()],
                )
            } else {
                lang::format(
                    lang::EFFECT_CLEAR_SPECIFIC_SUCCESS_MULTIPLE,
                    &[display, &targets.len().to_string()],
                )
            };
            Reply::one(line)
        }
        None => {
            let mut removed = 0usize;
            for target in &targets {
                if target_has_any_effect(target) {
                    clear_target_effects(target, ctx.registry, ctx.world);
                    removed += 1;
                }
            }
            if removed == 0 {
                return ctx.fail(lang::EFFECT_CLEAR_EVERYTHING_FAILED);
            }
            let line = if targets.len() == 1 {
                lang::format(
                    lang::EFFECT_CLEAR_EVERYTHING_SUCCESS_SINGLE,
                    &[targets[0].name()],
                )
            } else {
                lang::format(
                    lang::EFFECT_CLEAR_EVERYTHING_SUCCESS_MULTIPLE,
                    &[&targets.len().to_string()],
                )
            };
            Reply::one(line)
        }
    }
}

fn target_has_effect(target: &Target, id: u8) -> bool {
    match target {
        Target::Player(handle) => handle.game().effects.active(id),
        Target::Mob { id: entity, .. } => entities::mob_has_effect(*entity, id),
        Target::Other { .. } => false,
    }
}

fn target_has_any_effect(target: &Target) -> bool {
    match target {
        Target::Player(handle) => !handle.game().effects.is_empty(),
        Target::Mob { id, .. } => entities::mob_has_effects(*id),
        Target::Other { .. } => false,
    }
}

fn remove_target_effect(target: &Target, id: u8, registry: &SharedRegistry, world: &DemoWorld) {
    match target {
        Target::Player(handle) => effects::remove(handle, registry, world, id),
        Target::Mob { id: entity, .. } => {
            if entities::remove_mob_effect(*entity, id) {
                let ev = crate::protocol::ServerEvent::RemoveMobEffect {
                    entity_id: *entity,
                    effect: id,
                };
                registry.broadcast_tracking(*entity, &ev, world);
            }
        }
        Target::Other { .. } => {}
    }
}

fn clear_target_effects(target: &Target, registry: &SharedRegistry, world: &DemoWorld) {
    match target {
        Target::Player(handle) => effects::clear(handle, registry, world),
        Target::Mob { id, .. } => {
            for effect in entities::clear_mob_effects(*id) {
                let ev = crate::protocol::ServerEvent::RemoveMobEffect {
                    entity_id: *id,
                    effect,
                };
                registry.broadcast_tracking(*id, &ev, world);
            }
        }
        Target::Other { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// /enchant
// ---------------------------------------------------------------------------

fn exec_enchant(ctx: &mut Ctx) -> Reply {
    let target_text = text_arg(ctx, 0).map(str::to_owned);
    let enchant_text = text_arg(ctx, 1).map(str::to_owned);
    let level = int_arg(ctx, 2).unwrap_or(1);
    let (Some(target_text), Some(enchant_text)) = (target_text, enchant_text) else {
        return ctx.fail("Unknown or incomplete command");
    };
    // Vanilla's `EnchantCommand` argument is `integer(0)`: level 0 is legal and
    // removes the enchantment, while an absent level defaults to 1. Anything
    // above the enchantment's own maximum is the `failed.level` error below.
    if let Err(e) = int_range(level, 0, None) {
        return ctx.fail(e);
    }
    let Some(def) = enchantment::by_name(&enchant_text) else {
        return ctx.fail(format!("Unknown enchantment '{enchant_text}'"));
    };
    if level > def.max_level as i32 {
        return ctx.fail(lang::format(
            lang::ENCHANT_FAILED_LEVEL,
            &[&level.to_string(), &def.max_level.to_string()],
        ));
    }
    let targets = match resolve_entities(&target_text, ctx.source, ctx.registry) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };

    let mut enchanted = 0usize;
    for target in &targets {
        match target {
            Target::Player(handle) => {
                let index = handle.inventory().held_index();
                let held = handle.inventory().slot(index).cloned();
                let Some(stack) = held else {
                    return ctx.fail(lang::format(lang::ENCHANT_FAILED_ITEMLESS, &[&handle.name]));
                };
                let item_name = ctx
                    .world
                    .item_name(stack.item)
                    .unwrap_or_else(|| stack.item.vanilla_name().unwrap_or_default().to_string());
                // Creative is exempt from the compatibility check, exactly as
                // vanilla's `hasInfiniteMaterials`. A pack's real
                // `enchantable/…` tag is authoritative; the compiled category
                // is the fallback without one.
                let compatible = handle.game().mode == crate::protocol::GameMode::Creative
                    || match super::recipes::item_tag(&def.tag()) {
                        Some(ids) => ids.contains(&stack.item),
                        None => enchantment::can_enchant(&item_name, &def),
                    };
                if !compatible {
                    return ctx.fail(lang::format(
                        lang::ENCHANT_FAILED_INCOMPATIBLE,
                        &[&enchantment::item_display(&item_name)],
                    ));
                }
                {
                    let mut inv = handle.inventory();
                    if let Some(s) = inv.slot_mut(index).and_then(|s| s.as_mut()) {
                        if s.uid == stack.uid {
                            if level == 0 {
                                s.remove_enchantment(def.id);
                            } else {
                                s.add_enchantment(def.id, level as u8);
                            }
                        }
                    }
                }
                handle.sync_inventory(ctx.world);
                enchanted += 1;
            }
            Target::Mob { name, .. } => {
                return ctx.fail(lang::format(lang::ENCHANT_FAILED_ITEMLESS, &[name]));
            }
            Target::Other { name } => {
                return ctx.fail(lang::format(lang::ENCHANT_FAILED_ENTITY, &[name]));
            }
        }
    }
    if enchanted == 0 {
        return ctx.fail(lang::ENCHANT_FAILED);
    }
    let line = if targets.len() == 1 {
        lang::format(
            lang::ENCHANT_SUCCESS_SINGLE,
            &[def.display, targets[0].name()],
        )
    } else {
        lang::format(
            lang::ENCHANT_SUCCESS_MULTIPLE,
            &[def.display, &targets.len().to_string()],
        )
    };
    Reply::one(line)
}
