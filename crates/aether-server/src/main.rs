//! `aether-server` — a minimal **Minecraft 1.8.9 (protocol 47)** server that
//! lets a vanilla client connect and spawn in a full-bright flat world.
//!
//! Scope is deliberately small — offline mode, no compression, no encryption,
//! a superflat world, creative game mode, and **light pinned to maximum**
//! ([`aether_world::FullBright`]). It exercises the real engine world for chunk
//! data and assigns each connection a [`aether_api::Player`] entity.
//!
//! This targets 1.8.9 specifically; connect with that client version.

mod chunk;
mod config;
mod proto;

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_api::{block_ids, FlatGenerator, MemStore, Player, Vector3, World};
use aether_world::BlockStateId;

use config::Config;
use proto::{read_packet, PacketIn, PacketOut};

/// Protocol version we speak (Minecraft 1.8 – 1.8.9).
const PROTOCOL: i32 = 47;

type DemoWorld = World<MemStore, FlatGenerator>;

fn main() -> std::process::ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "aether-server.toml".to_string());
    let (cfg, created) = match Config::load_or_init(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config error: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if created {
        println!("No config at `{path}` — wrote a sample there; using defaults.\n");
    }

    let world: Arc<DemoWorld> = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
    let cfg = Arc::new(cfg);
    let next_eid = Arc::new(AtomicI32::new(1));

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind {addr}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("╔══════════════════════════════════════════════╗");
    println!("║        Aether Engine — 1.8.9 server          ║");
    println!("╚══════════════════════════════════════════════╝");
    println!("listening  : {addr}");
    println!("protocol   : {PROTOCOL} (Minecraft 1.8.x)");
    println!("world      : superflat, full-bright, creative");
    println!("connect with a 1.8.9 client. Ctrl-C to stop.\n");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let world = Arc::clone(&world);
                let cfg = Arc::clone(&cfg);
                let next_eid = Arc::clone(&next_eid);
                std::thread::spawn(move || {
                    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                    if let Err(e) = handle(s, &world, &cfg, &next_eid) {
                        if e.kind() != io::ErrorKind::UnexpectedEof {
                            eprintln!("[{peer}] disconnected: {e}");
                        }
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    std::process::ExitCode::SUCCESS
}

fn handle(
    mut s: TcpStream,
    world: &DemoWorld,
    cfg: &Config,
    next_eid: &AtomicI32,
) -> io::Result<()> {
    // --- Handshake ---
    let hs = match read_packet(&mut s)? {
        Some(p) if p.id == 0x00 => p,
        _ => return Ok(()),
    };
    let mut pin = PacketIn::new(&hs.data);
    let _proto = pin.var_int()?;
    let _addr = pin.string()?;
    let _port = pin.u16()?;
    let next_state = pin.var_int()?;

    match next_state {
        1 => status(&mut s, cfg),
        2 => login_and_play(&mut s, world, cfg, next_eid),
        _ => Ok(()),
    }
}

// --- Status (server-list ping) ---
fn status(s: &mut TcpStream, cfg: &Config) -> io::Result<()> {
    // Request (0x00, empty).
    match read_packet(s)? {
        Some(p) if p.id == 0x00 => {}
        _ => return Ok(()),
    }
    let motd = json_escape(&cfg.server.motd);
    let json = format!(
        "{{\"version\":{{\"name\":\"Aether 1.8.9\",\"protocol\":{PROTOCOL}}},\
         \"players\":{{\"max\":{},\"online\":0,\"sample\":[]}},\
         \"description\":{{\"text\":\"{motd}\"}}}}",
        cfg.server.max_players
    );
    PacketOut::new(0x00).string(&json).send(s)?;

    // Ping (0x01, long) -> Pong (0x01, same long).
    if let Some(p) = read_packet(s)? {
        if p.id == 0x01 {
            let mut pin = PacketIn::new(&p.data);
            let token = pin.i64().unwrap_or(0);
            PacketOut::new(0x01).i64(token).send(s)?;
        }
    }
    Ok(())
}

// --- Login + Play ---
fn login_and_play(
    s: &mut TcpStream,
    world: &DemoWorld,
    cfg: &Config,
    next_eid: &AtomicI32,
) -> io::Result<()> {
    // Login Start (0x00): username.
    let ls = match read_packet(s)? {
        Some(p) if p.id == 0x00 => p,
        _ => return Ok(()),
    };
    let name = PacketIn::new(&ls.data).string()?;

    let eid = next_eid.fetch_add(1, Ordering::Relaxed);
    let spawn = Vector3::new(8.5, 5.0, 8.5);
    let player = Player::spawn(eid, name.clone(), spawn);

    // Login Success (0x02): uuid, name. (No compression/encryption.)
    PacketOut::new(0x02)
        .string(&player.uuid_hyphenated())
        .string(&player.name)
        .send(s)?;

    println!(
        "[+] {} joined (eid {}, uuid {})",
        player.name,
        player.entity_id,
        player.uuid_hyphenated()
    );

    // Join Game (0x01).
    PacketOut::new(0x01)
        .i32(eid)
        .u8(player.game_mode.as_u8()) // creative
        .u8(0) // dimension: overworld
        .u8(0) // difficulty: peaceful
        .u8(cfg.server.max_players.min(255) as u8)
        .string("flat")
        .bool(false) // reduced debug info
        .send(s)?;

    // Player Abilities (0x39): invulnerable + fly + allow-fly + creative.
    PacketOut::new(0x39)
        .u8(0x0F)
        .f32(0.05) // flying speed
        .f32(0.10) // field-of-view / walk speed
        .send(s)?;

    // Stream chunks around spawn (chunk 0,0).
    let get = |x: i32, y: i32, z: i32| -> BlockStateId {
        if (0..256).contains(&y) {
            world.get_block(x, y, z)
        } else {
            block_ids::AIR
        }
    };
    let r = cfg.server.view_radius.clamp(1, 12);
    for cz in -r..=r {
        for cx in -r..=r {
            chunk::chunk_data_packet(cx, cz, &get).send(s)?;
        }
    }

    // Spawn Position (0x05) and where the player actually appears.
    PacketOut::new(0x05).i64(encode_position(8, 4, 8)).send(s)?;
    PacketOut::new(0x08)
        .f64(spawn.x)
        .f64(spawn.y)
        .f64(spawn.z)
        .f32(player.yaw)
        .f32(player.pitch)
        .u8(0) // all absolute
        .send(s)?;

    play_loop(s, &player)
}

/// Keep the connection alive and drain client packets.
fn play_loop(s: &mut TcpStream, player: &Player) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_millis(1000)))?;
    let mut last_keepalive = Instant::now();
    let mut keepalive_id: i32 = 1;

    loop {
        // Send a keep-alive roughly every 10s (client disconnects after ~30s).
        if last_keepalive.elapsed() >= Duration::from_secs(10) {
            PacketOut::new(0x00).var_int(keepalive_id).send(s)?;
            keepalive_id = keepalive_id.wrapping_add(1);
            last_keepalive = Instant::now();
        }

        match read_packet(s) {
            Ok(Some(_pkt)) => {
                // We accept and ignore client play packets (movement, chat,
                // keep-alive responses) — enough to stay connected.
            }
            Ok(None) => {} // read timeout; loop to maybe send keep-alive
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                println!("[-] {} left", player.name);
                return Ok(());
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Encode a block position into a 1.8 packed `i64`.
fn encode_position(x: i64, y: i64, z: i64) -> i64 {
    ((x & 0x3FF_FFFF) << 38) | ((y & 0xFFF) << 26) | (z & 0x3FF_FFFF)
}

/// Minimal JSON string escaping for the MOTD.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_encoding_matches_1_8() {
        // Round-trip a couple of values against the documented layout.
        let enc = encode_position(8, 4, 8);
        let x = (enc >> 38) as i32;
        let y = ((enc << 26) >> 52) as i32; // sign-extend 12-bit middle field
        let z = ((enc << 38) >> 38) as i32;
        assert_eq!(x, 8);
        assert_eq!(y, 4);
        assert_eq!(z, 8);
    }

    #[test]
    fn json_escape_handles_quotes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
