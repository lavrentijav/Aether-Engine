//! A test-only plugin whose behaviour is chosen by its config.
//!
//! The host tests need plugins that cancel, trap, overrun their time budget or
//! stay silent; writing four guest crates for that would be four times the
//! maintenance for the same coverage. Instead this one plugin reads its config
//! — `id`, `trap`, `spin` and `cancel` are comma-separated event lists — and
//! logs every event it is called for, so a test can assert both what happened
//! and in what order.
//!
//! Built once into `tests/fixtures/recorder.wasm`; see the Makefile recipe in
//! the fixture README.

wit_bindgen::generate!({
    world: "plugin",
    path: "../../../wit",
});

use aether::plugin::host;
use aether::plugin::types::LogLevel;

use std::sync::OnceLock;

static ID: OnceLock<String> = OnceLock::new();
static CONFIG: OnceLock<String> = OnceLock::new();

/// Crude `"key":"value"` lookup: the config is a flat object of strings, and
/// pulling in a JSON parser for four test keys is not worth it.
fn get(config: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = config.find(&needle)? + needle.len();
    let rest = &config[start..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let open = rest.find('"')?;
    let rest = &rest[open + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn list(config: &str, key: &str) -> Vec<String> {
    get(config, key)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn id() -> String {
    ID.get().cloned().unwrap_or_else(|| "recorder".to_string())
}

fn spin() -> ! {
    let mut x: u64 = 0;
    loop {
        x = x.wrapping_add(1);
        std::hint::black_box(x);
    }
}

/// Log the event, then honour `trap`/`spin`/`cancel`.
fn observe(event: &str) -> Decision {
    let config = CONFIG.get().cloned().unwrap_or_default();
    if list(&config, "trap").iter().any(|e| e == event) {
        panic!("recorder asked to trap in {event}");
    }
    host::log(LogLevel::Info, &format!("{}:{event}", id()));
    if list(&config, "spin").iter().any(|e| e == event) {
        spin();
    }
    if list(&config, "cancel").iter().any(|e| e == event) {
        Decision::Cancel(format!("{event} cancelled by {}", id()))
    } else {
        Decision::Allow
    }
}

struct Recorder;

impl Guest for Recorder {
    fn init(config: String) -> Result<ManifestAck, String> {
        let own = get(&config, "id").unwrap_or_else(|| "recorder".to_string());
        let _ = ID.set(own.clone());
        let _ = CONFIG.set(config);
        host::log(LogLevel::Info, &format!("{own}:init"));
        Ok(ManifestAck { id: own })
    }

    fn shutdown() {}

    fn before_block_break(
        _p: Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: String,
    ) -> Decision {
        observe("before-block-break")
    }

    fn before_block_place(
        _p: Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: String,
    ) -> Decision {
        observe("before-block-place")
    }

    fn before_use_item(_p: Player, _item: String) -> Decision {
        observe("before-use-item")
    }

    fn before_attack(_p: Player, _target: u64) -> Decision {
        observe("before-attack")
    }

    fn before_chat(_p: Player, _message: String) -> Decision {
        observe("before-chat")
    }

    fn before_container_click(_p: Player, _window: u32, _slot: i32) -> Decision {
        observe("before-container-click")
    }

    fn after_player_join(_p: Player) {
        let _ = observe("after-player-join");
    }

    fn after_player_leave(_p: Player) {
        let _ = observe("after-player-leave");
    }

    fn after_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) {
        let _ = observe("after-block-break");
    }

    fn after_entity_death(_entity: u64, _killer: Option<u64>) {
        let _ = observe("after-entity-death");
    }

    fn after_damage(_entity: u64, _amount: f64, _source: String) {
        let _ = observe("after-damage");
    }

    fn after_craft(_p: Player, _item: String, _count: u32) {
        let _ = observe("after-craft");
    }

    fn after_enchant(_p: Player, _item: String, _level: u32) {
        let _ = observe("after-enchant");
    }

    fn on_tick(_n: u64) {
        let _ = observe("on-tick");
    }

    fn on_command(_p: Player, name: String, _args: Vec<String>) -> CommandResult {
        let _ = observe("on-command");
        CommandResult {
            handled: name == id(),
            lines: vec![format!("{}:on-command", id())],
        }
    }
}

export!(Recorder);
