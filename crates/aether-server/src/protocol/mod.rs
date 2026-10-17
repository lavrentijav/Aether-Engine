//! Version-neutral protocol layer.
//!
//! The engine's own state is the anchor: the server core reasons in engine
//! [`BlockStateId`]s, entity ids and positions, and never in a particular
//! vanilla version's wire encoding. Each supported protocol is a **codec**
//! that renders those neutral facts into its own packets and parses incoming
//! packets back into neutral events.
//!
//! That is what makes multi-version work without per-packet rewriting: a
//! player's handle carries the codec they connected with, so one broadcast of
//! [`ServerEvent::SpawnPlayer`] is encoded separately for a 1.8 viewer and a
//! 1.21 viewer, from the same source of truth.

use aether_world::{BlockStateId, ItemTypeId};
use std::borrow::Cow;
use std::io;

use crate::players::{PlayerHandle, PosLook};
use crate::proto::{Conn, PacketOut, RawPacket};

pub mod commands;
pub mod compat;
pub mod modern;
pub mod nbt;
pub mod scoreboard;
pub mod v47;
pub mod v755;
pub mod v756;
pub mod v757;
pub mod v758;
pub mod v759;
pub mod v760;
pub mod v761;
pub mod v762;
pub mod v763;
pub mod v764;
pub mod v765;
pub mod v766;
pub mod v767;
pub mod v768;
pub mod v769;
pub mod v770;
pub mod v771;
pub mod v772;
pub mod v773;

/// A read-only view of the world, so a codec can render chunk columns without
/// depending on the concrete `World` type (and so tests can feed a fake one).
pub trait BlockSource: Sync {
    /// Engine block id at absolute world coordinates, air when out of range.
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId;

    /// Copy section `(cx, cy, cz)` into `out`, indexed `x | z << 4 | y << 8`.
    /// The default asks block by block; a world that can do better should.
    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.block_at(
                cx * 16 + (i & 15) as i32,
                cy * 16 + (i >> 8) as i32,
                cz * 16 + ((i >> 4) & 15) as i32,
            );
        }
    }

    /// The vertical section range a column reader should ask for, inclusive.
    /// The default is the whole `i8` addressable band.
    fn section_y_range(&self) -> (i32, i32) {
        (i8::MIN as i32, i8::MAX as i32)
    }

    /// Copy every section of column `(cx, cz)` from `cy_min` to `cy_max`
    /// (inclusive) as **one consistent snapshot**.
    ///
    /// `None` (the default) means "no single-lock snapshot available": a
    /// reader copies section by section, which is correct for a static source
    /// but can tear under concurrent writes. A world that can take one read
    /// lock for the whole column should return `Some`.
    fn copy_column(
        &self,
        _cx: i32,
        _cz: i32,
        _cy_min: i32,
        _cy_max: i32,
    ) -> Option<Vec<Box<[BlockStateId; 4096]>>> {
        None
    }

    /// The biomes of column `(cx, cz)`, when the generator produced any.
    fn column_biomes(
        &self,
        _cx: i32,
        _cz: i32,
    ) -> Option<std::sync::Arc<aether_worldgen::ColumnBiomes>> {
        None
    }
}

/// A view of one column that reads each section from the world once.
///
/// Chunk encoders ask for every block of a column, and their light pass asks
/// again; through the world that is a quarter of a million lookups, each
/// taking the world's locks — slower than generating the column was. This
/// copies a section the first time any block in it is asked for and answers
/// everything after from the copy. Anything outside the column goes to the
/// world as before.
pub struct ColumnView<'a> {
    inner: &'a dyn BlockSource,
    cx: i32,
    cz: i32,
    sections: std::sync::Mutex<std::collections::HashMap<i32, Box<[BlockStateId; 4096]>>>,
    /// A whole-column copy taken under one read lock on first use, when the
    /// source offers one. It is what makes an encode see a single state even
    /// while a bulk commit is applying its batches.
    snapshot: std::sync::OnceLock<Option<(i32, Vec<Box<[BlockStateId; 4096]>>)>>,
}

impl<'a> ColumnView<'a> {
    pub fn new(inner: &'a dyn BlockSource, cx: i32, cz: i32) -> Self {
        Self {
            inner,
            cx,
            cz,
            sections: std::sync::Mutex::new(std::collections::HashMap::new()),
            snapshot: std::sync::OnceLock::new(),
        }
    }

    /// The whole-column snapshot, taken once. `None` when the source cannot
    /// promise one; the caller then falls back to per-section copies.
    fn snapshot(&self) -> Option<&(i32, Vec<Box<[BlockStateId; 4096]>>)> {
        self.snapshot
            .get_or_init(|| {
                let (min, max) = self.inner.section_y_range();
                self.inner
                    .copy_column(self.cx, self.cz, min, max)
                    .map(|v| (min, v))
            })
            .as_ref()
    }
}

impl BlockSource for ColumnView<'_> {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if x >> 4 != self.cx || z >> 4 != self.cz {
            return self.inner.block_at(x, y, z);
        }
        let cy = y >> 4;
        let index = ((y & 15) << 8 | (z & 15) << 4 | (x & 15)) as usize;
        if let Some((min, sections)) = self.snapshot() {
            return sections
                .get((cy - *min) as usize)
                .map(|sec| sec[index])
                .unwrap_or(BlockStateId::AIR);
        }
        let mut sections = self.sections.lock().unwrap();
        let sec = sections.entry(cy).or_insert_with(|| {
            let mut out = Box::new([BlockStateId::AIR; 4096]);
            self.inner.copy_section(self.cx, cy, self.cz, &mut out);
            out
        });
        sec[index]
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        if cx == self.cx && cz == self.cz {
            if let Some((min, sections)) = self.snapshot() {
                match sections.get((cy - *min) as usize) {
                    Some(sec) => {
                        out.copy_from_slice(&sec[..]);
                        return;
                    }
                    None => {
                        out.fill(BlockStateId::AIR);
                        return;
                    }
                }
            }
        }
        self.inner.copy_section(cx, cy, cz, out)
    }

    fn section_y_range(&self) -> (i32, i32) {
        self.inner.section_y_range()
    }

    fn copy_column(
        &self,
        cx: i32,
        cz: i32,
        cy_min: i32,
        cy_max: i32,
    ) -> Option<Vec<Box<[BlockStateId; 4096]>>> {
        // A nested view delegates the one-lock snapshot to its source.
        self.inner.copy_column(cx, cz, cy_min, cy_max)
    }

    fn column_biomes(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<std::sync::Arc<aether_worldgen::ColumnBiomes>> {
        self.inner.column_biomes(cx, cz)
    }
}

