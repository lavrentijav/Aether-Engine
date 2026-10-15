# PV1 — item provenance gaps (journal format v4)

Read `docs/tasks/COMMON.md`, `crates/aether-world/src/journal/event.rs` (format, versioned decoder: v1–v3
are still readable — keep that), `crates/aether-server/src/game/provenance.rs`, `inventory.rs`,
`game/window.rs`, `crates/aether-server/src/db.rs` (Postgres mirror).

## Why (found on the live test server, fi2)

1. **Adding to an existing stack is journalled as a move to the same slot.** Stack #65588 (golden apples)
   got `item_move inv:…:39 → inv:…:39` with count 3, then 4: the added apples came from `/give` but there
   is no mint for them, so the count grows from nowhere. A dupe detector would flag every `/give`.
2. **`cause` is always empty**, so a destroy does not say why (eaten, despawned, burned, used in a
   craft, creative replace, command, lava/void, explosion).
3. **Moving an item through the crafting grid is journalled as `item_derive`** (diamond #65643 "derived"
   from diamond #65642). `derive` must mean a real transformation (craft/smelt/brew output).

## Design (decided — implement exactly; space matters more than convenience)

- **Format v4** (`FORMAT_VERSION = 4`); v1–v3 decode unchanged.
- New body **`ItemMerge { into: ItemUid, from: ItemUid, count: u8 }`** (tag 6): `count` units of stack
  `from` join stack `into`; `from` ceases to exist if fully merged (no separate destroy). Any source that
  creates units first emits its mint (`/give`, pickup of a ground item = the ground item's uid, smelt
  output, creative pick), then the merge. Splitting a stack = `ItemDerive` of the new part from the old?
  **No:** splitting keeps using the existing split path; only check it mints/moves consistently and
  document it.
- **`ItemDestroy` gets `reason: u8`** (enum `DestroyReason`): 0 other, 1 consumed (eat/drink), 2 despawn,
  3 burned (fire/lava), 4 void, 5 explosion, 6 crafted (ingredient used), 7 creative_replace,
  8 command (`/clear`, `/kill` drops…), 9 cactus/damage, 10 merged-away (only if a merge path cannot
  avoid it). One byte in the event, `smallint` column `reason` in the Postgres mirror (add with
  `ADD COLUMN IF NOT EXISTS`; no text).
- Crafting-grid placement and pickup without a craft are `ItemMove`. `ItemDerive` only for a real output
  (craft result, smelt/brew/stonecut/smithing result) with `from` = the consumed ingredient uids.
- No new strings anywhere in the journal; the item name stays only on mint (joins are fine).

## Tests (must exist)
- `/give` into an existing stack → mint(new) + merge(new→existing); the per-uid ledger balances
  (sum of mint counts = units alive + destroyed + merged away).
- pickup of a ground item onto a stack → merge from the ground uid; no orphan ground uid left.
- eating → destroy reason consumed; despawn → reason despawn; craft ingredients → reason crafted.
- moving through the crafting grid without crafting → only moves.
- v3 blobs still decode; v4 round trip; the Postgres insert builder emits `reason` and `merge` rows.
- A ledger-balance property test over random window/give/pickup/eat sequences.

Commit per step (PV1a format + types, PV1b server emitters, PV1c mirror, PV1d tests/docs). Update
`docs/STATUS.md`/`KNOWN_ISSUES.md` (EN+RU). Env CARGO_INCREMENTAL=0; tests via cargo-locked test.
