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

use aether_world::BlockStateId;
use std::io;

use crate::players::{PlayerHandle, PosLook};
use crate::proto::{Conn, PacketOut, RawPacket};

pub mod commands;
pub mod nbt;
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
pub mod v774;

/// A read-only view of the world, so a codec can render chunk columns without
/// depending on the concrete `World` type (and so tests can feed a fake one).
pub trait BlockSource: Sync {
    /// Engine block id at absolute world coordinates, air when out of range.
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId;
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
        /// `minecraft:`-qualified item name.
        item: String,
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
    /// commands are undiscoverable. 1.8 has no such packet and ignores the
    /// event.
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
    Sound {
        name: &'static str,
        category: u8,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: f32,
    },
}

/// A game window's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    /// 3×3 crafting.
    Crafting,
    /// A single chest, 27 slots.
    Chest,
    /// A furnace.
    Furnace,
}

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
    /// The item is named, not numbered: item ids differ between versions, and
    /// the inventory this feeds has to mean the same thing on all of them —
    /// the same anchoring the block events use. `item` is `None` when the slot
    /// was emptied.
    CreativeSlot {
        slot: i16,
        item: Option<String>,
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
    /// Middle-click on a block (creative pick).
    PickBlock { x: i32, y: i32, z: i32 },
    /// Anything this server does not act on (keep-alive replies, animations,
    /// ...).
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
}

impl GameMode {
    /// The value the Login packet carries.
    pub fn wire(self) -> u8 {
        match self {
            GameMode::Survival => 0,
            GameMode::Creative => 1,
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
}

impl std::str::FromStr for GameMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "survival" | "0" => Ok(GameMode::Survival),
            "creative" | "1" => Ok(GameMode::Creative),
            other => Err(format!(
                "`{other}` is not a game mode — use survival or creative"
            )),
        }
    }
}

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
}

/// Every codec this build can speak, newest protocol first.
pub fn codecs() -> &'static [&'static dyn ProtocolCodec] {
    static V47: v47::Codec = v47::Codec;
    static V774: v774::Codec = v774::Codec;
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
    static V767: v767::Codec = v767::Codec;
    static V768: v768::Codec = v768::Codec;
    static V769: v769::Codec = v769::Codec;
    static V770: v770::Codec = v770::Codec;
    static V771: v771::Codec = v771::Codec;
    static V772: v772::Codec = v772::Codec;
    static V773: v773::Codec = v773::Codec;
    // Newest first. 764 and 765 are absent on purpose: before 1.20.5 the whole
    // registry set travels as one NBT compound rather than a packet per
    // registry, which is a separate code path still to be written.
    static ALL: [&dyn ProtocolCodec; 19] = [
        &V774, &V773, &V772, &V771, &V770, &V769, &V768, &V767, &V766, &V763, &V762, &V761, &V760,
        &V759, &V758, &V757, &V756, &V755, &V47,
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
    fn dispatch_maps_ids_to_codecs() {
        assert_eq!(codec_for(47).unwrap().version_name(), "1.8.9");
        assert_eq!(codec_for(774).unwrap().version_name(), "1.21.11");
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
