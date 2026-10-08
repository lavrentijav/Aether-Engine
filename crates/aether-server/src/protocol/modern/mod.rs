//! The modern codec: Minecraft 1.21.11 (protocol 774) and the releases
//! after it — 26.1.x (775), 26.2 (776) and 26.3 (777).
//!
//! One codec, parameterized by a [`version::Version`]: every packet id,
//! block-state, item and entity-type id comes from that version's tables,
//! generated from the game's own `--reports` dump (`tools/gen/versions.py`).
//! Where a packet's *layout* changed between releases, the code branches on
//! [`version::Gen`] with the release that changed it named alongside.
//!
//! Structurally this shares nothing with 1.8 but the frame header. Login is
//! followed by a **configuration phase** (added in 1.20.2) in which the server
//! ships the registries the client builds its world from, chunks are paletted
//! containers rather than flat id arrays, and blocks are identified by
//! block-state id rather than `(id << 4) | meta`.
//!
//! **Not verified against a live client.** No 1.21 client was available, so
//! everything here is covered by encode/decode tests only. See the module
//! tests and the notes on [`Codec::decode`] for the known gaps.

pub mod chunk;
#[allow(clippy::all)]
pub mod gen;
pub mod inventory;
pub mod items;
pub mod play;
pub mod registry;
pub mod version;

use std::io;

use aether_world::BlockStateId;

use super::{BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};
use version::{Gen, Registries};

/// A spawn packet's velocity, at rest.
///
/// 1.21.9 changed this field twice over: it moved from the end of the packet
/// to just after the position, and the three `i16` components became a
/// variable-length quantized vector whose **zero is a single `0x00` byte**,
/// not six. A real 1.21.11 client refused the packet outright:
///
/// ```text
/// Packet play/clientbound/minecraft:add_entity was larger than I expected,
/// found 5 bytes extra whilst reading packet clientbound/minecraft:add_entity
/// ```
///
/// Nothing this server spawns moves under its own velocity — dropped items
/// are moved with position updates from [`crate::ground`] — so the zero form
/// is the only one needed, and the quantized encoding is not implemented.
const ZERO_VELOCITY: [u8; 1] = [0x00];

/// The window this server opens its own containers under. Never 0, which is
/// the player's own inventory and must not be replaced.
const STASH_WINDOW: i32 = 1;

/// Metadata index of `ItemEntity`'s stack.
///
/// `Entity` itself defines 0..=7 (shared flags, air, custom name and its
/// visibility, silent, no gravity, pose, ticks frozen), so the first field a
/// dropped item adds sits at 8.
const ITEM_DATA_INDEX: u8 = 8;
/// Metadata serializer id for an item stack.
const META_ITEM_STACK: i32 = 7;
/// Terminates a metadata list.
const META_END: u8 = 0xFF;

/// Stable id for the pack this server offers, so a client can tell a repeat
/// offer from a new one across reconnects.
const RESOURCE_PACK_ID: u128 = 0xae74_e701_0000_4000_8000_0000_0000_0001;

