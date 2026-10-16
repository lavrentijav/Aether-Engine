//! Plasmo Voice (v2.x) server protocol.
//!
//! Pinned against the mod's source (`plasmoapp/plasmo-voice`): the TCP stream
//! rides the Minecraft plugin channel [`channel::TCP`] and the audio rides its
//! own UDP socket. Both use the same framing shape — a one-byte packet type for
//! TCP, or `[magic][type][secret UUID][timestamp]` for UDP — and Java's
//! `DataOutputStream` primitives (big-endian, `writeUTF` strings, bare 128-bit
//! UUIDs), which [`crate::bytes`] already provides.
//!
//! This module is the wire format and the config the client needs to start
//! talking; the socket loops live in the plugin.

use crate::bytes::{Reader, Writer};
use crate::crypto::{name_uuid, Cbc};

/// The plugin channel the TCP packets travel on.
pub mod channel {
    pub const TCP: &str = "plasmo:voice/v2";
    pub const SERVICE: &str = "plasmo:voice/v2/service";
    pub const FLAG: &str = "plasmo:voice/v2/installed";
}

/// The UDP magic number that filters out packets that are not Plasmo Voice.
pub const UDP_MAGIC: i32 = 0x4e90_04e9;
/// The largest audio payload either direction accepts.
pub const MAX_AUDIO: usize = 2048;

/// TCP packet ids, in `PacketTcpCodec` registration order.
pub mod tcp {
    pub const CONNECTION: u8 = 1;
    pub const PLAYER_INFO_REQUEST: u8 = 2;
    pub const CONFIG: u8 = 3;
    pub const CONFIG_PLAYER_INFO: u8 = 4;
    pub const LANGUAGE_REQUEST: u8 = 5;
    pub const LANGUAGE: u8 = 6;
    pub const PLAYER_LIST: u8 = 7;
    pub const PLAYER_INFO_UPDATE: u8 = 8;
    pub const PLAYER_DISCONNECT: u8 = 9;
    pub const PLAYER_INFO: u8 = 10;
    pub const PLAYER_STATE: u8 = 11;
    pub const PLAYER_AUDIO_END: u8 = 12;
    pub const PLAYER_ACTIVATION_DISTANCES: u8 = 13;
    pub const SOURCE_INFO_REQUEST: u8 = 15;
    pub const SOURCE_INFO: u8 = 16;
    pub const SOURCE_AUDIO_END: u8 = 18;
}

/// UDP packet ids.
pub mod udp {
    pub const PING: u8 = 1;
    pub const PLAYER_AUDIO: u8 = 2;
    pub const SOURCE_AUDIO: u8 = 3;
    pub const SELF_AUDIO_INFO: u8 = 4;
}

/// The proximity source line and activation names the server registers; their
/// ids are the MD5 name UUIDs both sides derive from the name.
pub const PROXIMITY: &str = "proximity";

/// The proximity source line id.
pub fn proximity_line_id() -> u128 {
    name_uuid(&format!("{PROXIMITY}_line"))
}

/// The proximity activation id.
pub fn proximity_activation_id() -> u128 {
    name_uuid(&format!("{PROXIMITY}_activation"))
}

/// A `VoicePlayerInfo`, the per-player record Plasmo broadcasts.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfo {
    pub id: u128,
    pub nick: String,
    pub muted: bool,
    pub voice_disabled: bool,
    pub microphone_muted: bool,
}

impl PlayerInfo {
    fn write(&self, w: &mut Writer) {
        w.uuid(self.id)
            .utf(&self.nick)
            .bool(self.muted)
            .bool(self.voice_disabled)
            .bool(self.microphone_muted);
    }
}

/// A codec description.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecInfo {
    pub name: String,
    pub params: Vec<(String, String)>,
}

impl CodecInfo {
    fn write(&self, w: &mut Writer) {
        w.utf(&self.name).i32(self.params.len() as i32);
        for (k, v) in &self.params {
            w.utf(k).utf(v);
        }
    }
}

