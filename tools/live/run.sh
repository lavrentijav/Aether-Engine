#!/usr/bin/env bash
# V1 live smoke-test harness: run one MCC scenario against the fi2 test server.
#
# Usage: run.sh <scenario> [mc_version]
#   scenario   name of a file in tools/live/scenarios/<scenario>.txt
#   mc_version MCC MinecraftVersion to use (default 1.21.11)
#
# Exit code 0 = pass. Always prints one summary line:
#   PASS <scenario> <version>
#   FAIL <scenario> <version> <reason>
#
# Scenario file format (one directive per line, '#' comments):
#   send <cmd>       write <cmd> to MCC stdin (server cmds go through as-is)
#   wait <secs>      sleep before the next directive
#   expect <regex>   regex that MUST appear in the ANSI-stripped log
#   reject <regex>   regex that MUST NOT appear in the ANSI-stripped log
set -u
set -o pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MCC="${MCC:-/srv/dev/tools/mcc/MinecraftClient}"
HOST="${MCC_HOST:-fi2.walk.boykisser.online}"
ACCOUNT="${MCC_ACCOUNT:-_alpha_01}"
TEMPLATE="$HERE/mcc-template.ini"
SCRATCH_ROOT="${MCC_SCRATCH:-/tmp/opencode/live}"

SCEN="${1:-}"
VER="${2:-1.21.11}"
if [ -z "$SCEN" ]; then
    echo "usage: run.sh <scenario> [mc_version]" >&2
    exit 2
fi
SCENFILE="$HERE/scenarios/${SCEN}.txt"
if [ ! -f "$SCENFILE" ]; then
    echo "FAIL $SCEN $VER missing scenario file $SCENFILE"
    exit 2
fi

WORK="$SCRATCH_ROOT/${SCEN}-${VER}"
mkdir -p "$WORK"
RAW="$WORK/raw.log"
LOG="$WORK/output.log"

sed "s/^MinecraftVersion = .*/MinecraftVersion = \"${VER}\"/" "$TEMPLATE" > "$WORK/MinecraftClient.ini"

# Total budget: sum of wait directives plus a fixed margin for join/quit.
TOTAL="$(awk '/^[[:space:]]*wait[[:space:]]/{s+=$2} END{print s+50}' "$SCENFILE")"

feed() {
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%$'\r'}"
        case "$line" in ''|'#'*) continue ;; esac
        kind="${line%% *}"
        val="${line#* }"
        case "$kind" in
            send) printf '%s\n' "$val" ;;
            wait) sleep "$val" ;;
            *) : ;;
        esac
    done < "$SCENFILE"
    sleep 3
    printf '/quit\n'
}

( cd "$WORK" && feed ) | ( cd "$WORK" && timeout "$TOTAL" "$MCC" "$ACCOUNT" - "$HOST" > "$RAW" 2>&1 )
RC=$?

sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g; s/\x1b\][^\x07]*(\x07|\x1b\\)//g; s/\r/\n/g' "$RAW" > "$LOG"

FAIL=""
if [ "$RC" -eq 124 ]; then
    FAIL="timeout after ${TOTAL}s"
fi

while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    case "$line" in ''|'#'*) continue ;; esac
    kind="${line%% *}"
    val="${line#* }"
    case "$kind" in
        expect)
            if ! grep -Eq -- "$val" "$LOG"; then
                FAIL="${FAIL:+$FAIL; }missing expected /$val/"
            fi
            ;;
        reject)
            if grep -Eq -- "$val" "$LOG"; then
                FAIL="${FAIL:+$FAIL; }found rejected /$val/"
            fi
            ;;
        *) : ;;
    esac
done < "$SCENFILE"

if [ -z "$FAIL" ]; then
    echo "PASS $SCEN $VER"
    exit 0
fi
echo "FAIL $SCEN $VER $FAIL"
exit 1