/// The codec of one modern protocol version; see [`version`].
pub struct Codec(pub &'static version::Version);

/// Engine block id -> 1.21.11 block-state id. Other versions translate
/// through [`version::Version::state`]; this is the reference the tests hold
/// the 1.21.11 table to.
#[cfg(test)]
///
/// A table lookup, not a chain of comparisons: the engine's block ids *are*
/// this version's block ids (see `aether_world::registry::blocks`), so the
/// translation is an index. It used to be an `if` chain over ten blocks, with
/// everything else substituted as stone — which is why placing anything the
/// generator does not emit produced a stone cube.
pub fn block_state(id: BlockStateId) -> u32 {
    // The identity function, and that is the point: the engine's ids *are*
    // 1.21.11's block-state ids (DESIGN_NOTES §9). A state this version does
    // not have can only be a modded block, which substitutes stone rather than
    // handing the client an id outside its own registry.
    if (id.raw() as usize) < aether_world::registry::blocks::STATE_COUNT {
        id.raw()
    } else {
        aether_world::registry::ids::STONE.raw()
    }
}

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        self.0.name
    }

    fn protocol_id(&self) -> i32 {
        self.0.protocol
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        // Login Start: username, then the client's own UUID (ignored — this
        // server derives an offline UUID from the name like 1.8 does).
        let Some(p) = read_packet(s)? else {
            return Ok(None);
        };
        if p.id != self.0.packets.login_sb_hello {
            return Ok(None);
        }
        Ok(Some(PacketIn::new(&p.data).string()?))
    }

    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Login Success: UUID as 16 raw bytes here (1.8 wanted a hyphenated
        // string in the same packet), username, then an empty property list.
        let v = self.0;
        let mut finished = PacketOut::new(v.packets.login_cb_login_finished);
        finished.uuid(p.uuid).string(&p.name).var_int(0);
        if v.gen >= Gen::V26_2 {
            // 26.2: a session id, which the client attaches to its reports.
            // Any UUID will do for an offline server; the player's own is
            // as good as another.
            finished.uuid(p.uuid);
        }
        finished.send(s)?;

        // The client acknowledges and moves itself into the configuration
        // state; nothing may be sent in between.
        wait_for(s, v.packets.login_sb_login_acknowledged)?;

        // Configuration: hand over the registries the client builds its world
        // from. Without these it disconnects before ever reaching play.
        match &v.registries {
            Registries::Explicit => {
                for (id, entries) in registry::registries() {
                    let mut pkt = PacketOut::new(v.packets.cfg_cb_registry_data);
                    pkt.string(id).var_int(entries.len() as i32);
                    for (key, value) in entries {
                        pkt.string(&key)
                            .bool(true) // this entry carries data
                            .bytes(&value.to_network());
                    }
                    pkt.send(s)?;
                }
            }
            Registries::KnownPacks {
                core,
                synced,
                overworld,
                data,
            } => send_known_registries(s, v, core, synced, *overworld, data)?,
        }

        // Add Resource Pack, offered while still in configuration so the
        // client downloads before the world appears.
        if p.resource_pack.enabled() {
            PacketOut::new(v.packets.cfg_cb_resource_pack_push)
                .uuid(RESOURCE_PACK_ID)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .bool(p.resource_pack.required)
                .bool(false) // no custom prompt message
                .send(s)?;
        }

        // Tags. Not decoration: the client decides what water *is* by the
        // `minecraft:water` fluid tag — swimming, the underwater view and
        // drowning all ask it — and without one every water block behaves as
        // air. Climbing asks `climbable`; a pickaxe's speed asks the
        // `mineable/*` block tags.
        tags_packet(v).send(s)?;

        PacketOut::new(v.packets.cfg_cb_finish_configuration).send(s)?;
        wait_for(s, v.packets.cfg_sb_finish_configuration)?;

        // Play Login.
        let mut login = PacketOut::new(v.packets.cb_login);
        login
            .i32(p.entity_id)
            .bool(false) // not hardcore
            .var_int(1) // one world
            .string(registry::DIMENSION_NAME)
            .var_int(p.max_players as i32)
            .var_int(p.view_radius)
            .var_int(p.view_radius) // simulation distance
            .bool(false) // reduced debug info
            .bool(true) // enable respawn screen
            .bool(false) // limited crafting
            // SpawnInfo:
            .var_int(dimension_type_id(v))
            .string(registry::DIMENSION_NAME)
            .i64(0) // hashed seed
            .u8(p.game_mode.wire()); // survival 0 / creative 1 (a VarInt from 26.3: same byte)
        play::write_no_previous_mode(v, &mut login);
        login
            .bool(false) // not a debug world
            .bool(false) // not superflat
            .bool(false) // no death location
            .var_int(0) // portal cooldown
            .var_int(63); // sea level
        if v.gen >= Gen::V26_2 {
            login.bool(false); // 26.2: not in online mode
        }
        login.bool(false); // does not enforce secure chat
        login.send(s)?;

        // Tell the client to start waiting for chunks rather than rendering
        // an empty void while the first batch is in flight.
        PacketOut::new(self.0.packets.cb_game_event)
            .u8(13)
            .f32(0.0)
            .send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Synchronize Player Position. Deltas are zero and `flags` is 0, so
        // every value is absolute.
        PacketOut::new(self.0.packets.cb_player_position)
            .var_int(1) // teleport id
            .f64(p.spawn.0)
            .f64(p.spawn.1)
            .f64(p.spawn.2)
            .f64(0.0)
            .f64(0.0)
            .f64(0.0)
            .f32(p.yaw)
            .f32(p.pitch)
            .i32(0) // all absolute
            .send(s)?;

        // Player Abilities. The login packet's game mode alone does not grant
        // the creative flags; 1.8 has always been sent these and 1.21 was not,
        // which left the client's own idea of what it may do at the defaults.
        //
        // The flying bit is deliberately **off** while allow-flying is on: a
        // client that starts already flying never enters the swimming state,
        // and submerging with no underwater view is exactly the symptom
        // reported. Let the player choose to fly instead.
        let flags = match p.game_mode {
            super::GameMode::Creative => 0x01 | 0x04 | 0x08, // invulnerable | allow flying | creative
            super::GameMode::Survival => 0,
        };
        PacketOut::new(self.0.packets.cb_player_abilities)
            .u8(flags)
            .f32(0.05) // flying speed
            .f32(0.10) // walking speed
            .send(s)?;

        // The inventory itself follows from the session, which owns it: the
        // server's copy is authoritative and is sent whole once the player
        // is registered.
        inventory::held_item_packet(self.0.packets.cb_set_held_slot, 0).send(s)?;

        // Full health and food. This server models neither, so the values are
        // constant — but sending them is not optional: see the constant's
        // comment.
        PacketOut::new(self.0.packets.cb_set_health)
            .f32(20.0)
            .var_int(20)
            .f32(5.0)
            .send(s)?;
        Ok(())
    }

    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut> {
        match ev {
            ServerEvent::KeepAlive(id) => {
                let mut p = PacketOut::new(self.0.packets.cb_keep_alive);
                p.i64(*id); // a long here, a varint in 1.8
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                vec![chunk::chunk_data_packet(self.0, *cx, *cz, world)]
            }
            ServerEvent::UnloadColumn { cx, cz } => {
                vec![chunk::unload_chunk_packet(
                    self.0.packets.cb_forget_level_chunk,
                    *cx,
                    *cz,
                )]
            }
            ServerEvent::TabListAdd(h) => {
                let mut p = PacketOut::new(self.0.packets.cb_player_info_update);
                p.u8(0x01 | 0x08) // add_player | update_listed
                    .var_int(1) // one entry
                    .uuid(h.uuid)
                    .string(&h.name)
                    .var_int(0) // no profile properties
                    .bool(true); // listed
                vec![p]
            }
            ServerEvent::TabListRemove(uuid) => {
                let mut p = PacketOut::new(self.0.packets.cb_player_info_remove);
                p.var_int(1).uuid(*uuid);
                vec![p]
            }
            ServerEvent::SpawnPlayer(h) => {
                let pos = h.pos();
                let mut p = PacketOut::new(self.0.packets.cb_add_entity);
                p.var_int(h.entity_id)
                    .uuid(h.uuid)
                    .var_int(self.0.entity_type("minecraft:player").unwrap_or(0))
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    // Velocity precedes the angles in this version.
                    .bytes(&ZERO_VELOCITY)
                    .u8(angle(pos.pitch))
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.yaw)) // head yaw
                    .var_int(0); // no object data
                vec![p]
            }
            ServerEvent::DropItem {
                entity_id,
                x,
                y,
                z,
                item,
                count,
            } => {
                // Two packets, both required: the spawn makes an entity exist
                // and the metadata says what it is holding. A spawn on its own
                // renders as nothing at all, which looks exactly like the drop
                // having failed.
                let mut spawn = PacketOut::new(self.0.packets.cb_add_entity);
                spawn
                    .var_int(*entity_id)
                    // Derived from the entity id so a re-send names the same
                    // entity; nothing here needs it to be unguessable.
                    .uuid(0x1000_0000_0000_4000_8000_0000_0000_0000u128 | *entity_id as u128)
                    .var_int(self.0.entity_type("minecraft:item").unwrap_or(0))
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .bytes(&ZERO_VELOCITY)
                    .u8(0)
                    .u8(0)
                    .u8(0)
                    .var_int(0);

                let mut meta = PacketOut::new(self.0.packets.cb_set_entity_data);
                meta.var_int(*entity_id)
                    .u8(ITEM_DATA_INDEX)
                    .var_int(META_ITEM_STACK);
                match self.0.item_id(item) {
                    Some(id) => inventory::write_item(&mut meta, id, *count as i32),
                    None => inventory::write_empty(&mut meta),
                }
                meta.u8(META_END);
                vec![spawn, meta]
            }
            ServerEvent::MoveEntity { entity_id, x, y, z } => vec![play::position_sync(
                self.0,
                *entity_id,
                (*x, *y, *z),
                0.0,
                0.0,
                true,
            )],
            ServerEvent::EntityMove(h) => {
                let pos = h.pos();
                let tp = play::position_sync(
                    self.0,
                    h.entity_id,
                    (pos.x, pos.y, pos.z),
                    pos.yaw,
                    pos.pitch,
                    pos.on_ground,
                );
                let mut head = PacketOut::new(self.0.packets.cb_rotate_head);
                head.var_int(h.entity_id).u8(angle(pos.yaw));
                vec![tp, head]
            }
            ServerEvent::DespawnEntity(eid) => {
                let mut p = PacketOut::new(self.0.packets.cb_remove_entities);
                p.var_int(1).var_int(*eid);
                vec![p]
            }
            ServerEvent::BlockChange { x, y, z, block } => {
                let mut p = PacketOut::new(self.0.packets.cb_block_update);
                p.i64(encode_position(*x as i64, *y as i64, *z as i64))
                    .var_int(self.0.state(*block) as i32);
                vec![p]
            }
            ServerEvent::AckBlockChange(seq) => {
                let mut p = PacketOut::new(self.0.packets.cb_block_changed_ack);
                p.var_int(*seq);
                vec![p]
            }
            ServerEvent::OpenContainer { title, slots } => {
                // Two packets, and the order is not optional: a client that
                // receives contents for a window it has not opened discards
                // them, and the window then shows as empty with no error
                // anywhere.
                let mut open = PacketOut::new(self.0.packets.cb_open_screen);
                open.var_int(STASH_WINDOW)
                    .var_int(self.0.menu_double_chest)
                    // The title is an *anonymous* NBT component, the same
                    // shape System Chat uses in this generation.
                    .bytes(&super::nbt::string(title).to_network());

                let mut items = PacketOut::new(self.0.packets.cb_container_set_content);
                items
                    .var_int(STASH_WINDOW)
                    .var_int(1) // state id
                    .var_int(slots.len() as i32);
                for slot in slots {
                    write_container_slot(self.0, &mut items, slot);
                }
                inventory::write_empty(&mut items); // nothing on the cursor
                vec![open, items]
            }
            ServerEvent::SetCenterChunk { cx, cz } => {
                let mut p = PacketOut::new(self.0.packets.cb_set_chunk_cache_center);
                p.var_int(*cx).var_int(*cz);
                vec![p]
            }
            ServerEvent::CommandTree => {
                let mut p = PacketOut::new(self.0.packets.cb_commands);
                // From 1.19 on a parser is an index into the registry.
                super::commands::write_tree(&mut p, super::commands::Parsers::ById);
                vec![p]
            }
            ServerEvent::Chat(text) => {
                // System Chat carries an *anonymous* NBT text component here —
                // a bare tag with no name — where 1.8 sent a JSON string. A
                // plain string tag is a valid component on its own.
                let mut p = PacketOut::new(self.0.packets.cb_system_chat);
                p.bytes(&super::nbt::string(text).to_network()).bool(false); // not an action-bar overlay
                vec![p]
            }
            other => play::encode(self.0, other).unwrap_or_default(),
        }
    }

    /// Parse a received packet.
    ///
    /// Two deliberate gaps, both consequences of this server having no
    /// inventory model: block placement always places stone (1.21's placement
    /// packet names only the hand, not the held item, so the server is
    /// expected to already know the inventory), and chat signatures are read
    /// past rather than verified.
    fn initial_hotbar(&self) -> Vec<(String, u8)> {
        inventory::HOTBAR
            .iter()
            .filter_map(|(_, id)| items::name_of(*id).map(|n| (n.to_owned(), 1u8)))
            .collect()
    }

    fn block_for_item(&self, item: &str) -> Option<BlockStateId> {
        // Almost every placeable item shares its name with the block it places,
        // so the registry answers this directly. This used to be a lookup in a
        // seven-entry hotbar table, which meant every other block a player
        // selected was placed as **stone** — the fallback at the end of the
        // chain. A headless client found it in one run.
        use aether_world::registry::blocks::default_state;
        // The block's *default* state. Choosing a better one — a stair facing
        // the player, a slab in the half they clicked — is the placement's job
        // and needs context this method is not given; see DESIGN_NOTES §9.4.
        if let Some(id) = default_state(item) {
            return Some(id);
        }
        // The handful whose item and block names differ.
        items::block_name_for_item(item).and_then(default_state)
    }

    fn supports_containers(&self) -> bool {
        true
    }

    fn full_gameplay(&self) -> bool {
        true
    }

    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
        // Both block packets end with the client's prediction sequence, which
        // has to travel back in an ack or the client never applies the
        // server's view of the block. It is read from the end because the
        // fields in front of it differ between releases.
        let seq = super::trailing_var_int(&pkt.data);
        let p = self.0.packets;
        match pkt.id {
            id if id == p.sb_set_carried_item => match pin.u16() {
                Ok(slot) => ClientEvent::HeldSlot(slot as u8),
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_set_creative_mode_slot => {
                // Only the head of the stack is read: the count tells us
                // whether anything is there, the id says what. The component
                // arrays that follow are the client's business, and the rest
                // of the packet is discarded either way.
                let Ok(slot) = pin.u16() else {
                    return ClientEvent::Ignored;
                };
                let (item, count) = match pin.var_int() {
                    Ok(0) | Err(_) => (None, 0),
                    Ok(n) => (
                        pin.var_int()
                            .ok()
                            .and_then(|id| self.0.item_name(id))
                            .map(str::to_owned),
                        n.clamp(0, u8::MAX as i32) as u8,
                    ),
                };
                ClientEvent::CreativeSlot {
                    slot: slot as i16,
                    item,
                    count,
                }
            }
            id if id == p.sb_move_player_pos => match (pin.f64(), pin.f64(), pin.f64(), pin.u8()) {
                (Ok(x), Ok(y), Ok(z), Ok(flags)) => ClientEvent::Move(PosLook {
                    x,
                    y,
                    z,
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            id if id == p.sb_move_player_pos_rot => {
                match (
                    pin.f64(),
                    pin.f64(),
                    pin.f64(),
                    pin.f32(),
                    pin.f32(),
                    pin.u8(),
                ) {
                    (Ok(x), Ok(y), Ok(z), Ok(yaw), Ok(pitch), Ok(flags)) => {
                        ClientEvent::Move(PosLook {
                            x,
                            y,
                            z,
                            yaw,
                            pitch,
                            on_ground: flags & 1 != 0,
                        })
                    }
                    _ => ClientEvent::Ignored,
                }
            }
            id if id == p.sb_move_player_rot => match (pin.f32(), pin.f32(), pin.u8()) {
                (Ok(yaw), Ok(pitch), Ok(flags)) => ClientEvent::Move(PosLook {
                    yaw,
                    pitch,
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            id if id == p.sb_move_player_status_only => match pin.u8() {
                Ok(flags) => ClientEvent::Move(PosLook {
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_player_action => {
                let Ok(status) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let (x, y, z) = decode_position(packed);
                match status {
                    0 => ClientEvent::StartDig { x, y, z, seq },
                    1 => ClientEvent::CancelDig { seq },
                    2 => ClientEvent::Dig { x, y, z, seq },
                    3 => ClientEvent::DropHeld { all: true },
                    4 => ClientEvent::DropHeld { all: false },
                    5 => ClientEvent::ReleaseUse,
                    6 => ClientEvent::SwapHands,
                    _ => ClientEvent::Ignored,
                }
            }
            id if id == p.sb_change_game_mode => match pin.var_int() {
                Ok(0) => ClientEvent::ChangeGameMode(super::GameMode::Survival),
                Ok(1) => ClientEvent::ChangeGameMode(super::GameMode::Creative),
                _ => ClientEvent::Ignored,
            },
            id if id == p.sb_client_command => match pin.var_int() {
                Ok(0) => ClientEvent::Respawn,
                _ => ClientEvent::Ignored,
            },
            // 26.1 split attacking out of `interact` into a packet of its
            // own; what is left is always a use, with hand and location.
            id if id == p.sb_attack => match pin.var_int() {
                Ok(target) => ClientEvent::Interact {
                    target,
                    attack: true,
                },
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_interact && self.0.gen >= Gen::V26_1 => match pin.var_int() {
                Ok(target) => ClientEvent::Interact {
                    target,
                    attack: false,
                },
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_interact => {
                let (Ok(target), Ok(mouse)) = (pin.var_int(), pin.var_int()) else {
                    return ClientEvent::Ignored;
                };
                if mouse == 2 {
                    let _ = (pin.f32(), pin.f32(), pin.f32());
                }
                ClientEvent::Interact {
                    target,
                    attack: mouse == 1,
                }
            }
            id if id == p.sb_pick_item_from_block => match pin.i64() {
                Ok(packed) => {
                    let (x, y, z) = decode_position(packed);
                    ClientEvent::PickBlock { x, y, z }
                }
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_player_command => {
                let _ = pin.var_int();
                match pin.var_int() {
                    Ok(1) => ClientEvent::Sprint(true),
                    Ok(2) => ClientEvent::Sprint(false),
                    _ => ClientEvent::Ignored,
                }
            }
            id if id == p.sb_player_input => match pin.u8() {
                Ok(flags) => ClientEvent::Input {
                    sneak: flags & 0x20 != 0,
                },
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_swing => ClientEvent::Swing {
                hand: pin.var_int().unwrap_or(0) as u8,
            },
            // 26.3: an empty-handed swing at nothing — the arm swing the
            // swing packet used to report.
            id if id == p.sb_punch => ClientEvent::Swing { hand: 0 },
            id if id == p.sb_use_item => {
                let hand = pin.var_int().unwrap_or(0) as u8;
                let seq = pin.var_int().unwrap_or(0);
                ClientEvent::UseItem { hand, seq }
            }
            id if id == p.sb_use_item_on => {
                let Ok(_hand) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let Ok(face) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                if !(0..=5).contains(&face) {
                    return ClientEvent::Ignored;
                }
                // The cursor follows the face in this generation. It used to
                // be decoded implicitly and discarded; it is how a slab knows
                // which half it is.
                let cursor = match (pin.f32(), pin.f32(), pin.f32()) {
                    (Ok(cx), Ok(cy), Ok(cz)) => (cx, cy, cz),
                    _ => (0.5, 0.5, 0.5),
                };
                let (bx, by, bz) = decode_position(packed);
                let (dx, dy, dz) = super::v47::face_offset(face as u8);
                ClientEvent::Place {
                    x: bx + dx,
                    y: by + dy,
                    z: bz + dz,
                    // This version's placement packet names only the hand, so
                    // the block placed is whatever the selected hotbar slot
                    // holds — which the server is expected to already know.
                    // `decode` is handed no player, so it cannot: the held
                    // slot is per-connection state and this codec is one
                    // shared unit struct. Until the trait passes the player,
                    // fall back to the slot every player is given selected on
                    // join, so placement at least matches what they are
                    // holding until they change slots.
                    block: inventory::block_in_hotbar_slot(0)
                        .unwrap_or(aether_api::block_ids::STONE),
                    seq,
                    face: face as u8,
                    cursor,
                }
            }
            id if id == p.sb_container_click => {
                // windowId, stateId, slot, button, mode. The client's own
                // view of what changed (hashed slots) is deliberately
                // dropped: the server computes the click itself and sends the
                // whole window back, so a client cannot dictate contents.
                let Ok(window) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                let Ok(_state) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                let (Ok(slot), Ok(button), Ok(mode)) = (pin.i16(), pin.u8(), pin.var_int()) else {
                    return ClientEvent::Ignored;
                };
                ClientEvent::WindowClick {
                    window: window as u8,
                    slot,
                    button: button as i8,
                    mode,
                }
            }
            id if id == p.sb_container_close => ClientEvent::ContainerClose,
            id if id == p.sb_chat_command || id == p.sb_chat_command_signed => match pin.string() {
                // The command arrives without its slash; the
                // dispatcher keys on one, so it is put back.
                Ok(text) => ClientEvent::Chat(format!("/{text}")),
                Err(_) => ClientEvent::Ignored,
            },
            id if id == p.sb_chat => match pin.string() {
                Ok(text) => ClientEvent::Chat(text),
                Err(_) => ClientEvent::Ignored,
            },
            _ => ClientEvent::Ignored,
        }
    }
}

/// Write one container slot in this version's item format.
///
/// A slot the item table cannot resolve is drawn as a barrier rather than
/// skipped: a hole in the window would silently shift every later slot and
/// break the layout, while a barrier is visibly "something is here that I
/// cannot show you".
fn write_container_slot(v: &version::Version, p: &mut PacketOut, slot: &super::ContainerSlot) {
    if slot.is_empty() {
        inventory::write_empty(p);
        return;
    }
    let id = v
        .item_id(&slot.item)
        .or_else(|| v.item_id("minecraft:barrier"))
        .unwrap_or(0);
    // The badge shows `wire_count`, which is one for anything past a stack;
    // the real number rides in the name. The wire could carry 4000 — the count
    // is a VarInt since 1.20.5 — but the client renders that badge into the
    // corner of a 16-pixel icon and four digits do not fit there. A label
    // reading `Cobblestone (x4000)` is legible; a smear of pixels is not.
    inventory::write_named_item(
        p,
        v.component_custom_name,
        id,
        slot.wire_count() as i32,
        &slot.label,
    );
}

/// The dimension type the overworld uses, as an index into the registry.
fn dimension_type_id(v: &version::Version) -> i32 {
    v.synced_index("minecraft:dimension_type", registry::DIMENSION_NAME)
        .unwrap_or(0) as i32
}

/// The registries of a version whose client carries them itself.
///
/// The server offers the vanilla `minecraft:core` pack of each release that
/// shares this protocol; the client answers with the ones it has. With the
/// pack, each registry goes by name only — the client fills every entry from
/// its own copy — except the overworld dimension type, which is sent with
/// data: this server's world is taller than vanilla's. Without it (a bot
/// library has no game data) every entry carries its data, as a vanilla
/// server does.
fn send_known_registries(
    s: &mut Conn,
    v: &version::Version,
    core: &[&str],
    synced: &[(&str, &[&str])],
    overworld: fn() -> super::nbt::Nbt,
    data: &[u8],
) -> io::Result<()> {
    let mut offer = PacketOut::new(v.packets.cfg_cb_select_known_packs);
    offer.var_int(core.len() as i32);
    for version in core {
        offer.string("minecraft").string("core").string(version);
    }
    offer.send(s)?;

    let known = loop_until(s, v.packets.cfg_sb_select_known_packs)?;
    let mut pin = PacketIn::new(&known.data);
    let mut has_core = false;
    for _ in 0..pin.var_int().unwrap_or(0).clamp(0, 64) {
        match (pin.string(), pin.string(), pin.string()) {
            (Ok(ns), Ok(id), Ok(ver)) => {
                has_core |= ns == "minecraft" && id == "core" && core.contains(&ver.as_str())
            }
            _ => break,
        }
    }
    let full = if has_core {
        Vec::new()
    } else {
        miniz_oxide::inflate::decompress_to_vec_zlib(data).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "corrupt built-in registry data")
        })?
    };
    let mut at = 0usize;
    let mut next_entry = || -> &[u8] {
        let len = full
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .unwrap_or(0);
        let entry = full.get(at + 4..at + 4 + len).unwrap_or(&[]);
        at += 4 + len;
        entry
    };

    for (id, entries) in synced {
        let mut pkt = PacketOut::new(v.packets.cfg_cb_registry_data);
        pkt.string(id).var_int(entries.len() as i32);
        for key in entries.iter() {
            pkt.string(key);
            let entry = if has_core { &[][..] } else { next_entry() };
            if *id == "minecraft:dimension_type" && *key == registry::DIMENSION_NAME {
                pkt.bool(true)
                    .bytes(&tall_overworld(overworld()).to_network());
            } else if has_core {
                pkt.bool(false);
            } else {
                pkt.bool(true).bytes(entry);
            }
        }
        pkt.send(s)?;
    }
    Ok(())
}

/// A version's own overworld dimension type, at this server's height.
fn tall_overworld(vanilla: super::nbt::Nbt) -> super::nbt::Nbt {
    use super::nbt::Nbt;
    let Nbt::Compound(mut fields) = vanilla else {
        return vanilla;
    };
    for (key, value) in fields.iter_mut() {
        match key.as_str() {
            "min_y" => *value = Nbt::Int(registry::MIN_Y),
            "height" | "logical_height" => *value = Nbt::Int(registry::HEIGHT),
            _ => {}
        }
    }
    Nbt::Compound(fields)
}

/// The "Update Tags" packet for every static registry.
fn tags_packet(v: &version::Version) -> PacketOut {
    let mut p = PacketOut::new(v.packets.cfg_cb_update_tags);
    p.var_int(v.tags.len() as i32);
    for (registry, entries) in v.tags {
        p.string(registry).var_int(entries.len() as i32);
        for (name, ids) in entries.iter() {
            p.string(name).var_int(ids.len() as i32);
            for id in ids.iter() {
                p.var_int(*id as i32);
            }
        }
    }
    p
}

/// Read packets until one with `id` arrives and return it.
fn loop_until(s: &mut Conn, id: i32) -> io::Result<RawPacket> {
    for _ in 0..64 {
        if let Some(p) = read_packet(s)? {
            if p.id == id {
                return Ok(p);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "client never sent the expected handshake packet",
    ))
}

/// Read packets until one with `id` arrives, ignoring the rest.
///
/// The client interleaves its own configuration traffic (client settings,
/// plugin channels) with the handshake steps the server waits on, so skipping
/// is required rather than treating them as protocol errors.
fn wait_for(s: &mut Conn, id: i32) -> io::Result<()> {
    loop_until(s, id).map(|_| ())
}

/// Degrees -> the protocol's angle byte (256ths of a full turn).
fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u8
}

/// Encode a block position into the modern packed `i64`.
///
/// 1.14 reordered the fields relative to 1.8: `X (26) | Z (26) | Y (12)`,
/// with Y in the **low** bits rather than the middle.
fn encode_position(x: i64, y: i64, z: i64) -> i64 {
    ((x & 0x3FF_FFFF) << 38) | ((z & 0x3FF_FFFF) << 12) | (y & 0xFFF)
}

/// Unpack the modern packed block position, sign-extending each field.
fn decode_position(v: i64) -> (i32, i32, i32) {
    (
        (v >> 38) as i32,
        ((v << 52) >> 52) as i32,
        ((v << 26) >> 38) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Codec = Codec(&version::V1_21_11);
    use aether_api::block_ids as b;

    #[test]
    fn the_tags_make_water_water_and_ladders_climbable() {
        let find = |reg: &str, tag: &str| {
            version::V1_21_11
                .tags
                .iter()
                .find(|(r, _)| *r == reg)
                .and_then(|(_, t)| t.iter().find(|(n, _)| *n == tag))
                .map(|(_, ids)| ids.to_vec())
        };
        // flowing_water = 1, water = 2 in the fluid registry.
        assert_eq!(find("minecraft:fluid", "minecraft:water"), Some(vec![1, 2]));
        assert_eq!(find("minecraft:fluid", "minecraft:lava"), Some(vec![3, 4]));
        let ladder =
            aether_world::registry::blocks::block_id_of("minecraft:ladder").unwrap() as u32;
        assert!(find("minecraft:block", "minecraft:climbable")
            .unwrap()
            .contains(&ladder));
        let stone = aether_world::registry::blocks::block_id_of("minecraft:stone").unwrap() as u32;
        assert!(find("minecraft:block", "minecraft:mineable/pickaxe")
            .unwrap()
            .contains(&stone));
        // And the packet carries all of it.
        let mut wire = Vec::new();
        tags_packet(&version::V1_21_11)
            .write_to(&mut wire, None)
            .unwrap();
        assert!(wire.len() > 10_000);
    }

    #[test]
    fn position_layout_differs_from_1_8() {
        // Y sits in the low bits here; the 1.8 codec puts it in the middle.
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
        // And the two encodings really are different, so a mix-up would show.
        let modern = encode_position(1, 2, 3);
        assert_ne!(
            modern,
            ((1i64) << 38) | (2 << 26) | 3,
            "must not be 1.8 order"
        );
    }

    #[test]
    fn every_engine_block_maps_into_the_1_21_registry() {
        // Each engine block must land on its own distinct state, or the world
        // would render as a single material.
        let all = [
            b::AIR,
            b::STONE,
            b::GRASS_BLOCK,
            b::DIRT,
            b::BEDROCK,
            b::WATER,
            b::SAND,
            b::GRAVEL,
            b::OAK_LOG,
            b::OAK_LEAVES,
        ];
        let mut seen: Vec<u32> = all.iter().map(|b| block_state(*b)).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "block states must be distinct");
        assert_eq!(block_state(b::AIR), 0, "air must be state 0");
    }

    #[test]
    fn keep_alive_is_a_long_not_a_varint() {
        // 1.8 sends a varint here; getting this wrong desynchronises the
        // stream on the very first keep-alive.
        let world = chunk::tests_support::Empty;
        let pkts = C.encode(&ServerEvent::KeepAlive(1), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        // frame len, id, then 8 bytes of payload
        assert_eq!(wire[0] as usize, wire.len() - 1);
        assert_eq!(wire[1] as i32, version::V1_21_11.packets.cb_keep_alive);
        assert_eq!(wire.len(), 1 + 1 + 8);
    }

    #[test]
    fn block_change_carries_a_state_id_not_id_shifted_by_four() {
        let world = chunk::tests_support::Empty;
        let pkts = C.encode(
            &ServerEvent::BlockChange {
                x: 1,
                y: 2,
                z: 3,
                block: b::OAK_LEAVES,
            },
            &world,
        );
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        let body = &wire[2..]; // past frame length and packet id
        assert_eq!(
            i64::from_be_bytes(body[0..8].try_into().unwrap()),
            encode_position(1, 2, 3)
        );
        // 279 as a varint: 0x97 0x02
        assert_eq!(&body[8..10], &[0x97, 0x02], "state id 279");
    }

    /// Regression: a real 1.21.11 client rejected `add_entity` with
    /// "found 5 bytes extra". 1.21.9 replaced the velocity's three `i16`s
    /// with a quantized vector whose zero is one byte; six zeros left five of
    /// them in the stream, and every field after it — the angles and the
    /// object data — was read out of the wrong bytes.
    #[test]
    fn a_spawned_item_is_exactly_as_long_as_the_client_expects() {
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let pkts = C.encode(
            &ServerEvent::DropItem {
                entity_id: 7,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item: "minecraft:stone".into(),
                count: 1,
            },
            &Empty,
        );
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        let mut pin = crate::proto::PacketIn::new(&wire);
        let len = pin.var_int().unwrap() as usize;
        assert_eq!(
            pin.var_int().unwrap(),
            version::V1_21_11.packets.cb_add_entity
        );
        // varint id (1) + uuid (16) + varint type (1) + 3 x f64 (24)
        // + velocity (1) + pitch/yaw/head (3) + varint object data (1)
        let body = len - 1; // the packet id inside the length
        assert_eq!(body, 1 + 16 + 1 + 24 + 1 + 3 + 1, "add_entity body length");

        // And the velocity really is the one-byte zero, not six of them.
        let mut rest = crate::proto::PacketIn::new(&wire[wire.len() - body + 1..]);
        rest.var_int().unwrap(); // entity id
        for _ in 0..16 {
            rest.u8().unwrap();
        }
        rest.var_int().unwrap(); // type
        for _ in 0..3 {
            rest.f64().unwrap();
        }
        assert_eq!(
            rest.u8().unwrap(),
            0x00,
            "velocity at rest is one zero byte"
        );
    }

    #[test]
    fn system_chat_carries_an_anonymous_nbt_string() {
        // 1.8 sends JSON here; this version sends an NBT component with no
        // name. Decode per the spec: tag byte, then a length-prefixed string,
        // then the action-bar flag, and nothing left over.
        let world = chunk::tests_support::Empty;
        let pkts = C.encode(&ServerEvent::Chat("hi there".into()), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();

        assert_eq!(wire[1] as i32, version::V1_21_11.packets.cb_system_chat);
        let body = &wire[2..];
        assert_eq!(body[0], 0x08, "TAG_String, and no name follows it");
        let len = u16::from_be_bytes([body[1], body[2]]) as usize;
        assert_eq!(&body[3..3 + len], b"hi there");
        assert_eq!(body[3 + len], 0, "not an action bar");
        assert_eq!(body.len(), 3 + len + 1, "nothing may trail the flag");
    }

    #[test]
    fn chat_decodes_the_message_ahead_of_its_signature() {
        // The serverbound packet is message, timestamp, salt, signature, ...
        // Only the message matters here, but it has to be read from the front:
        // treating the packet as 1.8's bare string would still "work" and then
        // silently break the moment anything is read after it.
        let mut body = Vec::new();
        body.extend_from_slice(&[5]); // varint string length
        body.extend_from_slice(b"hello");
        body.extend_from_slice(&0i64.to_be_bytes()); // timestamp
        body.extend_from_slice(&0i64.to_be_bytes()); // salt
        body.push(0); // no signature
        let pkt = RawPacket {
            id: version::V1_21_11.packets.sb_chat,
            data: body,
        };
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        };
        match C.decode(&pkt, prev) {
            ClientEvent::Chat(text) => assert_eq!(text, "hello"),
            _ => panic!("a chat packet must decode as chat"),
        }
    }

    /// A `PosLook` for decode tests that do not care about position.
    fn any_pos() -> PosLook {
        PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        }
    }

    #[test]
    fn a_selected_slot_decodes_as_held_slot() {
        let pkt = RawPacket {
            id: version::V1_21_11.packets.sb_set_carried_item,
            data: 3u16.to_be_bytes().to_vec(),
        };
        match C.decode(&pkt, any_pos()) {
            ClientEvent::HeldSlot(slot) => assert_eq!(slot, 3),
            _ => panic!("a held-item packet must decode as a slot selection"),
        }
    }

    #[test]
    fn a_creative_slot_decodes_with_its_item() {
        // Slot 36 (the first hotbar slot) filled with one stone: count, id,
        // then the two component counts that follow every non-empty stack.
        let mut data = 36u16.to_be_bytes().to_vec();
        data.extend_from_slice(&[1, 1, 0, 0]);
        let pkt = RawPacket {
            id: version::V1_21_11.packets.sb_set_creative_mode_slot,
            data,
        };
        match C.decode(&pkt, any_pos()) {
            ClientEvent::CreativeSlot { slot, item, count } => {
                assert_eq!(slot, 36);
                assert_eq!(item.as_deref(), Some("minecraft:stone"));
                assert_eq!(count, 1);
            }
            _ => panic!("a creative-slot packet must decode as one"),
        }
    }

    #[test]
    fn an_emptied_creative_slot_decodes_as_no_item() {
        // A zero count ends the stack: no id follows, and reading one anyway
        // would take the next packet's bytes.
        let mut data = 36u16.to_be_bytes().to_vec();
        data.push(0);
        let pkt = RawPacket {
            id: version::V1_21_11.packets.sb_set_creative_mode_slot,
            data,
        };
        match C.decode(&pkt, any_pos()) {
            ClientEvent::CreativeSlot { item, .. } => assert_eq!(item, None),
            _ => panic!("an emptied creative slot must still decode"),
        }
    }

    #[test]
    fn the_hotbar_we_hand_out_maps_back_to_its_blocks() {
        // Placement resolves the held item through this table, so every item
        // the join hotbar contains has to resolve back to the block it came
        // from — otherwise selecting a slot silently places something else.
        for (block, item) in inventory::HOTBAR {
            let name = items::name_of(item).expect("hotbar item is in this version");
            assert_eq!(C.block_for_item(name), Some(block), "item {name}");
        }
        // Named, and every name resolves: a hotbar entry whose item this
        // version does not have would be dropped by the filter and the player
        // would silently join holding one thing fewer.
        assert_eq!(C.initial_hotbar().len(), inventory::HOTBAR.len());
    }

    #[test]
    fn only_ids_beyond_this_versions_states_fall_back_to_stone() {
        // 9,999 used to be "no such block"; it is now a perfectly real state,
        // so the substitution boundary moved to the end of the state space.
        // Below it, nothing is substituted at all.
        use aether_world::registry::blocks::STATE_COUNT;
        assert_eq!(block_state(BlockStateId(9_999)), 9_999);
        assert_eq!(
            block_state(BlockStateId(STATE_COUNT as u32)),
            aether_world::registry::ids::STONE.raw()
        );
    }

    #[test]
    fn the_ack_carries_the_sequence_and_sits_four_below_block_update() {
        // Two independent claims, because getting either wrong is silent: the
        // client would either drop the packet as malformed or read an ack as
        // some other block packet.
        assert_eq!(
            version::V1_21_11.packets.cb_block_changed_ack + 4,
            version::V1_21_11.packets.cb_block_update
        );
        // A block source is required by the signature but never consulted:
        // the ack names no block.
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let pkts = C.encode(&ServerEvent::AckBlockChange(300), &Empty);
        assert_eq!(pkts.len(), 1);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        // length, id, then 300 as a VarInt: 0b10101100, 0b00000010.
        assert_eq!(
            wire,
            vec![
                3,
                version::V1_21_11.packets.cb_block_changed_ack as u8,
                0xAC,
                0x02
            ]
        );
    }

    #[test]
    fn a_quantity_past_a_stack_moves_from_the_badge_into_the_name() {
        // The badge is drawn into the corner of a 16-pixel icon, so four
        // digits are a smear. Past a stack the count becomes `1` on the wire
        // and the real number rides in the slot's custom name, where it is
        // legible.
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let slot =
            crate::protocol::ContainerSlot::block("minecraft:cobblestone", 4000, "cobblestone")
                .with_quantity();
        assert_eq!(slot.wire_count(), 1);
        assert_eq!(slot.label, "cobblestone (x4000)");

        let pkts = C.encode(
            &ServerEvent::OpenContainer {
                title: "t".into(),
                slots: vec![slot],
            },
            &Empty,
        );
        assert_eq!(pkts.len(), 2, "open screen, then contents");

        // Decoded from the format description, not by calling the encoder:
        // packet id, window id, state id, slot count, then the stack —
        // itemCount, itemId, addedComponentCount, removedComponentCount.
        let body = pkts[1].body();
        let mut at = 0usize;
        let varint = |b: &[u8], at: &mut usize| -> i64 {
            let (mut v, mut shift) = (0i64, 0);
            loop {
                let byte = b[*at];
                *at += 1;
                v |= ((byte & 0x7F) as i64) << shift;
                if byte & 0x80 == 0 {
                    return v;
                }
                shift += 7;
            }
        };
        assert_eq!(
            varint(body, &mut at),
            version::V1_21_11.packets.cb_container_set_content as i64,
            "packet id"
        );
        assert_eq!(varint(body, &mut at), STASH_WINDOW as i64, "window");
        assert_eq!(varint(body, &mut at), 1, "state id");
        assert_eq!(varint(body, &mut at), 1, "one slot");
        assert_eq!(varint(body, &mut at), 1, "the badge shows one");
        assert_eq!(
            varint(body, &mut at),
            items::item_id("minecraft:cobblestone").unwrap() as i64
        );
        assert_eq!(varint(body, &mut at), 1, "one component added");
        assert_eq!(varint(body, &mut at), 0, "none removed");
        assert_eq!(varint(body, &mut at), 6, "custom_name");
        // The quantity has to survive into the bytes a player will read.
        let tail = String::from_utf8_lossy(&body[at..]);
        assert!(tail.contains("(x4000)"), "name was {tail:?}");
    }

    #[test]
    fn a_count_within_a_stack_stays_in_the_badge() {
        // Below the threshold nothing changes: the badge is the right place
        // for a number a player can actually read there.
        let slot =
            crate::protocol::ContainerSlot::block("minecraft:stone", 32, "stone").with_quantity();
        assert_eq!(slot.wire_count(), 32);
        assert_eq!(slot.label, "stone", "no suffix when the badge suffices");
    }

    #[test]
    fn a_dropped_stack_spawns_an_entity_and_then_says_what_it_holds() {
        // A spawn on its own renders as nothing at all, which looks exactly
        // like the drop having failed — so both packets, in this order.
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let pkts = C.encode(
            &ServerEvent::DropItem {
                entity_id: -1234,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item: "minecraft:diamond".into(),
                count: 200,
            },
            &Empty,
        );
        assert_eq!(pkts.len(), 2);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[1] as i32, version::V1_21_11.packets.cb_add_entity);
        let mut wire = Vec::new();
        pkts[1].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[1] as i32, version::V1_21_11.packets.cb_set_entity_data);
        // ...and the metadata list is terminated, or the client reads the next
        // packet as more metadata.
        assert_eq!(*wire.last().unwrap(), META_END);
    }

    #[test]
    fn every_engine_block_translates_to_this_versions_state() {
        // The engine's ids are this version's block ids, so the whole table
        // must resolve — no substitutions at all. Before the table existed
        // this was ten blocks and a stone fallback, so placing anything else
        // produced a stone cube.
        use aether_world::registry::blocks;
        // The translation is the identity now, over the whole state space —
        // not a table over blocks. Every state must survive it unchanged.
        for state in [0u32, 1, 137, 3717, 6795, blocks::STATE_COUNT as u32 - 1] {
            assert_eq!(block_state(BlockStateId(state)), state);
        }
        for (i, row) in blocks::BLOCKS.iter().enumerate() {
            let d = blocks::DEFAULT_STATE[i];
            assert_eq!(block_state(BlockStateId(d)), d, "{}", row.0);
        }
    }

    #[test]
    fn distinct_blocks_get_distinct_states() {
        // A collision here would render two different blocks identically, and
        // nothing else in the pipeline would notice.
        use aether_world::registry::blocks;
        use std::collections::HashSet;
        let mut seen: HashSet<u32> = HashSet::new();
        for (i, row) in blocks::BLOCKS.iter().enumerate() {
            let state = block_state(BlockStateId(blocks::DEFAULT_STATE[i]));
            assert!(
                seen.insert(state),
                "{} shares a state with another block",
                row.0
            );
        }
    }

    #[test]
    fn a_block_beyond_the_table_substitutes_rather_than_escaping_the_registry() {
        // Only reachable for a modded block. Handing the client an id outside
        // its own registry disconnects it.
        assert_eq!(
            block_state(BlockStateId(999_999)),
            aether_world::registry::ids::STONE.raw()
        );
    }

    #[test]
    fn any_block_item_resolves_to_its_block() {
        // The regression a headless client caught: everything outside a
        // seven-entry hotbar table was placed as stone, because stone is the
        // fallback at the end of the chain. The check is over the whole
        // vanilla set, not a sample.
        use aether_world::registry::blocks::{BLOCKS, DEFAULT_STATE as BLOCKS_DEFAULT};
        let mut resolved = 0usize;
        for (i, row) in BLOCKS.iter().enumerate() {
            if items::item_id(row.0).is_none() {
                continue; // a block with no item — a door's upper half, water
            }
            assert_eq!(
                C.block_for_item(row.0),
                Some(BlockStateId(BLOCKS_DEFAULT[i])),
                "{} resolved wrong",
                row.0
            );
            resolved += 1;
        }
        assert!(resolved > 700, "only {resolved} blocks have items?");
    }

    #[test]
    fn the_items_whose_names_differ_still_resolve() {
        for (item, block) in [
            ("minecraft:redstone", "minecraft:redstone_wire"),
            ("minecraft:water_bucket", "minecraft:water"),
            ("minecraft:wheat_seeds", "minecraft:wheat"),
        ] {
            let got = C.block_for_item(item).expect(item);
            // The id is a *state*, so the block it belongs to is one lookup
            // away — indexing `BLOCKS` with it would read a random row.
            assert_eq!(
                aether_world::registry::blocks::block_of_state(got).map(|(_, n)| n),
                Some(block)
            );
        }
    }

    #[test]
    fn a_non_block_item_places_nothing() {
        // A sword must not become a block, however the chain falls through.
        assert_eq!(C.block_for_item("minecraft:diamond_sword"), None);
        assert_eq!(C.block_for_item("minecraft:stick"), None);
    }
}
