# CMD-40 — Plugin: essentials-chat

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/essentials-chat`
Needs: `oc/P1`, `oc/CMD-32` (player API), `oc/CMD-33` (prefix/suffix meta)

## Goal
Chat features from EssentialsX/chat plugins: replies, mail, ignore, social spy, broadcast and
formatted chat with permission prefixes/suffixes (`docs/design/commands.md` §4).

## Ownership rule — vanilla names stay core
`/msg`, `/tell`, `/w`, `/say`, `/me` are **core** commands (CMD-17) and are **not** re-registered by
this plugin. The plugin:
- adds only the *new* names `/r` (reply), `/mail`, `/ignore`, `/socialspy`, `/broadcast`, `/motd`,
  `/rules`;
- changes `/msg`/`/me` behaviour through a `before-chat`/`on-command` **hook** that formats the
  message (prefix/suffix/colour), applies `ignore`, records the last whisper target for `/r`, and
  forwards to social spies.
The same rule applies everywhere: a plugin never answers a name the core owns; it hooks it or adds
a new name (e.g. `/tp` stays core, an essentials teleport plugin adds `/tpa` etc.).

## Commands (only the new names)
| command | syntax | node |
|---|---|---|
| reply | `/r <message>`, `/reply <message>` | `essentials.msg` |
| mail | `/mail send <player> <message>`, `/mail read`, `/mail clear` | `essentials.mail` |
| ignore | `/ignore [<player>]` | `essentials.ignore` |
| socialspy | `/socialspy` | `essentials.socialspy` |
| broadcast | `/broadcast <message>` | `essentials.broadcast` |
| motd | `/motd`, `/rules` | `essentials.motd` |

`/r` replies to the last player who whispered the sender (or the sender's last `/msg` target);
the target is recorded by the core `/msg` hook. `/broadcast` uses the component pipeline of
CMD-17. Mail and ignore persist in player KV.

## Files
- `plugins/essentials-chat/`: manifest, `src/`, README (already a CMD0c skeleton; update its command
  list to drop `/msg`/`/me`).
- chat hook in `aether-server`/P1; prefix/suffix from CMD-33; message/tellraw from CMD-17.

## Edge cases to test explicitly
- `/r` with no one to reply to errors; `/r` after the counterparty went offline (per EssX config).
- `/ignore <player>` hides their chat but not console/commands; `/ignore` toggles.
- social spy: a spy sees all private messages, including `/r`; not messages to themselves.
- prefix/suffix with a colour renders and does not break name sanitisation.
- `/broadcast` to all worlds; `&`/`§`/JSON colour codes handled safely.
- `/mail read` after a restart; `/mail clear` removes; an offline target still receives mail.
- a muted player (CMD-41) cannot chat or `/msg`; the hook order is documented.
- **the plugin declares no core name:** a test asserts its manifest `commands` contains none of
  `/msg /tell /w /say /me /tellraw`.

## Done criteria
- Plugin + hooks + persistence + tests; README lists commands and links CMD-40.
- fmt / clippy `-D warnings` / `cargo-locked test`.
