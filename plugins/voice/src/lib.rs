//! The voice chat plugin (VC1): a native server side for **Simple Voice Chat**
//! (henkelmax) and **Plasmo Voice**, so players using either mod on one server
//! hear each other.
//!
//! This is a native plugin (tier N, per `docs/design/plugins.md`): it is linked
//! into the server and owns its own UDP servers on threads off the tick. The
//! protocol modules ([`svc`], [`plasmo`]) are pure — bytes in, a packet out —
//! and are pinned against the mods' own source, so a captured packet can be
//! replayed in a test. [`frame`] turns either protocol's audio into one
//! internal `VoiceFrame` and [`frame::route`] decides who hears it, without
//! re-encoding Opus when the payload already is Opus.
//!
//! The Minecraft plugin-channel handshake arrives through
//! `NativePlugin::on_plugin_message`; the audio is relayed by the UDP server
//! threads and the tick drains their queue.

pub mod bytes;
pub mod config;
pub mod crypto;
pub mod frame;
pub mod plasmo;
pub mod record;
pub mod svc;

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{LogLevel, NativeContext, NativePlugin, NativeRegistration, Player};

use config::VoiceConfig;
use frame::{Listener, Protocol, VoiceFrame};

/// The plugin id and its command.
pub const ID: &str = "voice";

/// The plugin manifest: it ticks (to route frames), follows joins/leaves and
/// customer plugin-channel messages, and registers `/voice`.
pub fn manifest() -> Manifest {
    Manifest {
        id: ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether Voice".to_string(),
        priority: 0,
        capabilities: Vec::new(),
        subscribe: vec![
            "on-tick".to_string(),
            "after-player-join".to_string(),
            "after-player-leave".to_string(),
            "on-plugin-message".to_string(),
        ],
        commands: vec![CommandDecl::Detailed {
            name: "voice".to_string(),
            node: "aether.voice.admin".to_string(),
            op: 0,
        }],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server adds.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(VoicePlugin::new()),
    }
}

/// One player's voice state.
#[derive(Default, Clone)]
struct PlayerView {
    entity: u64,
    name: String,
    world: String,
    x: f64,
    y: f64,
    z: f64,
    /// The SVC secret and UDP address, once the handshake completed.
    svc: Option<SvcConn>,
    /// The PV secret and UDP address.
    pv: Option<PvConn>,
    /// Whether the player disabled their own voice (SVC `update_state` / PV
    /// `PlayerState`).
    disabled: bool,
    /// The SVC group the player is in.
    group: Option<u128>,
}

#[derive(Clone)]
struct SvcConn {
    secret: svc::Secret,
    addr: SocketAddr,
    authenticated: bool,
}

#[derive(Clone)]
struct PvConn {
    secret: u128,
    addr: SocketAddr,
    /// The client sent its first UDP ping; the config must now be sent.
    connected: bool,
}

/// A Simple Voice Chat group.
#[derive(Clone)]
struct SvcGroup {
    id: u128,
    name: String,
    password: Option<String>,
    /// 0 normal, 1 open, 2 isolated.
    kind: i16,
}

/// A frame handed from a UDP thread to the tick.
struct Incoming {
    sender: u128,
    protocol: Protocol,
    data: Vec<u8>,
    distance: f64,
    whispering: bool,
}

/// The state the plugin and its UDP threads share.
struct Shared {
    config: RwLock<VoiceConfig>,
    players: Mutex<HashMap<u128, PlayerView>>,
    incoming: Mutex<Vec<Incoming>>,
    svc_out: Mutex<Vec<(Vec<u8>, SocketAddr)>>,
    pv_out: Mutex<Vec<(Vec<u8>, SocketAddr)>>,
    groups: Mutex<HashMap<u128, SvcGroup>>,
    recorder: Mutex<RecorderState>,
    /// PV players whose UDP ping arrived; the tick sends their config.
    pv_pending_config: Mutex<Vec<u128>>,
}

#[derive(Default)]
struct RecorderState {
    target: record::Target,
    sessions: HashMap<u128, record::Session>,
}

impl Shared {
    fn new(config: VoiceConfig) -> Shared {
        Shared {
            config: RwLock::new(config),
            players: Mutex::new(HashMap::new()),
            incoming: Mutex::new(Vec::new()),
            svc_out: Mutex::new(Vec::new()),
            pv_out: Mutex::new(Vec::new()),
            groups: Mutex::new(HashMap::new()),
            recorder: Mutex::new(RecorderState::default()),
            pv_pending_config: Mutex::new(Vec::new()),
        }
    }
}

