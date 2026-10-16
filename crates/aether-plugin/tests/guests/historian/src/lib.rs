//! A test-only SDK dependent that exercises the `history` service.
//!
//! It registers `/hist` and, when dispatched, asks the history service two
//! questions — whether the block at `(1,2,3)` is worldgen-original and how many
//! block-history events it has — then reports both through the raw host `log`
//! import. A test grants (or withholds) the caller's `history.read` capability
//! and reads the log to see whether the query was answered or refused at the
//! gate.
//!
//! Built once into `../../../fixtures/historian.wasm`; see the fixture README.

wit_bindgen::generate!({
    world: "aether:sdk-client/dependent",
    generate_all,
    path: [
        "../../../../aether-plugin/wit",
        "../../../../aether-plugin/wit-sidebar",
        "../../../../aether-plugin/wit-sdk",
        "../../../../aether-plugin-sdk/wit",
    ],
});

// Keep the manifest's `requires` and this list from drifting.
use aether_plugin_sdk::requires;
requires!("sdk.history@0.1");

use aether::plugin::host;
use aether::plugin::types::LogLevel;
use aether::sdk::history;

struct Historian;

impl Guest for Historian {
    fn init(_config: String) -> Result<ManifestAck, String> {
        Ok(ManifestAck {
            id: "historian".to_string(),
        })
    }

    fn shutdown() {}

    fn on_command(_p: Player, name: String, _args: Vec<String>) -> CommandResult {
        if name != "hist" {
            return CommandResult {
                handled: false,
                lines: Vec::new(),
            };
        }
        let orig = history::worldgen_original(1, 2, 3);
        let page = history::block_history(1, 2, 3, None, 10);
        host::log(
            LogLevel::Info,
            &format!("historian orig={orig} n={}", page.events.len()),
        );
        CommandResult {
            handled: true,
            lines: Vec::new(),
        }
    }

    // The frozen export set; the manifest subscribes to none of these.
    fn before_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) -> Decision {
        Decision::Allow
    }
    fn before_block_place(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) -> Decision {
        Decision::Allow
    }
    fn before_use_item(_p: Player, _item: String) -> Decision {
        Decision::Allow
    }
    fn before_attack(_p: Player, _target: u64) -> Decision {
        Decision::Allow
    }
    fn before_chat(_p: Player, _message: String) -> Decision {
        Decision::Allow
    }
    fn before_container_click(_p: Player, _window: u32, _slot: i32) -> Decision {
        Decision::Allow
    }
    fn after_player_join(_p: Player) {}
    fn after_player_leave(_p: Player) {}
    fn after_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) {}
    fn after_entity_death(_entity: u64, _killer: Option<u64>) {}
    fn after_damage(_entity: u64, _amount: f64, _source: String) {}
    fn after_craft(_p: Player, _item: String, _count: u32) {}
    fn after_enchant(_p: Player, _item: String, _level: u32) {}
    fn on_tick(_n: u64) {}
}

export!(Historian);
