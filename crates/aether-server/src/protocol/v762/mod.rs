//! Minecraft 1.19.4 (protocol 762) codec.
//!
//! Self-contained by design: every packet id and field list below is written
//! out for this release alone, with no branching on version. Neighbouring
//! releases live in their own modules and a fix here cannot reach them —
//! which also means a fix here does not *propagate*, so a bug found in this
//! file is worth checking for in the siblings.
//!
//! Ids and field lists come from PrismarineJS `minecraft-data`
//! (`pc/1.19.4/protocol.json`), block states from its `blocks.json`.
//!
//! Structurally this generation has **no configuration phase** — that arrives
//! in 1.20.2. Everything 1.21 sends as Registry Data packets travels inside
//! the Play Login packet here, as one named-root NBT blob (see [`registry`]).
//!
//! **Not verified against a live client.** No client of this version was
//! available, so everything rests on encode/decode tests alone.

pub mod chunk;
pub mod registry;

use std::io;

use aether_world::BlockStateId;

use super::{json_escape, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

/// The protocol id this codec speaks.
pub const PROTOCOL: i32 = 762;

// Clientbound play packet ids.
/// Client Information (`settings`), play state: carries the view distance.
const SB_CLIENT_INFORMATION: i32 = 0x08;
const IDS_LOGIN: i32 = 0x28;
const IDS_MAP_CHUNK: i32 = 0x24;
/// Play: move the centre of the client's loaded-column window. Columns
/// arriving outside it are discarded on receipt.
const IDS_SET_CENTER_CHUNK: i32 = 0x4e;
const IDS_UNLOAD_CHUNK: i32 = 0x1e;
const IDS_BLOCK_CHANGE: i32 = 0x0a;
/// Play: release the client's block prediction up to a sequence.
///
/// Four below Block Update: the clientbound ids are registered in
/// alphabetical order and `block_changed_ack`, `block_destruction`,
/// `block_entity_data`, `block_event` and `block_update` are
/// contiguous in every release that has them.
const IDS_BLOCK_CHANGED_ACK: i32 = 0x06;
const IDS_PLAYER_INFO: i32 = 0x3a;
const IDS_PLAYER_REMOVE: i32 = 0x39;
const IDS_SPAWN_PLAYER: i32 = 0x03;
const IDS_ENTITY_TELEPORT: i32 = 0x68;
const IDS_HEAD_ROTATION: i32 = 0x42;
const IDS_DESTROY_ENTITIES: i32 = 0x3e;
const IDS_KEEP_ALIVE: i32 = 0x23;
const IDS_POSITION: i32 = 0x3c;
const IDS_SYSTEM_CHAT: i32 = 0x64;
const IDS_GAME_STATE: i32 = 0x1f;
const IDS_RESOURCE_PACK: i32 = 0x40;

// Serverbound play packet ids.
/// "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x10;
const SB_CHAT: i32 = 0x05;
/// A slash command. Since 1.19 the client sends these on their own packet
/// instead of as a chat message, so a server that only decodes chat never
/// sees a single command — which is exactly what happened here.
const SB_CHAT_COMMAND: i32 = 0x04;
const SB_POSITION: i32 = 0x14;
const SB_POSITION_LOOK: i32 = 0x15;
const SB_LOOK: i32 = 0x16;
const SB_FLYING: i32 = 0x17;
const SB_DIG: i32 = 0x1d;
const SB_PLACE: i32 = 0x31;

/// Engine block id -> this version's block-state id.
///
/// This half of the translation table is what makes one engine world render
/// on many clients. Anything the version cannot express falls back to stone,
/// so a client is never handed an id outside its registry.
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
        79
    } else if id == b::WATER {
        80
    } else if id == b::SAND {
        112
    } else if id == b::GRAVEL {
        118
    } else if id == b::OAK_LOG {
        127
    } else if id == b::OAK_LEAVES {
        260
    } else {
        1 // substitution: anything unexpressible becomes solid stone
    }
}

/// The 1.19.4 codec.
pub struct Codec;

