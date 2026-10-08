//! What a player does with their hands: mining, using items, throwing them,
//! opening blocks, hitting and using entities.
//!
//! Called from the connection thread for clients whose codec runs the full
//! game (see [`crate::protocol::ProtocolCodec::full_gameplay`]).

use aether_api::{block_ids, Vector3};
use aether_world::BlockStateId;

use super::containers::ContainerKind;
use super::player::WindowKind;
use super::{combat, entities, tables, window};
use crate::inventory::Stack;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::{GameMode, MetaValue, ServerEvent};
use crate::session::DemoWorld;

/// A dig in progress.
#[derive(Debug, Clone, Copy)]
pub struct Dig {
    pub pos: (i32, i32, i32),
    pub start: u64,
    pub ticks: u32,
}

fn block_name(world: &DemoWorld, x: i32, y: i32, z: i32) -> String {
    crate::game::block_name(world, world.get_block(x, y, z)).unwrap_or_default()
}

/// The player started mining. Returns `true` when the block should break
/// right now (creative, or a block that breaks instantly).
pub fn start_dig(handle: &PlayerHandle, world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    let name = block_name(world, x, y, z);
    let mut st = handle.game();
    if st.dead {
        return false;
    }
    if !st.survival() {
        return true;
    }
    let held = handle.held_item();
    let pos = handle.pos();
    let in_water = entities::in_water(world, Vector3::new(pos.x, pos.y + 1.5, pos.z));
    match tables::dig_ticks(&name, held.as_deref(), pos.on_ground, in_water) {
        None => false,
        Some(0) => true,
        Some(ticks) => {
            st.digging = Some(((x, y, z), super::now()));
            DIGS.lock().unwrap().insert(
                handle.entity_id,
                Dig {
                    pos: (x, y, z),
                    start: super::now(),
                    ticks,
                },
            );
            false
        }
    }
}

static DIGS: std::sync::Mutex<std::collections::BTreeMap<i32, Dig>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The client says it finished mining. Whether that is believable.
pub fn finish_dig(handle: &PlayerHandle, x: i32, y: i32, z: i32) -> bool {
    let dig = DIGS.lock().unwrap().remove(&handle.entity_id);
    handle.game().digging = None;
    if !handle.game().survival() {
        return true;
    }
    match dig {
        Some(d) if d.pos == (x, y, z) => {
            // Half the expected time, to allow for latency and the client's
            // own rounding; a client that is far faster than that is cheating.
            let took = super::now().saturating_sub(d.start);
            took * 2 + 4 >= d.ticks as u64
        }
        _ => false,
    }
}

/// Gave up mining: clear the cracks for everyone.
pub fn cancel_dig(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) {
    handle.game().digging = None;
    if let Some(d) = DIGS.lock().unwrap().remove(&handle.entity_id) {
        let (x, y, z) = d.pos;
        registry.broadcast_except(
            handle.entity_id,
            &ServerEvent::BreakAnimation {
                entity_id: handle.entity_id,
                x,
                y,
                z,
                stage: -1,
            },
            world,
        );
    }
}

/// Show everyone else the cracks on blocks being mined.
pub fn dig_progress(registry: &SharedRegistry, world: &DemoWorld) {
    let digs: Vec<(i32, Dig)> = DIGS.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect();
    let now = super::now();
    for (eid, d) in digs {
        let stage = ((now.saturating_sub(d.start) as f32 / d.ticks.max(1) as f32) * 10.0) as i8;
        let (x, y, z) = d.pos;
        registry.broadcast_except(
            eid,
            &ServerEvent::BreakAnimation {
                entity_id: eid,
                x,
                y,
                z,
                stage: stage.min(9),
            },
            world,
        );
    }
}

