# CMD-16 — V-data: tags, teams, team message

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Entity tags and scoreboard teams (with all their effect rules), plus `/teammsg`. Teams are read by
the chat/name-tag display, so implement the model once.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/tag <targets> add <name>` | `minecraft.command.tag` | 2 | `Added tag '<name>' to <n> entities` |
| `/tag <targets> remove <name>` | `minecraft.command.tag` | 2 | `Removed tag '<name>' from <n> entities` |
| `/tag <targets> list` | `minecraft.command.tag` | 2 | `<entity> has <n> tags: ...` / `... has no tags` |
| `/team add <name> [<displayName>]` | `minecraft.command.team` | 2 | `Team '<name>' has been created` |
| `/team remove <name>` | `minecraft.command.team` | 2 | `Team '<name>' has been removed` |
| `/team empty <name>` | `minecraft.command.team` | 2 | `Removed all members from team '<name>'` |
| `/team join <name> [<members>]` | `minecraft.command.team` | 2 | `Added <n> entities to team '<name>'` |
| `/team leave <members>` | `minecraft.command.team` | 2 | `Removed <n> entities from their team` |
| `/team list [<name>]` | `minecraft.command.team` | 2 | `There are <n> teams: ...` / `Team '<name>' has <n> members` |
| `/team modify <name> displayName\|color\|friendlyFire\|seeFriendlyInvisibles\|nametagVisibility\|deathMessageVisibility\|collisionRule\|prefix\|suffix ...` | `minecraft.command.team` | 2 | per option |
| `/teammsg <message>` (alias `/tm`) | `minecraft.command.teammsg` | 0 | `[<team>] <player>: <message>` to team only |

## Files
- new `crates/aether-server/src/commands/team_tag.rs`, a `team` model module.
- entity tag storage on the entity/player record; teams persisted in the world store (key prefix).
- chat/nametag/hurt display paths read the team colour/prefix.

## Edge cases to test explicitly
- tags are per-entity permanent; `/tag @e[tag=x]` selecting by a tag; adding the same tag twice is
  a no-op and reports 0.
- team names are team-colour-prefixed and distinct; adding a duplicate team errors unless `[add]`.
- `join` moves a member from an old team (one team per entity); `leave` on an unteamed entity no-ops.
- `collisionRule` actually changes collision; `friendlyFire` blocks team damage; invisibility.
- prefix/suffix render in chat and above the nametag; JSON components allowed.
- `/teammsg` from an unteamed player errors (`You are not in a team`); `/tm` alias.
- a player leaving/join keeps/removes the member per vanilla.

## Done criteria
- Model + commands + tests above; team data sent to clients so colours/prefixes show.
- fmt / clippy `-D warnings` / `cargo-locked test`.
