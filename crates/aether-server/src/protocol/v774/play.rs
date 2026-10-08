//! 1.21.11 encoding of the gameplay events: entities, combat, windows, time.
//!
//! Packet ids and field layouts are from `minecraft-data` `pc/1.21.11/
//! protocol.json`, the same source as the rest of this codec.

use super::{angle, encode_position, inventory, items};
use crate::inventory::Stack;
use crate::proto::PacketOut;
use crate::protocol::{GameMode, Menu, MetaValue, ServerEvent};

const SPAWN_ENTITY: i32 = 0x01;
const ANIMATION: i32 = 0x02;
const BREAK_ANIMATION: i32 = 0x05;
const CLOSE_WINDOW: i32 = 0x11;
const WINDOW_ITEMS: i32 = 0x12;
const WINDOW_PROPERTY: i32 = 0x13;
const DAMAGE_EVENT: i32 = 0x19;
const ENTITY_STATUS: i32 = 0x22;
const SYNC_ENTITY_POS: i32 = 0x23;
const EXPLOSION: i32 = 0x24;
const GAME_STATE: i32 = 0x26;
const WORLD_EVENT: i32 = 0x2D;
const OPEN_WINDOW: i32 = 0x39;
const ABILITIES: i32 = 0x3E;
const DEATH_COMBAT: i32 = 0x42;
const POSITION: i32 = 0x46;
const RESPAWN: i32 = 0x50;
const HEAD_ROTATION: i32 = 0x51;
const ENTITY_METADATA: i32 = 0x61;
const ENTITY_VELOCITY: i32 = 0x63;
const ENTITY_EQUIPMENT: i32 = 0x64;
const EXPERIENCE: i32 = 0x65;
const UPDATE_HEALTH: i32 = 0x66;
const HELD_ITEM: i32 = 0x67;
const UPDATE_TIME: i32 = 0x6F;
const SOUND_EFFECT: i32 = 0x73;
const COLLECT: i32 = 0x7A;

/// Data component `minecraft:damage`.
const COMPONENT_DAMAGE: i32 = 3;

/// Particle registry ids.
const PARTICLE_EXPLOSION_EMITTER: i32 = 22;
const PARTICLE_EXPLOSION: i32 = 23;

/// `minecraft:menu` registry ids.
fn menu_id(m: Menu) -> i32 {
    match m {
        Menu::Chest => 2,
        Menu::Crafting => 12,
        Menu::Furnace => 14,
    }
}

/// Append a stack in this version's slot format.
pub fn write_stack(p: &mut PacketOut, stack: Option<&Stack>) {
    let Some(s) = stack.filter(|s| s.count > 0) else {
        inventory::write_empty(p);
        return;
    };
    let Some(id) = items::item_id(&s.item) else {
        inventory::write_empty(p);
        return;
    };
    p.var_int(s.count as i32).var_int(id);
    if s.damage > 0 {
        p.var_int(1)
            .var_int(0)
            .var_int(COMPONENT_DAMAGE)
            .var_int(s.damage as i32);
    } else {
        p.var_int(0).var_int(0);
    }
}

/// A `lpVec3`: the quantized velocity vector 1.21.9 introduced.
///
/// Three 15-bit components scaled by a shared integer magnitude, packed into
/// 48 bits (low 16 bits little-end first, then 32 big-endian), with a VarInt
/// continuation when the scale exceeds 3. Zero is a single `0x00` byte.
pub fn write_lp_vec3(p: &mut PacketOut, v: (f64, f64, f64)) {
    const MAX_Q: f64 = 32766.0;
    let clean = |x: f64| {
        if x.is_nan() {
            0.0
        } else {
            x.clamp(-1.7179869183e10, 1.7179869183e10)
        }
    };
    let (x, y, z) = (clean(v.0), clean(v.1), clean(v.2));
    let max = x.abs().max(y.abs()).max(z.abs());
    if max < 3.051944088384301e-5 {
        p.u8(0);
        return;
    }
    let scale = max.ceil() as u64;
    let cont = scale > 3;
    let markers = if cont { (scale % 4) | 4 } else { scale };
    let pack = |c: f64| (((c / scale as f64) * 0.5 + 0.5) * MAX_Q).round() as u64;
    let packed = markers | (pack(x) << 3) | (pack(y) << 18) | (pack(z) << 33);
    p.u8((packed & 0xFF) as u8)
        .u8(((packed >> 8) & 0xFF) as u8)
        .bytes(&(((packed >> 16) & 0xFFFF_FFFF) as u32).to_be_bytes());
    if cont {
        p.var_int((scale / 4) as i32);
    }
}

