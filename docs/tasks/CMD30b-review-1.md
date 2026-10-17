# CMD-30b review 1 — budgeted bulk-edit publish

- Reviewed commit: `oc/CMD30b` @ `df33351` (adds +99 `bulk.rs`, +17
  `docs/design/bulk-edit.md` over `04d30bf`; production publish code unchanged,
  new light/shape tests and doc)
- Base: branch was based on `main` @ `55529de`. While this review was running,
  `oc/CMD30b` moved to `9332015` (merge of `origin/main` @ `4eedcb2`, `oc/CMD08`).
  `git diff df33351..9332015` touches **no** CMD-30b production file (no
  `bulk.rs`, `benches/bulk_edit.rs`, `aether-plugin`, or `docs/design/bulk-edit.md`
  changes), so every finding below still applies to `9332015`. The `/CMD30b`
  worktree has uncommitted author changes on top; those are **not** part of this
  review.
- Brief: `docs/tasks/CMD-30b-bulk-edit-publish.md` (umbrella `CMD-30-bulk-edit.md`)
- Reviewer worktree: `/srv/dev/projects/Aether-Engine-wt/crit-CMD30b`

## VERDICT: CHANGES

Resend batching, the phase/cursor design, the per-tick config and the hooks are
sound, and all gates pass. The light-via-`compute_column` decision is acceptable
as a temporary stand-in (it introduces no new light holes beyond the engine's
existing per-column model). But the **settle pass does not reach a valid
"after" state** for chests (concrete repro below), the 1M claim/bench cover only
part of the path, and the concurrent-edit "no torn chunk" edge case is not
actually guaranteed. That is enough for CHANGES.

## Gates (on `df33351`)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ pass |
| `cargo-locked clippy --workspace --all-targets -j4 -- -D warnings` | ✅ pass |
| `cargo-locked test --workspace` | ✅ pass — `aether-server` 1406 passed, 0 failed; all suites 0 failed |
| `cargo-locked bench -p aether-server --bench bulk_edit` | ✅ runs on `df33351` after a clean |
| `bulk_edit/commit_fill_32x16x32` (16,384 blocks) | 4.27–4.46 ms (≈3.76 Melem/s) |
| `bulk_publish/step_default_tick_budget` | 5.57–5.76 ms (≈11.6 Melem/s) |

The same bench on the shared builder under load earlier gave 12.7 ms / 13.9 ms;
the docs' 14–16 ms is a conservative upper bound, not wrong. The df33351 rerun
is the number quoted below.

Note: the remote builder reused a stale `aether-plugin` rlib for a narrow
`cargo test -p aether-server` invocation (missing `after_block_place`) until
`cargo clean -p aether-plugin -p aether-server`; after cleaning, the full gates
above are clean. Not a branch defect, but worth knowing for narrow test runs.

## Answers to the two questions

### Light via `compute_column` as a temporary solution

**Acceptable, with documentation caveats; no new light regression.**

- The engine's only real light engine at this commit is per-column
  `aether_world::light::column::compute_column`, and the modern codecs (1.19+,
  `crates/aether-server/src/protocol/modern/chunk.rs:316-398`) already call it
  on every column encode. Re-sending a touched column therefore recomputes its
  whole sky/block light from the post-commit, post-settle blocks. Within each
  touched column the light is correct and consistent with what a joining client
  would receive.
- `compute_column` deliberately does **not** propagate light across column
  boundaries (`crates/aether-world/src/light/column.rs:6-13`), and resending
  only touched columns does not fix that. This is a pre-existing limitation of
  every column send, not a new hole opened by this branch: a source in column A
  never lit column B before either. It is correctly called out as the S4 gap in
  `docs/design/bulk-edit.md:143-158`.
- Two caveats on the claim: (a) the new test
  `a_published_edit_rebuilds_the_affected_columns_light`
  (`crates/aether-server/src/bulk.rs:1657-1680`) calls `compute_column` directly
  on the world, not through `PublishSink::resend_column`/the codec; it would
  still pass if resends did nothing. It is a primitive-level test, not
  end-to-end. (b) "Re-sending the column also re-runs the codec's light pass"
  (`docs/design/bulk-edit.md:130-132`) is only true for the `modern` codecs:
  1.17/1.18 (`protocol/v755/chunk.rs:209-233`, `v756`) still send the
  pre-existing full-bright light packet, and 1.8 uses FullBright
  (`docs/KNOWN_ISSUES.md:182`). No regression, but the doc should say "modern
  codecs".

### Does settle break the "before/after" order?

The ordering itself is right: commit applies all bulk writes atomically, the
settle corrections run afterwards, and no column is re-sent until the whole
job has been settled (`crates/aether-server/src/bulk.rs:828-833`; test
`bulk.rs:1715-1745`). A viewer of the edited columns sees the whole before or
the whole after, never a half-filled wall from this edit.

