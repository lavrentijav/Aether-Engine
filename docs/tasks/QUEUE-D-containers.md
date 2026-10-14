# Queue D — containers and XP (crate aether-server)
D1 double chests (two adjacent chests merge into one 54-slot window; breaking one half splits;
`type=left/right` block state). D2 XP orbs: ores, mobs and smelting drop orbs (entity) instead of
crediting the killer directly; pickup and merge as vanilla. Journal/provenance must still record
item moves for double chests (see `game/provenance.rs`). Tests per item.