/// A block was broken by a player running the full game: drops, wear,
/// containers, the second half of a door.
pub fn on_broken(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
    broken: BlockStateId,
    cause: Option<u64>,
) {
    let Some(name) = crate::game::block_name(world, broken) else {
        return;
    };
    registry.broadcast_except(
        handle.entity_id,
        &ServerEvent::WorldEvent {
            event: 2001,
            x,
            y,
            z,
            data: broken.raw() as i32, // the codec translates it
        },
        world,
    );
    registry.broadcast_except(
        handle.entity_id,
        &ServerEvent::BreakAnimation {
            entity_id: handle.entity_id,
            x,
            y,
            z,
            stage: -1,
        },
        world,
    );
    if let Some((dy, _)) = crate::placement::other_half(broken) {
        if world.get_block(x, y + dy, z) != block_ids::AIR {
            crate::session::set_and_broadcast(
                world,
                registry,
                handle,
                x,
                y + dy,
                z,
                block_ids::AIR,
            );
        }
    }
    if let Some(kind) = ContainerKind::of_block(&name) {
        window::close_viewers((x, y, z), world);
        let items = super::containers::remove(world, (x, y, z), kind);
        entities::spill(x, y, z, items, aether_world::journal::ActorId(handle.uuid));
    }
    let survival = handle.game().survival();
    if !survival {
        return;
    }
    let held = handle.held_item();
    let drops = entities::with_rng(|r| tables::block_drops(&name, held.as_deref(), r));
    entities::drop_at_block(
        x,
        y,
        z,
        drops.into_iter().map(|(i, n)| Stack::new(&i, n)).collect(),
        aether_world::journal::ActorId(handle.uuid),
        cause,
    );
    handle.game().exhaust(0.005);
    let hard = tables::block_info(&name).map(|i| i.hardness).unwrap_or(0.0);
    if hard > 0.0 {
        let n = if held.as_deref().is_some_and(|h| h.ends_with("_sword")) {
            2
        } else {
            1
        };
        combat::wear_held(handle, world, n);
    }
}

/// Right-click on a block that opens something. Returns `true` if it did.
pub fn use_block(handle: &PlayerHandle, world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    let name = block_name(world, x, y, z);
    let sneaking = handle.game().sneaking;
    if sneaking && handle.held_item().is_some() {
        return false;
    }
    let short = name.strip_prefix("minecraft:").unwrap_or(&name);
    if short == "crafting_table" {
        window::open(
            handle,
            world,
            WindowKind::Crafting {
                grid: vec![None; 9],
            },
        );
        return true;
    }
    match ContainerKind::of_block(&name) {
        Some(ContainerKind::Chest) => {
            window::open(handle, world, WindowKind::Chest { pos: (x, y, z) });
            return true;
        }
        Some(ContainerKind::Furnace) => {
            window::open(handle, world, WindowKind::Furnace { pos: (x, y, z) });
            return true;
        }
        None => {}
    }
    if short.ends_with("_bed") {
        let pos = handle.pos();
        handle.game().spawn = (x as f64 + 0.5, y as f64 + 1.0, z as f64 + 0.5);
        let _ = pos;
        if super::is_night() {
            super::set_time_of_day(0);
            if let Some(r) = super::registry() {
                r.broadcast(&super::time_event(), world);
                r.broadcast(
                    &ServerEvent::Chat(format!("{} slept through the night", handle.name)),
                    world,
                );
            }
        } else {
            handle.emit(&ServerEvent::Chat("Respawn point set".into()), world);
        }
        return true;
    }
    false
}

/// Using an item on a block that does not place it: hoes, flint and steel.
/// Returns the block to put at `(x, y, z)` (the clicked one) if any.
pub fn item_on_block(item: &str, clicked: &str) -> Option<&'static str> {
    let short = item.strip_prefix("minecraft:")?;
    if short.ends_with("_hoe")
        && matches!(
            clicked,
            "minecraft:dirt" | "minecraft:grass_block" | "minecraft:dirt_path"
        )
    {
        return Some("minecraft:farmland");
    }
    if short.ends_with("_shovel") && clicked == "minecraft:grass_block" {
        return Some("minecraft:dirt_path");
    }
    None
}

