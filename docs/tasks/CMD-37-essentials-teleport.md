# CMD-37 — Plugin: essentials-teleport (umbrella)

Status: 📋 umbrella brief (CMD0). Split into **CMD-37a** and **CMD-37b**. tier: native plugin (N),
plugin: `plugins/essentials-teleport`. Needs: `oc/P1`, `oc/CMD-31`.

## Goal
The EssentialsX teleport/home/warp family, on the core teleport/safe-spot API (CMD-31) and player KV
(P1).

## Ownership rule — vanilla names stay core
`/spawn` is a **core** command (CMD-11). The plugin does **not** re-register or override it; it adds
only new names (`/sethome`, `/home`, `/warp`, `/tpa`, `/back`, …). If a server wants the plugin's
spawn behaviour, the core `/spawn` should read a spawn-provider service the plugin sets, rather than
the plugin answering the same name (this is an API question — see the overview's open questions).

## Split
| id | brief | contents | size |
|---|---|---|---|
| CMD-37a | `docs/tasks/CMD-37a-teleport-homes-warps.md` | `/sethome` `/home` `/delhome` `/homes`, warps, `/setspawn` | M |
| CMD-37b | `docs/tasks/CMD-37b-teleport-requests.md` | `/tpa` `/tpahere` `/tpaccept` `/tpdeny` `/tpcancel`, `/tpall` `/tphere` `/tppos`, `/back` `/top` `/jump` `/up` `/rtp` | M |

## Shared notes
Cooldowns and warmups (EssentialsX config) are plugin config. Home/warp/back state is
per-player/per-server KV. Teleport delays move the player only if they have not moved/taken damage
(EssX semantics). `back` is set on death, on a warmup teleport and on a `/tpa`.

## Files
- `plugins/essentials-teleport/`: manifest, `src/`, README.
- core teleport + safe spot (CMD-31); KV (P1); death hook (`after-entity-death`) for `/back`;
  move/damage hooks for warmup cancellation.

## Done criteria
- Per the sub-briefs; fmt / clippy `-D warnings` / `cargo-locked test`.
