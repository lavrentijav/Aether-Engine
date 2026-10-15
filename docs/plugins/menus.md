# Menus

> **Read this in other languages:** [Русский 🇷🇺](menus.ru.md)

The shared chest-menu host. Shops, kits, warps, settings and moderation all
need clickable inventory menus; instead of fifteen half-broken ones, the native
plugin `plugins/menus` (tier **N**) provides one. It **exports** the
`aether:menus@0.1` service — any native or WASM dependent opens a menu with one
call — and **loads** declarative `menus/*.toml` files for the common case where
no code is needed at all.

**Menus are not inventories.** A menu window is server-invented: its slots are
stand-ins for actions, not items. The core (see
`crates/aether-server/src/game/window.rs`) intercepts *every* click on a menu —
left, right, shift, number-key swap, offhand swap, drop, drag, double-click
collect, creative middle-click — delivers it to the plugin that owns the menu,
and resyncs the client. No click type can move a menu item into or out of a
player's real inventory. A click on a menu that has already closed (a race or a
resync) is ignored, never applied to whatever window replaced it.

## Opening a menu from a plugin

A native dependent resolves the service from the registry and calls it:

```rust
use aether_plugin::menus::{MenuAction, MenuBuilder, MenuLayout, menus};

let Some(menus) = menus(ctx.services) else { return };
let def = MenuBuilder::new("shop")
    .title("&6Shop")
    .layout(MenuLayout::Chest6)
    .button(0, "minecraft:emerald", "&aBuy a sword",
            vec![MenuAction::Command("buy sword".into()),
                 MenuAction::Close])
    .filler("minecraft:gray_stained_glass_pane")
    .build();
menus.open(caller_id, &player, &def)?;
```

A WASM dependent does the same through the `menus` import in the `dependent`
world: one host call, forwarded to the native provider in-process. The
interface is `aether:menus@0.1` (`crates/aether-plugin/wit-menus/`).

`open` replaces the player's current window; `update` replaces one slot of an
open menu (or clears it with `None`); `close` closes it. A live update resends
the whole window — a menu is small, and a partial update is one more thing the
client can disagree about.

## Declarative menus

Files under `plugins/menus/menus/*.toml` (the directory is configurable with
`[plugins.menus] menus_dir = "..."` in the server config) are loaded at startup
and on `/menus reload` (op level 2). A broken file is logged and skipped, never
fatal.

```toml
name = "shop"
title = "&6Shop"
layout = "chest6"           # or rows = 6
permission = "shop.use"
open_commands = ["shop"]    # /shop opens this menu, permission-checked

[[item]]
slot = 0
material = "minecraft:emerald"
name = "&aBuy a sword"
lore = ["&7Costs &e100"]
glint = true
view_condition = "permission:shop.buy"
click_condition = "balance>=100"
actions = ["[economy] take 100", "[give] minecraft:diamond_sword 1", "[close]"]
```

* **Layouts:** the generic chest menus `chest1` … `chest6` (9 to 54 slots,
  1–6 nine-wide rows; `chest`/`chest3` is 27 and `large_chest`/`chest6` is 54),
  `dispenser` (9) and `hopper` (5). `rows = 1..=6` picks the matching chest
  size.
* **Item:** `material`/`item`, `count`, `name`, `lore`, `glint`, `model`.
  A material without `minecraft:` gets the prefix; an unknown item is drawn as
  a barrier rather than vanishing.
* **Conditions** (optional `view_condition` and `click_condition`):
  * `permission:<node>` — the player holds the node (via `sdk.permissions`),
  * `group:<name>` — the player is in the group,
  * `balance>=<amount>` — balance at least (economy service),
  * `<placeholder> == <value>` and `<placeholder> >= <number>`.
* **Actions**, run in order:
  * `[command] <cmd>` — run as the clicking player (permission-checked),
  * `[console] <cmd>` — run on the console,
  * `[message] <text>` — send to the player,
  * `[open] <menu>` — open another menu by name,
  * `[close]`, `[back]` — close, or return to the previous menu,
  * `[sound] <name>`, `[economy] take <amount>`, `[give] <item> <count>`.

`/menu <name>` opens any menu by name. Each file's `open_commands` are
registered with the core command registry, so they are dispatched and
permission-checked like any command — a core name is refused, never shadowed.

## Placeholders

`%player%` expands to the clicking player's name wherever a menu string is used
(titles, item names/lore, messages, commands). Plugin-provided placeholders are
an optional integration not yet wired; an unknown placeholder is left verbatim
rather than blanked.

## Building on this

* **Security** is the core's, not the plugin's: a menu click runs only the
  actions the operator wrote, through host mechanisms (the command registry,
  chat, `[give]`), and never manipulates the inventory directly.
* **Optional integrations** (`economy`, `sound`, `players`) are resolved as
  services and degrade honestly: `balance` conditions fail closed and
  `[economy]`/`[sound]` are inert until those services exist.

See [KNOWN_ISSUES §20](../KNOWN_ISSUES.md) for the current limits (window
shapes, glint/model rendering, pre-1.20.2 codecs).
