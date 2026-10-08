//! One connection's lifetime, expressed without reference to any wire version.
//!
//! Everything here talks in [`ServerEvent`]/[`ClientEvent`] and engine block
//! ids; the client's [`ProtocolCodec`] is the only thing that knows what the
//! bytes look like. That is what lets clients of different versions share a
//! world: they run the same session logic and differ only in their codec.

use std::collections::HashSet;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use aether_api::{block_ids, FjallStore, JournalActor, Player, Vector3, World};
use aether_world::BlockStateId;

use crate::config::Config;
use crate::players::{PosLook, SharedRegistry};
use crate::proto::{read_packet, Conn};
use crate::protocol::{self, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent};

/// The world type this server runs: noise terrain over a persistent store, so
/// terrain and player builds survive a restart.
pub type DemoWorld = World<FjallStore, crate::gencache::Cached<crate::worldgen::Generator>>;

/// The engine's vertical range: `-64..=383`, 448 blocks.
///
/// Taller than vanilla's own 384. Versions whose client cannot represent a
/// negative Y are shifted by 64 rather than truncated at the bottom — see
/// `Y_OFFSET` in those codecs — so their players see the same world as
/// `0..=447`. The one exception is 1.8.9, whose 16-bit section mask makes 256
/// blocks a protocol limit rather than a choice.
const WORLD_BOTTOM: i32 = -64;
/// One past the tallest addressable block.
const WORLD_TOP: i32 = 384;

impl BlockSource for DemoWorld {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if (WORLD_BOTTOM..WORLD_TOP).contains(&y) {
            self.get_block(x, y, z)
        } else {
            block_ids::AIR
        }
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        if !(WORLD_BOTTOM >> 4..WORLD_TOP >> 4).contains(&cy) {
            out.fill(block_ids::AIR);
            return;
        }
        World::copy_section(self, cx, cy, cz, out)
    }

    fn column_biomes(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<std::sync::Arc<aether_worldgen::ColumnBiomes>> {
        self.generator().biomes(cx, cz)
    }
}

/// Serve one accepted connection: handshake, then either a status ping or a
/// full login and play session.
pub fn serve(
    stream: TcpStream,
    world: &DemoWorld,
    cfg: &Config,
    next_eid: &AtomicI32,
    registry: &SharedRegistry,
) -> io::Result<()> {
    // The handshake is always uncompressed: Set Compression cannot go out
    // before the client has told us which version it speaks.
    let mut s = Conn::new(stream);

    // Bound the handshake/login phase so a client that connects but never
    // sends anything can't pin a thread forever. The play loop later relaxes
    // this to a short poll interval for keep-alive interleaving.
    s.set_read_timeout(Some(Duration::from_secs(30)))?;

    let Some(hs) = read_packet(&mut s)? else {
        return Ok(());
    };
    if hs.id != 0x00 {
        return Ok(());
    }
    let mut pin = crate::proto::PacketIn::new(&hs.data);
    let protocol_id = pin.var_int()?;
    let _addr = pin.string()?;
    let _port = pin.u16()?;
    let next_state = pin.var_int()?;

    match next_state {
        1 => status(&mut s, protocol_id, cfg, registry),
        2 => match protocol::codec_for(protocol_id) {
            Some(codec) => login_and_play(&mut s, codec, world, cfg, next_eid, registry),
            None => protocol::kick_unsupported(&mut s, protocol_id),
        },
        _ => Ok(()),
    }
}

/// Answer a server-list ping.
fn status(s: &mut Conn, asked: i32, cfg: &Config, registry: &SharedRegistry) -> io::Result<()> {
    match read_packet(s)? {
        Some(p) if p.id == 0x00 => {}
        _ => return Ok(()),
    }

    // Echo the client's own protocol when this build speaks it, so no
    // supported client is labelled outdated; otherwise report the newest.
    // The version block only drives that label — joining is decided by the
    // handshake, not by this.
    let shown = protocol::codec_for(asked)
        .map(|_| asked)
        .unwrap_or_else(|| protocol::codecs()[0].protocol_id());
    let names: Vec<&str> = protocol::codecs()
        .iter()
        .map(|c| c.version_name())
        .collect();
    let json = format!(
        "{{\"version\":{{\"name\":\"Aether {}\",\"protocol\":{}}},\
         \"players\":{{\"max\":{},\"online\":{},\"sample\":[]}},\
         \"description\":{{\"text\":\"{}\"}}}}",
        protocol::json_escape(&names.join(" / ")),
        shown,
        cfg.server.max_players,
        registry.snapshot().len(),
        protocol::json_escape(&cfg.server.motd),
    );
    crate::proto::PacketOut::new(0x00).string(&json).send(s)?;

    // Ping (0x01, long) -> Pong (0x01, same long).
    if let Some(p) = read_packet(s)? {
        if p.id == 0x01 {
            let token = crate::proto::PacketIn::new(&p.data).i64().unwrap_or(0);
            crate::proto::PacketOut::new(0x01).i64(token).send(s)?;
        }
    }
    Ok(())
}