/// Something the server wants a client told, stated without reference to any
/// wire format. A codec turns one of these into zero or more packets.
/// One slot of a server-driven container, in version-neutral terms.
///
/// Named by item rather than by id so a codec can map it into its own
/// numbering — the same anchoring the block ids use.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSlot {
    /// `minecraft:`-qualified item name, or empty for an empty slot.
    pub item: String,
    /// How many to show.
    ///
    /// Larger than a stack is allowed and intended. Since 1.20.5 the count is
    /// a VarInt on the wire, so sixty-four is a rule about what a player may
    /// *carry*, not a limit on what a slot can *say* — and a manifest of
    /// recovered blocks needs to say the real number.
    pub count: u64,
    /// The hover text.
    pub label: String,
}

impl ServerEvent<'_> {
    /// The nearest event a codec without the gameplay events understands,
    /// for the few that have an older equivalent.
    pub fn legacy(&self) -> Option<ServerEvent<'static>> {
        match self {
            ServerEvent::EntityPos {
                entity_id, x, y, z, ..
            } => Some(ServerEvent::MoveEntity {
                entity_id: *entity_id,
                x: *x,
                y: *y,
                z: *z,
            }),
            // Codecs before 1.19 have no action-bar overlay, so the status line
            // is shown as an ordinary chat line rather than vanishing.
            ServerEvent::ActionBar(text) => Some(ServerEvent::Chat(text.clone())),
            _ => None,
        }
    }
}

impl ContainerSlot {
    /// The grey glass that frames the window.
    pub fn filler() -> Self {
        Self {
            item: "minecraft:gray_stained_glass_pane".into(),
            count: 1,
            label: " ".into(),
        }
    }
    /// A page arrow: `next` forward, otherwise back.
    pub fn arrow(next: bool) -> Self {
        Self {
            item: "minecraft:arrow".into(),
            count: 1,
            label: if next {
                "Next page".into()
            } else {
                "Previous page".into()
            },
        }
    }
    /// A control: a named item standing in for an action.
    pub fn button(item: &str, count: u64, label: &str) -> Self {
        Self {
            item: item.to_owned(),
            count: count.max(1),
            label: label.to_owned(),
        }
    }
    /// One recovered block.
    pub fn block(item: &str, count: u64, label: &str) -> Self {
        Self {
            item: item.to_owned(),
            count,
            label: label.to_owned(),
        }
    }
    /// Whether this slot holds nothing.
    pub fn is_empty(&self) -> bool {
        self.item.is_empty() || self.count == 0
    }

    /// The number to put in the stack-count badge.
    ///
    /// The real count up to a stack, and `1` beyond it. Past 64 the badge is
    /// no longer information — the client draws it into the corner of a
    /// 16-pixel icon, where four digits are a smear — so the quantity moves
    /// into the slot's name, where it can be read. See
    /// [`ContainerSlot::with_quantity`].
    pub fn wire_count(&self) -> u64 {
        if self.count > 64 {
            1
        } else {
            self.count
        }
    }

    /// Append `(xN)` to the label when the count has moved out of the badge.
    ///
    /// Applied by the caller that knows the label's wording, so that a slot
    /// never carries a quantity the player cannot see: either the badge shows
    /// it or the name does, and this is what guarantees one of the two.
    pub fn with_quantity(mut self) -> Self {
        if self.count > 64 {
            self.label = format!("{} (x{})", self.label, self.count);
        }
        self
    }
}

