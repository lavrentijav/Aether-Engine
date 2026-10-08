//! Minecraft 1.8.9 (protocol 47) codec.
//!
//! Offline mode, no compression, no encryption, creative game mode. This is
//! the dialect the server was originally written against; it now sits behind
//! [`ProtocolCodec`] like any other version.

pub mod chunk;

use std::io;

use super::{json_escape, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};
use crate::players::{PlayerHandle, PosLook};
use crate::proto::{read_packet, Conn, PacketIn, PacketOut, RawPacket};

/// Vanilla item ids handed to every player's hotbar so they can actually
/// build: this server sends no real inventory, and a client holding nothing
/// cannot place anything. Kept to blocks [`chunk::engine_id`] maps back, so
/// anything placed is a block the engine world can really store.
const HOTBAR: [u16; 7] = [1, 3, 2, 12, 13, 17, 18];

/// The 1.8.9 codec.
pub struct Codec;

impl ProtocolCodec for Codec {
    fn version_name(&self) -> &'static str {
        "1.8.9"
    }

    fn protocol_id(&self) -> i32 {
        47
    }

    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>> {
        // Login Start (0x00): username.
        let Some(p) = read_packet(s)? else {
            return Ok(None);
        };
        if p.id != 0x00 {
            return Ok(None);
        }
        Ok(Some(PacketIn::new(&p.data).string()?))
    }

    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Login Success (0x02): here the UUID is the *hyphenated string* form
        // — unlike Spawn Player, which wants the same UUID as 16 raw bytes.
        PacketOut::new(0x02)
            .string(&hyphenated(p.uuid))
            .string(&p.name)
            .send(s)?;

        // Join Game (0x01).
        PacketOut::new(0x01)
            .i32(p.entity_id)
            .u8(p.game_mode.wire()) // survival 0 / creative 1
            .u8(0) // dimension: overworld
            .u8(0) // difficulty: peaceful
            .u8(p.max_players.min(255) as u8)
            .string("flat")
            .bool(false) // reduced debug info
            .send(s)?;

        // Player Abilities (0x39): invulnerable + fly + allow-fly + creative.
        PacketOut::new(0x39)
            .u8(0x0F)
            .f32(0.05) // flying speed
            .f32(0.10) // field-of-view / walk speed
            .send(s)
    }

    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()> {
        // Spawn Position (0x05) and where the player actually appears.
        PacketOut::new(0x05)
            .i64(encode_position(8, (4 + Y_OFFSET) as i64, 8))
            .send(s)?;
        PacketOut::new(0x08)
            .f64(p.spawn.0)
            .f64(p.spawn.1 + Y_OFFSET as f64)
            .f64(p.spawn.2)
            .f32(p.yaw)
            .f32(p.pitch)
            .u8(0) // all absolute
            .send(s)?;

        // Resource Pack Send (0x48). 1.8 has no accept/decline handshake to
        // wait on — the offer is fire-and-forget.
        if p.resource_pack.enabled() {
            PacketOut::new(0x48)
                .string(&p.resource_pack.url)
                .string(&p.resource_pack.hash)
                .send(s)?;
        }

        for (slot, item) in HOTBAR.iter().enumerate() {
            PacketOut::new(0x2F)
                .u8(0) // window 0: the player's own inventory
                .u16(36 + slot as u16) // hotbar occupies slots 36..=44
                .u16(*item)
                .u8(1) // count
                .u16(0) // damage
                .u8(0) // no NBT
                .send(s)?;
        }
        Ok(())
    }

    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut> {
        match ev {
            ServerEvent::KeepAlive(id) => {
                let mut p = PacketOut::new(0x00);
                p.var_int(*id as i32);
                vec![p]
            }
            ServerEvent::ChunkColumn { cx, cz } => {
                vec![chunk::chunk_data_packet(*cx, *cz, &|x, y, z| {
                    world.block_at(x, y - Y_OFFSET, z)
                })]
            }
            ServerEvent::UnloadColumn { cx, cz } => vec![chunk::unload_chunk_packet(*cx, *cz)],
            ServerEvent::TabListAdd(h) => vec![player_list_add_packet(h)],
            ServerEvent::TabListRemove(uuid) => vec![player_list_remove_packet(*uuid)],
            ServerEvent::SpawnPlayer(h) => vec![spawn_player_packet(h)],
            // One neutral move becomes two packets here: 1.8 carries body and
            // head rotation separately, and without the head packet everyone
            // looks like they are staring straight ahead.
            ServerEvent::EntityMove(h) => vec![teleport_packet(h), head_look_packet(h)],
            ServerEvent::DespawnEntity(eid) => vec![destroy_entity_packet(*eid)],
            ServerEvent::BlockChange { x, y, z, block } => {
                let mut p = PacketOut::new(0x23);
                p.i64(encode_position(
                    *x as i64,
                    (*y + Y_OFFSET) as i64,
                    *z as i64,
                ))
                .var_int((chunk::vanilla_id(*block) as i32) << 4); // meta 0
                vec![p]
            }
            // 1.8 has no chunk-window centre: the client keeps every
            // column until explicitly told to unload it.
            ServerEvent::AckBlockChange(_) => Vec::new(),
            // This version has no server-driven container support here; the
            // caller falls back to a chat listing. See
            // `ProtocolCodec::supports_containers`.
            // No item entity on this version yet; the caller keeps the stack
            // rather than dropping it, so nothing is lost.
            ServerEvent::MoveEntity { .. } => Vec::new(),
            ServerEvent::DropItem { .. } => Vec::new(),
            ServerEvent::OpenContainer { .. } => Vec::new(),
            ServerEvent::SetCenterChunk { .. } => Vec::new(),
            // 1.8 predates the command tree entirely: it has no packet
            // for this and completes nothing.
            ServerEvent::CommandTree => vec![],
            ServerEvent::Chat(text) => {
                let mut p = PacketOut::new(0x02);
                p.string(&format!("{{\"text\":\"{}\"}}", json_escape(text)))
                    .u8(0); // position: normal chat
                vec![p]
            }
            // Gameplay events this version does not render.
            _ => Vec::new(),
        }
    }

    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent {
        let mut pin = PacketIn::new(&pkt.data);
        match pkt.id {
            0x03 => match pin.bool() {
                Ok(on_ground) => ClientEvent::Move(PosLook { on_ground, ..prev }),
                Err(_) => ClientEvent::Ignored,
            },
            0x04 | 0x06 => {
                let full = pkt.id == 0x06;
                let (Ok(x), Ok(y), Ok(z)) = (pin.f64(), pin.f64(), pin.f64()) else {
                    return ClientEvent::Ignored;
                };
                let (yaw, pitch) = if full {
                    match (pin.f32(), pin.f32()) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => return ClientEvent::Ignored,
                    }
                } else {
                    (prev.yaw, prev.pitch)
                };
                let Ok(on_ground) = pin.bool() else {
                    return ClientEvent::Ignored;
                };
                ClientEvent::Move(PosLook {
                    x,
                    // The client's world starts at zero; the engine's starts
                    // at -64. Everything crossing this boundary is shifted.
                    y: y - Y_OFFSET as f64,
                    z,
                    yaw,
                    pitch,
                    on_ground,
                })
            }
            0x05 => match (pin.f32(), pin.f32(), pin.bool()) {
                (Ok(yaw), Ok(pitch), Ok(on_ground)) => ClientEvent::Move(PosLook {
                    yaw,
                    pitch,
                    on_ground,
                    ..prev
                }),
                _ => ClientEvent::Ignored,
            },
            0x07 => {
                // Status 0 is "started digging"; in creative that is the whole
                // interaction, the client breaks the block immediately.
                let Ok(status) = pin.u8() else {
                    return ClientEvent::Ignored;
                };
                if status != 0 && status != 2 {
                    return ClientEvent::Ignored;
                }
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let (x, y, z) = decode_position(packed);
                let y = y - Y_OFFSET;
                ClientEvent::Dig { x, y, z, seq: 0 }
            }
            0x08 => {
                let Ok(packed) = pin.i64() else {
                    return ClientEvent::Ignored;
                };
                let Ok(face) = pin.u8() else {
                    return ClientEvent::Ignored;
                };
                if face > 5 {
                    return ClientEvent::Ignored; // 255: right-click in mid-air
                }
                let Ok(item) = pin.u16() else {
                    return ClientEvent::Ignored;
                };
                if item as i16 == -1 {
                    return ClientEvent::Ignored; // empty hand
                }
                let Some(block) = chunk::engine_id(item) else {
                    return ClientEvent::Ignored; // not a block this engine has
                };
                let (bx, by, bz) = decode_position(packed);
                let by = by - Y_OFFSET;
                let (dx, dy, dz) = face_offset(face);
                ClientEvent::Place {
                    x: bx + dx,
                    y: by + dy,
                    z: bz + dz,
                    block,
                    seq: 0,
                    face,
                    // No cursor on this codec's wire yet; the centre of
                    // the face is what a client clicking normally sends.
                    cursor: (0.5, 0.5, 0.5),
                }
            }
            0x01 => match pin.string() {
                Ok(text) => ClientEvent::Chat(text),
                Err(_) => ClientEvent::Ignored,
            },
            _ => ClientEvent::Ignored,
        }
    }
}

