//! Shared player registry so independent per-connection threads can see and
//! broadcast to each other — the minimum needed for players to see other
//! players move, join and leave.
//!
//! Each connection thread owns its `TcpStream` for *reading*. On join it
//! hands a `try_clone()`d write half to the registry, drained by the
//! connection's own writer thread ([`crate::outbox`]); every write after that
//! point — this player's own keep-alives included — is a push onto that
//! queue. An event's packets are queued together, so a broadcast from another
//! thread never interleaves with this connection's own output, and a push
//! never waits on the network: a client that stops reading is disconnected
//! instead of stalling whoever was talking to it.
//!
//! A handle also carries the **codec** its client connected with. Broadcasts
//! therefore take a version-neutral [`ServerEvent`] rather than finished
//! bytes, and each recipient renders it in their own dialect — which is how
//! a 1.8 and a 1.21 client can share one world.

use crate::outbox::Outbox;
use crate::proto::Conn;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::protocol::{BlockSource, ProtocolCodec, ServerEvent};

/// Position + look, updated as the client moves.
#[derive(Debug, Clone, Copy)]
pub struct PosLook {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

/// No resource-pack status has arrived yet (also set for a codec that cannot
/// report one). The plain compat stand-in is used.
const PACK_UNKNOWN: u8 = 0;
/// The client reported the compat pack `successfully_loaded`; carrier states
/// may be sent.
const PACK_LOADED: u8 = 1;
/// The client declined the pack, or its download failed. Stand-ins stay.
const PACK_NOT_LOADED: u8 = 2;

/// One connected player, as seen by every other connection thread.
pub struct PlayerHandle {
    pub entity_id: i32,
    pub uuid: u128,
    pub name: String,
    /// The address the client connected from.
    pub ip: Option<std::net::IpAddr>,
    /// Skin/cape properties from Mojang, empty for an offline login. Sent in
    /// the tab list so other clients render the right skin.
    pub properties: Vec<crate::auth::api::Property>,
    /// The wire dialect this particular client speaks.
    pub codec: &'static dyn ProtocolCodec,
    outbox: Arc<Outbox>,
    pos: Mutex<PosLook>,
    inventory: Mutex<crate::inventory::Inventory>,
    /// Inspection mode and the last query's paged output. See
    /// [`crate::commands`].
    session: Mutex<crate::commands::Session>,
    /// Survival state: health, hunger, the open window, the cursor.
    ///
    /// Lock order, everywhere: this, then the inventory, then the block
    /// entities. The tick thread and the connection thread both take these.
    game: Mutex<crate::game::player::PlayerState>,
    /// Entities (not players) this client currently has spawned.
    tracked: Mutex<std::collections::HashSet<i32>>,
    /// Anti-xray / visibility-culling bookkeeping for this client. See
    /// [`crate::visibility`].
    visibility: Mutex<crate::visibility::State>,
    /// Did this client load the resource pack? `0` unknown, `1` loaded, `2` not
    /// loaded (declined or failed). Read on every chunk render, so a plain
    /// atomic rather than a lock. See `compat::pack`.
    pack: AtomicU8,
    /// Whether the pack this client was offered was *the compat pack*. The
    /// status reply says a pack loaded, not which one, and the operator's plain
    /// `[resource_pack]` would otherwise satisfy the gate and turn carriers on
    /// without the carrier textures — so carriers require both this and the
    /// loaded status. Set once, at join, from the offer the codec was handed.
    compat_pack: AtomicBool,
}

impl PlayerHandle {
    /// Current position/look.
    pub fn pos(&self) -> PosLook {
        *self.pos.lock().unwrap()
    }

    /// Record a new position/look (called by the owning connection thread).
    pub fn set_pos(&self, p: PosLook) {
        *self.pos.lock().unwrap() = p;
    }

    /// Borrow this player's command session (inspect mode, current pager).
    pub fn session(&self) -> std::sync::MutexGuard<'_, crate::commands::Session> {
        self.session.lock().unwrap()
    }

    /// Whether the player is in inspect mode, where digging and placing read
    /// the history instead of changing the world.
    pub fn inspecting(&self) -> bool {
        self.session.lock().unwrap().inspecting
    }

