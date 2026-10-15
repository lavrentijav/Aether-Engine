# RP1 review 1 — compat continued + per-version resource pack

Reviewer pass over `oc/RP1` vs `main` (branch tip `b1a2199`). Read-only: no tracked
file was edited. Deliverable reviewed: `docs/tasks/QUEUE-RP-compat-pack.md`,
`docs/design/compat-resource-pack.md`, `docs/tasks/RP1-notes.md`, the Part A
codec wiring and the Part B prototype (`compat::pack`, `pack_gen/v1_20_1.rs`,
`tools/gen/compat/gen_pack.js`, `tools/gen/compat/pack.json`, `tools/pack/build.js`).

## What I ran

| check | command | result |
|---|---|---|
| tests | `CARGO_TARGET_DIR=/srv/dev/build/aether-target CARGO_INCREMENTAL=0 cargo-locked test -p aether-server --lib` | **963 passed, 0 failed** (22.35s) |
| fmt | `cargo fmt --all -- --check` | clean |
| clippy | `cargo clippy -p aether-server --all-targets -j4 -- -D warnings` | clean |
| generator reproducibility | `node tools/gen/compat/gen_pack.js /tmp/opencode/packgen` then `diff` vs `pack_gen/v1_20_1.rs` | **byte-identical** |
| pack builder | `node tools/pack/build.js --release 1.20.1 --stub-texture --out /tmp/opencode/packout` | pack.mcmeta `pack_format 15`; `note_block.json` has 1150 variants, exactly one (`instrument=harp,note=1,powered=false`) remapped, canonical `note=0` left `minecraft:block/note_block` |
| status ids | cross-checked every `resource_pack_status_id()` against `minecraft-data` | 47→0x19, 755–758→0x21, 759→0x23, 760–763→0x24: all correct |
| carrier math | `minecraft-data` 1.21.11 note_block min 581 + offset 3 = 584; 1.20.1 min 538 + 3 = 541 | matches `pack_gen` (25,584), `V1_20_1.state = 541` |

## Part A — pre-1.20.2 item wiring and compat-table regressions

**A.1 item wiring: correct.** `legacy_codec_item_hooks!` (`protocol/mod.rs:757`)
binds `initial_hotbar`, `block_for_item`, `resolve_reported_item` and
`resource_pack_status_id` on all ten old codecs (47, 755–763). `initial_hotbar`
now seeds `STARTER_HOTBAR`; `block_for_item` reuses `modern::block_for_item`;
`resolve_reported_item` goes through `legacy_item_table(protocol)`
(`protocol/mod.rs:724`). The v47 hard-coded `HOTBAR` id list is gone and the
1.8 hotbar ids now come from `V1_8.wire(..)`. The two new tests
(`the_pre_1_20_2_codecs_are_wired_into_the_item_layer`,
`...report_their_play_state_pack_status_id`) walk all ten codecs. Good.

**A.1 "block items follow their block's rules" and "per-move round trip": not
delivered on this branch.** Both are explicitly deferred to J10 (`oc/QP`) in
`RP1-notes.md` and remain open in `docs/KNOWN_ISSUES.md` item 15
(`docs/KNOWN_ISSUES.md:77`): the item tables still map an absent *block* item to
`barrier` and record it in `ITEM_FALLBACKS`, and the round trip is still
per-slot. The queue's Part A.1 asked for these here. They are **not regressions**
(the placeholder is pre-existing and J10 is a live sibling branch), but the
branch does not satisfy the queue bullet as written; this should be recorded as a
known split, not silently counted as done.

**A.2 audit: plausible, not machine-checked in-tree.** `RP1-notes.md` reports
3942 substitutions, 2974 clean / 968 with a mismatch, 0 stone fallbacks. The
audit is reproducible only by re-running the author's script shape; no checked-in
tooling or fixture backs the numbers. Acceptable as documentation, but it is a
claim, not a test.

**Compat tables byte-identical: confirmed.** No file under
`crates/aether-server/src/protocol/compat/gen/` or `compat/item_gen/` is touched
(`git diff --name-status main...oc/RP1`); `gen_pack.js` writes only to
`pack_gen/`, and regenerating `pack_gen/v1_20_1.rs` reproduces it exactly. No
regression here.

**Behaviour change worth confirming (medium):** because the macro now returns
`STARTER_HOTBAR` from `initial_hotbar`, `session.rs:397` seeds the *server*
inventory for 1.17–1.20.1 — but those codecs never send a hotbar to the client
("No hotbar" comments at `v755/mod.rs:202` … `v763/mod.rs:172`, they rely on the
creative menu). So `held_item()` (`players.rs:179`) now reports stone/dirt/…
for slots 0–6 that the client was never given, and the legacy placement path
(`session.rs:745-752`) prefers that fictional held item over the client-claimed
block. Net effect: placement in slots 1–6 changes from a fixed `STONE`
(`v755/mod.rs:415` … `v763/mod.rs:387` all hard-code `block: STONE`) to the
seeded slot's block. It is not obviously wrong, but it is an undocumented
behaviour change in a "wiring" commit and it deserves a deliberate decision/test.
The v47 count also changed from `1` to `64` (`v47/mod.rs` `finish_join`) while the
old comment said "count 1"; harmless but undocumented.

## Part B — the prototype