/// Log a player in and run their play session to disconnect.
fn login_and_play(
    s: &mut Conn,
    codec: &'static dyn ProtocolCodec,
    world: &DemoWorld,
    cfg: &Config,
    next_eid: &AtomicI32,
    registry: &SharedRegistry,
) -> io::Result<()> {
    let Some(name) = codec.read_login_start(s)? else {
        return Ok(());
    };

    // Between Login Start and Login Success, and nowhere else: every version
    // this server speaks takes Set Compression at login id 0x03, so the switch
    // belongs here rather than repeated in nineteen codecs. Both directions
    // change framing from the next packet onwards.
    if let Some(threshold) = cfg.server.compression() {
        s.enable_compression(threshold)?;
    }

    let eid = next_eid.fetch_add(1, Ordering::Relaxed);
    let world_spawn = world_spawn(world);
    let mut player = Player::spawn(eid, name.clone(), world_spawn);
    // A returning player comes back where they left, as they left.
    let saved = if codec.full_gameplay() {
        world
            .get_meta(&crate::game::player::key(player.uuid))
            .ok()
            .flatten()
            .and_then(|b| crate::game::player::decode(&b))
    } else {
        None
    };
    let spawn = saved
        .as_ref()
        .map(|s| Vector3::new(s.pos.0, s.pos.1, s.pos.2))
        .unwrap_or(world_spawn);
    if let Some(s) = &saved {
        player.yaw = s.yaw;
        player.pitch = s.pitch;
    }
    let game_mode = saved
        .as_ref()
        .map(|s| s.mode)
        .unwrap_or(cfg.server.game_mode);

    let params = JoinParams {
        game_mode,
        entity_id: eid,
        uuid: player.uuid,
        name: player.name.clone(),
        spawn: (spawn.x, spawn.y, spawn.z),
        yaw: player.yaw,
        pitch: player.pitch,
        max_players: cfg.server.max_players,
        view_radius: cfg.server.view_radius.clamp(1, 12),
        resource_pack: cfg.resource_pack.clone(),
    };

    codec.complete_login(s, &params)?;
    println!(
        "[+] {} joined (eid {}, {} via {})",
        player.name,
        eid,
        player.uuid_hyphenated(),
        codec.version_name()
    );

    // Ground under the player, and no more than that, before the join is
    // finished. Columns go out uncompressed at tens of kilobytes each, so a
    // full `view_radius` batch here is megabytes the client must swallow
    // before it is even placed — which showed up as the spawn teleport landing
    // some fifteen seconds after connecting. The rest of the radius follows
    // once the player is live, through the same streaming path that keeps up
    // with them as they walk.
    let (spawn_cx, spawn_cz) = (spawn.x.floor() as i32 >> 4, spawn.z.floor() as i32 >> 4);
    let r = params.view_radius;
    let initial = r.min(2);
    let mut loaded: HashSet<(i32, i32)> = HashSet::new();
    for pkt in codec.encode(
        &ServerEvent::SetCenterChunk {
            cx: spawn_cx,
            cz: spawn_cz,
        },
        world,
    ) {
        pkt.send(s)?;
    }
    // Generate the ground under the spawn in parallel first; sending it is
    // then only encoding.
    let ground: Vec<(i32, i32)> = (spawn_cz - initial..=spawn_cz + initial)
        .flat_map(|cz| (spawn_cx - initial..=spawn_cx + initial).map(move |cx| (cx, cz)))
        .collect();
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..gen_workers().min(ground.len()) {
            scope.spawn(|| loop {
                let i = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(&(cx, cz)) = ground.get(i) else {
                    break;
                };
                let _permit = GenPermit::acquire();
                let _ = world.get_block(cx * 16, 0, cz * 16);
            });
        }
    });
    for cz in spawn_cz - initial..=spawn_cz + initial {
        for cx in spawn_cx - initial..=spawn_cx + initial {
            let view = crate::protocol::ColumnView::new(world, cx, cz);
            for pkt in codec.encode(&ServerEvent::ChunkColumn { cx, cz }, &view) {
                pkt.send(s)?;
            }
            loaded.insert((cx, cz));
        }
    }

    codec.finish_join(s, &params)?;

    // From here on every write to this socket goes through the handle's mutex,
    // so a broadcast from another player's thread can never interleave with
    // this connection's own output.
    let pos = PosLook {
        x: spawn.x,
        y: spawn.y,
        z: spawn.z,
        yaw: player.yaw,
        pitch: player.pitch,
        on_ground: false,
    };
    let handle = registry.join(eid, player.uuid, player.name.clone(), codec, &*s, pos)?;
    // A returning player gets what they had; a new one gets the starter
    // hotbar. Seeding over a restored inventory would overwrite the first nine
    // slots of it, which is the whole hotbar.
    match load_inventory(player.uuid, world) {
        Some(saved) => handle.restore_inventory(saved),
        // A full-game client starts with nothing, as in vanilla; the older
        // ones are handed something to build with.
        None if !codec.full_gameplay() => handle.seed_hotbar(&codec.initial_hotbar()),
        None => {}
    }
    {
        let mut st = handle.game();
        st.mode = game_mode;
        st.spawn = saved.as_ref().map(|s| s.spawn).unwrap_or((
            world_spawn.x,
            world_spawn.y,
            world_spawn.z,
        ));
        if let Some(s) = &saved {
            st.health = s.health.max(1.0);
            st.food = s.food;
            st.saturation = s.saturation;
            st.xp_total = s.xp_total;
        }
    }

    // What this server answers, so the client can complete it. Sent before
    // anything the player might type at.
    handle.emit(&ServerEvent::CommandTree, world);

    // Tab list first, spawns second: a 1.8 client drops a spawn for a UUID it
    // has no list entry for, which made players invisible to each other even
    // though they always shared one world.
    handle.emit(&ServerEvent::TabListAdd(&handle), world);
    for other in registry.snapshot() {
        if other.entity_id != eid {
            handle.emit(&ServerEvent::TabListAdd(&other), world);
            handle.emit(&ServerEvent::SpawnPlayer(&other), world);
            handle.emit(&crate::game::tick::equipment_event(&other), world);
        }
    }
    registry.broadcast_except(eid, &ServerEvent::TabListAdd(&handle), world);
    registry.broadcast_except(eid, &ServerEvent::SpawnPlayer(&handle), world);
    registry.broadcast_except(
        eid,
        &ServerEvent::Chat(format!("{} joined the game", player.name)),
        world,
    );

    // Now fill out the rest of the view radius. The player is placed and
    // playable by this point, so the remaining columns arrive as scenery
    // rather than as a wall the join has to get through first.
    let streamer = Streamer::start(std::sync::Arc::clone(&handle), loaded, r);
    streamer.center((spawn_cx, spawn_cz), false);
    after_spawn(&handle, registry, world);

    let result = play_loop(s, &handle, registry, world, &streamer, (spawn_cx, spawn_cz));
    drop(streamer);

    if handle.full() {
        crate::game::window::close(&handle, world);
        crate::game::interact::cancel_dig(&handle, registry, world);
        save_inventory(&handle, world);
        save_player(&handle, world);
    }
    registry.leave(eid);
    // A stash is the result of a rollback the player just ran; it is not
    // persisted, and holding it for a player who has gone would leak. What was
    // rolled back is in the journal either way.
    crate::stash::forget(player.uuid);
    registry.broadcast_except(eid, &ServerEvent::DespawnEntity(eid), world);
    registry.broadcast_except(eid, &ServerEvent::TabListRemove(player.uuid), world);
    registry.broadcast_except(
        eid,
        &ServerEvent::Chat(format!("{} left the game", player.name)),
        world,
    );
    result
}