/// A player audio source as the server describes it in a `SourceInfoPacket`.
///
/// The client learns a source id from an incoming `SourceAudioPacket`, then
/// asks for its details with `SourceInfoRequestPacket`; without this reply it
/// keeps the audio buffered and drops it after five seconds. The `id` is the
/// server's own handle for the source (PV picks a random UUID); the plugin uses
/// a stable [`crate::source_id_for`] so the same speaker always maps to the
/// same source.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSource {
    pub addon_id: String,
    pub id: u128,
    pub line_id: u128,
    pub name: Option<String>,
    /// The activation state byte; the client re-requests when it changes.
    pub state: u8,
    pub stereo: bool,
    pub icon_visible: bool,
    pub angle: i32,
    pub player: PlayerInfo,
}

impl PlayerSource {
    fn write(&self, w: &mut Writer) {
        w.utf("PLAYER");
        w.utf(&self.addon_id);
        w.uuid(self.id);
        w.bool(self.name.is_some());
        if let Some(n) = &self.name {
            w.utf(n);
        }
        w.u8(self.state);
        // decoderInfo: opus, so the client opens a matching decoder.
        w.bool(true);
        CodecInfo {
            name: "opus".to_string(),
            params: vec![
                ("mode".to_string(), "voip".to_string()),
                ("bitrate".to_string(), "32000".to_string()),
            ],
        }
        .write(w);
        w.bool(self.stereo);
        w.uuid(self.line_id);
        w.bool(self.icon_visible);
        w.i32(self.angle);
        self.player.write(w);
    }
}

/// The server config packet the client needs to start sending audio.
#[derive(Debug, Clone)]
pub struct Config {
    pub server_id: u128,
    pub sample_rate: i32,
    pub mtu: i32,
    pub opus_mode: String,
    pub opus_bitrate: String,
    /// The AES-128 key audio is protected with, if encryption is on.
    pub aes_key: Option<[u8; 16]>,
    pub default_distance: i32,
    pub max_distance: i32,
    /// Per-permission grants (usually empty).
    pub permissions: Vec<(String, bool)>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            server_id: 0,
            sample_rate: 48_000,
            mtu: 1024,
            opus_mode: "voip".to_string(),
            opus_bitrate: "32000".to_string(),
            aes_key: None,
            default_distance: 32,
            max_distance: 48,
            permissions: Vec::new(),
        }
    }
}

impl Config {
    /// Encode the `ConfigPacket` body (without the type byte).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.uuid(self.server_id);
        // CaptureInfo
        w.i32(self.sample_rate).i32(self.mtu).bool(true);
        CodecInfo {
            name: "opus".to_string(),
            params: vec![
                ("mode".to_string(), self.opus_mode.clone()),
                ("bitrate".to_string(), self.opus_bitrate.clone()),
            ],
        }
        .write(&mut w);
        // EncryptionInfo (RSA-encrypted AES key is done by the caller; here the
        // optional presence flag plus the raw key).
        match &self.aes_key {
            Some(key) => {
                w.bool(true);
                w.utf("AES/CBC/PKCS5Padding");
                w.i32(key.len() as i32).raw(key);
            }
            None => {
                w.bool(false);
            }
        }
        // One source line: proximity.
        w.i32(1);
        w.utf(PROXIMITY)
            .utf("source_line.proximity")
            .utf("minecraft:player");
        w.f64(1.0).i32(0).bool(false);
        // One activation: proximity.
        w.i32(1);
        w.utf(PROXIMITY)
            .utf("activation.proximity")
            .utf("minecraft:player");
        w.i32(2).i32(-1).i32(self.max_distance);
        w.i32(self.default_distance);
        w.bool(true).bool(false).bool(false).bool(false);
        w.i32(0);
        // Permissions (ConfigPlayerInfoPacket).
        w.i32(self.permissions.len() as i32);
        for (k, v) in &self.permissions {
            w.utf(k).bool(*v);
        }
        // PlayerIconConfig: no hidden icons, zero offset.
        w.i32(0);
        w.f64(0.0).f64(0.0).f64(0.0);
        w.into_vec()
    }
}