pub enum ServerEvent<'a> {
    /// Liveness ping; clients drop the connection without one every ~30s.
    KeepAlive(i64),
    /// Full block data for one 16×16 column.
    ChunkColumn { cx: i32, cz: i32 },
    /// Tell the client to forget a column it no longer has in range.
    UnloadColumn { cx: i32, cz: i32 },
    /// Add to the player list. Must precede [`ServerEvent::SpawnPlayer`] —
    /// a 1.8 client silently drops a spawn for a UUID it has no entry for.
    TabListAdd(&'a PlayerHandle),
    /// Drop from the player list.
    TabListRemove(u128),
    /// Make a player's entity appear.
    SpawnPlayer(&'a PlayerHandle),
    /// Absolute position + head rotation update for a player's entity.
    EntityMove(&'a PlayerHandle),
    /// Remove an entity.
    DespawnEntity(i32),
    /// A single block in the shared world changed.
    BlockChange {
        x: i32,
        y: i32,
        z: i32,
        block: BlockStateId,
    },
    /// A chat line, already formatted as plain text.
    Chat(String),
    /// A status line above the hotbar, already formatted as plain text.
    ///
    /// From 1.19 this is System Chat with the action-bar overlay flag set; on
    /// older codecs [`ServerEvent::legacy`] rewrites it to a chat line, so the
    /// vanished staff indicator shows in every protocol.
    ActionBar(String),
    /// Release the client's block prediction for `seq`.
    ///
    /// From 1.19 on the client applies a placement or break locally before
    /// the server answers, and stamps the attempt with a sequence number. It
    /// then *withholds* every block update the server sends for a predicted
    /// position, keeping them as a pending "true" state, until this packet
    /// arrives with a sequence at least as high. Only then does it apply them
    /// — and if the applied state collides with the player, it snaps the
    /// player back to where they stood when the prediction was made.
    ///
    /// A server that never sends this therefore leaves predictions open
    /// forever: the client's world drifts from the server's with nothing able
    /// to correct it. Versions before 1.19 predict nothing and ignore the
    /// event.
    AckBlockChange(i32),
    /// Move an entity that is not a player to an absolute position.
    ///
    /// Distinct from [`ServerEvent::EntityMove`], which takes a player handle
    /// and reads its look angles: a dropped item has no look, and giving it
    /// one would make it spin as it fell.
    MoveEntity {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
    },
    /// A stack lying on the ground.
    ///
    /// What happens to items a player has been given but cannot hold. The
    /// alternative — deleting them — turns a full inventory into a silent
    /// loss, and the alternative to *that* — refusing the transfer — leaves
    /// money already spent with nothing to show for it.
    DropItem {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        /// The item type, resolved per codec to that version's own id.
        item: ItemTypeId,
        /// May exceed a stack: see [`ContainerSlot::count`].
        count: u64,
    },
    /// Open a container window on the client and fill it.
    ///
    /// The server drives a container that corresponds to nothing in the world
    /// — the recovery stash — so the whole window is described here rather than
    /// derived from a block entity. A codec whose version this server has no
    /// container support for encodes it as no packets, and the caller falls
    /// back to a chat listing; see [`ProtocolCodec::supports_containers`].
    OpenContainer {
        title: String,
        slots: Vec<ContainerSlot>,
    },
    /// Declare the server's command grammar to the client.
    ///
    /// Sent once, at join. Without it the client's command tree is empty:
    /// every slash command shows red, nothing completes, and the server's
    /// commands are undiscoverable. The packet arrived in 1.13; 1.8 predates
    /// it and has no server-supplied tree, so that codec encodes the event as
    /// no packets.
    CommandTree,
    /// Move the centre of the client's loaded-column window.
    ///
    /// Every version from 1.14 on keeps its chunk window around a centre the
    /// server nominates, and discards columns that fall outside it. Without
    /// this the centre stays wherever the join left it, so columns streamed in
    /// as the player walks away are dropped on arrival and the world looks
    /// finite. 1.8 has no such packet and ignores the event.
    SetCenterChunk { cx: i32, cz: i32 },

    // --- Gameplay. Encoded by the 1.21.11 codec; every other codec renders
    // these as no packets, so a version that has not implemented them keeps
    // working with the subset it always had.
    /// Make any non-player entity appear: a mob, a dropped item, an arrow.
    SpawnEntity {
        entity_id: i32,
        uuid: u128,
        /// `minecraft:`-qualified entity type.
        kind: &'a str,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        /// Blocks per tick.
        velocity: (f64, f64, f64),
        /// Type-specific spawn data (an arrow's shooter + 1, ...).
        data: i32,
    },
    /// Entity metadata entries.
    EntityMeta {
        entity_id: i32,
        entries: Vec<(u8, MetaValue)>,
    },
    /// Absolute position and look of any entity.
    EntityPos {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    /// Which way an entity's head points.
    EntityHead { entity_id: i32, yaw: f32 },
    /// Set an entity's velocity — for the player it describes, a knockback.
    EntityVelocity {
        entity_id: i32,
        velocity: (f64, f64, f64),
    },
    /// Arm swing (0 main hand, 3 offhand), critical hit (4), ...
    EntityAnimation { entity_id: i32, animation: u8 },
    /// Entity status byte: 3 death, 9 finished eating, ...
    EntityStatus { entity_id: i32, status: i8 },
    /// An entity was hurt: red flash, hurt sound, camera tilt for a player.
    Damage {
        entity_id: i32,
        /// Damage type, `minecraft:`-qualified (`minecraft:mob_attack`).
        source: &'static str,
        attacker: Option<i32>,
    },
    /// An item entity flew into `collector`.
    Collect {
        item: i32,
        collector: i32,
        count: u8,
    },
    /// What an entity is holding and wearing: `(slot, stack)` where slot is
    /// 0 main hand, 1 offhand, 2 boots, 3 leggings, 4 chestplate, 5 helmet.
    Equipment {
        entity_id: i32,
        slots: Vec<(u8, Option<crate::inventory::Stack>)>,
    },
    /// A status effect was applied or changed on an entity: the client shows
    /// its icon and, if visible, its particles.
    UpdateMobEffect {
        entity_id: i32,
        /// `minecraft:mob_effect` registry id.
        effect: u8,
        amplifier: u8,
        /// Ticks remaining, `-1` for infinite.
        duration: i32,
        ambient: bool,
        visible: bool,
        show_icon: bool,
    },
    /// A status effect ran out or was removed.
    RemoveMobEffect { entity_id: i32, effect: u8 },
    /// An entity's attributes changed — the movement-speed modifier a
    /// speed or slowness effect puts on.
    UpdateAttributes {
        entity_id: i32,
        /// `(attribute registry id, base value, [(modifier id, amount, operation)])`.
        properties: Vec<(i32, f64, Vec<(String, f64, i8)>)>,
    },
    /// The player's own health and hunger.
    Health {
        health: f32,
        food: i32,
        saturation: f32,
    },
    /// The player's own experience bar.
    Experience { bar: f32, level: i32, total: i32 },
    /// World age and time of day, in ticks.
    Time { age: i64, time_of_day: i64 },
    /// The weather the world is in. The codec sends the Game State Change
    /// packets that start or stop the rain and the thunder, and set the
    /// client's rain/thunder levels.
    Weather { rain: bool, thunder: bool },
    /// The world border, as one Initialize World Border packet. Sent at join
    /// and after a size change; `old_diameter`/`new_diameter`/`speed` describe
    /// a running resize, `speed` in ticks. A non-size edit uses one of the
    /// focused events below so it cannot cancel a running animation.
    WorldBorder {
        x: f64,
        z: f64,
        old_diameter: f64,
        new_diameter: f64,
        speed: i64,
        warning_blocks: i32,
        warning_time: i32,
    },
    /// Set just the world border centre, leaving its size animation alone.
    WorldBorderCenter { x: f64, z: f64 },
    /// Set just the world border diameter, immediately.
    WorldBorderSize { diameter: f64 },
    /// Set just the warning distance, in blocks.
    WorldBorderWarningDistance { blocks: i32 },
    /// Set just the warning time, in ticks.
    WorldBorderWarningTime { ticks: i32 },
    /// The world spawn point, for the compass and respawn screen.
    SpawnPosition { x: i32, y: i32, z: i32, yaw: f32 },
    /// Every slot of a window plus the stack on the cursor.
    WindowContents {
        window_id: u8,
        state_id: i32,
        slots: Vec<Option<crate::inventory::Stack>>,
        cursor: Option<crate::inventory::Stack>,
    },
    /// Open a game window (crafting table, chest, furnace).
    OpenWindow {
        window_id: u8,
        menu: Menu,
        title: String,
    },
    /// Close a window from the server's side.
    CloseWindow(u8),
    /// A window property — a furnace's flame and arrow.
    WindowProperty {
        window_id: u8,
        property: i16,
        value: i16,
    },
    /// A level event: 2001 is block-break particles and sound for `data`
    /// (a block state).
    WorldEvent {
        event: i32,
        x: i32,
        y: i32,
        z: i32,
        data: i32,
    },
    /// Cracks on a block somebody is mining, `0..=9`, anything else clears.
    BreakAnimation {
        entity_id: i32,
        x: i32,
        y: i32,
        z: i32,
        stage: i8,
    },
    /// Bring a dead player back.
    Respawn { game_mode: GameMode },
    /// Switch the player's game mode (and the abilities that come with it).
    GameModeChange(GameMode),
    /// Select a hotbar slot on the client.
    SetHeldSlot(u8),
    /// Move the player themselves.
    Teleport {
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },
    /// The death screen's message.
    DeathMessage { entity_id: i32, text: String },
    /// An explosion: sound, particles, and a push for the player it is sent
    /// to.
    Explosion {
        x: f64,
        y: f64,
        z: f64,
        radius: f32,
        knockback: Option<(f64, f64, f64)>,
    },
    /// A sound at a position, by its `minecraft:` sound event name.
    ///
    /// The name is a `Cow` so the game's own literals stay borrows on the hot
    /// path while a plugin-chosen name (a menu's `[sound]` action) can be
    /// carried as an owned string.
    Sound {
        name: Cow<'static, str>,
        category: u8,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: f32,
    },
    /// One scoreboard packet, already resolved into an explicit operation
    /// (see [`scoreboard::ScoreboardOp`]). The neutral source is a plugin's
    /// sidebar; the codec renders it in its own version's dialect.
    Scoreboard(scoreboard::ScoreboardOp),
    /// The tab list's header and footer, plain text with `§` codes. The core's
    /// per-player path renders it with the version's `playerlist_header`
    /// packet.
    TabHeaderFooter { header: String, footer: String },
    /// A plugin-channel (custom payload) message to the client. Only the
    /// releases whose packet table carries `custom_payload` render it; an
    /// older codec emits nothing.
    PluginMessage { channel: String, data: Vec<u8> },
}

impl ServerEvent<'_> {
    /// A sound at a player's position, with the usual menu volume and pitch.
    /// A name without a namespace gets the `minecraft:` prefix.
    pub fn sound_at(
        name: impl Into<Cow<'static, str>>,
        x: f64,
        y: f64,
        z: f64,
    ) -> ServerEvent<'static> {
        let name = name.into();
        let name = if name.contains(':') {
            name
        } else {
            Cow::Owned(format!("minecraft:{name}"))
        };
        ServerEvent::Sound {
            name,
            category: 0, // master
            x,
            y,
            z,
            volume: 1.0,
            pitch: 1.0,
        }
    }
}

/// A game window's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    /// 3×3 crafting.
    Crafting,
    /// A generic container of 9 slots (1 row).
    Chest1,
    /// A generic container of 18 slots (2 rows).
    Chest2,
    /// A single chest, 27 slots (3 rows).
    Chest,
    /// A generic container of 36 slots (4 rows).
    Chest4,
    /// A generic container of 45 slots (5 rows).
    Chest5,
    /// A double chest, 54 slots (6 rows).
    LargeChest,
    /// A furnace.
    Furnace,
    /// A hopper, 5 slots.
    Hopper,
    /// A dispenser or dropper, 9 slots.
    Dispenser,
    /// A brewing stand, 5 slots.
    BrewingStand,
}