/// Unit offset of the block face a placement was made against.
pub fn face_offset(face: u8) -> (i32, i32, i32) {
    match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// `u128` as the hyphenated UUID string Login Success expects.
fn hyphenated(uuid: u128) -> String {
    let h = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Degrees -> the protocol's angle byte (256ths of a full turn).
fn angle(deg: f32) -> u8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u8
}

/// World units -> the 1.8 entity-position fixed-point (1/32 of a block).
fn fixed(v: f64) -> i32 {
    (v * 32.0).round() as i32
}

/// Player List Item (0x38), action 0 — add `h` to the tab list.
///
/// A 1.8 client will **not** render a player entity whose UUID it has no tab
/// list entry for, so this has to reach a viewer before [`spawn_player_packet`]
/// or the spawn is silently dropped.
fn player_list_add_packet(h: &PlayerHandle) -> PacketOut {
    let mut pkt = PacketOut::new(0x38);
    pkt.var_int(0) // action: add player
        .var_int(1) // one entry
        .uuid(h.uuid)
        .string(&h.name)
        .var_int(0) // no profile properties (offline mode: no skin signature)
        .var_int(1) // game mode: creative
        .var_int(0) // ping
        .bool(false); // no display name override
    pkt
}

/// Player List Item (0x38), action 4 — drop `uuid` from the tab list.
fn player_list_remove_packet(uuid: u128) -> PacketOut {
    let mut pkt = PacketOut::new(0x38);
    pkt.var_int(4).var_int(1).uuid(uuid);
    pkt
}

/// Spawn Player (0x0C). The UUID goes out as 16 **raw bytes** here; sending
/// the hyphenated string instead desynchronises every following field and the
/// client drops the connection mid-decode.
fn spawn_player_packet(h: &PlayerHandle) -> PacketOut {
    let p = h.pos();
    let mut pkt = PacketOut::new(0x0C);
    pkt.var_int(h.entity_id)
        .uuid(h.uuid)
        .i32(fixed(p.x))
        .i32(fixed(p.y + Y_OFFSET as f64))
        .i32(fixed(p.z))
        .u8(angle(p.yaw))
        .u8(angle(p.pitch))
        .u16(0) // current item: empty hand
        .u8(0x7F); // entity metadata: empty list terminator
    pkt
}

/// Entity Teleport (0x18): absolute position/look update for `h`.
fn teleport_packet(h: &PlayerHandle) -> PacketOut {
    let p = h.pos();
    let mut pkt = PacketOut::new(0x18);
    pkt.var_int(h.entity_id)
        .i32(fixed(p.x))
        .i32(fixed(p.y + Y_OFFSET as f64))
        .i32(fixed(p.z))
        .u8(angle(p.yaw))
        .u8(angle(p.pitch))
        .bool(p.on_ground);
    pkt
}

/// Entity Head Look (0x19): keeps the head yaw in sync with the body yaw.
fn head_look_packet(h: &PlayerHandle) -> PacketOut {
    let mut pkt = PacketOut::new(0x19);
    pkt.var_int(h.entity_id).u8(angle(h.pos().yaw));
    pkt
}

/// Destroy Entities (0x13) for a single entity id.
fn destroy_entity_packet(entity_id: i32) -> PacketOut {
    let mut pkt = PacketOut::new(0x13);
    pkt.var_int(1).var_int(entity_id);
    pkt
}

/// Encode a block position into a 1.8 packed `i64`.
///
/// Layout is `X (26 bits) | Y (12 bits) | Z (26 bits)`, each a signed
/// two's-complement field; the client sign-extends on decode. 1.14 later
/// reordered these fields to X-Z-Y, which is why this lives in the codec.
fn encode_position(x: i64, y: i64, z: i64) -> i64 {
    debug_assert!(
        (-(1 << 25)..(1 << 25)).contains(&x),
        "x out of 26-bit range"
    );
    debug_assert!(
        (-(1 << 11)..(1 << 11)).contains(&y),
        "y out of 12-bit range"
    );
    debug_assert!(
        (-(1 << 25)..(1 << 25)).contains(&z),
        "z out of 26-bit range"
    );
    ((x & 0x3FF_FFFF) << 38) | ((y & 0xFFF) << 26) | (z & 0x3FF_FFFF)
}

/// Unpack a 1.8 packed block position, sign-extending each field.
fn decode_position(v: i64) -> (i32, i32, i32) {
    (
        (v >> 38) as i32,
        ((v << 26) >> 52) as i32,
        ((v << 38) >> 38) as i32,
    )
}

/// How much to add to an engine Y to get one this version's client can hold.
///
/// The engine's world runs `-64..=319`, the modern range. This version's client
/// has no concept of a negative Y — its world is `0..=255` — so the whole
/// column is shifted up by 64 on the way out and back down on the way in. A
/// player standing on bedrock sees `y = 0`, which is what they expect, and the
/// server still knows they are at `-64`.
///
/// This is the one version where the range genuinely cannot be widened. 1.8's
/// chunk packet carries its section bitmask as a **`u16`** — sixteen sections,
/// 256 blocks — and there is no dimension type to say otherwise, so `0..=255`
/// is a protocol limit rather than a choice. Shifted by 64 it shows the engine's
/// `-64..=191`; the 192 blocks above that are not sent. Clipping the sky is the
/// least damaging place to lose them.
pub const Y_OFFSET: i32 = 64;

#[cfg(test)]
mod tests {

    #[test]
    fn this_versions_ceiling_is_a_protocol_limit_and_not_a_choice() {
        // 1.8's chunk packet carries its section bitmask as a `u16`, so there
        // are sixteen sections and no dimension type to say otherwise. Every
        // other version this server speaks was widened to 448; this one cannot
        // be, and the constant that says so must stay in step with the mask it
        // is written into.
        assert_eq!(
            chunk::SECTIONS * 16,
            256,
            "sixteen sections of sixteen blocks is all a u16 mask can address"
        );
        assert!(
            chunk::SECTIONS <= u16::BITS as usize,
            "more sections than the bitmask has bits"
        );
    }

    #[test]
    fn the_vertical_shift_is_its_own_inverse() {
        // The whole shift rests on this: a coordinate that goes out shifted
        // must come back shifted the other way, or the server and the client
        // disagree about where everything is by exactly 64 blocks — and every
        // symptom of that looks like a different bug.
        // 1.8 keeps no registry module; its world is the classic 0..=255.
        const MIN_Y: i32 = 0;
        const HEIGHT: i32 = 256;
        use super::Y_OFFSET;
        assert_eq!(MIN_Y, 0, "this client's world starts at zero");
        assert_eq!(
            Y_OFFSET, 64,
            "the engine's floor is -64, so that is what has to be added"
        );
        for engine_y in [-64, -1, 0, 63, 64, HEIGHT - Y_OFFSET - 1] {
            let client_y = engine_y + Y_OFFSET;
            assert!(
                (MIN_Y..MIN_Y + HEIGHT).contains(&client_y),
                "engine {engine_y} -> client {client_y} is outside the client's range"
            );
            assert_eq!(client_y - Y_OFFSET, engine_y, "round trip");
        }
    }
    use super::*;

    #[test]
    fn position_encoding_matches_1_8() {
        let enc = encode_position(8, 4, 8);
        assert_eq!(decode_position(enc), (8, 4, 8));
    }

    #[test]
    fn position_round_trips_negative_coordinates() {
        for p in [(-1, 0, -1), (-300, 200, 4000), (1 << 20, -5, -(1 << 20))] {
            let enc = encode_position(p.0 as i64, p.1 as i64, p.2 as i64);
            assert_eq!(decode_position(enc), p, "round trip {p:?}");
        }
    }

    #[test]
    fn angle_wraps_and_scales() {
        assert_eq!(angle(0.0), 0);
        assert_eq!(angle(180.0), 128);
        assert_eq!(angle(360.0), 0);
        assert_eq!(angle(-90.0), 192); // -90 mod 360 = 270 -> 270/360*256 = 192
    }

    #[test]
    fn fixed_point_round_trips() {
        assert_eq!(fixed(8.5), 272); // 8.5 * 32
        assert_eq!(fixed(0.0), 0);
    }

    #[test]
    fn uuid_is_hyphenated_in_canonical_shape() {
        let s = hyphenated(0xc1bb91ad_ab62_3f9f_8ab7_78f49746acf0);
        assert_eq!(s, "c1bb91ad-ab62-3f9f-8ab7-78f49746acf0");
    }
}
