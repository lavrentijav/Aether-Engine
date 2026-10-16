//! Simple Voice Chat (henkelmax) server protocol.
//!
//! Pinned against the mod's `1.21.11` branch (`common/src/main/java/…`): the
//! plugin-channel handshake (`voicechat:*` custom payloads) and the UDP audio
//! protocol. The UDP payload is a `[0xFF][player UUID][encrypted]` envelope
//! where the encrypted part is `AES/GCM` of `[packet type][body]` under that
//! player's 16-byte secret (see [`crate::crypto`]).
//!
//! Everything here is pure: bytes in, a packet out, no sockets and no locks, so
//! a captured packet can be replayed in a test.

use crate::bytes::{Reader, Writer};
use crate::crypto::Gcm;

/// The UDP magic byte every SVC datagram starts with.
pub const MAGIC: u8 = 0xff;
/// The largest decrypted SVC payload the mod accepts.
pub const MAX_PACKET_SIZE: usize = 2048;
/// The largest Opus payload the mod accepts in one packet.
pub const MAX_OPUS_PAYLOAD: usize = 1275;
/// The secret key length.
pub const SECRET_SIZE: usize = 16;
/// The GCM IV length.
pub const IV_SIZE: usize = 12;

/// A per-player 16-byte secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Secret(pub [u8; SECRET_SIZE]);

impl Secret {
    /// A fresh secret from the OS random source.
    pub fn random() -> Secret {
        use rand::RngCore;
        let mut b = [0u8; SECRET_SIZE];
        rand::thread_rng().fill_bytes(&mut b);
        Secret(b)
    }

    /// Encrypt `plaintext` as the mod does: `IV || AES-GCM(plaintext)`, the IV
    /// random per packet.
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        use rand::RngCore;
        let mut iv = [0u8; IV_SIZE];
        rand::thread_rng().fill_bytes(&mut iv);
        let body = Gcm::new(&self.0).encrypt(&iv, &[], plaintext);
        let mut out = Vec::with_capacity(IV_SIZE + body.len());
        out.extend_from_slice(&iv);
        out.extend_from_slice(&body);
        out
    }

    /// Decrypt an `IV || ciphertext || tag` payload. `None` if the tag is bad.
    pub fn decrypt(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < IV_SIZE + 16 {
            return None;
        }
        let iv: [u8; IV_SIZE] = payload[..IV_SIZE].try_into().unwrap();
        Gcm::new(&self.0).decrypt(&iv, &[], &payload[IV_SIZE..])
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

/// The Opus codec the server advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    Voip = 0,
    Audio = 1,
    RestrictedLowdelay = 2,
}

impl Codec {
    pub fn from_ordinal(v: u8) -> Codec {
        match v {
            1 => Codec::Audio,
            2 => Codec::RestrictedLowdelay,
            _ => Codec::Voip,
        }
    }
}

/// One decrypted UDP packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Mic {
        data: Vec<u8>,
        sequence: i64,
        whispering: bool,
    },
    PlayerSound {
        channel_id: u128,
        sender: u128,
        data: Vec<u8>,
        sequence: i64,
        whispering: bool,
        distance: f32,
        category: Option<String>,
    },
    GroupSound {
        channel_id: u128,
        sender: u128,
        data: Vec<u8>,
        sequence: i64,
        category: Option<String>,
    },
    LocationSound {
        channel_id: u128,
        sender: u128,
        x: f64,
        y: f64,
        z: f64,
        data: Vec<u8>,
        sequence: i64,
        distance: f32,
        category: Option<String>,
    },
    Authenticate {
        player: u128,
        secret: Secret,
    },
    AuthenticateAck,
    Ping {
        id: u128,
        timestamp: i64,
    },
    KeepAlive,
    ConnectionCheck,
    ConnectionCheckAck,
}

pub const WHISPER_MASK: u8 = 0b1;
pub const HAS_CATEGORY_MASK: u8 = 0b10;

impl Packet {
    fn type_byte(&self) -> u8 {
        match self {
            Packet::Mic { .. } => 0x1,
            Packet::PlayerSound { .. } => 0x2,
            Packet::GroupSound { .. } => 0x3,
            Packet::LocationSound { .. } => 0x4,
            Packet::Authenticate { .. } => 0x5,
            Packet::AuthenticateAck => 0x6,
            Packet::Ping { .. } => 0x7,
            Packet::KeepAlive => 0x8,
            Packet::ConnectionCheck => 0x9,
            Packet::ConnectionCheckAck => 0xa,
        }
    }