/// Flag on an [`ServerEvent::EntityMeta`] index: the field belongs to a
/// subclass of `AgeableMob` and its index counts from the end of that
/// class's fields, which moved between versions. `META_AGEABLE | 0` is a
/// sheep's wool, say. Every other index is absolute.
pub const META_AGEABLE: u8 = 0x40;

/// One entity-metadata value, in neutral terms.
#[derive(Debug, Clone)]
pub enum MetaValue {
    Byte(i8),
    VarInt(i32),
    Float(f32),
    Item(Option<crate::inventory::Stack>),
    /// An entity pose: 0 standing, 5 crouching, 7 dying.
    Pose(i32),
}

/// How a client got on with a resource pack it was offered.
///
/// The wire value is a small VarInt in every release that reports one. It is
/// kept as an enum rather than the raw number so the server cannot silently
/// misread it — a VarInt of 256 cast to a `u8` is `0`, or "loaded".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackStatus {
    /// The client downloaded and applied the pack.
    Loaded,
    /// The client refused it.
    Declined,
    /// The download failed.
    Failed,
    /// The client accepted the offer and is downloading.
    Accepted,
    /// The download finished and is being applied.
    Downloaded,
    /// The URL the server gave was invalid.
    InvalidUrl,
    /// A value this server does not know; treated as "not loaded".
    Unknown,
}

impl PackStatus {
    /// Read the wire VarInt. Anything outside the known range is
    /// [`PackStatus::Unknown`], never an out-of-range `u8`.
    pub fn from_wire(value: i32) -> Self {
        match value {
            0 => PackStatus::Loaded,
            1 => PackStatus::Declined,
            2 => PackStatus::Failed,
            3 => PackStatus::Accepted,
            4 => PackStatus::Downloaded,
            5 => PackStatus::InvalidUrl,
            _ => PackStatus::Unknown,
        }
    }
}

/// What a client asked for, parsed out of its version's packets.
pub enum ClientEvent {
    /// Updated position and/or look.
    Move(PosLook),
    /// Break the block at these coordinates.
    ///
    /// `seq` is the client's prediction sequence number (see
    /// [`ServerEvent::AckBlockChange`]); `0` on versions that have none.
    Dig { x: i32, y: i32, z: i32, seq: i32 },
    /// Place `block` at these coordinates.
    Place {
        x: i32,
        y: i32,
        z: i32,
        block: BlockStateId,
        /// The client's prediction sequence number; `0` before 1.19.
        seq: i32,
        /// Which face of the clicked block was hit, `0..=5` in the order
        /// down, up, north, south, west, east.
        ///
        /// Carried because half of a block's state is derivable from it: a
        /// slab clicked on its underside is a *top* slab, a torch on a wall
        /// faces away from it. The codecs already decoded this and threw it
        /// away.
        face: u8,
        /// Where on that face, each `0.0..=1.0`.
        ///
        /// The other half: the vertical component decides a slab's or a
        /// stair's half when the face is a side rather than a top or bottom.
        cursor: (f32, f32, f32),
    },
    /// A chat line the player typed.
    Chat(String),
    /// The player selected hotbar slot `0..9`.
    ///
    /// Additive on purpose: a codec that never emits it is still correct, so
    /// this costs the other versions nothing until they implement it.
    HeldSlot(u8),
    /// In creative the client fills a slot itself and reports it here, which
    /// is how the server learns about items it never handed out.
    ///
    /// The item arrives as the engine's own type id: item ids differ between
    /// versions, and the inventory this feeds has to mean the same thing on all
    /// of them — the same anchoring the block events use. `item` is `None` when
    /// the slot was emptied, or names an item this build does not know.
    CreativeSlot {
        slot: i16,
        item: Option<ItemTypeId>,
        count: u8,
    },
    /// The player closed the open container.
    ContainerClose,
    /// Started mining a block (survival), or broke it outright (creative).
    StartDig { x: i32, y: i32, z: i32, seq: i32 },
    /// Gave up mining.
    CancelDig { seq: i32 },
    /// Throw the held item: one, or the whole stack.
    DropHeld { all: bool },
    /// Stopped using an item (eating, drawing a bow).
    ReleaseUse,
    /// Swap main hand and offhand.
    SwapHands,
    /// Right-click with an item, not at a block.
    UseItem { hand: u8, seq: i32 },
    /// Hit (`attack`) or use an entity.
    Interact { target: i32, attack: bool },
    /// Right-click a block while holding something that is not a placeable
    /// block (a tool, an item). `x`/`y`/`z` are the **clicked** cell, not the
    /// cell in front of it.
    ///
    /// A codec only emits this when it cannot produce a [`ClientEvent::Place`]
    /// — today protocol 47 with a non-block item. It exists so a native
    /// plugin's selection wand sees the click; the core itself does nothing
    /// with it (matching the old "ignored" behaviour for such a click).
    UseBlock { x: i32, y: i32, z: i32 },
    /// Arm swing.
    Swing { hand: u8 },
    /// Started or stopped sprinting.
    Sprint(bool),
    /// The movement keys; only sneaking is acted on.
    Input { sneak: bool },
    /// The respawn button on the death screen.
    Respawn,
    /// A click in a window, as the protocol describes it.
    WindowClick {
        window: u8,
        slot: i16,
        button: i8,
        mode: i32,
    },
    /// The game-mode switcher (F3+F4) asked for a mode.
    ChangeGameMode(GameMode),
    /// Middle-click on a block (creative pick).
    PickBlock { x: i32, y: i32, z: i32 },
    /// Where the client got to with the resource pack it was offered.
    ///
    /// Only [`PackStatus::Loaded`] means the compat pack's carrier blocks are
    /// safe to send; anything else — including never reporting at all — leaves
    /// the plain stand-in in place. See `compat::pack`.
    ResourcePackStatus(PackStatus),
    /// The client's reply to a keep-alive, echoing the id the server sent. The
    /// play loop matches it against the send time to measure round-trip
    /// latency (see `PlayerHandle::set_ping`).
    KeepAlive(i64),
    /// A plugin-channel (custom payload) message from the client. The voice
    /// plugin reads the Simple Voice Chat and Plasmo Voice handshakes here;
    /// the core never interprets the body.
    PluginMessage {
        /// The channel, `namespace:path` (e.g. `voicechat:request_secret`).
        channel: String,
        /// The opaque payload.
        data: Vec<u8>,
    },
    /// Anything this server does not act on (animations, ...).
    Ignored,
}

