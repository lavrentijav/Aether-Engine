//! The SDK-backed reference plugin.
//!
//! Where `plugins/examples/hello` talks to the raw host and hand-writes every
//! event export, this one uses the SDK: it declares the two services it needs
//! in `plugin.toml` and answers `/greet` through `commands` + `messages` —
//! colour codes included. The SDK lives in the native `plugins/sdk` plugin, so
//! each call is one host import the host forwards in-process.
//!
//! The bindings are generated in-crate from the SDK client's `dependent` world
//! (the frozen plugin world plus the SDK imports); `wit-bindgen` cannot export
//! a world from a separate binding crate, so a dependent generates its own.
//! Build with `make` in this directory; see the Makefile.

wit_bindgen::generate!({
    world: "aether:sdk-client/dependent",
    generate_all,
    path: [
        "../../../crates/aether-plugin/wit",
        "../../../crates/aether-plugin/wit-sidebar",
        "../../../crates/aether-plugin/wit-sdk",
        "../../../crates/aether-plugin-sdk/wit",
    ],
});

// Keep the manifest's `requires` and this list from drifting.
use aether_plugin_sdk::requires;
requires!("sdk.commands@0.1", "sdk.messages@0.1");

use aether::plugin::host;
use aether::plugin::types::LogLevel;
use aether::sdk::{commands, messages};

struct Greet;

impl Guest for Greet {
    fn init(_config: String) -> Result<ManifestAck, String> {
        commands::register(&commands::CommandSpec {
            name: "greet".to_string(),
            parent: None,
            permission: None,
            op: 0,
            usage: "/greet".to_string(),
            description: "Say hello.".to_string(),
            args: Vec::new(),
        })?;
        host::log(LogLevel::Info, "greet plugin ready");
        Ok(ManifestAck {
            id: "greet".to_string(),
        })
    }

    fn shutdown() {}

    // Only `/greet` is registered, so it is the only command that arrives.
    fn on_command(p: Player, name: String, _args: Vec<String>) -> CommandResult {
        if name != "greet" {
            return CommandResult {
                handled: false,
                lines: Vec::new(),
            };
        }
        messages::send(&p, &format!("&aHello, &f{}&a!", p.name));
        CommandResult {
            handled: true,
            lines: Vec::new(),
        }
    }

    // Every other event is a no-op default; the manifest subscribes to none of
    // them, so the host never calls them. They must still be present because
    // the WIT world's export set is fixed.
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

export!(Greet);