/// Consume what a successful placement used, in survival.
pub fn consumed_by_placement(handle: &PlayerHandle, world: &DemoWorld) {
    if !handle.game().survival() {
        return;
    }
    {
        let mut inv = handle.inventory();
        let i = inv.held_index();
        let Some(Some(s)) = inv.slot_mut(i) else {
            return;
        };
        if s.item.ends_with("_bucket") && s.item != "minecraft:milk_bucket" {
            *s = Stack::new("minecraft:bucket", 1);
        } else {
            s.count -= 1;
            if s.count == 0 {
                inv.put_slot(i, None);
            }
        }
    }
    handle.sync_inventory(world);
    crate::session::save_inventory(handle, world);
}

/// A block position.
type Cell = (i32, i32, i32);

/// Walk along the player's look for up to `reach` blocks; the first cell
/// `stop` accepts, and the cell before it.
fn raycast(
    handle: &PlayerHandle,
    reach: f64,
    mut stop: impl FnMut(i32, i32, i32) -> bool,
) -> Option<(Cell, Cell)> {
    let p = handle.pos();
    let (yaw, pitch) = (p.yaw.to_radians() as f64, p.pitch.to_radians() as f64);
    let dir = (
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    );
    let eye = (p.x, p.y + 1.62, p.z);
    let mut prev = (
        eye.0.floor() as i32,
        eye.1.floor() as i32,
        eye.2.floor() as i32,
    );
    let steps = (reach * 20.0) as i32;
    for i in 1..=steps {
        let t = i as f64 / 20.0;
        let c = (
            (eye.0 + dir.0 * t).floor() as i32,
            (eye.1 + dir.1 * t).floor() as i32,
            (eye.2 + dir.2 * t).floor() as i32,
        );
        if c != prev {
            if stop(c.0, c.1, c.2) {
                return Some((c, prev));
            }
            prev = c;
        }
    }
    None
}

/// Right-click with an item, not at a block.
pub fn use_item(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld, hand: u8) {
    let item = {
        let inv = handle.inventory();
        let slot = if hand == 1 {
            crate::inventory::OFFHAND
        } else {
            inv.held_index()
        };
        inv.slot(slot).map(|s| s.item.clone())
    };
    let Some(item) = item else { return };
    let now = super::now();
    if let Some(_food) = tables::food(&item) {
        let mut st = handle.game();
        let always = matches!(
            item.as_str(),
            "minecraft:golden_apple"
                | "minecraft:enchanted_golden_apple"
                | "minecraft:chorus_fruit"
        );
        if st.food < 20 || always || !st.survival() {
            st.eating = Some((now, hand));
        }
        return;
    }
    match item.as_str() {
        "minecraft:milk_bucket" => {
            handle.game().eating = Some((now, hand));
        }
        "minecraft:bow" => {
            let has_arrow =
                !handle.game().survival() || handle.inventory().count_of("minecraft:arrow") > 0;
            if has_arrow {
                handle.game().drawing = Some(now);
            }
        }
        "minecraft:bucket" => {
            let hit = raycast(handle, 5.0, |x, y, z| {
                world.get_block(x, y, z) != block_ids::AIR
            });
            let Some(((x, y, z), _)) = hit else { return };
            let name = block_name(world, x, y, z);
            let filled = match name.as_str() {
                "minecraft:water" => "minecraft:water_bucket",
                "minecraft:lava" => "minecraft:lava_bucket",
                _ => return,
            };
            crate::session::set_and_broadcast(world, registry, handle, x, y, z, block_ids::AIR);
            if handle.game().survival() {
                let mut inv = handle.inventory();
                let i = inv.held_index();
                let one = inv.slot(i).map(|s| s.count).unwrap_or(0) == 1;
                if one {
                    inv.put_slot(i, Some(Stack::new(filled, 1)));
                } else {
                    inv.consume_held();
                    if let Some(rest) = inv.insert(Stack::new(filled, 1)) {
                        drop(inv);
                        entities::throw_from(handle, rest);
                    }
                }
            }
            handle.sync_inventory(world);
        }
        _ => {}
    }
}