/// Everything a codec needs to walk a freshly authenticated player from the
/// login handshake up to the point where chunks can start flowing.
pub struct JoinParams {
    pub entity_id: i32,
    pub uuid: u128,
    pub name: String,
    pub spawn: (f64, f64, f64),
    pub yaw: f32,
    pub pitch: f32,
    pub max_players: u32,
    pub view_radius: i32,
    /// Pack to offer on join, if the server configures one.
    pub resource_pack: crate::config::ResourcePackConfig,
    /// The mode the player joins in.
    pub game_mode: GameMode,
    /// Whether the player authenticated with Mojang. Carried into the 26.2+
    /// play Login packet's `online_mode` field.
    pub online: bool,
    /// Skin and cape properties from the session server, if any. Empty for an
    /// offline login.
    pub properties: Vec<crate::auth::api::Property>,
}

/// Which game mode the server runs.
///
/// The wire values are the same in every version this server speaks, so this
/// is a plain enum rather than something each codec translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameMode {
    /// Blocks must be mined and are dropped as items; the client applies
    /// gravity and cannot fly.
    Survival,
    /// The client fills its own inventory and blocks break instantly.
    #[default]
    Creative,
    /// Like survival, but the client refuses to break or place blocks.
    Adventure,
    /// No collision and no interaction with the world; the admin flies freely
    /// and mobs ignore them. Entered by `/spectate`, which saves the previous
    /// mode and restores it on `/spectate off`.
    Spectator,
}

impl GameMode {
    /// The value the Login packet carries.
    pub fn wire(self) -> u8 {
        match self {
            GameMode::Survival => 0,
            GameMode::Creative => 1,
            GameMode::Adventure => 2,
            GameMode::Spectator => 3,
        }
    }

    /// Whether the server hands out what a broken block drops.
    ///
    /// The distinction that actually matters to the server: in creative the
    /// *client* decides what it holds and reports it, so the server's
    /// inventory is a mirror. In survival nothing arrives unless the server
    /// sends it, so the inventory becomes authoritative.
    pub fn server_grants_drops(self) -> bool {
        matches!(self, GameMode::Survival)
    }

    /// Whether the mode is flight-capable and non-colliding — spectator and
    /// creative, the two modes that may float through the world.
    pub fn flies(self) -> bool {
        matches!(self, GameMode::Creative | GameMode::Spectator)
    }

    /// The vanilla `gameMode.*` display name.
    pub fn display_name(self) -> &'static str {
        match self {
            GameMode::Survival => "Survival Mode",
            GameMode::Creative => "Creative Mode",
            GameMode::Adventure => "Adventure Mode",
            GameMode::Spectator => "Spectator Mode",
        }
    }
}

impl std::str::FromStr for GameMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "survival" | "0" => Ok(GameMode::Survival),
            "creative" | "1" => Ok(GameMode::Creative),
            "adventure" | "2" => Ok(GameMode::Adventure),
            "spectator" | "3" => Ok(GameMode::Spectator),
            other => Err(format!(
                "`{other}` is not a game mode — use survival, creative, adventure or spectator"
            )),
        }
    }
}

/// The starter hotbar for a codec whose player has a **client-side**
/// inventory — the pre-1.20.2 codecs (protocols ≤ 763).
///
/// Those codecs do not run the gameplay path: they never render arbitrary
/// stacks and never receive a creative slot report, so the server knows
/// nothing about what the player holds unless it put it there. This is what it
/// puts there, as engine item *names* (not ids, which differ per release), so
/// each codec resolves them through the compat item table to its own numbering
/// — see [`legacy_codec_item_hooks`]. Every entry is present, by the same name,
/// in every supported release, so a slot is never left empty.
pub const STARTER_HOTBAR: [(&str, u8); 7] = [
    ("minecraft:stone", 64),
    ("minecraft:cobblestone", 64),
    ("minecraft:dirt", 64),
    ("minecraft:sand", 64),
    ("minecraft:gravel", 64),
    ("minecraft:glass", 64),
    ("minecraft:sandstone", 64),
];

/// The compat item table for a pre-1.20.2 codec's protocol id, if this layer
/// knows it. `None` for a protocol the layer does not cover — including the
/// 1.20.2+ codecs, which take their table from their shared `Version`.
pub(crate) fn legacy_item_table(protocol: i32) -> Option<&'static compat::items::ItemTable> {
    let t = match protocol {
        47 => &compat::items::V1_8,
        755 => &compat::items::V1_17,
        756 => &compat::items::V1_17_1,
        757 => &compat::items::V1_18,
        758 => &compat::items::V1_18_2,
        759 => &compat::items::V1_19,
        760 => &compat::items::V1_19_2,
        761 => &compat::items::V1_19_3,
        762 => &compat::items::V1_19_4,
        763 => &compat::items::V1_20_1,
        _ => return None,
    };
    Some(t)
}

