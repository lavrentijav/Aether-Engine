//! Minecraft 1.21.11 (protocol 774) codec.
//!
//! Protocol id and every packet id, block-state id and entity-type id below
//! come from PrismarineJS `minecraft-data` for **pc/1.21.11** — that
//! distribution carries its own protocol definition for this exact release,
//! so none of it is inferred from a neighbouring version.
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
pub mod inventory;
pub mod items;
pub mod play;
pub mod registry;

use std::io;

use aether_world::BlockStateId;

use super::{BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

// --- Packet ids (1.21.11) ---
const LOGIN_SUCCESS: i32 = 0x02;
const LOGIN_ACKNOWLEDGED: i32 = 0x03;
const CFG_FINISH: i32 = 0x03;
const CFG_REGISTRY_DATA: i32 = 0x07;
const CFG_ADD_RESOURCE_PACK: i32 = 0x09;
const CFG_FINISH_ACK: i32 = 0x03;
const PLAY_LOGIN: i32 = 0x30;
const PLAY_CHUNK: i32 = 0x2C;
/// Play: move the centre of the client's loaded-column window. Columns
/// arriving outside it are discarded on receipt.
const PLAY_SET_CENTER_CHUNK: i32 = 0x5c;
const PLAY_UNLOAD_CHUNK: i32 = 0x25;
const PLAY_BLOCK_CHANGE: i32 = 0x08;
/// Play: release the client's block prediction up to a sequence.
///
/// Four below Block Update: the clientbound ids are registered in
/// alphabetical order and `block_changed_ack`, `block_destruction`,
/// `block_entity_data`, `block_event` and `block_update` are
/// contiguous in every release that has them.
const PLAY_BLOCK_CHANGED_ACK: i32 = 0x04;
const PLAY_PLAYER_INFO: i32 = 0x44;
const PLAY_PLAYER_REMOVE: i32 = 0x43;
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

const PLAY_SPAWN_ENTITY: i32 = 0x01;
const PLAY_SYNC_ENTITY_POS: i32 = 0x23;
const PLAY_HEAD_ROTATION: i32 = 0x51;
const PLAY_DESTROY_ENTITIES: i32 = 0x4B;
const PLAY_KEEP_ALIVE: i32 = 0x2B;
const PLAY_POSITION: i32 = 0x46;
const PLAY_SYSTEM_CHAT: i32 = 0x77;
const PLAY_GAME_EVENT: i32 = 0x26;
const PLAY_ABILITIES: i32 = 0x3E;
const PLAY_WINDOW_ITEMS: i32 = 0x12;
/// Play: open a container screen. Ids in this block are from `minecraft-data`
/// `pc/1.21.11/protocol.json` and cross-check against every other id already
/// in this file.
const PLAY_OPEN_WINDOW: i32 = 0x39;
/// The `minecraft:menu` registry id of `generic_9x6` — a double chest.
///
/// The generic_9xN menus have occupied 0..=5 since containers were registered,
/// and this server only ever opens the largest, so one constant covers it.
const MENU_GENERIC_9X6: i32 = 5;
/// The window this server opens its own containers under. Never 0, which is
/// the player's own inventory and must not be replaced.
const STASH_WINDOW: i32 = 1;
const PLAY_HELD_ITEM: i32 = 0x67;
/// Play: the player's health, hunger and saturation.
///
/// A vanilla client tolerates never receiving this and shows its defaults, so
/// it went unnoticed. Two things do not tolerate it: survival mode, where the
/// HUD is meaningless without it, and every headless client library, which
/// waits on it as the "you are really in the world now" signal — mineflayer
/// will not emit `spawn` until it arrives.
const PLAY_UPDATE_HEALTH: i32 = 0x66;

// Serverbound play.
/// "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x10;
const SB_CHAT: i32 = 0x08;
/// A slash command. Since 1.19 the client sends these on their own packet
/// instead of as a chat message, so a server that only decodes chat never
/// sees a single command — which is exactly what happened here.
const SB_CHAT_COMMAND: i32 = 0x06;
/// The same command, carrying the signatures of the arguments it quotes.
/// The command itself is still the first field, so both decode alike.
const SB_CHAT_COMMAND_SIGNED: i32 = 0x07;
const SB_POSITION: i32 = 0x1D;
const SB_POSITION_LOOK: i32 = 0x1E;
const SB_LOOK: i32 = 0x1F;
const SB_FLYING: i32 = 0x20;
/// Serverbound: the player selected a different hotbar slot.
const SB_HELD_ITEM: i32 = 0x34;
/// Serverbound: in creative the client fills a slot from its own menu and
/// tells the server what it put there.
const SB_CREATIVE_SLOT: i32 = 0x37;
const SB_BLOCK_DIG: i32 = 0x28;
const SB_BLOCK_PLACE: i32 = 0x3F;
/// Serverbound: a click inside an open container.
const SB_WINDOW_CLICK: i32 = 0x11;
/// Serverbound: respawn / statistics request.
const SB_CLIENT_COMMAND: i32 = 0x0B;
/// Serverbound: attack or use an entity.
const SB_USE_ENTITY: i32 = 0x19;
/// Serverbound: creative middle-click on a block.
const SB_PICK_BLOCK: i32 = 0x23;
/// Serverbound: sprinting and friends.
const SB_ENTITY_ACTION: i32 = 0x29;
/// Serverbound: the movement keys, sneaking among them since 1.21.6.
const SB_PLAYER_INPUT: i32 = 0x2A;
/// Serverbound: arm swing.
const SB_ARM_ANIMATION: i32 = 0x3C;
/// Serverbound: right-click with an item in the air.
const SB_USE_ITEM: i32 = 0x40;
/// Serverbound: the player closed a container.
const SB_CLOSE_WINDOW: i32 = 0x12;

/// Entity type id for `minecraft:player` in 1.21.11.
const PLAYER_ENTITY_TYPE: i32 = 155;
/// Entity type id for `minecraft:item`.
const ITEM_ENTITY_TYPE: i32 = 71;
/// Play: entity metadata. A dropped item is an entity that carries no item
/// until this arrives, so the spawn packet alone renders nothing.
const PLAY_ENTITY_METADATA: i32 = 0x61;
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

/// The 1.21.11 codec.
pub struct Codec;

/// Engine block id -> 1.21.11 block-state id.
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
        "1.21.11"
    }

    fn protocol_id(&self) -> i32 {
        774
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        // Login Start: username, then the client's own UUID (ignored — this
        // server derives an offline UUID from the name like 1.8 does).
        let Some(p) = read_packet(s)? else {
            return Ok(None);
        };
        if p.id != 0x00 {
            return Ok(None);
        }
        Ok(Some(PacketIn::new(&p.data).string()?))
    }

    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Login Success: UUID as 16 raw bytes here (1.8 wanted a hyphenated
        // string in the same packet), username, then an empty property list.
        PacketOut::new(LOGIN_SUCCESS)
            .uuid(p.uuid)
            .string(&p.name)
            .var_int(0)
            .send(s)?;

        // The client acknowledges and moves itself into the configuration
        // state; nothing may be sent in between.
        wait_for(s, LOGIN_ACKNOWLEDGED)?;

        // Configuration: hand over the registries the client builds its world
        // from. Without these it disconnects before ever reaching play.
        for (id, entries) in registry::registries() {
            let mut pkt = PacketOut::new(CFG_REGISTRY_DATA);
            pkt.string(id).var_int(entries.len() as i32);
            for (key, value) in entries {
                pkt.string(&key)
                    .bool(true) // this entry carries data
                    .bytes(&value.to_network());
            }
            pkt.send(s)?;
        }

        // Add Resource Pack, offered while still in configuration so the
        // client downloads before the world appears.
        if p.resource_pack.enabled() {
            PacketOut::new(CFG_ADD_RESOURCE_PACK)
                .uuid(RESOURCE_PACK_ID)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .bool(p.resource_pack.required)
                .bool(false) // no custom prompt message
                .send(s)?;
        }

        PacketOut::new(CFG_FINISH).send(s)?;
        wait_for(s, CFG_FINISH_ACK)?;

        // Play Login.
        let mut login = PacketOut::new(PLAY_LOGIN);
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
            .var_int(0) // dimension type: index 0 in the registry above
            .string(registry::DIMENSION_NAME)
            .i64(0) // hashed seed
            .u8(p.game_mode.wire()) // survival 0 / creative 1
            .u8(0xFF) // previous game mode: none
            .bool(false) // not a debug world
            .bool(false) // not superflat
            .bool(false) // no death location
            .var_int(0) // portal cooldown
            .var_int(63) // sea level
            .bool(false); // does not enforce secure chat
        login.send(s)?;

        // Tell the client to start waiting for chunks rather than rendering
        // an empty void while the first batch is in flight.
        PacketOut::new(PLAY_GAME_EVENT).u8(13).f32(0.0).send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Synchronize Player Position. Deltas are zero and `flags` is 0, so
        // every value is absolute.
        PacketOut::new(PLAY_POSITION)
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
        PacketOut::new(PLAY_ABILITIES)
            .u8(flags)
            .f32(0.05) // flying speed
            .f32(0.10) // walking speed
            .send(s)?;

        // The inventory itself follows from the session, which owns it: the
        // server's copy is authoritative and is sent whole once the player
        // is registered.
        inventory::held_item_packet(PLAY_HELD_ITEM, 0).send(s)?;

        // Full health and food. This server models neither, so the values are
        // constant — but sending them is not optional: see the constant's
        // comment.
        PacketOut::new(PLAY_UPDATE_HEALTH)
            .f32(20.0)
            .var_int(20)
            .f32(5.0)
            .send(s)?;
        Ok(())
    }

    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut> {
        match ev {
            ServerEvent::KeepAlive(id) => {
                let mut p = PacketOut::new(PLAY_KEEP_ALIVE);
                p.i64(*id); // a long here, a varint in 1.8
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                vec![chunk::chunk_data_packet(PLAY_CHUNK, *cx, *cz, world)]
            }
            ServerEvent::UnloadColumn { cx, cz } => {
                vec![chunk::unload_chunk_packet(PLAY_UNLOAD_CHUNK, *cx, *cz)]
            }
            ServerEvent::TabListAdd(h) => {
                let mut p = PacketOut::new(PLAY_PLAYER_INFO);
                p.u8(0x01 | 0x08) // add_player | update_listed
                    .var_int(1) // one entry
                    .uuid(h.uuid)
                    .string(&h.name)
                    .var_int(0) // no profile properties
                    .bool(true); // listed
                vec![p]
            }
            ServerEvent::TabListRemove(uuid) => {
                let mut p = PacketOut::new(PLAY_PLAYER_REMOVE);
                p.var_int(1).uuid(*uuid);
                vec![p]
            }
            ServerEvent::SpawnPlayer(h) => {
                let pos = h.pos();
                let mut p = PacketOut::new(PLAY_SPAWN_ENTITY);
                p.var_int(h.entity_id)
                    .uuid(h.uuid)
                    .var_int(PLAYER_ENTITY_TYPE)
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
                let mut spawn = PacketOut::new(PLAY_SPAWN_ENTITY);
                spawn
                    .var_int(*entity_id)
                    // Derived from the entity id so a re-send names the same
                    // entity; nothing here needs it to be unguessable.
                    .uuid(0x1000_0000_0000_4000_8000_0000_0000_0000u128 | *entity_id as u128)
                    .var_int(ITEM_ENTITY_TYPE)
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .bytes(&ZERO_VELOCITY)
                    .u8(0)
                    .u8(0)
                    .u8(0)
                    .var_int(0);

                let mut meta = PacketOut::new(PLAY_ENTITY_METADATA);
                meta.var_int(*entity_id)
                    .u8(ITEM_DATA_INDEX)
                    .var_int(META_ITEM_STACK);
                match items::item_id(item) {
                    Some(id) => inventory::write_item(&mut meta, id, *count as i32),
                    None => inventory::write_empty(&mut meta),
                }
                meta.u8(META_END);
                vec![spawn, meta]
            }
            ServerEvent::MoveEntity { entity_id, x, y, z } => {
                let mut p = PacketOut::new(PLAY_SYNC_ENTITY_POS);
                p.var_int(*entity_id)
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .f64(0.0)
                    .f64(0.0)
                    .f64(0.0)
                    .f32(0.0)
                    .f32(0.0)
                    .bool(true);
                vec![p]
            }
            ServerEvent::EntityMove(h) => {
                let pos = h.pos();
                let mut tp = PacketOut::new(PLAY_SYNC_ENTITY_POS);
                tp.var_int(h.entity_id)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .f64(0.0)
                    .f64(0.0)
                    .f64(0.0)
                    .f32(pos.yaw)
                    .f32(pos.pitch)
                    .bool(pos.on_ground);
                let mut head = PacketOut::new(PLAY_HEAD_ROTATION);
                head.var_int(h.entity_id).u8(angle(pos.yaw));
                vec![tp, head]
            }
            ServerEvent::DespawnEntity(eid) => {
                let mut p = PacketOut::new(PLAY_DESTROY_ENTITIES);
                p.var_int(1).var_int(*eid);
                vec![p]
            }
            ServerEvent::BlockChange { x, y, z, block } => {
                let mut p = PacketOut::new(PLAY_BLOCK_CHANGE);
                p.i64(encode_position(*x as i64, *y as i64, *z as i64))
                    .var_int(block_state(*block) as i32);
                vec![p]
            }
            ServerEvent::AckBlockChange(seq) => {
                let mut p = PacketOut::new(PLAY_BLOCK_CHANGED_ACK);
                p.var_int(*seq);
                vec![p]
            }
            ServerEvent::OpenContainer { title, slots } => {
                // Two packets, and the order is not optional: a client that
                // receives contents for a window it has not opened discards
                // them, and the window then shows as empty with no error
                // anywhere.
                let mut open = PacketOut::new(PLAY_OPEN_WINDOW);
                open.var_int(STASH_WINDOW)
                    .var_int(MENU_GENERIC_9X6)
                    // The title is an *anonymous* NBT component, the same
                    // shape System Chat uses in this generation.
                    .bytes(&super::nbt::string(title).to_network());

                let mut items = PacketOut::new(PLAY_WINDOW_ITEMS);
                items
                    .var_int(STASH_WINDOW)
                    .var_int(1) // state id
                    .var_int(slots.len() as i32);
                for slot in slots {
                    write_container_slot(&mut items, slot);
                }
                inventory::write_empty(&mut items); // nothing on the cursor
                vec![open, items]
            }
            ServerEvent::SetCenterChunk { cx, cz } => {
                let mut p = PacketOut::new(PLAY_SET_CENTER_CHUNK);
                p.var_int(*cx).var_int(*cz);
                vec![p]
            }
            ServerEvent::CommandTree => {
                let mut p = PacketOut::new(PLAY_COMMANDS);
                // From 1.19 on a parser is an index into the registry.
                super::commands::write_tree(&mut p, super::commands::Parsers::ById);
                vec![p]
            }
            ServerEvent::Chat(text) => {
                // System Chat carries an *anonymous* NBT text component here —
                // a bare tag with no name — where 1.8 sent a JSON string. A
                // plain string tag is a valid component on its own.
                let mut p = PacketOut::new(PLAY_SYSTEM_CHAT);
                p.bytes(&super::nbt::string(text).to_network()).bool(false); // not an action-bar overlay
                vec![p]
            }
            other => play::encode(other).unwrap_or_default(),
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
        match pkt.id {
            SB_HELD_ITEM => match pin.u16() {
                Ok(slot) => ClientEvent::HeldSlot(slot as u8),
                Err(_) => ClientEvent::Ignored,
            },
            SB_CREATIVE_SLOT => {
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
                            .and_then(items::name_of)
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
            SB_POSITION => match (pin.f64(), pin.f64(), pin.f64(), pin.u8()) {
                (Ok(x), Ok(y), Ok(z), Ok(flags)) => ClientEvent::Move(PosLook {
                    x,
                    y,
                    z,
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            SB_POSITION_LOOK => {
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
            SB_LOOK => match (pin.f32(), pin.f32(), pin.u8()) {
                (Ok(yaw), Ok(pitch), Ok(flags)) => ClientEvent::Move(PosLook {
                    yaw,
                    pitch,
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            SB_FLYING => match pin.u8() {
                Ok(flags) => ClientEvent::Move(PosLook {
                    on_ground: flags & 1 != 0,
                    ..prev
                }),
                Err(_) => ClientEvent::Ignored,
            },
            SB_BLOCK_DIG => {
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
            SB_CLIENT_COMMAND => match pin.var_int() {
                Ok(0) => ClientEvent::Respawn,
                _ => ClientEvent::Ignored,
            },
            SB_USE_ENTITY => {
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
            SB_PICK_BLOCK => match pin.i64() {
                Ok(packed) => {
                    let (x, y, z) = decode_position(packed);
                    ClientEvent::PickBlock { x, y, z }
                }
                Err(_) => ClientEvent::Ignored,
            },
            SB_ENTITY_ACTION => {
                let _ = pin.var_int();
                match pin.var_int() {
                    Ok(1) => ClientEvent::Sprint(true),
                    Ok(2) => ClientEvent::Sprint(false),
                    _ => ClientEvent::Ignored,
                }
            }
            SB_PLAYER_INPUT => match pin.u8() {
                Ok(flags) => ClientEvent::Input {
                    sneak: flags & 0x20 != 0,
                },
                Err(_) => ClientEvent::Ignored,
            },
            SB_ARM_ANIMATION => ClientEvent::Swing {
                hand: pin.var_int().unwrap_or(0) as u8,
            },
            SB_USE_ITEM => {
                let hand = pin.var_int().unwrap_or(0) as u8;
                let seq = pin.var_int().unwrap_or(0);
                ClientEvent::UseItem { hand, seq }
            }
            SB_BLOCK_PLACE => {
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
            SB_WINDOW_CLICK => {
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
            SB_CLOSE_WINDOW => ClientEvent::ContainerClose,
            SB_CHAT_COMMAND | SB_CHAT_COMMAND_SIGNED => match pin.string() {
                // The command arrives without its slash; the
                // dispatcher keys on one, so it is put back.
                Ok(text) => ClientEvent::Chat(format!("/{text}")),
                Err(_) => ClientEvent::Ignored,
            },
            SB_CHAT => match pin.string() {
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
fn write_container_slot(p: &mut PacketOut, slot: &super::ContainerSlot) {
    if slot.is_empty() {
        inventory::write_empty(p);
        return;
    }
    let id = items::item_for_block(&slot.item)
        .or_else(|| items::item_id("minecraft:barrier"))
        .unwrap_or(0);
    // The badge shows `wire_count`, which is one for anything past a stack;
    // the real number rides in the name. The wire could carry 4000 — the count
    // is a VarInt since 1.20.5 — but the client renders that badge into the
    // corner of a 16-pixel icon and four digits do not fit there. A label
    // reading `Cobblestone (x4000)` is legible; a smear of pixels is not.
    inventory::write_named_item(p, id, slot.wire_count() as i32, &slot.label);
}

/// Read packets until one with `id` arrives, ignoring the rest.
///
/// The client interleaves its own configuration traffic (client settings,
/// plugin channels) with the handshake steps the server waits on, so skipping
/// is required rather than treating them as protocol errors.
fn wait_for(s: &mut Conn, id: i32) -> io::Result<()> {
    for _ in 0..64 {
        match read_packet(s)? {
            Some(p) if p.id == id => return Ok(()),
            Some(_) => continue,
            None => continue,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "client never sent the expected handshake packet",
    ))
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
    use aether_api::block_ids as b;

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
        let pkts = Codec.encode(&ServerEvent::KeepAlive(1), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        // frame len, id, then 8 bytes of payload
        assert_eq!(wire[0] as usize, wire.len() - 1);
        assert_eq!(wire[1] as i32, PLAY_KEEP_ALIVE);
        assert_eq!(wire.len(), 1 + 1 + 8);
    }

    #[test]
    fn block_change_carries_a_state_id_not_id_shifted_by_four() {
        let world = chunk::tests_support::Empty;
        let pkts = Codec.encode(
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
        let pkts = Codec.encode(
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
        assert_eq!(pin.var_int().unwrap(), PLAY_SPAWN_ENTITY);
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
        let pkts = Codec.encode(&ServerEvent::Chat("hi there".into()), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();

        assert_eq!(wire[1] as i32, PLAY_SYSTEM_CHAT);
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
            id: SB_CHAT,
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
        match Codec.decode(&pkt, prev) {
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
            id: SB_HELD_ITEM,
            data: 3u16.to_be_bytes().to_vec(),
        };
        match Codec.decode(&pkt, any_pos()) {
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
            id: SB_CREATIVE_SLOT,
            data,
        };
        match Codec.decode(&pkt, any_pos()) {
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
            id: SB_CREATIVE_SLOT,
            data,
        };
        match Codec.decode(&pkt, any_pos()) {
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
            assert_eq!(Codec.block_for_item(name), Some(block), "item {name}");
        }
        // Named, and every name resolves: a hotbar entry whose item this
        // version does not have would be dropped by the filter and the player
        // would silently join holding one thing fewer.
        assert_eq!(Codec.initial_hotbar().len(), inventory::HOTBAR.len());
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
        assert_eq!(PLAY_BLOCK_CHANGED_ACK + 4, PLAY_BLOCK_CHANGE);
        // A block source is required by the signature but never consulted:
        // the ack names no block.
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let pkts = Codec.encode(&ServerEvent::AckBlockChange(300), &Empty);
        assert_eq!(pkts.len(), 1);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        // length, id, then 300 as a VarInt: 0b10101100, 0b00000010.
        assert_eq!(wire, vec![3, PLAY_BLOCK_CHANGED_ACK as u8, 0xAC, 0x02]);
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

        let pkts = Codec.encode(
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
        assert_eq!(varint(body, &mut at), PLAY_WINDOW_ITEMS as i64, "packet id");
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
        let pkts = Codec.encode(
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
        assert_eq!(wire[1] as i32, PLAY_SPAWN_ENTITY);
        let mut wire = Vec::new();
        pkts[1].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[1] as i32, PLAY_ENTITY_METADATA);
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
                Codec.block_for_item(row.0),
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
            let got = Codec.block_for_item(item).expect(item);
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
        assert_eq!(Codec.block_for_item("minecraft:diamond_sword"), None);
        assert_eq!(Codec.block_for_item("minecraft:stick"), None);
    }
}
