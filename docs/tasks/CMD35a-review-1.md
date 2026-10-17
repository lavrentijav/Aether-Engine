# CMD35a review 1 — claims model & golden shovel

Reviewer: independent critic. Branch `oc/CMD35a` at `0a2b84a`; base `main`
`c54281e` (same as merge-base, branch is 0 commits behind). Scope:
`git diff main...HEAD` — 19 files, +1979/-28.

## Verdict: CHANGES

The claim model, commands, shovel flow, KV persistence, limits and the core
region/check wiring are broadly in place, and the gates pass. The blocking
problems are in the new host region API: region ids are global across plugins
but claims allocate them from a private `1..` counter, and a refused upsert is
silent. That makes claims and the planned/`regions` plugin unable to coexist
safely, and can report a successful claim that protects nothing.

Gates run:

* `cargo fmt --all -- --check` — clean.
* `cargo clippy --workspace --all-targets -j4 -- -D warnings` — clean **after**
  `cargo clean -p aether-plugin -p aether-claims-plugin -p aether-server`.
  The first run compiled against a stale shared-target `aether-plugin` rlib
  (no `RegionInfo` / `before_use_block`); this was an artefact issue, not a
  source issue.
* `cargo-locked test --workspace` — all suites ok, **0 failed** (claims e2e
  16/16; `aether-server` 1431/1431; no FAILED/panicked lines).

No code was edited or committed.

## Findings

1. **HIGH — Claim ids collide with other region plugins, and the failure is
   silent.** `plugins/claims/src/lib.rs:214,224,262-269,475-496`.
   Claim ids are a plugin-private `1..` counter, but the core store keys regions
   by the bare `u64` (`crates/aether-server/src/plugins.rs:743`, `region.rs`
   `RegionId`). `region_upsert` refuses an id already owned by another plugin
   but returns `()` and only logs (`crates/aether-server/src/plugins.rs:741-768`);
   both `create_claim` (`lib.rs:488-502`) and startup replay (`lib.rs:272-275`)
   ignore that. The claim is persisted and reported as "Claim #N created", but
   no protection region is installed. The same applies at restart. A `regions`
   plugin using the obvious ids `1,2,…` is enough to hit this. Fix: allocate
   collision-free ids (probe `region_get`, use a plugin-namespaced range, or let
   the host allocate), make `region_upsert` return a `Result`, and fail the
   command/replay loudly instead of persisting an ineffective claim.

2. **HIGH — `region_upsert` can overwrite a core-owned region and its ownership
   check is check-then-act.** `crates/aether-server/src/plugins.rs:744-768`.
   The guard only refuses when `existing.source` is `Some(other)`. A region
   created directly in the core has `source: None`, so a plugin (e.g. claims id
   1) replaces it, contradicting the documented contract at
   `crates/aether-plugin/src/host.rs:425-433` ("a plugin may only replace or
   remove a region it owns"). In addition, the `snapshot().get` check happens
   outside `RegionStore`'s builder lock, so two concurrent writers of a free id
   can both pass and the last one wins. Fix: perform the ownership test inside
   the store's serialized edit (`upsert_owned(plugin, ...) -> Result`), refusing
   any existing region whose source is not `Some(plugin)`. Add host-level tests
   for both cases; today only the value round-trip is tested
   (`plugins.rs:1562-1585`).

3. **MEDIUM — The overlap query defeats the CMD-29a index.** 
   `crates/aether-server/src/plugins.rs:791-812`. `regions_overlapping` takes an
   `Arc<RegionIndex>` but then iterates `index.regions()`, i.e. scans every
   region in the process for every `/claim`, shovel second-click and replay.
   That is O(total regions), not the per-chunk index the brief asks the plugin
   to use. It is correct, but it will not scale and it means the hot new API
   does not actually exercise the index. Use `overlapping_chunk` over the
   covered chunk range (deduplicating by id) or add a box query to
   `RegionIndex`; add a test with regions far outside the queried box.

4. **MEDIUM — No concurrency test for two players claiming the same land.**
   `plugins/claims/tests/e2e.rs` covers sequential Bob-after-Alice only. The
   plugin's check (`lib.rs:465-473`) and insert (`lib.rs:494-496`) are not
   atomic; today `PluginHost` serializes calls, but that dependency is implicit
   and the host region API allows other callers/threads. Add a regression test
   (or an explicit atomic host `claim_if_free` operation) that proves exactly
   one of two competing claim attempts wins.

5. **LOW — Golden-shovel selections and `/abandonallclaims` confirmations are
   never cleared on logout.** `plugins/claims/src/lib.rs:216-217,343,562-567`.
   Both maps are keyed by the session entity id and the plugin does not
   subscribe to `after-player-leave` (`lib.rs:88-93`), although the native trait
   exposes it (`crates/aether-plugin/src/native.rs:181-182`). Entries leak for
   the process lifetime; if entity ids are ever reused (the counter is
   monotonic today), a new player can inherit a pending corner or an armed
   confirmation. Subscribe to `after-player-leave` and remove both entries, or
   key by a stable player identity.

6. **LOW — Config and arithmetic edges are unchecked.** 
   `plugins/claims/src/lib.rs:234-244,442-450,688-697`. Malformed/negative
   values silently fall back (`max-claims = "-1"` becomes 10,
   `max-claim-size = "-1"` makes every claim fail at runtime and
   `max-radius = "-1"` is clamped to 0), and the area product can overflow
   `i64` if an operator raises `max-radius` near `i32::MAX` (debug panic;
   release wrap could accept an enormous claim). Reject invalid config at
   `init` with a clear error and use `checked_mul`/cap the radius by
   `max-claim-size`.

## What passed

* `/claim` create/inspect, chunk claim, `<radius>` square, negative
  coordinates and chunk-border spanning are covered and correct.
* Foreign-claim overlap is refused; WorldGuard-style overlap is allowed and
  priority is written (`lib.rs:177-189,465-473`; e2e test).
* `/abandonclaim` ownership check, `/abandonallclaims` two-step 30 s
  confirmation, and per-player claim/size limits are covered.
* `/claimslist` only lists the caller's claims unless the operator or an
  explicit `.others`/`admin` node is present.
* KV persistence and startup replay work in the e2e restart test; the core
  store's snapshot swap gives the documented next-safe-point removal semantics.
* Golden shovel: two clicks, same-corner reset, existing-claim inspect, other
  items and air use are covered; the item id matches
  `minecraft-data` (`minecraft:golden_shovel`).
* The new `before-use-block` host event is native-only, dispatched with the
  subscription gate, and wired before vanilla item-on-block handling.