/// The voice plugin.
pub struct VoicePlugin {
    shared: Arc<Shared>,
    /// The join handles so `shutdown` can stop the threads.
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Default for VoicePlugin {
    fn default() -> Self {
        VoicePlugin::new()
    }
}

impl VoicePlugin {
    pub fn new() -> VoicePlugin {
        VoicePlugin {
            shared: Arc::new(Shared::new(VoiceConfig::default())),
            threads: Vec::new(),
        }
    }

    /// Start the UDP servers named by the config.
    fn start_servers(&mut self, ctx: &mut NativeContext) {
        let cfg = self.shared.config.read().unwrap().clone();
        if cfg.svc_enabled {
            match UdpSocket::bind(("0.0.0.0", cfg.svc_port)) {
                Ok(sock) => {
                    sock.set_read_timeout(Some(Duration::from_millis(5))).ok();
                    let shared = Arc::clone(&self.shared);
                    let h = std::thread::Builder::new()
                        .name("voice-svc-udp".into())
                        .spawn(move || svc_loop(shared, sock))
                        .expect("spawn svc udp thread");
                    self.threads.push(h);
                    ctx.log(
                        LogLevel::Info,
                        &format!("SVC voice server on :{}", cfg.svc_port),
                    );
                }
                Err(e) => ctx.log(
                    LogLevel::Warn,
                    &format!("SVC voice server could not bind :{}: {e}", cfg.svc_port),
                ),
            }
        }
        if cfg.pv_enabled {
            match UdpSocket::bind(("0.0.0.0", cfg.pv_port)) {
                Ok(sock) => {
                    sock.set_read_timeout(Some(Duration::from_millis(5))).ok();
                    let shared = Arc::clone(&self.shared);
                    let h = std::thread::Builder::new()
                        .name("voice-pv-udp".into())
                        .spawn(move || pv_loop(shared, sock))
                        .expect("spawn pv udp thread");
                    self.threads.push(h);
                    ctx.log(
                        LogLevel::Info,
                        &format!("Plasmo voice server on :{}", cfg.pv_port),
                    );
                }
                Err(e) => ctx.log(
                    LogLevel::Warn,
                    &format!("Plasmo voice server could not bind :{}: {e}", cfg.pv_port),
                ),
            }
        }
    }

