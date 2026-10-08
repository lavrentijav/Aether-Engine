//! Codec for **1.21.4** (protocol 769).
//!
//! One release, one module. Every packet id and field layout below is fixed
//! for this version alone — no flags, no ranges, no `match` on a protocol
//! number. Neighbouring releases have their own copies, so a correction here
//! cannot reach them and a correction there cannot reach this.
//!
//! Ids come from PrismarineJS `minecraft-data` (`pc/<version>/protocol.json`)
//! and the registries from that release's own server dump.
//!
//! **Not verified against a live client** — no client of this version was
//! available, so everything here rests on encode/decode tests.

pub mod chunk;
pub mod registry;

use std::io;

use aether_world::BlockStateId;

use super::{BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

/// Login state ids.
/// Client Information (`settings`), play state: carries the view distance.
const SB_CLIENT_INFORMATION: i32 = 0x0C;
/// Client Information, configuration state.
const CFG_SB_CLIENT_INFORMATION: i32 = 0x00;
const LOGIN_SUCCESS: i32 = 0x02;
const LOGIN_ACKNOWLEDGED: i32 = 0x03;

/// Configuration: one packet per registry.
const CFG_REGISTRY_DATA: i32 = 0x07;
/// Configuration: server says it is done.
const CFG_FINISH: i32 = 0x03;
/// Configuration: the client's acknowledgement.
const CFG_FINISH_ACK: i32 = 0x03;
/// Configuration: offer a resource pack.
const CFG_ADD_RESOURCE_PACK: i32 = 0x09;
/// Play: the join packet.
const PLAY_LOGIN: i32 = 0x2C;
/// Play: chunk data and light in one packet.
const PLAY_CHUNK: i32 = 0x28;
/// Play: move the centre of the client's loaded-column window. Columns
/// arriving outside it are discarded on receipt.
const PLAY_SET_CENTER_CHUNK: i32 = 0x58;
/// Play: drop a column (carries Z before X).
const PLAY_UNLOAD_CHUNK: i32 = 0x22;
/// Play: one block changed.
const PLAY_BLOCK_CHANGE: i32 = 0x09;
/// Play: release the client's block prediction up to a sequence.
///
/// Four below Block Update: the clientbound ids are registered in
/// alphabetical order and `block_changed_ack`, `block_destruction`,
/// `block_entity_data`, `block_event` and `block_update` are
/// contiguous in every release that has them.
const PLAY_BLOCK_CHANGED_ACK: i32 = 0x05;
/// Play: tab list entry added.
const PLAY_PLAYER_INFO: i32 = 0x40;
/// Play: tab list entry removed.
const PLAY_PLAYER_REMOVE: i32 = 0x3F;
/// Play: spawn an entity.
const PLAY_SPAWN_ENTITY: i32 = 0x01;
/// Play: absolute entity move.
const PLAY_ENTITY_TELEPORT: i32 = 0x77;
/// Play: entity head yaw.
const PLAY_HEAD_ROTATION: i32 = 0x4D;
/// Play: remove entities.
const PLAY_DESTROY_ENTITIES: i32 = 0x47;
/// Play: keep-alive (a long, unlike 1.8's varint).
const PLAY_KEEP_ALIVE: i32 = 0x27;
/// Play: authoritative player position.
const PLAY_POSITION: i32 = 0x42;
/// Play: system chat message.
const PLAY_SYSTEM_CHAT: i32 = 0x73;
/// Play: game event (used here for “wait for chunks”).
const PLAY_GAME_EVENT: i32 = 0x23;
/// Serverbound: chat message.
/// "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x11;
const SB_CHAT: i32 = 0x07;
/// A slash command. Since 1.19 the client sends these on their own packet
/// instead of as a chat message, so a server that only decodes chat never
/// sees a single command — which is exactly what happened here.
const SB_CHAT_COMMAND: i32 = 0x05;
/// The same command, carrying the signatures of the arguments it quotes.
/// The command itself is still the first field, so both decode alike.
const SB_CHAT_COMMAND_SIGNED: i32 = 0x06;
/// Serverbound: position only.
const SB_POSITION: i32 = 0x1C;
/// Serverbound: position and look.
const SB_POSITION_LOOK: i32 = 0x1D;
/// Serverbound: look only.
const SB_LOOK: i32 = 0x1E;
/// Serverbound: neither, just the on-ground flag.
const SB_FLYING: i32 = 0x1F;
/// Serverbound: digging.
const SB_BLOCK_DIG: i32 = 0x27;
/// Serverbound: block placement.
const SB_BLOCK_PLACE: i32 = 0x3C;

/// Entity type id for `minecraft:player` — a registry index, which moves
/// whenever an entity type is added ahead of `player`.
const PLAYER_ENTITY_TYPE: i32 = 147;

/// Stable id for the pack this server offers.
const RESOURCE_PACK_ID: u128 = 0xae74_e701_0000_4000_8000_0000_0000_0002;

/// Engine block id -> this version's block-state id.
pub fn block_state(id: BlockStateId) -> u32 {
    use aether_api::block_ids as b;
    if id == b::AIR {
        0
    } else if id == b::STONE {
        1
    } else if id == b::GRASS_BLOCK {
        9
    } else if id == b::DIRT {
        10
    } else if id == b::BEDROCK {
        85
    } else if id == b::WATER {
        86
    } else if id == b::SAND {
        118
    } else if id == b::GRAVEL {
        124
    } else if id == b::OAK_LOG {
        137
    } else if id == b::OAK_LEAVES {
        279
    } else {
        1 // substitution: anything unexpressible becomes solid stone
    }
}

/// The codec for 1.21.4.
pub struct Codec;

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.21.4"
    }

    fn protocol_id(&self) -> i32 {
        769
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
        // Login Success: UUID as 16 raw bytes, username, empty property
        // list. The trailing `strictErrorHandling` flag was dropped in
        // 1.21.2, so nothing follows the list here.
        let mut success = PacketOut::new(LOGIN_SUCCESS);
        success.uuid(p.uuid).string(&p.name).var_int(0);
        success.send(s)?;

        wait_for(s, LOGIN_ACKNOWLEDGED)?;

        // Configuration: the registries the client builds its world from.
        for (id, entries) in registry::registries() {
            let mut pkt = PacketOut::new(CFG_REGISTRY_DATA);
            pkt.string(id).var_int(entries.len() as i32);
            for (key, value) in entries {
                pkt.string(&key).bool(true).bytes(&value.to_network());
            }
            pkt.send(s)?;
        }

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
        crate::proto::await_config(s, CFG_FINISH_ACK, CFG_SB_CLIENT_INFORMATION)?;

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
            .var_int(0); // portal cooldown
        login.var_int(63); // sea level, added in 1.21.2
        login.bool(false); // does not enforce secure chat
        login.send(s)?;

        // Tell the client to wait for chunks rather than render an empty void.
        PacketOut::new(PLAY_GAME_EVENT).u8(13).f32(0.0).send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Synchronize Player Position, 1.21.2 layout: teleport id first,
        // velocity deltas, and a four-byte relative-flags field.
        let mut pkt = PacketOut::new(PLAY_POSITION);
        pkt.var_int(1) // teleport id
            .f64(p.spawn.0)
            .f64(p.spawn.1)
            .f64(p.spawn.2)
            .f64(0.0)
            .f64(0.0)
            .f64(0.0)
            .f32(p.yaw)
            .f32(p.pitch)
            .i32(0); // all absolute
        pkt.send(s)
        // No hotbar: Set Slot carries a full item-component structure this
        // server has no inventory model for. Creative players take blocks from
        // the creative menu instead.
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
            ServerEvent::SpawnPlayer(h) => {
                let pos = h.pos();
                let mut p = PacketOut::new(PLAY_SPAWN_ENTITY);
                p.var_int(h.entity_id)
                    .uuid(h.uuid)
                    .var_int(PLAYER_ENTITY_TYPE)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .u8(angle(pos.pitch))
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.yaw)) // head yaw
                    .var_int(0) // no object data
                    .u16(0)
                    .u16(0)
                    .u16(0); // velocity
                vec![p]
            }
            ServerEvent::EntityMove(h) => {
                let pos = h.pos();
                let mut tp = PacketOut::new(PLAY_ENTITY_TELEPORT);
                tp.var_int(h.entity_id)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.pitch))
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
            // This version has no server-driven container support here; the
            // caller falls back to a chat listing. See
            // `ProtocolCodec::supports_containers`.
            // No item entity on this version yet; the caller keeps the stack
            // rather than dropping it, so nothing is lost.
            ServerEvent::MoveEntity { .. } => Vec::new(),
            ServerEvent::DropItem { .. } => Vec::new(),
            ServerEvent::OpenContainer { .. } => Vec::new(),
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
                // System Chat takes an NBT text component; a bare string tag is
                // a valid component.
                let mut p = PacketOut::new(PLAY_SYSTEM_CHAT);
                p.bytes(&super::nbt::string(text).to_network()).bool(false);
                vec![p]
            }
            // Gameplay events this version does not render.
            _ => Vec::new(),
        }
    }

    /// Parse a received packet.
    ///
    /// Two gaps, both from this server having no inventory model: placement
    /// always places stone (the packet names only the hand, not the held
    /// item), and chat signatures are not verified.
    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
        if pkt.id == SB_CLIENT_INFORMATION {
            return match crate::proto::view_distance_of(&pkt.data) {
                Some(d) => ClientEvent::ViewDistance(d),
                None => ClientEvent::Ignored,
            };
        }
        // Both block packets end with the client's prediction sequence, which
        // has to travel back in an ack or the client never applies the
        // server's view of the block. It is read from the end because the
        // fields in front of it differ between releases.
        let seq = super::trailing_var_int(&pkt.data);
        match pkt.id {
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
            SB_POSITION_LOOK => match (
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
            },
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
                if status != 0 && status != 2 {
                    return ClientEvent::Ignored;
                }
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let (x, y, z) = decode_position(packed);
                ClientEvent::Dig { x, y, z, seq }
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
                let (bx, by, bz) = decode_position(packed);
                let (dx, dy, dz) = super::v47::face_offset(face as u8);
                ClientEvent::Place {
                    x: bx + dx,
                    y: by + dy,
                    z: bz + dz,
                    block: aether_api::block_ids::STONE,
                    seq,
                    face: face as u8,
                    // No cursor on this codec's wire yet; the centre of
                    // the face is what a client clicking normally sends.
                    cursor: (0.5, 0.5, 0.5),
                }
            }
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

