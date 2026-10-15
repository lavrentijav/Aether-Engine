#!/usr/bin/env bash
# V1: run every live scenario on each Minecraft version, print a table and
# write tools/live/last-run.md with the table plus failing log excerpts.
#
# Usage: all.sh [versions...]   (default: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2)
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN="$HERE/run.sh"
SCRATCH="${MCC_SCRATCH:-/tmp/opencode/live}"
OUT_MD="$HERE/last-run.md"

DEFAULT_VERSIONS=(1.21.11 1.21.4 1.20.6 1.20.4 1.20.2)
SCENARIOS=(join inspect place-break-rollback inventory-persist eat-effects container potion commands journal-upgrade multi-version-join)

if [ "$#" -gt 0 ]; then
    VERSIONS=("$@")
else
    VERSIONS=("${DEFAULT_VERSIONS[@]}")
fi

declare -A RESULT
FAIL_LINES=()

for scen in "${SCENARIOS[@]}"; do
    for ver in "${VERSIONS[@]}"; do
        line="$(bash "$RUN" "$scen" "$ver")"
        rc=$?
        echo "$line"
        status="${line%% *}"
        RESULT["$scen,$ver"]="$status"
        if [ "$rc" -ne 0 ]; then
            FAIL_LINES+=("$line")
        fi
        sleep 5
    done
done

# The auth scenario is login-layer, not gameplay, so it runs once after the
# matrix (it waits out the server's login-rate window itself).
AUTH_OUT="$(bash "$HERE/auth.sh" "${VERSIONS[0]}" 2>&1)"
AUTH_RC=$?
echo
echo "$AUTH_OUT"
AUTH_FAILS=0
if [ "$AUTH_RC" -ne 0 ]; then
    AUTH_FAILS=1
fi

{
    echo "# V1 live run"
    echo
    echo "- Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
    echo "- Target: ${MCC_HOST:-fi2.walk.boykisser.online}:25565, account ${MCC_ACCOUNT:-_alpha_01}"
    echo "- Versions: ${VERSIONS[*]}"
    echo
    echo "| scenario |$(for v in "${VERSIONS[@]}"; do printf ' %s |' "$v"; done)"
    echo "| --- |$(for _ in "${VERSIONS[@]}"; do printf ' --- |'; done)"
    for scen in "${SCENARIOS[@]}"; do
        printf '| %s |' "$scen"
        for ver in "${VERSIONS[@]}"; do
            s="${RESULT[$scen,$ver]:-?}"
            printf ' %s |' "$s"
        done
        echo
    done

    if [ "${#FAIL_LINES[@]}" -eq 0 ]; then
        echo
        echo "All scenarios passed."
    else

    echo
    echo "## Failures"
    for fl in "${FAIL_LINES[@]}"; do
        set -- $fl
        scen="$2"
        ver="$3"
        log="$SCRATCH/${scen}-${ver}/output.log"
        echo
        echo "### $fl"
        echo
        echo '```'
        if [ -f "$log" ]; then
            tail -n 30 "$log"
        else
            echo "(no log at $log)"
        fi
        echo '```'
    done
    fi

    echo
    echo "## Auth (AU1 hybrid, ${VERSIONS[0]})"
    echo
    echo '```'
    echo "$AUTH_OUT"
    echo '```'
} > "$OUT_MD"

echo
echo "Wrote $OUT_MD"
if [ "${#FAIL_LINES[@]}" -eq 0 ] && [ "$AUTH_FAILS" -eq 0 ]; then
    exit 0
fi
exit 1
