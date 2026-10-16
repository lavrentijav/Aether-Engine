//! A test-only plugin that exercises the `aether:sdk/placeholders` and
//! `aether:sidebar` imports.
//!
//! `/sid` drives every call on both services, so the host bridges can be tested
//! end to end: the guest's calls must cross the component boundary and land on
//! the registered `PlaceholdersService` / `SidebarService` providers with the
//! caller's id and the right arguments.
//!
//! Built once into `tests/fixtures/sidebar/sidebar.wasm`; see the fixture
//! README for the recipe.

wit_bindgen::generate!({
    world: "test:sidebar-guest/guest@0.1.0",
    // The guest world includes a foreign world and imports foreign
    // interfaces, so every imported interface is generated here.
    generate_all,
    path: [
        "../../../wit",
        "../../../wit-sidebar",
        "../../../wit-sdk",
        "wit",
    ],
});

use aether::plugin::types as types;
use aether::sdk::placeholders;
use aether::sidebar::sidebar;

struct Sid;

impl Guest for Sid {
    fn init(_config: String) -> Result<types::ManifestAck, String> {
        Ok(types::ManifestAck {
            id: "sid".to_string(),
        })
    }

    fn shutdown() {}

    fn on_command(p: types::Player, name: String, _args: Vec<String>) -> types::CommandResult {
        if name != "sid" {
            return types::CommandResult {
                handled: false,
                lines: Vec::new(),
            };
        }
        // Feed a placeholder through the shared registry...
        let registered = placeholders::register("balance");
        placeholders::set("balance", &p, "1234");
        let resolved = placeholders::resolve(&p, "b=%balance%");
        // ...put a line on the panel, then toggle.
        sidebar::set_line(&p, 3, "line three");
        sidebar::clear_line(&p, 3);
        let shown = sidebar::toggle(&p);
        sidebar::show(&p);
        sidebar::hide(&p);
        types::CommandResult {
            handled: true,
            lines: vec![format!(
                "shown={shown} registered={registered} resolved={resolved}"
            )],
        }
    }

    // The frozen export set must be provided in full; the events this fixture
    // does not use are neutral.
    fn before_block_break(_: types::Player, _: i32, _: i32, _: i32, _: String) -> types::Decision {
        types::Decision::Allow
    }
    fn before_block_place(_: types::Player, _: i32, _: i32, _: i32, _: String) -> types::Decision {
        types::Decision::Allow
    }
    fn before_use_item(_: types::Player, _: String) -> types::Decision {
        types::Decision::Allow
    }
    fn before_attack(_: types::Player, _: u64) -> types::Decision {
        types::Decision::Allow
    }
    fn before_chat(_: types::Player, _: String) -> types::Decision {
        types::Decision::Allow
    }
    fn before_container_click(_: types::Player, _: u32, _: i32) -> types::Decision {
        types::Decision::Allow
    }
    fn after_player_join(_: types::Player) {}
    fn after_player_leave(_: types::Player) {}
    fn after_block_break(_: types::Player, _: i32, _: i32, _: i32, _: String) {}
    fn after_entity_death(_: u64, _: Option<u64>) {}
    fn after_damage(_: u64, _: f64, _: String) {}
    fn after_craft(_: types::Player, _: String, _: u32) {}
    fn after_enchant(_: types::Player, _: String, _: u32) {}
    fn on_tick(_: u64) {}
}

export!(Sid);
