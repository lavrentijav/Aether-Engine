//! Minecraft 1.20.2 (protocol 764) codec.
//!
//! Self-contained by design: every packet id and field list below is written
//! out for this release alone, with no branching on version. Neighbouring
//! releases live in their own modules and a fix here cannot reach them —
//! which also means a fix here does not *propagate*, so a bug found in this
//! file is worth checking for in the siblings.
//!
//! Ids and field lists come from PrismarineJS `minecraft-data`
//! (`pc/1.20.2/protocol.json`), block states from its `blocks.json`.
//!
//! This is the release that introduced the **configuration phase** and the
//! **chunk-batch** flow-control packets. The registries still travel as one NBT
//! blob (unlike 1.20.5's packet per registry) but now in that phase instead of
//! inside Play Login, and the blob's root is anonymous (see [`registry`]).
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
    &super::modern::version::V1_20_2
}

use super::modern;
use super::{json_escape, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::PosLook;
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

/// The protocol id this codec speaks.
pub const PROTOCOL: i32 = 764;

/// Login state ids.
const LOGIN_SUCCESS: i32 = 0x02;
const LOGIN_ACKNOWLEDGED: i32 = 0x03;

/// Configuration: the whole registry codec as one anonymous-NBT blob.
const CFG_REGISTRY_DATA: i32 = 0x05;
/// Configuration: server says it is done.
const CFG_FINISH: i32 = 0x02;
/// Configuration: the client's acknowledgement.
const CFG_FINISH_ACK: i32 = 0x02;
/// Configuration: offer a resource pack.
const CFG_ADD_RESOURCE_PACK: i32 = 0x06;

/// Play: the join packet.
const PLAY_LOGIN: i32 = 0x29;
/// Play: chunk data and light in one packet.
const PLAY_MAP_CHUNK: i32 = 0x25;
/// Play: open a batch of columns. Empty.
const PLAY_CHUNK_BATCH_START: i32 = 0x0d;
/// Play: close a batch, naming how many columns it carried.
const PLAY_CHUNK_BATCH_FINISHED: i32 = 0x0c;
/// Play: move the centre of the client's loaded-column window. Columns
/// arriving outside it are discarded on receipt.
const PLAY_SET_CENTER_CHUNK: i32 = 0x50;
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
const PLAY_SYSTEM_CHAT: i32 = 0x67;
/// Play: "Declare Commands": the server's command grammar.
const PLAY_COMMANDS: i32 = 0x11;

/// Serverbound play packet ids.
/// A slash command. Since 1.19 the client sends these on their own packet
/// instead of as a chat message, so a server that only decodes chat never
/// sees a single command — which is exactly what happened here.
const SB_CHAT_COMMAND: i32 = 0x04;
const SB_CHAT: i32 = 0x05;
const SB_POSITION: i32 = 0x16;
const SB_POSITION_LOOK: i32 = 0x17;
const SB_LOOK: i32 = 0x18;
const SB_FLYING: i32 = 0x19;
const SB_BLOCK_DIG: i32 = 0x20;
const SB_BLOCK_PLACE: i32 = 0x34;

/// Engine block id -> this version's block-state id.
///
/// This half of the translation table is what makes one engine world render
/// on many clients. Anything the version cannot express falls back to stone,
/// so a client is never handed an id outside its registry.
pub fn block_state(id: BlockStateId) -> u32 {
    crate::protocol::compat::V1_20_2.state(id)
}

/// The 1.20.2 codec.
pub struct Codec;

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.20.2"
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
        // Login Start: the username comes first. Whatever follows it (a chat
        // signature, an optional player UUID) is left unread — this server
        // derives an offline UUID from the name.
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

        // The client acknowledges and moves itself into the configuration
        // state; nothing may be sent in between.
        wait_for(s, LOGIN_ACKNOWLEDGED)?;

        // Configuration: the registries the client builds its world from, as
        // one blob. Without these it disconnects before ever reaching play.
        PacketOut::new(CFG_REGISTRY_DATA)
            .bytes(&registry::blob())
            .send(s)?;

        if p.resource_pack.enabled() {
            PacketOut::new(CFG_ADD_RESOURCE_PACK)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .bool(p.resource_pack.required)
                .bool(false) // no custom prompt message
                .send(s)?;
        }

        PacketOut::new(CFG_FINISH).send(s)?;
        wait_for(s, CFG_FINISH_ACK)?;

        // Play Login. The registries are *not* here any more — they went out
        // in the configuration phase above.
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
        login.send(s)
        // No "wait for chunks" game event: reason 13 arrives in 1.20.3. A 1.20.2
        // client starts rendering as the first batch lands regardless.
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
                p.i64(*id); // a long here, a varint in 1.8
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                // A batch of one: 1.20.2 introduced chunk flow control, and a
                // client that never sees `finished` leaves its chunk-rate
                // estimate stalled. Wrapping each column is valid, if chatty.
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
                let mut p = PacketOut::new(PLAY_PLAYER_REMOVE);
                // Removal is its own packet from 1.19.3 on, carrying just
                // an array of UUIDs.
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
                // From 1.19 on a parser is an index into the registry.
                super::commands::write_tree(&mut p, super::commands::Parsers::ById);
                vec![p]
            }
            ServerEvent::Chat(text) => {
                // System Chat still takes a JSON string here; the NBT text
                // component only arrives with 1.20.3.
                let mut p = PacketOut::new(PLAY_SYSTEM_CHAT);
                p.string(&format!("{{\"text\":\"{}\"}}", json_escape(text)));
                p.bool(false); // not an action-bar message
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
    /// Two deliberate gaps, both from this server having no inventory model:
    /// placement always places stone (the packet names only the hand), and
    /// incoming chat signatures are ignored rather than verified — this server
    /// never echoes player chat as signed chat, so nothing depends on them.
    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
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
            // 1.20.2 does carry the cursor, unlike 1.20.1; it decides a
            // slab's half and a stair's corner on the server.
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
                block: aether_api::block_ids::STONE,
                seq,
                face: face as u8,
                cursor,
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
        // Window clicks, creative slots, held slot, sprint, sneak, swing,
        // entity interaction and use item: the shared gameplay decoder.
        modern::Codec(version()).decode(pkt, prev)
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
        assert_eq!(block_state(b::OAK_LEAVES), 264);
        assert_eq!(block_state(b::BEDROCK), 79);
        assert_eq!(block_state(b::WATER), 80);
        assert_eq!(block_state(b::AIR), 0);
        assert_eq!(block_state(b::STONE), 1);
        assert_eq!(block_state(b::GRASS_BLOCK), 9);
        assert_eq!(block_state(b::DIRT), 10);
    }

    #[test]
    fn unknown_blocks_fall_back_to_stone() {
        // 9,999 is a real engine state now (the engine's ids are 1.21.11's),
        // so the substitution boundary is the end of the state space.
        let modded = BlockStateId(aether_world::registry::blocks::STATE_COUNT as u32 + 7);
        assert_eq!(block_state(modded), 1);
    }

    #[test]
    fn position_layout_is_the_modern_one() {
        for p in [(0, 0, 0), (8, 64, 8), (-300, 100, 4000), (-1, 0, -1)] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
    }

    #[test]
    fn keep_alive_is_a_long_not_a_varint() {
        let (id, body) = encode_one(&ServerEvent::KeepAlive(1));
        assert_eq!(id, PLAY_KEEP_ALIVE);
        assert_eq!(body.len(), 8, "an i64 payload");
    }

    #[test]
    fn a_column_is_wrapped_in_a_chunk_batch() {
        // 1.20.2 introduced chunk flow control; the batch must open and close
        // around the column, and the close names the column count.
        let world = chunk::tests_support::Empty;
        let pkts = Codec.encode(&ServerEvent::ChunkColumn { cx: 0, cz: 0 }, &world);
        assert_eq!(pkts.len(), 3, "batch start, column, batch finished");
        let mut wire = Vec::new();
        pkts[0].write_to(&mut wire, None).unwrap();
        assert_eq!(split_packet(&wire).0, PLAY_CHUNK_BATCH_START);
        wire.clear();
        pkts[1].write_to(&mut wire, None).unwrap();
        assert_eq!(split_packet(&wire).0, PLAY_MAP_CHUNK);
        wire.clear();
        pkts[2].write_to(&mut wire, None).unwrap();
        let (id, body) = split_packet(&wire);
        assert_eq!(id, PLAY_CHUNK_BATCH_FINISHED);
        assert_eq!(body, vec![1], "one column in the batch");
    }

    #[test]
    fn block_change_carries_a_state_id() {
        let (id, body) = encode_one(&ServerEvent::BlockChange {
            x: 1,
            y: 2,
            z: 3,
            block: b::OAK_LEAVES,
        });
        assert_eq!(id, PLAY_BLOCK_CHANGE);
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
        assert_eq!(state, 264, "the leaves state id for this release");
    }

    #[test]
    fn tab_list_removal_uses_this_releases_shape() {
        assert_ne!(
            PLAY_PLAYER_REMOVE, PLAY_PLAYER_INFO,
            "from 1.19.3 removal is a packet of its own"
        );
        let uuid = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdefu128;
        let (id, body) = encode_one(&ServerEvent::TabListRemove(uuid));
        assert_eq!(id, PLAY_PLAYER_REMOVE);
        assert_eq!(body[0], 1, "one uuid follows");
        assert_eq!(body.len(), 1 + 16);
    }

    #[test]
    fn chat_is_json_in_this_release() {
        // 1.20.3 switched System Chat to an NBT component; here it is still a
        // length-prefixed JSON string plus the action-bar flag.
        let (id, body) = encode_one(&ServerEvent::Chat("hi".into()));
        assert_eq!(id, PLAY_SYSTEM_CHAT);
        let len = body[0] as usize;
        let text = std::str::from_utf8(&body[1..1 + len]).unwrap();
        assert_eq!(text, r#"{"text":"hi"}"#);
        assert_eq!(body.len(), 1 + len + 1, "one trailing flag byte");
    }

    #[test]
    fn movement_decodes_with_a_plain_on_ground_bool() {
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
        let pkt = RawPacket {
            id: SB_BLOCK_DIG,
            data,
        };
        match Codec.decode(&pkt, prev) {
            ClientEvent::StartDig { x, y, z, .. } => assert_eq!((x, y, z), (5, 70, -9)),
            _ => panic!("expected a start-dig event"),
        }
    }

    #[test]
    fn placing_decodes_the_cursor_this_release_sends() {
        let prev = PosLook {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
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
        match Codec.decode(&pkt, prev) {
            ClientEvent::Place {
                x,
                y,
                z,
                face,
                cursor,
                ..
            } => {
                assert_eq!((x, y, z), (5, 71, -9), "the clicked cell, offset up");
                assert_eq!(face, 1);
                assert_eq!(cursor, (0.25, 0.75, 0.5));
            }
            _ => panic!("expected a place event"),
        }
    }

    #[test]
    fn the_ack_carries_the_sequence_and_sits_four_below_block_update() {
        assert_eq!(PLAY_BLOCK_CHANGED_ACK + 4, PLAY_BLOCK_CHANGE);
        let (id, body) = encode_one(&ServerEvent::AckBlockChange(300));
        assert_eq!(id, PLAY_BLOCK_CHANGED_ACK);
        // 300 as a VarInt: 0b10101100, 0b00000010.
        assert_eq!(body, vec![0xAC, 0x02]);
    }

    #[test]
    fn the_codec_runs_the_full_game_and_exposes_windows() {
        assert!(Codec.full_gameplay());
        assert!(Codec.supports_containers());
    }

    #[test]
    fn the_trident_is_the_stand_in_the_item_layer_chooses_for_a_spear_on_764() {
        // A 1.20.2 client has no spears, so the item layer shows a trident;
        // the codec resolves the echoed stand-in back to the real spear.
        let spear = crate::inventory::item("minecraft:iron_spear");
        let shown = crate::protocol::compat::items::V1_20_2
            .wire(spear)
            .expect("1.20.2 has a stand-in for a spear");
        assert!(shown.disguised, "the spear is shown as another item");
        assert_eq!(
            crate::protocol::compat::items::V1_20_2.engine(shown.id),
            Some(crate::inventory::item("minecraft:trident")),
        );
        assert_eq!(
            Codec.resolve_reported_item(spear, crate::inventory::item("minecraft:trident")),
            spear,
        );
    }
}
