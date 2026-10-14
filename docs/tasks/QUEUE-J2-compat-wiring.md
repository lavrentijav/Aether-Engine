# Queue J, stage 2 — wire the compat layer end to end

Read `docs/tasks/COMMON.md`, `docs/tasks/QUEUE-J-compat.md` (stage 1 brief) and
`docs/tasks/QJ-notes.md` / `docs/tasks/QK-notes.md` (what stage 1 did and left). Stage 1 is merged
into main: `protocol/compat/{blocks,items,behaviour}.rs`, generated `compat/gen` + `compat/item_gen`,
generators in `tools/gen/compat/`. Three agents work in parallel, one task each:

## J9 — gameplay path for the 1.20.5–1.21.10 codecs (branch oc/QO)

Implement the plan in `QJ-notes.md` § J5: route v766–v773 through the modern gameplay path
(`full_gameplay() == true`), sharing `modern/play.rs` writers where the layout is identical and
gating the differences by version. Start with **v767 (1.21/1.21.1)** end to end (entities, health,
hunger, windows, items via `compat::items`, effects), then the others one commit each.
Tests: decode/encode tests per packet that differs; the J4 spear test on v767 (use-item + release on a
trident stand-in holding a real spear → spear charge, no trident entity, slot resent) must pass.
Do not edit the generators or `compat/items.rs` internals (another agent does); call their API.

## J10 — block items follow block rules + per-move round trip (branch oc/QP)

1. In `tools/gen/compat/gen_items.js`, replace the placeholder (`barrier`) for *block items* with
   the block substitution of `tools/gen/compat/blocks.json` through the shared resolver in
   `gen.js`; regenerate `compat/item_gen`; `ITEM_FALLBACKS` must shrink to items that are not blocks.
   Test: for every release, no block item whose block has a rule falls back to a generic item.
2. KNOWN_ISSUES 15, second bullet: the round trip is per slot only. Make it per **move**: when the
   client moves/swaps/splits a stand-in, resolve the destination from the server's authoritative
   window state (cursor + source slot), not from the wire id. Test: a creative/survival client moves a
   spear stand-in (trident) through cursor → empty slot → another slot → it is still a spear on the
   server; a client cannot conjure a real trident from it.
Do not edit codec gameplay code (another agent does).

## J11 — small fixes found by review (branch oc/QQ)

1. Postgres mirror migration (`crates/aether-server/src/db.rs`): an existing table whose
   `item_uid` is `uuid` and `derived_from` is `uuid[]` (pre-T03 schema) makes every insert fail,
   because `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS` do nothing. On connect, read
   `information_schema.columns`; if a column type differs from `COLUMNS`, migrate it (old uuid item
   ids cannot be converted: `ALTER ... TYPE bigint USING NULL`, log once how many rows lost the id).
   Test with the SQL builder tests the file already has (no live Postgres needed).
2. `/audit` and `/inventory` print item uids as `{:016x}` hex (`commands/mod.rs` ~222, 230, 451)
   while the mirror stores decimal: print decimal everywhere, and fix the doc comment in `db.rs`
   that claims they already agree.
3. `ledger.rs` reservation: a corrupted 8-byte bound near `u64::MAX` overflows
   `(v / UID_BLOCK + 1) * UID_BLOCK`; use checked arithmetic and fail loudly (error), never wrap to 0
   (`ItemUid(0)` means "no id"). Test with a bound of `u64::MAX`.
4. `protocol/modern/version.rs` item tables assume dense wire ids: assert density when building
   (`debug_assert` + a unit test over all versions), and treat an out-of-range wire id as `None`, never
   an index panic.

## Done

Per COMMON.md. Report: hashes, tests, what is left.
