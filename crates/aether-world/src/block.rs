//! Block identity and the per-block flags that feed the SoA masks.

/// A global block-state id.
///
/// In Vanilla terms this is the flattened "block state" — a specific block
/// with a specific set of properties (facing, powered, …). The engine assigns
/// its own dense ids; the [`crate::storage`] format persists them verbatim and
/// `aether-convert` maps Anvil namespaced states onto them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockStateId(pub u32);

impl BlockStateId {
    /// `minecraft:air` — the default fill of an empty sub-chunk.
    pub const AIR: BlockStateId = BlockStateId(0);

    /// The raw numeric id.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl Default for BlockStateId {
    fn default() -> Self {
        BlockStateId::AIR
    }
}

/// The handful of boolean properties the SoA bit masks track per block.
///
/// These drive the [`SubChunk`](crate::SubChunk) masks so that physics,
/// lighting and redstone can stream a single mask instead of chasing block
/// records. Air is "all false".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockProperties {
    /// Fully opaque cube for occlusion / SoA `SolidMask`.
    pub solid: bool,
    /// Participates in the collision broad-phase.
    pub collision: bool,
    /// Can carry or interact with redstone power.
    pub redstone: bool,
    /// Light this block gives off, `0..=15`.
    pub light_emission: u8,
    /// Light this block removes from what passes through it, `0..=15`.
    ///
    /// Separate from `solid` because they disagree for the blocks that matter
    /// most to lighting: glass is a full cube that blocks nothing, water is
    /// not a cube and blocks one level, leaves are a cube and block one.
    pub light_opacity: u8,
}

impl BlockProperties {
    /// The properties of air / any non-interactive empty space.
    pub const AIR: BlockProperties = BlockProperties {
        solid: false,
        collision: false,
        redstone: false,
        light_emission: 0,
        light_opacity: 0,
    };

    /// A plain full opaque, collidable, redstone-inert block (stone-like).
    pub const SOLID: BlockProperties = BlockProperties {
        solid: true,
        collision: true,
        redstone: false,
        light_emission: 0,
        light_opacity: 15,
    };
}
