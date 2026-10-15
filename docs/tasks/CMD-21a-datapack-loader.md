# CMD-21a — V-data: data pack loader

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-20a`, `oc/CMD-20b`, `oc/CMD-20c`
Parent/umbrella: `docs/tasks/CMD-21-function-datapack.md`

## Goal
The data pack loader, **its own task** (was part of CMD-21). Everything data-driven (loot, recipe,
advancement, function, predicate, tags, worldgen, enchantment) is read here; `/function`,
`/datapack` and `/reload` are the other two sub-tasks.

## Behaviour
- Discover packs in `world/datapacks/*` and the engine's built-in pack (the operator's
  `data/minecraft/`); honour `pack.mcmeta` `pack_format` compatibility for 1.21.11 and the `filter`
  section.
- Registries to load: `function`, `advancement`, `loot_table`, `predicate`, `item_modifier`,
  `tags/*`, `dimension`, `worldgen/*`, `enchantment` (E1), `recipe`.
- A broken pack is **reported and disabled**, not a crash (like a broken pack in vanilla). Pack
  order is deterministic and supports `first`/`last`/`before`/`after`.
- The loader produces an immutable in-memory registry set that other tasks read; reloading (CMD-21c)
  swaps it at a safe point.

## Files
- new `crates/aether-server/src/datapack.rs`; reads the operator's `data/` copy (never vendored).

## Edge cases to test explicitly
- `pack_format` too new → refused with a clear message (decide and record behaviour for too old).
- a pack with a syntax error in one file is disabled whole and names the file.
- duplicate resource across two packs resolves by pack order (later wins, per vanilla).
- the built-in pack cannot be disabled.
- a fixture data pack in the test tree loads and its entries are found.

## Done criteria
- Loader + tests + a fixture pack; fmt / clippy `-D warnings` / `cargo-locked test`.
