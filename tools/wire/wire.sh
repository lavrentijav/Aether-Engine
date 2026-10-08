#!/bin/bash
# Connect a client built from a release's own classes to a running server and
# decode everything it sends with that release's codecs.
#
#   tools/wire/wire.sh <version> <port> <player name> [known|empty|watch]
#
#   known  answer "select known packs" with the vanilla pack (a real client)
#   empty  answer with none, so every registry entry must carry its data
#   watch  only listen — run beside a `known` client to see what the server
#          sends to everyone else (block breaks, other players' swings)
#
# The release must have been fetched with tools/gen/fetch.sh (into
# $AETHER_VERSIONS, default ./versions); $JAVA must run it (Java 25 for 26.x).
# The player should be an operator: the script drives the game with commands.
set -e
V=$1; PORT=$2; NAME=$3; MODE=${4:-known}
ROOT=${AETHER_VERSIONS:-$PWD/versions}
JAVA=${JAVA:-java}
JAVAC=${JAVAC:-$(dirname "$(command -v "$JAVA")")/javac}
case "$V" in
  26.1*) P=775 ;; 26.2) P=776 ;; 26.3) P=777 ;;
  *) P=${PROTOCOL:?unknown version $V: set PROTOCOL} ;;
esac
HERE=$(cd "$(dirname "$0")" && pwd)
CP=$(find "$ROOT/$V/inner/META-INF/versions" -name '*.jar' | head -1)
for j in $(find "$ROOT/$V/inner/META-INF/libraries" -name '*.jar'); do CP="$CP:$j"; done
OUT=$ROOT/$V/wire
if [ ! -f "$OUT/Wire.class" ] || [ "$HERE/Wire.java" -nt "$OUT/Wire.class" ]; then
  rm -rf "$OUT"; mkdir -p "$OUT/src"
  if [ "$V" \< 26.3 ]; then
    cp "$HERE/Wire.java" "$OUT/src/Wire.java"
  else
    # 26.3 turned these classes into records and replaced the swing packet
    # with "punch".
    sed -e 's/new ServerboundSwingPacket(InteractionHand.MAIN_HAND)/ServerboundPunchPacket.INSTANCE/' \
        -e 's/t\.getTags()/t.tags()/' \
        -e 's/createVanillaPackSource())/createVanillaPackSource().fullResources())/' \
        -e 's/new ServerboundAcceptTeleportationPacket(pp\.id())/new ServerboundAcceptTeleportationPacket(pp.id(), px, py, pz, 0f, 0f)/' \
        -e 's/c\.getChunkData()/c.chunkData()/' -e 's/c\.getX()/c.x()/' -e 's/c\.getZ()/c.z()/' \
        "$HERE/Wire.java" > "$OUT/src/Wire.java"
  fi
  "$JAVAC" -nowarn -d "$OUT" -cp "$CP" "$OUT/src/Wire.java"
fi
mkdir -p "$OUT/run" && cd "$OUT/run"
"$JAVA" -cp "$CP:$OUT" Wire "$PORT" "$P" "$NAME" "$MODE" 2>&1 \
  | grep -v '^WARNING' | sed 's/^\[[^]]*\] \[main\/INFO\]: \[STDOUT\]: //' | grep -v '^\['