/// A server-to-client or client-to-server TCP packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Tcp {
    /// `ConnectionPacket`: the UDP secret, host and port.
    Connection { secret: u128, ip: String, port: i32 },
    /// `PlayerInfoRequestPacket`: sent to a client to make it identify itself.
    PlayerInfoRequest,
    /// `PlayerInfoPacket` (client): identity, versions and public key.
    PlayerInfo {
        voice_disabled: bool,
        microphone_muted: bool,
        minecraft_version: String,
        version: String,
        public_key: Vec<u8>,
    },
    /// `PlayerStatePacket` (client): mute flags.
    PlayerState {
        voice_disabled: bool,
        microphone_muted: bool,
    },
    /// `PlayerAudioEndPacket` (client): end of a speech segment.
    PlayerAudioEnd {
        sequence: i64,
        activation: u128,
        distance: i16,
    },
    /// `PlayerActivationDistancesPacket` (client): distance per activation.
    ActivationDistances { distances: Vec<(u128, i32)> },
    /// `SourceInfoRequestPacket` (client): ask for a source's details.
    SourceInfoRequest { source_id: u128 },
    /// `SourceInfoPacket` (server): the details of one player source.
    SourceInfo(Box<PlayerSource>),
    /// `LanguageRequestPacket` (client).
    LanguageRequest { language: String },
    /// `PlayerListPacket` (server).
    PlayerList { players: Vec<PlayerInfo> },
    /// `PlayerInfoUpdatePacket` (server).
    PlayerInfoUpdate { player: PlayerInfo },
    /// `PlayerDisconnectPacket` (server).
    PlayerDisconnect { id: u128 },
    /// `SourceAudioEndPacket` (server).
    SourceAudioEnd { source_id: u128, sequence: i64 },
}

impl Tcp {
    fn id(&self) -> u8 {
        match self {
            Tcp::Connection { .. } => tcp::CONNECTION,
            Tcp::PlayerInfoRequest => tcp::PLAYER_INFO_REQUEST,
            Tcp::PlayerInfo { .. } => tcp::PLAYER_INFO,
            Tcp::PlayerState { .. } => tcp::PLAYER_STATE,
            Tcp::PlayerAudioEnd { .. } => tcp::PLAYER_AUDIO_END,
            Tcp::ActivationDistances { .. } => tcp::PLAYER_ACTIVATION_DISTANCES,
            Tcp::SourceInfoRequest { .. } => tcp::SOURCE_INFO_REQUEST,
            Tcp::SourceInfo(_) => tcp::SOURCE_INFO,
            Tcp::LanguageRequest { .. } => tcp::LANGUAGE_REQUEST,
            Tcp::PlayerList { .. } => tcp::PLAYER_LIST,
            Tcp::PlayerInfoUpdate { .. } => tcp::PLAYER_INFO_UPDATE,
            Tcp::PlayerDisconnect { .. } => tcp::PLAYER_DISCONNECT,
            Tcp::SourceAudioEnd { .. } => tcp::SOURCE_AUDIO_END,
        }
    }