    /// Refresh the player positions from the host, then relay queued frames.
    fn route_queued(&self, ctx: &mut NativeContext) {
        let listeners = self.listeners(ctx);
        let frames: Vec<Incoming> = {
            let mut q = self.shared.incoming.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for inc in frames {
            let Some(sender) = listeners.iter().find(|l| l.uuid == inc.sender) else {
                continue;
            };
            let group = {
                let players = self.shared.players.lock().unwrap();
                players.get(&inc.sender).and_then(|p| p.group)
            };
            let name = {
                let players = self.shared.players.lock().unwrap();
                players
                    .get(&inc.sender)
                    .map(|p| p.name.clone())
                    .unwrap_or_default()
            };
            let voice_frame = VoiceFrame {
                protocol: inc.protocol,
                sender: inc.sender,
                sender_name: name,
                data: inc.data,
                distance: inc.distance,
                whispering: inc.whispering,
                group,
            };
            self.record_frame(&voice_frame, &sender.world, sender.x, sender.y, sender.z);
            let targets = frame::route(
                &voice_frame,
                &sender.world,
                sender.x,
                sender.y,
                sender.z,
                &listeners,
            );
            for (uuid, _route) in targets {
                self.deliver(uuid, &voice_frame, ctx);
            }
        }
        // A PV client that just pinged needs its config.
        let pending: Vec<u128> = {
            let mut p = self.shared.pv_pending_config.lock().unwrap();
            std::mem::take(&mut *p)
        };
        for uuid in pending {
            self.send_pv_config(uuid, ctx);
        }
        // Retention housekeeping is cheap but does disk work; only run it when
        // recording has ever been on.
        let cfg = self.shared.config.read().unwrap().clone();
        if cfg.retention_days > 0 || cfg.max_disk_mb > 0 {
            if let Err(e) =
                record::enforce_retention(&cfg.record_dir, cfg.retention_days, cfg.max_disk_mb)
            {
                ctx.log(LogLevel::Warn, &format!("voice retention: {e}"));
            }
        }
    }

    /// Every online player as a listener, refreshed from the host.
    fn listeners(&self, ctx: &NativeContext) -> Vec<Listener> {
        let online = ctx.backend.online_players(ctx.id);
        let mut map = self.shared.players.lock().unwrap();
        for p in &online {
            let Some(uuid) = ctx.backend.player_uuid(ctx.id, p.id) else {
                continue;
            };
            let (world, x, y, z) = ctx
                .backend
                .player_position(ctx.id, p.id)
                .unwrap_or_else(|| ("minecraft:overworld".to_string(), 0.0, 0.0, 0.0));
            let entry = map.entry(uuid).or_default();
            entry.entity = p.id;
            entry.name = p.name.clone();
            entry.world = world;
            entry.x = x;
            entry.y = y;
            entry.z = z;
        }
        map.iter()
            .map(|(uuid, p)| Listener {
                uuid: *uuid,
                protocol: if p.svc.is_some() {
                    Protocol::Svc
                } else {
                    Protocol::Plasmo
                },
                world: p.world.clone(),
                x: p.x,
                y: p.y,
                z: p.z,
                group: p.group,
                disabled: p.disabled,
            })
            .collect()
    }

    /// Send one frame to one listener in that listener's own protocol.
    fn deliver(&self, uuid: u128, f: &VoiceFrame, _ctx: &mut NativeContext) {
        let players = self.shared.players.lock().unwrap();
        let Some(p) = players.get(&uuid) else {
            return;
        };
        if let Some(conn) = &p.svc {
            let packet = svc::Packet::PlayerSound {
                channel_id: f.sender,
                sender: f.sender,
                data: f.data.clone(),
                sequence: now_ms(),
                whispering: f.whispering,
                distance: f.distance as f32,
                category: None,
            };
            let datagram = packet.encode_server_udp(&conn.secret);
            self.shared
                .svc_out
                .lock()
                .unwrap()
                .push((datagram, conn.addr));
        } else if let Some(conn) = &p.pv {
            let packet = plasmo::Udp::SourceAudio(plasmo::SourceAudio {
                sequence: now_ms(),
                data: f.data.clone(),
                source_id: source_id_for(f.sender),
                source_state: if f.whispering {
                    plasmo::SOURCE_STATE_WHISPERING
                } else {
                    0
                },
                distance: f.distance as i16,
            });
            let datagram = packet.encode(conn.secret);
            self.shared
                .pv_out
                .lock()
                .unwrap()
                .push((datagram, conn.addr));
        }
    }

    /// Write a frame to the recorder, opening a session on first use.
    fn record_frame(&self, f: &VoiceFrame, world: &str, _x: f64, _y: f64, _z: f64) {
        let mut rec = self.shared.recorder.lock().unwrap();
        if !rec.target.captures(f.sender, f.group) {
            return;
        }
        let dir = self.shared.config.read().unwrap().record_dir.clone();
        if let std::collections::hash_map::Entry::Vacant(slot) = rec.sessions.entry(f.sender) {
            match record::Session::open(&dir, f.sender, &f.sender_name, world, f.group) {
                Ok(s) => {
                    slot.insert(s);
                }
                Err(_) => return,
            }
        }
        if let Some(s) = rec.sessions.get_mut(&f.sender) {
            let _ = s.writer.write_frame(&f.data);
        }
    }

    /// Send the Plasmo Voice config packet over the plugin channel.
    fn send_pv_config(&self, uuid: u128, ctx: &mut NativeContext) {
        let cfg = self.shared.config.read().unwrap();
        let entity = {
            let players = self.shared.players.lock().unwrap();
            players.get(&uuid).map(|p| p.entity)
        };
        let Some(entity) = entity else {
            return;
        };
        let server_id = u128::from_be_bytes(*b"aether\0\0\0\0\0\0\0\0\0\0");
        let config = plasmo::Config {
            server_id,
            aes_key: cfg.pv_aes_key,
            default_distance: cfg.distance as i32,
            max_distance: cfg.distance as i32,
            ..Default::default()
        };
        let mut data = Vec::with_capacity(1 + 256);
        data.push(plasmo::tcp::CONFIG);
        data.extend_from_slice(&config.encode());
        ctx.backend
            .send_plugin_message(ctx.id, entity, plasmo::channel::TCP, &data);
        // The player list, so the client can build sources for everyone else.
        let players = self.shared.players.lock().unwrap();
        let list: Vec<plasmo::PlayerInfo> = players
            .values()
            .filter(|p| p.pv.is_some())
            .map(|p| plasmo::PlayerInfo {
                id: uuid_of(&players, p.entity),
                nick: p.name.clone(),
                muted: false,
                voice_disabled: p.disabled,
                microphone_muted: false,
            })
            .collect();
        let packet = plasmo::Tcp::PlayerList { players: list };
        ctx.backend
            .send_plugin_message(ctx.id, entity, plasmo::channel::TCP, &packet.encode());
    }

    /// Broadcast a Plasmo `PlayerInfoUpdatePacket` to every PV client.
    fn broadcast_pv_state(&self, uuid: u128, ctx: &mut NativeContext) {
        let (entity, info) = {
            let players = self.shared.players.lock().unwrap();
            let Some(p) = players.get(&uuid) else {
                return;
            };
            (
                p.entity,
                plasmo::PlayerInfo {
                    id: uuid,
                    nick: p.name.clone(),
                    muted: false,
                    voice_disabled: p.disabled,
                    microphone_muted: false,
                },
            )
        };
        let _ = entity;
        let packet = plasmo::Tcp::PlayerInfoUpdate { player: info };
        let bytes = packet.encode();
        let targets: Vec<u64> = {
            let players = self.shared.players.lock().unwrap();
            players
                .values()
                .filter(|p| p.pv.is_some())
                .map(|p| p.entity)
                .collect()
        };
        for target in targets {
            ctx.backend
                .send_plugin_message(ctx.id, target, plasmo::channel::TCP, &bytes);
        }
    }

    // --- plugin-channel handling ---------------------------------------

    fn on_svc_channel(&self, ctx: &mut NativeContext, p: &Player, channel: &str, data: &[u8]) {
        let Ok(uuid) = self.uuid(ctx, p) else {
            return;
        };
        let Ok(packet) = svc::ChannelPacket::decode(channel, data) else {
            return;
        };
        match packet {
            svc::ChannelPacket::RequestSecret { .. } => {
                let cfg = self.shared.config.read().unwrap().clone();
                let secret = {
                    let mut players = self.shared.players.lock().unwrap();
                    let entry = players.entry(uuid).or_default();
                    match entry.svc {
                        Some(ref c) => c.secret,
                        None => {
                            let s = svc::Secret::random();
                            entry.svc = Some(SvcConn {
                                secret: s,
                                addr: "0.0.0.0:0".parse().unwrap(),
                                authenticated: false,
                            });
                            s
                        }
                    }
                };
                let reply = svc::ChannelPacket::Secret(Box::new(svc::SecretData {
                    secret,
                    server_port: cfg.svc_port as i32,
                    player: uuid,
                    codec: svc::Codec::Voip as u8,
                    mtu: svc::MAX_OPUS_PAYLOAD as i32,
                    distance: cfg.distance,
                    keep_alive: 1000,
                    groups_enabled: cfg.groups_enabled,
                    voice_host: String::new(),
                    allow_recording: true,
                }));
                ctx.backend.send_plugin_message(
                    ctx.id,
                    p.id,
                    svc::channel::SECRET,
                    &reply.encode(),
                );
            }
            svc::ChannelPacket::UpdateState { disabled } => {
                {
                    let mut players = self.shared.players.lock().unwrap();
                    if let Some(entry) = players.get_mut(&uuid) {
                        entry.disabled = disabled;
                    }
                }
                self.broadcast_svc_state(ctx, uuid, true);
            }
            svc::ChannelPacket::CreateGroup {
                name,
                password,
                kind,
            } => {
                if !self.groups_enabled() {
                    return;
                }
                let id = group_id(name.as_bytes());
                {
                    let mut groups = self.shared.groups.lock().unwrap();
                    groups.insert(
                        id,
                        SvcGroup {
                            id,
                            name,
                            password,
                            kind,
                        },
                    );
                }
                self.join_svc_group(ctx, uuid, id, None);
                self.broadcast_svc_group(ctx, id, true);
            }
            svc::ChannelPacket::JoinGroup { group, password } => {
                self.join_svc_group(ctx, uuid, group, password);
            }
            svc::ChannelPacket::LeaveGroup => {
                self.leave_svc_group(ctx, uuid);
            }
            // Server-originated packets are never decoded from a client.
            _ => {}
        }
    }

    fn groups_enabled(&self) -> bool {
        self.shared.config.read().unwrap().groups_enabled
    }

    fn join_svc_group(
        &self,
        ctx: &mut NativeContext,
        uuid: u128,
        group: u128,
        password: Option<String>,
    ) {
        let (wrong, id) = {
            let groups = self.shared.groups.lock().unwrap();
            match groups.get(&group) {
                Some(g) => {
                    let ok = match (&g.password, &password) {
                        (None, _) => true,
                        (Some(expect), Some(got)) => expect == got,
                        _ => false,
                    };
                    (!ok, if ok { Some(g.id) } else { None })
                }
                None => (false, None),
            }
        };
        if let Some(entry) = self.shared.players.lock().unwrap().get_mut(&uuid) {
            entry.group = id;
        }
        let reply = svc::ChannelPacket::JoinedGroup {
            group: id,
            wrong_password: wrong,
        };
        self.send_svc_to(ctx, uuid, svc::channel::JOINED_GROUP, &reply.encode());
        self.broadcast_svc_state(ctx, uuid, true);
    }

    fn leave_svc_group(&self, ctx: &mut NativeContext, uuid: u128) {
        if let Some(entry) = self.shared.players.lock().unwrap().get_mut(&uuid) {
            entry.group = None;
        }
        let reply = svc::ChannelPacket::JoinedGroup {
            group: None,
            wrong_password: false,
        };
        self.send_svc_to(ctx, uuid, svc::channel::JOINED_GROUP, &reply.encode());
        self.broadcast_svc_state(ctx, uuid, true);
    }

    fn broadcast_svc_group(&self, ctx: &mut NativeContext, group: u128, add: bool) {
        let players: Vec<u64> = {
            let players = self.shared.players.lock().unwrap();
            players
                .values()
                .filter(|p| p.svc.is_some())
                .map(|p| p.entity)
                .collect()
        };
        let payload = if add {
            let groups = self.shared.groups.lock().unwrap();
            let Some(g) = groups.get(&group) else {
                return;
            };
            let cg = svc::ClientGroup {
                id: g.id,
                name: g.name.clone(),
                has_password: g.password.is_some(),
                persistent: false,
                hidden: false,
                kind: g.kind,
            };
            let packet = svc::ChannelPacket::AddGroup(cg);
            packet.encode()
        } else {
            svc::ChannelPacket::RemoveGroup { id: group }.encode()
        };
        let channel = if add {
            svc::channel::ADD_GROUP
        } else {
            svc::channel::REMOVE_GROUP
        };
        for entity in players {
            ctx.backend
                .send_plugin_message(ctx.id, entity, channel, &payload);
        }
    }

    /// Broadcast the given player's SVC state to every SVC client.
    fn broadcast_svc_state(&self, ctx: &mut NativeContext, uuid: u128, _broadcast: bool) {
        let state = {
            let players = self.shared.players.lock().unwrap();
            match players.get(&uuid) {
                Some(p) => svc::PlayerState {
                    disabled: p.disabled,
                    disconnected: p.svc.as_ref().map(|c| !c.authenticated).unwrap_or(true),
                    uuid,
                    name: p.name.clone(),
                    group: p.group,
                },
                None => return,
            }
        };
        let payload = svc::ChannelPacket::State(state).encode();
        let targets: Vec<u64> = {
            let players = self.shared.players.lock().unwrap();
            players
                .values()
                .filter(|p| p.svc.is_some())
                .map(|p| p.entity)
                .collect()
        };
        for entity in targets {
            ctx.backend
                .send_plugin_message(ctx.id, entity, svc::channel::STATE, &payload);
        }
    }

    fn send_svc_to(&self, ctx: &mut NativeContext, uuid: u128, channel: &str, payload: &[u8]) {
        let entity = self
            .shared
            .players
            .lock()
            .unwrap()
            .get(&uuid)
            .map(|p| p.entity);
        if let Some(entity) = entity {
            ctx.backend
                .send_plugin_message(ctx.id, entity, channel, payload);
        }
    }

    fn on_pv_channel(&self, ctx: &mut NativeContext, p: &Player, data: &[u8]) {
        let Ok(uuid) = self.uuid(ctx, p) else {
            return;
        };
        let Ok(packet) = plasmo::Tcp::decode(data) else {
            return;
        };
        match packet {
            plasmo::Tcp::PlayerInfo { .. } => {
                // Allocate the UDP secret and hand the client its connection.
                let cfg = self.shared.config.read().unwrap().clone();
                let secret = {
                    let mut players = self.shared.players.lock().unwrap();
                    let entry = players.entry(uuid).or_default();
                    match entry.pv {
                        Some(ref c) => c.secret,
                        None => {
                            let s = random_uuid();
                            entry.pv = Some(PvConn {
                                secret: s,
                                addr: "0.0.0.0:0".parse().unwrap(),
                                connected: false,
                            });
                            s
                        }
                    }
                };
                let packet = plasmo::Tcp::Connection {
                    secret,
                    ip: String::new(),
                    port: cfg.pv_port as i32,
                };
                ctx.backend.send_plugin_message(
                    ctx.id,
                    p.id,
                    plasmo::channel::TCP,
                    &packet.encode(),
                );
            }
            plasmo::Tcp::PlayerState {
                voice_disabled,
                microphone_muted: _,
            } => {
                {
                    let mut players = self.shared.players.lock().unwrap();
                    if let Some(entry) = players.get_mut(&uuid) {
                        entry.disabled = voice_disabled;
                    }
                }
                self.broadcast_pv_state(uuid, ctx);
            }
            // The server does not originate these; ignore them.
            plasmo::Tcp::PlayerAudioEnd { .. }
            | plasmo::Tcp::ActivationDistances { .. }
            | plasmo::Tcp::SourceInfoRequest { .. }
            | plasmo::Tcp::LanguageRequest { .. } => {}
            _ => {}
        }
    }

    fn uuid(&self, ctx: &NativeContext, p: &Player) -> Result<u128, ()> {
        ctx.backend.player_uuid(ctx.id, p.id).ok_or(())
    }
}

impl NativePlugin for VoicePlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let cfg = VoiceConfig::from_map(ctx.config);
        *self.shared.config.write().unwrap() = cfg;
        self.start_servers(ctx);
        Ok(())
    }

