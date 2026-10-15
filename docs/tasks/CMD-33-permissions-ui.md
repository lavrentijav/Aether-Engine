# CMD-33 — Plugin: permissions UI (LuckPerms-style)

Status: 📋 task brief (CMD0) · size: L · tier: native plugin (N) · plugin: `plugins/permissions`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-02`

## Split (CMD0 review)
| id | brief | contents | size |
|---|---|---|---|
| CMD-33a | `docs/tasks/CMD-33a-permissions-nodes.md` | `/lp user|group … permission` and `parent` commands | M |
| CMD-33b | `docs/tasks/CMD-33b-permissions-meta-editor.md` | meta (prefix/suffix/weight), tracks, web editor, sync/verbose | M |

## Goal
A management UI over the core permission groups/model (CMD-02): view/edit a player's or group's
nodes, groups, inheritance, meta and per-world context, tracks, and a web editor. The core keeps
the model; this plugin owns `/lp` and the UI (it is optional and not hot-path).

## Commands (LuckPerms reference; `lp` = `luckperms`)
| command | syntax | node |
|---|---|---|
| user | `/lp user <user> info\|parent set\|add\|remove\|permission set\|unset\|check\|list` | `luckperms.user.*` |
| group | `/lp group <group> info\|permission set\|unset\|list\|parent ...` | `luckperms.group.*` |
| create/delete | `/lp creategroup <name>` / `/lp deletegroup <name>` | `luckperms.creategroup` / `luckperms.deletegroup` |
| meta | `/lp user\|group <target> meta info\|set\|add\|remove\|settemp ...` (prefix/suffix/weight) | `luckperms.meta.*` |
| tracks | `/lp track <track> info\|create\|delete\|append\|insert\|remove` | `luckperms.track.*` |
| context | `/lp user <user> permission set <node> true world=<world>` | `luckperms.context.*` |
| editor | `/lp editor` → one-time web URL | `luckperms.editor` |
| sync/verbose | `/lp sync`, `/lp verbose on\|off` | `luckperms.sync` / `luckperms.verbose` |
| misc | `/lp info`, `/lp bulkupdate`, `/lp import`/`export` | `luckperms.info` etc. |

Op level 4 by default (or a configurable `luckperms` admin node). Tab-completion of users/groups/
nodes/groups is expected. The web editor uses the net capability (PA3) to serve a token URL; it must
work when PA3 is granted and be disabled otherwise.

## Files
- `plugins/permissions/` (skeleton already in the worktree): `plugin.toml`, `src/lib.rs`,
  `Cargo.toml`, README.
- reads/writes through the host `permissions` service or a host import added in CMD-02.

## Edge cases to test explicitly
- set/unset/check a node; a temporary node expires and the previous value returns.
- parent add/remove; deleting a group in use is refused or reassigns members (match LuckPerms).
- meta prefix/suffix/weight reflect in chat (CMD-40 depends on it).
- context-specific node, then a global node; the context one wins in that world.
- an invalid node name is rejected by the core (CMD-01); a weight that is not an integer errors.
- the editor URL is one-time and expires; disabled without PA3.
- a concurrent edit from the editor and the command line cannot lose data (ledger/transaction).

## Done criteria
- Plugin builds against the P1 host; conformance tests for the command surface; README lists the
  commands and links CMD-33.
- fmt / clippy `-D warnings` / `cargo-locked test` for the plugin workspace.