    /// Encode with the leading type byte, as the plugin channel carries it.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.id());
        match self {
            Tcp::Connection { secret, ip, port } => {
                w.uuid(*secret).utf(ip).i32(*port);
            }
            Tcp::PlayerInfoRequest => {}
            Tcp::PlayerInfo {
                voice_disabled,
                microphone_muted,
                minecraft_version,
                version,
                public_key,
            } => {
                w.bool(*voice_disabled)
                    .bool(*microphone_muted)
                    .utf(minecraft_version)
                    .utf(version)
                    .i32(public_key.len() as i32)
                    .raw(public_key);
            }
            Tcp::PlayerState {
                voice_disabled,
                microphone_muted,
            } => {
                w.bool(*voice_disabled).bool(*microphone_muted);
            }
            Tcp::PlayerAudioEnd {
                sequence,
                activation,
                distance,
            } => {
                w.i64(*sequence).uuid(*activation).i16(*distance);
            }
            Tcp::ActivationDistances { distances } => {
                w.i32(distances.len() as i32);
                for (id, d) in distances {
                    w.uuid(*id).i32(*d);
                }
            }
            Tcp::SourceInfoRequest { source_id } => {
                w.uuid(*source_id);
            }
            Tcp::SourceInfo(source) => source.write(&mut w),
            Tcp::LanguageRequest { language } => {
                w.utf(language);
            }
            Tcp::PlayerList { players } => {
                w.i32(players.len() as i32);
                for p in players {
                    p.write(&mut w);
                }
            }
            Tcp::PlayerInfoUpdate { player } => player.write(&mut w),
            Tcp::PlayerDisconnect { id } => {
                w.uuid(*id);
            }
            Tcp::SourceAudioEnd {
                source_id,
                sequence,
            } => {
                w.uuid(*source_id).i64(*sequence);
            }
        }
        w.into_vec()
    }

    /// Decode a client packet. Unknown or malformed ids are an error, never a
    /// panic.
    pub fn decode(bytes: &[u8]) -> Result<Tcp, String> {
        let mut r = Reader::new(bytes);
        let id = r.u8()?;
        Ok(match id {
            tcp::PLAYER_INFO => {
                let voice_disabled = r.bool()?;
                let microphone_muted = r.bool()?;
                let minecraft_version = r.utf(64)?;
                let version = r.utf(64)?;
                let len = r.i32()?;
                if len < 1 || len as usize > MAX_AUDIO {
                    return Err(format!("public key length {len} out of range"));
                }
                let public_key = r.take_n(len as usize)?;
                Tcp::PlayerInfo {
                    voice_disabled,
                    microphone_muted,
                    minecraft_version,
                    version,
                    public_key,
                }
            }
            tcp::PLAYER_STATE => Tcp::PlayerState {
                voice_disabled: r.bool()?,
                microphone_muted: r.bool()?,
            },
            tcp::PLAYER_AUDIO_END => Tcp::PlayerAudioEnd {
                sequence: r.i64()?,
                activation: r.uuid()?,
                distance: r.i16()?,
            },
            tcp::PLAYER_ACTIVATION_DISTANCES => {
                let count = r.i32()?;
                if !(0..=127).contains(&count) {
                    return Err(format!("activation count {count} out of range"));
                }
                let mut distances = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    distances.push((r.uuid()?, r.i32()?));
                }
                Tcp::ActivationDistances { distances }
            }
            tcp::SOURCE_INFO_REQUEST => Tcp::SourceInfoRequest {
                source_id: r.uuid()?,
            },
            tcp::LANGUAGE_REQUEST => Tcp::LanguageRequest {
                language: r.utf(32)?,
            },
            other => return Err(format!("unknown Plasmo TCP packet id {other}")),
        })
    }
}

/// A `PlayerAudioPacket`: Opus audio from one client, plus its activation.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerAudio {
    pub sequence: i64,
    pub data: Vec<u8>,
    pub activation: u128,
    pub distance: i16,
    pub stereo: bool,
}

/// A `SourceAudioPacket`: Opus audio forwarded to a listener.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceAudio {
    pub sequence: i64,
    pub data: Vec<u8>,
    pub source_id: u128,
    pub source_state: u8,
    pub distance: i16,
}