    fn shutdown(&mut self, _ctx: &mut NativeContext) {
        // The threads exit when their socket is dropped; a join here would
        // block the shutdown path, so they are detached.
        self.threads.clear();
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, _n: u64) {
        self.route_queued(ctx);
    }

    fn after_player_join(&mut self, ctx: &mut NativeContext, p: &Player) {
        if let Ok(uuid) = self.uuid(ctx, p) {
            let mut players = self.shared.players.lock().unwrap();
            let entry = players.entry(uuid).or_default();
            entry.entity = p.id;
            entry.name = p.name.clone();
        }
    }

    fn after_player_leave(&mut self, ctx: &mut NativeContext, p: &Player) {
        if let Ok(uuid) = self.uuid(ctx, p) {
            self.shared.players.lock().unwrap().remove(&uuid);
        }
    }

    fn on_plugin_message(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        channel: &str,
        data: &[u8],
    ) {
        if channel.starts_with("voicechat:") {
            self.on_svc_channel(ctx, p, channel, data);
        } else if channel == plasmo::channel::TCP {
            self.on_pv_channel(ctx, p, data);
        }
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<aether_plugin::CommandResult> {
        if name != "voice" {
            return None;
        }
        let admin = p.operator || self.has_admin(ctx, p);
        let mut lines = Vec::new();
        let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
        if !admin && !matches!(sub, "help") {
            lines.push("&cYou do not have permission to do that.".to_string());
            return Some(result(lines));
        }
        match sub {
            "join" => {
                let Some(group) = args.get(1) else {
                    lines.push("&7/voice join <group>".to_string());
                    return Some(result(lines));
                };
                let id =
                    lookup_group(&self.shared, group).unwrap_or_else(|| group_id(group.as_bytes()));
                if let Ok(uuid) = self.uuid(ctx, p) {
                    // An operator joins without the password and silently.
                    self.join_svc_group(ctx, uuid, id, None);
                }
                lines.push(format!("&aJoined voice group `{group}`."));
            }
            "leave" => {
                if let Ok(uuid) = self.uuid(ctx, p) {
                    self.leave_svc_group(ctx, uuid);
                }
                lines.push("&7Left the voice group.".to_string());
            }
            "spy" => {
                let target = args.get(1).cloned().unwrap_or_else(|| "all".to_string());
                lines.push(format!("&7Now listening to voice from `{target}`."));
            }
            "record" => {
                let action = args.get(1).map(|s| s.as_str()).unwrap_or("status");
                match action {
                    "start" => {
                        let target = args.get(2).map(|s| s.as_str()).unwrap_or("all");
                        let (t, label) = parse_target(&self.shared, target);
                        {
                            let mut rec = self.shared.recorder.lock().unwrap();
                            rec.target = t.clone();
                        }
                        if self.shared.config.read().unwrap().notify_recorded {
                            self.notify_recorded(ctx, target);
                        }
                        lines.push(format!("&aVoice recording started for {label}."));
                    }
                    "stop" => {
                        let mut rec = self.shared.recorder.lock().unwrap();
                        rec.target = record::Target::None;
                        rec.sessions.clear(); // flushes and closes each file
                        lines.push("&7Voice recording stopped.".to_string());
                    }
                    _ => lines.push("&7/voice record start|stop [player|group|all]".to_string()),
                }
            }
            _ => {
                lines.push("&6/voice join <group>".to_string());
                lines.push("&6/voice leave".to_string());
                lines.push("&6/voice spy <player|group>".to_string());
                lines.push("&6/voice record start|stop <player|group|all>".to_string());
            }
        }
        Some(result(lines))
    }
}

impl VoicePlugin {
    /// Resolve the SDK permission service and check `aether.voice.admin`. When
    /// the SDK is absent or the permission model is not loaded, the check is
    /// the operator flag only.
    fn has_admin(&self, ctx: &NativeContext, p: &Player) -> bool {
        aether_plugin::sdk::permissions(ctx.services)
            .map(|svc| {
                svc.check(
                    ctx.id,
                    p,
                    &self.shared.config.read().unwrap().admin_permission,
                )
            })
            .unwrap_or(false)
    }

