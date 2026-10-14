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
        Menu::LargeChest => v.menu_double_chest,
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
    let Some(id) = v.wire_item(s.item) else {
        // The version has no item by that name — a modded item, or one from a
        // release this client does not have. An empty slot makes it look as
        // though the item vanished; show a stand-in carrying the real item's
        // name instead, so the player can still see and handle it. The server
        // keeps the real stack; only the picture changes.
        write_disguised_stack(v, p, s);
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

/// A stand-in item id the version always has, for an item it cannot name.
fn stand_in_id(v: &Version) -> i32 {
    v.item_id("minecraft:barrier")
        .or_else(|| v.item_id("minecraft:paper"))
        .unwrap_or(0)
}

/// Write a stack whose item this version lacks, disguised under its real name.
///
/// Two data components ride along: `custom_name` shows the real item's
/// English name where the icon cannot, and `lore` carries one
/// `Aether: minecraft:<id>` line so a disguised spear can be told from a
/// genuine trident. The component ids are per-version (`minecraft:lore` is
/// 11 in every release this codec speaks); a wrong id makes the client reject
/// the whole stack, so they are generated, never guessed.
fn write_disguised_stack(v: &Version, p: &mut PacketOut, s: &Stack) {
    let name = crate::protocol::compat::items::english(s.item)
        .map(str::to_owned)
        .unwrap_or_else(|| aether_world::registry::items::display_name(s.item));
    let marker = match s.item.vanilla_name() {
        Some(real) => format!("Aether: {real}"),
        None => "Aether: unknown item".to_owned(),
    };
    p.var_int(s.count as i32)
        .var_int(stand_in_id(v))
        .var_int(2) // custom_name and lore added...
        .var_int(0) // ...and none removed
        .var_int(v.component_custom_name)
        .bytes(&crate::protocol::nbt::string(&name).to_network())
        .var_int(v.component_lore)
        .var_int(1) // one lore line
        .bytes(&crate::protocol::nbt::string(&marker).to_network());
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
        ServerEvent::UpdateMobEffect {
            entity_id,
            effect,
            amplifier,
            duration,
            ambient,
            visible,
            show_icon,
        } => {
            let mut p = PacketOut::new(ids.cb_update_mob_effect);
            let flags = (*ambient as u8) | ((*visible as u8) << 1) | ((*show_icon as u8) << 2);
            p.var_int(*entity_id)
                .var_int(*effect as i32)
                .var_int(*amplifier as i32)
                .var_int(*duration)
                .u8(flags);
            vec![p]
        }
        ServerEvent::RemoveMobEffect { entity_id, effect } => {
            let mut p = PacketOut::new(ids.cb_remove_mob_effect);
            p.var_int(*entity_id).var_int(*effect as i32);
            vec![p]
        }
        ServerEvent::UpdateAttributes {
            entity_id,
            properties,
        } => {
            // A client given an attribute it does not know drops the whole
            // packet, so a version without this packet is left alone rather
            // than sent a malformed one.
            if ids.cb_update_attributes < 0 {
                return Some(Vec::new());
            }
            let mut p = PacketOut::new(ids.cb_update_attributes);
            p.var_int(*entity_id).var_int(properties.len() as i32);
            for (key, value, modifiers) in properties {
                p.var_int(*key).f64(*value).var_int(modifiers.len() as i32);
                for (id, amount, operation) in modifiers {
                    p.string(id).f64(*amount).u8(*operation as u8);
                }
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

    #[test]
    fn an_item_the_version_lacks_is_a_named_stand_in_not_an_empty_slot() {
        // A modded item: no vanilla name, so no release has it by name. The
        // slot must still carry a stack, or the item looks as though it
        // vanished.
        let v = &crate::protocol::modern::version::V1_21_11;
        let stack = Stack::new(
            aether_world::ItemTypeId(aether_world::registry::items::ITEM_BEYOND),
            1,
        );
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&stack));
        let body = p.body()[1..].to_vec(); // past the packet id
        let mut pin = crate::proto::PacketIn::new(&body);
        let count = pin.var_int().unwrap();
        let id = pin.var_int().unwrap();
        assert!(count > 0, "not an empty slot");
        assert_eq!(
            id,
            v.item_id("minecraft:barrier").unwrap(),
            "a real stand-in item"
        );
        assert_eq!(pin.var_int().unwrap(), 2, "custom_name and lore added");
        assert_eq!(pin.var_int().unwrap(), 0, "none removed");
        assert_eq!(
            pin.var_int().unwrap(),
            v.component_custom_name,
            "the real name rides in custom_name"
        );
        // Skip the custom_name value: an anonymous NBT string (tag, length,
        // bytes) — the lore component id follows it.
        assert_eq!(pin.u8().unwrap(), 0x08, "TAG_String");
        let name_len = pin.u16().unwrap() as usize;
        for _ in 0..name_len {
            pin.u8().unwrap();
        }
        assert_eq!(
            pin.var_int().unwrap(),
            v.component_lore,
            "and the lore marker follows it"
        );
        assert!(contains(&body, b"Aether: "), "the marker names the item");
    }

    #[test]
    fn a_disguised_engine_item_carries_its_real_name_and_marker() {
        // The lore marker names the *real* item, so a disguised spear can be
        // told from the genuine trident it is shown as.
        let v = &crate::protocol::modern::version::V1_21_11;
        let spear = (0..aether_world::registry::items::ITEM_BEYOND)
            .map(aether_world::ItemTypeId)
            .find(|i| i.vanilla_name() == Some("minecraft:iron_spear"));
        let Some(spear) = spear else {
            return; // the engine pin has spears; nothing to check otherwise
        };
        // V1_21_11 has the spear, so drive the disguise helper directly, as a
        // version that lacked it would.
        let mut p = PacketOut::new(0);
        write_disguised_stack(v, &mut p, &Stack::new(spear, 1));
        let body = p.body()[1..].to_vec();
        assert!(
            contains(&body, b"Aether: minecraft:iron_spear"),
            "the marker carries the real item id"
        );
        assert!(
            contains(&body, b"Iron Spear"),
            "and the English name rides in custom_name"
        );
    }

    /// Whether `hay` contains `needle` as a contiguous run.
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
}