/// Read packets until one with `id` arrives, ignoring the rest.
///
/// The client interleaves its own configuration traffic (settings, plugin
/// channels) with the handshake steps the server waits on.
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

/// Degrees -> the protocol's angle byte (256ths of a full turn).
fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u8
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

    #[test]
    fn answers_only_for_its_own_protocol() {
        assert_eq!(Codec.protocol_id(), 769);
        assert!(Codec.supports(769));
        assert!(!Codec.supports(768));
        assert!(!Codec.supports(770));
    }

    #[test]
    fn every_engine_block_maps_to_a_distinct_state() {
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
        let mut seen: Vec<u32> = all.iter().map(|x| block_state(*x)).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "states must be distinct");
        // Anything the engine cannot express becomes stone rather than air.
        assert_eq!(block_state(BlockStateId(9999)), 1);
    }

    #[test]
    fn block_states_are_this_versions_numbering() {
        assert_eq!(block_state(b::BEDROCK), 85);
        assert_eq!(block_state(b::OAK_LEAVES), 279);
    }

    #[test]
    fn position_layout_round_trips() {
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
    }

    #[test]
    fn keep_alive_is_a_long_under_this_versions_id() {
        struct Empty;
        impl BlockSource for Empty {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
                BlockStateId::AIR
            }
        }
        let pkts = Codec.encode(&ServerEvent::KeepAlive(1), &Empty);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[1] as i32, PLAY_KEEP_ALIVE);
        assert_eq!(wire.len(), 1 + 1 + 8, "keep-alive payload is a long");
    }

    #[test]
    fn movement_decodes_under_this_versions_id() {
        let mut body = Vec::new();
        body.extend_from_slice(&1.5f64.to_be_bytes());
        body.extend_from_slice(&64.0f64.to_be_bytes());
        body.extend_from_slice(&2.5f64.to_be_bytes());
        body.push(1); // on ground
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        let pkt = RawPacket {
            id: SB_POSITION,
            data: body,
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
}
