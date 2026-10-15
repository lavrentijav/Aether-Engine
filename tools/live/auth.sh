#!/usr/bin/env bash
# V1 auth scenario: the login/admission layer introduced by AU1
# ([auth] mode = hybrid). Unlike the gameplay scenarios, these checks are
# about who the server lets in, so most of them drive a raw login probe
# (auth-probe.py) rather than the MCC client — an offline client cannot
# complete Mojang's encryption handshake, and the rate limit needs logins far
# faster than an MCC process can start.
#
# Checks (one PASS|FAIL line each):
#   invalid-name-space       a name with a space is refused "Invalid username."
#   invalid-name-long        a 20-char name is refused "Invalid username."
#   invalid-name-short       a 2-char name is refused "Invalid username."
#   login-rate-limit         rapid reconnects hit the per-IP limit ("Too many
#                            login attempts…"); the count before refusal is
#                            reported.
#   login-rate-recovery      after the window, a login is admitted again.
#   offline-open             a fresh, non-premium name joins offline.
#   operator-offline-admitted  `_alpha_01` (offline) is still admitted (MCC).
#
# Usage: auth.sh [mc_version]     (default 1.21.11; only for the MCC check)
#
# The rate-limit burst intentionally exhausts this client's per-IP budget, so
# the script waits out the server's login window (60 s) before measuring it and
# again before the recovery probe; the budget is then low enough for the two
# checks that follow.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN="$HERE/run.sh"
PROBE="$HERE/auth-probe.py"
HOST="${MCC_HOST:-fi2.walk.boykisser.online}"
PORT="${MCC_PORT:-25565}"
PROTO="${MCC_PROTOCOL:-774}"      # 1.21.11
BURST_CAP="${AUTH_BURST_CAP:-25}"
WINDOW_WAIT="${AUTH_WINDOW_WAIT:-62}"   # > the server's 60 s login window
VER="${1:-1.21.11}"

FAILED=0
pass() { echo "PASS auth $1 $2"; }
fail() { echo "FAIL auth $1 $2"; FAILED=1; }

# One raw login; prints "<status>\t<detail>".
login() { python3 "$PROBE" login "$HOST" "$PORT" "$1" "$PROTO"; }

# 1-3. Invalid usernames are refused with a clear message, before any auth
# work and without consuming the rate limit.
check_invalid() {
    local label="$1" name="$2"
    local out status detail
    out="$(login "$name")"
    status="${out%%$'\t'*}"
    detail="${out#*$'\t'}"
    if [ "$status" = "refused" ] && printf '%s' "$detail" | grep -q "Invalid username"; then
        pass "$label" "refused: $detail"
    else
        fail "$label" "expected refused/Invalid username, got: $out"
    fi
}

check_invalid invalid-name-space "bad name"
check_invalid invalid-name-long "abcdefghijklmnopqrst"
check_invalid invalid-name-short "ab"

# Start the rate-limit measurement from an empty window (no other login from
# this address in the last minute), so the count is the configured limit and
# not what earlier probes left over.
sleep "$WINDOW_WAIT"

# 4. Rapid reconnects hit the per-IP login rate limit.
burst_out="$(python3 "$PROBE" burst "$HOST" "$PORT" "rateprobelimit" "$PROTO" "$BURST_CAP")"
admitted="$(printf '%s\n' "$burst_out" | sed -n 's/.*admitted=\([0-9]*\) refusal=.*/\1/p' | tail -1)"
refusal="$(printf '%s\n' "$burst_out" | tail -1 | sed 's/.*refusal=//')"
if [ -z "${admitted:-}" ]; then
    fail login-rate-limit "could not parse burst output"
elif [ "$admitted" -lt 1 ]; then
    fail login-rate-limit "no attempt was admitted (admitted=$admitted)"
elif printf '%s' "$refusal" | grep -q "Too many login attempts"; then
    pass login-rate-limit "admitted=$admitted then refused: $(printf '%s' "$refusal" | cut -c1-90)"
else
    fail login-rate-limit "expected a rate-limit refusal after $admitted, got: $refusal"
fi

# 5. After the window passes, the same address is admitted again.
sleep "$WINDOW_WAIT"
rec="$(login "rateproberecover")"
rec_status="${rec%%$'\t'*}"
if [ "$rec_status" = "admitted" ]; then
    pass login-rate-recovery "admitted after ${WINDOW_WAIT}s"
else
    fail login-rate-recovery "expected admitted after ${WINDOW_WAIT}s, got: $rec"
fi

# 6. Offline play stays open: a fresh non-premium name is admitted offline.
off="$(login "probeofflineopen")"
off_status="${off%%$'\t'*}"
if [ "$off_status" = "admitted" ]; then
    pass offline-open "$off"
elif [ "$off_status" = "encryption" ]; then
    # A premium name would be offered encryption, not admitted — but this probe
    # name is not premium, so this is a surprise worth flagging.
    fail offline-open "a non-premium name was offered encryption"
else
    fail offline-open "expected admitted, got: $off"
fi

# 7. The operator account still gets in offline (via MCC, the real client).
# In hybrid mode `_alpha_01` is a premium name, so a fresh offline attempt is
# offered encryption, dropped, and admitted on MCC's automatic reconnect (the
# server's "recently dropped" fallback); the join scenario bundles that.
if join_out="$(bash "$RUN" join "$VER" 2>&1)"; then
    pass operator-offline-admitted "$join_out"
else
    fail operator-offline-admitted "$join_out"
fi

exit "$FAILED"
