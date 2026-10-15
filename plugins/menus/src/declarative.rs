//! Declarative menus: `menus/*.toml` files loaded at `init` and on `/menus
//! reload`.
//!
//! The schema is DeluxeMenus-shaped: a title, a layout, optional open
//! commands and permission, and a list of items. Each item names a material
//! and carries display overrides, optional view/click conditions and an
//! ordered action list. Conditions and actions are written as small,
//! audit-friendly strings, not as code:
//!
//! ```toml
//! name = "shop"
//! title = "&6Shop"
//! layout = "chest6"
//! permission = "shop.use"
//! open_commands = ["shop"]
//!
//! [[item]]
//! slot = 0
//! material = "minecraft:emerald"
//! name = "&aBuy a sword"
//! lore = ["&7Costs &e100"]
//! click_condition = "balance>=100"
//! actions = ["[economy] take 100", "[give] minecraft:diamond_sword 1", "[close]"]
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use aether_plugin::menus::{MenuAction, MenuCondition, MenuDefinition, MenuItem, MenuLayout};
use aether_plugin::{HostBackend, LogLevel, NativeContext};
use serde::Deserialize;

/// The operator-config key naming the directory, relative to the server's
/// working directory.
const DEFAULT_DIR: &str = "plugins/menus/menus";

/// The configured menus directory, or the default.
pub fn dir_from_config(config: &std::collections::BTreeMap<String, String>) -> String {
    config
        .get("menus_dir")
        .cloned()
        .unwrap_or_else(|| DEFAULT_DIR.to_string())
}

/// A menu file's on-disk shape.
#[derive(Debug, Deserialize)]
struct FileMenu {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    layout: Option<String>,
    #[serde(default)]
    rows: Option<u8>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    open_commands: Vec<String>,
    #[serde(default, rename = "item")]
    items: Vec<FileItem>,
}

#[derive(Debug, Deserialize)]
struct FileItem {
    slot: u32,
    /// The item name. `material` is the DeluxeMenus spelling; `item` is
    /// accepted too.
    #[serde(default, alias = "item")]
    material: String,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    lore: Vec<String>,
    #[serde(default)]
    glint: bool,
    #[serde(default)]
    model: Option<i32>,
    #[serde(default)]
    view_condition: Option<String>,
    #[serde(default)]
    click_condition: Option<String>,
    #[serde(default)]
    actions: Vec<String>,
}

/// Load every declarative menu the operator configured. Missing directory,
/// unreadable file or a bad schema is logged and skipped, never fatal: a
/// broken menu file must not stop the server or the other menus.
pub fn load(ctx: &NativeContext) -> Vec<MenuDefinition> {
    load_dir(&dir_from_config(ctx.config), ctx.backend)
}

/// Read and parse every `*.toml` under `dir`. This does blocking filesystem
/// I/O, so it is only ever called at `init` or from the reload worker thread —
/// never on the tick thread.
pub fn load_dir(dir: &str, backend: &Arc<dyn HostBackend>) -> Vec<MenuDefinition> {
    let log = |level: LogLevel, message: String| backend.log("menus", level, &message);
    let dir = PathBuf::from(dir);
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect(),
        Err(e) => {
            log(
                LogLevel::Info,
                format!("menus: no declarative directory {} ({e})", dir.display()),
            );
            return Vec::new();
        }
    };
    files.sort();
    let mut out = Vec::new();
    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_str(&text) {
                Ok(def) => out.push(def),
                Err(e) => log(LogLevel::Warn, format!("menus: {}: {e}", path.display())),
            },
            Err(e) => log(
                LogLevel::Warn,
                format!("menus: cannot read {}: {e}", path.display()),
            ),
        }
    }
    out
}

/// Parse one menu file. Public within the crate for the tests.
pub(crate) fn parse_str(text: &str) -> Result<MenuDefinition, String> {
    let file: FileMenu = toml::from_str(text).map_err(|e| e.to_string())?;
    from_file(file)
}

fn from_file(file: FileMenu) -> Result<MenuDefinition, String> {
    if file.name.trim().is_empty() {
        return Err("menu has no name".to_string());
    }
    let layout = match (&file.layout, file.rows) {
        (Some(l), _) => MenuLayout::parse(l).ok_or_else(|| format!("unknown layout `{l}`"))?,
        (None, Some(rows)) => {
            MenuLayout::from_rows(rows).ok_or_else(|| format!("unsupported row count {rows}"))?
        }
        (None, None) => MenuLayout::Chest3,
    };
    let mut items = Vec::with_capacity(file.items.len());
    for (i, it) in file.items.into_iter().enumerate() {
        items.push(from_item(i, it)?);
    }
    Ok(MenuDefinition {
        name: file.name,
        title: file.title.unwrap_or_default(),
        layout,
        items,
        open_commands: file.open_commands,
        permission: file.permission,
    })
}