impl Codec {}

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.19.4"
    }

    fn protocol_id(&self) -> i32 {
        PROTOCOL
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        // Login Start: the username comes first. Whatever follows it in this
        // release (a chat signature, an optional player UUID) is left unread —
        // this server derives an offline UUID from the name.
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
        PacketOut::new(0x02)
            .uuid(p.uuid)
            .string(&p.name)
            .var_int(0)
            .send(s)?;

        // No configuration phase in this generation: the client is in play the
        // moment Login Success lands, so the registries ride inside Login.
        let mut login = PacketOut::new(IDS_LOGIN);
        login
            .i32(p.entity_id)
            .bool(false) // not hardcore
            .u8(p.game_mode.wire()) // survival 0 / creative 1
            .u8(0xFF) // previous game mode: none (-1)
            .var_int(1) // one world
            .string(registry::DIMENSION_NAME)
            .bytes(&registry::named_root(&registry::codec()))
            .string(registry::DIMENSION_NAME) // dimension type
            .string(registry::DIMENSION_NAME) // dimension (world) name
            .i64(0) // hashed seed
            .var_int(p.max_players as i32)
            .var_int(p.view_radius)
            .var_int(p.view_radius) // simulation distance
            .bool(false) // reduced debug info
            .bool(true) // enable respawn screen
            .bool(false) // not a debug world
            .bool(false) // not superflat
            .bool(false); // no death location
        login.send(s)?;

        if p.resource_pack.enabled() {
            PacketOut::new(IDS_RESOURCE_PACK)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .bool(p.resource_pack.required)
                .bool(false) // no custom prompt message
                .send(s)?;
        }

        // Tell the client to wait for chunks rather than render empty void
        // while the first batch is in flight.
        PacketOut::new(IDS_GAME_STATE).u8(13).f32(0.0).send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        let mut pos = PacketOut::new(IDS_POSITION);
        pos.f64(p.spawn.0)
            .f64(p.spawn.1)
            .f64(p.spawn.2)
            .f32(p.yaw)
            .f32(p.pitch)
            .u8(0) // flags: every value absolute
            .var_int(1); // teleport id
        pos.send(s)
        // No hotbar: Set Slot in this generation carries a full item stack
        // this server has no inventory model for. Creative players take
        // blocks from the creative menu instead.
    }

    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut> {
        match ev {
            ServerEvent::KeepAlive(id) => {
                let mut p = PacketOut::new(IDS_KEEP_ALIVE);
                p.i64(*id); // a long here, a varint in 1.8
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                vec![chunk::chunk_data_packet(IDS_MAP_CHUNK, *cx, *cz, world)]
            }
            ServerEvent::UnloadColumn { cx, cz } => {
                vec![chunk::unload_chunk_packet(IDS_UNLOAD_CHUNK, *cx, *cz)]
            }
            ServerEvent::TabListAdd(h) => {
                let mut p = PacketOut::new(IDS_PLAYER_INFO);
                // 1.19.3 replaced the action enum with a bitfield; the
                // per-entry payload follows flag-bit order.
                p.u8(0x01 | 0x08) // add_player | update_listed
                    .var_int(1)
                    .uuid(h.uuid)
                    .string(&h.name)
                    .var_int(0) // no profile properties
                    .bool(true); // listed
                vec![p]
            }
            ServerEvent::TabListRemove(uuid) => {
                let mut p = PacketOut::new(IDS_PLAYER_REMOVE);
                // Removal is its own packet from 1.19.3 on, carrying just
                // an array of UUIDs.
                p.var_int(1).uuid(*uuid);
                vec![p]
            }
            ServerEvent::SpawnPlayer(h) => {
                // Players use Spawn Player here, not the generic Spawn Entity
                // that 1.20.2 folded them into.
                let pos = h.pos();
                let mut p = PacketOut::new(IDS_SPAWN_PLAYER);
                p.var_int(h.entity_id)
                    .uuid(h.uuid)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.pitch));
                vec![p]
            }
            ServerEvent::EntityMove(h) => {
                let pos = h.pos();
                let mut tp = PacketOut::new(IDS_ENTITY_TELEPORT);
                tp.var_int(h.entity_id)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.pitch))
                    .bool(pos.on_ground);
                let mut head = PacketOut::new(IDS_HEAD_ROTATION);
                head.var_int(h.entity_id).u8(angle(pos.yaw));
                vec![tp, head]
            }
            ServerEvent::DespawnEntity(eid) => {
                let mut p = PacketOut::new(IDS_DESTROY_ENTITIES);
                p.var_int(1).var_int(*eid);
                vec![p]
            }
            ServerEvent::BlockChange { x, y, z, block } => {
                let mut p = PacketOut::new(IDS_BLOCK_CHANGE);
                p.i64(encode_position(*x as i64, *y as i64, *z as i64))
                    .var_int(block_state(*block) as i32);
                vec![p]
            }
            ServerEvent::AckBlockChange(seq) => {
                let mut p = PacketOut::new(IDS_BLOCK_CHANGED_ACK);
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
                let mut p = PacketOut::new(IDS_SET_CENTER_CHUNK);
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
                // System Chat takes a JSON string here; the NBT text component
                // only arrives with 1.20.3.
                let mut p = PacketOut::new(IDS_SYSTEM_CHAT);
                p.string(&format!("{{\"text\":\"{}\"}}", json_escape(text)));
                // A single trailing zero byte: a message-type varint in 1.19,
                // an action-bar bool from 1.19.1 on. Different fields, same
                // encoding for an ordinary chat line.
                p.u8(0);
                vec![p]
            }
            // Gameplay events this version does not render.
            _ => Vec::new(),
        }
    }

    /// Parse a received packet.
    ///
    /// Two deliberate gaps, both from this server having no inventory model:
    /// placement always places stone (the packet names only the hand), and
    /// incoming chat signatures are ignored rather than verified — this server
    /// never echoes player chat as signed chat, so nothing depends on them.
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
        // Movement carries a plain `onGround` bool in this generation; the
        // packed flags byte only arrives much later.
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
        if pkt.id == SB_DIG {
            let Ok(status) = pin.var_int() else {
                return ClientEvent::Ignored;
            };
            // 0 starts digging (the whole interaction in creative), 2 finishes.
            if status != 0 && status != 2 {
                return ClientEvent::Ignored;
            }
            let Ok(packed) = pin.i64() else {
                return ClientEvent::Ignored;
            };
            let (x, y, z) = decode_position(packed);
            return ClientEvent::Dig { x, y, z, seq };
        }
        if pkt.id == SB_PLACE {
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
            return ClientEvent::Place {
                x: bx + dx,
                y: by + dy,
                z: bz + dz,
                block: aether_api::block_ids::STONE,
                seq,
                face: face as u8,
                // No cursor on this codec's wire yet; the centre of
                // the face is what a client clicking normally sends.
                cursor: (0.5, 0.5, 0.5),
            };
        }
        if pkt.id == SB_CHAT_COMMAND {
            // The command arrives without its slash; the dispatcher
            // keys on one, so it is put back.
            return match pin.string() {
                Ok(text) => ClientEvent::Chat(format!("/{text}")),
                Err(_) => ClientEvent::Ignored,
            };
        }
        if pkt.id == SB_CHAT {
            // The message is the first field; the signing fields that follow
            // are read past.
            return match pin.string() {
                Ok(text) => ClientEvent::Chat(text),
                Err(_) => ClientEvent::Ignored,
            };
        }
        ClientEvent::Ignored
    }
}

