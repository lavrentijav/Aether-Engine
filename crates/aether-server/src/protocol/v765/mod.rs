//! Minecraft 1.20.3/1.20.4 (protocol 765) codec.
//!
//! Self-contained by design: every packet id and field list below is written
//! out for this release alone, with no branching on version. Neighbouring
//! releases live in their own modules and a fix here cannot reach them —
//! which also means a fix here does not *propagate*, so a bug found in this
//! file is worth checking for in the siblings.
//!
//! Ids and field lists come from PrismarineJS `minecraft-data`
//! (`pc/1.20.3/protocol.json`), block states from 1.20.4's `blocks.json`
//! (1.20.3/1.20.4 share protocol 765).
//!
//! 1.20.3 changed several things over 1.20.2, all pinned below: System Chat
//! moved from a JSON string to an NBT component, the resource-pack
//! configuration packet grew a UUID, the added `level_chunks_load_start` game
//! event (13) finally lets the server say "wait for chunks", and the entity
//! type id for `minecraft:player` moved.
//!
//! **Not verified against a live client.** No client of this version was
//! available, so everything rests on encode/decode tests alone.

pub mod chunk;
pub mod registry;

use std::io;

use aether_world::{BlockStateId, ItemTypeId};

/// The shared [`super::modern`] version for this protocol: its packet ids,
/// entity types, menus and compat-backed block and item tables drive the
/// gameplay path this codec delegates to.
fn version() -> &'static super::modern::version::Version {
    &super::modern::version::V1_20_4
}