    /// Borrow this player's survival state.
    pub fn game(&self) -> std::sync::MutexGuard<'_, crate::game::player::PlayerState> {
        self.game.lock().unwrap()
    }

    /// Borrow the set of entities this client has spawned.
    pub fn tracked(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<i32>> {
        self.tracked.lock().unwrap()
    }

    /// Borrow this client's visibility-culling state.
    pub fn visibility(&self) -> std::sync::MutexGuard<'_, crate::visibility::State> {
        self.visibility.lock().unwrap()
    }

    /// Whether this client runs the full, server-authoritative gameplay.
    pub fn full(&self) -> bool {
        self.codec.full_gameplay()
    }

    /// Send the player's whole inventory window (and the open window, if
    /// any) as the server sees it.
    pub fn sync_inventory(&self, world: &dyn BlockSource) {
        if self.full() {
            crate::game::window::sync(self, world);
        }
    }

    /// Borrow this player's inventory.
    pub fn inventory(&self) -> std::sync::MutexGuard<'_, crate::inventory::Inventory> {
        self.inventory.lock().unwrap()
    }

    /// Replace this player's inventory wholesale — used once, on join, to
    /// restore what was saved.
    pub fn restore_inventory(&self, inv: crate::inventory::Inventory) {
        *self.inventory.lock().unwrap() = inv;
    }

    /// Record the hotbar slot the player just selected.
    pub fn set_held_slot(&self, slot: u8) {
        self.inventory.lock().unwrap().select(slot);
    }

    /// Record what the client put in one of its own inventory slots.
    ///
    /// Every slot is kept now, not just the hotbar: the whole window is what
    /// gets saved and restored, and an inventory that remembered only nine
    /// slots would silently eat the rest on every logout.
    ///
    /// Returns what changed, so the caller can write it to the journal.
    pub fn set_creative_slot(
        &self,
        slot: i16,
        item: Option<aether_world::ItemTypeId>,
        count: u8,
    ) -> crate::inventory::SlotChange {
        let Ok(index) = usize::try_from(slot) else {
            return crate::inventory::SlotChange::default();
        };
        self.inventory
            .lock()
            .unwrap()
            .set(index, item.map(|i| (i, count)))
    }

    /// Record the hotbar the server handed this player at join.
    pub fn seed_hotbar(&self, items: &[(String, u8)]) {
        let mut inv = self.inventory.lock().unwrap();
        for (i, (name, count)) in items.iter().enumerate() {
            inv.set(
                crate::inventory::FIRST_HOTBAR + i,
                Some((crate::inventory::item(name), *count)),
            );
        }
    }

    /// The item in the selected hotbar slot.
    pub fn held_item(&self) -> Option<aether_world::ItemTypeId> {
        self.inventory.lock().unwrap().held().map(|s| s.item)
    }

    /// Render `ev` with this player's codec and queue it for their socket.
    ///
    /// Encoding and framing happen in the caller and the event's packets are
    /// queued as one unit, so an event that becomes several packets cannot be
    /// split apart by another thread's broadcast. Never waits on the network:
    /// a client too far behind is disconnected by the push (see
    /// [`crate::outbox`]), and its own thread then cleans up the registry.
    pub fn emit(&self, ev: &ServerEvent, world: &dyn BlockSource) {
        let frames = self.render(ev, world);
        self.outbox.push(frames);
    }

    /// [`Self::emit`] for a sender with a lot to say to this one client — its
    /// chunk stream — which waits while the client is behind rather than
    /// queueing without bound.
    pub fn emit_paced(&self, ev: &ServerEvent, world: &dyn BlockSource) {
        let frames = self.render(ev, world);
        self.outbox.push_paced(frames);
    }

    fn render(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<Vec<u8>> {
        let legacy = if self.full() { None } else { ev.legacy() };
        let ev = legacy.as_ref().unwrap_or(ev);
        // The compat pack's carrier remap, only for a client that loaded the
        // pack. A disguised block renders as a carrier state the codec then
        // translates to this release exactly as it would any other block.
        let table = self.pack_table();
        let packets = match ev {
            // Every codec reads a column block by block; give it a view that
            // copies each section once instead of locking per block.
            ServerEvent::ChunkColumn { cx, cz } => {
                crate::web::note_chunk_send(*cx, *cz);
                let carrier_view;
                let base: &dyn BlockSource = match table {
                    Some(t) => {
                        carrier_view = crate::protocol::compat::pack::CarrierView::new(world, t);
                        &carrier_view
                    }
                    None => world,
                };
                let view = crate::protocol::ColumnView::new(base, *cx, *cz);
                // Anti-xray: mask the sections this player cannot see before
                // the codec ever reads them, so every version's cheapest
                // single-valued stone form falls out of the existing encoder.
                let hidden = crate::visibility::hidden_mask_for_render(self, world, *cx, *cz);
                if hidden != 0 {
                    let fill = crate::visibility::fill_for(self);
                    let masked =
                        crate::visibility::MaskedColumn::new(&view, *cx, *cz, hidden, fill);
                    self.codec.encode(ev, &masked)
                } else {
                    self.codec.encode(ev, &view)
                }
            }
            ServerEvent::UnloadColumn { cx, cz } => {
                crate::visibility::forget_column(self, *cx, *cz);
                self.codec.encode(ev, world)
            }
            // A single block change must be a carrier state too, or a placed
            // or mined block would flip back to the stand-in.
            ServerEvent::BlockChange { x, y, z, block } if table.is_some() => {
                let t = table.expect("guarded by the match arm");
                let remapped = ServerEvent::BlockChange {
                    x: *x,
                    y: *y,
                    z: *z,
                    block: t.display(*block),
                };
                self.codec.encode(&remapped, world)
            }
            _ => self.codec.encode(ev, world),
        };
        let threshold = self.outbox.threshold();
        packets.iter().map(|p| p.frame(threshold)).collect()
    }

    /// Record that the pack this client was offered is the compat pack, so a
    /// later `successfully_loaded` status may turn carriers on. Called once, at
    /// join, from the offer the codec was handed.
    pub fn set_compat_pack_offered(&self) {
        self.compat_pack.store(true, Ordering::Relaxed);
    }

    /// Whether the compat pack was the pack offered to this client.
    pub fn compat_pack_offered(&self) -> bool {
        self.compat_pack.load(Ordering::Relaxed)
    }

    /// Record the client's resource-pack status and report whether this
    /// *newly* turned carriers on (so the caller can re-send the view once).
    ///
    /// `status` is the protocol's own enum. Being `accepted`/`downloaded` is
    /// not enough: only a `successfully_loaded` client can render a carrier
    /// state, so anything but `Loaded` keeps — or marks — the player on the
    /// plain stand-in.
    pub fn set_pack_status(&self, status: crate::protocol::PackStatus) -> bool {
        use crate::protocol::PackStatus;
        match status {
            PackStatus::Loaded => self.pack.swap(PACK_LOADED, Ordering::Relaxed) == PACK_UNKNOWN,
            PackStatus::Declined | PackStatus::Failed | PackStatus::InvalidUrl => {
                self.pack.store(PACK_NOT_LOADED, Ordering::Relaxed);
                false
            }
            // Accepted/downloaded are in-progress; an unknown value changes
            // nothing.
            _ => false,
        }
    }

    /// Whether this client has the compat pack loaded, so carrier block states
    /// are safe to send.
    pub fn pack_loaded(&self) -> bool {
        self.pack.load(Ordering::Relaxed) == PACK_LOADED
    }

    /// This client's carrier table, if the **compat** pack was offered to it,
    /// it loaded it, and its release has a table. `None` — the common case —
    /// means "render the plain stand-in".
    ///
    /// Requiring [`Self::compat_pack_offered`] and not only [`Self::pack_loaded`]
    /// is the point: the operator's plain `[resource_pack]` also reports loaded,
    /// and its client has none of the carrier textures.
    fn pack_table(&self) -> Option<&'static crate::protocol::compat::pack::PackTable> {
        (self.compat_pack_offered() && self.pack_loaded())
            .then(|| crate::protocol::compat::pack::table_for(self.codec.protocol_id()))
            .flatten()
    }

    /// Disconnect this player: their socket is shut down and their own thread
    /// leaves the game. The first reason given is the one logged.
    pub fn disconnect(&self, reason: &str) {
        self.outbox.close(reason);
    }

    /// Why this player was disconnected, if they have been.
    pub fn disconnected(&self) -> Option<String> {
        self.outbox.closed()
    }

    /// Bytes queued for this player's socket and not yet written.
    pub fn queued_bytes(&self) -> usize {
        self.outbox.queued()
    }
}

