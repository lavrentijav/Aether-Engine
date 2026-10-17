//! 1.21.11 encoding of the gameplay events: entities, combat, windows, time.
//!
//! Packet ids and field layouts are from `minecraft-data` `pc/1.21.11/
//! protocol.json`, the same source as the rest of this codec.

use super::version::{Gen, Version};
use super::{angle, encode_position, inventory};
use crate::inventory::Stack;
use crate::proto::PacketOut;
use crate::protocol::nbt::Nbt;
use crate::protocol::{GameMode, Menu, MetaValue, ParticleOptions, ServerEvent};

/// `minecraft:menu` registry ids.
fn menu_id(v: &Version, m: Menu) -> i32 {
    match m {
        Menu::Chest1 => v.menu_chest1,
        Menu::Chest2 => v.menu_chest2,
        Menu::Chest => v.menu_chest,
        Menu::Chest4 => v.menu_chest4,
        Menu::Chest5 => v.menu_chest5,
        Menu::LargeChest => v.menu_double_chest,
        Menu::Crafting => v.menu_crafting,
        Menu::Furnace => v.menu_furnace,
        Menu::Hopper => v.menu_hopper,
        Menu::Dispenser => v.menu_dispenser,
        Menu::BrewingStand => v.menu_brewing_stand,
    }
}

/// The effect id this version puts on the wire. Protocol 764 (1.20.2) still
/// numbered effects the way the pre-registry `MobEffect.register` did, with
/// Speed as 1, so its wire id is the engine's 0-based registry index plus
/// one; 1.20.3 (765) moved to the 0-based registry index the engine uses
/// everywhere else. Getting this wrong shifts every effect by one and the
/// client announces a different potion entirely (1.20.2 showed "Nausea II"
/// for Regeneration, "Health Boost" for Absorption).
fn wire_effect_id(v: &Version, effect: u8) -> i32 {
    if v.gen == Gen::V1_20_2 {
        effect as i32 + 1
    } else {
        effect as i32
    }
}

/// Append a stack in this version's slot format.
pub fn write_stack(v: &Version, p: &mut PacketOut, stack: Option<&Stack>) {
    // Before 1.20.5 a slot is a presence flag, an item id and count, then an
    // optional NBT compound (`Slot`); 1.20.5 replaced the NBT with the two
    // data-component lists.
    if v.gen < Gen::V1_20_5 {
        write_legacy_stack(v, p, stack.filter(|s| s.count > 0));
        return;
    }
    let Some(s) = stack.filter(|s| s.count > 0) else {
        inventory::write_empty(p);
        return;
    };
    match v.wire_stack_item(s.item) {
        Some(w) if !w.disguised => {
            // A potion, splash potion or lingering potion carries its contents
            // in the `minecraft:potion_contents` component; a worn tool
            // carries its damage. Both are components added to the default set.
            let potion = potion_contents(s);
            // A plugin menu item shows a name, lore, glint and model id that
            // the real item does not. They are extra components on top of
            // damage and potion.
            let display = s.display.as_deref();
            let name = display.and_then(|d| d.name.as_deref());
            let lore: &[String] = display.map(|d| d.lore.as_slice()).unwrap_or(&[]);
            let glint = display.is_some_and(|d| d.glint);
            let model = display.and_then(|d| d.model);
            let added = (s.damage > 0) as i32
                + potion.is_some() as i32
                + name.is_some() as i32
                + !lore.is_empty() as i32
                + glint as i32
                + model.is_some() as i32;
            p.var_int(s.count as i32)
                .var_int(w.id)
                .var_int(added)
                .var_int(0);
            if s.damage > 0 {
                p.var_int(v.component_damage).var_int(s.damage as i32);
            }
            if let Some(potion) = potion {
                write_potion_contents(v, p, potion);
            }
            if let Some(name) = name {
                p.var_int(v.component_custom_name)
                    .bytes(&crate::protocol::nbt::string(name).to_network());
            }
            if !lore.is_empty() {
                p.var_int(v.component_lore).var_int(lore.len() as i32);
                for line in lore {
                    p.bytes(&crate::protocol::nbt::string(line).to_network());
                }
            }
            if glint {
                // `enchantment_glint_override` is a plain bool: true draws the
                // enchantment shimmer without naming an enchantment.
                p.var_int(v.component_glint_override).bool(true);
            }
            if let Some(model) = model {
                p.var_int(v.component_custom_model_data);
                write_custom_model_data(v, p, model);
            }
        }
        // A stand-in for a *different* item (the compat layer's substitution,
        // e.g. a spear shown as a trident): the icon is the stand-in but the
        // real name and marker ride along.
        Some(w) => write_disguised_stack(v, p, s, w.id),
        // The version has no item by that name at all — a modded item, or one
        // this build does not know. An empty slot makes it look as though the
        // item vanished; show a generic stand-in carrying the real item's name
        // instead. The server keeps the real stack; only the picture changes.
        None => write_disguised_stack(v, p, s, stand_in_id(v)),
    }
}

/// The `minecraft:potion` registry index of a potion stack, or `None` for
/// anything else. A potion item with no contents is water, which is what the
/// item's own default component holds (`PotionItem.getDefaultInstance`).
fn potion_contents(s: &Stack) -> Option<u16> {
    let name = s.item.vanilla_name()?;
    if !crate::game::brewing::is_potion_item(name) {
        return None;
    }
    Some(s.potion.unwrap_or(crate::game::brewing::WATER))
}

/// Append `minecraft:potion_contents`, the component that names the potion.
///
/// Its stream codec is `PotionContents.STREAM_CODEC`: an optional potion (a
/// boolean then the registry VarInt), an optional custom colour, a list of
/// custom effects and an optional custom name — the client derives the
/// effects, name and colour from the potion, so the last three are empty.
fn write_potion_contents(v: &Version, p: &mut PacketOut, potion: u16) {
    p.var_int(v.component_potion_contents)
        .bool(true)
        .var_int(potion as i32)
        .bool(false) // no custom colour: the client computes it
        .var_int(0) // no custom effects
        .bool(false); // no custom name
}

/// Append the payload of `minecraft:custom_model_data` (the component id is
/// written by the caller).
///
/// The component's stream form changed at 1.21.4 (protocol 769): before it was
/// a single VarInt, from 1.21.4 a record of four lists (`floats`, `flags`,
/// `strings`, `colors`). An integer model id maps onto a one-element `floats`
/// list — vanilla's own conversion — so an old-style numeric override keeps
/// working after the change. `colors` is present from 1.21.4, so all four
/// lists are always written there, empty but for `floats`.
fn write_custom_model_data(v: &Version, p: &mut PacketOut, model: i32) {
    if v.protocol < 769 {
        p.var_int(model);
        return;
    }
    p.var_int(1).f32(model as f32); // floats: the legacy int as one float
    p.var_int(0).var_int(0).var_int(0); // flags, strings, colors
}

/// A stand-in item id the version always has, for an item it cannot name.
fn stand_in_id(v: &Version) -> i32 {
    v.item_id("minecraft:barrier")
        .or_else(|| v.item_id("minecraft:paper"))
        .unwrap_or(0)
}

/// Write a stack under `id`, disguised under the real item's name.
///
/// Two data components ride along: `custom_name` shows the real item's
/// English name where the icon cannot, and `lore` carries one
/// `Aether: minecraft:<id>` line so a disguised spear can be told from a
/// genuine trident. The component ids are per-version; a wrong id makes the
/// client reject the whole stack, so they are generated, never guessed.
fn write_disguised_stack(v: &Version, p: &mut PacketOut, s: &Stack, id: i32) {
    let (name, marker) = disguise_labels(s);
    p.var_int(s.count as i32)
        .var_int(id)
        .var_int(2) // custom_name and lore added...
        .var_int(0) // ...and none removed
        .var_int(v.component_custom_name)
        .bytes(&crate::protocol::nbt::string(&name).to_network())
        .var_int(v.component_lore)
        .var_int(1) // one lore line
        .bytes(&crate::protocol::nbt::string(&marker).to_network());
}

/// The English name and the `Aether:` marker a disguised stack carries.
fn disguise_labels(s: &Stack) -> (String, String) {
    let name = crate::protocol::compat::items::english(s.item)
        .map(str::to_owned)
        .unwrap_or_else(|| aether_world::registry::items::display_name(s.item));
    let marker = match s.item.vanilla_name() {
        Some(real) => format!("Aether: {real}"),
        None => "Aether: unknown item".to_owned(),
    };
    (name, marker)
}

/// A `{"text": ...}` JSON text component, the pre-1.20.3 form.
pub(super) fn json_text(text: &str) -> String {
    format!("{{\"text\":\"{}\"}}", crate::protocol::json_escape(text))
}

