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
//!
//! **Off by default.** The two UDP listeners are opt-in (`svc_enabled` /
//! `pv_enabled`), so a server that installs the plugin does not open a port
//! until the operator asks for it.

pub mod bytes;
pub mod config;
pub mod crypto;
pub mod frame;
pub mod plasmo;
pub mod record;
pub mod svc;

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    HostBackend, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

use config::VoiceConfig;
use frame::{Listener, Protocol, VoiceFrame};

/// The plugin id and its command.
pub const ID: &str = "voice";

/// How many audio frames per second one sender may relay, and how large a
/// burst it may accumulate. Opus is 50 frames a second (20 ms), so this is
/// generous headroom while still bounding a spamming client.
const AUDIO_RATE_PER_SEC: f64 = 60.0;
/// The per-sender token-bucket depth.
const AUDIO_BURST: f64 = 40.0;
/// The most unrelayed audio frames kept between the UDP threads and the tick.
/// A full queue drops its oldest frame, so a flood cannot grow memory.
const INCOMING_CAP: usize = 512;
/// The most datagrams kept for the UDP threads to send. A full queue drops its
/// oldest packet, so a peer that cannot drain cannot grow memory.
const OUT_CAP: usize = 1024;
/// The recorder's job queue depth; a full queue drops the frame rather than
/// blocking the tick.
const RECORD_QUEUE: usize = 512;
/// How often server->client keepalives go out (SVC `KeepAlive`, PV `Ping`), in
/// milliseconds. Both mods time out after ~10 s of silence.
const KEEPALIVE_MS: i64 = 1000;
/// The PV `PlayerInfoRequest` retry schedule, mirroring the mod.
const PV_INFO_RETRY_MS: [i64; 5] = [1_000, 3_000, 5_000, 10_000, 15_000];
/// How often the maintenance thread checks recording retention.
const MAINTENANCE_SECS: u64 = 3_600;

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

/// A per-sender token bucket, so one client cannot relay unbounded audio.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_ms: i64,
}

impl Bucket {
    fn new(now_ms: i64) -> Bucket {
        Bucket {
            tokens: AUDIO_BURST,
            last_ms: now_ms,
        }
    }