/// All connected players, keyed by entity id.
#[derive(Default)]
pub struct Registry {
    players: RwLock<HashMap<i32, Arc<PlayerHandle>>>,
}

pub type SharedRegistry = Arc<Registry>;

impl Registry {
    /// Register a newly logged-in player and return its shared handle.
    pub fn join(
        &self,
        entity_id: i32,
        uuid: u128,
        name: String,
        ip: Option<std::net::IpAddr>,
        properties: Vec<crate::auth::api::Property>,
        codec: &'static dyn ProtocolCodec,
        stream: &Conn,
        pos: PosLook,
    ) -> io::Result<Arc<PlayerHandle>> {
        // Clones the compression state along with the socket: a broadcast must
        // be framed exactly as the owning thread's own writes are, and the
        // switch has already happened by the time a player reaches the
        // registry.
        let outbox = Outbox::start(stream.try_clone()?, &name)?;
        let handle = Arc::new(PlayerHandle {
            entity_id,
            uuid,
            name,
            ip,
            properties,
            codec,
            outbox,
            pos: Mutex::new(pos),
            inventory: Mutex::new(crate::inventory::Inventory::new()),
            session: Mutex::new(crate::commands::Session::default()),
            game: Mutex::new(crate::game::player::PlayerState::new(
                crate::server_game_mode(),
            )),
            tracked: Mutex::new(std::collections::HashSet::new()),
            visibility: Mutex::new(crate::visibility::State::default()),
            pack: AtomicU8::new(PACK_UNKNOWN),
            compat_pack: AtomicBool::new(false),
        });
        self.players
            .write()
            .unwrap()
            .insert(entity_id, Arc::clone(&handle));
        Ok(handle)
    }