    /// Whether a sound payload should be dropped after 500 ms (only a mic
    /// packet has that short TTL in the mod).
    pub fn ttl_ms(&self) -> i64 {
        match self {
            Packet::Mic { .. } => 500,
            _ => 10_000,
        }
    }

    fn write_body(&self, w: &mut Writer) {
        match self {
            Packet::Mic {
                data,
                sequence,
                whispering,
            } => {
                w.byte_array(data).i64(*sequence).bool(*whispering);
            }
            Packet::PlayerSound {
                channel_id,
                sender,
                data,
                sequence,
                whispering,
                distance,
                category,
            } => {
                w.uuid(*channel_id)
                    .uuid(*sender)
                    .byte_array(data)
                    .i64(*sequence)
                    .f32(*distance);
                let mut flags = 0u8;
                if *whispering {
                    flags |= WHISPER_MASK;
                }
                if category.is_some() {
                    flags |= HAS_CATEGORY_MASK;
                }
                w.u8(flags);
                if let Some(c) = category {
                    w.utf(c);
                }
            }
            Packet::GroupSound {
                channel_id,
                sender,
                data,
                sequence,
                category,
            } => {
                w.uuid(*channel_id)
                    .uuid(*sender)
                    .byte_array(data)
                    .i64(*sequence);
                w.u8(if category.is_some() {
                    HAS_CATEGORY_MASK
                } else {
                    0
                });
                if let Some(c) = category {
                    w.utf(c);
                }
            }
            Packet::LocationSound {
                channel_id,
                sender,
                x,
                y,
                z,
                data,
                sequence,
                distance,
                category,
            } => {
                w.uuid(*channel_id)
                    .uuid(*sender)
                    .f64(*x)
                    .f64(*y)
                    .f64(*z)
                    .byte_array(data)
                    .i64(*sequence)
                    .f32(*distance);
                w.u8(if category.is_some() {
                    HAS_CATEGORY_MASK
                } else {
                    0
                });
                if let Some(c) = category {
                    w.utf(c);
                }
            }
            Packet::Authenticate { player, secret } => {
                w.uuid(*player).secret16(&secret.0);
            }
            Packet::Ping { id, timestamp } => {
                w.uuid(*id).i64(*timestamp);
            }
            Packet::AuthenticateAck | Packet::KeepAlive | Packet::ConnectionCheck => {}
            Packet::ConnectionCheckAck => {}
        }
    }

    fn read_body(kind: u8, r: &mut Reader) -> Result<Packet, String> {
        Ok(match kind {
            0x1 => Packet::Mic {
                data: r.byte_array(MAX_OPUS_PAYLOAD)?,
                sequence: r.i64()?,
                whispering: r.bool()?,
            },
            0x2 => {
                let channel_id = r.uuid()?;
                let sender = r.uuid()?;
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let distance = r.f32()?;
                let flags = r.u8()?;
                let category = if flags & HAS_CATEGORY_MASK != 0 {
                    Some(r.utf(16)?)
                } else {
                    None
                };
                Packet::PlayerSound {
                    channel_id,
                    sender,
                    data,
                    sequence,
                    whispering: flags & WHISPER_MASK != 0,
                    distance,
                    category,
                }
            }
            0x3 => {
                let channel_id = r.uuid()?;
                let sender = r.uuid()?;
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let flags = r.u8()?;
                let category = if flags & HAS_CATEGORY_MASK != 0 {
                    Some(r.utf(16)?)
                } else {
                    None
                };
                Packet::GroupSound {
                    channel_id,
                    sender,
                    data,
                    sequence,
                    category,
                }
            }
            0x4 => {
                let channel_id = r.uuid()?;
                let sender = r.uuid()?;
                let x = r.f64()?;
                let y = r.f64()?;
                let z = r.f64()?;
                let data = r.byte_array(MAX_OPUS_PAYLOAD)?;
                let sequence = r.i64()?;
                let distance = r.f32()?;
                let flags = r.u8()?;
                let category = if flags & HAS_CATEGORY_MASK != 0 {
                    Some(r.utf(16)?)
                } else {
                    None
                };
                Packet::LocationSound {
                    channel_id,
                    sender,
                    x,
                    y,
                    z,
                    data,
                    sequence,
                    distance,
                    category,
                }
            }
            0x5 => Packet::Authenticate {
                player: r.uuid()?,
                secret: Secret(r.secret16()?),
            },
            0x6 => Packet::AuthenticateAck,
            0x7 => Packet::Ping {
                id: r.uuid()?,
                timestamp: r.i64()?,
            },
            0x8 => Packet::KeepAlive,
            0x9 => Packet::ConnectionCheck,
            0xa => Packet::ConnectionCheckAck,
            other => return Err(format!("unknown SVC packet type {other:#x}")),
        })
    }

