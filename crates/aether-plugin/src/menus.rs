//! The `menus` service: inventory (chest) menus any plugin can open.
//!
//! Menus are the one UI every plugin needs — shops, kits, warps, settings,
//! moderation — so they live once, in the native `plugins/menus` host, behind
//! a service any native or WASM dependent calls. A later section of this
//! module defines the neutral menu model; the provider trait is what the
//! native menus plugin implements; [`register_menus`]/[`menus`] publish and
//! resolve it in the shared [`crate::service::ServiceRegistry`].
//!
//! **Menus are not inventories.** The core renders them as a window (see
//! `aether_server::game::window::open_menu`) whose slots are virtual: every
//! click is intercepted, delivered to the plugin that opened the menu, and the
//! client is resynced. No click type — shift, number-key swap, drag, offhand,
//! drop, double-click collect, creative middle-click — can move a menu item
//! into a player's real inventory.

use std::sync::Arc;

use crate::bindings::aether::plugin::types::Player;
use crate::service::ServiceRegistry;

/// The shape of a menu window. Only the shapes whose protocol menu registry
/// id the generated tables carry are offered; a menu wanting another size
/// picks the nearest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MenuLayout {
    /// A single chest, 27 slots (3 rows).
    #[default]
    Chest3,
    /// A double chest, 54 slots (6 rows).
    Chest6,
    /// A dispenser/dropper, 9 slots (a 3×3 grid).
    Dispenser,
    /// A hopper, 5 slots.
    Hopper,
}

impl MenuLayout {
    /// How many menu slots the window has.
    pub fn slot_count(self) -> u32 {
        match self {
            MenuLayout::Chest3 => 27,
            MenuLayout::Chest6 => 54,
            MenuLayout::Dispenser => 9,
            MenuLayout::Hopper => 5,
        }
    }

    /// The layout a `rows` value names, or `None` for an unsupported size.
    /// Rows are `1..=6`; only 3 and 6 have a chest menu id, so a 1/2/4/5-row
    /// menu is refused rather than silently drawn at the wrong size.
    pub fn from_rows(rows: u8) -> Option<MenuLayout> {
        match rows {
            3 => Some(MenuLayout::Chest3),
            6 => Some(MenuLayout::Chest6),
            _ => None,
        }
    }

    /// The spelling used in a declarative `menus/*.toml`.
    pub fn parse(s: &str) -> Option<MenuLayout> {
        match s.to_ascii_lowercase().as_str() {
            "chest" | "chest3" | "3" => Some(MenuLayout::Chest3),
            "large_chest" | "large-chest" | "chest6" | "6" => Some(MenuLayout::Chest6),
            "dispenser" => Some(MenuLayout::Dispenser),
            "hopper" => Some(MenuLayout::Hopper),
            _ => None,
        }
    }
}

/// One thing an item click can do. The set is deliberately small and closed:
/// a declarative menu can only do what an operator can audit, and every action
/// runs through a host mechanism (the command registry, chat, the economy
/// service) rather than touching the world directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Run `command` **as the clicking player** (permission-checked).
    Command(String),
    /// Run `command` on the console.
    Console(String),
    /// Send `text` to the clicking player (with placeholders expanded).
    Message(String),
    /// Open another menu by name, replacing this one.
    Open(String),
    /// Close the menu.
    Close,
    /// Play a sound by name.
    Sound(String),
    /// Take `amount` from the player's balance (needs the economy service).
    EconomyTake(i64),
    /// Give `count` of `item` to the player.
    Give { item: String, count: u32 },
    /// Go back to the previous menu in this player's stack, if any.
    Back,
}

/// A condition that decides whether a slot/branch is shown or an action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuCondition {
    /// The player holds permission `node`.
    Permission(String),
    /// The player's balance is at least `amount`.
    BalanceAtLeast(i64),
    /// The player is in the group `group`.
    InGroup(String),
    /// A placeholder expands to exactly `value`.
    Equals { placeholder: String, value: String },
    /// A placeholder expands to a value `>=` this number.
    AtLeast { placeholder: String, value: i64 },
}

/// One clickable slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// The window slot (`0..slot_count`).
    pub slot: u32,
    /// The `minecraft:`-qualified item name shown (empty for none).
    pub item: String,
    pub count: u32,
    /// The item's display name, if any.
    pub name: Option<String>,
    pub lore: Vec<String>,
    pub glint: bool,
    pub model: Option<i32>,
    /// A condition the player must satisfy to see the item.
    pub view_condition: Option<MenuCondition>,
    /// A condition the player must satisfy for a click to act.
    pub click_condition: Option<MenuCondition>,
    /// What a click does, in order.
    pub actions: Vec<MenuAction>,
}

