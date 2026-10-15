# CMD-41 — Plugin: moderation

Status: 📋 task brief (CMD0) · size: L → split · core: no · plugin: `plugins/moderation`
Needs: `oc/P1`, `oc/CMD-32`, `oc/CMD-40`

## Split (CMD0 review)
| id | brief | contents | size |
|---|---|---|---|
| CMD-41a | `docs/tasks/CMD-41a-moderation-chat.md` | `/mute` `/tempmute` `/unmute` `/warn` `/warnings` `/delwarn` `/tempban` `/tempbanip` `/banip` `/clearchat` `/slowmode` `/staffchat` | M |
| CMD-41b | `docs/tasks/CMD-41b-moderation-staff.md` | `/jail` `/unjail` `/setjail` `/vanish` `/sudo` `/freeze` `/report` `/reports` `/history` | M |

## Goal
The staff toolkit: mutes, warnings, temporary bans, jails, vanish, freeze, sudo, chat control and
reports. Bans/kicks stay core (CMD-08); the **temporary / IP-range** variants and everything else
are here (`docs/design/commands.md` §4).

**Split before assigning**: suggested split — (a) mute/warn/kick-variants + chat control, (b)
jail/vanish/freeze/sudo/report/history. This brief is the combined spec.

## Commands (EssentialsX / common moderation plugins)
| command | syntax | node |
|---|---|---|
| mute | `/mute <player> [<reason>]`, `/tempmute <player> <time> [<reason>]`, `/unmute <player>` | `moderation.mute` |
| warn | `/warn <player> <reason>`, `/warnings <player>`, `/delwarn <player> <id>` | `moderation.warn` |
| temban | `/tempban <player> <time> [<reason>]`, `/tempbanip`, `/banip` (range) | `moderation.tempban` |
| jail | `/jail <player> [<area>]`, `/unjail <player>`, `/setjail <name>` | `moderation.jail` |
| vanish | `/vanish` (`/v`) | `moderation.vanish` |
| sudo | `/sudo <player> <command>` | `moderation.sudo` |
| freeze | `/freeze <player> [<reason>]` | `moderation.freeze` |
| chat | `/clearchat [<lines>]`, `/slowmode <seconds>`, `/staffchat <message>` (`/sc`) | `moderation.clearchat`, … |
| reports | `/report <player> <reason>`, `/reports`, `/report <id> <action>` | `moderation.report` |
| history | `/history <player>` (bans/kicks/mutes/warns on record) | `moderation.history` |

All temporary actions expire (a scheduled task from P1); records persist across restart. `/vanish`
hides the player from the tab list and entity tracking for non-staff and blocks mob targeting.
`/freeze` prevents movement (and cancels teleports/commands depending on config). `/sudo` runs a
command as the target — it must go through the same permission checks as the target, never as
console.

## Files
- `plugins/moderation/`: manifest, `src/`, README.
- player API (CMD-32) for vanish/freeze/flight; P1 scheduler for expiry; chat hook (CMD-40) for
  mute/slowmode/clearchat; core ban list (CMD-08) for `/tempban` persistence of a finite ban.

## Edge cases to test explicitly
- a muted player cannot chat or `/msg` but can run commands; unmute/temp expiry restores.
- `/tempmute 5m` expires while the player is offline (expiry still recorded).
- `/tempban` sets an expiry; after it, login is allowed; an operator can lift it early.
- `/jail` a player in another world; `/unjail` with a full inventory.
- `/vanish`: a staff member in vanish is hidden from non-staff, visible to staff; mobs ignore.
- `/freeze` prevents movement but not chat; `/freeze` again unfreezes.
- `/sudo` a command the target cannot run is denied as the target; no console escalation.
- `/clearchat` with a line count; `/slowmode` shared across worlds; `/staffchat` staff only.
- `/report` persists, `/reports` lists, a handled report is marked; abuse/rate-limit.
- `/history` merges core bans (CMD-08) and plugin records.

## Done criteria
- Plugin + persistence + expiry + tests; README lists commands and links CMD-41.
- fmt / clippy `-D warnings` / `cargo-locked test`.
