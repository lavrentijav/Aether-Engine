//! 1.21.11 encoding of the gameplay events: entities, combat, windows, time.
//!
//! Packet ids and field layouts are from `minecraft-data` `pc/1.21.11/
//! protocol.json`, the same source as the rest of this codec.

use super::version::{Gen, Version};
use super::{angle, encode_position, inventory};
use crate::inventory::Stack;
use crate::proto::PacketOut;
use crate::protocol::{GameMode, Menu, MetaValue, ServerEvent};

/// `minecraft:menu` registry ids.
fn menu_id(v: &Version, m: Menu) -> i32 {
    match m {
        Menu::Chest => v.menu_chest,
        Menu::Crafting => v.menu_crafting,
        Menu::Furnace => v.menu_furnace,
    }
}

/// Append a stack in this version's slot format.
pub fn write_stack(v: &Version, p: &mut PacketOut, stack: Option<&Stack>) {
    let Some(s) = stack.filter(|s| s.count > 0) else {
        inventory::write_empty(p);
        return;
    };
    let Some(id) = v.item_id(&s.item) else {
        inventory::write_empty(p);
        return;
    };
    p.var_int(s.count as i32).var_int(id);
    if s.damage > 0 {
        p.var_int(1)
            .var_int(0)
            .var_int(v.component_damage)
            .var_int(s.damage as i32);
    } else {
        p.var_int(0).var_int(0);
    }
}

/// Entity Position Sync: absolute position and look. 26.3 made the
/// position a *path* — a tagged union of one point or several steps — so
/// the single point is tag 0 there.
pub fn position_sync(
    v: &Version,
    entity_id: i32,
    (x, y, z): (f64, f64, f64),
    yaw: f32,
    pitch: f32,
    on_ground: bool,
) -> PacketOut {
    let mut p = PacketOut::new(v.packets.cb_entity_position_sync);
    p.var_int(entity_id);
    if v.gen >= Gen::V26_3 {
        p.var_int(0).f64(x).f64(y).f64(z);
    } else {
        p.f64(x).f64(y).f64(z).f64(0.0).f64(0.0).f64(0.0);
    }
    p.f32(yaw).f32(pitch).bool(on_ground);
    p
}

/// The previous game mode of a spawn: none. A byte of `-1` until 26.3 made
/// it an optional VarInt, whose "none" is zero.
pub fn write_no_previous_mode(v: &Version, p: &mut PacketOut) {
    if v.gen >= Gen::V26_3 {
        p.var_int(0);
    } else {
        p.u8(0xFF);
    }
}