/// Degrees -> the protocol's angle byte (256ths of a full turn).
fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u8
}

/// Encode a block position into the modern packed `i64`.
///
/// 1.14 reordered the fields relative to 1.8: `X (26) | Z (26) | Y (12)`, with
/// Y in the **low** bits rather than the middle.
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
        // Each release is registered separately, so this codec must not answer
        // for a neighbour it would then encode wrongly.
        assert_eq!(Codec.protocol_id(), PROTOCOL);
        assert!(Codec.supports(PROTOCOL));
        assert!(!Codec.supports(PROTOCOL + 1));
        assert!(!Codec.supports(PROTOCOL - 1));
        assert!(!Codec.supports(47), "must not claim 1.8");
    }

    #[test]
    fn block_states_match_this_release() {
        // Leaves moved with almost every release in this span; a wrong id
        // renders the wrong material rather than failing loudly.
        assert_eq!(block_state(b::OAK_LEAVES), 260);
        assert_eq!(block_state(b::BEDROCK), 79);
        assert_eq!(block_state(b::WATER), 80);
        // Blocks that never moved across the generation.
        assert_eq!(block_state(b::AIR), 0);
        assert_eq!(block_state(b::STONE), 1);
        assert_eq!(block_state(b::GRASS_BLOCK), 9);
        assert_eq!(block_state(b::DIRT), 10);
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
        let mut seen: Vec<u32> = all.iter().map(|b| block_state(*b)).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "states must be distinct");
    }

    #[test]
    fn unknown_blocks_fall_back_to_stone() {
        assert_eq!(block_state(BlockStateId(9999)), 1);
    }

    #[test]
    fn position_layout_is_the_modern_one() {
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
        // And it is not 1.8's X|Y|Z ordering.
        assert_ne!(encode_position(1, 2, 3), ((1i64) << 38) | (2 << 26) | 3);
    }

    #[test]
    fn keep_alive_is_a_long_not_a_varint() {
        // 1.8 sends a varint here; getting it wrong desynchronises the stream
        // on the very first keep-alive.
        let (id, body) = encode_one(&ServerEvent::KeepAlive(1));
        assert_eq!(id, IDS_KEEP_ALIVE);
        assert_eq!(body.len(), 8, "an i64 payload");
    }

    #[test]
    fn block_change_carries_a_state_id() {
        let (id, body) = encode_one(&ServerEvent::BlockChange {
            x: 1,
            y: 2,
            z: 3,
            block: b::OAK_LEAVES,
        });
        assert_eq!(id, IDS_BLOCK_CHANGE);
        assert_eq!(
            i64::from_be_bytes(body[0..8].try_into().unwrap()),
            encode_position(1, 2, 3)
        );
        let mut state = 0i32;
        for (i, &byte) in body[8..].iter().take(5).enumerate() {
            state |= ((byte & 0x7f) as i32) << (7 * i);
            if byte & 0x80 == 0 {
                break;
            }
        }
        assert_eq!(state, 260, "the leaves state id for this release");
    }

    #[test]
    fn tab_list_removal_uses_this_releases_shape() {
        assert_ne!(
            IDS_PLAYER_REMOVE, IDS_PLAYER_INFO,
            "from 1.19.3 removal is a packet of its own"
        );
        let uuid = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdefu128;
        let (id, body) = encode_one(&ServerEvent::TabListRemove(uuid));
        assert_eq!(id, IDS_PLAYER_REMOVE);
        // One UUID count byte followed by the raw UUID.
        assert_eq!(body[0], 1, "one uuid follows");
        assert_eq!(body.len(), 1 + 16);
    }

    #[test]
    fn chat_is_json_not_nbt_in_this_generation() {
        // 1.20.3 switched System Chat to an NBT component; here it is still a
        // length-prefixed JSON string.
        let (id, body) = encode_one(&ServerEvent::Chat("hi".into()));
        assert_eq!(id, IDS_SYSTEM_CHAT);
        let len = body[0] as usize;
        let text = std::str::from_utf8(&body[1..1 + len]).unwrap();
        assert_eq!(text, r#"{"text":"hi"}"#);
        assert_eq!(body.len(), 1 + len + 1, "one trailing flag byte");
    }

    #[test]
    fn movement_decodes_with_a_plain_on_ground_bool() {
        // This generation has no packed movement flags byte; reading one would
        // shift every field after it.
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
        data.extend_from_slice(&(-2.5f64).to_be_bytes());
        data.push(1); // onGround
        let pkt = RawPacket {
            id: SB_POSITION,
            data,
        };
        match Codec.decode(&pkt, prev) {
            ClientEvent::Move(p) => {
                assert_eq!((p.x, p.y, p.z), (1.5, 64.0, -2.5));
                assert!(p.on_ground);
            }
            _ => panic!("expected a movement event"),
        }
    }

    #[test]
    fn digging_decodes_to_the_clicked_block() {
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        let mut data = vec![0]; // status 0: started digging
        data.extend_from_slice(&encode_position(5, 70, -9).to_be_bytes());
        data.push(1); // face
        let pkt = RawPacket { id: SB_DIG, data };
        match Codec.decode(&pkt, prev) {
            ClientEvent::Dig { x, y, z, .. } => assert_eq!((x, y, z), (5, 70, -9)),
            _ => panic!("expected a dig event"),
        }
    }

    #[test]
    fn the_ack_carries_the_sequence_and_sits_four_below_block_update() {
        // Two independent claims, because getting either wrong is silent: the
        // client would either drop the packet as malformed or read an ack as
        // some other block packet.
        assert_eq!(IDS_BLOCK_CHANGED_ACK + 4, IDS_BLOCK_CHANGE);
        let (id, body) = encode_one(&ServerEvent::AckBlockChange(300));
        assert_eq!(id, IDS_BLOCK_CHANGED_ACK);
        // 300 as a VarInt: 0b10101100, 0b00000010.
        assert_eq!(body, vec![0xAC, 0x02]);
    }
}
