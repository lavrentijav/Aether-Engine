# Wire check: the release's own codecs against this server

`Wire.java` is a minimal client compiled against a release's server jar. Since
26.1 Mojang ships those jars unobfuscated, and they contain the full network
stack. So instead of trusting our encoder's own tests, the client decodes
**every packet the server sends with that release's real `STREAM_CODEC`** and
reports:

* any packet that fails to decode, or that decodes with bytes left over;
* registries the client would reject. They are built with the game's own
  `RegistryDataLoader`, from the network entries plus the vanilla pack, the
  way the client builds them;
* chunk sections that `LevelChunkSection.read` cannot parse, or whose non-air
  and fluid counts disagree with the blocks inside them.

It also drives the game with commands and the release's own serverbound
packets: it summons mobs, attacks with the 26.1 `attack` packet, digs, places
and opens a crafting table, gets blown up by a creeper, and respawns. Those
paths are exercised as well.

It found two real bugs on its first runs, both invisible to a bot library that
does not model them:

* 26.3 sends a `BitSet` as a byte array, not as 64-bit words. Every chunk's
  light masks were misread.
* The section non-air count included cave air and void air.

## Running

```sh
export JAVA=/path/to/jdk-25/bin/java            # 26.x needs Java 25
tools/gen/fetch.sh 26.3                          # server jar + libraries into ./versions
# a server on port 25998, with the player names below as operators
tools/wire/wire.sh 26.3 25998 Watcher watch &    # listens only
tools/wire/wire.sh 26.3 25998 Wired known        # plays, as a client with the vanilla pack
tools/wire/wire.sh 26.3 25998 Wired empty        # a client without it: every entry with data
```

The last line is `OK: no wire problems` or `PROBLEMS: n`, with each problem
printed above it. `NOT SEEN:` lists the expected packets the run never
received, which usually means the scenario took a different turn (the player
died early, say); it is not a wire problem.

Use a scratch server with its own `world_dir` and no `AETHER_DATABASE_URL`.
The script gives items, kills mobs and blows things up.
