# Permissions — core store and on-disk format

Status: ✅ implemented (CMD-02, 2026-10-16). The model lives in the **core**
(`crates/aether-server/src/permissions.rs`); the LuckPerms-style `/lp` UI and web editor are a
separate native plugin (CMD-33). This document is the as-built record for the store, its
resolution and the file it persists to.

## Model

| concept | summary |
|---|---|
| **level** | vanilla `0..=4`, clamped on the way in. |
| **node** | lower-case, dot-separated `[a-z0-9_.-]`; a malformed node is a deny. A stored node may be a wildcard: `*`, or a trailing `.*` (`aether.command.*`). |
| **default** | per node: `allow` / `deny` / `op` (unknown defaults to `op`, failing closed). |
| **group** | id, `level`, `priority`, `parents`, explicit `nodes`, `meta`, `members`. |
| **player** | explicit `nodes` and `meta` (its own prefix/suffix/weight). |
| **context** | a dimension/world key on a node; absent means "everywhere". |

A subject is a player name plus optional connection address. A check resolves **explicit nodes**
first — the player's, then every group the player reaches by inheritance — and only then the
node's default. The holders are ordered `(priority desc, depth asc, id asc)`, where `depth` is `0`
for a direct group and increases with inheritance; the first holder with a node for the key wins.
The order is total, so a resolution is reproducible. An explicit grant/deny never lowers a
command's op floor.

Context is resolved **within** a holder: a node with the matching context beats that holder's
global node, and a node for another context is skipped. Callers with no world pass `None`.

Within a holder an **exact** node beats a wildcard. Among wildcards the longest matching prefix
wins, and at equal length a context-specific pattern beats a global one; `*` is the least specific.
Across holders the normal order applies, so a player's deny still beats a group's wildcard grant.

A user's **direct groups** are the groups whose member list names them. `/lp user <u> parent
set|clear` therefore rewrites those memberships atomically; a group's **parents** are its own
`parents` list and `/lp group <g> parent …` validates existence, de-duplicates and rejects a cycle
before changing the store. Deleting a group removes it from every parent list and from its members'
effective groups (as LuckPerms does); the built-in `op` group cannot be deleted.

## Persistence

The store is written to `<world_dir>/permissions.toml`, atomically (temp file + rename), whenever a
write goes through the process-wide API. It is loaded **before** `config.operators` is seeded, so
the add-only rule holds across restarts.

```toml
[nodes]
# node -> default
aether.command.ban = "op"
worldguard.flag.pvp = "deny"

[groups.op]
id = "op"
level = 4
builtin = true

[groups.op.members]
root = {}
admin = { addr = "10.0.0.7" }

[groups.moderator]
id = "moderator"
level = 2
builtin = false
priority = 10
parents = ["staff"]

[groups.moderator.nodes]
"aether.command.kick" = true
"worldguard.flag.pvp#overworld" = false

[groups.moderator.meta]
prefix = "[mod] "
weight = 10

[players.alice.nodes]
"aether.command.kick" = false

[players.alice.meta]
suffix = " the brave"
```

A node key is `node` for a global node and `node#context` for a context-specific one. A node can
never contain `#`, so the split is unambiguous. `players` keys are lower-cased names.

`op` is rebuilt and forced to `level = 4`, `builtin = true` on every load: a hand-edited file
cannot weaken it, and it cannot be deleted. An inheritance cycle (or a self-parent) is rejected at
load with the cycle path, never recursed forever.

## Hot path

The store is an immutable snapshot behind an `Arc`. A reader clones the `Arc` (a brief read lock,
never held across a check) and resolves; a writer clones the whole store off the tick, edits the
copy and swaps the pointer. A concurrent check therefore sees either the old or the new snapshot,
never a half-applied edit, and the hot path takes no `Mutex`.