/// One decoded UDP datagram.
#[derive(Debug, Clone, PartialEq)]
pub enum Udp {
    /// `PingPacket`: the client's first UDP contact, carrying its secret.
    Ping {
        time: i64,
        server_ip: Option<String>,
        server_port: u16,
    },
    PlayerAudio(PlayerAudio),
    SourceAudio(SourceAudio),
}

/// The UDP source-state bits Plasmo sets on forwarded audio.
pub const SOURCE_STATE_WHISPERING: u8 = 0b1;

impl Udp {
    fn id(&self) -> u8 {
        match self {
            Udp::Ping { .. } => udp::PING,
            Udp::PlayerAudio(_) => udp::PLAYER_AUDIO,
            Udp::SourceAudio(_) => udp::SOURCE_AUDIO,
        }
    }

    /// Encode the datagram for `secret`.
    pub fn encode(&self, secret: u128) -> Vec<u8> {
        let mut w = Writer::new();
        w.i32(UDP_MAGIC).u8(self.id()).uuid(secret).i64(now_ms());
        match self {
            Udp::Ping {
                time,
                server_ip,
                server_port,
            } => {
                w.i64(*time);
                if let Some(ip) = server_ip {
                    w.utf(ip).u16(*server_port);
                }
            }
            Udp::PlayerAudio(a) => {
                w.i64(a.sequence).i32(a.data.len() as i32).raw(&a.data);
                w.uuid(a.activation).i16(a.distance).bool(a.stereo);
            }
            Udp::SourceAudio(a) => {
                w.i64(a.sequence).i32(a.data.len() as i32).raw(&a.data);
                w.uuid(a.source_id).u8(a.source_state).i16(a.distance);
            }
        }
        w.into_vec()
    }

    /// Decode the envelope, returning `(secret, packet)`. The secret is read
    /// before the body so a socket loop can look the player up.
    pub fn decode(bytes: &[u8]) -> Result<(u128, Udp), String> {
        let mut r = Reader::new(bytes);
        if r.i32()? != UDP_MAGIC {
            return Err("bad Plasmo UDP magic".to_string());
        }
        let id = r.u8()?;
        let secret = r.uuid()?;
        let _timestamp = r.i64()?;
        let packet = match id {
            udp::PING => {
                let time = r.i64()?;
                let server_ip = r.utf(255).ok();
                let server_port = if server_ip.is_some() {
                    r.u16().unwrap_or(0)
                } else {
                    0
                };
                Udp::Ping {
                    time,
                    server_ip,
                    server_port,
                }
            }
            udp::PLAYER_AUDIO => {
                let sequence = r.i64()?;
                let data = read_audio(&mut r)?;
                Udp::PlayerAudio(PlayerAudio {
                    sequence,
                    data,
                    activation: r.uuid()?,
                    distance: r.i16()?,
                    stereo: r.bool()?,
                })
            }
            udp::SOURCE_AUDIO => {
                let sequence = r.i64()?;
                let data = read_audio(&mut r)?;
                Udp::SourceAudio(SourceAudio {
                    sequence,
                    data,
                    source_id: r.uuid()?,
                    source_state: r.u8()?,
                    distance: r.i16()?,
                })
            }
            other => return Err(format!("unknown Plasmo UDP packet id {other}")),
        };
        Ok((secret, packet))
    }
}

