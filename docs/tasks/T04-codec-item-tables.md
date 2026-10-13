# T04 (phase 1, P1c) — per-version item id tables keyed by `ItemTypeId`

## Context
Aether Engine (`/srv/dev/projects/Aether-Engine`), Rust. Phase 1 of storage v2
(`docs/storage-v2/phase1-item-types.md`) moves items from `String` names to
`ItemTypeId(u16)` (`crates/aether-world/src/registry/items.rs`, re-exported as
`aether_api::ItemTypeId` / `aether_world::ItemTypeId`). Vanilla ids there are the 1.21.11 item ids
(`ItemTypeId::vanilla(name)`, `ItemTypeId::vanilla_name()`); ids `>= ITEM_BEYOND` are modded items
interned per world and have no wire id in any vanilla release.

The modern codecs (1.21.11 and 26.1–26.3) translate items through
`crates/aether-server/src/protocol/modern/version.rs`:
- `Version::item_id(&self, name) -> Option<i32>` — binary search by name (fine);
- `Version::item_name(&self, id: i32) -> Option<&'static str>` — **linear scan of ~1500 entries**,
  called while decoding client packets (hot path).

## Where to work
```
git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/T04 -b oc/T04
cd /srv/dev/projects/Aether-Engine-wt/T04
```
Build only there, `-j4` max.

## Wanted
1. In `version.rs`, per `Version`, two tables built once (lazily, e.g. `OnceLock` fields or a
   `static` per version — follow how the struct is constructed today):
   - `to_wire: Vec<i32>` indexed by `ItemTypeId.0` for every vanilla `ItemTypeId`
     (`0..ITEM_BEYOND`), value = this version's wire id for that item's name, or `-1` when the
     item does not exist in this version;
   - `from_wire: Vec<ItemTypeId>` indexed by wire id → engine id (`ItemTypeId::AIR` for ids this
     version has but the engine registry lacks — log nothing here).
2. New methods: `wire_item(&self, id: ItemTypeId) -> Option<i32>` (None for `-1` and for any
   `id >= ITEM_BEYOND`) and `engine_item(&self, wire: i32) -> Option<ItemTypeId>`.
3. Re-implement `item_name(&self, id: i32)` via `from_wire` + `vanilla_name()` (O(1)); keep
   `item_id(name)` as it is. Do **not** change call sites outside `version.rs` in this task
   (P1b will switch them) — only make the existing API fast and add the id-based one.
4. Old codecs (`protocol/v755` … `v773`, `v47`) are out of scope.

## Tests
- For each modern version: every name in its item table round-trips
  `name → ItemTypeId::vanilla → wire_item → engine_item → vanilla_name == name` when the name
  exists in the engine registry; count and assert how many names a version has that the engine
  registry lacks (expected 0 for 1.21.11; for 26.x print/assert the actual number so a future change
  is visible).
- `wire_item` of an id `>= ITEM_BEYOND` is `None`; `engine_item(-1)` and out-of-range are `None`.
- `item_name` results are identical to the old linear implementation for every wire id
  (keep the old one as a test-only helper for the comparison).

## Done when (in the worktree)
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -j4 -- -D warnings`
- `cargo test -p aether-server -j4`

Commit on `oc/T04` (e.g. `codecs: per-version item tables keyed by ItemTypeId; O(1) item_name`).
No push, no merge, no sudo, no edits outside the worktree. Match the comment style (explain *why*).

## Report back
Branch, commit hash, check results, the per-version "names missing from the engine registry" counts.