/// Keep the connection alive, drain client packets, mirror movement to every
/// other connected player, and stream chunks in as this player crosses into
/// view range they don't already have loaded.
fn play_loop(
    s: &mut Conn,
    handle: &crate::players::PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    streamer: &Streamer,
    mut last_chunk: (i32, i32),
) -> io::Result<()> {
    use crate::game::interact;
    s.set_read_timeout(Some(Duration::from_millis(1000)))?;
    let mut last_keepalive = Instant::now();
    let mut keepalive_id: i64 = 1;
    let mut last_paid = Instant::now();
    let mut last_saved = Instant::now();

    loop {
        // Paid per *completed* interval, and the clock is reset rather than
        // advanced by the interval, so a stall cannot bank several payments at
        // once. Being present is the floor under the economy: it is the one
        // thing every player can do equally.
        if last_paid.elapsed() >= crate::rewards::ONLINE_INTERVAL {
            last_paid = Instant::now();
            pay_for_time(handle, world);
        }
        if last_saved.elapsed() >= Duration::from_secs(30) {
            last_saved = Instant::now();
            save_player(handle, world);
        }

        // Send a keep-alive roughly every 10s (clients disconnect after ~30s).
        if last_keepalive.elapsed() >= Duration::from_secs(10) {
            handle.emit(&ServerEvent::KeepAlive(keepalive_id), world);
            keepalive_id = keepalive_id.wrapping_add(1);
            last_keepalive = Instant::now();
        }

        let pkt = match read_packet(s) {
            Ok(Some(pkt)) => pkt,
            Ok(None) => continue, // read timeout; loop to maybe send keep-alive
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                println!("[-] {} left", handle.name);
                return Ok(());
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        let full = handle.full();
        let dead = full && handle.game().dead;
        match handle.codec.decode(&pkt, handle.pos()) {
            ClientEvent::Move(pos) => {
                let before = handle.pos();
                handle.set_pos(pos);
                if full {
                    interact::on_move(handle, registry, world, before, pos);
                }
                registry.broadcast_except(
                    handle.entity_id,
                    &ServerEvent::EntityMove(handle),
                    world,
                );

                let chunk = (pos.x.floor() as i32 >> 4, pos.z.floor() as i32 >> 4);
                if chunk != last_chunk {
                    last_chunk = chunk;
                    // Move the window's centre first: columns that arrive
                    // outside it are discarded on receipt, which is what
                    // made the world look like it stopped generating.
                    handle.emit(
                        &ServerEvent::SetCenterChunk {
                            cx: chunk.0,
                            cz: chunk.1,
                        },
                        world,
                    );
                    streamer.center(chunk, false);
                }
            }
            ClientEvent::Respawn => {
                if let Some(spawn) = crate::game::combat::respawn(handle, registry, world) {
                    let chunk = (spawn.0.floor() as i32 >> 4, spawn.2.floor() as i32 >> 4);
                    last_chunk = chunk;
                    handle.emit(
                        &ServerEvent::SetCenterChunk {
                            cx: chunk.0,
                            cz: chunk.1,
                        },
                        world,
                    );
                    streamer.center(chunk, true);
                    after_spawn(handle, registry, world);
                    registry.broadcast_except(
                        handle.entity_id,
                        &ServerEvent::SpawnPlayer(handle),
                        world,
                    );
                    registry.broadcast_except(
                        handle.entity_id,
                        &crate::game::tick::equipment_event(handle),
                        world,
                    );
                    for other in registry.snapshot() {
                        if other.entity_id != handle.entity_id {
                            handle.emit(&ServerEvent::SpawnPlayer(&other), world);
                            handle.emit(&crate::game::tick::equipment_event(&other), world);
                        }
                    }
                    save_player(handle, world);
                }
            }
            _ if dead => {
                // A dead player can do nothing but press the button above.
            }
            ClientEvent::StartDig { x, y, z, seq } => {
                if handle.inspecting() {
                    inspect(handle, registry, world, x, y, z);
                } else if interact::start_dig(handle, world, x, y, z) {
                    break_block(handle, registry, world, x, y, z);
                }
                ack(handle, world, seq);
            }
            ClientEvent::CancelDig { seq, .. } => {
                interact::cancel_dig(handle, registry, world);
                ack(handle, world, seq);
            }
            ClientEvent::Dig { x, y, z, seq } => {
                if handle.inspecting() {
                    inspect(handle, registry, world, x, y, z);
                } else if !full || interact::finish_dig(handle, x, y, z) {
                    break_block(handle, registry, world, x, y, z);
                } else {
                    // Refused: put the block back on their screen.
                    let block = world.get_block(x, y, z);
                    handle.emit(&ServerEvent::BlockChange { x, y, z, block }, world);
                }
                ack(handle, world, seq);
            }
            ClientEvent::Place {
                x,
                y,
                z,
                block,
                seq,
                face,
                cursor,
            } => {
                // What was clicked, before the face offset moved us into
                // the cell in front of it.
                let (dx, dy, dz) = crate::protocol::v47::face_offset(face);
                let (tx, ty, tz) = (x - dx, y - dy, z - dz);

                if handle.inspecting() {
                    inspect(handle, registry, world, x, y, z);
                } else if full && interact::use_block(handle, world, tx, ty, tz) {
                    // Opened a crafting table, a chest, a furnace, a bed.
                } else if let Some(crate::placement::Interaction::Toggle(next)) =
                    crate::placement::interact(
                        world.get_block(tx, ty, tz),
                        full && handle.game().sneaking,
                    )
                {
                    // Using a block beats building against it — vanilla
                    // checks this first, and not doing so is why a door
                    // appeared to open and then snapped shut with the held
                    // block in front of it: the client predicted the
                    // opening, the server placed a block, and the ack
                    // reverted the prediction.
                    set_and_broadcast(world, registry, handle, tx, ty, tz, next);
                    // A door is two blocks and they must agree, or the top
                    // half stays shut while the bottom swings.
                    if let Some((oy, other)) = crate::placement::other_half(next) {
                        set_and_broadcast(world, registry, handle, tx, ty + oy, tz, other);
                    }
                } else {
                    // The placement packet names only the hand, so the codec
                    // can only guess. What the player actually holds is known
                    // here, from the slot and creative-slot reports, and wins.
                    let held_item = handle.held_item();
                    let held = held_item
                        .as_deref()
                        .and_then(|item| handle.codec.block_for_item(item));
                    let chosen = if full {
                        held
                    } else {
                        Some(held.unwrap_or(block))
                    };
                    match chosen {
                        Some(b) => {
                            // A replaceable block that was clicked is built
                            // into, not against.
                            let clicked = world.get_block(tx, ty, tz);
                            let (px, py, pz) = if is_replaceable(world, clicked) {
                                (tx, ty, tz)
                            } else {
                                (x, y, z)
                            };
                            let placed =
                                place_block(handle, registry, world, px, py, pz, b, face, cursor);
                            if placed && full {
                                interact::consumed_by_placement(handle, world);
                            } else if !placed {
                                let now = world.get_block(px, py, pz);
                                handle.emit(
                                    &ServerEvent::BlockChange {
                                        x: px,
                                        y: py,
                                        z: pz,
                                        block: now,
                                    },
                                    world,
                                );
                                handle.sync_inventory(world);
                            }
                        }
                        None => {
                            let clicked =
                                crate::game::block_name(world, world.get_block(tx, ty, tz))
                                    .unwrap_or_default();
                            let tool = held_item.clone().unwrap_or_default();
                            if let Some(to) = interact::item_on_block(&tool, &clicked) {
                                if let Some(state) =
                                    aether_world::registry::blocks::default_state(to)
                                {
                                    set_and_broadcast(world, registry, handle, tx, ty, tz, state);
                                    crate::game::combat::wear_held(handle, world, 1);
                                }
                            } else if tool == "minecraft:flint_and_steel"
                                && world.get_block(x, y, z) == block_ids::AIR
                            {
                                if let Some(fire) =
                                    aether_world::registry::blocks::default_state("minecraft:fire")
                                {
                                    set_and_broadcast(world, registry, handle, x, y, z, fire);
                                    crate::game::combat::wear_held(handle, world, 1);
                                }
                            } else {
                                let now = world.get_block(x, y, z);
                                handle.emit(
                                    &ServerEvent::BlockChange {
                                        x,
                                        y,
                                        z,
                                        block: now,
                                    },
                                    world,
                                );
                            }
                        }
                    }
                }
                // Sent even when the placement was refused: the ack is what
                // makes the client apply the server's view of the block, so
                // withholding it on a rejected placement is the one case
                // where the client is left permanently out of step.
                ack(handle, world, seq);
            }
            ClientEvent::HeldSlot(slot) => {
                handle.set_held_slot(slot);
                if full {
                    let mut st = handle.game();
                    st.eating = None;
                    st.drawing = None;
                }
            }
            ClientEvent::CreativeSlot { slot, item, count } => {
                // Only a creative player conjures items; in survival the
                // server's inventory is the truth and the report is refused.
                if !full || !handle.game().survival() {
                    record_slot_change(handle, world, slot, item, count);
                } else {
                    handle.sync_inventory(world);
                }
            }
            ClientEvent::Chat(text) => {
                if let Some(reply) = crate::commands::dispatch(&text, handle, registry, world) {
                    // Command output goes only to whoever typed it: a
                    // rollback report names players and sequence numbers
                    // and is nobody else's business.
                    for line in reply.0 {
                        handle.emit(&ServerEvent::Chat(line), world);
                    }
                    continue;
                }
                let line = format!("<{}> {}", handle.name, text);
                println!("[chat] {line}");
                let ev = ServerEvent::Chat(line);
                handle.emit(&ev, world);
                registry.broadcast_except(handle.entity_id, &ev, world);
            }
            ClientEvent::WindowClick {
                window,
                slot,
                button,
                mode,
            } => {
                if window == STASH_WINDOW {
                    if slot >= 0 && mode == 0 {
                        for line in crate::stash::on_click(handle, registry, world, slot as usize) {
                            handle.emit(&ServerEvent::Chat(line), world);
                        }
                    }
                } else {
                    crate::game::window::click(handle, world, window, slot, button, mode);
                    save_inventory(handle, world);
                }
            }
            ClientEvent::ContainerClose => {
                if full {
                    crate::game::window::close(handle, world);
                    save_inventory(handle, world);
                }
            }
            ClientEvent::DropHeld { all } => interact::drop_held(handle, world, all),
            ClientEvent::ReleaseUse => interact::release_use(handle, world),
            ClientEvent::SwapHands => interact::swap_hands(handle, world),
            ClientEvent::UseItem { hand, seq } => {
                interact::use_item(handle, registry, world, hand);
                ack(handle, world, seq);
            }
            ClientEvent::Interact { target, attack, .. } => {
                if attack {
                    crate::game::combat::player_attack(
                        handle,
                        target,
                        registry,
                        world,
                        crate::game::now(),
                    );
                } else {
                    interact::use_entity(handle, world, target);
                }
            }
            ClientEvent::Swing { hand } => {
                registry.broadcast_except(
                    handle.entity_id,
                    &ServerEvent::EntityAnimation {
                        entity_id: handle.entity_id,
                        animation: if hand == 1 { 3 } else { 0 },
                    },
                    world,
                );
            }
            ClientEvent::Sprint(on) => {
                interact::set_stance(handle, registry, world, None, Some(on))
            }
            ClientEvent::Input { sneak } => {
                interact::set_stance(handle, registry, world, Some(sneak), None)
            }
            ClientEvent::PickBlock { x, y, z } => interact::pick_block(handle, world, x, y, z),
            ClientEvent::ChangeGameMode(mode) => {
                // The switcher is the client asking; only an operator is
                // answered, the way vanilla gates it on permission level 2.
                if crate::is_operator(&handle.name) {
                    interact::set_mode(handle, world, mode);
                } else {
                    handle.emit(&ServerEvent::Chat("You are not an operator.".into()), world);
                }
            }
            ClientEvent::Ignored => {}
        }
    }
}

/// The window id the recovery stash opens under.
const STASH_WINDOW: u8 = 1;

/// Whether placing into a cell holding `b` replaces it.
fn is_replaceable(world: &DemoWorld, b: BlockStateId) -> bool {
    if b == block_ids::AIR || b == block_ids::WATER {
        return true;
    }
    let name = crate::game::block_name(world, b).unwrap_or_default();
    matches!(
        name.trim_start_matches("minecraft:"),
        "short_grass"
            | "tall_grass"
            | "fern"
            | "large_fern"
            | "dead_bush"
            | "vine"
            | "snow"
            | "seagrass"
            | "tall_seagrass"
            | "lava"
            | "water"
            | "air"
            | "cave_air"
            | "void_air"
            | "fire"
            | "short_dry_grass"
            | "tall_dry_grass"
            | "bush"
    )
}

/// Break the block at `(x, y, z)` on a player's behalf.
fn break_block(
    handle: &crate::players::PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
) {
    let broken = world.get_block(x, y, z);
    if broken == block_ids::AIR {
        return;
    }
    set_and_broadcast(world, registry, handle, x, y, z, block_ids::AIR);
    settle_neighbours(world, registry, handle, x, y, z);
    if handle.full() {
        crate::game::interact::on_broken(handle, registry, world, x, y, z, broken);
    }
    on_block_broken(handle, registry, world, x, y, z, broken);
}

/// Place `block` at `(x, y, z)` with its state chosen from how it was placed.
/// Returns whether anything was placed.
#[allow(clippy::too_many_arguments)]
fn place_block(
    handle: &crate::players::PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
    block: BlockStateId,
    face: u8,
    cursor: (f32, f32, f32),
) -> bool {
    if !(WORLD_BOTTOM..WORLD_TOP).contains(&y) {
        return false;
    }
    let here = world.get_block(x, y, z);
    if handle.full() && !is_replaceable(world, here) {
        return false;
    }
    // The block is chosen by the item; the *state* is chosen here, where
    // the player's yaw and what was in the cell are known. See
    // `crate::placement`.
    let pos = handle.pos();
    let ctx = crate::placement::Context {
        yaw: pos.yaw,
        pitch: pos.pitch,
        face,
        cursor,
        into_water: here == block_ids::WATER,
    };
    let state = crate::placement::state_for(block, &ctx);
    // Corner shapes are a function of the neighbours, and vanilla settles
    // them at placement too.
    let state =
        crate::placement::reshape(state, x, y, z, &|nx, ny, nz| world.get_block(nx, ny, nz));
    // A door is two blocks; if either would land in somebody, the whole
    // placement is refused, the way vanilla refuses it.
    let upper = crate::placement::upper_half_of(state);
    let buried = would_bury_a_player(registry, x, y, z, state)
        || upper.is_some_and(|u| would_bury_a_player(registry, x, y + 1, z, u));
    if buried {
        return false;
    }
    if let Some(u) = upper {
        if y + 1 >= WORLD_TOP || !is_replaceable(world, world.get_block(x, y + 1, z)) {
            return false;
        }
        let _ = u;
    }
    set_and_broadcast(world, registry, handle, x, y, z, state);
    // And the blocks it was built against have to notice it.
    settle_neighbours(world, registry, handle, x, y, z);
    // A door is two blocks that depend on each other, so the second one is
    // placed here rather than waiting for a neighbour pass that does not
    // exist.
    if let Some(upper) = upper {
        set_and_broadcast(world, registry, handle, x, y + 1, z, upper);
    }
    true
}

/// Everything a client needs after being placed in the world — at join and
/// after a respawn: health, inventory, time, experience.
fn after_spawn(
    handle: &crate::players::PlayerHandle,
    _registry: &SharedRegistry,
    world: &DemoWorld,
) {
    if !handle.full() {
        return;
    }
    let (health, food, saturation, mode, xp) = {
        let mut st = handle.game();
        st.sent_health = (st.health, st.food, st.saturation);
        let (level, bar) = st.xp_level();
        (
            st.health,
            st.food,
            st.saturation,
            st.mode,
            (bar, level, st.xp_total),
        )
    };
    let pos = handle.pos();
    handle.emit(
        &ServerEvent::Teleport {
            x: pos.x,
            y: pos.y,
            z: pos.z,
            yaw: pos.yaw,
            pitch: pos.pitch,
        },
        world,
    );
    handle.emit(&ServerEvent::GameModeChange(mode), world);
    handle.emit(
        &ServerEvent::Health {
            health,
            food,
            saturation,
        },
        world,
    );
    handle.emit(
        &ServerEvent::Experience {
            bar: xp.0,
            level: xp.1,
            total: xp.2,
        },
        world,
    );
    handle.emit(&crate::game::time_event(), world);
    // Permission level, as an entity event on the player's own entity: 24
    // is level 0, 28 level 4. Without it the client treats an operator as
    // anyone else — the F3+F4 switcher and F3+N refuse to open.
    let level = if crate::is_operator(&handle.name) {
        4
    } else {
        0
    };
    handle.emit(
        &ServerEvent::EntityStatus {
            entity_id: handle.entity_id,
            status: 24 + level,
        },
        world,
    );
    let held = handle.inventory().held_slot();
    handle.emit(&ServerEvent::SetHeldSlot(held), world);
    handle.sync_inventory(world);
}

/// Persist a player's position, health, hunger, experience and mode.
pub(crate) fn save_player(handle: &crate::players::PlayerHandle, world: &DemoWorld) {
    if !handle.full() {
        return;
    }
    let pos = handle.pos();
    let st = handle.game();
    let saved = crate::game::player::Saved {
        pos: (pos.x, pos.y, pos.z),
        yaw: pos.yaw,
        pitch: pos.pitch,
        health: if st.dead { 20.0 } else { st.health },
        food: if st.dead { 20 } else { st.food },
        saturation: st.saturation,
        xp_total: st.xp_total,
        mode: st.mode,
        spawn: st.spawn,
    };
    let pos = if st.dead { st.spawn } else { saved.pos };
    drop(st);
    let saved = crate::game::player::Saved { pos, ..saved };
    let _ = world.put_meta(
        &crate::game::player::key(handle.uuid),
        &crate::game::player::encode(&saved),
    );
}

/// How many columns are generated at once, across every player: one per
/// core, so several players streaming at once share the machine rather than
/// each claiming all of it.
fn gen_workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16)
    })
}

