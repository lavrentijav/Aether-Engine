# CMD-35 — Plugin: GriefPrevention-style claims

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/claims`
Needs: `oc/P1`, `oc/CMD-29`

## Goal
A simpler claim system for players than WorldGuard regions: claim land with a golden shovel, trust
other players, abandon claims. Built on the same core region index/check (CMD-29) but owned by
players rather than operators.

## Commands (GriefPrevention reference)
| command | syntax | node |
|---|---|---|
| claim | `/claim` (create/inspect a claim) | `griefprevention.claim` |
| claim | `/claim <radius>` (square claim) | `griefprevention.claim` |
| abandon | `/abandonclaim` | `griefprevention.abandonclaim` |
| abandon all | `/abandonallclaims` (confirm) | `griefprevention.abandonallclaims` |
| trust | `/trust <player>` / `/untrust <player>` (build/container/access levels) | `griefprevention.trust` |
| list | `/claimslist` | `griefprevention.claimslist` |
| claiming mode | `/claimmode` / `/basicclaims` (optional) | `griefprevention.mode` |
| shovel | right-click ground with a golden shovel to claim; right-click a claim to inspect | — |

Claim limits: a player's max claim area/accrued blocks (config; default EssX-like golden shovel
rules). Claims are stored per player; the protection check consults the region index. Trust levels:
`access` (enter), `container`, `build` + `manage`.

## Files
- `plugins/claims/`: manifest, `src/`, README.
- region index + check (CMD-29); item-use hook (`before-use-item`, golden shovel) from P1; player
  meta/KV for claim ownership.

## Edge cases to test explicitly
- claiming a chunk already claimed by another player is refused.
- a claim overlapping a WorldGuard region: decide precedence (region priority) and test.
- trust levels: a container-trusted player opens chests but cannot break; untrust removes.
- abandoning a claim frees it; `/abandonallclaims` requires confirmation.
- claim area over the limit sells/buys nothing and is refused with the shortfall.
- a claimed chunk near the world border; a claim across chunk borders.
- a golden shovel on a mob/entity does not claim.
- claims persist across restart; `/claimslist` names coordinates.

## Done criteria
- Plugin + persistence + tests; README lists commands and links CMD-35.
- fmt / clippy `-D warnings` / `cargo-locked test`.
