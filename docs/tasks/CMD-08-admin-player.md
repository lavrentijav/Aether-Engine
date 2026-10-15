# CMD-08 — V-admin: player & access management

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-06`

## Goal
Implement the vanilla commands that manage operators, bans and the whitelist. All target lists use
the selector/player parser from CMD-06 and the permission check from CMD-01.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/op <targets>` | `minecraft.command.op` | 3 | `Made <player> a server operator` |
| `/deop <targets>` | `minecraft.command.deop` | 3 | `Made <player> no longer a server operator` |
| `/ban <targets> [<reason>]` | `minecraft.command.ban` | 3 | `Banned <player>: <reason>` / `Banned <player>` |
| `/ban-ip <targets> [<reason>]` | `minecraft.command.ban-ip` | 3 | `Banned IP address <ip>` / `Banned IP address <ip>: <reason>` |
| `/banlist [ips|players]` | `minecraft.command.banlist` | 3 | `There are no bans` / `There are <n> bans:` + lines |
| `/pardon <targets>` | `minecraft.command.pardon` | 3 | `Unbanned <player>` |
| `/pardon-ip <targets>` | `minecraft.command.pardon-ip` | 3 | `Unbanned IP address <ip>` |
| `/kick <targets> [<reason>]` | `minecraft.command.kick` | 3 | `Kicked <player>: <reason>` |
| `/whitelist <on|off|list|add|remove|reload>` | `minecraft.command.whitelist` | 3 | `Whitelist is now turned on` / `Added <player> to the whitelist` / `There are <n> whitelisted players:` / `Reloaded the whitelist` |

Targets that don't resolve: vanilla reports `No player was found` (for a name selector) — do not
silently skip. `/op`/`/deop` also require the *server* to be in online/offline mode consistently:
the operator list is `config.operators` (name or bound address); an IP-bound entry (`name@ip`)
must stay bound and not be widened by `/op` (vanilla `/op` adds the name only — record the
decision in the brief's notes).

## Files
- `crates/aether-server/src/commands/admin.rs` (new) or extend `game/commands.rs` for the
  selector plumbing.
- ban/whitelist persistence: reuse the config/player store; if none exists, add a small
  `banned.json` / `whitelist.json` in the world dir, with the key prefix registered (T5).
- `crates/aether-server/src/session.rs`: enforce the ban/whitelist at login (`session.rs:193`
  already has the max-players + operator admission; ban/whitelist gates go beside it).

## Edge cases to test explicitly
- banning an online player kicks them; banning an offline name persists and refuses the next login
  with the reason (`You are banned from this server! <reason>`).
- `/pardon` on a name that is not banned → `Nothing changed` style error, not a success.
- IP ban applies to a name entry that later reconnects from another address; name ban applies
  regardless of address.
- `/banlist` with a large list pages/formats exactly like vanilla; `/banlist ips` filters.
- whitelist on with an empty list refuses everyone except operators (vanilla: ops bypass).
- adding an offline player to the whitelist stores name+uuid; a later join matches by uuid.
- removing a group target kicks all of them.
- permission: level-2 cannot run these; a level-3 can; server console is level 4.

## Done criteria
- Command implementations + persistence + login enforcement, with the login-path tests above.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
