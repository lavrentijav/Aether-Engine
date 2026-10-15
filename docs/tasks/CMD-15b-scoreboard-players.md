# CMD-15b — V-data: /scoreboard players

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-06`, `oc/CMD-15a`
Parent/umbrella: `docs/tasks/CMD-15-scoreboard.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/scoreboard players set\|add\|remove\|reset <targets> <objective> [<score>]` | `minecraft.command.scoreboard` | 2 | `Set score of <name> for objective '<obj>' to <n>` |
| `/scoreboard players get <target> <objective>` | `minecraft.command.scoreboard` | 2 | `<name> has <n> [<objective>]` |
| `/scoreboard players enable\|operation\|display …` | `minecraft.command.scoreboard` | 2 | per subcommand |

## Files
- `crates/aether-server/src/scoreboard.rs` + `commands/scoreboard.rs` (from CMD-15a).

## Edge cases to test explicitly
- `operation` (assign `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `<`, `>`, `><`) for players and `#fake`
  names; division by zero errors.
- `reset` clears one score; scores are unbounded 32-bit ints; set/parse overflow errors.
- `enable` marks a trigger objective usable (CMD-18).
- scores persist; display updates on the sidebar.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test`.