use super::modern;
use super::{nbt, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

/// The protocol id this codec speaks.
pub const PROTOCOL: i32 = 765;

/// Login state ids.
const LOGIN_SUCCESS: i32 = 0x02;
const LOGIN_ACKNOWLEDGED: i32 = 0x03;

/// Configuration: the whole registry codec as one anonymous-NBT blob.
const CFG_REGISTRY_DATA: i32 = 0x05;
/// Configuration: server says it is done.
const CFG_FINISH: i32 = 0x02;
/// Configuration: the client's acknowledgement.
const CFG_FINISH_ACK: i32 = 0x02;
/// Configuration: offer a resource pack (renamed from `resource_pack_send`
/// and now carrying a UUID).
const CFG_ADD_RESOURCE_PACK: i32 = 0x07;

/// Stable id for the pack this server offers.
const RESOURCE_PACK_ID: u128 = 0xae74_e701_0000_4000_8000_0000_0000_0000;

/// Play: the join packet.
const PLAY_LOGIN: i32 = 0x29;
/// Play: chunk data and light in one packet.
const PLAY_MAP_CHUNK: i32 = 0x25;
/// Play: open a batch of columns. Empty.
const PLAY_CHUNK_BATCH_START: i32 = 0x0d;
/// Play: close a batch, naming how many columns it carried.
const PLAY_CHUNK_BATCH_FINISHED: i32 = 0x0c;
/// Play: move the centre of the client's loaded-column window.
const PLAY_SET_CENTER_CHUNK: i32 = 0x52;
/// Play: drop a column (carries Z before X).
const PLAY_UNLOAD_CHUNK: i32 = 0x1f;
const PLAY_BLOCK_CHANGE: i32 = 0x09;
/// Play: release the client's block prediction up to a sequence.
///
/// Four below Block Update: the clientbound ids are registered in
/// alphabetical order and `block_changed_ack`, `block_destruction`,
/// `block_entity_data`, `block_event` and `block_update` are
/// contiguous in every release that has them.
const PLAY_BLOCK_CHANGED_ACK: i32 = 0x05;
const PLAY_PLAYER_INFO: i32 = 0x3c;
const PLAY_PLAYER_REMOVE: i32 = 0x3b;
const PLAY_DESTROY_ENTITIES: i32 = 0x40;
const PLAY_KEEP_ALIVE: i32 = 0x24;
const PLAY_POSITION: i32 = 0x3e;
const PLAY_SYSTEM_CHAT: i32 = 0x69;
/// Play: game event. Reason 13 ("level chunks load start") is new in 1.20.3.
const PLAY_GAME_EVENT: i32 = 0x20;
/// Play: "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x11;

/// Serverbound play packet ids.
/// A slash command. Since 1.19 the client sends these on their own packet
/// instead of as a chat message, so a server that only decodes chat never
/// sees a single command — which is exactly what happened here.
const SB_CHAT_COMMAND: i32 = 0x04;
const SB_CHAT: i32 = 0x05;
const SB_POSITION: i32 = 0x17;
const SB_POSITION_LOOK: i32 = 0x18;
const SB_LOOK: i32 = 0x19;
const SB_FLYING: i32 = 0x1a;
const SB_BLOCK_DIG: i32 = 0x21;
const SB_BLOCK_PLACE: i32 = 0x35;

/// Engine block id -> this version's block-state id.
///
/// This half of the translation table is what makes one engine world render
/// on many clients. Anything the version cannot express falls back to stone,
/// so a client is never handed an id outside its registry.
pub fn block_state(id: BlockStateId) -> u32 {
    crate::protocol::compat::V1_20_4.state(id)
}

/// The 1.20.3/1.20.4 codec.
pub struct Codec;

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.20.3/1.20.4"
    }

    fn protocol_id(&self) -> i32 {
        PROTOCOL
    }

    fn supports_containers(&self) -> bool {
        true
    }

    fn full_gameplay(&self) -> bool {
        true
    }

    fn block_for_item(&self, item: &str) -> Option<BlockStateId> {
        super::modern::block_for_item(item)
    }

    fn resolve_reported_item(&self, server: ItemTypeId, reported: ItemTypeId) -> ItemTypeId {
        // A spear shown as a trident (this release has no spears) comes back
        // as a trident; the slot's real item wins.
        version().resolve_reported(server, reported)
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        let Some(p) = read_packet(s)? else {
            return Ok(None);
        };
        if p.id != 0x00 {
            return Ok(None);
        }
        Ok(Some(PacketIn::new(&p.data).string()?))
    }

    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Login Success: UUID as 16 raw bytes, username, empty property list.
        PacketOut::new(LOGIN_SUCCESS)
            .uuid(p.uuid)
            .string(&p.name)
            .var_int(0)
            .send(s)?;

        wait_for(s, LOGIN_ACKNOWLEDGED)?;

        // Configuration: the registries, as one blob.
        PacketOut::new(CFG_REGISTRY_DATA)
            .bytes(&registry::blob())
            .send(s)?;

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

        let mut login = PacketOut::new(PLAY_LOGIN);
        login
            .i32(p.entity_id)
            .bool(false) // not hardcore
            .var_int(1) // one world
            .string(registry::DIMENSION_NAME) // worldNames
            .var_int(p.max_players as i32)
            .var_int(p.view_radius)
            .var_int(p.view_radius) // simulation distance
            .bool(false) // reduced debug info
            .bool(true) // enable respawn screen
            .bool(false) // limited crafting
            .string(registry::DIMENSION_NAME) // worldType: the dimension type
            .string(registry::DIMENSION_NAME) // world name
            .i64(0) // hashed seed
            .u8(p.game_mode.wire()) // survival 0 / creative 1
            .u8(0xFF) // previous game mode: none (-1)
            .bool(false) // not a debug world
            .bool(false) // not superflat
            .bool(false) // no death location
            .var_int(0); // portal cooldown
        login.send(s)?;

        // Tell the client to wait for chunks rather than render empty void
        // while the first batch is in flight. Reason 13 exists from 1.20.3.
        PacketOut::new(PLAY_GAME_EVENT).u8(13).f32(0.0).send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        let mut pos = PacketOut::new(PLAY_POSITION);
        pos.f64(p.spawn.0)
            .f64(p.spawn.1)
            .f64(p.spawn.2)
            .f32(p.yaw)
            .f32(p.pitch)
            .u8(0) // flags: every value absolute
            .var_int(1); // teleport id
        pos.send(s)?;
        // This codec runs the full game, so the client needs the same
        // bootstrap the 1.21.11 codec sends: abilities, the selected hotbar
        // slot, and full health and food.
        let v = version();
        let flags = match p.game_mode {
            super::GameMode::Creative => 0x01 | 0x04 | 0x08,
            super::GameMode::Survival => 0,
        };
        let mut abilities = PacketOut::new(v.packets.cb_player_abilities);
        abilities.u8(flags).f32(0.05).f32(0.10);
        abilities.send(s)?;
        PacketOut::new(v.packets.cb_set_held_slot)
            .var_int(0)
            .send(s)?;
        let mut health = PacketOut::new(v.packets.cb_set_health);
        health.f32(20.0).var_int(20).f32(5.0);
        health.send(s)
    }

    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut> {
        match ev {
            ServerEvent::KeepAlive(id) => {
                let mut p = PacketOut::new(PLAY_KEEP_ALIVE);
                p.i64(*id);
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                // A batch of one; see the sibling 1.20.2 module.
                let mut finished = PacketOut::new(PLAY_CHUNK_BATCH_FINISHED);
                finished.var_int(1);
                vec![
                    PacketOut::new(PLAY_CHUNK_BATCH_START),
                    chunk::chunk_data_packet(PLAY_MAP_CHUNK, *cx, *cz, world),
                    finished,
                ]
            }
            ServerEvent::UnloadColumn { cx, cz } => {
                vec![chunk::unload_chunk_packet(PLAY_UNLOAD_CHUNK, *cx, *cz)]
            }
            ServerEvent::TabListAdd(h) => {
                let mut p = PacketOut::new(PLAY_PLAYER_INFO);
                p.u8(0x01 | 0x08) // add_player | update_listed
                    .var_int(1)
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
            ServerEvent::SetCenterChunk { cx, cz } => {
                let mut p = PacketOut::new(PLAY_SET_CENTER_CHUNK);
                p.var_int(*cx).var_int(*cz);
                vec![p]
            }
            ServerEvent::CommandTree => {
                let mut p = PacketOut::new(PLAY_COMMANDS);
                super::commands::write_tree(
                    &mut p,
                    super::commands::Parsers::ById(&super::commands::PARSERS_1_20_3),
                );
                vec![p]
            }
            ServerEvent::Chat(text) => {
                // 1.20.3 switched System Chat to an NBT text component; a bare
                // string tag is a valid component.
                let mut p = PacketOut::new(PLAY_SYSTEM_CHAT);
                p.bytes(&nbt::string(text).to_network()).bool(false);
                vec![p]
            }
            // Everything else — entities, combat, health, hunger, windows,
            // effects — is the shared gameplay path, driven by this version's
            // ids and its pre-1.20.5 layout gates.
            other => modern::Codec(version()).encode(other, world),
        }
    }

    /// Parse a received packet.
    ///
    /// Two gaps, both from this server having no inventory model: placement
    /// always places stone (the packet names only the hand, not the held
    /// item), and chat signatures are not verified.
    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
        let seq = super::trailing_var_int(&pkt.data);
        if pkt.id == SB_POSITION {
            return match (pin.f64(), pin.f64(), pin.f64(), pin.bool()) {
                (Ok(x), Ok(y), Ok(z), Ok(on_ground)) => ClientEvent::Move(PosLook {
                    x,
                    y,
                    z,
                    on_ground,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_POSITION_LOOK {
            return match (
                pin.f64(),
                pin.f64(),
                pin.f64(),
                pin.f32(),
                pin.f32(),
                pin.bool(),
            ) {
                (Ok(x), Ok(y), Ok(z), Ok(yaw), Ok(pitch), Ok(on_ground)) => {
                    ClientEvent::Move(PosLook {
                        x,
                        y,
                        z,
                        yaw,
                        pitch,
                        on_ground,
                    })
                }
                _ => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_LOOK {
            return match (pin.f32(), pin.f32(), pin.bool()) {
                (Ok(yaw), Ok(pitch), Ok(on_ground)) => ClientEvent::Move(PosLook {
                    yaw,
                    pitch,
                    on_ground,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_FLYING {
            return match pin.bool() {
                Ok(on_ground) => ClientEvent::Move(PosLook { on_ground, ..prev }),
                Err(_) => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_BLOCK_DIG {
            let Ok(status) = pin.var_int() else {
                return ClientEvent::Ignored;
            };
            // The same packet carries digging and the non-dig actions: status
            // 0 starts, 1 cancels, 2 finishes; 5 releases a use (a spear's
            // charge is read from it) and 6 swaps hands.
            match status {
                0 => {
                    let Ok(packed) = pin.i64() else {
                        return ClientEvent::Ignored;
                    };
                    let (x, y, z) = decode_position(packed);
                    return ClientEvent::StartDig { x, y, z, seq };
                }
                1 => return ClientEvent::CancelDig { seq },
                2 => {
                    let Ok(packed) = pin.i64() else {
                        return ClientEvent::Ignored;
                    };
                    let (x, y, z) = decode_position(packed);
                    return ClientEvent::Dig { x, y, z, seq };
                }
                3 => return ClientEvent::DropHeld { all: true },
                4 => return ClientEvent::DropHeld { all: false },
                5 => return ClientEvent::ReleaseUse,
                6 => return ClientEvent::SwapHands,
                _ => return ClientEvent::Ignored,
            }
        }
        if pkt.id == SB_BLOCK_PLACE {
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
            let Ok(cursor) =
                (|| -> io::Result<(f32, f32, f32)> { Ok((pin.f32()?, pin.f32()?, pin.f32()?)) })()
            else {
                return ClientEvent::Ignored;
            };
            let (bx, by, bz) = decode_position(packed);
            let (dx, dy, dz) = super::v47::face_offset(face as u8);
            return ClientEvent::Place {
                x: bx + dx,
                y: by + dy,
                z: bz + dz,
                // This version's placement packet names only the hand, so the
                // block placed is whatever the selected hotbar slot holds —
                // which `decode` cannot see. Fall back to the slot every
                // player is given selected on join, the same fallback the
                // shared path uses.
                block: super::modern::inventory::block_in_hotbar_slot(0)
                    .unwrap_or(aether_api::block_ids::STONE),
                seq,
                face: face as u8,
                cursor,
            };
        }
        if pkt.id == SB_CHAT_COMMAND {
            return match pin.string() {
                Ok(text) => ClientEvent::Chat(format!("/{text}")),
                Err(_) => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_CHAT {
            return match pin.string() {
                Ok(text) => ClientEvent::Chat(text),
                Err(_) => ClientEvent::Ignored,
            };
        }
        // Window clicks, creative slots, held slot, sprint, sneak, swing,
        // entity interaction and use item: the shared gameplay decoder.
        modern::Codec(version()).decode(pkt, prev)
    }
}

/// Read packets until one with `id` arrives, ignoring the rest.
fn wait_for(s: &mut Conn, id: i32) -> io::Result<()> {
    for _ in 0..64 {
        match read_packet(s)? {
            Some(p) if p.id == id => return Ok(()),
            _ => continue,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "client never sent the expected handshake packet",
    ))
}

/// Encode a block position: `X (26) | Z (26) | Y (12)`, Y in the low bits.
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

    /// Strip the frame length varint and packet id, returning `(id, body)`.
    fn split_packet(wire: &[u8]) -> (i32, &[u8]) {
        let mut pos = 0usize;
        let read_varint = |w: &[u8], pos: &mut usize| -> i32 {
            let mut v = 0i32;
            for i in 0..5 {
                let byte = w[*pos];
                *pos += 1;
                v |= ((byte & 0x7f) as i32) << (7 * i);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            v
        };
        let _len = read_varint(wire, &mut pos);
        let id = read_varint(wire, &mut pos);
        (id, &wire[pos..])
    }

    fn encode_one(ev: &ServerEvent) -> (i32, Vec<u8>) {
        let world = chunk::tests_support::Empty;
        let pkts = Codec.encode(ev, &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        let (id, body) = split_packet(&wire);
        (id, body.to_vec())
    }

    #[test]
    fn claims_only_its_own_protocol_id() {
        assert_eq!(Codec.protocol_id(), PROTOCOL);
        assert!(Codec.supports(PROTOCOL));
        assert!(!Codec.supports(PROTOCOL + 1));
        assert!(!Codec.supports(PROTOCOL - 1));
        assert!(!Codec.supports(47), "must not claim 1.8");
    }

    #[test]
    fn block_states_match_this_release() {
        assert_eq!(block_state(b::OAK_LEAVES), 264);
        assert_eq!(block_state(b::BEDROCK), 79);
        assert_eq!(block_state(b::WATER), 80);
        assert_eq!(block_state(b::AIR), 0);
        assert_eq!(block_state(b::STONE), 1);
        // 9,999 is a real engine state now, so the substitution boundary is
        // the end of the state space.
        let modded = BlockStateId(aether_world::registry::blocks::STATE_COUNT as u32 + 7);
        assert_eq!(block_state(modded), 1, "unknown -> stone");
    }

    #[test]
    fn position_layout_round_trips() {
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
    }

    #[test]
    fn chat_is_an_nbt_component_from_1_20_3() {
        // The change that separates this codec from 1.20.2: a JSON string here
        // shifts every following field.
        let (id, body) = encode_one(&ServerEvent::Chat("hi".into()));
        assert_eq!(id, PLAY_SYSTEM_CHAT);
        assert_eq!(body[0], 8, "TAG_String root");
        let len = u16::from_be_bytes([body[1], body[2]]) as usize;
        assert_eq!(&body[3..3 + len], b"hi");
        assert_eq!(body.len(), 3 + len + 1, "then the action-bar flag");
    }

    #[test]
    fn a_column_is_wrapped_in_a_chunk_batch() {
        let world = chunk::tests_support::Empty;
        let pkts = Codec.encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &world);
        assert_eq!(pkts.len(), 3);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        assert_eq!(split_packet(&wire).0, PLAY_CHUNK_BATCH_START);
        wire.clear();
        pkts[1].write_to(&mut wire, None).unwrap();
        assert_eq!(split_packet(&wire).0, PLAY_MAP_CHUNK);
        wire.clear();
        pkts[2].write_to(&mut wire, None).unwrap();
        assert_eq!(split_packet(&wire).0, PLAY_CHUNK_BATCH_FINISHED);
    }

    #[test]
    fn movement_decodes_under_this_versions_id() {
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        let mut data = Vec::new();
        data.extend_from_slice(&1.5f64.to_be_bytes());
        data.extend_from_slice(&64.0f64.to_be_bytes());
        data.extend_from_slice(&2.5f64.to_be_bytes());
        data.push(1);
        let pkt = RawPacket {
            id: SB_POSITION,
            data,
        };
        match Codec.decode(&pkt, prev) {
            ClientEvent::Move(p) => {
                assert_eq!((p.x, p.y, p.z), (1.5, 64.0, 2.5));
                assert!(p.on_ground);
            }
            _ => panic!("must decode its own position id"),
        }
    }

    #[test]
    fn the_ack_carries_the_sequence_and_sits_four_below_block_update() {
        assert_eq!(PLAY_BLOCK_CHANGED_ACK + 4, PLAY_BLOCK_CHANGE);
        let (id, body) = encode_one(&ServerEvent::AckBlockChange(300));
        assert_eq!(id, PLAY_BLOCK_CHANGED_ACK);
        assert_eq!(body, vec![0xAC, 0x02]);
    }

    #[test]
    fn placement_falls_back_to_the_shared_hotbar_block() {
        // The placement packet names only the hand, so the codec cannot see
        // what is held; it must use the same join-hotbar fallback the shared
        // path does rather than a hard-coded stone.
        let mut data = Vec::new();
        data.push(0); // hand
        data.extend_from_slice(&encode_position(5, 70, -9).to_be_bytes());
        data.push(1); // direction: up
        data.extend_from_slice(&0.25f32.to_be_bytes());
        data.extend_from_slice(&0.75f32.to_be_bytes());
        data.extend_from_slice(&0.5f32.to_be_bytes());
        data.push(0); // insideBlock
        data.push(0); // sequence
        let pkt = RawPacket {
            id: SB_BLOCK_PLACE,
            data,
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::Place { block, .. } => assert_eq!(
                block,
                crate::protocol::modern::inventory::block_in_hotbar_slot(0).unwrap()
            ),
            _ => panic!("expected a place event"),
        }
    }

    #[test]
    fn the_codec_runs_the_full_game_and_exposes_windows() {
        assert!(Codec.full_gameplay());
        assert!(Codec.supports_containers());
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
    fn gameplay_packets_the_old_subset_never_read_decode_through_the_shared_path() {
        // These were not in this codec's hand-written subset; the fallback to
        // the shared gameplay decoder is what makes them work.
        let p = version().packets;

        // Held slot: a short here, the same bytes a byte would be for 0..9.
        let pkt = RawPacket {
            id: p.sb_set_carried_item,
            data: 4u16.to_be_bytes().to_vec(),
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::HeldSlot(4) => {}
            _ => panic!("held slot must decode"),
        }

        // A 1.20.4 creative slot is the old NBT `Slot`: presence, id, `i8`
        // count, NBT. The shared reader decodes that pre-1.20.5 form.
        let mut data = 36u16.to_be_bytes().to_vec();
        data.extend_from_slice(&[1, 1, 7, 0]); // present, id=stone, count=7, TAG_End
        let pkt = RawPacket {
            id: p.sb_set_creative_mode_slot,
            data,
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::CreativeSlot { slot, item, count } => {
                assert_eq!(slot, 36);
                assert_eq!(item, Some(crate::inventory::item("minecraft:stone")));
                assert_eq!(count, 7);
            }
            _ => panic!("creative slot must decode"),
        }

        // A window click: window, state, slot, button, mode.
        let mut data = vec![1]; // window 1
        data.push(0); // state id
        data.extend_from_slice(&(-999i16).to_be_bytes()); // outside-click slot
        data.push(0); // left button
        data.push(0); // normal mode
        let pkt = RawPacket {
            id: p.sb_container_click,
            data,
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::WindowClick {
                window, slot, mode, ..
            } => {
                assert_eq!(window, 1);
                assert_eq!(slot, -999);
                assert_eq!(mode, 0);
            }
            _ => panic!("window click must decode"),
        }
    }

    #[test]
    fn the_trident_is_the_stand_in_the_item_layer_chooses_for_a_spear_on_765() {
        // The item layer shows a spear as a trident on this release, and the
        // codec resolves the echoed stand-in back to the real spear.
        let spear = crate::inventory::item("minecraft:iron_spear");
        let shown = crate::protocol::compat::items::V1_20_4
            .wire(spear)
            .expect("1.20.4 has a stand-in for a spear");
        assert!(shown.disguised, "the spear is shown as another item");
        assert_eq!(
            crate::protocol::compat::items::V1_20_4.engine(shown.id),
            Some(crate::inventory::item("minecraft:trident")),
        );
        assert_eq!(
            Codec.resolve_reported_item(spear, crate::inventory::item("minecraft:trident")),
            spear,
        );
    }

    #[test]
    fn a_use_then_release_routes_a_trident_stand_in_to_the_real_spear() {
        // A 1.20.4 client has no spears; the compat layer shows it a trident.
        // Its use and release must run the *real* spear's charge, not the
        // trident's throw.
        let p = version().packets;

        // Use Item: hand, sequence.
        let pkt = RawPacket {
            id: p.sb_use_item,
            data: vec![0, 0],
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::UseItem { hand, .. } => assert_eq!(hand, 0),
            _ => panic!("use item must decode"),
        }

        // Player Action status 5: release use, ending with the sequence.
        let mut body = vec![5];
        body.extend_from_slice(&0i64.to_be_bytes()); // location, ignored here
        body.push(0); // face
        body.push(0); // sequence
        let pkt = RawPacket {
            id: SB_BLOCK_DIG,
            data: body,
        };
        match Codec.decode(&pkt, any_pos()) {
            ClientEvent::ReleaseUse => {}
            _ => panic!("release use must decode"),
        }

        use crate::protocol::compat::behaviour::{self, Action, Input};
        let spear = crate::inventory::item("minecraft:iron_spear");
        assert!(behaviour::swallows_release(spear));
        assert_eq!(
            behaviour::route(spear, Input::UseRelease).action,
            Action::SpearCharge
        );
    }
}
