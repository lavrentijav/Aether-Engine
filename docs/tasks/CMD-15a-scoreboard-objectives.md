# CMD-15a — V-data: scoreboard model + objectives

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-06`
Parent/umbrella: `docs/tasks/CMD-15-scoreboard.md`

## Goal
The scoreboard model (objectives, criteria, display slots) and the `objectives` half of the command.
The model is read by `/execute if score` (CMD-22), `/trigger` (CMD-18) and display packets.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/scoreboard objectives add <name> <criterion> [<displayName>]` | `minecraft.command.scoreboard` | 2 | `Added new objective '<name>' successfully` |
| `/scoreboard objectives remove <name>` | `minecraft.command.scoreboard` | 2 | `Removed objective '<name>' successfully` |
| `/scoreboard objectives list` | `minecraft.command.scoreboard` | 2 | `There are <n> objectives: …` |
| `/scoreboard objectives modify <name> displayname\|displayautoupdate\|numberformat\|rendertype …` | `minecraft.command.scoreboard` | 2 | `Changed …` |
| `/scoreboard objectives setdisplay <slot> [<objective>]` | `minecraft.command.scoreboard` | 2 | `Set display slot <slot> to …` / `Cleared display slot <slot>` |

Criteria: the vanilla set (`dummy`, `trigger`, `health`, `food`, `air`, `armor`, `xp`, `level`,
`killed`, `mined`, `crafted`, `used`, `broken`, `picked_up`, `dropped`, `teamkill.<color>`,
`killedByTeam.<color>`, statistics). Criteria the engine cannot track are accepted and stay 0,
except `trigger` (CMD-18). `setdisplay sidebar` shows top scores.

## Files
- new `crates/aether-server/src/scoreboard.rs` (model) + `commands/scoreboard.rs`.
- persistence in the world store; objective ids registered in the key list (T5); display packets.

## Edge cases to test explicitly
- add objective with a duplicate name errors (`An objective already exists by that name`).
- unknown criterion errors; `trigger` objectives start locked.
- `setdisplay` slots (`sidebar`, `below_name`, `list`, sidebars); `clear` a slot.
- display name JSON component with a `numberformat` style; an invalid component errors.
- objectives persist across restart.

## Done criteria
- Model + commands + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
