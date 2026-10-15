Verified against the Java in `/tmp/mc`, probes under `/tmp/opencode/probe`. `CARGO_INCREMENTAL=0 cargo test --locked -p aether-redstone` (19 unit + 31 integration + 1 external + 1 doc) and clippy `--all-targets` are clean. No repo files edited.

## Status of round-3 findings

| # | Finding | Status |
|---|---------|--------|
| D1 | Hard 512 chain cap mislabelled | **FIXED** — `RedstoneView::max_chained_neighbor_updates` (default 1,000,000, per-world) read in `Context::new`; probe: a supported 21×21 wire patch charged by one `on_placed` resolves to exact `16 − Manhattan` (wrong=0); a limit of 16 truncates |
| D2 | `NeighborSource.pos` was the fan-out centre | **FIXED** — `Multi`/`Simple` carry `changed` separately from `center`; `updateNeighborsAt` reports the changed cell, the diode blast/wire blast report the diode/wire; tests `except_facing_update_carries_the_diode_as_source`, `a_wire_blast_reports_the_wire_as_source` |
| D3 | Standing torch used the FULL test | **PARTIAL** — switched to `shapes::face_center`; but the approximation is wrong in the over-accept direction (below) |
| D4 | Shape budget started at `j`, not `j-1` | **FIXED** — `on_placed`/`on_removed` pass `SHAPE_UPDATE_LIMIT - 1`; budget test measures 512 |
| D5 | Registry not extensible / dead surface | **FIXED** — `behaviour::register` public, external crate test `an_external_crate_registers_a_block_behaviour` passes; `is_repeater`/`update_neighbors_except` removed |

## New defects

**N1 — MAJOR. Repeater survival uses the FULL face test where vanilla uses `SupportType.RIGID`; a repeater on a hopper is destroyed.** `blocks/repeater.rs:116-120` (`if !shapes::face_full(below, Direction::Up) { return AIR }`; the comment even says "isFaceSturdy(UP, RIGID); a full top face is used here as a close approximation").
Vanilla `DiodeBlock.canSurviveOn` = `isFaceSturdy(..., UP, SupportType.RIGID)` (`DiodeBlock.java:56`); `RIGID_SUPPORT_SHAPE` is only the 2/16 frame (`SupportType.RIGID`, `Block.column(12,0,16)`), and a hopper's top ring covers exactly that frame, so `RIGID(hopper,UP)` is **true** while `FULL` is false.
Circuit (probe): hopper at (0,0,0), `repeater[facing=north,delay=1]` at (0,1,0); flip the hopper to `enabled=false` through `set_block` → a DOWN shape update reaches the repeater → engine result **`minecraft:air`**, vanilla keeps the repeater. This is not covered by the KNOWN_ISSUES wording ("support shape approximated by collision shape") — it is a *predicate* error (FULL vs RIGID), and it will hit comparators too (same `DiodeBlock` base), including the canonical comparator-on-hopper.

**N2 — MAJOR (test). The standing-torch CENTER approximation over-accepts supports whose centre is only covered below the top; the test pins the wrong vanilla behaviour.** `shapes.rs:106-119` (`face_center` = "any collision box covers the central 2×2, ignoring the axis"), `blocks/torch.rs:99-107`, test `a_standing_torch_survives_on_a_fence_or_a_hopper`.
Vanilla `BaseTorchBlock.canSurvive` → `Block.canSupportCenter(..., UP)` → `isFaceSturdy(UP, SupportType.CENTER)` (`Block.java:340`, `SupportType.CENTER`), which compares the block's **top face** (`getFaceShape(UP)`, the extreme 1/16 slice — `VoxelShape.calculateFace`) against the central 2×2 column. A hopper's top slice is the rim ring (probe boxes: only `[0,0.6875,0,0.125,1,1]`, `[0.125,0.6875,0,1,1,0.125]`, `[0.125,0.6875,0.875,1,1,1]`, `[0.875,0.6875,0.125,1,1,0.875]` reach y=1; the centre is hollow), so `CENTER(hopper,UP)` is **false**. Engine probe: `face_center(hopper,UP)=true` and the torch survives. So `a_standing_torch_survives_on_a_fence_or_a_hopper` asserts that a torch survives on a hopper, which vanilla rejects; the fence half of the test is correct. The flaw is structural: `face_center` ignores the axis extent, so the hopper's *sub-top* central funnel box counts as support.

**N3 — MINOR. The registry leaks a full table on every rebuild and can drop a late registration under a race.** `behaviour.rs:625-700` (`build()` `Box::leak`s a new `Vec<&'static dyn BlockBehaviour>` of `STATE_COUNT≈29,671` entries — ~0.5 MB — on every `register`), `register` sets `REGISTRY=None`, and `behaviour()` rebuilds lazily. Two threads or a `register` interleaving between `build()` and the `REGISTRY.write` can leave the stale table installed, losing the registration until the next `register`. Acceptable for startup-only registration, but it is a new process-global mutable with per-lookup `RwLock` read.

**N4 — MINOR (documented). Dropping is delegated, not implemented.** `behaviour.rs:614-620` calls `RedstoneView::drop_block`; the crate spawns no item, so a popped wire/torch/repeater drops nothing unless the host implements it. Vanilla `destroyBlock(pos, drop=true)` runs the loot table. Already in KNOWN_ISSUES; noted for completeness.

## On the stated deviations

- **Support shape ≈ collision shape (wire, repeater, lever, button):** does not matter. Only ~9 blocks override `getBlockSupportShape` (hanging signs, fence gate, leaves, mud, shulker box, snow layer, soul sand, multiface); for the redstone-relevant ones the FULL/CENTER answer is unchanged, so no observable circuit difference.
- **Wall torch keeps FULL via collision shape:** does not matter — `WallTorchBlock.canSurvive` really is the default `isFaceSturdy(dir)` = FULL (`WallTorchBlock.java:57`), so the predicate is right and the shape-source approximation is immaterial (probe: wall torch on a bottom slab correctly pops).
- **CENTER approximation:** matters (N2): it over-supports hollow-top blocks, and the test encodes the wrong answer for the hopper.
- **Unstated:** the repeater's FULL-vs-RIGID predicate (N1) matters more than any of the above and is only a code comment.

## Verdict

**NOT STABLE** — blockers: N1 (repeaters/comparators are destroyed on RIGID-but-not-FULL supports such as a hopper) and N2 (`face_center` accepts a hopper's hollow top, and `a_standing_torch_survives_on_a_fence_or_a_hopper` pins the wrong vanilla result). Both are localized to the support predicates, and the extension architecture (per-block trait, public registry, flag-driven writes, depth-first updater, per-world ticks/burnout, `j-1` budget) is otherwise sound and ready to extend once these two are corrected.
