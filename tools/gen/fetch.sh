#!/bin/bash
# Fetch one release's server jar, run its data generator, and unpack its
# vanilla data pack — everything `versions.py` reads.
#
#   tools/gen/fetch.sh <version>
#
# Into $AETHER_VERSIONS/<version>/ (default ./versions):
#   server.jar, inner/ (the bundled jar and libraries), data/data/minecraft/,
#   generated/reports/. Needs curl, unzip, python3 and a Java new enough for
#   the release ($JAVA, default `java`; 26.x needs Java 25).
set -e
V=$1
[ -n "$V" ] || { echo "usage: $0 <version>" >&2; exit 2; }
ROOT=${AETHER_VERSIONS:-$PWD/versions}
JAVA=${JAVA:-java}
D=$ROOT/$V; mkdir -p "$D"; cd "$D"
[ -f manifest.json ] || curl -sf -o manifest.json https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
URL=$(python3 -c "import json,sys;print([v['url'] for v in json.load(open('manifest.json'))['versions'] if v['id']==sys.argv[1]][0])" "$V")
curl -sf -o version.json "$URL"
SJ=$(python3 -c "import json;print(json.load(open('version.json'))['downloads']['server']['url'])")
[ -f server.jar ] || curl -sf -o server.jar "$SJ"
mkdir -p inner && (cd inner && unzip -qo ../server.jar 'META-INF/versions/*' 'META-INF/libraries/*')
INNER=$(find inner/META-INF/versions -name '*.jar' | head -1)
mkdir -p data && (cd data && unzip -qo "../$INNER" 'data/*')
if [ ! -d generated/reports ]; then
  "$JAVA" -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports --output generated > gen.log 2>&1 \
    || { tail -5 gen.log; exit 1; }
fi
echo "$D ready"
