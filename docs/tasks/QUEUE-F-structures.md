# Queue F — structure generation groundwork (crate aether-worldgen)
F1 Structure placement: implement vanilla `structure_set` placement (random_spread with spacing,
separation, salt; concentric_rings for strongholds) to decide which chunks start which structure,
seeded exactly as 1.21.11. Pure function + tests against known seed/positions if obtainable
(otherwise determinism tests). F2 Structure templates: load `.nbt` structure templates from the
data pack and place one simple template-based structure (igloo or desert pyramid) at the starts
from F1, including its loot chests empty for now. No jigsaw yet; write a short design note for
jigsaw (villages) in `docs/tasks/QF-notes.md`.
