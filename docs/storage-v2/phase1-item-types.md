# Phase 1 — item types as `ItemTypeId(u16)`

Part of [storage v2](README.md). Goal: stop carrying item names as `String` through hot structs,
inventories, events and the wire path; carry a 2-byte id instead, with one name table per world.

## Today
- `inventory::Stack.item: String`, `journal::EventBody::{ItemMint,ItemDerive}.item: String`,
  `ledger` entries, `game/provenance.rs` `Entry.item`, economy listings — each a heap allocation and
  20–40 bytes per stack/event; comparisons are string compares.
- Game data (`game/data.rs`, `game/tables.rs`, recipes, drops) uses `&'static str` literals (~4.9k).
- Codecs map names to protocol ids per version (`protocol/modern/items.rs` `ITEMS`/`ITEMS_BY_ID`).
- Blocks already have a persisted name table (`storage/mod.rs` `REGISTRY_KEY = b"REG"`), but a
  truncated/unknown-version table silently falls back to the default order — a corruption would turn
  blocks into other blocks without an error.

## Design
1. **`aether_world::registry::items`** (new):
   - `ItemTypeId(pub u16)`; `0` = air/empty, never a real item.
   - `ItemRegistry`: `name(id) -> &'static str`, `id(name) -> Option<ItemTypeId>`
     (FxHashMap or a sorted table; lookups only at load/command time, never per tick).
   - Seeded from the canonical item list of the newest supported release (the names in
     `protocol/modern/items.rs`), sorted by name → dense ids. Unknown names met later are appended.
   - **Append-only per world**, persisted under key `b"ITM"` (check unused) with the same blob shape
     as `save_registry`. On open: the persisted table wins; names missing from it are appended; ids
     never move.
   - Process-wide handle via `OnceLock` installed at startup (same pattern as the uid allocator in T03).
2. **Fail loud, both registries.** A present-but-unreadable `ITM` or `REG` blob is an error that
   stops world open (`StorageError::Codec`), never a fallback to defaults. Missing = fresh world.
3. **Hot structs carry ids:** `Stack.item: ItemTypeId`; events `item: ItemTypeId` (event
   `FORMAT_VERSION` bump); provenance/ledger entries likewise. Inventory blob stores `u16`.
4. **Game data stays readable:** tables keep `&'static str`; each table resolves its names to ids
   once at startup into a parallel `Vec<ItemTypeId>` / lazy static, panicking with the offending
   name if a literal is not in the registry (a typo is a bug, M-PANIC-ON-BUG). Hot code compares ids.
5. **Wire:** every codec builds `Vec<i32>` indexed by `ItemTypeId` → its protocol id (and the reverse
   map) at codec construction; unknown → the codec's "air"/barrier fallback, logged once.
6. **External surfaces keep names:** commands, Postgres/ClickHouse rows, logs — always the name, so
   archives stay meaningful when ids differ between worlds.

## Tasks
| id | what | owner | depends |
|---|---|---|---|
| P1a | `registry::items` + persistence + fail-loud for `ITM` and `REG` | Claude | — |
| P1b | `Stack.item` → `ItemTypeId`, inventory encode/decode, windows/crafting/containers | opencode | P1a, T03 |
| P1c | codec id tables per version, wire encode/decode via ids | opencode | P1a |
| P1d | events/ledger/provenance/economy carry ids; names at external surfaces | opencode | P1a, P1b |