But the **"after" state itself can be wrong**, and a viewer can therefore get a
consistent-but-incorrect after: chest shapes have a shape→shape dependency
(`placement::chest_shape` reads a neighbour's `type`,
`crates/aether-server/src/placement.rs:1050-1073`), while the settle pass
computes the changed cell's own reshape before applying the neighbour
corrections from the same batch (`bulk.rs:776-825`). A neighbour correction can
invalidate the changed cell's just-computed shape, and nothing runs a second
pass. Concrete minimal repro (verified against `df33351`):

- a valid, lone pre-existing chest at `(1,20,2)`, facing south (`type=single`;
  it has no partner, so this is a settled state);
- bulk-place chests at `(0,20,2)` and `(2,20,2)`, both facing south;
- publish to completion;
- `(0,20,2)` ends `type=right`, but `placement::reshape` on the final world
  returns `type=single`: the middle chest was corrected to `right` (pairing
  with the east one) after the west cell's own reshape had already run, and
  nothing re-settled the west cell.

So the edit is not left "as if placed by a hand" until something else re-settles
the cells. The existing shape test only covers a fence pair
(`bulk.rs:1683-1713`); fences have no shape→shape dependency, so it misses this.
Fix by iterating the affected region to a fixed point (or ordering chest
corrections), and add a chest-chain test. This is finding 1.

## Edge-case checklist

| Brief edge case | Status | Note |
|---|---|---|
| 1M fill completes within budget, does not stall the tick | ⚠️ partial | Publication is budgeted; commit is synchronous and the test bypasses it (finding 2). |
| Edit across chunk borders resends each touched section exactly once | ⚠️ | Internally counted once; the wire collapses to one column packet, test checks columns only (finding 7). |
| Hook sees every changed block (documented choice) | ✅ | Per-block for online actors; offline/console documented as no hook. |
| Concurrent edits from two actors do not tear a chunk | ❌ | No encode-level guarantee; test checks final world state only (finding 3). |
| Edit larger than the budget is resumable | ✅ | Phases resume by cursor; 512-cell test. |
| Tick-budget bench | ⚠️ | Measures one settle slice, not a mixed settle→resend→hooks tick (finding 2). |
| Neighbour updates/light after commit | ⚠️ | After commit and before resend (good), but settle can be internally inconsistent (finding 1); light is the existing per-column model (see above). |

## Findings

### 1. HIGH — settle is not a fixed point for chest chains; the published "after" can be wrong

`crates/aether-server/src/bulk.rs:776-825`: for each changed cell the pass
computes the cell's own `reshape` first, then computes neighbour corrections and
applies the whole batch at the end (`apply_block_changes` at `bulk.rs:825`).
`crates/aether-server/src/placement.rs:1050-1073` makes a chest's `type` depend
on the neighbour chest's `type`, so a neighbour correction in the same batch can
invalidate the already-computed changed-cell shape, and changed neighbours are
skipped (`bulk.rs:803-805`). Repro above: a valid lone south-facing chest at
x=1, bulk-place chests at x=0 and x=2; x=0 ends `type=right` while `reshape`
says `single`. This is exactly the region whose "after" the resend publishes. Fix
with a fixed-point/second pass over the affected set (including changed cells)
or dependency ordering; add a chest test (`bulk.rs:1683` only tests fences).

### 2. HIGH/MEDIUM — the 1M "does not stall the tick" claim covers only publication; the test bypasses commit

`BulkHistory::commit_and_publish` (`bulk.rs:529-539`) synchronously commits the
whole edit (`commit_with_changes`, `bulk.rs:375-403`: journal append plus
`apply_block_changes` for all changes) and only then queues publication. Plugin
commands run on the tick thread (`docs/design/plugins.md:98`), so a 1M fill
still stalls that tick for the write half. The headline test builds the change
list and calls `world.apply_block_changes(&changes)` directly
(`bulk.rs:1829-1855`) instead of `commit_with_changes`, so it never measures the
commit. At the measured ~4.4 ms/16,384 blocks (up to ~12.7 ms under load), a 1M
commit is ~0.27–0.8 s in release before journal encoding — 5–16 ticks, not a
bounded tick. `docs/STATUS.md:154` and
`docs/design/bulk-edit.md:150-152` say the 1M fill "completes within budget" /
"never takes an unbounded tick"; scope that to publication or make the write
half resumable, and make the test use the real commit path.

### 3. MEDIUM — concurrent commits can still tear a column during encode; settle corrections race commits