    /// Encrypt this packet for `secret` and wrap it in the **server-to-client**
    /// UDP envelope, `[0xFF][var-int length][IV || ciphertext || tag]`. The
    /// client's own envelope additionally carries its player UUID in plaintext
    /// before the byte array ([`encode_client_udp`]).
    pub fn encode_server_udp(&self, secret: &Secret) -> Vec<u8> {
        let mut body = Writer::new();
        body.u8(self.type_byte());
        self.write_body(&mut body);
        let mut out = Writer::new();
        out.u8(MAGIC);
        out.byte_array(&secret.encrypt(body.as_slice()));
        out.into_vec()
    }

    /// Wrap this packet in the **client-to-server** envelope, which prefixes the
    /// player UUID so the server can find the secret before decrypting.
    pub fn encode_client_udp(&self, player: u128, secret: &Secret) -> Vec<u8> {
        let mut body = Writer::new();
        body.u8(self.type_byte());
        self.write_body(&mut body);
        let mut out = Writer::new();
        out.u8(MAGIC);
        out.uuid(player);
        out.byte_array(&secret.encrypt(body.as_slice()));
        out.into_vec()
    }

    /// Decode a client-to-server datagram: `[0xFF][player UUID][encrypted]`.
    /// `None` on a bad magic, a bad length or a failed tag check — the packet
    /// is simply dropped, as the mod does.
    pub fn decode_client_udp(datagram: &[u8], secret: &Secret) -> Option<Packet> {
        let mut r = Reader::new(datagram);
        if r.u8().ok()? != MAGIC {
            return None;
        }
        let _player = r.uuid().ok()?;
        Self::decode_encrypted(&mut r, secret)
    }

    /// Decode a server-to-client datagram: `[0xFF][encrypted]`.
    pub fn decode_server_udp(datagram: &[u8], secret: &Secret) -> Option<Packet> {
        let mut r = Reader::new(datagram);
        if r.u8().ok()? != MAGIC {
            return None;
        }
        Self::decode_encrypted(&mut r, secret)
    }

    fn decode_encrypted(r: &mut Reader, secret: &Secret) -> Option<Packet> {
        let encrypted = r.byte_array(MAX_PACKET_SIZE + 64).ok()?;
        let plain = secret.decrypt(&encrypted)?;
        let mut pr = Reader::new(&plain);
        let kind = pr.u8().ok()?;
        Packet::read_body(kind, &mut pr).ok()
    }
}

/// One decrypted datagram together with the metadata the mod tracks.
#[derive(Debug, Clone)]
pub struct Datagram {
    pub packet: Packet,
    pub timestamp: i64,
    /// The player UUID from the *unencrypted* envelope, before the secret is
    /// known — used only to look the secret up.
    pub claimed_player: u128,
}

/// Peek the claimed player UUID out of a client-to-server datagram without a
/// secret. Returns `None` when the magic or framing is wrong.
pub fn peek_player(datagram: &[u8]) -> Option<u128> {
    let mut r = Reader::new(datagram);
    if r.u8().ok()? != MAGIC {
        return None;
    }
    r.uuid().ok()
}

// ---------------------------------------------------------------------------
// Plugin-channel (custom payload) packets.
// ---------------------------------------------------------------------------

/// The `voicechat:` channels the server answers.
pub mod channel {
    pub const REQUEST_SECRET: &str = "voicechat:request_secret";
    pub const SECRET: &str = "voicechat:secret";
    pub const UPDATE_STATE: &str = "voicechat:update_state";
    pub const STATE: &str = "voicechat:state";
    pub const STATES: &str = "voicechat:states";
    pub const REMOVE_STATE: &str = "voicechat:remove_state";
    pub const ADD_GROUP: &str = "voicechat:add_group";
    pub const REMOVE_GROUP: &str = "voicechat:remove_group";
    pub const SET_GROUP: &str = "voicechat:set_group";
    pub const CREATE_GROUP: &str = "voicechat:create_group";
    pub const LEAVE_GROUP: &str = "voicechat:leave_group";
    pub const JOINED_GROUP: &str = "voicechat:joined_group";
}

/// A player's presence as the mod broadcasts it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub disabled: bool,
    pub disconnected: bool,
    pub uuid: u128,
    pub name: String,
    pub group: Option<u128>,
}