/// The item half of the compat layer, bound to a pre-1.20.2 codec.
///
/// The old codecs predate the gameplay path, so they render no arbitrary
/// stacks; three things on their join and placement path still need the item
/// layer, and this one macro installs all three so no codec repeats the
/// binding:
///
/// * [`ProtocolCodec::initial_hotbar`] — only for a codec that actually sends
///   the hotbar to the client (`$hotbar`); for the 1.17–1.20.1 codecs, which
///   do not, it stays empty. Seeding the *server* inventory for a client that
///   was never handed those items would make `held_item` fictional and change
///   which block the placement path thinks is held;
/// * [`ProtocolCodec::block_for_item`] says which block a held item places —
///   the placement packet names only the hand, so the server must already know
///   what is in it;
/// * [`ProtocolCodec::resolve_reported_item`] keeps the real engine item when a
///   client echoes a stand-in back, exactly as the wired codecs do per slot;
/// * [`ProtocolCodec::resource_pack_status_id`] names the play-state packet
///   that reports the pack status, so the session can turn the compat pack's
///   carriers on for a client that loaded it.
macro_rules! legacy_codec_item_hooks {
    ($proto:expr, $status:expr, $hotbar:expr) => {
        fn initial_hotbar(&self) -> Vec<(String, u8)> {
            if !$hotbar {
                return Vec::new();
            }
            super::STARTER_HOTBAR
                .iter()
                .map(|(name, count)| ((*name).to_owned(), *count))
                .collect()
        }

        fn block_for_item(&self, item: &str) -> Option<BlockStateId> {
            super::modern::block_for_item(item)
        }

        fn resolve_reported_item(&self, server: ItemTypeId, reported: ItemTypeId) -> ItemTypeId {
            super::legacy_item_table($proto)
                .map_or(reported, |t| t.resolve_reported(server, reported))
        }

        fn resource_pack_status_id(&self) -> Option<i32> {
            Some($status)
        }
    };
}
pub(crate) use legacy_codec_item_hooks;

/// One wire dialect: how neutral state becomes packets and back.
pub trait ProtocolCodec: Sync + Send {
    /// Human-readable version this codec speaks, for logs and the server list.
    fn version_name(&self) -> &'static str;

    /// The protocol id a client announces in the handshake. For a codec that
    /// covers a range of releases this is the newest one it speaks — the id
    /// reported to the server list.
    fn protocol_id(&self) -> i32;

    /// Whether this codec can serve a client announcing `protocol`.
    ///
    /// Point releases inside one generation share a wire format, so a codec
    /// generally answers for a whole span of ids rather than a single one.
    /// The default is the single-version case.
    fn supports(&self, protocol: i32) -> bool {
        protocol == self.protocol_id()
    }

    /// Read the client's login request and return the username it asked for.
    /// `None` means the client sent something else and the connection is over.
    fn read_login_start(&self, s: &mut Conn) -> io::Result<Option<String>>;

    /// Whether this version's clientbound Encryption Request ends with the
    /// `should_authenticate` boolean.
    ///
    /// It was added in **1.20.5** (protocol 766) and is present ever since,
    /// including 26.x — it was *not* in 1.19/1.19.2, which is the opposite of
    /// what an early version of this code assumed. Getting it wrong is not
    /// subtle: a 1.21.11 client reads one byte past the packet, rejects it and
    /// drops the connection before answering, which looks like a failed login
    /// with no response.
    ///
    /// The clientbound Encryption Request id is `0x01` in every version this
    /// server speaks, so only this field is version-gated, and the version id
    /// alone decides it.
    fn encryption_request_has_should_authenticate(&self) -> bool {
        self.protocol_id() >= 766
    }

    /// Whether this version's serverbound Encryption Response carries the
    /// `has_verify_token` boolean (with an optional salt and chat signature)
    /// between the shared secret and the verify token.
    ///
    /// Only 1.19 and 1.19.2 (protocols 759 and 760) do; from 1.19.3 the
    /// packet is just the two byte arrays again.
    fn encryption_response_has_verify_token_flag(&self) -> bool {
        false
    }

    /// Acknowledge the login and drive the client all the way into the play
    /// state — for modern versions that includes the whole configuration
    /// phase, which is why the codec is handed the socket rather than just
    /// returning packets.
    ///
    /// Everything the client needs *before* chunk data goes here.
    fn complete_login(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()>;

    /// Everything that must follow the initial chunk batch: the authoritative
    /// spawn position and any inventory. Split from [`Self::complete_login`]
    /// because clients place the player only once the ground under them has
    /// arrived — sending the position first drops them through the world.
    fn finish_join(&self, s: &mut Conn, p: &JoinParams) -> io::Result<()>;

    /// Render a neutral event into this version's packets.
    fn encode(&self, ev: &ServerEvent, world: &dyn BlockSource) -> Vec<PacketOut>;

    /// Parse one received packet into a neutral event. `prev` supplies the
    /// fields a partial movement packet does not carry.
    fn decode(&self, pkt: &RawPacket, prev: PosLook) -> ClientEvent;

    /// Whether this codec can drive a server-owned container window.
    ///
    /// Default `false`: a codec that has not implemented
    /// [`ServerEvent::OpenContainer`] must say so, or the stash would open a
    /// window the player can see and cannot use. Callers fall back to chat.
    fn supports_containers(&self) -> bool {
        false
    }

    /// Whether this codec renders the gameplay events — entities, combat,
    /// windows, health — so the server can run survival authoritatively for
    /// this client. Versions that do not keep the older, client-trusting
    /// subset.
    fn full_gameplay(&self) -> bool {
        false
    }

    /// This version's scoreboard packet ids and field shapes, or `None` on a
    /// wire without scoreboard packets. Selected from [`Self::protocol_id`] by
    /// the core's map; a codec never spells its own ids here, so adding a new
    /// release is one row in one table rather than an edit to every module.
    fn scoreboard_wire(&self) -> Option<scoreboard::Wire> {
        scoreboard::wire_for(self.protocol_id())
    }

    /// How this codec draws a sidebar, or [`scoreboard::SidebarStyle::Unsupported`]
    /// if it has no scoreboard packets. The sidebar plugin asks this before it
    /// sends anything, so a version without support is skipped rather than
    /// silently mis-rendered.
    fn sidebar_style(&self) -> scoreboard::SidebarStyle {
        match self.scoreboard_wire() {
            Some(w) if w.number_format => scoreboard::SidebarStyle::DisplayNames,
            Some(_) => scoreboard::SidebarStyle::Teams,
            None => scoreboard::SidebarStyle::Unsupported,
        }
    }

    /// The items this codec puts in the hotbar at join, slot `0..9`, as
    /// `(name, count)`.
    ///
    /// The server has to know what it handed out, or selecting a slot does
    /// nothing until the client happens to report that slot back. Named rather
    /// than numbered so the inventory this seeds means the same thing on every
    /// version.
    fn initial_hotbar(&self) -> Vec<(String, u8)> {
        Vec::new()
    }

    /// The block this item places, if it places one.
    ///
    /// The session knows *which* item the player holds but not what it means;
    /// only a codec knows its own version's item set. The default says "no
    /// idea", which leaves a version that has not implemented this exactly as
    /// it was.
    fn block_for_item(&self, _item: &str) -> Option<BlockStateId> {
        None
    }

    /// Map an item a client reported in a slot back to the real engine item.
    ///
    /// A client that was handed a *substitute* for an item its version lacks
    /// reports the substitute back. The server keeps its own stack rather than
    /// the stand-in, so a client cannot turn a spear into a real trident by
    /// moving the slot around — see `compat::items`. The default trusts the
    /// report, which is right for a version whose ids the engine itself uses.
    fn resolve_reported_item(&self, _server: ItemTypeId, reported: ItemTypeId) -> ItemTypeId {
        reported
    }

    /// The packet id of the client's resource-pack status reply **in the play
    /// state**, when this codec reads it there.
    ///
    /// The reply says whether the client loaded the pack a `[compat.pack]`
    /// offered; the server shows carrier block states only to a client whose
    /// status is `successfully_loaded` (see `compat::pack`). The default is
    /// `None`: the server never learns the status, so that client keeps the
    /// plain stand-in. This is the safe default — 1.20.2 and later answer in
    /// the configuration phase, which this server does not decode.
    fn resource_pack_status_id(&self) -> Option<i32> {
        None
    }
}

/// Every codec this build can speak, newest protocol first.
pub fn codecs() -> &'static [&'static dyn ProtocolCodec] {
    static V47: v47::Codec = v47::Codec;
    static V777: modern::Codec = modern::Codec(&modern::version::V26_3);
    static V776: modern::Codec = modern::Codec(&modern::version::V26_2);
    static V775: modern::Codec = modern::Codec(&modern::version::V26_1);
    static V774: modern::Codec = modern::Codec(&modern::version::V1_21_11);
    static V755: v755::Codec = v755::Codec;
    static V756: v756::Codec = v756::Codec;
    static V757: v757::Codec = v757::Codec;
    static V758: v758::Codec = v758::Codec;
    static V759: v759::Codec = v759::Codec;
    static V760: v760::Codec = v760::Codec;
    static V761: v761::Codec = v761::Codec;
    static V762: v762::Codec = v762::Codec;
    static V763: v763::Codec = v763::Codec;
    static V766: v766::Codec = v766::Codec;
    static V765: v765::Codec = v765::Codec;
    static V764: v764::Codec = v764::Codec;
    static V767: v767::Codec = v767::Codec;
    static V768: v768::Codec = v768::Codec;
    static V769: v769::Codec = v769::Codec;
    static V770: v770::Codec = v770::Codec;
    static V771: v771::Codec = v771::Codec;
    static V772: v772::Codec = v772::Codec;
    static V773: v773::Codec = v773::Codec;
    // Newest first.
    static ALL: [&dyn ProtocolCodec; 24] = [
        &V777, &V776, &V775, &V774, &V773, &V772, &V771, &V770, &V769, &V768, &V767, &V766, &V765,
        &V764, &V763, &V762, &V761, &V760, &V759, &V758, &V757, &V756, &V755, &V47,
    ];
    &ALL
}