fn from_item(index: usize, it: FileItem) -> Result<MenuItem, String> {
    if it.material.trim().is_empty() {
        return Err(format!("item {index} has no material"));
    }
    let item = if it.material.contains(':') {
        it.material
    } else {
        format!("minecraft:{}", it.material)
    };
    let view_condition = match it.view_condition {
        Some(s) => Some(parse_condition(&s).ok_or_else(|| format!("bad view_condition `{s}`"))?),
        None => None,
    };
    let click_condition = match it.click_condition {
        Some(s) => Some(parse_condition(&s).ok_or_else(|| format!("bad click_condition `{s}`"))?),
        None => None,
    };
    let mut actions = Vec::with_capacity(it.actions.len());
    for a in &it.actions {
        actions.push(parse_action(a).ok_or_else(|| format!("bad action `{a}`"))?);
    }
    Ok(MenuItem {
        slot: it.slot,
        item,
        count: it.count.unwrap_or(1),
        name: it.name,
        lore: it.lore,
        glint: it.glint,
        model: it.model,
        view_condition,
        click_condition,
        actions,
    })
}

/// Parse a `[tag] args` action line.
pub(crate) fn parse_action(s: &str) -> Option<MenuAction> {
    let s = s.trim();
    let rest = s.strip_prefix('[')?;
    let (tag, args) = rest.split_once(']')?;
    let args = args.trim();
    Some(match tag.trim().to_ascii_lowercase().as_str() {
        "command" | "player" | "cmd" => MenuAction::Command(args.to_string()),
        "console" | "server" => MenuAction::Console(args.to_string()),
        "message" | "msg" => MenuAction::Message(args.to_string()),
        "open" => MenuAction::Open(args.to_string()),
        "close" => MenuAction::Close,
        "sound" => MenuAction::Sound(args.to_string()),
        "back" => MenuAction::Back,
        "economy" | "econ" => {
            // `[economy] take 100`
            let (op, amount) = args.split_once(char::is_whitespace)?;
            if !op.eq_ignore_ascii_case("take") {
                return None;
            }
            MenuAction::EconomyTake(amount.trim().parse().ok()?)
        }
        "give" => {
            // `[give] minecraft:diamond 8`
            let (item, count) = args.rsplit_once(char::is_whitespace)?;
            let count: u32 = count.trim().parse().ok()?;
            MenuAction::Give {
                item: item.trim().to_string(),
                count,
            }
        }
        _ => return None,
    })
}

