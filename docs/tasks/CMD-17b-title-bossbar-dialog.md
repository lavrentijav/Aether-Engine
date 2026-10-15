# CMD-17b — V-data: title, bossbar, dialog

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-17-messaging.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/title <targets> title\|subtitle\|actionbar <title>` | `minecraft.command.title` | 2 | (no feedback) |
| `/title <targets> times <fadeIn> <stay> <fadeOut>` | `minecraft.command.title` | 2 | (no feedback) |
| `/title <targets> clear\|reset` | `minecraft.command.title` | 2 | (no feedback) |
| `/bossbar add <id> <name>` | `minecraft.command.bossbar` | 2 | `Added bossbar <id>` |
| `/bossbar remove\|list\|get\|set <id> …` | `minecraft.command.bossbar` | 2 | `Removed bossbar <id>` / `There are <n> bossbars` |
| `/dialog show <targets> <dialog>` | `minecraft.command.dialog` | 2 | (opens the dialog) |

## Files
- new `crates/aether-server/src/commands/title.rs`, `bossbar.rs`, `dialog.rs`.
- per-codec title / bossbar / dialog packets.

## Edge cases to test explicitly
- `/title` fade/stay/fade negative or zero; default times (10/70/20); `actionbar`.
- `/bossbar set value 0..1` clamps; `color`, `style`, `max`, `visible`, `players`, `name`;
  unknown id → `No bossbar named <id>`; `list` before/after; removed-while-shown disappears.
- `/dialog show` on a codec without dialogs degrades gracefully (record which).

## Done criteria
- Implementation + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
