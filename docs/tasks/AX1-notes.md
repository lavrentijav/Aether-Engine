# AX1 notes — anti-xray

Branch `oc/AX1`, from `main`. Commits start with `AX1`.

## What shipped

`[anti_xray]` config section with two modes:

- **`engine`** (default, cheap): a hidden-list block (default: every ore, ancient debris, monster
  spawner) with all six neighbours opaque is sent as the block actually around it — the most common
  opaque non-hidden neighbour (deepslate, netherrack, andesite, …), not a y-based guess. Reveal runs
  from the game tick: any block write queued by `invalidate_block` is drained and the now-exposed
  hidden neighbours are re-sent as themselves, deduplicated per player. Single server-side writes
  (`/setblock`, creative, `/rollback`) are masked through the same path in `players.rs::render`.
- **`culling`** (the design doc): the section face graph + reachable-set BFS, stone section masking,
  nearest-first reveal budget; single block changes inside a hidden section are masked to the fill.

Both go through the existing per-version chunk writers as a `BlockSource` wrapper
(`EngineColumn` / `MaskedColumn`). Config: `hidden_blocks`, `worlds` (per-world toggle),
`bypass_players`; bypass also via the `aether.antixray.bypass` node (granted explicitly, not
implied by being an operator — see the SP1 follow-up) and `/visibility off`.

## Backward compatibility (review (c))

`[visibility]` is gone as the canonical name but **not broken**: `Config::migrate_legacy` reads the
old table and migrates it to `[anti_xray] mode = "culling"` (its old meaning — silently defaulting
to `engine`, or off, would have been a security regression), with a startup warning. The old
`ray_refinement`/`mask_buried_ores` keys are accepted and reported as ignored. If both tables are
present, `[anti_xray]` wins and the old one is ignored with a warning. `load_or_init` calls it; the
integration path logs through `crate::log::warn`. Tests: `a_pre_ax1_visibility_config_is_migrated...`,
`anti_xray_wins_when_both_sections_are_present`, `the_deprecated_reserved_keys_are_accepted...`.

## Review response (round 1: CHANGES)

- **(c) blocker — old configs:** done, above.
- **(a) single `BlockChange` leaks:** `visibility::mask_block` now runs for every `BlockChange` in
  `render` (engine: substitute an enclosed hidden block; culling: fill inside a hidden section).
- **(a) substitute by environment:** `surrounding_block` picks the most common opaque non-hidden
  neighbour, so an andesite/tuff/gravel pocket or the deepslate transition zone is masked with the
  right block, not a lone stone. The `y < 0` and `name.contains("nether")` heuristics are gone.
- **(a) cave ores:** engine mode genuinely does not hide an ore that touches air. Documented loudly in
  KNOWN_ISSUES A.16 (EN+RU) and in the design doc; `culling` is the mode that hides cave walls.
- **(b) dedup:** `State::revealed` remembers the cells sent real per player; dropped on column unload
  (`forget`) and reset. Tests `a_reveal_is_sent_once_and_reset_on_unload`.
- **(b) reach clipping:** `engine_tick` no longer clips by Y (the client holds the whole column
  height), only by horizontal view distance.
- **(b) comment:** "six faces plus the changed cell" corrected to "the six faces".
- **(d)/(e):** bypass and per-world were already correct; the per-world string is now resolved once
  (`anti_xray_enabled` caches the answer in an `AtomicU8` after config install) instead of parsing
  the world dir on every block write. `applies_to_world` has a test.
- **(f) benchmark:** fixed the per-call `ore_table()` allocation; the bench now measures a whole
  1.21.11 column through the real codec as well as an isolated section copy; the command is in the
  bench header and below. See "Benchmark".
- **(g) tests:** added an integration test `crates/aether-server/tests/anti_xray_e2e.rs` that drives
  the real codec and asserts a buried-ore column encodes **byte-identically** to the same column with
  the ore replaced by the surrounding rock, and that an exposed ore is unchanged.
- Doc contradiction (`visibility-culling.md` light) fixed.
- `EngineColumn` is now generic over its source (not `&dyn BlockSource`), so the inner read is a
  direct call — the codec reads every cell twice (blocks + light) and a second virtual dispatch per
  cell was the bulk of the overhead.

## Benchmark

```text
export CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0
cargo-locked bench -p aether-server --bench anti_xray
```

Deep column, one ore per 256 cells (≈ real overworld density). On a quiet machine:

| bench | time |
|---|---|
| `column_encode_plain` | ≈ 1.98 ms |
| `column_encode_engine_masked` | ≈ 2.19 ms |
| `section_copy_plain` | ≈ 5.1 µs |
| `section_copy_engine_masked` | ≈ 9.5 µs |

End-to-end **≈ 10 %**, under the `< 15 %` target. The shared build/runtime host makes the end-to-end
ratio load-dependent (observed 0–20 %); the isolated section copy is stable and shows the wrapper
adding about one L1-resident membership test per cell. Run both pairs back to back.

## Decisions

- **`[anti_xray]` replaces `[visibility]`** as canonical, with a migration + warning (see above)
  rather than two live sections.
- **Engine mode is core code, not a plugin** (deviation from COMMON's "optional → plugin"): it is
  performance-sensitive and lives in the network send layer, where the plugin API has no hook; it
  follows the existing `crate::visibility` precedent (core mechanism, policy in config).
- **`spawner` is hidden by default; worldgen chests are not** (the generator keeps no block entity,
  so hiding a chest hides natural loot without hiding the texture a player already knows).

## Known environment note

`cargo-locked test -p aether-server --doc` fails on this branch with
`extern location for aether_plugin does not exist` — a missing optional-dependency rlib in the
shared target dir, unrelated to this change. The unit/integration suite is green.
