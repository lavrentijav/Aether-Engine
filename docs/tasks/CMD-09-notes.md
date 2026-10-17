# CMD-09 — notes

Decisions taken against the local 1.21.11 data-generator report
(`generated/reports/commands.json`, `packets.json`).

- **`/seed` takes no argument in 1.21.11.** The tree is the bare
  `/seed` literal; an extra word is `Incorrect argument for command`. The
  brief's `[<world>]` form and its "unknown dimension" edge case are not in
  the release, so they are not implemented.
- **`/publish` is present on the dedicated server and fails there.** It is
  registered at owner level with `[allow-commands] [gamemode] [port]`, and
  execution answers `commands.publish.failed` (`Unable to host local game`).
- **Bounds are part of the arguments.** `setidletimeout <minutes>` is an
  integer with min 0; `transfer <hostname> [port] [players]` has port
  1..=65535 and an `EntityArgument.players` target; `publish ... [port]` has
  0..=65535. They are declared on the wire and enforced while parsing, with
  vanilla's `argument.integer.low/big` text.
- **Transfer.** The packet id is 0x73 on 766/767, 0x7A on 768–772, 0x7F on
  773/774, 0x81 on 775/776 and 0x84 on 777. It exists only from 1.20.5 (766),
  so older clients are refused rather than sent a packet their table lacks.
  The connection closes with `disconnect.transfer` after the packet has been
  written.
- **`/list`** is player-visible at level 0, prints connection order, honours
  staff vanish for a player viewer, and appends the hyphenated UUID in the
  `uuids` form.
- **Saving.** `/save-all [flush]` and `/save-off` control the periodic save
  path; a clean `/stop` still flushes. Saves are serialised, and `/save-all`
  writes online player state as well as the world.
