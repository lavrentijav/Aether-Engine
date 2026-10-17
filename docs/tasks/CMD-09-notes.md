# CMD-09 — notes

Behaviour and text were checked against a **live dedicated 1.21.11 server**
(`server.jar` from `/tmp/aetherversions/1.21.11`, offline-mode), not only the
data-generator report.

- **`/seed` takes no argument and prints brackets.** Live: `Seed:
  [6133924130035410389]`; `commands.seed.success` is `Seed: [%s]`. A second
  word is `Incorrect argument for command`. The brief's `[<world>]` form and
  its unloaded-dimension case are not in 1.21.11, so they are not
  implemented.
- **`/version` is the 1.21.11 header plus nine fields**, not the old
  `This server is running …` line (which no longer exists in the release):

  ```text
  Server version info:
  id = 0.1.0
  name = Aether Engine
  data = 4671
  series = main
  protocol = 774-777 (0x306-0x309)
  build_time = unknown
  pack_resource = 75.0
  pack_data = 94.1
  stable = no
  ```

  `id` is the crate version, `data`/`pack_resource`/`pack_data` are the
  1.21.11 data/resource generation the engine's tables are built from,
  `protocol` spans every modern codec, `stable = no` reflects the preview
  status, and `build_time` is `AETHER_BUILD_TIME` when a build sets it
  (`unknown` otherwise).
- **`/save-all` prints two lines in vanilla's order**, as live vanilla does:
  `Saving the game (this may take a moment!)` **before** the write, then
  `Saved the game` after it; on failure the first line is still informational
  and only `Unable to save the game (is there enough disk space?)` is an
  error. A player (chat) and the console get the first line immediately
  through a progress sink (`execute_feedback_streaming`); RCON, whose protocol
  carries one response body, gets both lines in order. A non-streaming caller
  (tests, command functions) gets only the result line, so progress text is
  never error-coloured.
- **`/transfer` target errors.** With no `players` argument the source's own
  player is used; a console/RCON source has none and gets
  `A player is required to run this command here`
  (`permissions.requires.player`). An empty selector, an unknown selector or
  an unknown name is `No player was found`
  (`argument.entity.notfound.player`). `commands.transfer.error.no_players`
  is not reachable from this command in 1.21.11, so it is not emitted.
- **`/publish` is absent from the dedicated tree.** The live dedicated server
  answers `Unknown or incomplete command`; the command belongs to the
  singleplayer integrated server. It is not in the tree, the catalogue or
  `/help`.
- **Save locking.** The shared save lock covers `/save-all`, the autosave
  thread and the shutdown watcher, so `/stop` racing `/save-all` cannot flush
  twice concurrently. Shutdown still flushes after a `/save-off`, as
  vanilla's clean stop does.
- **Idle timeout closes the connection, without a play-phase Disconnect
  packet.** `/setidletimeout` records the timeout and the play loop closes a
  silent player's socket (logged with the vanilla reason); the engine has no
  play Disconnect event, so the client sees the connection drop rather than a
  `multiplayer.disconnect.idling` screen.
