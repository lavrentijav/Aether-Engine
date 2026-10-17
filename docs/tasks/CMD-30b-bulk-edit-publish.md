# CMD-30b — Native API: bulk edit publish (resend, light, budget)

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N)
Needs: `oc/CMD-00N`, `oc/CMD-01`, `oc/P1`, `oc/CMD-30a`
Parent/umbrella: `docs/tasks/CMD-30-bulk-edit.md`

## Goal
Publishing a committed bulk edit: batched chunk resends, neighbour updates and lighting over the
affected region, and a per-tick budget so a million-block fill never stalls the tick.

## Behaviour
- Chunk resends are batched **per chunk** (one packet per changed section; on this wire that is
  one packet per touched **column**, since the protocol has no section packet), not per block, and
  published only after all writes are applied — viewers see the before or the after, never a
  half-filled wall.
- Neighbour updates and lighting run once per affected region after commit, reusing the existing
  placement/light passes (no per-block full recompute).
- A large edit is spread over ticks within a per-tick budget (configurable); measure and document
  the budget.
- Plugin `after-block-break/place` hooks fire per changed block (or batched — decide and document).

## Files
- `crates/aether-server/src/bulk.rs`; chunk-resend batching in `session.rs`/`outbox.rs`;
  placement neighbour updates; light update (S4 if merged).

## Edge cases to test explicitly
- a 1M-block fill completes within budget and does not stall the tick.
- editing across chunk borders resends each touched section exactly once.
- a plugin hook sees every changed block (per the documented choice).
- concurrent edits from two actors do not produce a torn chunk.
- an edit larger than the budget is resumable and eventually completes.

## Done criteria
- Implementation + tests + a tick-budget bench; fmt / clippy `-D warnings` / `cargo-locked test`.
