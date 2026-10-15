# CMD-35b — Plugin: claims trust & limits

Status: 📋 task brief (CMD0) · size: S · core: no · plugin: `plugins/claims`
Needs: `oc/P1`, `oc/CMD-29b`, `oc/CMD-35a`
Parent/umbrella: `docs/tasks/CMD-35-claims-plugin.md`

## Commands (GriefPrevention reference)
| command | syntax | node |
|---|---|---|
| trust | `/trust <player>` (build), `/containerstrust`, `/accesstrust`, `/managetrust` | `griefprevention.trust` |
| untrust | `/untrust <player>` / `/untrust all` | `griefprevention.trust` |

Trust levels: `access` (enter), `container`, `build`, `manage`. Claim area limits: a player's max
claim area/accrued blocks from config (default EssX-like golden-shovel rules).

## Files
- `plugins/claims/`: manifest, `src/`, README.
- claim model (CMD-35a), region check (CMD-29b), player meta/KV.

## Edge cases to test explicitly
- a container-trusted player opens chests but cannot break; untrust removes all levels.
- claim area over the limit is refused with the shortfall; a smaller claim still succeeds.
- trust persists across restart; `/untrust all` clears every trustee.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