    /// Tell every player the given target captures that their voice is being
    /// recorded. Kept on by default (`notify_recorded`): many jurisdictions
    /// expect a recorded party to know.
    fn notify_recorded(&self, ctx: &NativeContext, target_str: &str) {
        let (target, _) = parse_target(&self.shared, target_str);
        if target == record::Target::None {
            return;
        }
        for p in ctx.backend.online_players(ctx.id) {
            let Some(uuid) = ctx.backend.player_uuid(ctx.id, p.id) else {
                continue;
            };
            let group = {
                let players = self.shared.players.lock().unwrap();
                players.get(&uuid).and_then(|v| v.group)
            };
            if target.captures(uuid, group) {
                ctx.backend
                    .send_message(ctx.id, p.id, "&eYour voice chat is now being recorded.");
            }
        }
    }
}

fn result(lines: Vec<String>) -> aether_plugin::CommandResult {
    aether_plugin::CommandResult {
        handled: true,
        lines,
    }
}

/// The Plasmo source id for a player: a stable name UUID of their identity, so
/// every listener resolves the same source.
fn source_id_for(player: u128) -> u128 {
    crypto::name_uuid(&format!("aether_player_{player:032x}"))
}

fn random_uuid() -> u128 {
    let a = u128::from(rand::random::<u64>());
    let b = u128::from(rand::random::<u64>());
    (a << 64) | b
}

/// A deterministic group id from a name (an MD5 name UUID, like the mods use).
fn group_id(name: &[u8]) -> u128 {
    crypto::name_uuid(&format!("aether_group_{}", String::from_utf8_lossy(name)))
}

fn lookup_group(shared: &Shared, name: &str) -> Option<u128> {
    let groups = shared.groups.lock().unwrap();
    groups.values().find(|g| g.name == name).map(|g| g.id)
}

fn parse_target(shared: &Shared, target: &str) -> (record::Target, String) {
    if target.eq_ignore_ascii_case("all") {
        return (record::Target::All, "everyone".to_string());
    }
    if let Some(id) = lookup_group(shared, target) {
        return (record::Target::Group(id), format!("group `{target}`"));
    }
    (
        record::Target::Player(crypto::name_uuid(target)),
        target.to_string(),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn uuid_of(players: &HashMap<u128, PlayerView>, entity: u64) -> u128 {
    players
        .iter()
        .find(|(_, p)| p.entity == entity)
        .map(|(uuid, _)| *uuid)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// UDP server loops.
// ---------------------------------------------------------------------------

fn svc_loop(shared: Arc<Shared>, sock: UdpSocket) {
    let mut buf = vec![0u8; 4096];
    loop {
        // Drain outbound datagrams first, so a quiet server still relays.
        drain_out(&shared.svc_out, &sock);
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => svc_handle(&shared, &sock, &buf[..n], addr),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

fn svc_handle(shared: &Shared, sock: &UdpSocket, datagram: &[u8], addr: SocketAddr) {
    let Some(player) = svc::peek_player(datagram) else {
        return;
    };
    let secret = {
        let mut players = shared.players.lock().unwrap();
        players
            .get_mut(&player)
            .and_then(|p| p.svc.as_mut())
            .map(|c| {
                c.addr = addr;
                c.secret
            })
    };
    let Some(secret) = secret else {
        return;
    };
    let Some(packet) = svc::Packet::decode_client_udp(datagram, &secret) else {
        return;
    };
    match packet {
        svc::Packet::ConnectionCheck => {
            let ack = svc::Packet::ConnectionCheckAck.encode_server_udp(&secret);
            let _ = sock.send_to(&ack, addr);
            if let Some(conn) = shared
                .players
                .lock()
                .unwrap()
                .get_mut(&player)
                .and_then(|p| p.svc.as_mut())
            {
                conn.authenticated = true;
            }
        }
        svc::Packet::Authenticate {
            secret: claimed, ..
        } => {
            if claimed == secret {
                let ack = svc::Packet::AuthenticateAck.encode_server_udp(&secret);
                let _ = sock.send_to(&ack, addr);
            }
        }
        svc::Packet::KeepAlive => {}
        svc::Packet::Mic {
            data,
            whispering,
            sequence: _,
        } => {
            let cfg = shared.config.read().unwrap();
            let distance = if whispering {
                cfg.whisper
            } else {
                cfg.distance
            };
            shared.incoming.lock().unwrap().push(Incoming {
                sender: player,
                protocol: Protocol::Svc,
                data,
                distance,
                whispering,
            });
        }
        _ => {}
    }
}

fn pv_loop(shared: Arc<Shared>, sock: UdpSocket) {
    let mut buf = vec![0u8; 4096];
    loop {
        drain_out(&shared.pv_out, &sock);
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => pv_handle(&shared, &sock, &buf[..n], addr),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

fn pv_handle(shared: &Shared, _sock: &UdpSocket, datagram: &[u8], addr: SocketAddr) {
    let Ok((secret, packet)) = plasmo::Udp::decode(datagram) else {
        return;
    };
    // Map the secret back to the player.
    let uuid = {
        let players = shared.players.lock().unwrap();
        players
            .iter()
            .find(|(_, p)| p.pv.as_ref().map(|c| c.secret) == Some(secret))
            .map(|(uuid, _)| *uuid)
    };
    let Some(uuid) = uuid else {
        return;
    };
    match packet {
        plasmo::Udp::Ping { .. } => {
            let newly = {
                let mut players = shared.players.lock().unwrap();
                if let Some(p) = players.get_mut(&uuid) {
                    if let Some(c) = p.pv.as_mut() {
                        c.addr = addr;
                    }
                    let was = p.pv.as_ref().map(|c| c.connected).unwrap_or(false);
                    if let Some(c) = p.pv.as_mut() {
                        c.connected = true;
                    }
                    !was
                } else {
                    false
                }
            };
            if newly {
                shared.pv_pending_config.lock().unwrap().push(uuid);
            }
        }
        plasmo::Udp::PlayerAudio(a) => {
            let distance = a.distance as f64;
            shared.incoming.lock().unwrap().push(Incoming {
                sender: uuid,
                protocol: Protocol::Plasmo,
                data: a.data,
                distance: if distance <= 0.0 { 48.0 } else { distance },
                whispering: a.stereo,
            });
        }
        plasmo::Udp::SourceAudio(_) => {}
    }
}

fn drain_out(out: &Mutex<Vec<(Vec<u8>, SocketAddr)>>, sock: &UdpSocket) {
    let queued: Vec<(Vec<u8>, SocketAddr)> = {
        let mut q = out.lock().unwrap();
        std::mem::take(&mut *q)
    };
    for (datagram, addr) in queued {
        let _ = sock.send_to(&datagram, addr);
    }
}
