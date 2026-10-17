# Permissions plugin — `/lp` (CMD-33a)

Status: ✅ implemented (CMD-33a), **native tier** (in-process, CMD-00N). The permission **model**
(groups, inheritance, priority, context, persistence, wildcards) is the core's CMD-01/CMD-02 store;
this plugin is the LuckPerms-style management surface over the server's `permissions.admin@0.1`
service. Meta, tracks, sync and the web editor are CMD-33b.

- CMD task: **CMD-33a** — `docs/tasks/CMD-33a-permissions-nodes.md`
- Parent: **CMD-33** — `docs/tasks/CMD-33-permissions-ui.md`
- Store: `crates/aether-server/src/permissions.rs`, format in `docs/design/permissions.md`

## Commands
`/lp` is also reachable as `/luckperms` and `/perms`. Op level 4 is the command gate; every
subcommand then checks its `luckperms.*` node.

| command | syntax | permission |
|---|---|---|
| user info | `/lp user <user> info` | `luckperms.user.info` |
| user nodes | `/lp user <user> permission set <node> <true\|false> [world=<world>]` | `luckperms.user.permission.set` |
| | `/lp user <user> permission unset <node> [world=<world>]` | `.unset` |
| | `/lp user <user> permission check <node> [world=<world>]` | `.check` |
| | `/lp user <user> permission list` | `.list` |
| user parents | `/lp user <user> parent set\|add\|remove\|switch\|clear <group>` | `luckperms.user.parent` |
| group info | `/lp group <group> info` | `luckperms.group.info` |
| group nodes | `/lp group <group> permission set\|unset\|check\|list …` | `luckperms.group.permission.*` |
| group parents | `/lp group <group> parent set\|add\|remove\|switch\|clear <group>` | `luckperms.group.parent` |
| create/delete | `/lp creategroup <name>` / `/lp deletegroup <name>` | `luckperms.creategroup` / `.deletegroup` |
| info/help | `/lp info`, `/lp help` | `luckperms.info` |

`check` reports the value and the holder that decided it (`user <name>`, `group <id>` or `default`).
A stored node may be a wildcard: `*` grants everything, `a.b.*` every descendant. An exact node
beats a wildcard inside one holder; the core's group order decides across holders. A context token
(`world=overworld`) restricts the node to that world, and a context-specific node beats the global
one there.

`parent switch` has no tracks in CMD-33a, so it replaces the current parent/membership exactly as
`parent set` does. `deletegroup op` is refused; deleting another group removes its parent edges and
its members lose the group (LuckPerms behaviour). All successful writes are persisted immediately
and journalled as a `permissions` audit event (action + detail), so who changed what is recoverable.

## Tab-completion
`/lp` completion is answered by the plugin through `NativePlugin::on_complete`: online users plus
known store users, group ids, known node names, `true|false`, and `world=<current world>`.

## Capabilities
`log`, `players`, and the `permissions.admin@0.1` service it requires at load. There is no private
copy of the store: every read is a snapshot and every write is the core's copy-and-swap update.

## Notes
Prefix/suffix meta is read by the chat plugin (CMD-40) and is CMD-33b, along with tracks, temporary
nodes, sync/verbose and the web editor.
