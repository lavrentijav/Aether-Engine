# QP notes — task J10 (block items follow block rules + per-move round trip)

Branch `oc/QP`, worktree `/srv/dev/projects/Aether-Engine-wt/QP`.

## Part 1 — block items follow the block rules

* `tools/gen/compat/gen.js` now exports its resolver, rule loader and name
  helpers and only generates when run directly (`require.main === module`).
  `tools/gen/compat/gen_items.js` requires it and resolves a block item
  (an engine item whose name is an engine block) through the same
  `blocks.json` rules and the same chained resolver the block half uses; the
  `barrier` placeholder is gone.
* A block no rule covers takes the version's **stone item** — the block
  table's own last resort — so `ITEM_FALLBACKS` holds only non-block items.
  Fallbacks dropped from 3919 to 623; 1.21.9 is 6.
* Regenerated `crates/aether-server/src/protocol/compat/item_gen/*.rs` (the
  block tables regenerate byte-identical; `english.rs` unchanged).
* Test `no_block_item_is_left_without_a_stand_in` asserts no fallback names an
  engine block in any release; `how_many_items_still_have_no_stand_in_is_recorded`
  now pins the much smaller counts.

## Part 2 — the round trip follows a move, not just a slot

* `compat::items` gains `ItemTable::stand_in_wire` (is a wire id ambiguous?),
  `resolve_move` (resolve a report against the window's real items),
  `resolve_change` (the pure per-move decision, returning the resolution and
  the updated shadow), and `table_for(protocol)` (the table a codec serves).
* `session::record_slot_change` resolves each creative slot report through
  `table_for(codec.protocol_id())`, shadowing the creative cursor in the new
  `PlayerState::creative_carry`. A stand-in moved slot → cursor → slot keeps
  its real item; a stand-in with no source is refused and the client resynced.
  A version with no table (1.21.11 and later, whose ids are the engine's own)
  is trusted as before, still honouring `ProtocolCodec::resolve_reported_item`.
* Tests: `a_moved_stand_in_keeps_the_real_item_it_stands_for`,
  `a_stand_in_survives_being_carried_through_the_cursor_between_slots` (the
  slot → cursor → empty slot → another slot flow, plus the refused conjure and
  a genuine item still believed).

## Checks

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4 --
-D warnings`, `cargo-locked test -p aether-server` (796 passed) all pass.

## Merge with main (T4 + QO)

`git merge main` conflicted in `tools/gen/compat/gen.js` (T4 added `META_MAPS`
and the `meta`/`legacy` rule fields with a richer `resolver` returning
`{ name, rule, cap }`) and in `docs/KNOWN_ISSUES.md`. Resolved so both survive:

* `loadRules(file = 'blocks.json')` keeps the parameter (J10) and also parses
  `metaMaps` and the `meta`/`legacy` fields (T4); `main()` keeps the
  require-safe exports and the `flatRules = rules.filter(r => !r.legacy)`
  split, so a flattened version drops the legacy-only rules.
* `gen_items.js` follows the object-returning resolver (`resolveItem(name)?.name`)
  and passes all rules to 1.8 but the legacy-filtered set to flattened
  versions, so its block-item stand-ins exactly match the block tables'.
* Docs: kept T4's item 14 and J10's item 15; synced the Russian item 14 to
  T4's (main had updated only the English one and left the stale 422-block
  list).

`node tools/gen/compat/gen.js` regenerates `compat/gen/*` **byte-identical to
main's**; `gen_items.js` regenerates only `item_gen/v1_8.rs` differently from
the pre-merge J10 tables (the legacy rules), the flattened tables unchanged.

## Left / interface note for the J5 (J9) agent

The per-move logic keys off `compat::items::table_for(handle.codec.protocol_id())`.
If the older codecs (v766–v773) are instead routed through the modern codec
with its own per-version `Version` tables, that codec must expose the same
resolution (or so must its `protocol_id`) for the shadow to engage; today the
compat tables are reached by `protocol_id` 47 and 755–773.