impl PlayerState {
    fn write(&self, w: &mut Writer) {
        w.bool(self.disabled)
            .bool(self.disconnected)
            .uuid(self.uuid)
            .utf(&self.name)
            .bool(self.group.is_some());
        if let Some(g) = self.group {
            w.uuid(g);
        }
    }
}

/// A group as clients know it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientGroup {
    pub id: u128,
    pub name: String,
    pub has_password: bool,
    pub persistent: bool,
    pub hidden: bool,
    pub kind: i16,
}

impl ClientGroup {
    fn write(&self, w: &mut Writer) {
        w.uuid(self.id)
            .utf(&self.name)
            .bool(self.has_password)
            .bool(self.persistent)
            .bool(self.hidden)
            .i16(self.kind);
    }
}

/// A plugin-channel packet.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelPacket {
    RequestSecret {
        compatibility_version: i32,
    },
    Secret(Box<SecretData>),
    UpdateState {
        disabled: bool,
    },
    State(PlayerState),
    States(Vec<PlayerState>),
    RemoveState {
        id: u128,
    },
    AddGroup(ClientGroup),
    RemoveGroup {
        id: u128,
    },
    JoinGroup {
        group: u128,
        password: Option<String>,
    },
    CreateGroup {
        name: String,
        password: Option<String>,
        kind: i16,
    },
    LeaveGroup,
    JoinedGroup {
        group: Option<u128>,
        wrong_password: bool,
    },
}

/// The `voicechat:secret` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SecretData {
    pub secret: Secret,
    pub server_port: i32,
    pub player: u128,
    pub codec: u8,
    pub mtu: i32,
    pub distance: f64,
    pub keep_alive: i32,
    pub groups_enabled: bool,
    pub voice_host: String,
    pub allow_recording: bool,
}

