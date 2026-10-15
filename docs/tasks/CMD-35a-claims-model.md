# CMD-35a — Plugin: claims model & shovel

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/claims`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-29b`
Parent/umbrella: `docs/tasks/CMD-35-claims-plugin.md`

## Commands (GriefPrevention reference)
| command | syntax | node |
|---|---|---|
| claim | `/claim` (create/inspect), `/claim <radius>` | `griefprevention.claim` |
| abandon | `/abandonclaim`, `/abandonallclaims` (confirm) | `griefprevention.abandonclaim` / `.abandonallclaims` |
| list | `/claimslist` | `griefprevention.claimslist` |
| shovel | right-click ground with a golden shovel to claim, a claim to inspect | — |

## Files
- `plugins/claims/`: manifest, `src/`, README.
- region index/check (CMD-29a/29b); item-use hook (`before-use-item`, golden shovel) from P1; player
  KV for claim ownership.

## Edge cases to test explicitly
- claiming a chunk already claimed by another player is refused.
- a claim overlapping a WorldGuard region: precedence by region priority (decide and test).
- abandoning frees it; `/abandonallclaims` requires confirmation.
- a claimed chunk near the world border; a claim across chunk borders.
- a golden shovel on a mob/entity does not claim; claims persist across restart.

## Done criteria
- Plugin + persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