/// The wire index of an entity metadata field. Indices at or above
/// [`crate::protocol::META_AGEABLE`] count from the end of `AgeableMob`'s
/// own fields, which 26.1 grew by one (`age_locked`).
fn meta_index(v: &Version, index: u8) -> u8 {
    if index & crate::protocol::META_AGEABLE == 0 {
        return index;
    }
    let base = if v.gen >= Gen::V26_1 { 18 } else { 17 };
    base + (index & !crate::protocol::META_AGEABLE)
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

fn abilities(v: &Version, mode: GameMode) -> PacketOut {
    let mut p = PacketOut::new(v.packets.cb_player_abilities);
    let flags = match mode {
        GameMode::Creative => 0x01 | 0x04 | 0x08,
        GameMode::Survival => 0,
    };
    p.u8(flags).f32(0.05).f32(0.10);
    p
}

/// Encode a gameplay event, or `None` if it is not one of these.
pub fn encode(v: &Version, ev: &ServerEvent) -> Option<Vec<PacketOut>> {
    let ids = v.packets;
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
            let Some(type_id) = v.entity_type(kind) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.cb_add_entity);
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
            let mut head = PacketOut::new(ids.cb_rotate_head);
            head.var_int(*entity_id).u8(angle(*yaw));
            vec![p, head]
        }
        ServerEvent::EntityMeta { entity_id, entries } => {
            let mut p = PacketOut::new(ids.cb_set_entity_data);
            p.var_int(*entity_id);
            for (index, value) in entries {
                p.u8(meta_index(v, *index));
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
                        write_stack(v, &mut p, s.as_ref());
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
            vec![position_sync(
                v,
                *entity_id,
                (*x, *y, *z),
                *yaw,
                *pitch,
                *on_ground,
            )]
        }
        ServerEvent::EntityHead { entity_id, yaw } => {
            let mut p = PacketOut::new(ids.cb_rotate_head);
            p.var_int(*entity_id).u8(angle(*yaw));
            vec![p]
        }
        ServerEvent::EntityVelocity {
            entity_id,
            velocity,
        } => {
            let mut p = PacketOut::new(ids.cb_set_entity_motion);
            p.var_int(*entity_id);
            write_lp_vec3(&mut p, *velocity);
            vec![p]
        }
        ServerEvent::EntityAnimation {
            entity_id,
            animation,
        } => {
            // 26.3 moved arm swings to a packet of their own, carrying the
            // hand and the swing's shape, and renumbered what was left.
            if v.gen >= Gen::V26_3 {
                return Some(match animation {
                    0 | 3 => {
                        let mut p = PacketOut::new(ids.cb_swing_animation);
                        p.var_int(*entity_id)
                            .var_int((*animation == 3) as i32) // hand
                            .var_int(1) // whack
                            .var_int(6); // ticks
                        vec![p]
                    }
                    2 | 4 | 5 => {
                        let mut p = PacketOut::new(ids.cb_animate);
                        p.var_int(*entity_id).u8(match animation {
                            2 => 0,
                            4 => 1,
                            _ => 2,
                        });
                        vec![p]
                    }
                    _ => Vec::new(),
                });
            }
            let mut p = PacketOut::new(ids.cb_animate);
            p.var_int(*entity_id).u8(*animation);
            vec![p]
        }
        ServerEvent::EntityStatus { entity_id, status } => {
            let mut p = PacketOut::new(ids.cb_entity_event);
            p.i32(*entity_id).u8(*status as u8);
            vec![p]
        }
        ServerEvent::Damage {
            entity_id,
            source,
            attacker,
        } => {
            let ty = v.damage_type(source).unwrap_or(0);
            let mut p = PacketOut::new(ids.cb_damage_event);
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
            let mut p = PacketOut::new(ids.cb_take_item_entity);
            p.var_int(*item).var_int(*collector).var_int(*count as i32);
            vec![p]
        }
        ServerEvent::Equipment { entity_id, slots } => {
            if slots.is_empty() {
                return Some(Vec::new());
            }
            let mut p = PacketOut::new(ids.cb_set_equipment);
            p.var_int(*entity_id);
            for (i, (slot, stack)) in slots.iter().enumerate() {
                let more = if i + 1 < slots.len() { 0x80 } else { 0 };
                p.u8(*slot | more);
                write_stack(v, &mut p, stack.as_ref());
            }
            vec![p]
        }
        ServerEvent::Health {
            health,
            food,
            saturation,
        } => {
            let mut p = PacketOut::new(ids.cb_set_health);
            p.f32(*health).var_int(*food).f32(*saturation);
            vec![p]
        }
        ServerEvent::Experience { bar, level, total } => {
            let mut p = PacketOut::new(ids.cb_set_experience);
            p.f32(*bar).var_int(*level).var_int(*total);
            vec![p]
        }
        ServerEvent::Time { age, time_of_day } => {
            let mut p = PacketOut::new(ids.cb_set_time);
            p.i64(*age);
            if v.gen >= Gen::V26_1 {
                // 26.1 replaced the time of day with world clocks: a map of
                // clock -> (total ticks, partial tick, rate). The overworld
                // clock is the only one a client in the overworld reads.
                match v.synced_index("minecraft:world_clock", "minecraft:overworld") {
                    Some(clock) => {
                        p.var_int(1)
                            .var_int(clock as i32)
                            .var_long(*time_of_day)
                            .f32(0.0)
                            .f32(1.0);
                    }
                    None => {
                        p.var_int(0);
                    }
                }
            } else {
                p.i64(*time_of_day).bool(true);
            }
            vec![p]
        }
        ServerEvent::WindowContents {
            window_id,
            state_id,
            slots,
            cursor,
        } => {
            let mut p = PacketOut::new(ids.cb_container_set_content);
            p.var_int(*window_id as i32)
                .var_int(*state_id)
                .var_int(slots.len() as i32);
            for s in slots {
                write_stack(v, &mut p, s.as_ref());
            }
            write_stack(v, &mut p, cursor.as_ref());
            vec![p]
        }
        ServerEvent::OpenWindow {
            window_id,
            menu,
            title,
        } => {
            let mut p = PacketOut::new(ids.cb_open_screen);
            p.var_int(*window_id as i32)
                .var_int(menu_id(v, *menu))
                .bytes(&crate::protocol::nbt::string(title).to_network());
            vec![p]
        }
        ServerEvent::CloseWindow(id) => {
            let mut p = PacketOut::new(ids.cb_container_close);
            p.var_int(*id as i32);
            vec![p]
        }
        ServerEvent::WindowProperty {
            window_id,
            property,
            value,
        } => {
            let mut p = PacketOut::new(ids.cb_container_set_data);
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
            let mut p = PacketOut::new(ids.cb_level_event);
            // Breaking a block (2001) names the block by state id, which is
            // the engine's until it reaches the wire.
            let data = if *event == 2001 {
                v.state(aether_world::BlockStateId(*data as u32)) as i32
            } else {
                *data
            };
            p.i32(*event)
                .i64(encode_position(*x as i64, *y as i64, *z as i64))
                .i32(data)
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
            let mut p = PacketOut::new(ids.cb_block_destruction);
            p.var_int(*entity_id)
                .i64(encode_position(*x as i64, *y as i64, *z as i64))
                .u8(*stage as u8);
            vec![p]
        }
        ServerEvent::Respawn { game_mode } => {
            let mut p = PacketOut::new(ids.cb_respawn);
            p.var_int(super::dimension_type_id(v))
                .string(super::registry::DIMENSION_NAME)
                .i64(0);
            // A byte until 26.3 and a VarInt since: the same byte for the two
            // modes this server has.
            p.u8(game_mode.wire());
            write_no_previous_mode(v, &mut p);
            p.bool(false)
                .bool(false)
                .bool(false) // no death location
                .var_int(0)
                .var_int(63)
                .u8(0); // keep nothing
                        // Then "start waiting for level chunks", as at login. Without it a
                        // 1.20.3+ client holds the loading screen after a respawn until
                        // it gives up — the world never appears.
            let mut wait = PacketOut::new(ids.cb_game_event);
            wait.u8(13).f32(0.0);
            vec![p, abilities(v, *game_mode), wait]
        }
        ServerEvent::GameModeChange(mode) => {
            let mut p = PacketOut::new(ids.cb_game_event);
            p.u8(3).f32(mode.wire() as f32);
            vec![p, abilities(v, *mode)]
        }
        ServerEvent::SetHeldSlot(slot) => {
            vec![inventory::held_item_packet(
                ids.cb_set_held_slot,
                *slot as i32,
            )]
        }
        ServerEvent::Teleport {
            x,
            y,
            z,
            yaw,
            pitch,
        } => {
            let mut p = PacketOut::new(ids.cb_player_position);
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
            let mut p = PacketOut::new(ids.cb_player_combat_kill);
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
            let mut p = PacketOut::new(ids.cb_explode);
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
                v.particle_explosion_emitter
            } else {
                v.particle_explosion
            };
            p.var_int(particle);
            write_sound(&mut p, "minecraft:entity.generic.explode");
            p.var_int(0); // no block particles
            if v.gen >= Gen::V26_3 {
                p.bool(true); // play the sound
            }
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
            let mut p = PacketOut::new(ids.cb_sound);
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