impl MenuItem {
    /// A plain filler item: no actions, a name, no condition.
    pub fn filler(item: &str, name: &str) -> MenuItem {
        MenuItem {
            slot: 0,
            item: item.to_owned(),
            count: 1,
            name: Some(name.to_owned()),
            lore: Vec::new(),
            glint: false,
            model: None,
            view_condition: None,
            click_condition: None,
            actions: Vec::new(),
        }
    }
}

/// A whole menu: a title, a shape and its items. Produced by the builder or
/// loaded from a declarative file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuDefinition {
    /// The menu's name, for `[open] other_menu` and `/menu <name>`.
    pub name: String,
    pub title: String,
    pub layout: MenuLayout,
    pub items: Vec<MenuItem>,
    /// Commands that open this menu (declarative only); the menus plugin
    /// registers them. Empty for a menu built in code.
    pub open_commands: Vec<String>,
    /// Who may run those commands / open the menu (`None` = everyone).
    pub permission: Option<String>,
}

/// A fluent builder for a [`MenuDefinition`], for a dependent that opens a
/// menu in code. A declarative `menus/*.toml` produces the same value; the
/// builder is just the ergonomic path.
///
/// ```ignore
/// let def = MenuBuilder::new("shop")
///     .title("&6Shop")
///     .layout(MenuLayout::Chest6)
///     .button(0, "minecraft:emerald", "Buy", vec![MenuAction::Command("buy sword".into())])
///     .filler("minecraft:gray_stained_glass_pane")
///     .build();
/// menus.open(caller, &player, &def)?;
/// ```
pub struct MenuBuilder {
    def: MenuDefinition,
}

impl MenuBuilder {
    /// Start a menu named `name`.
    pub fn new(name: &str) -> MenuBuilder {
        MenuBuilder {
            def: MenuDefinition {
                name: name.to_owned(),
                layout: MenuLayout::Chest3,
                ..MenuDefinition::default()
            },
        }
    }

    /// Set the window title (with `&`-style colour codes).
    pub fn title(mut self, title: &str) -> MenuBuilder {
        self.def.title = title.to_owned();
        self
    }

    /// Set the window shape.
    pub fn layout(mut self, layout: MenuLayout) -> MenuBuilder {
        self.def.layout = layout;
        self
    }

    /// Add a fully specified item.
    pub fn item(mut self, item: MenuItem) -> MenuBuilder {
        self.def.items.push(item);
        self
    }

    /// Add a clickable button: `item` at `slot`, shown with `name`, running
    /// `actions` on a click.
    pub fn button(
        self,
        slot: u32,
        item: &str,
        name: &str,
        actions: Vec<MenuAction>,
    ) -> MenuBuilder {
        self.item(MenuItem {
            slot,
            item: item.to_owned(),
            count: 1,
            name: Some(name.to_owned()),
            lore: Vec::new(),
            glint: false,
            model: None,
            view_condition: None,
            click_condition: None,
            actions,
        })
    }

    /// Add a filler: a decorative item with no actions, into every empty slot
    /// not already filled.
    pub fn filler(self, item: &str) -> MenuBuilder {
        let mut builder = self;
        let used: Vec<u32> = builder.def.items.iter().map(|i| i.slot).collect();
        let slots = builder.def.layout.slot_count();
        for slot in 0..slots {
            if !used.contains(&slot) {
                builder = builder.item(MenuItem::filler(item, " ").at(slot));
            }
        }
        builder
    }

    /// Add a border of `item` around the edge of the window.
    pub fn border(self, item: &str) -> MenuBuilder {
        let mut builder = self;
        let slots = builder.def.layout.slot_count();
        let rows = slots / 9;
        let used: Vec<u32> = builder.def.items.iter().map(|i| i.slot).collect();
        let mut edge = Vec::new();
        for slot in 0..slots {
            let row = slot / 9;
            let col = slot % 9;
            if (row == 0 || row + 1 == rows || col == 0 || col == 8) && !used.contains(&slot) {
                edge.push(slot);
            }
        }
        for slot in edge {
            builder = builder.item(MenuItem::filler(item, " ").at(slot));
        }
        builder
    }

