//! The reference Aether plugin.
//!
//! It does three small things so a developer can see the whole surface in one
//! place: it registers the `/hello` command, cancels breaking bedrock, and
//! logs player joins and leaves. Everything else is a no-op `allow`, which is
//! what an unimplemented `before-*` hook must look like.
//!
//! Build with `make` (or `make build`) in this directory; see the Makefile.

// The bindings are generated from the host's own WIT package, so the guest and
// the host are compiled against one source of truth.
wit_bindgen::generate!({
    world: "plugin",
    path: "../../../crates/aether-plugin/wit",
});

use aether::plugin::host;
use aether::plugin::types::LogLevel;

struct Hello;

impl Guest for Hello {
    fn init(_config: String) -> Result<ManifestAck, String> {
        host::log(LogLevel::Info, "hello plugin ready");
        Ok(ManifestAck {
            id: "hello".to_string(),
        })
    }

    fn shutdown() {
        host::log(LogLevel::Info, "hello plugin stopping");
    }

    fn before_block_break(
        _p: Player,
        _x: i32,
        _y: i32,
        _z: i32,
        block: String,
    ) -> Decision {
        // Bedrock is the one block every player expects to be unbreakable, so
        // this is the smallest useful demonstration of a veto.
        if block.ends_with("bedrock") {
            Decision::Cancel("Bedrock is indestructible.".to_string())
        } else {
            Decision::Allow
        }
    }

    fn before_block_place(
        _p: Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: String,
    ) -> Decision {
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

    fn after_player_join(p: Player) {
        host::log(LogLevel::Info, &format!("{} joined the world", p.name));
    }

    fn after_player_leave(p: Player) {
        host::log(LogLevel::Info, &format!("{} left the world", p.name));
    }

    fn after_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) {}

    fn after_entity_death(_entity: u64, _killer: Option<u64>) {}

    fn after_damage(_entity: u64, _amount: f64, _source: String) {}

    fn after_craft(_p: Player, _item: String, _count: u32) {}

    fn after_enchant(_p: Player, _item: String, _level: u32) {}

    fn on_tick(n: u64) {
        if n % 1200 == 0 {
            host::log(LogLevel::Debug, &format!("hello plugin at tick {n}"));
        }
    }

    fn on_command(p: Player, name: String, args: Vec<String>) -> CommandResult {
        if name != "hello" {
            return CommandResult {
                handled: false,
                lines: Vec::new(),
            };
        }
        let who = args.first().map(String::as_str).unwrap_or(&p.name);
        CommandResult {
            handled: true,
            lines: vec![format!("Hello, {who}!")],
        }
    }
}

export!(Hello);
