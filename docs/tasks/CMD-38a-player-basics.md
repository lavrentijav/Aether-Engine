# CMD-38a — Plugin: essentials-player — movement & state

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/essentials-player`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-32a`
Parent/umbrella: `docs/tasks/CMD-38-essentials-player.md`

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| flight | `/fly [<player>]` | `essentials.fly` / `.others` |
| god | `/god [<player>]` | `essentials.god` |
| speed | `/speed [walk\|fly] <value> [<player>]` | `essentials.speed` |
| food | `/feed [<player>]` | `essentials.feed` |
| heal | `/heal [<player>]` | `essentials.heal` |
| repair | `/repair [all\|hand\|armor]` | `essentials.repair` |
| hat | `/hat` | `essentials.hat` |

Uses the player-state API (CMD-32a) for effects/flight/speed. Toggle state persists per session and
is restored on rejoin where EssX does.

## Files
- `plugins/essentials-player/`: manifest, `src/`, README.
- player API (CMD-32a); note `/heal` also exists as an Aether core command — the plugin must hook it
  or use a distinct name, never re-register a core name (see the overview's ownership rule).

## Edge cases to test explicitly
- `/fly` in a region where flight is denied — decide and test.
- `/god` + the void; document whether void damage still applies.
- `/speed` bounds: walk 0..1, fly 1..10; out of range clamps/errors like EssX.
- `/hat` with an empty hand; `/repair` cost if configured; permission self vs others.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
