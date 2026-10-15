# CMD-17 — V-data: chat, titles, bossbars, dialogs

Status: 📋 task brief (CMD0) · size: L · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`

## Split (CMD0 review)
This L group is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-17a | `docs/tasks/CMD-17a-chat-tellraw.md` | `/say` `/me` `/msg` `/tell` `/w` `/tellraw` | M |
| CMD-17b | `docs/tasks/CMD-17b-title-bossbar-dialog.md` | `/title` `/bossbar` `/dialog` | M |

## Goal
The text/UI commands: chat, raw JSON components, titles/actionbar and bossbars. These are also the
output primitives the plugin/API layer reuses.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/say <message>` | `minecraft.command.say` | 2 | `[Server] <message>` broadcast |
| `/me <action>` | `minecraft.command.me` | 0 | `* <player> <action>` broadcast |
| `/msg <targets> <message>` (aliases `/tell`, `/w`) | `minecraft.command.msg` | 0 | `You whisper to <player>: <message>`; recipient `You whisper to ...` |
| `/tellraw <targets> <message>` | `minecraft.command.tellraw` | 2 | (renders the component; no feedback line) |
| `/title <targets> <title\|subtitle\|actionbar> <title>` | `minecraft.command.title` | 2 | (no feedback) |
| `/title <targets> times <fadeIn> <stay> <fadeOut>` | `minecraft.command.title` | 2 | (no feedback) |
| `/title <targets> clear\|reset` | `minecraft.command.title` | 2 | (no feedback) |
| `/bossbar add <id> <name>` | `minecraft.command.bossbar` | 2 | `Added bossbar <id>` |
| `/bossbar remove\|list\|get\|set <id> ...` | `minecraft.command.bossbar` | 2 | `Removed bossbar <id>` / `There are <n> bossbars` |
| `/dialog show <targets> <dialog>` | `minecraft.command.dialog` | 2 | (opens the dialog) |

Chat components: 1.20.3+ sends NBT components; 1.20.2 sends JSON (see KNOWN_ISSUES A.10). Use the
existing text-component writer and keep the older-codec path. Messages must be sanitised: a player
name in `/say`/`/me` is inserted as a literal, never re-parsed as a component.

**Ownership rule (vanilla names stay core).** `/msg`, `/tell`, `/w`, `/say`, `/me`, `/tellraw`,
`/title`, `/bossbar`, `/dialog` are owned by the core here, permanently. A plugin may **not**
re-register any of them to change behaviour; it hooks them through an event instead. Concretely,
`essentials-chat` (CMD-40) adds only the *new* names `/r`, `/mail`, `/ignore`, `/socialspy`,
`/broadcast`, `/motd`, `/rules` and formats chat through a `before-chat`/`on-command` hook on the
core `/msg` and `/me` — it never declares `/msg`/`/me` itself. The same rule applies to `/tp` vs an
Essentials teleport plugin: the vanilla name stays core, the plugin adds new names only.

## Files
- new `crates/aether-server/src/commands/text.rs` (say/me/msg/tellraw), `title.rs`, `bossbar.rs`,
  `dialog.rs`; reuse the text-component code in `protocol/`.
- chat broadcast path in `session.rs:776` (already broadcasts `<name> text`).
- whisper: per-player routing + a reply target for the plugin `/r` (CMD-40); store the last
  whisper counterpart.

## Edge cases to test explicitly
- `/msg` to a non-player (mob) errors; to yourself works (vanilla allows).
- `/tellraw @a <component>` with hover/click/translate/score components on each codec.
- `/title` fade/stay/fade negative or zero; default times (10/70/20); `actionbar` and `title`.
- `/bossbar set value 0..1` clamped; `color`, `style`, `max`, `visible`, `players`, `name`;
  an unknown bossbar id errors (`No bossbar named <id>`).
- `/bossbar list` before/after; a bossbar removed while shown disappears on clients.
- `/dialog show` on a codec without dialogs degrades gracefully (component text or no-op) — record
  which.
- `/me` and `/say` never let a player inject a component through their name.
- console `say` uses `[Server]`.

## Done criteria
- Implementations + tests across 1.21.11 and one older codec (NBT vs JSON component).
- fmt / clippy `-D warnings` / `cargo-locked test`.
