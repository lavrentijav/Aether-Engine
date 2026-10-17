# CMD-36b independent review

VERDICT: CHANGES

Reviewed `oc/CMD36b` (`ee540ac`) against `main`. Targeted gates all pass:

- `cargo fmt --all -- --check`
- `cargo-locked clippy -p aether-server -p aether-worldedit-plugin --all-targets -- -D warnings`
- `cargo-locked test -p aether-server --lib -p aether-worldedit-plugin` — 1449 + 49 tests pass

Verified as correct: copy/cut/paste `-a`/`-o`/`-s`; empty clipboard errors; copy/paste and stack/move limits are refused before `begin_edit`; cut fills the clipboard and clears the source in one commit; stack direction and both move argument orders; undo records paste/stack/move/cut/brush through `commit_writes`; region checks run on the cells actually written (including both source and destination for move), not on the source for copy/stack; op 0 for selection/wand/copy/rotate/flip/mask and op 2 for destructive/undo/schem; schematic files persist under `<world_dir>/schematics`. The problems below are not covered by the current tests.

## Findings

1. **MAJOR — rotation keeps the old offset, so the pivot is the clipboard minimum corner instead of the origin/centre.**
   `plugins/worldedit/src/clipboard.rs:36` (`remap` copies `offset` unchanged), used by all three rotations at `clipboard.rs:52-65`; paste then anchors at `player + offset` (`plugins/worldedit/src/lib.rs:867-875`). After a 90°/270° rotation the footprint changes (`[w,h,d] -> [d,h,w]`) but the offset is not transformed, so the rotated clipboard is anchored to its old minimum corner. Probe: select `(10,0,10)..(12,0,10)`, stand at `(11,*,10)`, `//copy`, `//rotate 90`, `//paste -o`; the rotated copy lands at `x=10, z=10..12`, not centred on the player at `x=11, z=9..11`. This contradicts `plugins/worldedit/README.md:121` ("around the box centre") and WorldEdit's behaviour (rotation about the clipboard origin). X/Z rotations with unequal extents have the same problem. Fix by transforming the offset along with the size (or storing the origin and applying the transform at paste time), and add a test where the player is not at the minimum corner.

2. **MAJOR — `.schem` `Offset` uses the wrong Sponge v2 semantics, so files are not WorldEdit-compatible.**
   `plugins/worldedit/src/schematic.rs:139` writes `clipboard.offset` (which is `min - copy origin`, see `lib.rs:380-387`) into the top-level `Offset`, and no `Metadata.WEOffsetX/Y/Z` is written (`schematic.rs:132-144`). In Sponge v2 the top-level `Offset` is the **absolute minimum point**, while `Metadata.WEOffsetX/Y/Z` is `min - origin`. The reader repeats the mistake: `schematic.rs:432-442` reads the top-level `Offset` as a relative offset, and `lib.rs:1190-1197` adds it to the current player position. `to_bytes`/`from_bytes` round-trip only because both sides share the error. A real WorldEdit `.schem` currently pastes at `player + absolute-min` (usually far away), and WorldEdit loading our file interprets the relative offset as the absolute minimum. Persist the absolute min (or origin) in `Clipboard`, write top-level `Offset = min` plus `Metadata.WEOffset = min - origin`, and on load prefer the metadata for relative placement. Add a fixture produced by WorldEdit/Sponge, not just a self-round-trip.

3. **MAJOR — `//rotate` and `//flip` move block strings but do not transform block states.**
   `plugins/worldedit/src/clipboard.rs:40-65` and `:85-93` only clone the stored block name; `lib.rs:878-929` applies those remaps. Directional states therefore keep their old orientation: `oak_stairs[facing=north]`, logs/`axis`, rails/`shape`, `half`, chest `type`, etc. are wrong after a rotation or mirror. WorldEdit transforms the state (facing/axis/half/shape) along with the position. If state transforms are intentionally out of scope, the README should say so; otherwise structures with stairs, logs, rails or directional blocks cannot be rotated/flipped correctly.

4. **MINOR — brush limit checks use the bounding cube and reject legal brushes.**
   `plugins/worldedit/src/brush.rs:20-26` (`estimated_volume` = `(2r+1)^3` for spheres/cylinders), enforced at bind time in `lib.rs:1297-1303` and again in `apply_brush` before the exact writes are counted (`lib.rs:1369-1374`). A sphere of radius 6 has 925 cells but an estimated 2197, so with `max-volume = 1000` it is refused even though the actual edit is under the limit; the same applies to cylinders and `smooth`. The exact cell count is already available from `Brush::cells`; use it (or a cheap exact count) before refusing.

5. **MINOR — `//schem load` has no size/decompression cap and can OOM before the volume limit is seen.**
   `plugins/worldedit/src/schematic.rs:203` inflates the whole gzip stream with no output bound, `schematic.rs:409-420` trusts `Width`/`Height`/`Length` and does `Vec::with_capacity(volume)` before any `max_volume` check, and `lib.rs:1147-1149` installs the result as the clipboard. A crafted or simply very large `.schem` can abort the process (allocation failure) and an over-limit clipboard is only rejected later by `//schem paste`. Pass the max volume into `from_bytes` and check dimensions/volume (and a decompressed-byte cap) before allocating.

6. **MINOR — `//stack` refuses stacks that fit the core limit.**
   `plugins/worldedit/src/lib.rs:958-963` tests `blocks * (copies + 1)` although only `blocks * copies` cells are written (the source is not changed). The core limit is on distinct pending positions (`docs/design/bulk-edit.md:84`), so e.g. a 600k selection with `//stack 1` under a 1M limit is refused as 1.2M. Count the actual writes (or cap the source+copies pre-check separately from the edit limit).

## Test gaps

No test asserts rotation pivot/offset with the player away from the minimum corner, external `.schem` compatibility (only a self-round-trip), directional block-state transforms, exact brush volume, a crafted/oversized schematic, or `//stack` at the core limit.