/// Stopped using the held item: a drawn bow fires.
pub fn release_use(handle: &PlayerHandle, world: &DemoWorld) {
    let drawn = {
        let mut st = handle.game();
        st.eating = None;
        st.drawing.take()
    };
    let Some(start) = drawn else { return };
    let t = super::now().saturating_sub(start) as f64 / 20.0;
    let power = ((t * t + t * 2.0) / 3.0).min(1.0);
    if power < 0.1 {
        return;
    }
    let survival = handle.game().survival();
    if survival && handle.inventory().take("minecraft:arrow", 1) == 0 {
        return;
    }
    let p = handle.pos();
    let (yaw, pitch) = (p.yaw.to_radians() as f64, p.pitch.to_radians() as f64);
    let speed = power * 3.0;
    let vel = Vector3::new(
        -yaw.sin() * pitch.cos() * speed,
        -pitch.sin() * speed,
        yaw.cos() * pitch.cos() * speed,
    );
    entities::spawn_arrow(
        Vector3::new(p.x, p.y + 1.5, p.z),
        vel,
        Some(handle.entity_id),
        true,
        2.0,
    );
    if let Some(r) = super::registry() {
        r.broadcast(
            &ServerEvent::Sound {
                name: "minecraft:entity.arrow.shoot",
                category: 7,
                x: p.x,
                y: p.y,
                z: p.z,
                volume: 1.0,
                pitch: 1.0,
            },
            world,
        );
    }
    if survival {
        combat::wear_held(handle, world, 1);
    } else {
        handle.sync_inventory(world);
    }
}

/// Q: throw one of the held stack, or all of it.
pub fn drop_held(handle: &PlayerHandle, world: &DemoWorld, all: bool) {
    let thrown = {
        let mut inv = handle.inventory();
        let i = inv.held_index();
        if all {
            inv.take_slot(i)
        } else {
            inv.consume_held()
        }
    };
    if let Some(s) = thrown {
        entities::throw_from(handle, s);
    }
    handle.sync_inventory(world);
    crate::session::save_inventory(handle, world);
}

/// F: swap the main hand and the offhand.
pub fn swap_hands(handle: &PlayerHandle, world: &DemoWorld) {
    {
        let mut inv = handle.inventory();
        let i = inv.held_index();
        let main = inv.take_slot(i);
        let off = inv.take_slot(crate::inventory::OFFHAND);
        inv.put_slot(i, off);
        inv.put_slot(crate::inventory::OFFHAND, main);
    }
    handle.sync_inventory(world);
}

/// Middle-click on a block.
pub fn pick_block(handle: &PlayerHandle, world: &DemoWorld, x: i32, y: i32, z: i32) {
    let name = block_name(world, x, y, z);
    let item = crate::protocol::modern::items::item_for_block(&name)
        .and_then(crate::protocol::modern::items::name_of)
        .map(str::to_owned);
    let Some(item) = item else { return };
    let creative = !handle.game().survival();
    let select = {
        let mut inv = handle.inventory();
        if let Some(h) = (0..9).find(|h| inv.slot(36 + h).is_some_and(|s| s.item == item)) {
            Some(h as u8)
        } else if let Some(from) = (9..36).find(|i| inv.slot(*i).is_some_and(|s| s.item == item)) {
            let i = inv.held_index();
            let moving = inv.take_slot(from);
            let held = inv.take_slot(i);
            inv.put_slot(i, moving);
            inv.put_slot(from, held);
            None
        } else if creative {
            let target = (0..9)
                .find(|h| inv.slot(36 + h).is_none())
                .map(|h| h as u8)
                .unwrap_or(inv.held_slot());
            inv.put_slot(36 + target as usize, Some(Stack::new(&item, 1)));
            Some(target)
        } else {
            None
        }
    };
    if let Some(h) = select {
        handle.set_held_slot(h);
        handle.emit(&ServerEvent::SetHeldSlot(h), world);
    }
    handle.sync_inventory(world);
}

