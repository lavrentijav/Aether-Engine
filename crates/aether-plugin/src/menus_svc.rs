//! The host side of `aether:menus@0.1`.
//!
//! A WASM dependent that imports the menus interface has each call land on the
//! generated host trait implemented here on [`crate::host::StoreData`], which
//! forwards it to the **provider** registered in the [`crate::service`]
//! registry — the native `plugins/menus` plugin. The caller's plugin id is
//! injected from the store, so the provider keys per-caller state.
//!
//! A native dependent skips all of this and resolves [`crate::menus::MenusService`]
//! directly from the registry; both paths reach the same provider.

use crate::bindings::aether::plugin::types::Player;
use crate::host::StoreData;
use crate::menus::{self as model, MenuAction, MenuDefinition, MenuItem, MenuLayout};

/// The generated host bindings for the menus interface.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: ["wit-menus"],
        world: "aether:menus/services",
    });
}

pub use bindings::aether::menus::menus::{Action, Definition, Item, Layout};
pub use bindings::Services;

fn layout(l: Layout) -> MenuLayout {
    match l {
        Layout::Chest3 => MenuLayout::Chest3,
        Layout::Chest6 => MenuLayout::Chest6,
        Layout::Dispenser => MenuLayout::Dispenser,
        Layout::Hopper => MenuLayout::Hopper,
    }
}

fn action(a: Action) -> MenuAction {
    match a {
        Action::Command(c) => MenuAction::Command(c),
        Action::Console(c) => MenuAction::Console(c),
        Action::Message(m) => MenuAction::Message(m),
        Action::Open(o) => MenuAction::Open(o),
        Action::Close => MenuAction::Close,
        Action::Sound(s) => MenuAction::Sound(s),
        Action::EconomyTake(n) => MenuAction::EconomyTake(n),
        Action::Give((item, count)) => MenuAction::Give { item, count },
        Action::Back => MenuAction::Back,
    }
}

fn item(i: Item) -> MenuItem {
    MenuItem {
        slot: i.slot,
        item: i.item,
        count: i.count,
        name: i.name,
        lore: i.lore,
        glint: i.glint,
        model: i.model,
        // Conditions are a declarative-file feature; a WASM dependent builds
        // its visibility logic itself and only sends the items to show.
        view_condition: None,
        click_condition: None,
        actions: i.actions.into_iter().map(action).collect(),
    }
}

fn definition(d: Definition) -> MenuDefinition {
    MenuDefinition {
        name: d.name,
        title: d.title,
        layout: layout(d.layout),
        items: d.items.into_iter().map(item).collect(),
        open_commands: d.open_commands,
        permission: d.permission,
    }
}

/// Forward a call to a registered menus provider, or answer the neutral value
/// (an error) with a warning. A missing provider is not fatal to the caller:
/// it means the menus plugin is disabled.
fn unavailable(store: &StoreData, what: &str) -> String {
    store.warn(&format!(
        "menus service is not registered; refused `{what}` (is the menus plugin enabled?)"
    ));
    "menus service unavailable".to_string()
}

fn player(id: u64, name: &str) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator: false,
    }
}

impl bindings::aether::menus::menus::Host for StoreData {
    fn open(
        &mut self,
        player_id: u64,
        player_name: String,
        menu: Definition,
    ) -> Result<(), String> {
        let p = player(player_id, &player_name);
        match model::menus(&self.services) {
            Some(svc) => svc.open(&self.plugin, &p, &definition(menu)),
            None => Err(unavailable(self, "open")),
        }
    }

    fn update(
        &mut self,
        player_id: u64,
        player_name: String,
        menu: String,
        slot: u32,
        it: Option<Item>,
    ) -> Result<(), String> {
        let p = player(player_id, &player_name);
        match model::menus(&self.services) {
            Some(svc) => svc.update(&self.plugin, &p, &menu, slot, it.map(item)),
            None => Err(unavailable(self, "update")),
        }
    }

    fn close(&mut self, player_id: u64, player_name: String, menu: String) -> Result<(), String> {
        let p = player(player_id, &player_name);
        match model::menus(&self.services) {
            Some(svc) => svc.close(&self.plugin, &p, &menu),
            None => Err(unavailable(self, "close")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menus::MenusService;
    use crate::service::ServiceRegistry;
    use std::sync::Arc;

    struct Rec;

    impl MenusService for Rec {
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

    #[test]
    fn a_missing_provider_answers_an_error() {
        let reg = Arc::new(ServiceRegistry::new());
        let mut s = StoreData::for_test("caller", Vec::new(), reg, Arc::new(Null {}) as _);
        let r = bindings::aether::menus::menus::Host::open(
            &mut s,
            1,
            "Steve".to_string(),
            empty_definition(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn a_registered_provider_forwards() {
        let reg = Arc::new(ServiceRegistry::new());
        model::register_menus(&reg, Arc::new(Rec)).unwrap();
        let mut s = StoreData::for_test("caller", Vec::new(), reg, Arc::new(Null {}) as _);
        assert!(bindings::aether::menus::menus::Host::open(
            &mut s,
            1,
            "Steve".to_string(),
            empty_definition()
        )
        .is_ok());
    }

    fn empty_definition() -> Definition {
        Definition {
            name: "m".to_string(),
            title: "t".to_string(),
            layout: Layout::Chest3,
            items: Vec::new(),
            open_commands: Vec::new(),
            permission: None,
        }
    }

    #[derive(Default)]
    struct Null;
    impl crate::HostBackend for Null {
        fn kv_get(&self, _: &str, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn kv_put(&self, _: &str, _: &str, _: &[u8]) {}
        fn kv_delete(&self, _: &str, _: &str) {}
        fn kv_list(&self, _: &str, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn log(&self, _: &str, _: crate::LogLevel, _: &str) {}
        fn config(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn supports(&self, _: &str, _: &str) -> bool {
            false
        }
    }
}
