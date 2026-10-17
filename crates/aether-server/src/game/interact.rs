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

/// Whether `state` is a *source* water block (`level=0`), as opposed to a
/// flowing cell. A bottle fills only from a source, vanilla's
/// `ClipContext.Fluid.SOURCE_ONLY`.
fn source_water(state: BlockStateId) -> bool {
    matches!(
        super::fluids::cell_of(state),
        Some(c) if c.fluid == super::fluids::Fluid::Water && c.level == 0
    )
}

/// Whether the block `state` carries a source water fluid as its waterlogged
/// property — a waterlogged block is a source the same way a water block is.
fn waterlogged(state: BlockStateId) -> bool {
    let Some((id, _)) = aether_world::registry::blocks::block_of_state(state) else {
        return false;
    };
    let (lo, _) = aether_world::registry::blocks::STATE_RANGE[id as usize];
    aether_world::registry::props::values_of(id, state.raw() - lo)
        .iter()
        .any(|(k, v)| *k == "waterlogged" && *v == "true")
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
    let held = handle.held_item().and_then(|i| i.vanilla_name());
    let pos = handle.pos();
    let in_water = entities::in_water(world, Vector3::new(pos.x, pos.y + 1.5, pos.z));
    match tables::dig_ticks(&name, held, pos.on_ground, in_water) {
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
        let other_y = y + dy;
        if world.get_block(x, other_y, z) != block_ids::AIR {
            // A door or a tall plant is two cells, and the break hook checked
            // only the clicked one. A region whose boundary runs between them
            // must keep its half: a claim with a ceiling at `y + 1` does not
            // lose it because the other half stands outside.
            if let Some(reason) =
                crate::region::denial_at(handle, x, other_y, z, crate::region::Action::Break)
            {
                if !reason.is_empty() {
                    handle.emit(&ServerEvent::Chat(reason), world);
                }
            } else {
                crate::session::set_and_broadcast(
                    world,
                    registry,
                    handle,
                    x,
                    other_y,
                    z,
                    block_ids::AIR,
                );
            }
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
    let held_name = held.and_then(|i| i.vanilla_name());
    let ctx = super::loot::LootContext {
        tool: held_name.map(str::to_owned),
        block: tables::block_state_context(broken),
        ..Default::default()
    };
    let drops = entities::with_rng(|r| tables::block_drops_ctx(&name, &ctx, r));
    entities::drop_at_block(
        x,
        y,
        z,
        drops
            .into_iter()
            .map(|(i, n)| Stack::named(&i, n))
            .collect(),
        aether_world::journal::ActorId(handle.uuid),
        cause,
    );
    // Ores drop experience as orbs, on the ground, regardless of the tool.
    if let Some((min, max)) = tables::block_xp(&name) {
        let xp = entities::with_rng(|r| r.range(min as i32, max as i32));
        if xp > 0 {
            entities::award_xp(
                Vector3::new(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5),
                xp,
            );
        }
    }
    handle.game().exhaust(0.005);
    let hard = tables::block_info(&name).map(|i| i.hardness).unwrap_or(0.0);
    if hard > 0.0 {
        let n = if held_name.is_some_and(|h| h.ends_with("_sword")) {
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
    let interactive = short == "crafting_table"
        || ContainerKind::of_block(&name).is_some()
        || short.ends_with("_bed");
    if interactive {
        if let Some(reason) =
            crate::region::denial_at(handle, x, y, z, crate::region::Action::Interact)
        {
            if !reason.is_empty() {
                handle.emit(&ServerEvent::Chat(reason), world);
            }
            // Consumed: a refused interaction must not fall through to a
            // placement against the protected block.
            return true;
        }
    }
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
            let state = world.get_block(x, y, z);
            let (left, right) = crate::placement::chest_halves(state, x, y, z, &|nx, ny, nz| {
                world.get_block(nx, ny, nz)
            });
            window::open(handle, world, WindowKind::Chest { left, right });
            return true;
        }
        Some(ContainerKind::Furnace) => {
            window::open(handle, world, WindowKind::Furnace { pos: (x, y, z) });
            return true;
        }
        Some(ContainerKind::Hopper) => {
            window::open(handle, world, WindowKind::Hopper { pos: (x, y, z) });
            return true;
        }
        Some(ContainerKind::Dispenser) => {
            window::open(handle, world, WindowKind::Dispenser { pos: (x, y, z) });
            return true;
        }
        Some(ContainerKind::BrewingStand) => {
            window::open(handle, world, WindowKind::BrewingStand { pos: (x, y, z) });
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
        if s.item
            .vanilla_name()
            .is_some_and(|n| n.ends_with("_bucket"))
            && s.item != crate::inventory::item("minecraft:milk_bucket")
        {
            *s = Stack::named("minecraft:bucket", 1);
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
    let held = {
        let inv = handle.inventory();
        let slot = if hand == 1 {
            crate::inventory::OFFHAND
        } else {
            inv.held_index()
        };
        inv.slot(slot).cloned()
    };
    let Some(held) = held else { return };
    let item = held.item;
    let now = super::now();
    // A potion is drunk (thirty-two ticks to swallow) or, if it is a splash or
    // lingering potion, thrown.
    if let Some(name) = item.vanilla_name() {
        if super::brewing::is_potion_item(name) {
            if super::brewing::is_drinkable(name) {
                handle.game().eating = Some((now, hand));
            } else {
                throw_potion(handle, registry, world, hand, &held, name);
            }
            return;
        }
    }
    if tables::food_id(item).is_some() {
        let mut st = handle.game();
        let always = item == crate::inventory::item("minecraft:golden_apple")
            || item == crate::inventory::item("minecraft:enchanted_golden_apple")
            || item == crate::inventory::item("minecraft:chorus_fruit");
        if st.food < 20 || always || !st.survival() {
            st.eating = Some((now, hand));
        }
        return;
    }
    if item == crate::inventory::item("minecraft:milk_bucket") {
        handle.game().eating = Some((now, hand));
    } else if item == crate::inventory::item("minecraft:bow") {
        let has_arrow = !handle.game().survival()
            || handle
                .inventory()
                .count_of(crate::inventory::item("minecraft:arrow"))
                > 0;
        if has_arrow {
            handle.game().drawing = Some(now);
        }
    } else if item == crate::inventory::item("minecraft:bucket") {
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
        // The `use` flag is checked at the cell the bucket drains, not where
        // the player stands: the ray reaches up to five blocks.
        if let Some(reason) =
            crate::region::denial_at(handle, x, y, z, crate::region::Action::UseItem)
        {
            if !reason.is_empty() {
                handle.emit(&ServerEvent::Chat(reason), world);
            }
            return;
        }
        crate::session::set_and_broadcast(world, registry, handle, x, y, z, block_ids::AIR);
        if handle.game().survival() {
            let mut inv = handle.inventory();
            let i = inv.held_index();
            let one = inv.slot(i).map(|s| s.count).unwrap_or(0) == 1;
            if one {
                inv.put_slot(i, Some(Stack::named(filled, 1)));
            } else {
                inv.consume_held();
                if let Some(rest) = inv.insert(Stack::named(filled, 1)) {
                    drop(inv);
                    entities::throw_from(handle, rest);
                }
            }
        }
        handle.sync_inventory(world);
    } else if item == crate::inventory::item("minecraft:glass_bottle") {
        // A glass bottle fills from a water *source* only: flowing water is
        // transparent to the ray, and a waterlogged source block fills the
        // same way a water block does (vanilla's `ClipContext.Fluid.SOURCE_ONLY`).
        let hit = raycast(handle, 5.0, |x, y, z| {
            let state = world.get_block(x, y, z);
            if source_water(state) {
                return true;
            }
            // Any other fluid (flowing water, lava) is transparent.
            if super::fluids::cell_of(state).is_some() {
                return false;
            }
            world.props_of(state).collision
        });
        let Some(((x, y, z), _)) = hit else { return };
        let state = world.get_block(x, y, z);
        if !source_water(state) && !waterlogged(state) {
            return;
        }
        // As with a bucket, the region's `use` flag is checked at the target
        // cell the bottle reaches, not at the player's feet.
        if let Some(reason) =
            crate::region::denial_at(handle, x, y, z, crate::region::Action::UseItem)
        {
            if !reason.is_empty() {
                handle.emit(&ServerEvent::Chat(reason), world);
            }
            return;
        }
        if handle.game().survival() {
            let mut inv = handle.inventory();
            let i = if hand == 1 {
                crate::inventory::OFFHAND
            } else {
                inv.held_index()
            };
            inv.consume_at(i);
            let water = Stack::potion("minecraft:potion", super::brewing::WATER, 1);
            if let Some(rest) = inv.insert(water) {
                drop(inv);
                entities::throw_from(handle, rest);
            }
        }
        handle.sync_inventory(world);
        crate::session::save_inventory(handle, world);
    } else if crate::protocol::compat::behaviour::is_spear(item) {
        // The real item decides, not the stand-in: a client without spears
        // was shown a trident (or a sword on 1.8.9) and presses right click
        // to charge. The server starts the *spear's* charge; the trident's
        // throw is never run.
        handle.game().charging = Some(now);
    }
}

/// Throw a splash or lingering potion, vanilla's `ThrowablePotionItem.use`:
/// `spawnProjectileFromRotation` with a twenty-degree lift and power 0.5.
fn throw_potion(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    hand: u8,
    stack: &Stack,
    name: &str,
) {
    let p = handle.pos();
    let xr = (p.pitch as f64).to_radians();
    let yr = (p.yaw as f64).to_radians();
    let dx = -yr.sin() * xr.cos();
    let dy = -((p.pitch as f64 - 20.0).to_radians()).sin();
    let dz = yr.cos() * xr.cos();
    let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-9);
    let speed = 0.5;
    let vel = Vector3::new(dx / len * speed, dy / len * speed, dz / len * speed);
    let potion = stack.potion.unwrap_or(super::brewing::WATER);
    let lingering = name.ends_with("lingering_potion");
    entities::spawn_potion(
        Vector3::new(p.x, p.y + 1.5, p.z),
        vel,
        potion,
        lingering,
        Some(handle.entity_id),
    );
    let sound = if lingering {
        "minecraft:entity.lingering_potion.throw"
    } else {
        "minecraft:entity.splash_potion.throw"
    };
    registry.broadcast_except(
        handle.entity_id,
        &ServerEvent::Sound {
            name: sound.into(),
            category: 7,
            x: p.x,
            y: p.y,
            z: p.z,
            volume: 0.5,
            pitch: 1.0,
        },
        world,
    );
    if handle.game().survival() {
        let mut inv = handle.inventory();
        let slot = if hand == 1 {
            crate::inventory::OFFHAND
        } else {
            inv.held_index()
        };
        inv.consume_at(slot);
        handle.sync_inventory(world);
        crate::session::save_inventory(handle, world);
    }
}

/// Stopped using the held item: a drawn bow fires, a charged spear lunges.
pub fn release_use(handle: &PlayerHandle, world: &DemoWorld) {
    let (drawn, charged) = {
        let mut st = handle.game();
        st.eating = None;
        (st.drawing.take(), st.charging.take())
    };
    if let Some(start) = charged {
        // A spear's release is its lunge. This is checked before the bow so
        // that a stray `drawing` cannot fire an arrow for a spear.
        spear_charge(handle, world, start);
        return;
    }
    let Some(start) = drawn else { return };
    let t = super::now().saturating_sub(start) as f64 / 20.0;
    let power = ((t * t + t * 2.0) / 3.0).min(1.0);
    if power < 0.1 {
        return;
    }
    let survival = handle.game().survival();
    if survival
        && handle
            .inventory()
            .take(crate::inventory::item("minecraft:arrow"), 1)
            == 0
    {
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
                name: "minecraft:entity.arrow.shoot".into(),
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

/// The minimum hold, in ticks, before a spear charge is worth anything.
const SPEAR_MIN_CHARGE: u64 = 10;
/// How far in front of the player a spear's kinetic hit reaches.
const SPEAR_REACH: f64 = 3.0;

/// The release of a held right click on a spear: the charged lunge.
///
/// The real item decides: this runs only while the server holds a spear, no
/// matter which stand-in the client was shown. It never spawns a projectile —
/// the client may have predicted a trident throw, and `correct_release`
/// cancels that — instead it drives the player forward and deals the kinetic
/// blow to whatever is in front.
pub fn spear_charge(handle: &PlayerHandle, world: &DemoWorld, start: u64) {
    // The held item can have changed since the press; only a spear lunges.
    if !handle
        .held_item()
        .is_some_and(crate::protocol::compat::behaviour::is_spear)
    {
        correct_release(handle, world);
        return;
    }
    let held_ticks = super::now().saturating_sub(start);
    if held_ticks < SPEAR_MIN_CHARGE {
        // Not enough charge to lunge, but the client's predicted throw still
        // has to be cancelled.
        correct_release(handle, world);
        return;
    }
    let p = handle.pos();
    let (yaw, pitch) = (p.yaw.to_radians() as f64, p.pitch.to_radians() as f64);
    let dir = Vector3::new(
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    );
    let seconds = held_ticks as f64 / 20.0;
    let speed = seconds.min(3.0) * 0.9;

    // The lunge itself: the client applies this velocity, so the player
    // travels the way they are looking.
    handle.emit(
        &ServerEvent::EntityVelocity {
            entity_id: handle.entity_id,
            velocity: (dir.x * speed, dir.y * speed + 0.15, dir.z * speed),
        },
        world,
    );
    kinetic_hit(
        handle,
        world,
        Vector3::new(p.x, p.y + 1.0, p.z),
        dir,
        seconds,
    );
    // The arm swings for the real action, whatever the stand-in.
    if let Some(r) = super::registry() {
        r.broadcast_except(
            handle.entity_id,
            &ServerEvent::EntityAnimation {
                entity_id: handle.entity_id,
                animation: 0,
            },
            world,
        );
    }
    if handle.game().survival() {
        combat::wear_held(handle, world, 1);
    }
    correct_release(handle, world);
}

/// Give the spear back to a client that predicted the stand-in's projectile.
///
/// A trident stand-in's release makes the client spawn a predicted thrown
/// trident and pull the item out of the hand. Resending the slot makes the
/// server's view — the spear, still in hand — win, cancelling the prediction.
fn correct_release(handle: &PlayerHandle, world: &DemoWorld) {
    handle.sync_inventory(world);
    crate::session::save_inventory(handle, world);
}

/// Damage the nearest mob in front of `eye` within [`SPEAR_REACH`].
///
/// Returns whether anything was hit. The blow scales with the charge, as
/// vanilla's kinetic weapon does.
fn kinetic_hit(
    handle: &PlayerHandle,
    world: &DemoWorld,
    eye: Vector3,
    dir: Vector3,
    seconds: f64,
) -> bool {
    let (base, _) = tables::weapon_id(handle.held_item());
    let damage = base * (1.0 + seconds.min(3.0) as f32);
    let mut target: Option<(f64, i32)> = None;
    for (id, pos) in entities::mobs_in(eye, SPEAR_REACH) {
        let to = Vector3::new(pos.x - eye.x, pos.y - eye.y, pos.z - eye.z);
        let dist = (to.x * to.x + to.y * to.y + to.z * to.z).sqrt();
        if dist < 0.01 {
            continue;
        }
        // In front: the target is within the cone the player looks down.
        if (to.x * dir.x + to.y * dir.y + to.z * dir.z) / dist < 0.5 {
            continue;
        }
        if target.map_or(true, |(d, _)| dist < d) {
            target = Some((dist, id));
        }
    }
    let Some((_, id)) = target else {
        return false;
    };
    let Some(hit) = entities::hurt_mob(id, damage, Some(eye), Some(handle.entity_id)) else {
        return false;
    };
    if let Some(reg) = super::registry() {
        combat::on_mob_hurt(
            id,
            &hit,
            Some(handle),
            reg,
            world,
            "minecraft:player_attack",
        );
    }
    true
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
        .map(crate::inventory::item);
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
            inv.put_slot(36 + target as usize, Some(Stack::new(item, 1)));
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
    if let Some(p) = entities::position(target) {
        if let Some(reason) = crate::region::denial_at(
            handle,
            p.x.floor() as i32,
            p.y.floor() as i32,
            p.z.floor() as i32,
            crate::region::Action::Interact,
        ) {
            if !reason.is_empty() {
                handle.emit(&ServerEvent::Chat(reason), world);
            }
            return;
        }
    }
    let held = handle.held_item();
    match entities::interact(target, held) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::registry::blocks;

    fn test_world(seed: u64) -> DemoWorld {
        use aether_api::{FjallStore, World};
        let dir = std::env::temp_dir().join(format!("aether-use-{}-{seed}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", seed).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, seed, revision, &cfg);
        World::new(store, generator)
    }

    #[test]
    fn a_region_blocks_draining_a_source_from_outside() {
        // The player stands outside the claim and looks down at a source
        // inside it: the `use` flag must be checked at the bucket's target,
        // not at the player's feet.
        let regions = crate::region::install_test_store(crate::region::RegionStore::new());
        let region = crate::region::Region::new(
            crate::region::RegionId(1),
            "overworld",
            crate::region::BlockPos::new(0, 71, 0),
            crate::region::BlockPos::new(0, 71, 0),
        )
        .with_flag("use", false);
        regions.replace_all([region.handle()]);

        let world = test_world(29);
        let registry: SharedRegistry = std::sync::Arc::new(crate::players::Registry::default());
        let handle = crate::players::PlayerHandle::for_test(1, "BucketGuest");
        registry.insert_for_test(std::sync::Arc::clone(&handle));
        {
            let mut inv = handle.inventory();
            inv.put_slot(0, Some(Stack::named("minecraft:bucket", 1)));
        }
        handle.set_held_slot(0);
        let mut p = handle.pos();
        p.x = 0.5;
        p.y = 74.0;
        p.z = 0.5;
        p.yaw = 0.0;
        p.pitch = 90.0;
        handle.set_pos(p);
        for y in 72..=74 {
            world.set_block(0, y, 0, "minecraft:air");
        }
        world.set_block(0, 71, 0, "minecraft:water");

        use_item(&handle, &registry, &world, 0);

        assert!(
            source_water(world.get_block(0, 71, 0)),
            "the protected source must survive a bucket used from outside"
        );
        let held = handle
            .inventory()
            .slot(0)
            .and_then(|s| s.item.vanilla_name());
        assert_eq!(
            held,
            Some("minecraft:bucket"),
            "the bucket must not fill from a protected source"
        );
    }

    #[test]
    fn a_bottle_fills_only_from_a_source() {
        let water = blocks::states_of("minecraft:water").expect("water is a block");
        assert!(
            source_water(BlockStateId(*water.start())),
            "level 0 is a source"
        );
        assert!(
            !source_water(BlockStateId(*water.start() + 1)),
            "level 1 flows and a bottle must refuse it"
        );
        // A waterlogged block carries a source water fluid.
        let stairs = blocks::default_state("minecraft:oak_stairs").unwrap();
        let (id, _) = blocks::block_of_state(stairs).unwrap();
        assert!(!waterlogged(stairs), "a fresh stair is dry");
        let wet =
            aether_world::registry::props::state_with(id, &[("waterlogged", "true")]).unwrap();
        assert!(waterlogged(BlockStateId(wet)));
        assert!(!waterlogged(
            blocks::default_state("minecraft:stone").unwrap()
        ));
    }
}