/// Parse a condition string. Supported forms:
///
/// * `permission:<node>`
/// * `group:<name>`
/// * `balance>=<amount>` (needs the economy service)
/// * `<placeholder> == <value>` and `<placeholder> >= <number>`
pub(crate) fn parse_condition(s: &str) -> Option<MenuCondition> {
    let s = s.trim();
    if let Some(node) = s.strip_prefix("permission:") {
        return (!node.is_empty()).then(|| MenuCondition::Permission(node.trim().to_string()));
    }
    if let Some(group) = s.strip_prefix("group:") {
        return (!group.is_empty()).then(|| MenuCondition::InGroup(group.trim().to_string()));
    }
    if let Some(amount) = s.strip_prefix("balance>=") {
        return amount
            .trim()
            .parse()
            .ok()
            .map(MenuCondition::BalanceAtLeast);
    }
    if let Some((lhs, rhs)) = s.split_once(">=") {
        let value: i64 = rhs.trim().parse().ok()?;
        return Some(MenuCondition::AtLeast {
            placeholder: lhs.trim().to_string(),
            value,
        });
    }
    if let Some((lhs, rhs)) = s.split_once("==") {
        return Some(MenuCondition::Equals {
            placeholder: lhs.trim().to_string(),
            value: rhs.trim().to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_menu_file_parses() {
        let text = r#"
name = "shop"
title = "&6Shop"
layout = "chest6"
permission = "shop.use"
open_commands = ["shop", "store"]

[[item]]
slot = 13
material = "emerald"
name = "&aBuy"
lore = ["line one", "line two"]
glint = true
model = 7
view_condition = "permission:shop.buy"
click_condition = "balance>=100"
actions = ["[economy] take 100", "[give] diamond_sword 1", "[close]"]
"#;
        let def = parse_str(text).unwrap();
        assert_eq!(def.name, "shop");
        assert_eq!(def.title, "&6Shop");
        assert_eq!(def.layout, MenuLayout::Chest6);
        assert_eq!(def.permission.as_deref(), Some("shop.use"));
        assert_eq!(def.open_commands, vec!["shop", "store"]);
        assert_eq!(def.items.len(), 1);
        let it = &def.items[0];
        assert_eq!(it.slot, 13);
        assert_eq!(it.item, "minecraft:emerald");
        assert_eq!(it.name.as_deref(), Some("&aBuy"));
        assert_eq!(it.lore.len(), 2);
        assert!(it.glint);
        assert_eq!(it.model, Some(7));
        assert_eq!(
            it.view_condition,
            Some(MenuCondition::Permission("shop.buy".to_string()))
        );
        assert_eq!(it.click_condition, Some(MenuCondition::BalanceAtLeast(100)));
        assert_eq!(
            it.actions,
            vec![
                MenuAction::EconomyTake(100),
                MenuAction::Give {
                    item: "diamond_sword".to_string(),
                    count: 1
                },
                MenuAction::Close,
            ]
        );
    }

    #[test]
    fn rows_one_through_six_pick_the_chest_layout() {
        for (rows, layout) in [
            (1, MenuLayout::Chest1),
            (2, MenuLayout::Chest2),
            (3, MenuLayout::Chest3),
            (4, MenuLayout::Chest4),
            (5, MenuLayout::Chest5),
            (6, MenuLayout::Chest6),
        ] {
            let text = format!("name=\"a\"\nrows={rows}\n[[item]]\nslot=0\nmaterial=\"stone\"\n");
            assert_eq!(parse_str(&text).unwrap().layout, layout, "{rows} rows");
        }
        let bad = "name=\"a\"\nrows=7\n[[item]]\nslot=0\nmaterial=\"stone\"\n";
        assert!(parse_str(bad).is_err());
    }

    #[test]
    fn a_nameless_menu_file_is_refused() {
        assert!(parse_str("title=\"x\"\n").is_err());
    }

    #[test]
    fn actions_parse_every_form() {
        assert_eq!(
            parse_action("[command] buy sword"),
            Some(MenuAction::Command("buy sword".to_string()))
        );
        assert_eq!(
            parse_action("[CONSOLE]  say hi"),
            Some(MenuAction::Console("say hi".to_string()))
        );
        assert_eq!(
            parse_action("[message] &aHello"),
            Some(MenuAction::Message("&aHello".to_string()))
        );
        assert_eq!(
            parse_action("[open] other"),
            Some(MenuAction::Open("other".to_string()))
        );
        assert_eq!(parse_action("[close]"), Some(MenuAction::Close));
        assert_eq!(parse_action("[back]"), Some(MenuAction::Back));
        assert_eq!(
            parse_action("[economy] take 250"),
            Some(MenuAction::EconomyTake(250))
        );
        assert_eq!(
            parse_action("[give] minecraft:diamond 8"),
            Some(MenuAction::Give {
                item: "minecraft:diamond".to_string(),
                count: 8
            })
        );
        // A malformed line is refused rather than run as something else.
        assert_eq!(parse_action("command buy"), None);
        assert_eq!(parse_action("[economy] give 5"), None);
        assert_eq!(parse_action("[nope] x"), None);
    }

    #[test]
    fn the_shipped_example_menus_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("menus");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).expect("example menus dir") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let def = parse_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!def.name.is_empty(), "{}", path.display());
            n += 1;
        }
        assert!(n >= 3, "expected the shipped example menus, found {n}");
    }

    #[test]
    fn conditions_parse_every_form() {
        assert_eq!(
            parse_condition("permission:shop.buy"),
            Some(MenuCondition::Permission("shop.buy".to_string()))
        );
        assert_eq!(
            parse_condition("group:staff"),
            Some(MenuCondition::InGroup("staff".to_string()))
        );
        assert_eq!(
            parse_condition("balance>=100"),
            Some(MenuCondition::BalanceAtLeast(100))
        );
        assert_eq!(
            parse_condition("%player% == Steve"),
            Some(MenuCondition::Equals {
                placeholder: "%player%".to_string(),
                value: "Steve".to_string()
            })
        );
        assert_eq!(
            parse_condition("%points% >= 10"),
            Some(MenuCondition::AtLeast {
                placeholder: "%points%".to_string(),
                value: 10
            })
        );
        assert_eq!(parse_condition("nonsense"), None);
    }
}