/// Append a pre-1.20.5 `Slot`: presence flag, item id, `i8` count, then an
/// optional NBT compound.
///
/// The compound carries `Damage` for a worn tool and, for a stand-in, the
/// `display` compound whose `Name` and `Lore` are JSON text components (the
/// NBT-component form arrives with 1.20.3).
fn write_legacy_stack(v: &Version, p: &mut PacketOut, stack: Option<&Stack>) {
    let Some(s) = stack else {
        p.bool(false);
        return;
    };
    let (id, disguised) = match v.wire_stack_item(s.item) {
        Some(w) => (w.id, w.disguised),
        None => (stand_in_id(v), true),
    };
    p.bool(true).var_int(id).u8(s.count);
    let mut fields: Vec<(String, Nbt)> = Vec::new();
    if s.damage > 0 {
        fields.push(("Damage".to_owned(), Nbt::Int(s.damage as i32)));
    }
    // A potion names itself in the NBT `Potion` tag before 1.20.5, where
    // 1.20.5+ uses the `minecraft:potion_contents` component. The value is the
    // `minecraft:potion` registry *name* — which is why the older form needs
    // no component id and why the registry ordering cannot drift here.
    if let Some(id) = potion_contents(s) {
        if let Some(def) = crate::game::brewing::def_of(id) {
            fields.push((
                "Potion".to_owned(),
                Nbt::Str(format!("minecraft:{}", def.name)),
            ));
        }
    }
    if disguised {
        let (name, marker) = disguise_labels(s);
        fields.push((
            "display".to_owned(),
            crate::protocol::nbt::compound([
                ("Name", Nbt::Str(json_text(&name))),
                ("Lore", Nbt::List(vec![Nbt::Str(json_text(&marker))])),
            ]),
        ));
    } else if let Some(d) = s.display.as_deref() {
        // A plugin menu item on a pre-1.20.5 client: the name and lore ride in
        // the NBT `display` tag rather than the data components.
        let mut display: Vec<(String, Nbt)> = Vec::new();
        if let Some(name) = &d.name {
            display.push(("Name".to_owned(), Nbt::Str(json_text(name))));
        }
        if !d.lore.is_empty() {
            display.push((
                "Lore".to_owned(),
                Nbt::List(d.lore.iter().map(|l| Nbt::Str(json_text(l))).collect()),
            ));
        }
        if !display.is_empty() {
            fields.push(("display".to_owned(), Nbt::Compound(display)));
        }
        // Glint has no override before 1.20.5: it is drawn when the item
        // carries any enchantment, so a hidden placeholder enchantment is
        // added and `HideFlags` (bit 1 = hide enchantments) keeps it out of
        // the tooltip. `lure` is a real enchantment every release knows.
        if d.glint {
            fields.push((
                "Enchantments".to_owned(),
                Nbt::List(vec![Nbt::Compound(vec![
                    ("id".to_owned(), Nbt::Str("minecraft:lure".to_owned())),
                    ("lvl".to_owned(), Nbt::Short(1)),
                ])]),
            ));
            fields.push(("HideFlags".to_owned(), Nbt::Int(1)));
        }
        // A numeric model override is the plain `CustomModelData` tag before
        // 1.20.5 (since 1.14).
        if let Some(model) = d.model {
            fields.push(("CustomModelData".to_owned(), Nbt::Int(model)));
        }
    }
    if fields.is_empty() {
        p.u8(0); // no NBT: a lone TAG_End
    } else {
        p.bytes(&Nbt::Compound(fields).to_network());
    }
}

