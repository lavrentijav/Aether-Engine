# BLK — Block placement rules: the support table, the replaceable list, and the placement constraints

Read `docs/tasks/COMMON.md`, and in the code: `crates/aether-server/src/session.rs`
(`place_block`, `is_replaceable`, the `ClientEvent::PlaceBlock` arm), `crates/aether-server/src/placement.rs`
(`state_for`, `interact`, `reshape`, `neighbour_updates`), `crates/aether-world/src/registry/`
(`blocks.rs`, `props.rs`), and `tools/gen/gamedata.js` (the generator pattern to copy).

## Problem
Placement currently enforces **no support/attachment rule at all**: after the target cell and the state
are chosen (`state_for`), `place_block` writes the block. So a plant, torch, `leaf_litter`, sapling, etc.
can be placed anywhere — on a wall, floating, under water. And "replaceable" is a hand-written `match`
in `session.rs::is_replaceable` (a *subset* of vanilla): it has no `soul_fire`, `glow_lichen`,
`resin_clump`, `light`, `structure_void`, `bubble_column`, `warped_roots`, `nether_sprouts`,
`crimson_roots`, `hanging_roots`, and no `leaf_litter`.

Owners reported two concrete bugs:
- grass can be placed on sand (should follow its valid-soil rule);
- `leaf_litter` can be placed **on a wall** and **under water** (it must be on the floor and not
  under water) — two independent triggers, both unchecked today.

## Goal
One **data-driven placement-rules module** that the server consults on every placement, with a
**generated, documented table** and a readable markdown file, plus the bug fixes. Three things:

### 1. The replaceable list (generated, authoritative)
The vanilla data pack ships the authoritative list:
`<pack>/data/minecraft/tags/block/replaceable.json` (1.21.11: 29 entries — air, water, lava,
short_grass, fern, dead_bush, bush, short_dry_grass, tall_dry_grass, seagrass, tall_seagrass, fire,
soul_fire, snow, vine, glow_lichen, resin_clump, light, tall_grass, large_fern, structure_void,
void_air, cave_air, bubble_column, warped_roots, nether_sprouts, crimson_roots, leaf_litter,
hanging_roots).
- Add a generator (extend `tools/gen/gamedata.js`, or a new `tools/gen/placement.js` + a `regen.sh`
  entry) that emits this list into the checked-in data, the way `game/data.rs` is generated
  ("Do not edit by hand"). Keep a built-in fallback for a server started without a pack, and mark
  which one is in use.
- Replace `session.rs::is_replaceable`'s hand-written `match` with the generated list. Removing a
  name that vanilla does **not** have here is part of the fix; adding the ones above is too.

### 2. The support / "what can be placed on what" table (and its reverse)
Build the placement-support table and **enforce it**. This is the owner's "полный список: какой
блок на какой нельзя ставить или обратное".
- For every block that **requires support**, encode what satisfies it: the block(s) below it, and/or
  a face it attaches to (wall torches, ladders, vines, wall signs, `leaf_litter` on the floor, …),
  and/or a neighbour (e.g. a wall block). Give each rule a clear key and keep them grouped.
- Provide the **reverse index** too (which blocks each block can support / attach to), since the
  owner asked for "or the reverse".
- Sources, in order of preference:
  1. the data pack's block tags where they exist and apply (`#minecraft:dirt`, `sand`, `leaves`,
     `bamboo_plantable_on`, `big_dripleaf_placeable`, `azalea_grows_on`, …);
  2. `minecraft-data` (already used by `tools/gen`) for block properties / state (`attachment`,
     `half`, `facing`);
  3. a **curated, commented** Rust table for the rules vanilla enforces in code and that no data
     source carries (plants need valid soil, `leaf_litter` floor-only + no water, torch/lantern
     support, saplings, crops, …). Be explicit in the module doc about which rules are data-derived
     and which are curated.
- Enforce it in `place_block` (before `set_and_broadcast`): refuse (no write, ack the client) when the
  target position does not satisfy the block's rule. Reuse the existing refusal path (the `!placed`
  branch already re-sends the real block and syncs the inventory).
- Do **not** invent a rule the owner did not ask for and vanilla does not have; when in doubt, check
  the vanilla 1.21.11 behaviour and the pack, and note the decision.

### 3. The two bug fixes
- grass (and the other plants): placement refused on a block that is not in its valid-soil set.
- `leaf_litter`: placeable only on the floor (the block below/face rule), and **not** when the target
  cell is water/under water — encode both triggers explicitly and test each separately (the owner's
  "сразу 2 триггера": only on floor, and not under water).

## A readable markdown table (the owner asked for a file)
Add a generator (like the AX/CMD `COMMANDS.md` pattern) that renders the whole rule set to
**`docs/BLOCK_PLACEMENT.md`** and **`docs/BLOCK_PLACEMENT.ru.md`**, with a byte-compare test so it
cannot drift. The file must show, per rule group:
- the block and its placement constraints;
- the valid support blocks (and the reverse "supports" list);
- the replaceable list;
- a header/legend and a note that it is generated (do not edit by hand) plus the regeneration command.

## Files
- `crates/aether-server/src/placement.rs` (or a new `placement/rules.rs`) — the rules + the check.
- `crates/aether-server/src/session.rs` — `place_block`/`is_replaceable` use the module.
- a generated data file (e.g. `crates/aether-server/src/placement/data.rs`) + `tools/gen/placement.js`
  (or an extension of `gamedata.js`) with a `regen.sh` entry.
- the markdown generator + `docs/BLOCK_PLACEMENT.md` (+ `.ru.md`).
- `docs/KNOWN_ISSUES.md` (+ `.ru.md`) for any remaining deviation you find but do not fix.

## Edge cases to test explicitly
- a plant on invalid soil is refused; on valid soil it places;
- grass on sand is refused (the reported bug);
- `leaf_litter` on a wall is refused; on the floor it places; on the floor **under water** it is
  refused (each trigger a separate test, fail-before/pass-after);
- a full block placed into each replaceable name replaces it (a test driven off the generated list,
  so a new entry is covered automatically);
- a block that is not replaceable is built **against**, not into;
- torch/ladder/wall-block support: refused with the support removed, accepted with it present;
- a refused placement still acks the client (the existing rule) so the client does not desync.

## Done criteria
- The replaceable list is generated from the pack with a no-pack fallback; `is_replaceable` uses it.
- The support table (with its reverse index) is enforced in `place_block`; both reported bugs are
  fixed and covered by fail-before/pass-after tests.
- `docs/BLOCK_PLACEMENT.md` (+ `.ru.md`) generated from the same data, with a drift test.
- fmt / clippy `-D warnings` / `cargo-locked test` clean.
- A report: which rules are data-derived vs curated, what the pack/KNOWN_ISSUES said, and what you
  deliberately did not encode.
