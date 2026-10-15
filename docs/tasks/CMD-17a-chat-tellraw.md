# CMD-17a — V-data: say, me, msg, tellraw

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-17-messaging.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/say <message>` | `minecraft.command.say` | 2 | `[Server] <message>` broadcast |
| `/me <action>` | `minecraft.command.me` | 0 | `* <player> <action>` broadcast |
| `/msg <targets> <message>` (aliases `/tell`, `/w`) | `minecraft.command.msg` | 0 | `You whisper to <player>: <message>`; recipient sees it too |
| `/tellraw <targets> <message>` | `minecraft.command.tellraw` | 2 | (renders the component; no feedback) |

**These names are core and permanent**; plugins must not re-register them (see CMD-40). `/msg`
records the last whisper counterpart per player so a plugin's `/r` can reply (hook, CMD-40).

Chat components: 1.20.3+ NBT, 1.20.2 JSON (KNOWN_ISSUES A.10). Player names are inserted as
literals, never re-parsed as components.

## Files
- new `crates/aether-server/src/commands/text.rs`; `session.rs:776` broadcast path; the
  text-component writer in `protocol/`.

## Edge cases to test explicitly
- `/msg` to a non-player (mob) errors; to yourself works (vanilla allows).
- `/tellraw @a <component>` with hover/click/translate/score components on each codec.
- `/me` and `/say` never let a player inject a component through their name.
- console `say` uses `[Server]`; unknown target → `No player was found`.

## Done criteria
- Implementation + tests across 1.21.11 and one older codec (NBT vs JSON component).
- fmt / clippy `-D warnings` / `cargo-locked test`.