    /// Remove a player on disconnect.
    pub fn leave(&self, entity_id: i32) {
        self.players.write().unwrap().remove(&entity_id);
    }

    /// Snapshot of everyone currently online (e.g. to spawn them for a new
    /// joiner without holding the registry lock while writing to sockets).
    pub fn snapshot(&self) -> Vec<Arc<PlayerHandle>> {
        self.players.read().unwrap().values().cloned().collect()
    }

    /// Send an event to everyone except `exclude`, encoded per recipient.
    /// The UUID of the online player called `name`, case-insensitively.
    ///
    /// Only online players: an offline player's UUID would have to come from a
    /// profile cache this server does not keep, and guessing one would roll
    /// back a stranger's work.
    pub fn uuid_of(&self, name: &str) -> Option<u128> {
        self.players
            .read()
            .unwrap()
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.uuid)
    }

    /// The online player with this UUID.
    pub fn by_uuid(&self, uuid: u128) -> Option<std::sync::Arc<PlayerHandle>> {
        self.players
            .read()
            .unwrap()
            .values()
            .find(|p| p.uuid == uuid)
            .map(std::sync::Arc::clone)
    }

    /// The name of the online player with this UUID.
    ///
    /// Only online players. A history line about someone who has logged off
    /// falls back to a short hex id rather than inventing a name.
    pub fn name_of(&self, uuid: u128) -> Option<String> {
        self.players
            .read()
            .unwrap()
            .values()
            .find(|p| p.uuid == uuid)
            .map(|p| p.name.clone())
    }

    /// Send `ev` to everyone.
    pub fn broadcast(&self, ev: &ServerEvent, world: &dyn BlockSource) {
        // `i32::MIN` is not a real entity id, and the ground's drops count
        // *down* from -1,000,000, so excluding nobody has to be spelled this
        // way rather than by passing a plausible id.
        self.broadcast_except(i32::MIN, ev, world);
    }

    /// The online player with this entity id.
    pub fn by_entity(&self, entity_id: i32) -> Option<Arc<PlayerHandle>> {
        self.players.read().unwrap().get(&entity_id).cloned()
    }

    /// Send `ev` to everyone whose client has entity `entity_id` spawned.
    pub fn broadcast_tracking(&self, entity_id: i32, ev: &ServerEvent, world: &dyn BlockSource) {
        for handle in self.snapshot() {
            if handle.tracked().contains(&entity_id) {
                handle.emit(ev, world);
            }
        }
    }

    pub fn broadcast_except(&self, exclude: i32, ev: &ServerEvent, world: &dyn BlockSource) {
        for handle in self.snapshot() {
            if handle.entity_id != exclude {
                handle.emit(ev, world);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::read_packet_from;
    use crate::protocol::{codec_for, BlockSource};
    use aether_world::registry::blocks::default_state;
    use aether_world::BlockStateId;
    use std::net::{TcpListener, TcpStream};

    /// A `BlockSource` with nothing in it; the render test only uses
    /// `BlockChange`, which carries its own block.
    struct Empty;
    impl BlockSource for Empty {
        fn block_at(&self, _: i32, _: i32, _: i32) -> BlockStateId {
            aether_api::block_ids::AIR
        }
    }

    fn handle(codec: &'static dyn ProtocolCodec) -> Arc<PlayerHandle> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let _peer = listener.accept().unwrap();
        Registry::default()
            .join(
                1,
                0,
                "tester".into(),
                None,
                Vec::new(),
                codec,
                &client,
                PosLook {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                },
            )
            .unwrap()
    }

    /// The block-state VarInt of the first framed packet, which for a
    /// `BlockChange` is `id` then `i64 position` then the state.
    fn block_change_state(handle: &PlayerHandle) -> i32 {
        let block = default_state("minecraft:pale_oak_planks").unwrap();
        let ev = ServerEvent::BlockChange {
            x: 0,
            y: 0,
            z: 0,
            block,
        };
        let frames = handle.render(&ev, &Empty);
        let mut cur = frames[0].as_slice();
        let pkt = read_packet_from(&mut cur, None).unwrap().unwrap();
        let mut pin = crate::proto::PacketIn::new(&pkt.data);
        let _pos = pin.i64().unwrap();
        pin.var_int().unwrap()
    }

    #[test]
    fn a_pack_bearing_client_sees_the_carrier_and_others_the_stand_in() {
        let codec = codec_for(763).unwrap();
        let table = crate::protocol::compat::pack::table_for(763).unwrap();
        let block = default_state("minecraft:pale_oak_planks").unwrap();
        let carrier_wire = crate::protocol::compat::V1_20_1.state(table.carrier(block).unwrap());

        // Before the pack is loaded: the plain stand-in, never the carrier.
        let early = handle(codec);
        assert!(!early.pack_loaded());
        assert_ne!(block_change_state(&early), carrier_wire as i32);

        // The compat pack was offered and loaded: the carrier state this
        // release draws the real block on.
        use crate::protocol::PackStatus;
        let loaded = handle(codec);
        loaded.set_compat_pack_offered();
        assert!(
            loaded.set_pack_status(PackStatus::Loaded),
            "the first load is a transition"
        );
        assert!(
            !loaded.set_pack_status(PackStatus::Loaded),
            "a repeat load is not"
        );
        assert!(loaded.pack_loaded());
        assert_eq!(block_change_state(&loaded), carrier_wire as i32);

        // Accepted-but-not-loaded, and declined, both stay on the stand-in.
        for status in [
            PackStatus::Accepted,
            PackStatus::Downloaded,
            PackStatus::Declined,
            PackStatus::Failed,
            PackStatus::InvalidUrl,
            PackStatus::Unknown,
        ] {
            let h = handle(codec);
            h.set_pack_status(status);
            assert!(!h.pack_loaded(), "{status:?} is not successfully_loaded");
            assert_ne!(block_change_state(&h), carrier_wire as i32);
        }
    }

    #[test]
    fn a_client_that_loaded_a_different_pack_never_sees_carriers() {
        // Review 1 blocker: the operator's plain `[resource_pack]` also reports
        // loaded, but its client has none of the carrier textures. Only a
        // client the *compat* pack was offered may be remapped, even when its
        // status says a pack loaded.
        let codec = codec_for(763).unwrap();
        let table = crate::protocol::compat::pack::table_for(763).unwrap();
        let block = default_state("minecraft:pale_oak_planks").unwrap();
        let carrier_wire = crate::protocol::compat::V1_20_1.state(table.carrier(block).unwrap());

        let h = handle(codec);
        assert!(!h.compat_pack_offered(), "the plain pack was offered");
        h.set_pack_status(crate::protocol::PackStatus::Loaded);
        assert!(h.pack_loaded(), "the plain pack loaded");
        assert!(h.pack_table().is_none(), "but carriers stay off");
        assert_ne!(block_change_state(&h), carrier_wire as i32);
    }

    #[test]
    fn a_codec_that_sends_no_hotbar_seeds_nothing() {
        // Review 1 minor: 1.17–1.20.1 never send the hotbar, so seeding the
        // server inventory would make `held_item` fictional and let the
        // placement path prefer a block the client was never given. Their
        // `initial_hotbar` is empty, so nothing is seeded and placement keeps
        // its own fallback.
        let codec = codec_for(763).unwrap();
        let h = handle(codec);
        h.seed_hotbar(&codec.initial_hotbar());
        assert!(h.held_item().is_none(), "nothing was handed out");

        // 1.8.9 does send it, so it is seeded.
        let v47 = codec_for(47).unwrap();
        let h = handle(v47);
        h.seed_hotbar(&v47.initial_hotbar());
        assert!(h.held_item().is_some());
    }

    #[test]
    fn a_release_with_no_generated_pack_is_never_remapped() {
        // 1.8.9 has no carrier table, so even a loaded compat pack changes
        // nothing.
        let h = handle(codec_for(47).unwrap());
        h.set_compat_pack_offered();
        h.set_pack_status(crate::protocol::PackStatus::Loaded);
        assert!(h.pack_loaded() && h.compat_pack_offered());
        assert!(h.pack_table().is_none());
    }
}
