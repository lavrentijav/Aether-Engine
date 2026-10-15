# AX1 notes — anti-xray

Branch `oc/AX1`, from `main`. One commit per sub-step (messages start with `AX1`).

## What shipped

`[anti_xray]` config section (was `[visibility]`; the old section is gone — it was the same module)
with two modes:

- **`engine`** (default, cheap): a hidden-list block (default: every ore, ancient debris, monster
  spawner) with all six neighbours opaque is sent as the surrounding stone/deepslate/netherrack.
  Reveal runs from the game tick: any block write queued by `invalidate_block` is drained and the
  now-exposed hidden neighbours are re-sent as themselves. No per-section state.
- **`culling`** (the design doc): unchanged section face graph + reachable-set BFS, stone section
  masking, nearest-first reveal budget.

Both go through the existing per-version chunk writers as a `BlockSource` wrapper
(`EngineColumn` / `MaskedColumn`). Config also has `hidden_blocks`, `worlds` (per-world toggle),
`bypass_players`, and the `aether.antixray.bypass` permission node (operators until CMD-01), plus
`/visibility off`.

## Decisions

- **`[anti_xray]` replaces `[visibility]`** rather than adding a second section: the brief names
  `[anti_xray]`, and two sections for one module would be ambiguous. Docs and the example config
  were updated.
- **Engine mode is not a plugin.** Anti-xray is performance-sensitive and lives in the send layer,
  where the plugin API has no hook; it follows the existing `crate::visibility` precedent (core
  mechanism, policy in config). Recorded as a deviation from the "optional → plugin" rule of
  COMMON with that reasoning.
- **Hidden set is a dense bitset**, not a `HashSet`: engine mode asks it once per cell of every
  column. The first version used `HashSet` and the bench showed ~19 % overhead; the bitset brings it
  to ~5 %.
- **`spawner` is hidden by default; worldgen chests are not** (the generator keeps no block entity,
  so hiding a chest hides natural loot without hiding the texture a player already knows).
- **Re-hidden is intentionally not done** for either mode (the player has already seen the block /
  section; there is no leak). Matches the design doc's stance.

## Benchmark (`cargo-locked bench -p aether-server --bench anti_xray`)

Reading a full 16³ deep section (1 ore per 256 cells, 11 ore types), the wrapper vs plain:

| bench | time |
|---|---|
| `section_copy_plain` | ~259 µs |
| `section_copy_engine_masked` | ~272 µs |

**Overhead ~5 %**, under the <15 % target. The full chunk send is lower still: encode (palette,
light) dominates the block read measured here.

## Known environment note

`cargo-locked test -p aether-server --doc` fails on this branch with
`extern location for aether_plugin does not exist` — a missing optional-dependency rlib in the
shared target dir, unrelated to this change. The unit/integration suite (`--lib`, 1052 tests) is
green.