    /// Spend a token if one is available.
    fn allow(&mut self, now_ms: i64) -> bool {
        let elapsed = ((now_ms - self.last_ms).max(0) as f64) / 1000.0;
        self.tokens = (self.tokens + elapsed * AUDIO_RATE_PER_SEC).min(AUDIO_BURST);
        self.last_ms = now_ms;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// What an operator is silently listening to with `/voice spy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpyTarget {
    All,
    Player(u128),
    Group(u128),
}

impl SpyTarget {
    fn matches(&self, f: &VoiceFrame) -> bool {
        match self {
            SpyTarget::All => true,
            SpyTarget::Player(u) => *u == f.sender,
            SpyTarget::Group(g) => f.group == Some(*g),
        }
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

/// A pending PV `PlayerInfoRequest` retry.
struct PvRequest {
    attempts: usize,
    next_ms: i64,
}

/// A job for the recording writer thread.
enum RecordJob {
    Frame {
        sender: u128,
        name: String,
        world: String,
        group: Option<u128>,
        data: Vec<u8>,
    },
}

/// The recording state: the current target, and the writer's queue.
#[derive(Default)]
struct RecorderState {
    target: record::Target,
    tx: Option<SyncSender<RecordJob>>,
}

/// The state the plugin and its UDP threads share.
struct Shared {
    config: RwLock<VoiceConfig>,
    players: Mutex<HashMap<u128, PlayerView>>,
    /// PV secret -> player uuid, so a UDP datagram is resolved in O(1).
    pv_secrets: Mutex<HashMap<u128, u128>>,
    incoming: Mutex<VecDeque<Incoming>>,
    /// Per-sender audio token buckets.
    rates: Mutex<HashMap<u128, Bucket>>,
    svc_out: Mutex<VecDeque<(Vec<u8>, SocketAddr)>>,
    pv_out: Mutex<VecDeque<(Vec<u8>, SocketAddr)>>,
    groups: Mutex<HashMap<u128, SvcGroup>>,
    recorder: Mutex<RecorderState>,
    /// Admin uuid -> what they are listening to.
    spies: Mutex<HashMap<u128, SpyTarget>>,
    /// PV players whose UDP ping arrived; the tick sends their config.
    pv_pending_config: Mutex<Vec<u128>>,
    /// PV players awaiting a `PlayerInfoRequest` (join and retries).
    pv_pending_info: Mutex<HashMap<u128, PvRequest>>,
    /// SVC players that just validated, to be sent the state list.
    svc_pending_states: Mutex<Vec<u128>>,
    /// When the last SVC/PV keepalive batch went out.
    last_keepalive: Mutex<i64>,
    /// Cleared at shutdown so the UDP and maintenance threads exit.
    running: AtomicBool,
}

impl Shared {
    fn new(config: VoiceConfig) -> Shared {
        Shared {
            config: RwLock::new(config),
            players: Mutex::new(HashMap::new()),
            pv_secrets: Mutex::new(HashMap::new()),
            incoming: Mutex::new(VecDeque::new()),
            rates: Mutex::new(HashMap::new()),
            svc_out: Mutex::new(VecDeque::new()),
            pv_out: Mutex::new(VecDeque::new()),
            groups: Mutex::new(HashMap::new()),
            recorder: Mutex::new(RecorderState::default()),
            spies: Mutex::new(HashMap::new()),
            pv_pending_config: Mutex::new(Vec::new()),
            pv_pending_info: Mutex::new(HashMap::new()),
            svc_pending_states: Mutex::new(Vec::new()),
            last_keepalive: Mutex::new(0),
            running: AtomicBool::new(true),
        }
    }

    /// Queue an audio frame if the sender is inside its rate limit and there is
    /// room. A full queue drops the oldest frame.
    fn push_incoming(&self, inc: Incoming, now_ms: i64) {
        {
            let mut rates = self.rates.lock().unwrap();
            let bucket = rates
                .entry(inc.sender)
                .or_insert_with(|| Bucket::new(now_ms));
            if !bucket.allow(now_ms) {
                return;
            }
        }
        let mut q = self.incoming.lock().unwrap();
        while q.len() >= INCOMING_CAP {
            q.pop_front();
        }
        q.push_back(inc);
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

    /// Start the UDP servers named by the config. Both are opt-in.
    fn start_servers(&mut self, ctx: &mut NativeContext) {
        let cfg = self.shared.config.read().unwrap().clone();
        if !cfg.svc_enabled && !cfg.pv_enabled {
            ctx.log(
                LogLevel::Info,
                "voice: both listeners disabled (set [plugins.voice] svc_enabled/pv_enabled)",
            );
            return;
        }
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

    /// Start the recording writer thread, if it is not already running.
    fn ensure_recorder(&self) {
        let mut rec = self.shared.recorder.lock().unwrap();
        if rec.tx.is_some() {
            return;
        }
        let dir = self.shared.config.read().unwrap().record_dir.clone();
        let (tx, rx) = mpsc::sync_channel(RECORD_QUEUE);
        let spawned = std::thread::Builder::new()
            .name("voice-record".into())
            .spawn(move || record_writer(dir, rx))
            .is_ok();
        if spawned {
            rec.tx = Some(tx);
        }
    }

    // --- tick ----------------------------------------------------------

    /// Refresh the player positions, relay queued frames, answer pending
    /// handshakes and send keepalives. Everything here is bounded and the
    /// recording/retention I/O runs on other threads.
    fn tick(&self, ctx: &mut NativeContext) {
        let now = now_ms();
        let listeners = self.listeners(ctx);
        self.relay_queued(&listeners);
        self.pv_info_tick(ctx, now);
        self.pv_config_tick(ctx);
        self.svc_states_tick(ctx);
        self.keepalives(now);
    }

    fn relay_queued(&self, listeners: &[Listener]) {
        loop {
            let Some(inc) = self.shared.incoming.lock().unwrap().pop_front() else {
                break;
            };
            let Some(sender) = listeners.iter().find(|l| l.uuid == inc.sender) else {
                continue;
            };
            let (group, name) = {
                let players = self.shared.players.lock().unwrap();
                match players.get(&inc.sender) {
                    Some(p) => (p.group, p.name.clone()),
                    None => (None, String::new()),
                }
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
            self.record_frame(&voice_frame, &sender.world);
            let mut targets = frame::route(
                &voice_frame,
                &sender.world,
                sender.x,
                sender.y,
                sender.z,
                listeners,
            );
            // Operators listening silently get the frame too.
            {
                let spies = self.shared.spies.lock().unwrap();
                for (&admin, &target) in spies.iter() {
                    if admin != inc.sender
                        && target.matches(&voice_frame)
                        && !targets.iter().any(|(u, _)| *u == admin)
                    {
                        targets.push((admin, frame::Route::Proximity));
                    }
                }
            }
            for (uuid, _route) in targets {
                self.deliver(uuid, &voice_frame);
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
    fn deliver(&self, uuid: u128, f: &VoiceFrame) {
        let players = self.shared.players.lock().unwrap();
        let Some(p) = players.get(&uuid) else {
            return;
        };
        if let Some(conn) = &p.svc {
            let packet = if let Some(group) = f.group {
                // Group audio is not distance-attenuated, so it travels as a
                // group packet rather than a positional one.
                svc::Packet::GroupSound {
                    channel_id: group,
                    sender: f.sender,
                    data: f.data.clone(),
                    sequence: now_ms(),
                    category: None,
                }
            } else {
                svc::Packet::PlayerSound {
                    channel_id: f.sender,
                    sender: f.sender,
                    data: f.data.clone(),
                    sequence: now_ms(),
                    whispering: f.whispering,
                    distance: f.distance as f32,
                    category: None,
                }
            };
            let datagram = packet.encode_server_udp(&conn.secret);
            push_out(&self.shared.svc_out, (datagram, conn.addr));
        } else if let Some(conn) = &p.pv {
            let packet = plasmo::Udp::SourceAudio(plasmo::SourceAudio {
                sequence: now_ms(),
                data: f.data.clone(),
                source_id: source_id_for(f.sender),
                source_state: 0,
                distance: f.distance as i16,
            });
            let datagram = packet.encode(conn.secret);
            push_out(&self.shared.pv_out, (datagram, conn.addr));
        }
    }

    /// Send a frame to the recording writer if the current target captures it.
    fn record_frame(&self, f: &VoiceFrame, world: &str) {
        let rec = self.shared.recorder.lock().unwrap();
        if !rec.target.captures(f.sender, f.group) {
            return;
        }
        if let Some(tx) = &rec.tx {
            let _ = tx.try_send(RecordJob::Frame {
                sender: f.sender,
                name: f.sender_name.clone(),
                world: world.to_string(),
                group: f.group,
                data: f.data.clone(),
            });
        }
    }

    /// On the PV retry schedule, ask a client that has not identified itself
    /// to send its `PlayerInfoPacket`.
    fn pv_info_tick(&self, ctx: &mut NativeContext, now: i64) {
        let due: Vec<u128> = {
            let mut pending = self.shared.pv_pending_info.lock().unwrap();
            let due: Vec<u128> = pending
                .iter()
                .filter(|(_, st)| now >= st.next_ms && st.attempts < PV_INFO_RETRY_MS.len())
                .map(|(u, _)| *u)
                .collect();
            for u in &due {
                if let Some(st) = pending.get_mut(u) {
                    let delay = PV_INFO_RETRY_MS[st.attempts];
                    st.attempts += 1;
                    st.next_ms = now + delay;
                }
            }
            due
        };
        for uuid in due {
            let entity = self.entity_of(uuid);
            if let Some(entity) = entity {
                let pkt = plasmo::Tcp::PlayerInfoRequest;
                ctx.backend.send_plugin_message(
                    ctx.id,
                    entity,
                    plasmo::channel::TCP,
                    &pkt.encode(),
                );
            }
        }
    }

    /// Send a freshly-pinged PV client its config and player list.
    fn pv_config_tick(&self, ctx: &mut NativeContext) {
        let pending: Vec<u128> = {
            let mut p = self.shared.pv_pending_config.lock().unwrap();
            std::mem::take(&mut *p)
        };
        for uuid in pending {
            self.send_pv_config(uuid, ctx);
        }
    }

    /// Send a freshly-validated SVC client the states of every SVC player.
    fn svc_states_tick(&self, ctx: &mut NativeContext) {
        let pending: Vec<u128> = {
            let mut p = self.shared.svc_pending_states.lock().unwrap();
            std::mem::take(&mut *p)
        };
        if pending.is_empty() {
            return;
        }
        let states: Vec<svc::PlayerState> = {
            let players = self.shared.players.lock().unwrap();
            players
                .iter()
                .filter(|(_, p)| p.svc.is_some())
                .map(|(uuid, p)| svc::PlayerState {
                    disabled: p.disabled,
                    disconnected: p.svc.as_ref().map(|c| !c.authenticated).unwrap_or(true),
                    uuid: *uuid,
                    name: p.name.clone(),
                    group: p.group,
                })
                .collect()
        };
        let payload = svc::ChannelPacket::States(states).encode();
        for uuid in pending {
            self.send_svc_to(ctx, uuid, svc::channel::STATES, &payload);
        }
    }

    /// Send the periodic keepalives. SVC times a client out after ten seconds
    /// of silence, and a PV client only considers UDP live after a server
    /// ping, so both must be driven from here.
    fn keepalives(&self, now: i64) {
        {
            let mut last = self.shared.last_keepalive.lock().unwrap();
            if now - *last < KEEPALIVE_MS {
                return;
            }
            *last = now;
        }
        let players = self.shared.players.lock().unwrap();
        for p in players.values() {
            if let Some(conn) = &p.svc {
                if conn.authenticated {
                    let datagram = svc::Packet::KeepAlive.encode_server_udp(&conn.secret);
                    push_out(&self.shared.svc_out, (datagram, conn.addr));
                }
            }
            if let Some(conn) = &p.pv {
                if conn.connected {
                    let datagram = plasmo::Udp::Ping {
                        time: now,
                        server_ip: None,
                        server_port: 0,
                    }
                    .encode(conn.secret);
                    push_out(&self.shared.pv_out, (datagram, conn.addr));
                }
            }
        }
    }

    /// Send the Plasmo Voice config packet over the plugin channel.
    fn send_pv_config(&self, uuid: u128, ctx: &mut NativeContext) {
        let Some(entity) = self.entity_of(uuid) else {
            return;
        };
        let (config_bytes, list_bytes) = {
            let cfg = self.shared.config.read().unwrap();
            let server_id = u128::from_be_bytes(*b"aether\0\0\0\0\0\0\0\0\0\0");
            let config = plasmo::Config {
                server_id,
                default_distance: cfg.distance as i32,
                max_distance: cfg.distance as i32,
                ..Default::default()
            };
            drop(cfg);
            let mut data = Vec::with_capacity(1 + 256);
            data.push(plasmo::tcp::CONFIG);
            data.extend_from_slice(&config.encode());
            // The player list, so the client can build sources for everyone
            // else. Built (and the lock released) before any host call.
            let players = self.shared.players.lock().unwrap();
            let list: Vec<plasmo::PlayerInfo> = players
                .iter()
                .filter(|(_, p)| p.pv.is_some())
                .map(|(uuid, p)| plasmo::PlayerInfo {
                    id: *uuid,
                    nick: p.name.clone(),
                    muted: false,
                    voice_disabled: p.disabled,
                    microphone_muted: false,
                })
                .collect();
            let packet = plasmo::Tcp::PlayerList { players: list };
            (data, packet.encode())
        };
        ctx.backend
            .send_plugin_message(ctx.id, entity, plasmo::channel::TCP, &config_bytes);
        ctx.backend
            .send_plugin_message(ctx.id, entity, plasmo::channel::TCP, &list_bytes);
    }

    /// Broadcast a Plasmo `PlayerInfoUpdatePacket` to every PV client.
    fn broadcast_pv_state(&self, uuid: u128, ctx: &mut NativeContext) {
        let info = {
            let players = self.shared.players.lock().unwrap();
            let Some(p) = players.get(&uuid) else {
                return;
            };
            plasmo::PlayerInfo {
                id: uuid,
                nick: p.name.clone(),
                muted: false,
                voice_disabled: p.disabled,
                microphone_muted: false,
            }
        };
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

    /// Answer a PV client's `SourceInfoRequestPacket` with a player source.
    fn send_pv_source_info(&self, uuid: u128, source_id: u128, ctx: &mut NativeContext) {
        let (source, entity) = {
            let players = self.shared.players.lock().unwrap();
            // The requester is the listener that must be told about a source;
            // find the speaker that owns `source_id`.
            let Some((speaker_uuid, speaker)) = players
                .iter()
                .find(|(su, _)| source_id_for(**su) == source_id)
            else {
                return;
            };
            let Some(requester) = players.get(&uuid) else {
                return;
            };
            let source = plasmo::PlayerSource {
                addon_id: "plasmo".to_string(),
                id: source_id,
                line_id: plasmo::proximity_line_id(),
                name: None,
                state: 0,
                stereo: false,
                icon_visible: true,
                angle: 0,
                player: plasmo::PlayerInfo {
                    id: *speaker_uuid,
                    nick: speaker.name.clone(),
                    muted: false,
                    voice_disabled: speaker.disabled,
                    microphone_muted: false,
                },
            };
            (source, requester.entity)
        };
        let packet = plasmo::Tcp::SourceInfo(Box::new(source));
        ctx.backend
            .send_plugin_message(ctx.id, entity, plasmo::channel::TCP, &packet.encode());
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
                // The listener is opt-in: with SVC off, answer nothing so the
                // client does not try to reach a socket that is not bound.
                if !cfg.svc_enabled {
                    return;
                }
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
                    keep_alive: KEEPALIVE_MS as i32,
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
                self.broadcast_svc_state(ctx, uuid);
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
                self.join_svc_group(ctx, uuid, id, None, false);
                self.broadcast_svc_group(ctx, id, true);
            }
            svc::ChannelPacket::JoinGroup { group, password } => {
                self.join_svc_group(ctx, uuid, group, password, false);
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

    /// Join `uuid` to `group`. `bypass` is the operator path: it skips the
    /// password, so an admin can join a protected group without knowing it.
    fn join_svc_group(
        &self,
        ctx: &mut NativeContext,
        uuid: u128,
        group: u128,
        password: Option<String>,
        bypass: bool,
    ) {
        let (wrong, id, exists) = {
            let groups = self.shared.groups.lock().unwrap();
            match groups.get(&group) {
                Some(g) => {
                    let ok = bypass
                        || match (&g.password, &password) {
                            (None, _) => true,
                            (Some(expect), Some(got)) => expect == got,
                            _ => false,
                        };
                    (!ok, if ok { Some(g.id) } else { None }, true)
                }
                None => (false, None, false),
            }
        };
        if exists {
            if let Some(entry) = self.shared.players.lock().unwrap().get_mut(&uuid) {
                entry.group = id;
            }
        }
        let reply = svc::ChannelPacket::JoinedGroup {
            group: id,
            wrong_password: wrong,
        };
        self.send_svc_to(ctx, uuid, svc::channel::JOINED_GROUP, &reply.encode());
        self.broadcast_svc_state(ctx, uuid);
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
        self.broadcast_svc_state(ctx, uuid);
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
            svc::ChannelPacket::AddGroup(cg).encode()
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
    fn broadcast_svc_state(&self, ctx: &mut NativeContext, uuid: u128) {
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
        let entity = self.entity_of(uuid);
        if let Some(entity) = entity {
            ctx.backend
                .send_plugin_message(ctx.id, entity, channel, payload);
        }
    }

    fn on_pv_channel(&self, ctx: &mut NativeContext, p: &Player, data: &[u8]) {
        let Ok(uuid) = self.uuid(ctx, p) else {
            return;
        };
        // Any TCP packet means the client is present, so stop retrying.
        self.shared.pv_pending_info.lock().unwrap().remove(&uuid);
        let Ok(packet) = plasmo::Tcp::decode(data) else {
            return;
        };
        match packet {
            plasmo::Tcp::PlayerInfo { .. } => {
                // Allocate the UDP secret and hand the client its connection.
                let cfg = self.shared.config.read().unwrap().clone();
                if !cfg.pv_enabled {
                    return;
                }
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
                self.shared.pv_secrets.lock().unwrap().insert(secret, uuid);
                let packet = plasmo::Tcp::Connection {
                    secret,
                    // The client replaces `0.0.0.0` with the MC server's remote
                    // IP; an empty string would send it to loopback.
                    ip: "0.0.0.0".to_string(),
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
            plasmo::Tcp::SourceInfoRequest { source_id } => {
                self.send_pv_source_info(uuid, source_id, ctx);
            }
            // The server does not act on these.
            plasmo::Tcp::PlayerAudioEnd { .. }
            | plasmo::Tcp::ActivationDistances { .. }
            | plasmo::Tcp::LanguageRequest { .. } => {}
            _ => {}
        }
    }

    fn uuid(&self, ctx: &NativeContext, p: &Player) -> Result<u128, ()> {
        ctx.backend.player_uuid(ctx.id, p.id).ok_or(())
    }

    fn entity_of(&self, uuid: u128) -> Option<u64> {
        self.shared
            .players
            .lock()
            .unwrap()
            .get(&uuid)
            .map(|p| p.entity)
    }
}

impl NativePlugin for VoicePlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let cfg = VoiceConfig::from_map(ctx.config);
        *self.shared.config.write().unwrap() = cfg;
        self.start_servers(ctx);
        // Retention housekeeping runs off the tick: a filesystem walk must not
        // stall the game (review VC1-1 §5).
        let shared = Arc::clone(&self.shared);
        let backend: Arc<dyn HostBackend> = Arc::clone(ctx.backend);
        let id = ctx.id.to_string();
        let _ = std::thread::Builder::new()
            .name("voice-maintain".into())
            .spawn(move || maintenance(shared, backend, id));
        Ok(())
    }

    fn shutdown(&mut self, _ctx: &mut NativeContext) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.shared.recorder.lock().unwrap().tx = None;
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, _n: u64) {
        self.tick(ctx);
    }

    fn after_player_join(&mut self, ctx: &mut NativeContext, p: &Player) {
        if let Ok(uuid) = self.uuid(ctx, p) {
            {
                let mut players = self.shared.players.lock().unwrap();
                let entry = players.entry(uuid).or_default();
                entry.entity = p.id;
                entry.name = p.name.clone();
            }
            // Start looking for the PV client; it only answers a
            // `PlayerInfoRequestPacket`. Only when the PV listener is on.
            if self.shared.config.read().unwrap().pv_enabled {
                self.shared.pv_pending_info.lock().unwrap().insert(
                    uuid,
                    PvRequest {
                        attempts: 0,
                        next_ms: now_ms(),
                    },
                );
            }
        }
    }

    fn after_player_leave(&mut self, ctx: &mut NativeContext, p: &Player) {
        if let Ok(uuid) = self.uuid(ctx, p) {
            let secret = {
                let mut players = self.shared.players.lock().unwrap();
                players.remove(&uuid).and_then(|v| v.pv).map(|c| c.secret)
            };
            if let Some(secret) = secret {
                self.shared.pv_secrets.lock().unwrap().remove(&secret);
            }
            self.shared.pv_pending_info.lock().unwrap().remove(&uuid);
            self.shared.spies.lock().unwrap().remove(&uuid);
            self.shared.rates.lock().unwrap().remove(&uuid);
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
                // Only an existing group can be joined; a typo must not
                // report success.
                let Some(id) = lookup_group(&self.shared, group) else {
                    lines.push(format!("&cNo voice group `{group}`."));
                    return Some(result(lines));
                };
                if let Ok(uuid) = self.uuid(ctx, p) {
                    // An operator joins without the password and silently.
                    self.join_svc_group(ctx, uuid, id, None, true);
                }
                lines.push(format!("&aJoined voice group `{group}`."));
            }
            "leave" => {
                if let Ok(uuid) = self.uuid(ctx, p) {
                    self.leave_svc_group(ctx, uuid);
                    self.shared.spies.lock().unwrap().remove(&uuid);
                }
                lines.push("&7Left the voice group and stopped spying.".to_string());
            }
            "spy" => {
                let Ok(uuid) = self.uuid(ctx, p) else {
                    return Some(result(lines));
                };
                match args.get(1).map(|s| s.as_str()) {
                    None | Some("off") | Some("stop") => {
                        self.shared.spies.lock().unwrap().remove(&uuid);
                        lines.push("&7Stopped spying on voice.".to_string());
                    }
                    Some("all") => {
                        self.shared
                            .spies
                            .lock()
                            .unwrap()
                            .insert(uuid, SpyTarget::All);
                        lines.push("&7Listening to all voice.".to_string());
                    }
                    Some(t) => {
                        if let Some(g) = lookup_group(&self.shared, t) {
                            self.shared
                                .spies
                                .lock()
                                .unwrap()
                                .insert(uuid, SpyTarget::Group(g));
                            lines.push(format!("&7Listening to group `{t}`."));
                        } else if let Some(target) = self.player_uuid_by_name(ctx, t) {
                            self.shared
                                .spies
                                .lock()
                                .unwrap()
                                .insert(uuid, SpyTarget::Player(target));
                            lines.push(format!("&7Listening to `{t}`."));
                        } else {
                            lines.push(format!("&cNo group or player `{t}`."));
                        }
                    }
                }
            }
            "record" => {
                let action = args.get(1).map(|s| s.as_str()).unwrap_or("status");
                match action {
                    "start" => {
                        let target = args.get(2).map(|s| s.as_str()).unwrap_or("all");
                        let (t, label) = self.resolve_target(ctx, target);
                        if t == record::Target::None {
                            lines.push(format!("&cNo player or group `{target}`."));
                        } else {
                            self.ensure_recorder();
                            self.shared.recorder.lock().unwrap().target = t.clone();
                            if self.shared.config.read().unwrap().notify_recorded {
                                self.notify_recorded(ctx, &t);
                            }
                            lines.push(format!("&aVoice recording started for {label}."));
                        }
                    }
                    "stop" => {
                        {
                            let mut rec = self.shared.recorder.lock().unwrap();
                            rec.target = record::Target::None;
                            // Dropping the sender closes the writer's channel,
                            // which flushes and closes every open session.
                            rec.tx = None;
                        }
                        lines.push("&7Voice recording stopped.".to_string());
                    }
                    _ => lines.push("&7/voice record start|stop [player|group|all]".to_string()),
                }
            }
            _ => {
                lines.push("&6/voice join <group>".to_string());
                lines.push("&6/voice leave".to_string());
                lines.push("&6/voice spy <player|group|all|off>".to_string());
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

    /// Turn a `/voice record` target name into a capture target. A player is
    /// resolved to their **real** uuid (an online lookup), so the recorder
    /// actually matches them.
    fn resolve_target(&self, ctx: &NativeContext, target: &str) -> (record::Target, String) {
        if target.eq_ignore_ascii_case("all") {
            return (record::Target::All, "everyone".to_string());
        }
        if let Some(id) = lookup_group(&self.shared, target) {
            return (record::Target::Group(id), format!("group `{target}`"));
        }
        if let Some(uuid) = self.player_uuid_by_name(ctx, target) {
            return (record::Target::Player(uuid), target.to_string());
        }
        (record::Target::None, target.to_string())
    }

    /// The uuid of an online player whose name matches `name`.
    fn player_uuid_by_name(&self, ctx: &NativeContext, name: &str) -> Option<u128> {
        for p in ctx.backend.online_players(ctx.id) {
            if p.name.eq_ignore_ascii_case(name) {
                if let Some(uuid) = ctx.backend.player_uuid(ctx.id, p.id) {
                    return Some(uuid);
                }
            }
        }
        None
    }

    /// Tell every player the target captures that their voice is being
    /// recorded. Kept on by default (`notify_recorded`): many jurisdictions
    /// expect a recorded party to know.
    fn notify_recorded(&self, ctx: &NativeContext, target: &record::Target) {
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
pub fn source_id_for(player: u128) -> u128 {
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Background threads: recording and retention.
// ---------------------------------------------------------------------------

/// The recording writer thread: owns every open `OggOpusWriter`, so a write
/// never happens on the tick.
fn record_writer(dir: String, rx: Receiver<RecordJob>) {
    let mut sessions: HashMap<u128, record::Session> = HashMap::new();
    while let Ok(job) = rx.recv() {
        let RecordJob::Frame {
            sender,
            name,
            world,
            group,
            data,
        } = job;
        if let std::collections::hash_map::Entry::Vacant(slot) = sessions.entry(sender) {
            match record::Session::open(&dir, sender, &name, &world, group) {
                Ok(s) => {
                    slot.insert(s);
                }
                Err(_) => continue,
            }
        }
        if let Some(s) = sessions.get_mut(&sender) {
            let _ = s.writer.write_frame(&data);
        }
    }
    // The channel closed: every session drops, flushing its file.
}

/// The retention thread: walks the recording directory on its own thread so
/// the tick never touches the filesystem (review VC1-1 §5).
fn maintenance(shared: Arc<Shared>, backend: Arc<dyn HostBackend>, id: String) {
    loop {
        for _ in 0..MAINTENANCE_SECS {
            if !shared.running.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if !shared.running.load(Ordering::SeqCst) {
            return;
        }
        let cfg = shared.config.read().unwrap().clone();
        if cfg.retention_days == 0 && cfg.max_disk_mb == 0 {
            continue;
        }
        if let Err(e) =
            record::enforce_retention(&cfg.record_dir, cfg.retention_days, cfg.max_disk_mb)
        {
            backend.log(&id, LogLevel::Warn, &format!("voice retention: {e}"));
        }
    }
}

// ---------------------------------------------------------------------------
// UDP server loops.
// ---------------------------------------------------------------------------

fn svc_loop(shared: Arc<Shared>, sock: UdpSocket) {
    let mut buf = vec![0u8; 4096];
    while shared.running.load(Ordering::SeqCst) {
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
            let newly = {
                let mut players = shared.players.lock().unwrap();
                match players.get_mut(&player).and_then(|p| p.svc.as_mut()) {
                    Some(conn) => {
                        let was = conn.authenticated;
                        conn.authenticated = true;
                        !was
                    }
                    None => false,
                }
            };
            if newly {
                shared.svc_pending_states.lock().unwrap().push(player);
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
            drop(cfg);
            shared.push_incoming(
                Incoming {
                    sender: player,
                    protocol: Protocol::Svc,
                    data,
                    distance,
                    whispering,
                },
                now_ms(),
            );
        }
        _ => {}
    }
}

fn pv_loop(shared: Arc<Shared>, sock: UdpSocket) {
    let mut buf = vec![0u8; 4096];
    while shared.running.load(Ordering::SeqCst) {
        drain_out(&shared.pv_out, &sock);
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => pv_handle(&shared, &buf[..n], addr),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

fn pv_handle(shared: &Shared, datagram: &[u8], addr: SocketAddr) {
    let Ok((secret, packet)) = plasmo::Udp::decode(datagram) else {
        return;
    };
    // Map the secret back to the player in O(1).
    let uuid = shared.pv_secrets.lock().unwrap().get(&secret).copied();
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
            shared.push_incoming(
                Incoming {
                    sender: uuid,
                    protocol: Protocol::Plasmo,
                    data: a.data,
                    distance: if distance <= 0.0 { 48.0 } else { distance },
                    // `stereo` is not "whispering"; the frame is not a whisper.
                    whispering: false,
                },
                now_ms(),
            );
        }
        plasmo::Udp::SourceAudio(_) => {}
    }
}

/// Queue one outbound datagram, dropping the oldest if the queue is full.
fn push_out(out: &Mutex<VecDeque<(Vec<u8>, SocketAddr)>>, item: (Vec<u8>, SocketAddr)) {
    let mut q = out.lock().unwrap();
    while q.len() >= OUT_CAP {
        q.pop_front();
    }
    q.push_back(item);
}

fn drain_out(out: &Mutex<VecDeque<(Vec<u8>, SocketAddr)>>, sock: &UdpSocket) {
    let queued: VecDeque<(Vec<u8>, SocketAddr)> = {
        let mut q = out.lock().unwrap();
        std::mem::take(&mut *q)
    };
    for (datagram, addr) in queued {
        let _ = sock.send_to(&datagram, addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server must answer a PV client's `SourceInfoRequest` with a
    /// `SourceInfoPacket` whose first two fields are the type byte and the
    /// literal `PLAYER`. This is the packet whose absence left PV audio
    /// buffered and dropped after five seconds.
    #[test]
    fn pv_source_info_encodes_a_player_source() {
        let source = plasmo::PlayerSource {
            addon_id: "plasmo".to_string(),
            id: 0x11,
            line_id: plasmo::proximity_line_id(),
            name: None,
            state: 0,
            stereo: false,
            icon_visible: true,
            angle: 0,
            player: plasmo::PlayerInfo {
                id: 0x22,
                nick: "Steve".to_string(),
                muted: false,
                voice_disabled: false,
                microphone_muted: false,
            },
        };
        let bytes = plasmo::Tcp::SourceInfo(Box::new(source)).encode();
        assert_eq!(bytes[0], plasmo::tcp::SOURCE_INFO);
        // writeUTF "PLAYER" -> 0x0006 "PLAYER".
        assert_eq!(&bytes[1..3], &6u16.to_be_bytes());
        assert_eq!(&bytes[3..9], b"PLAYER");
    }

    /// `PlayerInfoRequestPacket` is a bare type byte; the client only
    /// identifies itself in answer to it, so it must be sent on join.
    #[test]
    fn pv_player_info_request_is_a_bare_type_byte() {
        assert_eq!(
            plasmo::Tcp::PlayerInfoRequest.encode(),
            vec![plasmo::tcp::PLAYER_INFO_REQUEST]
        );
    }

    /// The `ConnectionPacket` must name `0.0.0.0` (not an empty string) so the
    /// client substitutes the Minecraft server's remote IP instead of dialling
    /// loopback.
    #[test]
    fn pv_connection_ip_is_the_wildcard() {
        let pkt = plasmo::Tcp::Connection {
            secret: 0xabc,
            ip: "0.0.0.0".to_string(),
            port: 60_606,
        };
        let bytes = pkt.encode();
        assert_eq!(bytes[0], plasmo::tcp::CONNECTION);
        // type (1) + uuid (16) then writeUTF "0.0.0.0" (length 7).
        assert_eq!(u16::from_be_bytes([bytes[17], bytes[18]]), 7);
        assert_eq!(&bytes[19..26], b"0.0.0.0");
    }

    /// The server-side `PingPacket` carries the UDP magic, the ping type and
    /// the secret, so a client can tell the connection is alive.
    #[test]
    fn pv_ping_has_the_magic_and_type() {
        let bytes = plasmo::Udp::Ping {
            time: 1,
            server_ip: None,
            server_port: 0,
        }
        .encode(0x1234);
        assert_eq!(&bytes[..4], &plasmo::UDP_MAGIC.to_be_bytes());
        assert_eq!(bytes[4], plasmo::udp::PING);
    }

    /// The per-sender token bucket allows a burst, then throttles to the
    /// steady rate, refilling over time.
    #[test]
    fn a_sender_is_rate_limited() {
        let mut b = Bucket::new(0);
        let mut allowed = 0;
        for _ in 0..(AUDIO_BURST as usize + 10) {
            if b.allow(0) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, AUDIO_BURST as usize, "only the burst at t=0");
        // A second later the bucket has refilled to the steady rate.
        assert!(b.allow(1000));
    }

    /// The inbound queue is capped and drops its oldest frame, so a flood
    /// cannot grow memory without bound.
    #[test]
    fn the_incoming_queue_is_capped() {
        let shared = Shared::new(VoiceConfig::default());
        let over = INCOMING_CAP + 88;
        for i in 0..over {
            shared.push_incoming(
                Incoming {
                    sender: i as u128,
                    protocol: Protocol::Svc,
                    data: vec![0u8; 4],
                    distance: 48.0,
                    whispering: false,
                },
                0,
            );
        }
        let q = shared.incoming.lock().unwrap();
        assert_eq!(q.len(), INCOMING_CAP);
        // The first `over - INCOMING_CAP` senders were dropped from the front.
        assert_eq!(q.front().unwrap().sender, (over - INCOMING_CAP) as u128);
    }

    /// One sender cannot exceed its rate even by pushing many frames.
    #[test]
    fn one_sender_cannot_flood_the_queue() {
        let shared = Shared::new(VoiceConfig::default());
        for _ in 0..1000 {
            shared.push_incoming(
                Incoming {
                    sender: 7,
                    protocol: Protocol::Svc,
                    data: vec![0u8; 4],
                    distance: 48.0,
                    whispering: false,
                },
                0,
            );
        }
        assert!(shared.incoming.lock().unwrap().len() <= AUDIO_BURST as usize);
    }

    #[test]
    fn spy_targets_match() {
        let f = VoiceFrame {
            protocol: Protocol::Svc,
            sender: 5,
            sender_name: "s".to_string(),
            data: vec![],
            distance: 1.0,
            whispering: false,
            group: Some(9),
        };
        assert!(SpyTarget::All.matches(&f));
        assert!(SpyTarget::Player(5).matches(&f));
        assert!(!SpyTarget::Player(6).matches(&f));
        assert!(SpyTarget::Group(9).matches(&f));
        assert!(!SpyTarget::Group(8).matches(&f));
    }

    /// The PV retry schedule grows, mirroring the mod's `PlayerInfoRequest`
    /// backoff.
    #[test]
    fn the_pv_info_retry_schedule_grows() {
        for w in PV_INFO_RETRY_MS.windows(2) {
            assert!(w[1] >= w[0], "retry delays are non-decreasing");
        }
        assert_eq!(PV_INFO_RETRY_MS.len(), 5);
    }
}

/// Server-process tests: they drive the plugin the way the tick and the
/// plugin-channel handler do, with a mock host backend, and assert on the
/// control packets that a client actually needs. These are the tests the first
/// review said were missing (a round-trip alone would not have caught a
/// missing `PlayerInfoRequest` or `KeepAlive`).
#[cfg(test)]
mod proc_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    struct MockBackend {
        sent: Mutex<Vec<(u64, String, Vec<u8>)>>,
        players: Mutex<Vec<(u64, String, u128)>>,
    }

    impl MockBackend {
        fn with_player(id: u64, name: &str, uuid: u128) -> Arc<MockBackend> {
            Arc::new(MockBackend {
                sent: Mutex::new(Vec::new()),
                players: Mutex::new(vec![(id, name.to_string(), uuid)]),
            })
        }
        fn sent(&self) -> Vec<(u64, String, Vec<u8>)> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl HostBackend for MockBackend {
        fn kv_get(&self, _: &str, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn kv_put(&self, _: &str, _: &str, _: &[u8]) {}
        fn kv_delete(&self, _: &str, _: &str) {}
        fn kv_list(&self, _: &str, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn log(&self, _: &str, _: LogLevel, _: &str) {}
        fn config(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn supports(&self, _: &str, _: &str) -> bool {
            false
        }
        fn send_plugin_message(&self, _: &str, player: u64, channel: &str, data: &[u8]) {
            self.sent
                .lock()
                .unwrap()
                .push((player, channel.to_string(), data.to_vec()));
        }
        fn player_uuid(&self, _: &str, player: u64) -> Option<u128> {
            self.players
                .lock()
                .unwrap()
                .iter()
                .find(|(id, _, _)| *id == player)
                .map(|(_, _, u)| *u)
        }
        fn online_players(&self, _: &str) -> Vec<Player> {
            self.players
                .lock()
                .unwrap()
                .iter()
                .map(|(id, name, _)| Player {
                    id: *id,
                    name: name.clone(),
                    operator: true,
                })
                .collect()
        }
    }

    const PLAYER_ID: u64 = 5;
    const PLAYER_UUID: u128 = 0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111;

    fn harness() -> (
        VoicePlugin,
        Arc<MockBackend>,
        Arc<aether_plugin::ServiceRegistry>,
        BTreeMap<String, String>,
    ) {
        let backend = MockBackend::with_player(PLAYER_ID, "Steve", PLAYER_UUID);
        let services = Arc::new(aether_plugin::ServiceRegistry::new());
        let cfg = BTreeMap::new();
        let plugin = VoicePlugin::new();
        {
            let mut vc = plugin.shared.config.write().unwrap();
            vc.svc_enabled = true;
            vc.pv_enabled = true;
        }
        (plugin, backend, services, cfg)
    }

    fn ctx<'a>(
        backend: &'a Arc<dyn HostBackend>,
        services: &'a Arc<aether_plugin::ServiceRegistry>,
        cfg: &'a BTreeMap<String, String>,
    ) -> NativeContext<'a> {
        NativeContext {
            id: ID,
            capabilities: &[],
            config: cfg,
            services,
            backend,
        }
    }

    fn player() -> Player {
        Player {
            id: PLAYER_ID,
            name: "Steve".to_string(),
            operator: true,
        }
    }

    /// On join the server must ask a Plasmo client to identify itself; the
    /// client only sends `PlayerInfo` in answer to this request.
    #[test]
    fn a_pv_client_is_asked_for_its_info_on_join() {
        let (mut plugin, backend, services, cfg) = harness();
        let dynb: Arc<dyn HostBackend> = backend.clone();
        let mut c = ctx(&dynb, &services, &cfg);
        plugin.after_player_join(&mut c, &player());
        plugin.tick(&mut c);
        let sent = backend.sent();
        assert!(
            sent.iter().any(|(id, ch, data)| *id == PLAYER_ID
                && ch == plasmo::channel::TCP
                && data == &vec![plasmo::tcp::PLAYER_INFO_REQUEST]),
            "PlayerInfoRequest must be sent on join: {sent:?}"
        );
    }

    /// The `ConnectionPacket` answer carries the wildcard IP so the client
    /// substitutes the Minecraft server's address.
    #[test]
    fn a_pv_connection_uses_the_wildcard_ip() {
        let (plugin, backend, services, cfg) = harness();
        let dynb: Arc<dyn HostBackend> = backend.clone();
        let mut c = ctx(&dynb, &services, &cfg);
        let info = plasmo::Tcp::PlayerInfo {
            voice_disabled: false,
            microphone_muted: false,
            minecraft_version: "1.21.11".to_string(),
            version: "2.1.7".to_string(),
            public_key: vec![0x30, 0x82],
        };
        plugin.on_pv_channel(&mut c, &player(), &info.encode());
        let sent = backend.sent();
        let conn = sent
            .iter()
            .find(|(_, ch, d)| ch == plasmo::channel::TCP && d[0] == plasmo::tcp::CONNECTION)
            .expect("a Connection packet");
        assert_eq!(&conn.2[19..26], b"0.0.0.0");
    }

    /// An operator's `/voice join` bypasses a group password and actually
    /// joins.
    #[test]
    fn an_admin_joins_a_password_group_without_it() {
        let (plugin, backend, services, cfg) = harness();
        let dynb: Arc<dyn HostBackend> = backend.clone();
        let mut c = ctx(&dynb, &services, &cfg);
        plugin
            .shared
            .players
            .lock()
            .unwrap()
            .entry(PLAYER_UUID)
            .or_default()
            .entity = PLAYER_ID;
        plugin.shared.groups.lock().unwrap().insert(
            1,
            SvcGroup {
                id: 1,
                name: "raid".to_string(),
                password: Some("secret".to_string()),
                kind: 0,
            },
        );
        plugin.join_svc_group(&mut c, PLAYER_UUID, 1, None, true);
        assert_eq!(
            plugin
                .shared
                .players
                .lock()
                .unwrap()
                .get(&PLAYER_UUID)
                .unwrap()
                .group,
            Some(1),
            "the admin is in the group"
        );
    }

    /// `/voice spy all` records the admin's spy target.
    #[test]
    fn the_spy_command_arms_listening() {
        let (mut plugin, backend, services, cfg) = harness();
        let dynb: Arc<dyn HostBackend> = backend.clone();
        let mut c = ctx(&dynb, &services, &cfg);
        plugin
            .shared
            .players
            .lock()
            .unwrap()
            .entry(PLAYER_UUID)
            .or_default()
            .entity = PLAYER_ID;
        plugin.on_command(
            &mut c,
            &player(),
            "voice",
            &["spy".to_string(), "all".to_string()],
        );
        assert_eq!(
            plugin.shared.spies.lock().unwrap().get(&PLAYER_UUID),
            Some(&SpyTarget::All)
        );
        plugin.on_command(
            &mut c,
            &player(),
            "voice",
            &["spy".to_string(), "off".to_string()],
        );
        assert!(plugin.shared.spies.lock().unwrap().is_empty());
    }

    /// The SVC keepalive is a valid, decodable packet for the player's secret.
    #[test]
    fn the_svc_keepalive_round_trips() {
        let secret = svc::Secret([0x33; 16]);
        let datagram = svc::Packet::KeepAlive.encode_server_udp(&secret);
        assert_eq!(
            svc::Packet::decode_server_udp(&datagram, &secret),
            Some(svc::Packet::KeepAlive)
        );
    }

    /// `/voice join` on a group that does not exist must report failure, not
    /// claim success.
    #[test]
    fn joining_a_missing_group_reports_failure() {
        let (mut plugin, backend, services, cfg) = harness();
        let dynb: Arc<dyn HostBackend> = backend.clone();
        let mut c = ctx(&dynb, &services, &cfg);
        plugin
            .shared
            .players
            .lock()
            .unwrap()
            .entry(PLAYER_UUID)
            .or_default()
            .entity = PLAYER_ID;
        let r = plugin
            .on_command(
                &mut c,
                &player(),
                "voice",
                &["join".to_string(), "nope".to_string()],
            )
            .unwrap();
        assert!(
            r.lines.iter().any(|l| l.contains("No voice group")),
            "{:?}",
            r.lines
        );
        assert!(
            plugin
                .shared
                .players
                .lock()
                .unwrap()
                .get(&PLAYER_UUID)
                .unwrap()
                .group
                .is_none(),
            "the player must not be in a group"
        );
    }
}
