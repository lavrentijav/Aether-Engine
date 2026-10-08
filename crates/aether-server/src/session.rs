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
use crate::protocol::{
    self, BlockSource, ClientEvent, JoinParams, ProtocolCodec, ServerEvent,
};
use crate::proto::{read_packet, Conn};

/// The world type this server runs: noise terrain over a persistent store, so
/// terrain and player builds survive a restart.
pub type DemoWorld =
    World<FjallStore, crate::gencache::Cached<crate::worldgen::Generator>>;

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
        1 => status(&mut s, cfg, registry),
        2 => match protocol::codec_for(protocol_id) {
            Some(codec) => login_and_play(&mut s, codec, world, cfg, next_eid, registry),
            None => protocol::kick_unsupported(&mut s, protocol_id),
        },
        _ => Ok(()),
    }
}

/// Answer a server-list ping.
fn status(s: &mut Conn, cfg: &Config, registry: &SharedRegistry) -> io::Result<()> {
    match read_packet(s)? {
        Some(p) if p.id == 0x00 => {}
        _ => return Ok(()),
    }

    // Report the newest protocol this build speaks. A client of any other
    // supported version still connects fine — the version block only drives
    // the "outdated client/server" label in the list.
    let newest = protocol::codecs()[0];
    let names: Vec<&str> = protocol::codecs().iter().map(|c| c.version_name()).collect();
    let json = format!(
        "{{\"version\":{{\"name\":\"Aether {}\",\"protocol\":{}}},\
         \"players\":{{\"max\":{},\"online\":{},\"sample\":[]}},\
         \"description\":{{\"text\":\"{}\"}}}}",
        protocol::json_escape(&names.join(" / ")),
        newest.protocol_id(),
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
    let spawn_y = find_spawn_y(world, 8, 8);
    let spawn = Vector3::new(8.5, spawn_y, 8.5);
    let player = Player::spawn(eid, name.clone(), spawn);

    let params = JoinParams {
        game_mode: cfg.server.game_mode,
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
    for cz in spawn_cz - initial..=spawn_cz + initial {
        for cx in spawn_cx - initial..=spawn_cx + initial {
            for pkt in codec.encode(&ServerEvent::ChunkColumn { cx, cz }, world) {
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
        None => handle.seed_hotbar(&codec.initial_hotbar()),
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
    stream_chunks(&handle, world, &mut loaded, (spawn_cx, spawn_cz), r);
    // Items left on the floor by an earlier session are still there; a player
    // who cannot see them cannot walk over them.
    crate::ground::show_all(&handle, world);

    let result = play_loop(s, &handle, registry, world, loaded, (spawn_cx, spawn_cz), r);

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
    mut loaded: HashSet<(i32, i32)>,
    mut last_chunk: (i32, i32),
    r: i32,
) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_millis(1000)))?;
    let mut last_keepalive = Instant::now();
    let mut keepalive_id: i64 = 1;
    let mut last_paid = Instant::now();

    loop {
        // Paid per *completed* interval, and the clock is reset rather than
        // advanced by the interval, so a stall cannot bank several payments at
        // once. Being present is the floor under the economy: it is the one
        // thing every player can do equally.
        if last_paid.elapsed() >= crate::rewards::ONLINE_INTERVAL {
            last_paid = Instant::now();
            pay_for_time(handle, world);
        }

        // Send a keep-alive roughly every 10s (clients disconnect after ~30s).
        if last_keepalive.elapsed() >= Duration::from_secs(10) {
            handle.emit(&ServerEvent::KeepAlive(keepalive_id), world);
            keepalive_id = keepalive_id.wrapping_add(1);
            last_keepalive = Instant::now();
        }

        match read_packet(s) {
            Ok(Some(pkt)) => match handle.codec.decode(&pkt, handle.pos()) {
                ClientEvent::Move(pos) => {
                    handle.set_pos(pos);
                    registry.broadcast_except(
                        handle.entity_id,
                        &ServerEvent::EntityMove(handle),
                        world,
                    );

                    // Anything of theirs on the floor comes back if they walk
                    // over it — otherwise a drop is a deletion with extra
                    // steps.
                    crate::ground::try_pickup(handle, registry, world);

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
                        stream_chunks(handle, world, &mut loaded, chunk, r);
                    }
                }
                ClientEvent::Dig { x, y, z, seq } => {
                    if handle.inspecting() {
                        inspect(handle, registry, world, x, y, z);
                    } else {
                        let broken = world.get_block(x, y, z);
                        set_and_broadcast(world, registry, handle, x, y, z, block_ids::AIR);
                        settle_neighbours(world, registry, handle, x, y, z);
                        on_block_broken(handle, registry, world, x, y, z, broken);
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
                    // The placement packet names only the hand, so the codec
                    // can only guess. What the player actually holds is known
                    // here, from the slot and creative-slot reports, and wins.
                    let held = handle
                        .held_item()
                        .and_then(|item| handle.codec.block_for_item(&item));
                    // What was clicked, before the face offset moved us into
                    // the cell in front of it.
                    let (dx, dy, dz) = crate::protocol::v47::face_offset(face);
                    let (tx, ty, tz) = (x - dx, y - dy, z - dz);

                    if handle.inspecting() {
                        inspect(handle, registry, world, x, y, z);
                    } else if let Some(crate::placement::Interaction::Toggle(next)) =
                        crate::placement::interact(world.get_block(tx, ty, tz), false)
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
                    } else if (WORLD_BOTTOM..WORLD_TOP).contains(&y) {
                        // The block is chosen by the item; the *state* is
                        // chosen here, where the player's yaw and what was in
                        // the cell are known. See `crate::placement`.
                        let pos = handle.pos();
                        let ctx = crate::placement::Context {
                            yaw: pos.yaw,
                            pitch: pos.pitch,
                            face,
                            cursor,
                            into_water: world.get_block(x, y, z) == block_ids::WATER,
                        };
                        let state =
                            crate::placement::state_for(held.unwrap_or(block), &ctx);
                        // Corner shapes are a function of the neighbours, and
                        // vanilla settles them at placement too.
                        let state = crate::placement::reshape(state, x, y, z, &|nx, ny, nz| {
                            world.get_block(nx, ny, nz)
                        });
                        // A door is two blocks; if either would land in
                        // somebody, the whole placement is refused, the way
                        // vanilla refuses it.
                        let upper = crate::placement::upper_half_of(state);
                        let buried = would_bury_a_player(registry, x, y, z, state)
                            || upper.is_some_and(|u| {
                                would_bury_a_player(registry, x, y + 1, z, u)
                            });
                        if buried {
                            // Refused, but still acked below: the ack is what
                            // makes the client drop its own prediction, so a
                            // silent refusal would leave the block standing
                            // on the client and nowhere else.
                            ack(handle, world, seq);
                            continue;
                        }
                        set_and_broadcast(world, registry, handle, x, y, z, state);
                        // And the blocks it was built against have to notice
                        // it — this is the half that was missing.
                        settle_neighbours(world, registry, handle, x, y, z);
                        // A door is two blocks that depend on each other, so
                        // the second one is placed here rather than waiting
                        // for a neighbour pass that does not exist. Refused if
                        // the cell above is occupied — vanilla refuses the
                        // whole placement there, which needs the first half
                        // undone; this at least never leaves a door standing
                        // inside something.
                        if let Some(upper) = upper {
                            if y + 1 < WORLD_TOP
                                && world.get_block(x, y + 1, z) == block_ids::AIR
                            {
                                set_and_broadcast(
                                    world, registry, handle, x, y + 1, z, upper,
                                );
                            }
                        }
                    }
                    // Sent even when the placement was refused: the ack is what
                    // makes the client apply the server's view of the block, so
                    // withholding it on a rejected placement is the one case
                    // where the client is left permanently out of step.
                    ack(handle, world, seq);
                }
                ClientEvent::HeldSlot(slot) => handle.set_held_slot(slot),
                ClientEvent::CreativeSlot { slot, item, count } => {
                    record_slot_change(handle, world, slot, item, count);
                }
                ClientEvent::Chat(text) => {
                    if let Some(reply) = crate::commands::dispatch(&text, handle, registry, world)
                    {
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
                ClientEvent::ContainerClick { slot } => {
                    if slot >= 0 {
                        for line in crate::stash::on_click(handle, registry, world, slot as usize) {
                            handle.emit(&ServerEvent::Chat(line), world);
                        }
                    }
                }
                ClientEvent::ContainerClose => {}
                ClientEvent::Ignored => {}
            },
            Ok(None) => {} // read timeout; loop to maybe send keep-alive
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                println!("[-] {} left", handle.name);
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

/// Send newly-in-range columns around `center` and unload ones that just fell
/// out of range, updating `loaded` in place.
///
/// Writes go through the handle, not the raw socket: once this player is in
/// the registry another thread may be writing to the same socket, and a large
/// chunk packet written outside the handle's mutex would interleave with it
/// and corrupt both frames.
fn stream_chunks(
    handle: &crate::players::PlayerHandle,
    world: &DemoWorld,
    loaded: &mut HashSet<(i32, i32)>,
    center: (i32, i32),
    r: i32,
) {
    let (ccx, ccz) = center;
    let wanted: HashSet<(i32, i32)> = (ccz - r..=ccz + r)
        .flat_map(|cz| (ccx - r..=ccx + r).map(move |cx| (cx, cz)))
        .collect();

    for &(cx, cz) in wanted.difference(&*loaded) {
        handle.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
    }
    for &(cx, cz) in loaded.difference(&wanted) {
        handle.emit(&ServerEvent::UnloadColumn { cx, cz }, world);
    }
    *loaded = wanted;
}

/// Write one block into the shared world and push the result to every client,
/// the actor included so a refused edit is reverted rather than left

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
fn would_bury_a_player(registry: &SharedRegistry, x: i32, y: i32, z: i32, block: BlockStateId) -> bool {
    if !aether_world::registry::blocks::props_of_state(block)
        .is_some_and(|p| p.collision)
    {
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
/// mispredicted on their screen.
fn set_and_broadcast(
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
    let updates = crate::placement::neighbour_updates(x, y, z, &|nx, ny, nz| {
        world.get_block(nx, ny, nz)
    });
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
    let where_ = aether_world::journal::Place::Inventory {
        owner: actor,
        slot,
    };
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
fn save_inventory(handle: &crate::players::PlayerHandle, world: &DemoWorld) {
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

    if crate::server_game_mode().server_grants_drops() {
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
    match econ.mint(
        aether_world::journal::ActorId(handle.uuid),
        value,
        &format!("mined {name}"),
    ) {
        Ok(()) => handle.emit(&ServerEvent::Chat(format!("+{value} for {name}")), world),
        // Silent: a player who cannot be paid should not be told about the
        // database on every swing of a pickaxe.
        Err(_) => {}
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
    for y in (0..WORLD_TOP).rev() {
        if world.get_block(x, y, z) != block_ids::AIR {
            return (y + 1) as f64;
        }
    }
    1.0
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
        assert!(!stands_in_block(p, 0, 66, 0), "1.8 tall does not reach y=66");
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