/// The codec for a client-announced protocol id, if this build speaks it.
pub fn codec_for(protocol: i32) -> Option<&'static dyn ProtocolCodec> {
    codecs().iter().copied().find(|c| c.supports(protocol))
}

/// Minimal JSON string escaping.
///
/// Escapes the mandatory JSON characters and every C0 control byte (`< 0x20`),
/// which would otherwise produce invalid JSON and make the client drop the
/// message.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

/// Refuse a login with a message the client shows.
///
/// Login-phase Disconnect (`0x00`) carries a JSON chat string in every
/// version this server speaks; see [`kick_unsupported`].
pub fn kick_login(s: &mut Conn, text: &str) -> io::Result<()> {
    PacketOut::new(0x00)
        .string(&format!("{{\"text\":\"{}\"}}", json_escape(text)))
        .send(s)
}

/// Turn away a client whose protocol this build does not speak.
///
/// Sent as login-phase Disconnect (`0x00`), whose payload is a JSON chat
/// string in *every* version to date — including the ones that switched the
/// play-phase disconnect to NBT — so one encoding reaches them all.
pub fn kick_unsupported(s: &mut Conn, protocol: i32) -> io::Result<()> {
    let supported: Vec<&str> = codecs().iter().map(|c| c.version_name()).collect();
    let text = format!(
        "This server speaks {} (you connected with protocol {protocol}).",
        supported.join(" and ")
    );
    PacketOut::new(0x00)
        .string(&format!("{{\"text\":\"{}\"}}", json_escape(&text)))
        .send(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_handles_quotes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\tb\r\n"), "a\\tb\\r\\n");
        assert_eq!(json_escape("\u{01}\u{1f}"), "\\u0001\\u001f");
    }

    #[test]
    fn every_1_20_2_to_1_21_10_codec_runs_the_full_game() {
        // J9 routed 766-773 through the shared gameplay path; T1 extends it to
        // 764/765. Entities, combat, health, hunger, windows and effects, not
        // the old client-trusting subset.
        for proto in 764..=773 {
            let c = codec_for(proto).unwrap_or_else(|| panic!("no codec for {proto}"));
            assert!(c.full_gameplay(), "{proto} must run the full game");
            assert!(c.supports_containers(), "{proto} must support windows");
        }
    }

    #[test]
    fn the_pre_1_20_2_codecs_are_wired_into_the_item_layer() {
        // 1.8.9 and 1.17-1.20.1 keep the client-trusting subset, so they render
        // no arbitrary stacks; but their join hotbar, their block placement and
        // the stand-in round trip all go through the compat item tables now.
        for proto in [47, 755, 756, 757, 758, 759, 760, 761, 762, 763] {
            let c = codec_for(proto).unwrap_or_else(|| panic!("no codec for {proto}"));
            assert!(!c.full_gameplay(), "{proto} must stay on the old subset");
            let table =
                legacy_item_table(proto).unwrap_or_else(|| panic!("no compat table for {proto}"));

            // The starter hotbar is seeded only for a codec that sends it to
            // the client — 1.8.9 does, 1.17–1.20.1 do not (they rely on the
            // creative menu), so theirs stays empty and the placement path
            // keeps its stone fallback.
            let hotbar = c.initial_hotbar();
            if proto == 47 {
                assert_eq!(hotbar.len(), STARTER_HOTBAR.len(), "{proto}");
                for (name, count) in &hotbar {
                    assert!(*count > 0, "{proto}: {name} with no count");
                    assert!(
                        table.wire(crate::inventory::item(name)).is_some(),
                        "{proto}: {name} has no stand-in"
                    );
                }
            } else {
                assert!(hotbar.is_empty(), "{proto} must not seed a hotbar");
            }

            // The held block item decides what a placement places.
            assert_eq!(
                c.block_for_item("minecraft:stone"),
                Some(aether_api::block_ids::STONE),
                "{proto}"
            );
            assert_eq!(c.block_for_item("minecraft:diamond_sword"), None, "{proto}");

            // A version that shows a spear as a trident resolves the echo back
            // to the real spear, exactly as the wired codecs do per slot.
            let spear = crate::inventory::item("minecraft:iron_spear");
            let trident = crate::inventory::item("minecraft:trident");
            if let Some(w) = table.wire(spear) {
                if w.disguised && table.engine(w.id) == Some(trident) {
                    assert_eq!(c.resolve_reported_item(spear, trident), spear, "{proto}");
                }
            }
        }
    }

    #[test]
    fn an_old_codec_places_blocks_and_keeps_a_spear_through_a_trident() {
        let c = codec_for(767).unwrap();
        assert_eq!(
            c.block_for_item("minecraft:stone"),
            Some(aether_api::block_ids::STONE),
        );
        let spear = crate::inventory::item("minecraft:iron_spear");
        let trident = crate::inventory::item("minecraft:trident");
        assert_eq!(c.resolve_reported_item(spear, trident), spear);
    }

    #[test]
    fn a_pack_status_out_of_range_is_unknown_not_loaded() {
        // Review 1 (`RP1-f`): the status was read as a raw VarInt and cast with
        // `as u8`, so 256 wrapped to 0 — "loaded" — and turned carriers on for
        // a client that never loaded the pack. `from_wire` maps the known
        // values and everything else to `Unknown`.
        assert_eq!(PackStatus::from_wire(0), PackStatus::Loaded);
        assert_eq!(PackStatus::from_wire(1), PackStatus::Declined);
        assert_eq!(PackStatus::from_wire(2), PackStatus::Failed);
        assert_eq!(PackStatus::from_wire(3), PackStatus::Accepted);
        assert_eq!(PackStatus::from_wire(4), PackStatus::Downloaded);
        assert_eq!(PackStatus::from_wire(5), PackStatus::InvalidUrl);
        // The regression: 256 must NOT become Loaded.
        for out_of_range in [256, 257, 512, 65536, -1, i32::MAX, i32::MIN] {
            assert_eq!(
                PackStatus::from_wire(out_of_range),
                PackStatus::Unknown,
                "wire {out_of_range} must not be Loaded"
            );
        }
    }

    #[test]
    fn the_pre_1_20_2_codecs_report_their_play_state_pack_status_id() {
        // Every old codec decodes the pack status; 1.20.2+ answer in the
        // configuration phase and report `None`, so their clients keep the
        // plain stand-in rather than waiting on a status this server never
        // reads.
        for proto in [47, 755, 756, 757, 758, 759, 760, 761, 762, 763] {
            assert!(
                codec_for(proto)
                    .unwrap()
                    .resource_pack_status_id()
                    .is_some(),
                "{proto} must report a status id"
            );
        }
        for proto in [764, 767, 773, 774] {
            assert!(
                codec_for(proto)
                    .unwrap()
                    .resource_pack_status_id()
                    .is_none(),
                "{proto} answers in config, not play"
            );
        }
        // The ids are the release's own `resource_pack_receive`.
        assert_eq!(codec_for(47).unwrap().resource_pack_status_id(), Some(0x19));
        assert_eq!(
            codec_for(763).unwrap().resource_pack_status_id(),
            Some(0x24)
        );
    }

    #[test]
    fn dispatch_maps_ids_to_codecs() {
        assert_eq!(codec_for(47).unwrap().version_name(), "1.8.9");
        assert_eq!(codec_for(774).unwrap().version_name(), "1.21.11");
        assert_eq!(codec_for(775).unwrap().version_name(), "26.1.x");
        assert_eq!(codec_for(776).unwrap().version_name(), "26.2");
        assert_eq!(codec_for(777).unwrap().version_name(), "26.3");
        assert_eq!(codec_for(765).unwrap().version_name(), "1.20.3/1.20.4");
        assert_eq!(codec_for(764).unwrap().version_name(), "1.20.2");
        assert!(codec_for(1).is_none(), "unknown protocol must not resolve");
    }
}