impl ChannelPacket {
    /// The channel a server packet is sent on, or `None` for a client-only one.
    pub fn channel(&self) -> Option<&'static str> {
        Some(match self {
            ChannelPacket::Secret(_) => channel::SECRET,
            ChannelPacket::State(_) => channel::STATE,
            ChannelPacket::States(_) => channel::STATES,
            ChannelPacket::RemoveState { .. } => channel::REMOVE_STATE,
            ChannelPacket::AddGroup(_) => channel::ADD_GROUP,
            ChannelPacket::RemoveGroup { .. } => channel::REMOVE_GROUP,
            ChannelPacket::JoinedGroup { .. } => channel::JOINED_GROUP,
            // Client-bound channels this server does not originate.
            ChannelPacket::RequestSecret { .. }
            | ChannelPacket::UpdateState { .. }
            | ChannelPacket::JoinGroup { .. }
            | ChannelPacket::CreateGroup { .. }
            | ChannelPacket::LeaveGroup => return None,
        })
    }

    /// Encode the payload (without the channel string).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            ChannelPacket::RequestSecret {
                compatibility_version,
            } => {
                w.i32(*compatibility_version);
            }
            ChannelPacket::Secret(s) => {
                w.secret16(&s.secret.0)
                    .i32(s.server_port)
                    .uuid(s.player)
                    .u8(s.codec)
                    .i32(s.mtu)
                    .f64(s.distance)
                    .i32(s.keep_alive)
                    .bool(s.groups_enabled)
                    .utf(&s.voice_host)
                    .bool(s.allow_recording);
            }
            ChannelPacket::UpdateState { disabled } => {
                w.bool(*disabled);
            }
            ChannelPacket::State(state) => state.write(&mut w),
            ChannelPacket::States(states) => {
                w.i32(states.len() as i32);
                for s in states {
                    s.write(&mut w);
                }
            }
            ChannelPacket::RemoveState { id } => {
                w.uuid(*id);
            }
            ChannelPacket::AddGroup(g) => g.write(&mut w),
            ChannelPacket::RemoveGroup { id } => {
                w.uuid(*id);
            }
            ChannelPacket::JoinGroup { group, password } => {
                w.uuid(*group).bool(password.is_some());
                if let Some(p) = password {
                    w.utf(p);
                }
            }
            ChannelPacket::CreateGroup {
                name,
                password,
                kind,
            } => {
                w.utf(name).bool(password.is_some());
                if let Some(p) = password {
                    w.utf(p);
                }
                w.i16(*kind);
            }
            ChannelPacket::LeaveGroup => {}
            ChannelPacket::JoinedGroup {
                group,
                wrong_password,
            } => {
                w.bool(group.is_some());
                if let Some(g) = group {
                    w.uuid(*g);
                }
                w.bool(*wrong_password);
            }
        }
        w.into_vec()
    }

    /// Decode a client packet for `channel`.
    pub fn decode(channel: &str, payload: &[u8]) -> Result<ChannelPacket, String> {
        let mut r = Reader::new(payload);
        Ok(match channel {
            channel::REQUEST_SECRET => ChannelPacket::RequestSecret {
                compatibility_version: r.i32()?,
            },
            channel::UPDATE_STATE => ChannelPacket::UpdateState {
                disabled: r.bool()?,
            },
            channel::SET_GROUP => {
                let group = r.uuid()?;
                let password = if r.bool()? { Some(r.utf(512)?) } else { None };
                ChannelPacket::JoinGroup { group, password }
            }
            channel::CREATE_GROUP => {
                let name = r.utf(24)?;
                let password = if r.bool()? { Some(r.utf(24)?) } else { None };
                let kind = r.i16()?;
                ChannelPacket::CreateGroup {
                    name,
                    password,
                    kind,
                }
            }
            channel::LEAVE_GROUP => ChannelPacket::LeaveGroup,
            other => return Err(format!("unsupported voicechat channel `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_round_trips_through_udp() {
        let secret = Secret([9u8; 16]);
        let packet = Packet::Mic {
            data: vec![1, 2, 3, 4],
            sequence: 42,
            whispering: true,
        };
        let datagram = packet.encode_server_udp(&secret);
        assert_eq!(datagram[0], MAGIC);
        let back = Packet::decode_server_udp(&datagram, &secret).unwrap();
        assert_eq!(back, packet);
    }

    #[test]
    fn a_client_envelope_carries_the_player_uuid() {
        let secret = Secret([9u8; 16]);
        let player = 0x1122334455667788u128;
        let packet = Packet::ConnectionCheck;
        let datagram = packet.encode_client_udp(player, &secret);
        assert_eq!(peek_player(&datagram), Some(player));
        assert_eq!(Packet::decode_client_udp(&datagram, &secret), Some(packet));
    }

    #[test]
    fn a_wrong_secret_cannot_decrypt() {
        let packet = Packet::ConnectionCheck;
        let datagram = packet.encode_server_udp(&Secret([1u8; 16]));
        assert!(Packet::decode_server_udp(&datagram, &Secret([2u8; 16])).is_none());
    }

    #[test]
    fn player_sound_round_trips_with_category_and_whisper() {
        let packet = Packet::PlayerSound {
            channel_id: 5,
            sender: 6,
            data: vec![9; 40],
            sequence: 7,
            whispering: true,
            distance: 24.0,
            category: Some("proximity".to_string()),
        };
        let mut w = Writer::new();
        w.u8(packet.type_byte());
        packet.write_body(&mut w);
        let mut r = Reader::new(w.as_slice());
        let kind = r.u8().unwrap();
        assert_eq!(Packet::read_body(kind, &mut r).unwrap(), packet);
    }

    #[test]
    fn request_secret_encodes_a_compatibility_version() {
        let p = ChannelPacket::RequestSecret {
            compatibility_version: 18,
        };
        assert_eq!(p.encode(), vec![0, 0, 0, 18]);
        assert_eq!(
            ChannelPacket::decode(channel::REQUEST_SECRET, &p.encode()).unwrap(),
            p
        );
    }

    #[test]
    fn secret_packet_matches_the_field_order() {
        let s = ChannelPacket::Secret(Box::new(SecretData {
            secret: Secret([0xab; 16]),
            server_port: 24454,
            player: 1,
            codec: 0,
            mtu: 1275,
            distance: 48.0,
            keep_alive: 1000,
            groups_enabled: true,
            voice_host: String::new(),
            allow_recording: false,
        }));
        let bytes = s.encode();
        assert_eq!(&bytes[..16], &[0xab; 16]);
        // i32 port big-endian after the secret.
        assert_eq!(&bytes[16..20], &24454i32.to_be_bytes());
    }

    #[test]
    fn create_group_round_trips() {
        let p = ChannelPacket::CreateGroup {
            name: "raid".to_string(),
            password: Some("pw".to_string()),
            kind: 2,
        };
        let decoded = ChannelPacket::decode(channel::CREATE_GROUP, &p.encode()).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn peek_player_reads_the_envelope_uuid() {
        let mut w = Writer::new();
        w.u8(MAGIC);
        w.uuid(0x1122334455667788);
        w.byte_array(&[0u8; 40]);
        assert_eq!(peek_player(w.as_slice()), Some(0x1122334455667788));
    }
}