/// Right-click on an entity.
pub fn use_entity(handle: &PlayerHandle, world: &DemoWorld, target: i32) {
    let held = handle.held_item();
    match entities::interact(target, held.as_deref()) {
        Some(entities::Interaction::Replace(with)) => {
            {
                let mut inv = handle.inventory();
                let i = inv.held_index();
                if inv.slot(i).map(|s| s.count) == Some(1) {
                    inv.put_slot(i, Some(Stack::new(with, 1)));
                } else {
                    inv.consume_held();
                    if let Some(rest) = inv.insert(Stack::new(with, 1)) {
                        drop(inv);
                        entities::throw_from(handle, rest);
                    }
                }
            }
            handle.sync_inventory(world);
        }
        Some(entities::Interaction::Wear) => combat::wear_held(handle, world, 1),
        None => {}
    }
}

/// The shared-flags byte and pose for a player's look to others.
fn player_meta(sneaking: bool, sprinting: bool) -> Vec<(u8, MetaValue)> {
    let mut flags = 0i8;
    if sneaking {
        flags |= 0x02;
    }
    if sprinting {
        flags |= 0x08;
    }
    vec![
        (0, MetaValue::Byte(flags)),
        (6, MetaValue::Pose(if sneaking { 5 } else { 0 })),
    ]
}

/// Sneaking or sprinting changed: show everyone else.
pub fn set_stance(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    sneak: Option<bool>,
    sprint: Option<bool>,
) {
    let (sn, sp, changed) = {
        let mut st = handle.game();
        let before = (st.sneaking, st.sprinting);
        if let Some(s) = sneak {
            st.sneaking = s;
        }
        if let Some(s) = sprint {
            st.sprinting = s;
        }
        (
            st.sneaking,
            st.sprinting,
            before != (st.sneaking, st.sprinting),
        )
    };
    if changed {
        registry.broadcast_except(
            handle.entity_id,
            &ServerEvent::EntityMeta {
                entity_id: handle.entity_id,
                entries: player_meta(sn, sp),
            },
            world,
        );
    }
}

/// Track the fall and exhaustion of a movement; hurt on a hard landing.
pub fn on_move(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    before: crate::players::PosLook,
    after: crate::players::PosLook,
) {
    let fall = {
        let mut st = handle.game();
        if !st.survival() || st.dead {
            st.fall_peak = None;
            return;
        }
        let dist = ((after.x - before.x).powi(2) + (after.z - before.z).powi(2)).sqrt();
        if dist < 10.0 {
            let rate = if st.sprinting { 0.1 } else { 0.0 };
            st.exhaust(dist as f32 * rate);
            if before.on_ground && !after.on_ground && after.y > before.y {
                let jump = if st.sprinting { 0.2 } else { 0.05 };
                st.exhaust(jump);
            }
        }
        let feet = block_name(
            world,
            after.x.floor() as i32,
            after.y.floor() as i32,
            after.z.floor() as i32,
        );
        let soft = matches!(
            feet.as_str(),
            "minecraft:water"
                | "minecraft:ladder"
                | "minecraft:vine"
                | "minecraft:scaffolding"
                | "minecraft:cobweb"
                | "minecraft:bubble_column"
                | "minecraft:powder_snow"
        );
        if soft {
            st.fall_peak = None;
            None
        } else if !after.on_ground {
            let peak = st.fall_peak.map_or(after.y, |p| p.max(after.y));
            st.fall_peak = Some(peak);
            None
        } else {
            st.fall_peak.take().map(|peak| peak - after.y)
        }
    };
    if let Some(d) = fall {
        let below = block_name(
            world,
            after.x.floor() as i32,
            (after.y - 0.2).floor() as i32,
            after.z.floor() as i32,
        );
        let damage = (d - 3.0).ceil() as f32;
        if damage > 0.0
            && below != "minecraft:hay_block"
            && !below.ends_with("_bed")
            && below != "minecraft:slime_block"
        {
            combat::hurt_player(handle, damage, "minecraft:fall", None, registry, world);
        }
    }
}

/// Switch a player's game mode.
pub fn set_mode(handle: &PlayerHandle, world: &DemoWorld, mode: GameMode) {
    {
        let mut st = handle.game();
        st.mode = mode;
        st.fall_peak = None;
    }
    handle.emit(&ServerEvent::GameModeChange(mode), world);
    handle.sync_inventory(world);
}
