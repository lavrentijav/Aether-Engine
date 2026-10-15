# CMD-40 — Plugin: essentials-chat

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/essentials-chat`
Needs: `oc/P1`, `oc/CMD-32` (player API), `oc/CMD-33` (prefix/suffix meta)

## Goal
Chat features from EssentialsX/chat plugins: private messages with `/r`, mail, ignore, social spy,
broadcast and formatted chat with permission prefixes/suffixes (`docs/design/commands.md` §4).

## Commands
| command | syntax | node |
|---|---|---|
| msg | `/msg <player> <message>` (also core `/tell`/`/w`, CMD-17) | `essentials.msg` |
| reply | `/r <message>`, `/reply <message>` | `essentials.msg` |
| mail | `/mail send <player> <message>`, `/mail read`, `/mail clear` | `essentials.mail` |
| ignore | `/ignore [<player>]` | `essentials.ignore` |
| socialspy | `/socialspy` | `essentials.socialspy` |
| broadcast | `/broadcast <message>` | `essentials.broadcast` |
| motd | `/motd`, `/rules` | `essentials.motd` |
| me | `/me <action>` (formatting override of core `/me`) | `essentials.me` |

Chat pipeline: an `after-chat`/`before-chat` hook (P1) formats player chat according to the
permission meta (prefix/suffix/colour/weight from CMD-33), applies the player's `chat_color`,
`ignore` state and social-spy routing. `/broadcast` and `/me` use the same component pipeline as
CMD-17; mail and ignore persist in player KV.

## Files
- `plugins/essentials-chat/`: manifest, `src/`, README.
- chat hook in `aether-server`/P1; prefix/suffix from CMD-33; message/tellraw from CMD-17.

## Edge cases to test explicitly
- `/r` with no one to reply to errors; `/r` to an offline player queues/errors per EssX.
- `/ignore <player>` hides their chat but not console/commands; `/ignore` toggles.
- social spy: a spy sees all private messages, including `/r`; not messages to themselves.
- prefix/suffix with a colour renders and does not break name sanitisation.
- `/broadcast` to all worlds; `&`/`§`/JSON colour codes handled safely.
- `/mail read` after a restart; `/mail clear` removes; an offline target still receives mail.
- a muted player (CMD-41) cannot chat or `/msg`; the hook order is documented.

## Done criteria
- Plugin + hooks + persistence + tests; README lists commands and links CMD-40.
- fmt / clippy `-D warnings` / `cargo-locked test`.