/// Decode the VarInt that ends `body`.
///
/// Both packets that carry a prediction sequence — Player Action and Use Item
/// On — put it last, and have done since 1.19, while the fields in front of it
/// have changed several times (the cursor vector, `inside_block`, and in 1.21
/// `world_border_hit`). Reading backwards from the end therefore stays correct
/// across the whole range without any codec having to spell out a tail layout
/// it cannot test against a live client.
///
/// Returns `0` — "no sequence" — when the tail is not a well-formed VarInt.
pub fn trailing_var_int(body: &[u8]) -> i32 {
    // A VarInt's last byte has the continuation bit clear and every earlier
    // byte has it set, so the start is found by walking back over set bits.
    let end = body.len();
    if end == 0 || body[end - 1] & 0x80 != 0 {
        return 0;
    }
    let mut start = end - 1;
    while start > 0 && body[start - 1] & 0x80 != 0 && end - start < 5 {
        start -= 1;
    }
    let mut value: i32 = 0;
    for (i, b) in body[start..end].iter().enumerate() {
        value |= ((b & 0x7F) as i32) << (7 * i);
    }
    value
}

#[cfg(test)]
mod trailing_tests {
    use super::trailing_var_int;

    /// The decoder here is written from the VarInt description (7 payload bits
    /// per byte, little-endian, high bit means "another byte follows"), not by
    /// calling the encoder, so a bug in the encoder cannot hide in it.
    fn encode(mut v: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            if v < 0x80 {
                out.push(v as u8);
                return out;
            }
            out.push((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
    }

    #[test]
    fn reads_a_sequence_from_the_end_of_a_packet_body() {
        for seq in [0u32, 1, 127, 128, 300, 16_383, 16_384, 2_097_151] {
            // The tail of a real Player Action: ..., face byte, sequence.
            // The byte in front of the sequence always has its high bit
            // clear — see the test below for why that is what makes the
            // backwards walk unambiguous.
            let mut body = vec![0x01, 0x02, 0x03, 0xAA, 0x05];
            body.extend_from_slice(&encode(seq));
            assert_eq!(trailing_var_int(&body), seq as i32, "seq {seq}");
        }
    }

    #[test]
    fn the_field_in_front_of_the_sequence_cannot_be_mistaken_for_it() {
        // Walking backwards is only unambiguous because in both packets that
        // carry a sequence the field immediately before it is a small byte
        // with the high bit clear: `face` (0..=5) in Player Action, and the
        // `inside_block` / `world_border_hit` booleans in Use Item On. Those
        // are the two shapes to hold to.
        for preceding in [0u8, 1, 5] {
            assert_eq!(trailing_var_int(&[preceding, 0x2A]), 42);
            assert_eq!(trailing_var_int(&[preceding, 0xAC, 0x02]), 300);
        }
    }

    #[test]
    fn a_truncated_or_empty_tail_reads_as_no_sequence() {
        assert_eq!(trailing_var_int(&[]), 0);
        assert_eq!(trailing_var_int(&[0x80]), 0);
    }
}
