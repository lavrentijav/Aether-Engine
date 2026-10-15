# CMD-46 — Plugin: fun / cosmetic

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/fun`
Needs: `oc/P1`, `oc/CMD-32`

## Goal
The small crowd-pleasers: firework, lightning/smite, tree planting, books, powertools, particles and
pets. Cosmetic or convenience, not hot-path — a plugin.

## Commands
| command | syntax | node |
|---|---|---|
| firework | `/firework [<color>] [<shape>] [<trail>] [<flicker>]` (spawn at the player) | `fun.firework` |
| lightning | `/lightning [<player>]` (`/smite`) | `fun.lightning` |
| tree | `/tree <type>`, `/bigtree <type>` (plant at the look position) | `fun.tree` |
| book | `/book title <title>`, `/book author <author>`, `/book give` | `fun.book` |
| powertool | `/powertool <command>` (bind the held item to a command), `/powertool` (clear) | `fun.powertool` |
| particles | `/particles [<effect>] [<player>]` (toggle a cosmetic trail) | `fun.particles` |
| pet | `/pet [<type>]`, `/pet remove` (spawn/remove a cosmetic follower) | `fun.pet` |

- `/lightning` spawns a real lightning bolt (damage/fire per vanilla), not just an effect.
- `/tree` places the worldgen tree feature (reuse the decoration code) at a safe spot.
- `/book` builds a writable book item with the given title/author/text input.
- `/powertool` runs the bound command as the player (permission-checked) when they left-click.
- pets are cosmetic entities that follow; no combat AI.

## Files
- `plugins/fun/`: manifest, `src/`, README.
- player/entity API (CMD-32), worldgen tree features, text input for the book, item/server events.

## Edge cases to test explicitly
- `/tree` on a position with no room / in a protected region is refused.
- `/lightning` near a player can kill/hurt them (real damage), and respects `/god`/effects.
- `/book` with a long title/text; an invalid colour code is sanitised.
- `/powertool` cannot bind the command that grants powertool (no loop); a permission it lacks is
  denied at use time.
- `/pet` a type the version lacks (compat stand-in); removing an already-removed pet no-ops.
- `/firework` on an older codec spawns a firework rocket entity with the right components.
- `/particles` toggle persists for the session; disabled by a permission.

## Done criteria
- Plugin + tests; README lists commands and links CMD-46.
- fmt / clippy `-D warnings` / `cargo-locked test`.
