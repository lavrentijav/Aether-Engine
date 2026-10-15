# CMD-09 — V-admin: server control & info

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`

## Goal
Vanilla server-control and informational commands. These talk to the existing persistence/tick
machinery; no new gameplay.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/stop` | `minecraft.command.stop` | 4 | (none; server saves and exits) |
| `/save-all [flush]` | `minecraft.command.save-all` | 4 | `Saved the game` |
| `/save-on` | `minecraft.command.save-on` | 4 | `Automatic saving is now enabled` |
| `/save-off` | `minecraft.command.save-off` | 4 | `Automatic saving is now disabled` |
| `/list [uuids]` | `minecraft.command.list` | 0 | `There are <n> of a max of <m> players online: <names>` |
| `/seed [<world>]` | `minecraft.command.seed` | 2 | `Seed: [<seed>]` |
| `/version` | `minecraft.command.version` | 2 | `This server is running Aether Engine version <v> (MC: 1.21.11)` |
| `/setidletimeout <minutes>` | `minecraft.command.setidletimeout` | 3 | `Kicking idle players after <n> minute(s)` / `Disabled idle timeout` at 0 |
| `/transfer <hostname> [<port>] [<players>]` | `minecraft.command.transfer` | 3 | `Transferred <players> to <host>:<port>` |
| `/publish [allow-commands] [gamemode] [port]` | `minecraft.command.publish` | 4 | `Started serving on <port>` |

`/publish` is a singleplayer-integrated-server command; on the dedicated server it must either be
absent from the tree or fail like vanilla does. Decide from the 1.21.11 behaviour and record it.

## Files
- new `crates/aether-server/src/commands/server.rs`.
- `/save-all`: the existing save path (grep `save` in `session.rs`/`stored.rs`).
- `/list`: `players::SharedRegistry` + `config.server.max_players`.
- `/seed`: `worldgen` seed.
- `/transfer`: send the vanilla Transfer packet (add it to the codecs that support it, 1.20.5+)
  then close the connection.
- idle timeout: a per-player last-input timestamp already exists for keep-alive? if not, add it.

## Edge cases to test explicitly
- `/save-all` while another save is in flight does not corrupt state; `/save-off` then `/stop`
  still flushes on shutdown (or matches vanilla: save-off means `/stop` warns).
- `/list` exactly vanilla when 0 players and when full; `uuids` form.
- `/seed` with no world arg uses the overworld; an unloaded dimension errors.
- `/setidletimeout 0` disables; a negative/!numeric arg errors with the vanilla type message.
- `/transfer` to an unknown host sends the packet then the client is disconnected; a player who
  cannot transfer is reported.
- permission levels as in the table (level 3 cannot `/stop`).

## Done criteria
- Implementations + packet for `/transfer` + tests for the edge cases.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
