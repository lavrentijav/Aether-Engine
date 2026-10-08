//! Shared player registry so independent per-connection threads can see and
//! broadcast to each other — the minimum needed for players to see other
//! players move, join and leave.
//!
//! Each connection thread owns its `TcpStream` for *reading*. On join it
//! hands a `try_clone()`d write half to the registry; every write to that
//! socket after that point — this player's own keep-alives included — goes
//! through the handle's mutex so a broadcast from another thread can never
//! interleave bytes with this connection's own output.
//!
//! A handle also carries the **codec** its client connected with. Broadcasts
//! therefore take a version-neutral [`ServerEvent`] rather than finished
//! bytes, and each recipient renders it in their own dialect — which is how
//! a 1.8 and a 1.21 client can share one world.

use crate::proto::Conn;
use std::collections::HashMap;
use std::io;
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

/// One connected player, as seen by every other connection thread.
pub struct PlayerHandle {
    pub entity_id: i32,
    pub uuid: u128,
    pub name: String,
    /// The wire dialect this particular client speaks.
    pub codec: &'static dyn ProtocolCodec,
    writer: Mutex<Conn>,
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
        item: Option<String>,
        count: u8,
    ) -> crate::inventory::SlotChange {
        let Ok(index) = usize::try_from(slot) else {
            return crate::inventory::SlotChange::default();
        };
        self.inventory
            .lock()
            .unwrap()
            .set(index, item.as_deref().map(|n| (n, count)))
    }

    /// Record the hotbar the server handed this player at join.
    pub fn seed_hotbar(&self, items: &[(String, u8)]) {
        let mut inv = self.inventory.lock().unwrap();
        for (i, (name, count)) in items.iter().enumerate() {
            inv.set(crate::inventory::FIRST_HOTBAR + i, Some((name, *count)));
        }
    }

    /// The name of the item in the selected hotbar slot.
    pub fn held_item(&self) -> Option<String> {
        self.inventory
            .lock()
            .unwrap()
            .held()
            .map(|s| s.item.clone())
    }

    /// Render `ev` with this player's codec and write it to their socket.
    ///
    /// Encoding happens outside the lock and the whole event is written under
    /// a single lock acquisition, so an event that becomes several packets
    /// cannot be split apart by another thread's broadcast. Write errors are
    /// swallowed: a dead socket is detected by the owning thread's own read
    /// loop, which then cleans up the registry — a broadcaster shouldn't tear
    /// down someone else's connection.
    pub fn emit(&self, ev: &ServerEvent, world: &dyn BlockSource) {
        let legacy = if self.full() { None } else { ev.legacy() };
        let packets = match ev {
            // Every codec reads a column block by block; give it a view that
            // copies each section once instead of locking per block.
            ServerEvent::ChunkColumn { cx, cz } => {
                let view = crate::protocol::ColumnView::new(world, *cx, *cz);
                self.codec.encode(ev, &view)
            }
            _ => self.codec.encode(legacy.as_ref().unwrap_or(ev), world),
        };
        if let Ok(mut w) = self.writer.lock() {
            for p in &packets {
                if p.send(&mut w).is_err() {
                    break;
                }
            }
        }
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
        codec: &'static dyn ProtocolCodec,
        stream: &Conn,
        pos: PosLook,
    ) -> io::Result<Arc<PlayerHandle>> {
        // Clones the compression state along with the socket: a broadcast must
        // be framed exactly as the owning thread's own writes are, and the
        // switch has already happened by the time a player reaches the
        // registry.
        let writer = stream.try_clone()?;
        let handle = Arc::new(PlayerHandle {
            entity_id,
            uuid,
            name,
            codec,
            writer: Mutex::new(writer),
            pos: Mutex::new(pos),
            inventory: Mutex::new(crate::inventory::Inventory::new()),
            session: Mutex::new(crate::commands::Session::default()),
            game: Mutex::new(crate::game::player::PlayerState::new(
                crate::server_game_mode(),
            )),
            tracked: Mutex::new(std::collections::HashSet::new()),
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