fn read_audio(r: &mut Reader) -> Result<Vec<u8>, String> {
    let len = r.i32()?;
    if len < 1 || len as usize > MAX_AUDIO {
        return Err(format!("audio length {len} out of range"));
    }
    r.take_n(len as usize)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Decrypt a Plasmo audio payload with the server AES key (CBC, PKCS#5). The
/// client may send audio unencrypted when the server configured no key; in that
/// case the caller passes the bytes through unchanged and never calls this.
pub fn decrypt_audio(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    Cbc::new(key).decrypt(iv, data)
}

/// Encrypt a Plasmo audio payload with the server AES key.
pub fn encrypt_audio(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    Cbc::new(key).encrypt(iv, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proximity_ids_are_md5_name_uuids() {
        // The id is stable — it is what the client derives from the name, so it
        // must never change by accident.
        assert_eq!(
            proximity_activation_id(),
            name_uuid("proximity_activation"),
            "activation id"
        );
        assert_ne!(proximity_line_id(), proximity_activation_id());
    }

    #[test]
    fn player_info_round_trips() {
        let info = PlayerInfo {
            id: 0xdead_beef,
            nick: "Steve".to_string(),
            muted: false,
            voice_disabled: true,
            microphone_muted: false,
        };
        let tcp = Tcp::PlayerInfoUpdate {
            player: info.clone(),
        };
        let bytes = tcp.encode();
        assert_eq!(bytes[0], tcp::PLAYER_INFO_UPDATE);
        // The server never decodes its own update packet, but the body layout is
        // pinned by reading it back field by field.
        let mut r = Reader::new(&bytes[1..]);
        assert_eq!(r.uuid().unwrap(), info.id);
        assert_eq!(r.utf(64).unwrap(), "Steve");
        assert!(!r.bool().unwrap());
        assert!(r.bool().unwrap());
        assert!(!r.bool().unwrap());
    }

    #[test]
    fn client_player_info_decodes() {
        let tcp = Tcp::PlayerInfo {
            voice_disabled: false,
            microphone_muted: true,
            minecraft_version: "1.21.11".to_string(),
            version: "2.1.7".to_string(),
            public_key: vec![1, 2, 3, 4, 5],
        };
        // Re-encode as a client would (the server's encode is identical for
        // this packet) and decode it.
        let bytes = tcp.encode();
        assert_eq!(Tcp::decode(&bytes).unwrap(), tcp);
    }

    #[test]
    fn player_audio_round_trips_through_udp() {
        let packet = Udp::PlayerAudio(PlayerAudio {
            sequence: 7,
            data: vec![9; 80],
            activation: proximity_activation_id(),
            distance: 32,
            stereo: false,
        });
        let secret = 0x1234_5678_9abc_def0u128;
        let bytes = packet.encode(secret);
        let (got_secret, decoded) = Udp::decode(&bytes).unwrap();
        assert_eq!(got_secret, secret);
        assert_eq!(decoded, packet);
    }

    #[test]
    fn source_audio_re_stamps_the_secret() {
        let packet = Udp::SourceAudio(SourceAudio {
            sequence: 1,
            data: vec![1, 2, 3],
            source_id: proximity_activation_id(),
            source_state: SOURCE_STATE_WHISPERING,
            distance: 16,
        });
        let bytes = packet.encode(0xaa);
        // `replaceSecret` rewrites the 16 bytes after the magic + type.
        let mut patched = bytes.clone();
        patched[5..21].copy_from_slice(&0xbbu128.to_be_bytes());
        let (secret, decoded) = Udp::decode(&patched).unwrap();
        assert_eq!(secret, 0xbb);
        assert_eq!(decoded, packet);
    }

    #[test]
    fn a_bad_magic_is_rejected() {
        assert!(Udp::decode(&[0, 0, 0, 0, 1, 0, 0]).is_err());
    }

    #[test]
    fn config_encodes_a_proximity_line_and_activation() {
        let bytes = Config::default().encode();
        // Parse the fixed prefix: server UUID, sample rate, mtu, codec-present.
        let mut r = Reader::new(&bytes);
        let _server = r.uuid().unwrap();
        assert_eq!(r.i32().unwrap(), 48_000);
        assert_eq!(r.i32().unwrap(), 1024);
        assert!(r.bool().unwrap());
        assert_eq!(r.utf(64).unwrap(), "opus");
        let params = r.i32().unwrap();
        assert_eq!(params, 2);
        assert_eq!(r.utf(64).unwrap(), "mode");
        assert_eq!(r.utf(64).unwrap(), "voip");
    }
}
