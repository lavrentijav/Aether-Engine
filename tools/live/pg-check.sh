#!/usr/bin/env bash
# Read-only check that the PostgreSQL mirror behind fi2 exposes the
# `aether_events_named` view and that it returns readable rows for a session.
#
# The mirror is fed from the on-disk journal (see crates/aether-server/src/db.rs
# and docs for PV1). This script only ever runs SELECTs. It reads the mirror's
# connection string from /etc/aether.env on the test host over SSH, so it needs
# the fleet key (Host fi2 in ~/.ssh/config) and passwordless sudo there; when
# that is unavailable it exits 3 and prints how to run the query by hand.
#
# Usage: pg-check.sh [actor-uuid]
#   actor-uuid  restrict to one actor; default: the actor of the newest event,
#               which right after a scenario ran is your own session.
set -u

HOST="${MCC_DB_HOST:-fi2}"
ACTOR="${1:-}"

if [ -n "$ACTOR" ] && ! printf '%s' "$ACTOR" | grep -Eq '^[0-9a-fA-F-]{32,36}$'; then
    echo "pg-check: actor must be a UUID, got '$ACTOR'" >&2
    exit 2
fi

if [ -n "$ACTOR" ]; then
    ACTOR_SQL="WHERE actor = '$ACTOR'"
    LABEL="actor $ACTOR"
else
    ACTOR_SQL="WHERE actor = (SELECT actor FROM aether_events_named ORDER BY seq DESC LIMIT 1)"
    LABEL="actor of the newest event"
fi

SQL=$(cat <<EOF
\pset pager off
\echo === session ($LABEL) ===
SELECT actor, count(*) AS events, min(at) AS first, max(at) AS last
FROM aether_events_named $ACTOR_SQL GROUP BY actor;
\echo === readable rows (kind_name / reason_name / item_name) ===
SELECT at, seq, kind_name, x, y, z, item_name, item_count, reason_name, place_from, place_to
FROM aether_events_named $ACTOR_SQL ORDER BY seq DESC LIMIT 15;
\echo === kind coverage (unmatched kind_name would be NULL) ===
SELECT kind_name, count(*) FROM aether_events_named $ACTOR_SQL GROUP BY kind_name ORDER BY 2 DESC;
EOF
)

B64=$(printf '%s' "$SQL" | base64 -w0)
if ! ssh "$HOST" "echo $B64 | base64 -d | sudo -n bash -c 'set -a; . /etc/aether.env; set +a; psql \"\$AETHER_DATABASE_URL\" -f -'"; then
    echo "pg-check: could not query the mirror on $HOST" >&2
    echo "  (needs SSH access to $HOST and passwordless sudo; run the SELECTs by hand otherwise)" >&2
    exit 3
fi
