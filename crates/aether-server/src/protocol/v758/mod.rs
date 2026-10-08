//! Minecraft 1.18.2 (protocol 758) codec.
//!
//! Every packet id, packet shape and block-state id below comes from
//! PrismarineJS `minecraft-data`: `pc/1.18.2/protocol.json` and
//! `pc/1.18/blocks.json`, with the dimension codec taken from
//! `pc/1.18.2/loginPacket.json` — a capture of what a real server sends.
//!
//! One protocol, one module, no version branching: this codec answers for 758
//! and nothing else. Its 1.18/1.18.1 neighbour is a near-identical copy on
//! purpose — the two releases happen to agree on everything this server sends,
//! but keeping them apart means a correction to one can never disturb the
//! other.
//!
//! Structurally this sits between the two codecs it has for neighbours: there
//! is no configuration phase yet (that is 1.20.2), so the registries travel as
//! NBT inside the play Login packet, but chunks are already paletted
//! containers and blocks are already block-state ids.
//!
//! **Not verified against a live client.** No 1.18 client was available, so
//! everything here rests on encode/decode tests and the data above.

pub mod chunk;
pub mod registry;

use std::io;

use aether_world::BlockStateId;

use super::{json_escape, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{Conn, read_packet, PacketIn, PacketOut, RawPacket};

// --- Clientbound packet ids (1.18 / 1.18.2; the two tables are identical) ---
const LOGIN_SUCCESS: i32 = 0x02;
const PLAY_SPAWN_PLAYER: i32 = 0x04;
const PLAY_BLOCK_CHANGE: i32 = 0x0C;
const PLAY_CHAT: i32 = 0x0F;
const PLAY_UNLOAD_CHUNK: i32 = 0x1D;
const PLAY_GAME_EVENT: i32 = 0x1E;
const PLAY_KEEP_ALIVE: i32 = 0x21;
const PLAY_CHUNK: i32 = 0x22;
/// Play: move the centre of the client's loaded-column window. Columns
/// arriving outside it are discarded on receipt.
const PLAY_SET_CENTER_CHUNK: i32 = 0x49;
const PLAY_LOGIN: i32 = 0x26;
const PLAY_ABILITIES: i32 = 0x32;
const PLAY_PLAYER_INFO: i32 = 0x36;
const PLAY_POSITION: i32 = 0x38;
const PLAY_DESTROY_ENTITIES: i32 = 0x3A;
const PLAY_HEAD_ROTATION: i32 = 0x3E;
const PLAY_RESOURCE_PACK: i32 = 0x3C;
const PLAY_VIEW_POSITION: i32 = 0x49;
const PLAY_ENTITY_TELEPORT: i32 = 0x62;

// --- Serverbound play ---
/// "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x12;
const SB_CHAT: i32 = 0x03;
const SB_POSITION: i32 = 0x11;
const SB_POSITION_LOOK: i32 = 0x12;
const SB_LOOK: i32 = 0x13;
const SB_FLYING: i32 = 0x14;
const SB_BLOCK_DIG: i32 = 0x1A;
const SB_BLOCK_PLACE: i32 = 0x2E;

/// The only protocol this codec answers for: 1.18.2.
const PROTOCOL: i32 = 758;

/// The 1.18.2 codec.
pub struct Codec;

/// Engine block id -> 1.18 block-state id.
///
/// The version's half of the translation table. These are *not* the 1.21
/// numbers — the flattening registry grew between the two, so bedrock is 33
/// here against 85 there — which is exactly why each codec owns its own map.
/// Anything the version cannot express falls back to stone, so a client is
/// never handed an id outside its registry.
pub fn block_state(id: BlockStateId) -> u32 {
    use aether_api::block_ids as b;
    // Default states from minecraft-data pc/1.18/blocks.json.
    if id == b::AIR {
        0
    } else if id == b::STONE {
        1
    } else if id == b::GRASS_BLOCK {
        9
    } else if id == b::DIRT {
        10
    } else if id == b::BEDROCK {
        33
    } else if id == b::WATER {
        34
    } else if id == b::SAND {
        66
    } else if id == b::GRAVEL {
        68
    } else if id == b::OAK_LOG {
        77
    } else if id == b::OAK_LEAVES {
        161
    } else {
        1 // substitution: anything unexpressible becomes solid stone
    }
}

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.18.2"
    }

    fn protocol_id(&self) -> i32 {
        PROTOCOL
    }

    fn supports(&self, protocol: i32) -> bool {
        protocol == PROTOCOL
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        // Login Start carries only the username in this generation.
        let Some(p) = read_packet(s)? else {
            return Ok(None);
        };
        if p.id != 0x00 {
            return Ok(None);
        }
        Ok(Some(PacketIn::new(&p.data).string()?))
    }

    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Login Success: UUID as 16 raw bytes, then the name. No property
        // array here — that is a 1.19 addition.
        PacketOut::new(LOGIN_SUCCESS)
            .uuid(p.uuid)
            .string(&p.name)
            .send(s)?;

        // Play Login. The registries ride along as NBT: `dimensionCodec` holds
        // them all, `dimension` repeats the element for the world being
        // joined. Both are named-root NBT in this generation.
        let codec_nbt = registry::named_root(&registry::dimension_codec());
        let dimension_nbt = registry::named_root(&registry::dimension_type());
        PacketOut::new(PLAY_LOGIN)
            .i32(p.entity_id)
            .bool(false) // not hardcore
            .u8(p.game_mode.wire()) // survival 0 / creative 1
            .u8(0xFF) // previous game mode: none
            .var_int(1) // one world
            .string(registry::DIMENSION_NAME)
            .bytes(&codec_nbt)
            .bytes(&dimension_nbt)
            .string(registry::DIMENSION_NAME)
            .i64(0) // hashed seed
            .var_int(p.max_players as i32)
            .var_int(p.view_radius)
            .var_int(p.view_radius) // simulation distance
            .bool(false) // reduced debug info
            .bool(true) // enable respawn screen
            .bool(false) // not a debug world
            .bool(false) // not superflat
            .send(s)?;

        // Creative flight. The login game mode alone leaves the client without
        // the ability flags it checks before letting the player fly.
        PacketOut::new(PLAY_ABILITIES)
            .u8(0x0F) // invulnerable | flying | allow flying | creative
            .f32(0.05)
            .f32(0.10)
            .send(s)?;

        if p.resource_pack.enabled() {
            PacketOut::new(PLAY_RESOURCE_PACK)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .bool(p.resource_pack.required)
                .bool(false) // no custom prompt message
                .send(s)?;
        }

        // Set Center Chunk. From 1.14 on the client anchors its view on this,
        // and chunks arriving before it can be discarded.
        let (cx, cz) = (
            (p.spawn.0.floor() as i32) >> 4,
            (p.spawn.2.floor() as i32) >> 4,
        );
        PacketOut::new(PLAY_VIEW_POSITION)
            .var_int(cx)
            .var_int(cz)
            .send(s)?;

        // Tell the client to wait for chunks rather than render an empty void
        // while the first batch is in flight.
        PacketOut::new(PLAY_GAME_EVENT).u8(13).f32(0.0).send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Synchronize Player Position; `flags` of 0 makes every value absolute.
        PacketOut::new(PLAY_POSITION)
            .f64(p.spawn.0)
            .f64(p.spawn.1)
            .f64(p.spawn.2)
            .f32(p.yaw)
            .f32(p.pitch)
            .u8(0) // all absolute
            .var_int(1) // teleport id
            .bool(false) // do not dismount
            .send(s)
        // No hotbar: 1.18's Set Slot carries a full item stack this server has
        // no inventory model for. Creative players take blocks from the menu.
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
                p.var_int(0) // action: add player
                    .var_int(1) // one entry
                    .uuid(h.uuid)
                    .string(&h.name)
                    .var_int(0) // no profile properties
                    .var_int(1) // game mode: creative
                    .var_int(0) // ping
                    .bool(false); // no display name override
                vec![p]
            }
            ServerEvent::TabListRemove(uuid) => {
                let mut p = PacketOut::new(PLAY_PLAYER_INFO);
                p.var_int(4) // action: remove player
                    .var_int(1)
                    .uuid(*uuid);
                vec![p]
            }
            ServerEvent::SpawnPlayer(h) => {
                let pos = h.pos();
                // Spawn Player carries no head yaw in this generation, so the
                // head is squared up with a second packet.
                let mut spawn = PacketOut::new(PLAY_SPAWN_PLAYER);
                spawn
                    .var_int(h.entity_id)
                    .uuid(h.uuid)
                    .f64(pos.x)
                    .f64(pos.y)
                    .f64(pos.z)
                    .u8(angle(pos.yaw))
                    .u8(angle(pos.pitch));
                let mut head = PacketOut::new(PLAY_HEAD_ROTATION);
                head.var_int(h.entity_id).u8(angle(pos.yaw));
                vec![spawn, head]
            }
            ServerEvent::EntityMove(h) => {
                let pos = h.pos();
                // Angles are single bytes here, not the floats 1.21 uses.
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
            ServerEvent::AckBlockChange(_) => Vec::new(),
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
                // Before 1.19 a parser is named by identifier.
                super::commands::write_tree(&mut p, super::commands::Parsers::ByName);
                vec![p]
            }
            ServerEvent::Chat(text) => {
                // Still a JSON chat component in this generation, and the
                // packet ends with the sender's UUID — nil for system lines.
                let mut p = PacketOut::new(PLAY_CHAT);
                p.string(&format!("{{\"text\":\"{}\"}}", json_escape(text)))
                    .u8(1) // position: system message
                    .uuid(0);
                vec![p]
            }
            // Gameplay events this version does not render.
            _ => Vec::new(),
        }
    }

    /// Parse a received packet.
    ///
    /// Block placement always places stone: the packet names only the hand,
    /// so a server is expected to already know the player's inventory, and
    /// this one has no inventory model.
    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
        match pkt.id {
            SB_POSITION => match (pin.f64(), pin.f64(), pin.f64(), pin.bool()) {
                (Ok(x), Ok(y), Ok(z), Ok(on_ground)) => ClientEvent::Move(PosLook {
                    x,
                    y,
                    z,
                    on_ground,
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
            },
            SB_LOOK => match (pin.f32(), pin.f32(), pin.bool()) {
                (Ok(yaw), Ok(pitch), Ok(on_ground)) => ClientEvent::Move(PosLook {
                    yaw,
                    pitch,
                    on_ground,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            SB_FLYING => match pin.bool() {
                Ok(on_ground) => ClientEvent::Move(PosLook { on_ground, ..prev }),
                Err(_) => ClientEvent::Ignored,
            },
            SB_BLOCK_DIG => {
                let Ok(status) = pin.var_int() else {
                    return ClientEvent::Ignored;
                };
                // 0 is "started digging", which in creative is the whole
                // interaction; 2 is the survival "finished" case.
                if status != 0 && status != 2 {
                    return ClientEvent::Ignored;
                }
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let (x, y, z) = decode_position(packed);
                ClientEvent::Dig { x, y, z, seq: 0 }
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
                    seq: 0,
                    face: face as u8,
                    // No cursor on this codec's wire yet; the centre of
                    // the face is what a client clicking normally sends.
                    cursor: (0.5, 0.5, 0.5),
                }
            }
            SB_CHAT => match pin.string() {
                Ok(text) => ClientEvent::Chat(text),
                Err(_) => ClientEvent::Ignored,
            },
            _ => ClientEvent::Ignored,
        }
    }
}

/// Degrees -> the protocol's angle byte (256ths of a full turn).
fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u8
}

/// Encode a block position into the packed `i64`.
///
/// The 1.14 layout, unchanged through 1.21: `X (26) | Z (26) | Y (12)`, with Y
/// in the **low** bits — not 1.8's `X | Y | Z`.
fn encode_position(x: i64, y: i64, z: i64) -> i64 {
    ((x & 0x3FF_FFFF) << 38) | ((z & 0x3FF_FFFF) << 12) | (y & 0xFFF)
}

/// Unpack the packed block position, sign-extending each field.
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
    fn supports_exactly_its_own_protocol() {
        // One id per codec: 757 is a separate module and 759 (1.19) moved on
        // again. Claiming a neighbour would hand a client packets shaped for
        // a different release.
        assert!(Codec.supports(758), "1.18.2");
        assert!(!Codec.supports(757), "1.18/1.18.1 has its own module");
        assert!(!Codec.supports(759), "1.19 moved on again");
        assert_eq!(Codec.protocol_id(), 758);
    }

    #[test]
    fn position_uses_the_modern_layout() {
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
        // And it is not the 1.8 order, which would silently misplace blocks.
        assert_ne!(
            encode_position(1, 2, 3),
            ((1i64) << 38) | (2 << 26) | 3,
            "must not be 1.8 order"
        );
    }

    #[test]
    fn block_states_are_the_1_18_numbers_not_the_1_21_ones() {
        // The registry grew between the versions; reusing 1.21's ids here
        // would render bedrock as something else entirely.
        assert_eq!(block_state(b::BEDROCK), 33, "1.21 calls this 85");
        assert_eq!(block_state(b::WATER), 34, "1.21 calls this 86");
        assert_eq!(block_state(b::OAK_LEAVES), 161, "1.21 calls this 279");
        assert_eq!(block_state(b::AIR), 0, "air must be state 0");
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
        assert_eq!(seen.len(), before, "block states must be distinct");
    }

    #[test]
    fn unknown_blocks_fall_back_to_stone() {
        assert_eq!(block_state(BlockStateId(9999)), 1);
    }

    #[test]
    fn keep_alive_is_a_long_not_a_varint() {
        let world = chunk::tests_support::Empty;
        let pkts = Codec.encode(&ServerEvent::KeepAlive(1), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[0] as usize, wire.len() - 1, "frame length");
        assert_eq!(wire[1] as i32, PLAY_KEEP_ALIVE);
        assert_eq!(wire.len(), 1 + 1 + 8, "8-byte payload");
    }

    #[test]
    fn block_change_carries_a_state_id_and_the_modern_position() {
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
        // 161 as a varint: 0xA1 0x01
        assert_eq!(&body[8..10], &[0xA1, 0x01], "state id 161");
    }

    /// A registered player backed by a real loopback socket, since
    /// `PlayerHandle` owns a `TcpStream` and has no public constructor.
    fn loopback_player() -> std::sync::Arc<crate::players::PlayerHandle> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Conn::new(std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let _server_side = listener.accept().unwrap();
        crate::players::Registry::default()
            .join(
                7,
                0x1234,
                "tester".into(),
                &Codec,
                &client,
                PosLook {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                    yaw: 90.0,
                    pitch: 0.0,
                    on_ground: true,
                },
            )
            .unwrap()
    }

    #[test]
    fn spawn_player_is_followed_by_a_head_rotation() {
        // The spawn packet has no head yaw field in this generation, so a
        // codec that emits only one packet leaves heads facing north.
        let world = chunk::tests_support::Empty;
        let handle = loopback_player();
        let pkts = Codec.encode(&ServerEvent::SpawnPlayer(&handle), &world);
        assert_eq!(pkts.len(), 2, "spawn plus head rotation");
        let mut wire = Vec::new();
        pkts[1].write_to(&mut wire, None).unwrap();
        assert_eq!(wire[1] as i32, PLAY_HEAD_ROTATION);
        // Yaw 90° is a quarter turn: 64 of the 256 steps in an angle byte.
        assert_eq!(*wire.last().unwrap(), 64, "head yaw as an angle byte");
    }

    #[test]
    fn entity_teleport_sends_angles_as_bytes_not_floats() {
        // 1.21 widened these to floats. Writing floats here would push the
        // on-ground flag out of place and desynchronise the stream.
        let world = chunk::tests_support::Empty;
        let handle = loopback_player();
        let pkts = Codec.encode(&ServerEvent::EntityMove(&handle), &world);
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        let body = &wire[2..]; // past frame length and packet id
                               // varint entity id (7), 3 doubles, 2 angle bytes, 1 bool
        assert_eq!(body.len(), 1 + 24 + 2 + 1, "1.18 field widths");
        assert_eq!(body[0], 7, "entity id");
        assert_eq!(body[25], 64, "yaw byte");
        assert_eq!(body[27], 1, "on ground");
    }

    #[test]
    fn decode_reads_on_ground_as_a_bool_not_a_flag_byte() {
        // 1.21 replaced this field with a bit flag; reading it that way here
        // would still parse, so the difference has to be pinned by a test.
        let mut data = Vec::new();
        data.extend_from_slice(&1.0f64.to_be_bytes());
        data.extend_from_slice(&2.0f64.to_be_bytes());
        data.extend_from_slice(&3.0f64.to_be_bytes());
        data.push(1); // on ground
        let pkt = RawPacket {
            id: SB_POSITION,
            data,
        };
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 10.0,
            pitch: 20.0,
            on_ground: false,
        };
        match Codec.decode(&pkt, prev) {
            ClientEvent::Move(p) => {
                assert_eq!((p.x, p.y, p.z), (1.0, 2.0, 3.0));
                assert!(p.on_ground);
                assert_eq!((p.yaw, p.pitch), (10.0, 20.0), "look carried over");
            }
            _ => panic!("expected a move"),
        }
    }

    #[test]
    fn digging_decodes_the_position_it_was_given() {
        let mut data = Vec::new();
        data.push(0); // status: started digging
        data.extend_from_slice(&encode_position(-3, 70, 12).to_be_bytes());
        data.push(1); // face
        let pkt = RawPacket {
            id: SB_BLOCK_DIG,
            data,
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
            ClientEvent::Dig { x, y, z, .. } => assert_eq!((x, y, z), (-3, 70, 12)),
            _ => panic!("expected a dig"),
        }
    }
}
