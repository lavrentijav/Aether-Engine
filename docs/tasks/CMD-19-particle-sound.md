# CMD-19 — V-data: particles and sounds

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Vanilla client-effects commands: spawn particles, play/stop sounds.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/particle <name> [<pos>] [<delta>] [<speed>] [<count>] [force\|normal] [<viewers>]` | `minecraft.command.particle` | 2 | `Created <count> particle(s) of <name>` (no feedback line in vanilla; keep the operation silent) |
| `/playsound <sound> [<source>] [<targets>] [<pos>] [<volume>] [<pitch>] [<minVolume>]` | `minecraft.command.playsound` | 2 | `Played sound <sound> to <targets>` |
| `/stopsound <targets> [<source>] [<sound>]` | `minecraft.command.stopsound` | 2 | `Stopped sound <sound> for <targets>` |

Resources come from the 1.21.11 registries (particle types, `sound_event`s); an unknown id errors
naming it. `/particle` `count=0` means "as many as the client can render"; `force` is 1.21.9+.
Sound source categories: `master`, `music`, `record`, `weather`, `block`, `hostile`, `neutral`,
`player`, `ambient`, `voice`, `ui`.

## Files
- new `crates/aether-server/src/commands/effects.rs`.
- `protocol/` particle + sound packets per codec (some particle/`playsound` packet shapes differ
  across releases; use the release's ids).

## Edge cases to test explicitly
- particle with a `count` and with the `force` flag on 1.21.11; older codecs ignore unsupported
  flags.
- delta defaults (0 0 0) and a spread; `speed` only meaningful with a spread > 0.
- targets for `/particle` default to everyone in range; the optional `<viewers>` selector narrows.
- an invalid sound/particle resource errors; a valid namespaced id without the `minecraft:` prefix
  resolves.
- `/stopsound` with only a target stops all; with a source; with a sound.
- `/playsound` `minVolume` below `volume` (vanilla swaps them).
- volume 0 is silent but reports success.

## Done criteria
- Implementations + tests; a loopback smoke test that a particle/sound packet is emitted.
- fmt / clippy `-D warnings` / `cargo-locked test`.
