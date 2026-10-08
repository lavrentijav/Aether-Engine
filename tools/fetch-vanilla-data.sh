#!/bin/bash
# Fetch the vanilla data pack the generator reads (worldgen_data).
#
#   tools/fetch-vanilla-data.sh [version] [dest]
#
# Downloads the official server jar for `version` (default 1.21.11 — the
# block set the engine is built on) from Mojang, checks its SHA-1 against the
# version manifest, and unpacks the game's data/minecraft into
# `dest`/<version>/data/minecraft (default dest: ./vanilla). Prints the line to
# put in worldgen_data. Needs curl, unzip, python3 and sha1sum; no Java.
set -euo pipefail
V=${1:-1.21.11}
DEST=${2:-$PWD/vanilla}
OUT=$DEST/$V
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

MANIFEST=https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
curl -fsSL -o "$TMP/manifest.json" "$MANIFEST"
URL=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(next(v["url"] for v in m["versions"] if v["id"]==sys.argv[2]))' "$TMP/manifest.json" "$V") \
  || { echo "no such version: $V" >&2; exit 1; }
curl -fsSL -o "$TMP/version.json" "$URL"
read -r JAR_URL JAR_SHA1 < <(python3 -c 'import json,sys; s=json.load(open(sys.argv[1]))["downloads"]["server"]; print(s["url"], s["sha1"])' "$TMP/version.json")

echo "downloading the $V server jar..."
curl -fsSL -o "$TMP/server.jar" "$JAR_URL"
echo "$JAR_SHA1  $TMP/server.jar" | sha1sum -c --quiet - || { echo "SHA-1 mismatch: refusing to use the jar" >&2; exit 1; }

# The download is a bundler; the game itself is the jar inside it.
mkdir -p "$TMP/bundle"
unzip -q -o "$TMP/server.jar" 'META-INF/versions/*' -d "$TMP/bundle"
INNER=$(find "$TMP/bundle/META-INF/versions" -name '*.jar' | head -1)
[ -n "$INNER" ] || { echo "no game jar inside the bundle" >&2; exit 1; }

rm -rf "$OUT.new" && mkdir -p "$OUT.new"
unzip -q -o "$INNER" 'data/minecraft/*' -d "$OUT.new"
[ -d "$OUT.new/data/minecraft/worldgen" ] || { echo "the jar has no data/minecraft/worldgen" >&2; exit 1; }
rm -rf "$OUT" && mv "$OUT.new" "$OUT"
echo "worldgen_data = \"$OUT\""