`ColumnView` copies a column one section at a time, each taking the world cache
read lock separately (`crates/aether-server/src/protocol/mod.rs:107-118`), while
a commit applies all its sections under one cache write lock
(`crates/aether-api/src/lib.rs:660-668`). A commit landing between two section
reads yields a column whose lower section is pre-commit and upper section
post-commit — a torn column. Settle corrections run outside `COMMIT_LOCK`
(`bulk.rs:825`), so a concurrent commit between the `was`/`to` read and the
correction apply can also be overwritten by a stale correction. The
"concurrency" test edits a single section and uses a `RecordingSink`
(`bulk.rs:1878-1943`), so it never encodes and cannot catch either. Snapshot the
whole column under one read lock (or retry on revision change), and serialize
settle/commit or version corrections.

### 4. MEDIUM — derived settle corrections are outside the journal, and undo does not publish or reverse them

`BulkHistory::undo` (`bulk.rs:547-608`) commits the inverse but never calls
`enqueue_publish`, so viewers keep the post-edit column after a bulk undo until
something else re-sends it. Settle corrections are applied as a batch without a
revision (`bulk.rs:821-825`, design note at `docs/design/bulk-edit.md:121-124`),
while journal column replay applies only `c.to`
(`crates/aether-api/src/lib.rs:403-414`). A crash/rebuild between the edit and
the snapshot flush therefore reloads chest/fence/stair shapes without the
settle result, and an undo restores only the recorded `changes`, not the shape
corrections. Either journalled corrections (or re-settle on replay/undo) are
needed, and undo should go through the publisher.

### 5. MEDIUM — plugin hooks run under the process-wide publisher mutex

`publish_tick` holds `publisher()` for the whole `queue.step`
(`bulk.rs:1035-1042`); the hook phase calls `ServerSink` →
`crate::plugins::after_block_break/place` (`bulk.rs:999-1028`,
`crates/aether-server/src/plugins.rs:1284-1292`) synchronously. If a native
after-block hook commits/enqueues a bulk edit, `enqueue_publish`
(`bulk.rs:974-979`) locks the same non-reentrant mutex on the same tick thread:
self-deadlock. Even without reentrancy, every enqueue stalls for the duration of
broadcast/encode. Pop a job under the lock, drop the guard, then process.

### 6. LOW — `after-block-place` is declared as a general event but only bulk edits fire it

`crates/aether-plugin/src/manifest.rs:42-43` and
`crates/aether-plugin/src/native.rs:175-186` present `after-block-place` as the
mirror of `after-block-break`, but the normal placement path never calls it
(`crates/aether-server/src/session.rs:1579-1583`; only the break path calls
`after_block_break` at `session.rs:1182`). A native plugin subscribing to the
event sees bulk edits only; `docs/design/plugins.md:101-102` was not updated.
Call it from normal placement or rename/document it as bulk-only.

### 7. LOW — the brief says one packet per changed section; the wire sends one per column

The plan is per-section but collapsed into one column packet
(`bulk.rs:838-848`, `880-886`), documented as intentional
(`docs/design/bulk-edit.md:125-130`). That is reasonable for
`ServerEvent::ChunkColumn`, but the test checks `RecordingSink.columns`, not a
section packet (`bulk.rs:1562-1616`; `total.sections` is an internal counter).
Update the brief to "per changed column" or add a section-capable event, and
make the test assert the actual wire unit.

### 8. LOW — module docs still say commits are not serialised

`bulk.rs:39-43` says "The commit is not, however, serialised against other
writers ... CMD-30b owns per-region serialisation", contradicted by
`COMMIT_LOCK`/`commit_guard` (`bulk.rs:133-145`) and their use in
`commit_with_changes`/`undo`. Update the module header.

## Smaller notes

- `commit_and_publish` clones the whole change list (`bulk.rs:536`); moving it
  avoids a ~20 MB peak copy for a 1M edit.
- A zero field in a directly constructed `PublishBudget` stops `step` forever
  (`bulk.rs:940-946`) even though `job.step` uses `max(1)` (`773`, `843`,
  `860`); config clamps to 1, but the public struct is unvalidated.
- The "61 settle steps" arithmetic in `docs/design/bulk-edit.md:150` should be
  62 (61×16,384 + 576); same for hooks.
- New tests import `aether_world::light::column` directly; good primitive
  coverage, but they do not exercise `ServerSink`, actor→player hook lookup, or
  the actual column encoder.

## What is good

- Explicit Settle→Resend→Hooks phases with per-job cursors and a shared
  remaining budget; resends wait for the whole settle, so the branch's own edit
  is never published half-filled.
- Sections are deduped in a `BTreeSet`, columns in a `BTreeMap`; a column with
  many changed sections goes out once.
- Hooks are per changed block, use the post-reshape state, and are documented
  native-only with a host test.
- `[bulk]` config, example TOML, docs and the light/shape regression tests are
  present; all gates pass.