/// A slot in the server-wide generation budget of [`gen_workers`].
struct GenPermit;

fn gen_slots() -> &'static (std::sync::Mutex<usize>, std::sync::Condvar) {
    static S: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    S.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()))
}

impl GenPermit {
    fn acquire() -> Self {
        let (lock, cv) = gen_slots();
        let mut busy = lock.lock().unwrap();
        while *busy >= gen_workers() {
            busy = cv.wait(busy).unwrap();
        }
        *busy += 1;
        GenPermit
    }
}

impl Drop for GenPermit {
    fn drop(&mut self) {
        let (lock, cv) = gen_slots();
        *lock.lock().unwrap() -= 1;
        cv.notify_one();
    }
}

/// Streams columns to one player on a thread of its own.
///
/// A view radius of eight is 289 columns, and generating and encoding them
/// takes seconds. Done on the connection thread, that was seconds in which
/// nothing the player sent was read — a command typed just after joining
/// answered only once the horizon had arrived. Here the connection thread
/// only says where the player is; this thread sends the nearest missing
/// columns first and re-targets as soon as the player moves on. Writes go
/// through the handle, so they never interleave with anyone else's.
pub(crate) struct Streamer {
    tx: std::sync::mpsc::Sender<((i32, i32), bool)>,
}

impl Streamer {
    fn start(
        handle: std::sync::Arc<crate::players::PlayerHandle>,
        mut loaded: HashSet<(i32, i32)>,
        r: i32,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<((i32, i32), bool)>();
        let world = crate::game::world().expect("the game starts before anyone can connect");
        std::thread::Builder::new()
            .name(format!("chunks-{}", handle.name))
            .spawn(move || {
                let mut next: Option<((i32, i32), bool)> = None;
                loop {
                    let mut msg = match next.take() {
                        Some(m) => m,
                        None => match rx.recv() {
                            Ok(m) => m,
                            Err(_) => return,
                        },
                    };
                    while let Ok(m) = rx.try_recv() {
                        msg = (m.0, msg.1 || m.1);
                    }
                    let ((ccx, ccz), reset) = msg;
                    if reset {
                        loaded.clear();
                    }
                    let in_range =
                        |c: &(i32, i32)| (c.0 - ccx).abs() <= r && (c.1 - ccz).abs() <= r;
                    let gone: Vec<(i32, i32)> =
                        loaded.iter().filter(|c| !in_range(c)).copied().collect();
                    for (cx, cz) in gone {
                        loaded.remove(&(cx, cz));
                        handle.emit(&ServerEvent::UnloadColumn { cx, cz }, world);
                    }
                    let mut missing: Vec<(i32, i32)> = (ccz - r..=ccz + r)
                        .flat_map(|cz| (ccx - r..=ccx + r).map(move |cx| (cx, cz)))
                        .filter(|c| !loaded.contains(c))
                        .collect();
                    missing.sort_by_key(|(cx, cz)| (cx - ccx).pow(2) + (cz - ccz).pow(2));
                    // Nearest first, a batch at a time, each batch generated
                    // and encoded in parallel on the shared workers and sent
                    // as each column finishes; between batches, a new centre
                    // re-targets the rest.
                    for batch in missing.chunks(gen_workers() * 2) {
                        match rx.try_recv() {
                            Ok(m) => {
                                next = Some(m);
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                        }
                        let cursor = std::sync::atomic::AtomicUsize::new(0);
                        std::thread::scope(|scope| {
                            for _ in 0..gen_workers().min(batch.len()) {
                                scope.spawn(|| loop {
                                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                                    let Some(&(cx, cz)) = batch.get(i) else { break };
                                    let _permit = GenPermit::acquire();
                                    handle.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
                                });
                            }
                        });
                        loaded.extend(batch.iter().copied());
                    }
                }
            })
            .expect("failed to start a chunk streamer");
        Self { tx }
    }

    /// The player is now centred on `chunk`; `reset` forgets what the
    /// client had (a respawn starts it with an empty level).
    fn center(&self, chunk: (i32, i32), reset: bool) {
        let _ = self.tx.send((chunk, reset));
    }
}

/// Half the width of a player's collision box, in blocks.
///
/// A player is 0.6 wide and 1.8 tall, centred on its position.
const PLAYER_HALF_WIDTH: f64 = 0.3;
/// A player's height, in blocks.
const PLAYER_HEIGHT: f64 = 1.8;

/// Whether a solid block at `(x, y, z)` would be placed inside somebody.
///
/// Vanilla refuses such a placement outright, and a server that does not lets
/// a player seal themselves inside a block — the client predicts the block,
/// the server accepts it, and the ack shoves the player out or traps them.
/// Checked against every connected player, not only the one placing: burying
/// somebody else is the same bug pointed elsewhere.
///
/// The block is treated as a full cube. Per-state collision shapes do not
/// exist here yet (`KNOWN_ISSUES` A.6), so a slab or a stair is refused where
/// vanilla would allow it — the conservative direction, and the one that
/// cannot trap anyone.
fn would_bury_a_player(
    registry: &SharedRegistry,
    x: i32,
    y: i32,
    z: i32,
    block: BlockStateId,
) -> bool {
    if !aether_world::registry::blocks::props_of_state(block).is_some_and(|p| p.collision) {
        return false;
    }
    registry
        .snapshot()
        .iter()
        .any(|p| stands_in_block(p.pos(), x, y, z))
}

/// Whether a player standing at `pos` overlaps the cube at `(x, y, z)`.
///
/// Touching is not overlapping: a player standing exactly on top of a block
/// has `pos.y == y + 1.0` and must not count, or nobody could ever place a
/// block at their own feet.
fn stands_in_block(pos: crate::players::PosLook, x: i32, y: i32, z: i32) -> bool {
    let (bx, by, bz) = (x as f64, y as f64, z as f64);
    pos.x + PLAYER_HALF_WIDTH > bx
        && pos.x - PLAYER_HALF_WIDTH < bx + 1.0
        && pos.z + PLAYER_HALF_WIDTH > bz
        && pos.z - PLAYER_HALF_WIDTH < bz + 1.0
        && pos.y + PLAYER_HEIGHT > by
        && pos.y < by + 1.0
}
/// Write one block into the shared world and push the result to every client,
/// the actor included so a refused edit is reverted rather than left
/// mispredicted on their screen.
pub(crate) fn set_and_broadcast(
    world: &DemoWorld,
    registry: &SharedRegistry,
    handle: &crate::players::PlayerHandle,
    x: i32,
    y: i32,
    z: i32,
    block: BlockStateId,
) {
    let props = world.props_of(block);
    // Attributed, so it can be rolled back and audited. A block written
    // without an author is invisible to `/rollback` and to any later question
    // about who did what.
    world.set_block_by(JournalActor(handle.uuid), x, y, z, block, props);
    let ev = ServerEvent::BlockChange { x, y, z, block };
    handle.emit(&ev, world);
    registry.broadcast_except(handle.entity_id, &ev, world);
    // Water and lava next to the change get to react to it.
    crate::game::fluids::notify(world, x, y, z);
}

/// Re-settle the shapes around a block that just changed.
///
/// The second half of vanilla's two-stage shape rule: a fence, wall, pane or
/// stair gets its shape when it is placed, and again whenever a neighbour
/// moves. Without this, connections were one-directional — building *towards*
/// an existing fence joined up, building away from one did not, and breaking a
/// fence left its neighbour still reaching for it.
///
/// One step only. These shapes depend on the six touching cells and nothing
/// further, so a change cannot cascade, and the pass needs no queue.
fn settle_neighbours(
    world: &DemoWorld,
    registry: &SharedRegistry,
    handle: &crate::players::PlayerHandle,
    x: i32,
    y: i32,
    z: i32,
) {
    let updates =
        crate::placement::neighbour_updates(x, y, z, &|nx, ny, nz| world.get_block(nx, ny, nz));
    for (nx, ny, nz, state) in updates {
        set_and_broadcast(world, registry, handle, nx, ny, nz, state);
    }
}

/// Fold a client's slot report into the player's inventory and write what
/// changed to the journal.
///
/// This is where item provenance actually starts existing: until now the
/// ledger had no events to replay because nothing ever emitted any. A stack
/// leaving a slot and a stack arriving in it are recorded separately — as a
/// destroy and a mint — rather than as one move, because in creative they are
/// unrelated: the client conjured the new one and discarded the old, and
/// pretending otherwise would draw a provenance line between two things that
/// never touched.
fn record_slot_change(
    handle: &crate::players::PlayerHandle,
    world: &DemoWorld,
    slot: i16,
    item: Option<String>,
    count: u8,
) {
    let change = handle.set_creative_slot(slot, item, count);
    if change.is_noop() {
        return;
    }
    let actor = JournalActor(handle.uuid);
    let where_ = aether_world::journal::Place::Inventory { owner: actor, slot };
    let journal = world.journal();
    if let Some(gone) = change.removed {
        let _ = journal.append(
            actor,
            aether_world::journal::EventBody::ItemDestroy {
                uid: gone.uid,
                from: where_,
            },
        );
    }
    if let Some(now) = change.added {
        let _ = journal.append(
            actor,
            aether_world::journal::EventBody::ItemMint {
                uid: now.uid,
                item: now.item,
                count: now.count,
                to: where_,
            },
        );
    }
    save_inventory(handle, world);
}

/// Persist a player's inventory into the world's KV store.
///
/// Written on every change rather than on a timer: an inventory is small, the
/// store is an LSM that batches writes anyway, and the alternative is losing
/// whatever a player did in the last thirty seconds of a crash — which is
/// exactly the window in which they were doing something interesting.
pub(crate) fn save_inventory(handle: &crate::players::PlayerHandle, world: &DemoWorld) {
    let blob = crate::inventory::encode(&handle.inventory());
    let _ = world.put_meta(&crate::inventory::key(handle.uuid), &blob);
}

/// Read back a player's saved inventory, if there is one.
fn load_inventory(uuid: u128, world: &DemoWorld) -> Option<crate::inventory::Inventory> {
    let blob = world.get_meta(&crate::inventory::key(uuid)).ok()??;
    crate::inventory::decode(&blob)
}

/// Credit one interval of connected time.
fn pay_for_time(handle: &crate::players::PlayerHandle, world: &DemoWorld) {
    let Some(econ) = crate::economy::get() else {
        return;
    };
    let amount = crate::rewards::ONLINE_REWARD;
    if econ
        .mint(
            aether_world::journal::ActorId(handle.uuid),
            amount,
            "time online",
        )
        .is_ok()
    {
        handle.emit(
            &ServerEvent::Chat(format!(
                "+{amount} for {} minutes online",
                crate::rewards::ONLINE_INTERVAL.as_secs() / 60
            )),
            world,
        );
    }
}

/// Hand over what a broken block drops, and pay for it.
///
/// Two separate decisions that happen to share a moment:
///
/// * **The drop** is a survival-only thing. In creative the client fills its
///   own inventory and would be confused by the server pushing items into it.
/// * **The payment** is checked against the world history, so a block a player
///   placed pays nothing. Without that, placing and breaking one diamond block
///   is an infinite coin press — the cheapest exploit there is.
///
/// The history lookup runs only for blocks that are worth something, which is
/// a small minority of what a player breaks.
fn on_block_broken(
    handle: &crate::players::PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
    broken: BlockStateId,
) {
    if broken == block_ids::AIR {
        return;
    }
    let Some(name) = world.block_name_of(broken) else {
        return;
    };

    // Clients running the full game get real drops on the ground, rolled
    // from the loot tables (see `game::interact::on_broken`); the others
    // keep the older shortcut of the block going straight to them.
    if !handle.full() && crate::server_game_mode().server_grants_drops() {
        crate::ground::give_or_drop(handle, registry, world, &name, 1);
    }

    if !crate::rewards::is_payable(&name) {
        return;
    }
    if was_ever_placed(world, x, y, z) {
        return;
    }

    let Some(econ) = crate::economy::get() else {
        return;
    };
    let value = crate::rewards::block_value(&name);
    // Silent on failure: a player who cannot be paid should not be told
    // about the database on every swing of a pickaxe.
    if econ
        .mint(
            aether_world::journal::ActorId(handle.uuid),
            value,
            &format!("mined {name}"),
        )
        .is_ok()
    {
        handle.emit(&ServerEvent::Chat(format!("+{value} for {name}")), world);
    }
}

/// Whether anyone has ever *put* a block at this position.
///
/// The whole anti-exploit check, and it reduces to one question because of how
/// the journal records things: every entry carries the state it installed, so a
/// position that has only ever been emptied — broken, never filled — was filled
/// by the generator. Placing and breaking the same diamond block therefore pays
/// once, at most, and only if the generator put it there.
///
/// Two deliberate consequences:
///
/// * A block restored by a rollback counts as placed, so it does not pay a
///   second time. Conservative, and the right direction to be wrong in.
/// * An unreadable history counts as placed. Refusing to pay costs a player one
///   reward; paying on a guess would hand the economy a press.
///
/// The scan is per column and runs only for blocks worth paying for, which are
/// a small minority of what anyone breaks.
fn was_ever_placed(world: &DemoWorld, x: i32, y: i32, z: i32) -> bool {
    use aether_world::journal::EventBody;
    match world.journal().column_events(x >> 4, z >> 4) {
        Ok(events) => events.iter().any(|e| {
            matches!(
                &e.body,
                EventBody::BlockSet { x: ex, y: ey, z: ez, to, .. }
                    if (*ex, *ey, *ez) == (x, y, z) && *to != block_ids::AIR
            )
        }),
        Err(_) => true,
    }
}

/// Report a block's history to the player instead of changing it.
///
/// Which block that is depends on how they asked, and the difference is
/// useful rather than accidental:
///
/// * **Hitting** a block inspects that block — "who broke this, who put it
///   here", the common case.
/// * **Right-clicking** inspects the cell the block *would have gone into* —
///   usually air, which answers the other common question: "what used to be
///   here before someone dug it out".
///
/// Either way the client has already predicted the change it was denied, so
/// the true state is pushed back first. Without that the player is left
/// looking at a block that exists only on their screen until something else
/// happens to update it — and the ack the caller sends afterwards is what
/// actually lets the client apply this.
fn inspect(
    handle: &crate::players::PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
) {
    handle.emit(
        &ServerEvent::BlockChange {
            x,
            y,
            z,
            block: world.get_block(x, y, z),
        },
        world,
    );
    for line in crate::commands::inspect_block(handle, registry, world, x, y, z).0 {
        handle.emit(&ServerEvent::Chat(line), world);
    }
}

/// Release the client's block prediction for `seq`.
///
/// Ordered after the block update on purpose: the client holds updates for a
/// predicted position until the ack arrives, then applies them, so an ack sent
/// first would release nothing and the update behind it would be discarded.
///
/// `seq == 0` means the version predicts nothing (before 1.19); those codecs
/// encode the event as no packets at all, so this stays a plain call.
fn ack(handle: &crate::players::PlayerHandle, world: &DemoWorld, seq: i32) {
    if seq != 0 {
        handle.emit(&ServerEvent::AckBlockChange(seq), world);
    }
}

/// The topmost non-air block's Y, plus one, at world column `(x, z)`.
fn find_spawn_y(world: &DemoWorld, x: i32, z: i32) -> f64 {
    for y in (WORLD_BOTTOM..WORLD_TOP).rev() {
        if world.get_block(x, y, z) != block_ids::AIR {
            return (y + 1) as f64;
        }
    }
    1.0
}

/// Where new players appear: the first dry column found spiralling out
/// from the origin, so nobody starts in the middle of an ocean.
fn world_spawn(world: &DemoWorld) -> Vector3 {
    static SPAWN: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();
    let (x, z) = *SPAWN.get_or_init(|| {
        for ring in 0..16i32 {
            let r = ring * 16;
            for (dx, dz) in [
                (0, 0),
                (r, 0),
                (-r, 0),
                (0, r),
                (0, -r),
                (r, r),
                (-r, -r),
                (r, -r),
                (-r, r),
            ] {
                let (x, z) = (8 + dx, 8 + dz);
                let y = find_spawn_y(world, x, z) as i32;
                let top = world.get_block(x, y - 1, z);
                if top != block_ids::WATER && world.props_of(top).solid {
                    return (x, z);
                }
            }
        }
        (8, 8)
    });
    Vector3::new(x as f64 + 0.5, find_spawn_y(world, x, z), z as f64 + 0.5)
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::players::PosLook;

    fn at(x: f64, y: f64, z: f64) -> PosLook {
        PosLook {
            x,
            y,
            z,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        }
    }

    #[test]
    fn a_player_overlaps_the_two_blocks_they_occupy() {
        // Standing on top of block y=63, so occupying y=64 and y=65.
        let p = at(0.5, 64.0, 0.5);
        assert!(stands_in_block(p, 0, 64, 0), "their feet");
        assert!(stands_in_block(p, 0, 65, 0), "their head");
        assert!(
            !stands_in_block(p, 0, 66, 0),
            "1.8 tall does not reach y=66"
        );
    }

    #[test]
    fn the_block_underfoot_is_placeable() {
        // The regression this guards: a player standing exactly on y=64.0 must
        // not be counted as inside the block below them, or they could never
        // place at their own feet — including pillaring up.
        let p = at(0.5, 64.0, 0.5);
        assert!(!stands_in_block(p, 0, 63, 0));
    }

    #[test]
    fn a_player_half_over_an_edge_occupies_both_columns() {
        // 0.6 wide, so standing at x = 1.0 straddles x=0 and x=1.
        let p = at(1.0, 64.0, 0.5);
        assert!(stands_in_block(p, 0, 64, 0));
        assert!(stands_in_block(p, 1, 64, 0));
        assert!(!stands_in_block(p, 2, 64, 0));
    }

    #[test]
    fn standing_clear_of_a_cell_leaves_it_free() {
        let p = at(0.5, 64.0, 0.5);
        assert!(!stands_in_block(p, 5, 64, 5));
        assert!(!stands_in_block(p, 0, 60, 0), "well below them");
    }
}
