# CMD-41a — Plugin: moderation — mutes, warnings, temp bans, chat control

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/moderation`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-32a`, `oc/CMD-40`
Parent/umbrella: `docs/tasks/CMD-41-moderation.md`

## Commands
| command | syntax | node |
|---|---|---|
| mute | `/mute <player> [<reason>]`, `/tempmute <player> <time> [<reason>]`, `/unmute <player>` | `moderation.mute` |
| warn | `/warn <player> <reason>`, `/warnings <player>`, `/delwarn <player> <id>` | `moderation.warn` |
| temban | `/tempban <player> <time> [<reason>]`, `/tempbanip`, `/banip` (range) | `moderation.tempban` |
| chat | `/clearchat [<lines>]`, `/slowmode <seconds>`, `/staffchat <message>` (`/sc`) | `moderation.clearchat`, … |

Temporary actions expire via the P1 scheduler and persist across restart; core bans/kicks stay in
CMD-08.

## Files
- `plugins/moderation/`: manifest, `src/`, README.
- player API (CMD-32a); chat hook (CMD-40) for mute/slowmode/clearchat; core ban list (CMD-08) for
  `/tempban` persistence of a finite ban.

## Edge cases to test explicitly
- a muted player cannot chat or `/msg` but can run commands; unmute/temp expiry restores.
- `/tempmute 5m` expires while the player is offline (expiry still recorded).
- `/tempban` sets an expiry; login allowed after it; an operator can lift it early.
- `/clearchat` with a line count; `/slowmode` shared across worlds; `/staffchat` staff only.
- hook order with CMD-40 documented.

## Done criteria
- Plugin + persistence + expiry + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