/// Append a pre-1.20.5 container slot carrying a display name.
///
/// A server-driven window says what it holds through the display name; the
/// pre-1.20.5 form puts it in the NBT `display.Name` as a JSON text component.
pub fn write_legacy_container_slot(p: &mut PacketOut, item_id: i32, count: u8, name: &str) {
    p.bool(true).var_int(item_id).u8(count);
    p.bytes(
        &Nbt::Compound(vec![(
            "display".to_owned(),
            crate::protocol::nbt::compound([("Name", Nbt::Str(json_text(name)))]),
        )])
        .to_network(),
    );
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
    // Before 1.21.2 the absolute position was `entity_teleport`, with the
    // entity id first and no velocity deltas; 1.21.2 replaced it with
    // `entity_position_sync`, whose extra three doubles are the delta.
    if v.gen < Gen::V1_21_3 {
        let mut p = PacketOut::new(v.packets.cb_entity_teleport);
        p.var_int(entity_id)
            .f64(x)
            .f64(y)
            .f64(z)
            .u8(angle(yaw))
            .u8(angle(pitch))
            .bool(on_ground);
        return p;
    }
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

/// An entity velocity, in the release's own form.
///
/// 1.21.9 replaced the three `i16` components (1/8000 of a block per tick)
/// with the quantized [`write_lp_vec3`]. The older codecs still use the
/// shorts, and they sit in the same place in the packet either way.
pub fn write_velocity(v: &Version, p: &mut PacketOut, velocity: (f64, f64, f64)) {
    if v.gen >= Gen::V1_21_9 {
        write_lp_vec3(p, velocity);
    } else {
        for c in [velocity.0, velocity.1, velocity.2] {
            p.u16((c.clamp(-3.9, 3.9) * 8000.0).round() as i16 as u16);
        }
    }
}

/// A stable UUID for an attribute modifier named by identifier.
///
/// 1.20.5 and older name a modifier by UUID on the wire. The value is only an
/// identity, so one is derived from the identifier — the same modifier keeps
/// the same id across a session.
fn modifier_uuid(name: &str) -> u128 {
    let mut h: u128 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// An inline sound event: holder id 0, then the identifier and no range.
fn write_sound(p: &mut PacketOut, name: &str) {
    p.var_int(0).string(name).bool(false);
}

/// The `level_particles` packet id of a release in the shared gameplay path.
///
/// The id is not in the generated [`super::version::Packets`] table yet
/// because the packet is only used by CMD-19; the values are the releases'
/// own packet reports.
fn particle_packet_id(protocol: i32) -> Option<i32> {
    Some(match protocol {
        764 | 765 => 39,
        766 | 767 => 41,
        768 | 769 => 42,
        770..=772 => 41,
        773 | 774 => 46,
        775 | 776 => 47,
        777 => 48,
        _ => return None,
    })
}

/// The `stop_sound` packet id of a release in the shared gameplay path.
fn stop_sound_packet_id(protocol: i32) -> Option<i32> {
    Some(match protocol {
        764 => 102,
        765 => 104,
        766 | 767 => 106,
        768 | 769 => 113,
        770..=772 => 112,
        773 | 774 => 117,
        775 | 776 => 119,
        777 => 121,
        _ => return None,
    })
}

/// Append a particle's type-specific payload.
///
/// The command layer parses vanilla's options object and carries it as a
/// [`ParticleOptions`]; this writes the shape the release expects. The two
/// shapes that moved are the dust colours (four floats through 1.21.3, one
/// int from 1.21.4) and the vibration source kind (a registry string through
/// 1.21.3, a VarInt from 1.21.4). A field a release does not have (an
/// `entity_effect` colour before 1.20.5, the newer `effect` fields before
/// 1.21.9) is simply not written.
fn write_particle_options(v: &Version, p: &mut PacketOut, options: &ParticleOptions) {
    let rgb = |color: i32| {
        (
            ((color >> 16) & 0xFF) as f32 / 255.0,
            ((color >> 8) & 0xFF) as f32 / 255.0,
            (color & 0xFF) as f32 / 255.0,
        )
    };
    match options {
        ParticleOptions::None => {}
        ParticleOptions::Block(state) => {
            p.var_int(v.state(*state) as i32);
        }
        ParticleOptions::Dust { color, scale } => {
            if v.protocol <= 768 {
                let (r, g, b) = rgb(*color);
                p.f32(r).f32(g).f32(b).f32(*scale);
            } else {
                p.i32(*color).f32(*scale);
            }
        }
        ParticleOptions::DustColorTransition { from, to, scale } => {
            if v.protocol <= 768 {
                let (fr, fg, fb) = rgb(*from);
                let (tr, tg, tb) = rgb(*to);
                p.f32(fr)
                    .f32(fg)
                    .f32(fb)
                    .f32(*scale)
                    .f32(tr)
                    .f32(tg)
                    .f32(tb);
            } else {
                p.i32(*from).i32(*to).f32(*scale);
            }
        }
        ParticleOptions::EntityEffect { color } => {
            // 1.20.2-1.20.4's particle payload has no entity-effect entry.
            if v.protocol >= 766 {
                p.i32(*color);
            }
        }
        ParticleOptions::Item { item, count } => {
            let stack = crate::inventory::Stack::new(*item, *count);
            write_stack(v, p, Some(&stack));
        }
        ParticleOptions::SculkCharge { roll } => {
            p.f32(*roll);
        }
        ParticleOptions::Shriek { delay } => {
            p.var_int(*delay);
        }
        ParticleOptions::VibrationBlock { x, y, z, ticks } => {
            if v.protocol <= 768 {
                p.string("minecraft:block")
                    .i64(encode_position(*x as i64, *y as i64, *z as i64))
                    .var_int(*ticks);
            } else {
                p.var_int(0) // block position source
                    .i64(encode_position(*x as i64, *y as i64, *z as i64))
                    .var_int(*ticks);
            }
        }
        ParticleOptions::Trail {
            x,
            y,
            z,
            color,
            duration,
        } => {
            // The trail particle arrived in 1.21.4 together with its fields.
            p.f64(*x).f64(*y).f64(*z).i32(*color).var_int(*duration);
        }
        ParticleOptions::TintedLeaves { color } | ParticleOptions::Flash { color } => {
            if v.protocol >= 773 {
                p.i32(*color);
            }
        }
        ParticleOptions::DragonBreath { power } => {
            // The power field arrived in 1.21.9; before that the payload is void.
            if v.protocol >= 773 {
                p.f32(*power);
            }
        }
        ParticleOptions::Effect { color, power }
        | ParticleOptions::InstantEffect { color, power } => {
            if v.protocol >= 773 {
                p.i32(*color).f32(*power);
            }
        }
    }
}

fn abilities(v: &Version, mode: GameMode) -> PacketOut {
    let mut p = PacketOut::new(v.packets.cb_player_abilities);
    let flags = match mode {
        GameMode::Creative => 0x01 | 0x04 | 0x08,
        GameMode::Spectator => 0x01 | 0x04,
        GameMode::Adventure | GameMode::Survival => 0,
    };
    p.u8(flags).f32(0.05).f32(0.10);
    p
}

/// The world-border and spawn-position packet ids of one release.
///
/// They are not in the shared [`super::version::Packets`] table yet: the
/// world border arrived before it and only 1.21.11 has been wired, so a newer
/// release simply renders no border packet rather than one at a wrong id.
struct BorderIds {
    initialize: i32,
    center: i32,
    size: i32,
    warning_delay: i32,
    warning_reach: i32,
    spawn_position: i32,
}

fn border_ids(v: &Version) -> Option<BorderIds> {
    if v.gen == Gen::V1_21_11 {
        Some(BorderIds {
            initialize: 0x2a,
            center: 0x56,
            size: 0x58,
            warning_delay: 0x59,
            warning_reach: 0x5a,
            spawn_position: 0x5f,
        })
    } else {
        None
    }
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
            // 1.21.9 moved the velocity from the end of the packet to just
            // after the position, and quantized it.
            if v.gen >= Gen::V1_21_9 {
                write_velocity(v, &mut p, *velocity);
            }
            p.u8(angle(*pitch))
                .u8(angle(*yaw))
                .u8(angle(*yaw))
                .var_int(*data);
            if v.gen < Gen::V1_21_9 {
                write_velocity(v, &mut p, *velocity);
            }
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
                    MetaValue::Pose(pose) => {
                        // The pose serializer is 20 before 1.20.5 (1.20.5 grew
                        // a `particles` serializer ahead of it), 21 from 1.20.5
                        // to 1.21.8, and 20 again from 1.21.9.
                        let id = if v.gen >= Gen::V1_21_9 || v.gen < Gen::V1_20_5 {
                            20
                        } else {
                            21
                        };
                        p.var_int(id).var_int(*pose);
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
            write_velocity(v, &mut p, *velocity);
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
            p.var_int(*entity_id).var_int(wire_effect_id(v, *effect));
            if v.gen < Gen::V1_20_5 {
                // Pre-1.20.5: the amplifier is a signed byte and the flags
                // byte is followed by an *optional* factor-data compound,
                // which this server never sends — but the presence bool is
                // not optional, and omitting it leaves the packet a byte
                // short so the client rejects it.
                p.u8(*amplifier).var_int(*duration).u8(flags).bool(false);
            } else {
                p.var_int(*amplifier as i32).var_int(*duration).u8(flags);
            }
            vec![p]
        }
        ServerEvent::RemoveMobEffect { entity_id, effect } => {
            let mut p = PacketOut::new(ids.cb_remove_mob_effect);
            p.var_int(*entity_id).var_int(wire_effect_id(v, *effect));
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
            // Before 1.20.5 an attribute is named by its identifier on the
            // wire; the registry index arrived with 1.20.5. Drop a property
            // whose name this release has none for.
            if v.gen < Gen::V1_20_5 {
                let named: Vec<(&str, f64, &Vec<(String, f64, i8)>)> = properties
                    .iter()
                    .filter_map(|(key, value, modifiers)| {
                        let name = v
                            .attribute_names
                            .get(*key as usize)
                            .copied()
                            .filter(|n| !n.is_empty())?;
                        Some((name, *value, modifiers))
                    })
                    .collect();
                let mut p = PacketOut::new(ids.cb_update_attributes);
                p.var_int(*entity_id).var_int(named.len() as i32);
                for (name, value, modifiers) in named {
                    p.string(name).f64(value).var_int(modifiers.len() as i32);
                    for (id, amount, operation) in modifiers {
                        p.uuid(modifier_uuid(id)); // modifiers are UUIDs here
                        p.f64(*amount).u8(*operation as u8);
                    }
                }
                return Some(vec![p]);
            }
            // The attribute registry was reordered between releases, and
            // 1.20.5 still names a modifier by UUID where 1.21 uses an
            // identifier. Translate each key through the version's map and
            // drop a property the release does not have.
            let mapped: Vec<(i32, f64, &Vec<(String, f64, i8)>)> = properties
                .iter()
                .filter_map(|(key, value, modifiers)| {
                    let key = match v.attribute_map.is_empty() {
                        true => *key,
                        false => match v.attribute_map.get(*key as usize) {
                            Some(&k) if k >= 0 => k,
                            _ => return None,
                        },
                    };
                    Some((key, *value, modifiers))
                })
                .collect();
            let mut p = PacketOut::new(ids.cb_update_attributes);
            p.var_int(*entity_id).var_int(mapped.len() as i32);
            for (key, value, modifiers) in mapped {
                p.var_int(key).f64(value).var_int(modifiers.len() as i32);
                for (id, amount, operation) in modifiers {
                    if v.gen <= Gen::V1_20_5 {
                        // 1.20.5 wants a UUID; derive a stable one from the
                        // identifier so the same modifier keeps its id.
                        p.uuid(modifier_uuid(id));
                    } else {
                        p.string(id);
                    }
                    p.f64(*amount).u8(*operation as u8);
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
                p.i64(*time_of_day);
                // `tickDayTime` was added in 1.21.2; writing it before that
                // leaves a stray byte the client reads as the next packet.
                if v.gen >= Gen::V1_21_3 {
                    p.bool(true);
                }
            }
            vec![p]
        }
        ServerEvent::Weather { rain, thunder } => {
            // Start or stop the rain, then set both levels. `clear` must also
            // lower the thunder, or the client keeps its storm sky.
            let mut toggle = PacketOut::new(ids.cb_game_event);
            toggle.u8(if *rain { 1 } else { 2 }).f32(0.0);
            let mut rain_level = PacketOut::new(ids.cb_game_event);
            rain_level.u8(7).f32(if *rain { 1.0 } else { 0.0 });
            let mut thunder_level = PacketOut::new(ids.cb_game_event);
            thunder_level.u8(8).f32(if *thunder { 1.0 } else { 0.0 });
            vec![toggle, rain_level, thunder_level]
        }
        ServerEvent::WorldBorder {
            x,
            z,
            old_diameter,
            new_diameter,
            speed,
            warning_blocks,
            warning_time,
        } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.initialize);
            p.f64(*x)
                .f64(*z)
                .f64(*old_diameter)
                .f64(*new_diameter)
                // `speed` is a VarLong in `ClientboundInitializeBorderPacket`.
                .var_long(*speed)
                // `getAbsoluteMaxSize`, the portal reach vanilla sends.
                .var_int(29_999_984)
                .var_int(*warning_blocks)
                .var_int(*warning_time);
            vec![p]
        }
        ServerEvent::WorldBorderCenter { x, z } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.center);
            p.f64(*x).f64(*z);
            vec![p]
        }
        ServerEvent::WorldBorderSize { diameter } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.size);
            p.f64(*diameter);
            vec![p]
        }
        ServerEvent::WorldBorderWarningDistance { blocks } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.warning_reach);
            p.var_int(*blocks);
            vec![p]
        }
        ServerEvent::WorldBorderWarningTime { ticks } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.warning_delay);
            p.var_int(*ticks);
            vec![p]
        }
        ServerEvent::SpawnPosition { x, y, z, yaw } => {
            let Some(ids) = border_ids(v) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(ids.spawn_position);
            p.string("minecraft:overworld")
                .i64(encode_position(i64::from(*x), i64::from(*y), i64::from(*z)))
                .f32(*yaw)
                .f32(0.0);
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
            p.var_int(*window_id as i32).var_int(menu_id(v, *menu));
            // 1.20.3 made the title an NBT text component; 1.20.2 still sends
            // the JSON string form.
            if v.gen < Gen::V1_20_4 {
                p.string(&json_text(title));
            } else {
                p.bytes(&crate::protocol::nbt::string(title).to_network());
            }
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
            // Before 1.20.5 the respawn packet predates the `SpawnInfo`
            // compound: the dimension type is named directly and the flags
            // and death location follow it.
            if v.gen < Gen::V1_20_5 {
                let mut p = PacketOut::new(ids.cb_respawn);
                p.string(super::registry::DIMENSION_NAME) // dimension type
                    .string(super::registry::DIMENSION_NAME) // world name
                    .i64(0) // hashed seed
                    .u8(game_mode.wire()) // game mode (an i8: same byte)
                    .u8(0xFF) // previous game mode: none
                    .bool(false) // not a debug world
                    .bool(false) // not superflat
                    .bool(false) // no death location
                    .var_int(0) // portal cooldown
                    .bool(false); // keep nothing
                                  // Game event 13 ("level chunks load start") arrives with
                                  // 1.20.3; a 1.20.2 client would not know it.
                if v.gen >= Gen::V1_20_4 {
                    let mut wait = PacketOut::new(ids.cb_game_event);
                    wait.u8(13).f32(0.0);
                    return Some(vec![p, abilities(v, *game_mode), wait]);
                }
                return Some(vec![p, abilities(v, *game_mode)]);
            }
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
                .var_int(0); // portal cooldown
                             // 1.21.2 added the sea level to the SpawnInfo; 1.20.5-1.21.1 do not
                             // have it, and writing it would shift the next byte.
            if v.gen >= Gen::V1_21_3 {
                p.var_int(63);
            }
            p.u8(0); // keep nothing
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
            if v.gen >= Gen::V1_21_3 {
                // 1.21.2 put the teleport id first and added explicit deltas.
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
            } else {
                // 1.20.5-1.21.1: position, look, relative flags, then the id.
                p.f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .f32(*yaw)
                    .f32(*pitch)
                    .u8(0) // all absolute
                    .var_int(2);
            }
            vec![p]
        }
        ServerEvent::DeathMessage { entity_id, text } => {
            let mut p = PacketOut::new(ids.cb_player_combat_kill);
            p.var_int(*entity_id);
            // 1.20.3 switched the death message to an NBT component.
            if v.gen < Gen::V1_20_4 {
                p.string(&json_text(text));
            } else {
                p.bytes(&crate::protocol::nbt::string(text).to_network());
            }
            vec![p]
        }
        ServerEvent::Explosion {
            x,
            y,
            z,
            radius,
            knockback,
        } => {
            // The explosion packet was rebuilt in 1.21.9 (block-offset array,
            // player motion, two particle ids, an inline sound holder); the
            // older layout also names particles by registry id, which this
            // build has no table for. The event is cosmetic — damage and
            // knockback are applied by the game layer — so it is left
            // unsent on the older codecs rather than sent malformed.
            if v.gen < Gen::V1_21_11 {
                return Some(Vec::new());
            }
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
        ServerEvent::Particle {
            name,
            x,
            y,
            z,
            dx,
            dy,
            dz,
            speed,
            count,
            force,
            options,
        } => {
            let Some(id) = crate::protocol::resources::particle_id(v.protocol, name) else {
                // This release has no particle by that name. It is cosmetic;
                // sending another release's id would draw the wrong thing.
                return Some(Vec::new());
            };
            let Some(packet_id) = particle_packet_id(v.protocol) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(packet_id);
            if v.protocol <= 765 {
                // 1.20.2-1.20.4: the id and distance flag lead the packet.
                p.var_int(id)
                    .bool(*force)
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .f32(*dx)
                    .f32(*dy)
                    .f32(*dz)
                    .f32(*speed)
                    .i32(*count);
                write_particle_options(v, &mut p, options);
            } else {
                p.bool(*force);
                // 1.21.4 added `alwaysShow`; 1.20.5-1.21.3 have only the
                // long-distance flag. `ParticleCommand` always passes
                // `alwaysShow = false`, so `force` only sets the distance
                // flag.
                if v.protocol >= 769 {
                    p.bool(false);
                }
                p.f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .f32(*dx)
                    .f32(*dy)
                    .f32(*dz)
                    .f32(*speed)
                    .i32(*count)
                    .var_int(id);
                write_particle_options(v, &mut p, options);
            }
            vec![p]
        }
        ServerEvent::StopSound { source, sound } => {
            let Some(packet_id) = stop_sound_packet_id(v.protocol) else {
                return Some(Vec::new());
            };
            let mut p = PacketOut::new(packet_id);
            let flags = u8::from(source.is_some()) | (u8::from(sound.is_some()) << 1);
            p.u8(flags);
            if let Some(source) = source {
                p.var_int(*source as i32);
            }
            if let Some(sound) = sound {
                p.string(sound);
            }
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
    fn a_menu_item_writes_glint_and_model_data_components() {
        // 1.21.11: glint is the boolean `enchantment_glint_override`, model
        // data the four-list record. Nothing else is set, so exactly two
        // components ride along.
        for v in [
            &crate::protocol::modern::version::V1_21_5,
            // 1.21.4 is protocol 769, exactly where the record form begins: it
            // must be exercised, or a 1.21.4 client would desync on the field
            // count.
            &crate::protocol::modern::version::V1_21_4,
            &crate::protocol::modern::version::V1_21_11,
            &crate::protocol::modern::version::V26_1,
        ] {
            let stack = Stack::menu_item(
                aether_world::ItemTypeId::vanilla("minecraft:stone").unwrap(),
                1,
                None,
                Vec::new(),
                true,
                Some(7),
            );
            let mut p = PacketOut::new(0);
            write_stack(v, &mut p, Some(&stack));
            let body = p.body()[1..].to_vec();
            let mut pin = crate::proto::PacketIn::new(&body);
            assert!(pin.var_int().unwrap() > 0);
            pin.var_int().unwrap(); // item id
            assert_eq!(
                pin.var_int().unwrap(),
                2,
                "glint and model data, {}",
                v.name
            );
            assert_eq!(pin.var_int().unwrap(), 0, "none removed");
            assert_eq!(pin.var_int().unwrap(), v.component_glint_override);
            assert!(pin.bool().unwrap(), "glint is on");
            assert_eq!(pin.var_int().unwrap(), v.component_custom_model_data);
            // From 1.21.4 the model data is a record; the int becomes one
            // float, and the other three lists are empty.
            assert_eq!(pin.var_int().unwrap(), 1, "one float, {}", v.name);
            assert_eq!(pin.f32().unwrap(), 7.0);
            assert_eq!(pin.var_int().unwrap(), 0, "no flags");
            assert_eq!(pin.var_int().unwrap(), 0, "no strings");
            assert_eq!(pin.var_int().unwrap(), 0, "no colours");
        }
    }

    #[test]
    fn pre_1_21_4_model_data_is_a_plain_var_int() {
        // 1.20.5/1.21.1/1.21.3: `custom_model_data` is still a single VarInt.
        for v in [
            &crate::protocol::modern::version::V1_20_5,
            &crate::protocol::modern::version::V1_21_1,
            &crate::protocol::modern::version::V1_21_3,
        ] {
            let stack = Stack::menu_item(
                aether_world::ItemTypeId::vanilla("minecraft:stone").unwrap(),
                1,
                None,
                Vec::new(),
                false,
                Some(1234),
            );
            let mut p = PacketOut::new(0);
            write_stack(v, &mut p, Some(&stack));
            let body = p.body()[1..].to_vec();
            let mut pin = crate::proto::PacketIn::new(&body);
            pin.var_int().unwrap();
            pin.var_int().unwrap();
            assert_eq!(pin.var_int().unwrap(), 1, "model data only, {}", v.name);
            assert_eq!(pin.var_int().unwrap(), 0);
            assert_eq!(pin.var_int().unwrap(), v.component_custom_model_data);
            assert_eq!(pin.var_int().unwrap(), 1234);
        }
    }

    #[test]
    fn a_legacy_menu_item_writes_glint_and_model_as_nbt() {
        // 1.20.2 has no data components: glint is a hidden placeholder
        // enchantment and the model id the `CustomModelData` tag.
        let v = &crate::protocol::modern::version::V1_20_2;
        let stack = Stack::menu_item(
            aether_world::ItemTypeId::vanilla("minecraft:stone").unwrap(),
            1,
            Some("Hello".to_string()),
            Vec::new(),
            true,
            Some(9),
        );
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&stack));
        let body = p.body()[1..].to_vec();
        assert!(
            contains(&body, b"Enchantments"),
            "a placeholder enchantment"
        );
        assert!(contains(&body, b"minecraft:lure"), "a real enchantment id");
        assert!(contains(&body, b"HideFlags"), "the enchant line is hidden");
        assert!(contains(&body, b"CustomModelData"), "the model id tag");
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
        write_disguised_stack(v, &mut p, &Stack::new(spear, 1), stand_in_id(v));
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

    #[test]
    fn a_potion_stack_carries_potion_contents() {
        let v = &crate::protocol::modern::version::V1_21_11;
        let potion = crate::game::brewing::potion_id("minecraft:long_swiftness").unwrap();
        let stack = Stack::potion("minecraft:potion", potion, 1);
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&stack));
        let body = p.body()[1..].to_vec(); // past the packet id
        let mut pin = crate::proto::PacketIn::new(&body);
        assert_eq!(pin.var_int().unwrap(), 1, "count");
        assert_eq!(
            pin.var_int().unwrap(),
            v.item_id("minecraft:potion").unwrap(),
            "the potion item"
        );
        assert_eq!(pin.var_int().unwrap(), 1, "potion_contents added");
        assert_eq!(pin.var_int().unwrap(), 0, "none removed");
        assert_eq!(pin.var_int().unwrap(), v.component_potion_contents);
        assert!(pin.bool().unwrap(), "the potion field is present");
        assert_eq!(pin.var_int().unwrap(), potion as i32);
        assert!(!pin.bool().unwrap(), "no custom colour");
        assert_eq!(pin.var_int().unwrap(), 0, "no custom effects");
        assert!(!pin.bool().unwrap(), "no custom name");
    }

    #[test]
    fn a_potion_stack_uses_each_versions_own_component_id() {
        // The component id is not shared: every codec must write its own
        // release's. A test that pinned only 1.21.11 would miss a 26.x registry
        // that grew a component before `potion_contents`, or a 1.20.5-1.21.10
        // release whose compat table is one of many.
        use crate::protocol::modern::version::{
            V1_20_5, V1_21_1, V1_21_11, V1_21_3, V1_21_4, V1_21_5, V1_21_6, V1_21_8, V1_21_9,
            V26_1, V26_2, V26_3,
        };
        let potion = crate::game::brewing::potion_id("minecraft:swiftness").unwrap();
        let stack = Stack::potion("minecraft:potion", potion, 1);
        for v in [
            &V1_21_11, &V26_1, &V26_2, &V26_3, &V1_20_5, &V1_21_1, &V1_21_3, &V1_21_4, &V1_21_5,
            &V1_21_6, &V1_21_8, &V1_21_9,
        ] {
            let mut p = PacketOut::new(0);
            write_stack(v, &mut p, Some(&stack));
            let body = p.body()[1..].to_vec();
            let mut pin = crate::proto::PacketIn::new(&body);
            assert_eq!(pin.var_int().unwrap(), 1, "{}: count", v.name);
            assert_eq!(
                pin.var_int().unwrap(),
                v.item_id("minecraft:potion").unwrap(),
                "{}: the potion item",
                v.name
            );
            assert_eq!(pin.var_int().unwrap(), 1, "{}: component added", v.name);
            assert_eq!(pin.var_int().unwrap(), 0, "{}: none removed", v.name);
            assert_eq!(
                pin.var_int().unwrap(),
                v.component_potion_contents,
                "{}: its own potion_contents id",
                v.name
            );
        }
    }

    /// Whether `hay` contains `needle` as a contiguous run.
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn id(name: &str) -> aether_world::ItemTypeId {
        crate::inventory::item(name)
    }

    #[test]
    fn an_old_add_entity_puts_the_short_velocity_after_the_object_data() {
        // 1.21.9 moved the velocity from the tail to just after the position
        // and quantized it. A 1.21.1 client still wants three shorts at the
        // end, and getting the place wrong shifts the angles it reads.
        let ev = ServerEvent::SpawnEntity {
            entity_id: 9,
            uuid: 0x1234,
            kind: "minecraft:zombie",
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw: 0.0,
            pitch: 0.0,
            velocity: (0.0, 0.0, 0.0),
            data: 0,
        };
        let v = &crate::protocol::modern::version::V1_21_1;
        let pkts = encode(v, &ev).unwrap();
        let body = pkts[0].body();
        assert_eq!(body[0] as i32, v.packets.cb_add_entity);
        let mut pin = crate::proto::PacketIn::new(&body[1..]);
        pin.var_int().unwrap(); // entity id
        for _ in 0..16 {
            pin.u8().unwrap(); // uuid
        }
        pin.var_int().unwrap(); // type
        for _ in 0..3 {
            pin.f64().unwrap();
        }
        pin.u8().unwrap(); // pitch
        pin.u8().unwrap(); // yaw
        pin.u8().unwrap(); // head yaw
        pin.var_int().unwrap(); // object data
                                // The remaining payload is exactly the three shorts of the velocity.
        assert_eq!(pin.u16().unwrap(), 0);
        assert_eq!(pin.u16().unwrap(), 0);
        assert_eq!(pin.u16().unwrap(), 0);
        // Nothing may follow the velocity.
        assert!(pin.u8().is_err(), "nothing may follow the velocity");
    }

    #[test]
    fn older_versions_teleport_with_the_1_20_packet() {
        let old = position_sync(
            &crate::protocol::modern::version::V1_21_1,
            7,
            (1.0, 2.0, 3.0),
            0.0,
            0.0,
            true,
        );
        assert_eq!(
            old.body()[0] as i32,
            crate::protocol::modern::version::V1_21_1
                .packets
                .cb_entity_teleport
        );
        let new = position_sync(
            &crate::protocol::modern::version::V1_21_3,
            7,
            (1.0, 2.0, 3.0),
            0.0,
            0.0,
            true,
        );
        assert_eq!(
            new.body()[0] as i32,
            crate::protocol::modern::version::V1_21_3
                .packets
                .cb_entity_position_sync
        );
    }

    #[test]
    fn the_pose_serializer_id_moved_in_1_21_9() {
        let ev = ServerEvent::EntityMeta {
            entity_id: 1,
            entries: vec![(0, MetaValue::Pose(0))],
        };
        let check = |v: &Version, want: i32| {
            let pkts = encode(v, &ev).unwrap();
            let body = pkts[0].body();
            let mut pin = crate::proto::PacketIn::new(&body[1..]);
            pin.var_int().unwrap(); // entity id
            pin.u8().unwrap(); // index
            assert_eq!(pin.var_int().unwrap(), want, "{}: pose serializer", v.name);
        };
        check(&crate::protocol::modern::version::V1_21_1, 21);
        check(&crate::protocol::modern::version::V1_21_11, 20);
    }

    #[test]
    fn a_spear_is_shown_as_a_trident_on_1_21_1_and_cannot_be_laundered() {
        let v = &crate::protocol::modern::version::V1_21_1;
        let spear = id("minecraft:iron_spear");
        let w = v.wire_stack_item(spear).expect("a stand-in exists");
        assert!(w.disguised, "the spear is a stand-in on this release");
        assert_eq!(v.engine_item(w.id), Some(id("minecraft:trident")));
        // The client echoes the trident; the slot's real spear wins.
        assert_eq!(v.resolve_reported(spear, id("minecraft:trident")), spear);
        // A genuinely different report still stands.
        assert_eq!(
            v.resolve_reported(spear, id("minecraft:diamond")),
            id("minecraft:diamond")
        );
    }

    #[test]
    fn weather_border_and_spawn_position_reach_a_1_21_11_client() {
        let v = &crate::protocol::modern::version::V1_21_11;

        let pkts = encode(
            v,
            &ServerEvent::Weather {
                rain: true,
                thunder: false,
            },
        )
        .unwrap();
        assert_eq!(pkts.len(), 3, "start rain, rain level, thunder level");
        let body = pkts[0].body();
        assert_eq!(body[0] as i32, v.packets.cb_game_event);
        let mut pin = crate::proto::PacketIn::new(&body[1..]);
        assert_eq!(pin.u8().unwrap(), 1, "start_raining");
        assert_eq!(pin.f32().unwrap(), 0.0);
        let mut pin = crate::proto::PacketIn::new(&pkts[1].body()[1..]);
        assert_eq!(pin.u8().unwrap(), 7, "rain level change");
        assert_eq!(pin.f32().unwrap(), 1.0);
        let mut pin = crate::proto::PacketIn::new(&pkts[2].body()[1..]);
        assert_eq!(pin.u8().unwrap(), 8, "thunder level change");
        assert_eq!(pin.f32().unwrap(), 0.0);

        let pkts = encode(
            v,
            &ServerEvent::WorldBorder {
                x: 1.0,
                z: 2.0,
                old_diameter: 100.0,
                new_diameter: 200.0,
                speed: 40,
                warning_blocks: 5,
                warning_time: 300,
            },
        )
        .unwrap();
        assert_eq!(pkts[0].body()[0], 0x2a, "initialize_world_border");
        let mut pin = crate::proto::PacketIn::new(&pkts[0].body()[1..]);
        assert_eq!(pin.f64().unwrap(), 1.0);
        assert_eq!(pin.f64().unwrap(), 2.0);
        assert_eq!(pin.f64().unwrap(), 100.0);
        assert_eq!(pin.f64().unwrap(), 200.0);
        assert_eq!(pin.var_int().unwrap(), 40, "speed in ticks");
        assert_eq!(pin.var_int().unwrap(), 29_999_984);
        assert_eq!(pin.var_int().unwrap(), 5);
        assert_eq!(pin.var_int().unwrap(), 300, "warning time is in ticks");

        // A focused edit carries one field and cannot cancel the resize.
        let center = encode(v, &ServerEvent::WorldBorderCenter { x: 3.0, z: 4.0 }).unwrap();
        assert_eq!(center[0].body()[0], 0x56, "world_border_center");
        let mut pin = crate::proto::PacketIn::new(&center[0].body()[1..]);
        assert_eq!(pin.f64().unwrap(), 3.0);
        assert_eq!(pin.f64().unwrap(), 4.0);

        let size = encode(v, &ServerEvent::WorldBorderSize { diameter: 42.0 }).unwrap();
        assert_eq!(size[0].body()[0], 0x58, "world_border_size");
        let mut pin = crate::proto::PacketIn::new(&size[0].body()[1..]);
        assert_eq!(pin.f64().unwrap(), 42.0);

        let warn = encode(v, &ServerEvent::WorldBorderWarningTime { ticks: 600 }).unwrap();
        assert_eq!(warn[0].body()[0], 0x59, "world_border_warning_delay");
        let mut pin = crate::proto::PacketIn::new(&warn[0].body()[1..]);
        assert_eq!(pin.var_int().unwrap(), 600);

        let reach = encode(v, &ServerEvent::WorldBorderWarningDistance { blocks: 12 }).unwrap();
        assert_eq!(reach[0].body()[0], 0x5a, "world_border_warning_reach");
        let mut pin = crate::proto::PacketIn::new(&reach[0].body()[1..]);
        assert_eq!(pin.var_int().unwrap(), 12);

        let pkts = encode(
            v,
            &ServerEvent::SpawnPosition {
                x: 10,
                y: 64,
                z: -3,
                yaw: 90.0,
            },
        )
        .unwrap();
        assert_eq!(pkts[0].body()[0], 0x5f, "spawn_position");
        let mut pin = crate::proto::PacketIn::new(&pkts[0].body()[1..]);
        assert_eq!(pin.string().unwrap(), "minecraft:overworld");
        assert_eq!(pin.i64().unwrap(), encode_position(10, 64, -3));
        assert_eq!(pin.f32().unwrap(), 90.0);
        assert_eq!(pin.f32().unwrap(), 0.0);

        // A release whose border ids are not wired yet gets no packet rather
        // than one at the wrong id.
        let newer = encode(
            &crate::protocol::modern::version::V26_1,
            &ServerEvent::WorldBorder {
                x: 0.0,
                z: 0.0,
                old_diameter: 1.0,
                new_diameter: 1.0,
                speed: 0,
                warning_blocks: 5,
                warning_time: 15,
            },
        )
        .unwrap();
        assert!(newer.is_empty());
    }

    #[test]
    fn entity_move_uses_the_old_teleport_then_the_1_21_2_position_sync() {
        // 1.21.2 replaced the 3×f64 / angle-byte teleport with a packet
        // carrying explicit deltas and f32 angles: `entity_position_sync`
        // (and `entity_teleport` gained a trailing flags word). The older
        // codecs must not keep sending the old body at the new id.
        let v = &crate::protocol::modern::version::V1_21_1;
        let old = position_sync(v, 7, (1.0, 2.0, 3.0), 90.0, 45.0, true);
        assert_eq!(old.body()[0] as i32, v.packets.cb_entity_teleport);
        // id + varint entity id + 3×f64 + yaw + pitch + onGround.
        assert_eq!(old.body().len(), 1 + 1 + 24 + 1 + 1 + 1);

        for v in [
            &crate::protocol::modern::version::V1_21_3,
            &crate::protocol::modern::version::V1_21_4,
            &crate::protocol::modern::version::V1_21_5,
            &crate::protocol::modern::version::V1_21_6,
            &crate::protocol::modern::version::V1_21_8,
            &crate::protocol::modern::version::V1_21_9,
        ] {
            let p = position_sync(v, 7, (1.0, 2.0, 3.0), 90.0, 45.0, true);
            assert_eq!(
                p.body()[0] as i32,
                v.packets.cb_entity_position_sync,
                "{}",
                v.name
            );
            // id + varint entity id + 6×f64 + 2×f32 + onGround.
            assert_eq!(p.body().len(), 1 + 1 + 48 + 8 + 1, "{}", v.name);
        }
    }

    #[test]
    fn a_legacy_versions_damage_types_match_its_own_registry_order() {
        // The generated table is exactly the registry the codec sends, so a
        // damage type's index here is its wire id. The first version of the
        // generator dropped message ids containing `.` or `_`, silently
        // shifting every later entry.
        let v = &crate::protocol::modern::version::V1_20_5;
        assert_eq!(v.damage_types.len(), 45);
        assert_eq!(v.damage_type("minecraft:starve"), Some(36));
        assert_eq!(v.damage_type("minecraft:player_explosion"), Some(32));

        let v = &crate::protocol::modern::version::V1_21_1;
        assert_eq!(v.damage_types.len(), 47);
        assert_eq!(v.damage_type("minecraft:starve"), Some(37));
        assert_eq!(v.damage_type("minecraft:player_explosion"), Some(33));
        assert_eq!(v.damage_type("minecraft:sonic_boom"), Some(34));
        assert_eq!(v.damage_type("minecraft:wind_charge"), Some(44));

        let v = &crate::protocol::modern::version::V1_21_3;
        assert_eq!(v.damage_types.len(), 49);
        assert_eq!(v.damage_type("minecraft:starve"), Some(39));
        assert_eq!(v.damage_type("minecraft:mace_smash"), Some(26));

        let v = &crate::protocol::modern::version::V1_21_9;
        assert_eq!(v.damage_types.len(), 47);
        assert_eq!(v.damage_type("minecraft:starve"), Some(37));
    }

    #[test]
    fn the_time_packet_has_no_tick_day_time_before_1_21_2() {
        // `tickDayTime` arrived in 1.21.2; writing it earlier leaves a stray
        // byte the client reads as the next packet.
        let ev = ServerEvent::Time {
            age: 1,
            time_of_day: 2,
        };
        let len = |v: &Version| encode(v, &ev).unwrap()[0].body().len();
        assert_eq!(len(&crate::protocol::modern::version::V1_20_5), 1 + 16);
        assert_eq!(len(&crate::protocol::modern::version::V1_21_1), 1 + 16);
        assert_eq!(len(&crate::protocol::modern::version::V1_21_3), 1 + 17);
        // 1.20.2-1.20.4 match 1.20.5 here.
        assert_eq!(len(&crate::protocol::modern::version::V1_20_2), 1 + 16);
        assert_eq!(len(&crate::protocol::modern::version::V1_20_4), 1 + 16);
    }

    #[test]
    fn a_pre_1_20_5_slot_is_the_old_nbt_form_not_a_component_stack() {
        // 1.20.5 replaced the NBT `Slot` with count/id/two component counts.
        // The older form is presence, id, `i8` count, then optional NBT.
        let v = &crate::protocol::modern::version::V1_20_4;

        // Empty: a lone `present = false` (after the packet id).
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, None);
        assert_eq!(p.body(), &[0, 0]);

        // One stone: present, id, `i8` count, then `TAG_End` (no NBT).
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&Stack::new(id("minecraft:stone"), 1)));
        let body = p.body()[1..].to_vec();
        let mut pin = crate::proto::PacketIn::new(&body);
        assert!(pin.bool().unwrap(), "present");
        assert_eq!(
            pin.var_int().unwrap(),
            v.item_id("minecraft:stone").unwrap()
        );
        assert_eq!(pin.u8().unwrap(), 1, "i8 count");
        assert_eq!(pin.u8().unwrap(), 0, "no NBT: a lone TAG_End");
        assert!(pin.u8().is_err(), "nothing may trail the slot");
    }

    #[test]
    fn a_worn_tool_carries_its_damage_in_the_old_nbt_slot() {
        let v = &crate::protocol::modern::version::V1_20_4;
        let mut s = Stack::new(id("minecraft:diamond_pickaxe"), 1);
        s.damage = 7;
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&s));
        let body = p.body()[1..].to_vec();
        assert!(contains(&body, b"Damage"), "the Damage tag is present");
        assert!(contains(&body, &7i32.to_be_bytes()), "and carries the wear");
    }

    #[test]
    fn a_potion_carries_its_name_in_the_old_nbt_slot() {
        // Before 1.20.5 a potion names itself through the NBT `Potion` tag —
        // the registry *name*, not an index — rather than the
        // `minecraft:potion_contents` component. This is why 764/765 need no
        // component id.
        for v in [
            &crate::protocol::modern::version::V1_20_2,
            &crate::protocol::modern::version::V1_20_4,
        ] {
            let potion = crate::game::brewing::potion_id("minecraft:long_swiftness").unwrap();
            let s = Stack::potion("minecraft:potion", potion, 1);
            let mut p = PacketOut::new(0);
            write_stack(v, &mut p, Some(&s));
            let body = p.body()[1..].to_vec();
            let mut pin = crate::proto::PacketIn::new(&body);
            assert!(pin.bool().unwrap(), "{}: present", v.name);
            assert_eq!(
                pin.var_int().unwrap(),
                v.item_id("minecraft:potion").unwrap(),
                "{}: the potion item",
                v.name
            );
            assert_eq!(pin.u8().unwrap(), 1, "{}: i8 count", v.name);
            assert_eq!(pin.u8().unwrap(), 0x0A, "{}: TAG_Compound", v.name);
            // The compound holds the `Potion` string with the registry name.
            assert!(
                contains(&body, b"Potion"),
                "{}: the Potion tag is present",
                v.name
            );
            assert!(
                contains(&body, b"minecraft:long_swiftness"),
                "{}: and carries the registry name",
                v.name
            );
        }
        // A non-potion stack carries no `Potion` tag.
        let v = &crate::protocol::modern::version::V1_20_4;
        let mut p = PacketOut::new(0);
        write_stack(v, &mut p, Some(&Stack::new(id("minecraft:diamond"), 1)));
        assert!(!contains(&p.body()[1..], b"Potion"));
    }

    #[test]
    fn the_pose_serializer_is_20_before_1_20_5() {
        // 1.20.5 grew a `particles` serializer ahead of `pose`, shifting it
        // from 20 to 21; 1.21.9 moved it back. Older codecs must keep 20.
        let ev = ServerEvent::EntityMeta {
            entity_id: 1,
            entries: vec![(0, MetaValue::Pose(0))],
        };
        let check = |v: &Version, want: i32| {
            let pkts = encode(v, &ev).unwrap();
            let body = pkts[0].body();
            let mut pin = crate::proto::PacketIn::new(&body[1..]);
            pin.var_int().unwrap(); // entity id
            pin.u8().unwrap(); // index
            assert_eq!(pin.var_int().unwrap(), want, "{}: pose serializer", v.name);
        };
        check(&crate::protocol::modern::version::V1_20_2, 20);
        check(&crate::protocol::modern::version::V1_20_4, 20);
        check(&crate::protocol::modern::version::V1_20_5, 21);
    }

    #[test]
    fn a_pre_1_20_5_attribute_is_named_and_its_modifier_is_a_uuid() {
        let ev = ServerEvent::UpdateAttributes {
            entity_id: 5,
            properties: vec![(20, 0.1, vec![("minecraft:effect.speed".to_owned(), 0.2, 2)])],
        };
        for v in [
            &crate::protocol::modern::version::V1_20_2,
            &crate::protocol::modern::version::V1_20_4,
        ] {
            let pkts = encode(v, &ev).unwrap();
            let body = pkts[0].body();
            assert_eq!(body[0] as i32, v.packets.cb_update_attributes);
            let mut pin = crate::proto::PacketIn::new(&body[1..]);
            assert_eq!(pin.var_int().unwrap(), 5);
            assert_eq!(pin.var_int().unwrap(), 1, "one property");
            assert_eq!(
                pin.string().unwrap(),
                "minecraft:generic.movement_speed",
                "{}: the attribute is named",
                v.name
            );
            assert_eq!(pin.f64().unwrap(), 0.1);
            assert_eq!(pin.var_int().unwrap(), 1, "one modifier");
            for _ in 0..16 {
                pin.u8().unwrap(); // a UUID, not an identifier
            }
            assert_eq!(pin.f64().unwrap(), 0.2);
            assert_eq!(pin.u8().unwrap(), 2);
            assert!(pin.u8().is_err(), "{}: no identifier trails", v.name);
        }

        // 1.20.5 indexes the attribute and also uses a UUID here.
        let v = &crate::protocol::modern::version::V1_20_5;
        let pkts = encode(v, &ev).unwrap();
        let mut pin = crate::proto::PacketIn::new(&pkts[0].body()[1..]);
        assert_eq!(pin.var_int().unwrap(), 5);
        assert_eq!(pin.var_int().unwrap(), 1);
        assert_eq!(pin.var_int().unwrap(), v.attribute_map[20]);
        assert_eq!(pin.f64().unwrap(), 0.1);
        assert_eq!(pin.var_int().unwrap(), 1);
        for _ in 0..16 {
            pin.u8().unwrap(); // 1.20.5 still a UUID
        }
        assert_eq!(pin.f64().unwrap(), 0.2);
        assert_eq!(pin.u8().unwrap(), 2);
    }

    #[test]
    fn the_respawn_packet_predates_spawn_info_before_1_20_5() {
        let ev = ServerEvent::Respawn {
            game_mode: GameMode::Survival,
        };
        // 1.20.2-1.20.4: dimension type name, world name, seed, game mode,
        // previous, debug, flat, death, portal cooldown, copyMetadata.
        let pkts = encode(&crate::protocol::modern::version::V1_20_2, &ev).unwrap();
        assert_eq!(pkts.len(), 2, "respawn + abilities; no wait-for-chunks");
        let body = pkts[0].body();
        assert_eq!(
            body[0] as i32,
            crate::protocol::modern::version::V1_20_2.packets.cb_respawn
        );
        let mut pin = crate::proto::PacketIn::new(&body[1..]);
        assert_eq!(pin.string().unwrap(), "minecraft:overworld");
        assert_eq!(pin.string().unwrap(), "minecraft:overworld");
        assert_eq!(pin.i64().unwrap(), 0);
        assert_eq!(pin.u8().unwrap(), 0);
        assert_eq!(pin.u8().unwrap(), 0xFF);
        assert!(!pin.bool().unwrap());
        assert!(!pin.bool().unwrap());
        assert!(!pin.bool().unwrap());
        assert_eq!(pin.var_int().unwrap(), 0);
        assert!(!pin.bool().unwrap());
        assert!(pin.u8().is_err(), "nothing may trail the respawn");

        // 1.20.3 adds the wait-for-chunks game event after the abilities.
        let pkts = encode(&crate::protocol::modern::version::V1_20_4, &ev).unwrap();
        assert_eq!(pkts.len(), 3);
        assert_eq!(
            pkts[2].body()[0] as i32,
            crate::protocol::modern::version::V1_20_4
                .packets
                .cb_game_event
        );
    }

    #[test]
    fn open_screen_and_death_messages_are_json_in_1_20_2() {
        let open = ServerEvent::OpenWindow {
            window_id: 1,
            menu: Menu::Chest,
            title: "Stash".to_owned(),
        };
        // 1.20.2: a length-prefixed JSON string, not an NBT component.
        let pkts = encode(&crate::protocol::modern::version::V1_20_2, &open).unwrap();
        let body = pkts[0].body();
        let mut pin = crate::proto::PacketIn::new(&body[1..]);
        assert_eq!(pin.var_int().unwrap(), 1);
        assert_eq!(
            pin.var_int().unwrap(),
            crate::protocol::modern::version::V1_20_2.menu_chest
        );
        assert_eq!(pin.string().unwrap(), r#"{"text":"Stash"}"#);

        // 1.20.3 sends the NBT component form: tag byte then payload.
        let pkts = encode(&crate::protocol::modern::version::V1_20_4, &open).unwrap();
        assert_eq!(pkts[0].body()[3], 0x08, "TAG_String NBT component");

        // The same open-screen path carries the brewing-stand menu id, which
        // is one below 1.21's on 764/765.
        for v in [
            &crate::protocol::modern::version::V1_20_2,
            &crate::protocol::modern::version::V1_20_4,
        ] {
            let ev = ServerEvent::OpenWindow {
                window_id: 1,
                menu: Menu::BrewingStand,
                title: "Brewing Stand".to_owned(),
            };
            let pkts = encode(v, &ev).unwrap();
            let mut pin = crate::proto::PacketIn::new(&pkts[0].body()[1..]);
            assert_eq!(pin.var_int().unwrap(), 1);
            assert_eq!(
                pin.var_int().unwrap(),
                v.menu_brewing_stand,
                "{}: brewing-stand menu id",
                v.name
            );
            assert_eq!(v.menu_brewing_stand, 10, "{}", v.name);
        }

        let death = ServerEvent::DeathMessage {
            entity_id: 3,
            text: "Steve died".to_owned(),
        };
        let pkts = encode(&crate::protocol::modern::version::V1_20_2, &death).unwrap();
        let body = pkts[0].body();
        let mut pin = crate::proto::PacketIn::new(&body[1..]);
        assert_eq!(pin.var_int().unwrap(), 3);
        assert_eq!(pin.string().unwrap(), r#"{"text":"Steve died"}"#);
    }

    #[test]
    fn update_mob_effect_uses_the_pre_1_20_5_body_on_764_and_765() {
        // 764/765 `entity_effect`, per minecraft-data:
        // entityId varint, effectId varint, amplifier **i8**, duration varint,
        // hideParticles **i8**, factorCodec **option**<anonymousNbt>.
        // 1.20.5 replaced the tail with a single flags u8. Omitting the
        // option bool leaves the packet a byte short and kicks the client.
        let ev = ServerEvent::UpdateMobEffect {
            entity_id: 7,
            effect: 20,
            amplifier: 2,
            duration: 300,
            ambient: false,
            visible: true,
            show_icon: true,
        };
        for (v, wire_effect) in [
            (&crate::protocol::modern::version::V1_20_2, 21),
            (&crate::protocol::modern::version::V1_20_4, 20),
        ] {
            let pkts = encode(v, &ev).unwrap();
            let body = pkts[0].body();
            assert_eq!(body[0] as i32, v.packets.cb_update_mob_effect);
            let mut pin = crate::proto::PacketIn::new(&body[1..]);
            assert_eq!(pin.var_int().unwrap(), 7, "{}", v.name);
            assert_eq!(
                pin.var_int().unwrap(),
                wire_effect,
                "{}: 764 uses the legacy 1-based effect id",
                v.name
            );
            assert_eq!(pin.u8().unwrap(), 2, "{}: amplifier is an i8", v.name);
            assert_eq!(pin.var_int().unwrap(), 300, "{}", v.name);
            assert_eq!(pin.u8().unwrap(), 0b110, "{}: visible|show_icon", v.name);
            assert!(!pin.bool().unwrap(), "{}: factorCodec is absent", v.name);
            assert!(pin.u8().is_err(), "{}: nothing may trail", v.name);
        }
        // 1.20.5 uses the VarInt amplifier and no factor option.
        let v = &crate::protocol::modern::version::V1_20_5;
        let pkts = encode(v, &ev).unwrap();
        let mut pin = crate::proto::PacketIn::new(&pkts[0].body()[1..]);
        assert_eq!(pin.var_int().unwrap(), 7);
        assert_eq!(pin.var_int().unwrap(), 20);
        assert_eq!(pin.var_int().unwrap(), 2, "amplifier is a VarInt here");
        assert_eq!(pin.var_int().unwrap(), 300);
        assert_eq!(pin.u8().unwrap(), 0b110);
        assert!(pin.u8().is_err(), "no factor option from 1.20.5");
    }

    #[test]
    fn particle_packets_follow_each_release_layout() {
        let ev = ServerEvent::Particle {
            name: "minecraft:flame".into(),
            x: 1.5,
            y: 64.0,
            z: -2.5,
            dx: 0.25,
            dy: 0.5,
            dz: -0.75,
            speed: 0.1,
            count: 4,
            force: true,
            options: ParticleOptions::None,
        };
        // 1.21.11: longDistance, alwaysShow, position, offsets, count, id.
        let v = &crate::protocol::modern::version::V1_21_11;
        let pkts = encode(v, &ev).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        assert_eq!(pin.var_int().unwrap(), 46);
        assert!(pin.bool().unwrap(), "force is the long-distance flag");
        assert!(!pin.bool().unwrap(), "alwaysShow is always false");
        assert_eq!(pin.f64().unwrap(), 1.5);
        assert_eq!(pin.f64().unwrap(), 64.0);
        assert_eq!(pin.f64().unwrap(), -2.5);
        assert_eq!(pin.f32().unwrap(), 0.25);
        assert_eq!(pin.f32().unwrap(), 0.5);
        assert_eq!(pin.f32().unwrap(), -0.75);
        assert_eq!(pin.f32().unwrap(), 0.1);
        assert_eq!(pin.i32().unwrap(), 4);
        let id = pin.var_int().unwrap();
        assert_eq!(
            id,
            crate::protocol::resources::particle_id(774, "minecraft:flame").unwrap()
        );
        assert!(pin.u8().is_err(), "flame has no option payload");

        // 1.21.1 (protocol 767) predates alwaysShow; the packet starts with
        // one flag, then the position, and carries no particle options.
        let v = &crate::protocol::modern::version::V1_21_1;
        let pkts = encode(v, &ev).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        assert_eq!(pin.var_int().unwrap(), 41);
        assert!(pin.bool().unwrap());
        assert_eq!(pin.f64().unwrap(), 1.5);
    }

    #[test]
    fn particle_options_follow_each_release_payload() {
        let dust = ServerEvent::Particle {
            name: "minecraft:dust".into(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
            speed: 0.0,
            count: 1,
            force: false,
            options: ParticleOptions::Dust {
                color: 0xFFFF_0000u32 as i32,
                scale: 2.0,
            },
        };
        // 1.21.11 writes the colour as one int and the scale as a float.
        let v = &crate::protocol::modern::version::V1_21_11;
        let pkts = encode(v, &dust).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        let _ = pin.var_int().unwrap();
        let _ = pin.bool().unwrap();
        let _ = pin.bool().unwrap();
        for _ in 0..3 {
            let _ = pin.f64().unwrap();
        }
        for _ in 0..3 {
            let _ = pin.f32().unwrap();
        }
        let _ = pin.f32().unwrap(); // speed
        let _ = pin.i32().unwrap(); // count
        assert_eq!(pin.var_int().unwrap(), 14);
        assert_eq!(pin.i32().unwrap(), 0xFFFF_0000u32 as i32);
        assert_eq!(pin.f32().unwrap(), 2.0);

        // 1.21.1 wrote red, green, blue and scale as four floats.
        let v = &crate::protocol::modern::version::V1_21_1;
        let pkts = encode(v, &dust).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        let _ = pin.var_int().unwrap();
        let _ = pin.bool().unwrap();
        for _ in 0..3 {
            let _ = pin.f64().unwrap();
        }
        for _ in 0..3 {
            let _ = pin.f32().unwrap();
        }
        let _ = pin.f32().unwrap();
        let _ = pin.i32().unwrap();
        assert_eq!(pin.var_int().unwrap(), 13);
        assert_eq!(pin.f32().unwrap(), 1.0);
        assert_eq!(pin.f32().unwrap(), 0.0);
        assert_eq!(pin.f32().unwrap(), 0.0);
        assert_eq!(pin.f32().unwrap(), 2.0);

        let block = ServerEvent::Particle {
            name: "minecraft:block".into(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
            speed: 0.0,
            count: 1,
            force: false,
            options: ParticleOptions::Block(aether_world::registry::ids::STONE),
        };
        let v = &crate::protocol::modern::version::V1_21_11;
        let pkts = encode(v, &block).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        let _ = pin.var_int().unwrap();
        let _ = pin.bool().unwrap();
        let _ = pin.bool().unwrap();
        for _ in 0..3 {
            let _ = pin.f64().unwrap();
        }
        for _ in 0..3 {
            let _ = pin.f32().unwrap();
        }
        let _ = pin.f32().unwrap();
        let _ = pin.i32().unwrap();
        assert_eq!(pin.var_int().unwrap(), 1);
        assert_eq!(
            pin.var_int().unwrap(),
            v.state(aether_world::registry::ids::STONE) as i32
        );
    }

    #[test]
    fn stop_sound_packets_carry_only_the_parts_that_were_named() {
        let both = ServerEvent::StopSound {
            source: Some(4),
            sound: Some("minecraft:block.note_block.harp".into()),
        };
        let v = &crate::protocol::modern::version::V1_21_11;
        let pkts = encode(v, &both).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        assert_eq!(pin.var_int().unwrap(), 117);
        assert_eq!(pin.u8().unwrap(), 0b11);
        assert_eq!(pin.var_int().unwrap(), 4);
        assert_eq!(pin.string().unwrap(), "minecraft:block.note_block.harp");
        assert!(pin.u8().is_err());

        let all = ServerEvent::StopSound {
            source: None,
            sound: None,
        };
        let pkts = encode(v, &all).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        assert_eq!(pin.var_int().unwrap(), 117);
        assert_eq!(pin.u8().unwrap(), 0);
        assert!(pin.u8().is_err());

        // The 1.20.5 wire has its own stop-sound id.
        let v = &crate::protocol::modern::version::V1_20_5;
        let pkts = encode(v, &both).unwrap();
        let mut pin = crate::proto::PacketIn::new(pkts[0].body());
        assert_eq!(pin.var_int().unwrap(), 106);
        assert_eq!(pin.u8().unwrap(), 0b11);
    }

    #[test]
    fn effect_ids_are_one_based_on_764_and_zero_based_from_765() {
        // The engine's `effect` field is the 0-based `minecraft:mob_effect`
        // registry index (`effects::REGENERATION = 9`, `ABSORPTION = 21`).
        // 1.20.2 still numbered them the legacy way (Speed = 1), so its wire
        // id is one higher; 1.20.3 and later use the registry index as-is.
        // The remove packet must agree with the update packet or a client
        // keeps the effect forever.
        let update = ServerEvent::UpdateMobEffect {
            entity_id: 1,
            effect: 9,
            amplifier: 1,
            duration: 100,
            ambient: false,
            visible: true,
            show_icon: true,
        };
        let remove = ServerEvent::RemoveMobEffect {
            entity_id: 1,
            effect: 9,
        };
        let update_id = |v: &crate::protocol::modern::version::Version| {
            let pkts = encode(v, &update).unwrap();
            let mut pin = crate::proto::PacketIn::new(pkts[0].body());
            assert_eq!(pin.var_int().unwrap(), v.packets.cb_update_mob_effect);
            assert_eq!(pin.var_int().unwrap(), 1);
            pin.var_int().unwrap()
        };
        let remove_id = |v: &crate::protocol::modern::version::Version| {
            let pkts = encode(v, &remove).unwrap();
            let mut pin = crate::proto::PacketIn::new(pkts[0].body());
            assert_eq!(pin.var_int().unwrap(), v.packets.cb_remove_mob_effect);
            assert_eq!(pin.var_int().unwrap(), 1);
            pin.var_int().unwrap()
        };
        for v in [
            &crate::protocol::modern::version::V1_20_2,
            &crate::protocol::modern::version::V1_20_4,
            &crate::protocol::modern::version::V1_20_5,
            &crate::protocol::modern::version::V1_21_11,
        ] {
            let expected = if v.gen == Gen::V1_20_2 { 10 } else { 9 };
            assert_eq!(update_id(v), expected, "{} update", v.name);
            assert_eq!(remove_id(v), expected, "{} remove", v.name);
        }
    }
}
