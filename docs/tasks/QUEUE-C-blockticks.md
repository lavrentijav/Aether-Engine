# Queue C — scheduled block behaviour (crate aether-server, `game/`)
Context: `docs/KNOWN_ISSUES.md` item 11; fluids already use scheduled updates in `game/fluids.rs` —
reuse that scheduling. C1 falling sand/gravel/concrete powder (falling block entity or instant drop
as vanilla does server-side). C2 crop growth (wheat, carrots, potatoes, beetroot, sugar cane,
cactus) via random ticks with vanilla rates and light/farmland rules (light is full-bright: note it).
C3 leaf decay (distance property already exists on leaves). Tests per item.