**Real note blocks: sound ok, canonical state NOT enforced (design invariant
unimplemented).** The pack only retextures `note=1` and leaves the canonical
`note=0` variant on `minecraft:block/note_block`, so the canonical state's model
(and the vanilla `note_block` sound event, which the pack never touches) is fine.
But design invariant §2.1 / §7 ("the server pins real instances to one canonical
state … so a real carrier placed by a player is coerced back",
`docs/design/compat-resource-pack.md:26-27,109,147-151`) has **no code**: nothing
searches for or canonicalises `note_block` states on the write path, and the
generator does not reserve the canonical states. A genuine note block in the
disguise state `(harp, note=1, powered=false)` — reachable: note blocks are
craftable (`game/data.rs:3517`) and tunable by the client — renders as pale-oak
planks. The design's stated safety property does not hold yet. For a one-block
prototype this is a narrow window, but it is a real hole and it is exactly one of
the review's checkpoints.

**A player without the (compat) pack can still get carrier states — blocker.**
The gate is per-player status plus protocol only:

```rust
// players.rs:284
fn pack_table(&self) -> Option<&'static ...PackTable> {
    self.pack_loaded()
        .then(|| compat::pack::table_for(self.codec.protocol_id()))
        .flatten()
}
```

`pack_loaded()` is set by *any* `0` status reply, and the status interception in
`session.rs:603` is unconditional — it does not know which URL was offered. But
`resource_pack_for` (`session.rs:482`) deliberately offers the operator's plain
`[resource_pack]` when `[compat.pack]` is off/empty for that release. So on
1.20.1 with a normal `[resource_pack]` set and `[compat.pack]` off (or on but
`url=""`), the client loads the operator's pack, sends
`resource_pack_receive = 0`, the server marks it loaded and starts remapping
`pale_oak_planks → note_block[note=1]`. The client sees a raw note block (the
operator's pack has no such variant) instead of the plain J2 stand-in. This
directly violates the review checkpoint "a player without the pack never gets
carrier states" and the design's "No pack → plain stand-in" invariant (§2.4). The
existing `players::tests` only drive `set_pack_status` directly, so they cannot
catch it.

Minimal fix: remember, per player, whether the *compat* pack was the offered one
(e.g. only intercept/record the status when `cfg.compat.pack.active()` and
`table_for(protocol)` is `Some`, or store the offered URL and compare), and make
`pack_table()` depend on that, not on `pack_loaded()` alone.

**Pack hash / URL handling: partial.** `CompatPackConfig::active()` requires
`enabled && !url.is_empty()` and `offer()` returns `{url, hash, required}`
(`config.rs:186-197`); `resource_pack_for` precedence is as designed. But the
server never checks that the offered `pack_format` matches `PackTable.pack_format`
(stored but unused outside tests), and the design's generated
`packs/compat/index.json` manifest (§8.4) does not exist — the operator maps
`url`/`hash` by hand. Acceptable for a prototype, but the "the pack offered must
declare the same format" contract is unenforced.

**No vanilla assets committed: confirmed.** No `.png`/pack output is tracked
(`git diff --stat` has none); `/packs/` is gitignored (`.gitignore`); the only
embedded image is the synthetic `STUB_PNG` in `build.js`. `client_jar` is
build-time only.

**`tools/pack/build.js`: correct for the prototype.** I ran it end-to-end with
`--stub-texture`: it writes `pack.mcmeta`, the merged full `note_block.json`
(all 1150 variants, canonical left vanilla, one override), the `cube_all` model
and texture. It guards the override key against the carrier's real variant set
(`throw` when a key is not a variant), and `gen_pack.js` throws on
translatability/duplicate-carrier-state errors. Two limits, both documented or
minor: a single `--texture` serves every assignment (per-block override needed
for the multi-block follow-up), and the `pack_format` is taken from the
checked-in `pack.json` rather than read from the jar's `version.json` as §8.1
demands (contradiction between design and implementation).

## Other notes

* Session parse `PacketIn::new(&pkt.data).var_int().unwrap_or(3) as u8`
  (`session.rs:604`) does not range-check: a VarInt of 256 casts to `0`
  ("loaded"). No security value (a client only fools itself into seeing
  carriers), but it should be a `match` on the enum, not an unchecked cast.
* Design §7 ("the engine's own light pass reads the carrier (that is what is in
  the world)") contradicts §2.3 ("the carrier state exists only on the wire");
  the world holds the real block. Doc-only, but the two sections disagree.
* No test covers `tools/pack/build.js`; the notes' claim that tests assert the
  wire state and the pack variant key agree is only half true — the wire state
  (541) is tested, the builder's key is not (I checked it manually).

## Summary

Part A's codec wiring is clean, tested, and does not touch the generated compat
tables (byte-identical, regenerable). Part B's prototype is small and mostly
careful. But the branch as-is can send carrier states to a client that loaded a
*different* resource pack, which is the precise invariant the task asks to
protect, and the "real carriers stay canonical" invariant is only asserted in the
design, not implemented. The Part A.1 block-item and per-move bullets are also
not satisfied here (deferred to J10). Fix the pack-status gate (and, for the
prototype's honesty, either implement or explicitly scope out carrier
canonicalisation) and re-run the touched tests; the rest can land with follow-ups.

Verdict: DO NOT MERGE