/// An inline sound event: holder id 0, then the identifier and no range.
fn write_sound(p: &mut PacketOut, name: &str) {
    p.var_int(0).string(name).bool(false);
}

fn abilities(mode: GameMode) -> PacketOut {
    let mut p = PacketOut::new(ABILITIES);
    let flags = match mode {
        GameMode::Creative => 0x01 | 0x04 | 0x08,
        GameMode::Survival => 0,
    };
    p.u8(flags).f32(0.05).f32(0.10);
    p
}

/// Encode a gameplay event, or `None` if it is not one of these.
pub fn encode(ev: &ServerEvent) -> Option<Vec<PacketOut>> {
    Some(match ev {
        ServerEvent::SpawnEntity {
            entity_id,
            uuid,
            kind,
            x,
            y,
            z,
            yaw,
            pitch,
            velocity,
            data,
        } => {
            let Some((type_id, _, _)) = crate::game::tables::entity_type(kind) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(SPAWN_ENTITY);
            p.var_int(*entity_id)
                .uuid(*uuid)
                .var_int(type_id)
                .f64(*x)
                .f64(*y)
                .f64(*z);
            write_lp_vec3(&mut p, *velocity);
            p.u8(angle(*pitch))
                .u8(angle(*yaw))
                .u8(angle(*yaw))
                .var_int(*data);
            let mut head = PacketOut::new(HEAD_ROTATION);
            head.var_int(*entity_id).u8(angle(*yaw));
            vec![p, head]
        }
        ServerEvent::EntityMeta { entity_id, entries } => {
            let mut p = PacketOut::new(ENTITY_METADATA);
            p.var_int(*entity_id);
            for (index, value) in entries {
                p.u8(*index);
                match value {
                    MetaValue::Byte(b) => {
                        p.var_int(0).u8(*b as u8);
                    }
                    MetaValue::VarInt(v) => {
                        p.var_int(1).var_int(*v);
                    }
                    MetaValue::Float(f) => {
                        p.var_int(3).f32(*f);
                    }
                    MetaValue::Item(s) => {
                        p.var_int(7);
                        write_stack(&mut p, s.as_ref());
                    }
                    MetaValue::Pose(v) => {
                        p.var_int(20).var_int(*v);
                    }
                }
            }
            p.u8(0xFF);
            vec![p]
        }
        ServerEvent::EntityPos {
            entity_id,
            x,
            y,
            z,
            yaw,
            pitch,
            on_ground,
        } => {
            let mut p = PacketOut::new(SYNC_ENTITY_POS);
            p.var_int(*entity_id)
                .f64(*x)
                .f64(*y)
                .f64(*z)
                .f64(0.0)
                .f64(0.0)
                .f64(0.0)
                .f32(*yaw)
                .f32(*pitch)
                .bool(*on_ground);
            vec![p]
        }
        ServerEvent::EntityHead { entity_id, yaw } => {
            let mut p = PacketOut::new(HEAD_ROTATION);
            p.var_int(*entity_id).u8(angle(*yaw));
            vec![p]
        }
        ServerEvent::EntityVelocity {
            entity_id,
            velocity,
        } => {
            let mut p = PacketOut::new(ENTITY_VELOCITY);
            p.var_int(*entity_id);
            write_lp_vec3(&mut p, *velocity);
            vec![p]
        }
        ServerEvent::EntityAnimation {
            entity_id,
            animation,
        } => {
            let mut p = PacketOut::new(ANIMATION);
            p.var_int(*entity_id).u8(*animation);
            vec![p]
        }
        ServerEvent::EntityStatus { entity_id, status } => {
            let mut p = PacketOut::new(ENTITY_STATUS);
            p.i32(*entity_id).u8(*status as u8);
            vec![p]
        }
        ServerEvent::Damage {
            entity_id,
            source,
            attacker,
        } => {
            let ty = super::registry::damage_type_index(source).unwrap_or(0);
            let mut p = PacketOut::new(DAMAGE_EVENT);
            let who = attacker.map(|a| a + 1).unwrap_or(0);
            p.var_int(*entity_id)
                .var_int(ty as i32)
                .var_int(who)
                .var_int(who)
                .bool(false);
            vec![p]
        }
        ServerEvent::Collect {
            item,
            collector,
            count,
        } => {
            let mut p = PacketOut::new(COLLECT);
            p.var_int(*item).var_int(*collector).var_int(*count as i32);
            vec![p]
        }
        ServerEvent::Equipment { entity_id, slots } => {
            if slots.is_empty() {
                return Some(Vec::new());
            }
            let mut p = PacketOut::new(ENTITY_EQUIPMENT);
            p.var_int(*entity_id);
            for (i, (slot, stack)) in slots.iter().enumerate() {
                let more = if i + 1 < slots.len() { 0x80 } else { 0 };
                p.u8(*slot | more);
                write_stack(&mut p, stack.as_ref());
            }
            vec![p]
        }
        ServerEvent::Health {
            health,
            food,
            saturation,
        } => {
            let mut p = PacketOut::new(UPDATE_HEALTH);
            p.f32(*health).var_int(*food).f32(*saturation);
            vec![p]
        }
        ServerEvent::Experience { bar, level, total } => {
            let mut p = PacketOut::new(EXPERIENCE);
            p.f32(*bar).var_int(*level).var_int(*total);
            vec![p]
        }
        ServerEvent::Time { age, time_of_day } => {
            let mut p = PacketOut::new(UPDATE_TIME);
            p.i64(*age).i64(*time_of_day).bool(true);
            vec![p]
        }
        ServerEvent::WindowContents {
            window_id,
            state_id,
            slots,
            cursor,
        } => {
            let mut p = PacketOut::new(WINDOW_ITEMS);
            p.var_int(*window_id as i32)
                .var_int(*state_id)
                .var_int(slots.len() as i32);
            for s in slots {
                write_stack(&mut p, s.as_ref());
            }
            write_stack(&mut p, cursor.as_ref());
            vec![p]
        }
        ServerEvent::OpenWindow {
            window_id,
            menu,
            title,
        } => {
            let mut p = PacketOut::new(OPEN_WINDOW);
            p.var_int(*window_id as i32)
                .var_int(menu_id(*menu))
                .bytes(&crate::protocol::nbt::string(title).to_network());
            vec![p]
        }
        ServerEvent::CloseWindow(id) => {
            let mut p = PacketOut::new(CLOSE_WINDOW);
            p.var_int(*id as i32);
            vec![p]
        }
        ServerEvent::WindowProperty {
            window_id,
            property,
            value,
        } => {
            let mut p = PacketOut::new(WINDOW_PROPERTY);
            p.var_int(*window_id as i32)
                .u16(*property as u16)
                .u16(*value as u16);
            vec![p]
        }
        ServerEvent::WorldEvent {
            event,
            x,
            y,
            z,
            data,
        } => {
            let mut p = PacketOut::new(WORLD_EVENT);
            p.i32(*event)
                .i64(encode_position(*x as i64, *y as i64, *z as i64))
                .i32(*data)
                .bool(false);
            vec![p]
        }
        ServerEvent::BreakAnimation {
            entity_id,
            x,
            y,
            z,
            stage,
        } => {
            let mut p = PacketOut::new(BREAK_ANIMATION);
            p.var_int(*entity_id)
                .i64(encode_position(*x as i64, *y as i64, *z as i64))
                .u8(*stage as u8);
            vec![p]
        }
        ServerEvent::Respawn { game_mode } => {
            let mut p = PacketOut::new(RESPAWN);
            p.var_int(0) // dimension type 0
                .string(super::registry::DIMENSION_NAME)
                .i64(0)
                .u8(game_mode.wire())
                .u8(0xFF)
                .bool(false)
                .bool(false)
                .bool(false) // no death location
                .var_int(0)
                .var_int(63)
                .u8(0); // keep nothing
                        // Then "start waiting for level chunks", as at login. Without it a
                        // 1.20.3+ client holds the loading screen after a respawn until
                        // it gives up — the world never appears.
            let mut wait = PacketOut::new(GAME_STATE);
            wait.u8(13).f32(0.0);
            vec![p, abilities(*game_mode), wait]
        }
        ServerEvent::GameModeChange(mode) => {
            let mut p = PacketOut::new(GAME_STATE);
            p.u8(3).f32(mode.wire() as f32);
            vec![p, abilities(*mode)]
        }
        ServerEvent::SetHeldSlot(slot) => {
            vec![inventory::held_item_packet(HELD_ITEM, *slot as i32)]
        }
        ServerEvent::Teleport {
            x,
            y,
            z,
            yaw,
            pitch,
        } => {
            let mut p = PacketOut::new(POSITION);
            p.var_int(2)
                .f64(*x)
                .f64(*y)
                .f64(*z)
                .f64(0.0)
                .f64(0.0)
                .f64(0.0)
                .f32(*yaw)
                .f32(*pitch)
                .i32(0);
            vec![p]
        }
        ServerEvent::DeathMessage { entity_id, text } => {
            let mut p = PacketOut::new(DEATH_COMBAT);
            p.var_int(*entity_id)
                .bytes(&crate::protocol::nbt::string(text).to_network());
            vec![p]
        }
        ServerEvent::Explosion {
            x,
            y,
            z,
            radius,
            knockback,
        } => {
            let mut p = PacketOut::new(EXPLOSION);
            p.f64(*x).f64(*y).f64(*z).f32(*radius).i32(0);
            match knockback {
                Some((kx, ky, kz)) => {
                    p.bool(true).f64(*kx).f64(*ky).f64(*kz);
                }
                None => {
                    p.bool(false);
                }
            }
            let particle = if *radius >= 2.0 {
                PARTICLE_EXPLOSION_EMITTER
            } else {
                PARTICLE_EXPLOSION
            };
            p.var_int(particle);
            write_sound(&mut p, "minecraft:entity.generic.explode");
            p.var_int(0); // no block particles
            vec![p]
        }
        ServerEvent::Sound {
            name,
            category,
            x,
            y,
            z,
            volume,
            pitch,
        } => {
            let mut p = PacketOut::new(SOUND_EFFECT);
            write_sound(&mut p, name);
            p.var_int(*category as i32)
                .i32((x * 8.0) as i32)
                .i32((y * 8.0) as i32)
                .i32((z * 8.0) as i32)
                .f32(*volume)
                .f32(*pitch)
                .i64(0);
            vec![p]
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference decoder from minecraft-protocol's `lpVec3.js`, ported.
    fn read_lp(b: &[u8]) -> (f64, f64, f64) {
        if b[0] == 0 {
            return (0.0, 0.0, 0.0);
        }
        let c = u32::from_be_bytes([b[2], b[3], b[4], b[5]]) as u64;
        let packed = (c << 16) | ((b[1] as u64) << 8) | b[0] as u64;
        let mut scale = (b[0] & 3) as u64;
        if b[0] & 4 == 4 {
            scale += (b[6] as u64) * 4;
        }
        let un = |shift: u32| {
            let q = ((packed >> shift) & 0x7FFF).min(32766) as f64;
            q * 2.0 / 32766.0 - 1.0
        };
        (
            un(3) * scale as f64,
            un(18) * scale as f64,
            un(33) * scale as f64,
        )
    }

    #[test]
    fn lp_vec3_round_trips() {
        for v in [
            (0.0, 0.0, 0.0),
            (0.1, -0.2, 0.3),
            (1.5, 0.42, -2.9),
            (5.0, -7.0, 0.5),
        ] {
            let mut p = PacketOut::new(0);
            write_lp_vec3(&mut p, v);
            let got = read_lp(&p.body()[1..]);
            for (a, b) in [(got.0, v.0), (got.1, v.1), (got.2, v.2)] {
                assert!((a - b).abs() < 0.01, "{v:?} -> {got:?}");
            }
        }
    }
}
