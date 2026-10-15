# CMD-15 — V-data: `/scoreboard`

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-06`

## Split (CMD0 review)
This L task is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-15a | `docs/tasks/CMD-15a-scoreboard-objectives.md` | scoreboard model + `/scoreboard objectives …` | M |
| CMD-15b | `docs/tasks/CMD-15b-scoreboard-players.md` | `/scoreboard players …` (set/add/remove/reset/get/enable/operation/display) | M |

## Goal
The full vanilla scoreboard command, which is also storage the rest of the engine reads
(`/execute if score`, `/trigger`, sidebar display). Implement the scoreboard model first (objectives,
criteria, players, display slots), then the commands.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/scoreboard objectives add <name> <criterion> [<displayName>]` | `minecraft.command.scoreboard` | 2 | `Added new objective '<name>' successfully` |
| `/scoreboard objectives remove <name>` | `minecraft.command.scoreboard` | 2 | `Removed objective '<name>' successfully` |
| `/scoreboard objectives list` | `minecraft.command.scoreboard` | 2 | `There are <n> objectives: ...` |
| `/scoreboard objectives modify <name> displayname\|displayautoupdate\|numberformat\|rendertype ...` | `minecraft.command.scoreboard` | 2 | `Changed display name for objective '<name>' to ...` |
| `/scoreboard objectives setdisplay <slot> [<objective>]` | `minecraft.command.scoreboard` | 2 | `Set display slot <slot> to show objective '<name>'` / `Cleared display slot <slot>` |
| `/scoreboard players set\|add\|remove\|reset <targets> <objective> [<score>]` | `minecraft.command.scoreboard` | 2 | `Set score of <name> for objective '<obj>' to <n>` |
| `/scoreboard players get <target> <objective>` | `minecraft.command.scoreboard` | 2 | `<name> has <n> [<objective>]` |
| `/scoreboard players enable\|operation\|display ...` | `minecraft.command.scoreboard` | 2 | per subcommand |

Objective criteria: the vanilla set (`dummy`, `trigger`, `health`, `food`, `air`, `armor`, `xp`,
`level`, `killed`, `mined`, `crafted`, `used`, `broken`, `picked_up`, `dropped`, `teamkill.<color>`,
`killedByTeam.<color>`, statistics). Criteria the engine cannot track are accepted and stay 0,
except `trigger` which CMD-18 uses.

## Files
- new `crates/aether-server/src/scoreboard.rs` (model) + `commands/scoreboard.rs` (commands).
- persistence in the world/player store; team/objective ids registered in the key list (T5).
- display: objective titles go out via the existing scoreboard/title packets per codec.

## Edge cases to test explicitly
- add objective with a duplicate name errors (`An objective already exists by that name`).
- unknown criterion errors; `trigger` objectives start `locked`.
- `players operation` (assign, +=, -=, *=, /=, %=, =, <, >, ><) for both players and `#fake`
  player names; division by zero errors.
- `reset` clears one score; `setdisplay sidebar` shows top scores; `below_name`/`list` slots.
- scores are unbounded 32-bit ints (vanilla); set/parse overflow errors.
- display name JSON components with a `numberformat` style; an invalid component errors.
- all scores' state persists across restart.

## Done criteria
- Model + commands + tests; `/execute if score` (CMD-22) can read the model.
- fmt / clippy `-D warnings` / `cargo-locked test`.
