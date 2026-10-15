# V1 live scenarios — command notes (found with interactive MCC in tmux)
#
# Internal MCC commands need terrain/inventory handling (mcc-template.ini):
#   TerrainAndMovements = true   -> /move /dig /look /useblock
#   InventoryHandling   = true   -> /inventory /changeslot /useitem
#   EntityHandling      = true   -> entities
#
# send <cmd>  — a line starting with '/' goes to the server; unknown server
#               commands are forwarded, so use "/send /help" for server help.
# wait <secs> — sleep before the next step.
# expect <re> — regex that MUST appear in the ANSI-stripped log.
# reject <re> — regex that MUST NOT appear.
#
# Useful messages observed (used as per-step expects):
#   /gamemode creative           -> "is now in Creative mode"
#   /spawn                       -> "Teleported to spawn"
#   /tp x y z                    -> "Teleported to x y z"
#   /give <item> <n>             -> "Gave <n> <item>"
#   /inventory creativedelete N  -> "Requested to clear slot #N"
#   /inventory creativegive N I C-> "Requested <I> x<C> in slot #N"
#   /changeslot N                -> "Changed to slot N" ; N is 1..9 (hotbar).
#                                   "/changeslot 0" prints "Could not change slot".
#   /look up|down                -> "Looking Up" / "Looking Down"
#   /dig [x y z]                 -> "Attempting to dig block at ..." (out of
#                                   reach: "You are too far away from this block.")
#   /useblock x y z              -> "Useblock at (...)" ; opens containers too
#   /useitem                     -> "Used an item"
#   /inspect                     -> "Inspect mode ON"/"Inspect mode OFF"
#   /rollback <filters>          -> "rolled back N block(s)"
#   /effects                     -> "Active effects:" / "No active effects."
#   /audit                       -> "<n> item instance(s), <m> anomal(y/ies):"
#                                   (decodes every journal event, so it also
#                                    checks old journal formats still read)
#   /send /help                  -> CMD0 task catalog ("Commands not implemented
#                                   yet — /help <command> ..."), not the old
#                                   "History commands:" blurb.
#
# journal-upgrade.txt checks /inspect and /lookup against entries written before
# and after a server upgrade (old v3 journal + new v4). pg-check.sh (separate,
# read-only) checks the PostgreSQL aether_events_named view for a session.