    /// Finish the menu.
    pub fn build(self) -> MenuDefinition {
        self.def
    }
}

impl MenuItem {
    /// Move this item to `slot`.
    pub fn at(mut self, slot: u32) -> MenuItem {
        self.slot = slot;
        self
    }

    /// Chain one more action onto this item.
    pub fn action(mut self, action: MenuAction) -> MenuItem {
        self.actions.push(action);
        self
    }

    /// Set the item's lore lines.
    pub fn lore(mut self, lines: Vec<String>) -> MenuItem {
        self.lore = lines;
        self
    }

    /// Set the display name.
    pub fn named(mut self, name: &str) -> MenuItem {
        self.name = Some(name.to_owned());
        self
    }
}

/// The provider side of the `menus` service: what the native menus plugin
/// implements and a dependent calls.
///
/// `caller` is the id of the dependent that is opening the menu, so the
/// provider keys per-caller state and can drop it when that plugin is disabled.
pub trait MenusService: Send + Sync {
    /// Open `menu` for `player`, replacing any window they have open.
    fn open(&self, caller: &str, player: &Player, menu: &MenuDefinition) -> Result<(), String>;
    /// Replace one slot of the named menu this caller currently has open for
    /// `player`. `item` empty (`None`) clears the slot.
    fn update(
        &self,
        caller: &str,
        player: &Player,
        menu: &str,
        slot: u32,
        item: Option<MenuItem>,
    ) -> Result<(), String>;
    /// Close the named menu this caller opened for `player`.
    fn close(&self, caller: &str, player: &Player, menu: &str) -> Result<(), String>;
}

/// Erasure wrapper so the trait object can live in the untyped registry.
pub struct MenusProvider(pub Arc<dyn MenusService>);

/// The service name under which menus are registered.
pub const MENUS_SERVICE: &str = "menus";
/// The service version.
pub const MENUS_VERSION: &str = "0.1";

/// Publish a menus provider.
pub fn register_menus(
    registry: &ServiceRegistry,
    service: Arc<dyn MenusService>,
) -> Result<(), String> {
    registry.register(
        MENUS_SERVICE,
        MENUS_VERSION,
        Arc::new(MenusProvider(service)),
    )
}

/// Resolve the menus provider, if one is registered.
pub fn menus(registry: &ServiceRegistry) -> Option<Arc<dyn MenusService>> {
    registry
        .get::<MenusProvider>(MENUS_SERVICE, MENUS_VERSION)
        .map(|p| Arc::clone(&p.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_map_to_their_slot_counts() {
        assert_eq!(MenuLayout::Chest3.slot_count(), 27);
        assert_eq!(MenuLayout::Chest6.slot_count(), 54);
        assert_eq!(MenuLayout::Dispenser.slot_count(), 9);
        assert_eq!(MenuLayout::Hopper.slot_count(), 5);
    }

    #[test]
    fn only_three_and_six_rows_have_a_chest_menu() {
        assert_eq!(MenuLayout::from_rows(3), Some(MenuLayout::Chest3));
        assert_eq!(MenuLayout::from_rows(6), Some(MenuLayout::Chest6));
        for bad in [0, 1, 2, 4, 5, 7] {
            assert_eq!(MenuLayout::from_rows(bad), None, "{bad} rows");
        }
    }

    #[test]
    fn layout_names_parse_both_spellings() {
        assert_eq!(MenuLayout::parse("chest"), Some(MenuLayout::Chest3));
        assert_eq!(MenuLayout::parse("LARGE_CHEST"), Some(MenuLayout::Chest6));
        assert_eq!(MenuLayout::parse("hopper"), Some(MenuLayout::Hopper));
        assert_eq!(MenuLayout::parse("nope"), None);
    }

    #[test]
    fn a_registered_provider_resolves_by_version() {
        struct Noop;
        impl MenusService for Noop {
            fn open(&self, _: &str, _: &Player, _: &MenuDefinition) -> Result<(), String> {
                Ok(())
            }
            fn update(
                &self,
                _: &str,
                _: &Player,
                _: &str,
                _: u32,
                _: Option<MenuItem>,
            ) -> Result<(), String> {
                Ok(())
            }
            fn close(&self, _: &str, _: &Player, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let reg = ServiceRegistry::new();
        assert!(menus(&reg).is_none(), "no provider yet");
        register_menus(&reg, Arc::new(Noop)).unwrap();
        assert!(menus(&reg).is_some());
    }
}
