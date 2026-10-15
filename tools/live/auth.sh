#!/usr/bin/env bash
# V1 auth scenario: the login/admission layer introduced by AU1
# ([auth] mode = hybrid). Unlike the gameplay scenarios, these checks are
# about who the server lets in, so most of them drive a raw login probe
# (auth-probe.py) rather than the MCC client — an offline client cannot
# complete Mojang's encryption handshake, and the rate limit needs logins far
# faster than an MCC process can start.
#
# The account under test is the dedicated bot `_aether_bot1`
# (MCC_ACCOUNT, default), which has **no** Mojang profile: hybrid auth admits
# it offline directly. The owner's real premium account is no longer used.
#
# Checks (one PASS|FAIL line each):
#   bot-name-not-premium     api.mojang.com has no profile for the bot name,
#                            so the server never offers it encryption (404).
#   invalid-name-space       a name with a space is refused "Invalid username."
#   invalid-name-long        a 20-char name is refused "Invalid username."
#   invalid-name-short       a 2-char name is refused "Invalid username."
#   login-rate-limit         rapid reconnects hit the per-IP limit ("Too many
#                            login attempts…"); the count before refusal is
#                            reported.
#   login-rate-recovery      after the window, a login is admitted again.
#   offline-open             a fresh, non-premium name joins offline.
#   bot-admitted             the bot joins offline via MCC (the real client).
#
# Usage: auth.sh [mc_version]     (default 1.21.11; only for the MCC check)
#
# The rate-limit burst intentionally exhausts this client's per-IP budget, so
# the script waits out the server's login window (60 s) before measuring it and
# again before the recovery probe; the budget is then low enough for the later
# checks.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN="$HERE/run.sh"
PROBE="$HERE/auth-probe.py"
HOST="${MCC_HOST:-fi2.walk.boykisser.online}"
PORT="${MCC_PORT:-25565}"
PROTO="${MCC_PROTOCOL:-774}"      # 1.21.11
BOT="${MCC_ACCOUNT:-_aether_bot1}"
BURST_CAP="${AUTH_BURST_CAP:-25}"
WINDOW_WAIT="${AUTH_WINDOW_WAIT:-62}"   # > the server's 60 s login window
VER="${1:-1.21.11}"

FAILED=0
pass() { echo "PASS auth $1 $2"; }
fail() { echo "FAIL auth $1 $2"; FAILED=1; }

# One raw login; prints "<status>\t<detail>".
login() { python3 "$PROBE" login "$HOST" "$PORT" "$1" "$PROTO"; }

# 0. The bot name must not be a premium (Mojang) name, or a hybrid server
# would offer it encryption and an offline client could not answer. 404 (or
# 204) from the profile API means "not premium".
code="$(curl -s -m 10 -o /dev/null -w '%{http_code}' "https://api.mojang.com/users/profiles/minecraft/$BOT" || true)"
if [ "$code" = "404" ] || [ "$code" = "204" ]; then
    pass bot-name-not-premium "$BOT has no Mojang profile (HTTP $code)"
elif [ "$code" = "200" ]; then
    fail bot-name-not-premium "$BOT is a premium name (HTTP 200); pick another bot name (MCC_ACCOUNT)"
else
    fail bot-name-not-premium "could not check $BOT: HTTP $code (uncertain; proceed with care)"
fi

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

# 6. Offline play stays open: the bot is a fresh non-premium name and is
# admitted offline (not offered encryption).
off="$(login "$BOT")"
off_status="${off%%$'\t'*}"
if [ "$off_status" = "admitted" ]; then
    pass offline-open "$off"
elif [ "$off_status" = "encryption" ]; then
    fail offline-open "the non-premium bot was offered encryption"
else
    fail offline-open "expected admitted, got: $off"
fi

# 7. The bot still gets in via the real client (MCC). The join scenario is
# name-agnostic and runs as $MCC_ACCOUNT (default the bot).
if join_out="$(bash "$RUN" join "$VER" 2>&1)"; then
    pass bot-admitted "$join_out"
else
    fail bot-admitted "$join_out"
fi

exit "$FAILED"
