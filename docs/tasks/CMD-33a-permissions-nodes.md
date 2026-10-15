# CMD-33a — Plugin: permissions nodes & parents

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/permissions`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-02`
Parent/umbrella: `docs/tasks/CMD-33-permissions-ui.md`

## Commands (LuckPerms reference; `lp` = `luckperms`)
| command | syntax | node |
|---|---|---|
| user | `/lp user <user> info\|permission set\|unset\|check\|list` | `luckperms.user.*` |
| group | `/lp group <group> info\|permission set\|unset\|list` | `luckperms.group.*` |
| parents | `/lp user\|group <t> parent set\|add\|remove\|switch\|clear [<group>]` | `luckperms.parent.*` |
| create/delete | `/lp creategroup <name>` / `/lp deletegroup <name>` | `luckperms.creategroup` / `.deletegroup` |
| info | `/lp info`, `/lp user <u> info` | `luckperms.info` |

Op level 4 by default. Tab-completion of users/groups/nodes.

## Files
- `plugins/permissions/`: manifest, `src/`, README.
- reads/writes through the host `permissions` service added in CMD-02.

## Edge cases to test explicitly
- set/unset/check a node; a temporary node expires and the previous value returns.
- parent add/remove/switch; deleting a group in use is refused or reassigns members (match
  LuckPerms); deleting the built-in `op` group is refused.
- an invalid node name is rejected by the core (CMD-01).

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
