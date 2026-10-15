//! The **native** plugin tier: in-process Rust plugins.
//!
//! A native plugin is trusted, compiled-in code — there is no sandbox, no
//! wasmtime and no epoch budget around it. It exists so a plugin like the SDK
//! service host (`plugins/sdk`) can keep state and call core Rust APIs without
//! paying a boundary crossing, while still being loaded, ordered, enabled and
//! disabled through the same manifest and event bus as a WASM plugin.
//!
//! Trust is explicit: a native plugin may panic or block, and unlike a WASM
//! plugin neither is contained. Operators install native plugins deliberately;
//! the host documents it as trusted-by-install rather than pretending otherwise.
//!
//! The trait mirrors the frozen WIT world's exports, so a native plugin and a
//! WASM one implement the same surface.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::bindings::aether::plugin::types::{CommandResult, Decision, LogLevel, Player};
use crate::host::HostBackend;
use crate::service::ServiceRegistry;

/// What a native plugin sees when it is initialised or called: its own
/// identity, what the operator granted it, its config, the shared service
/// registry and the host backend.
pub struct NativeContext<'a> {
    /// The plugin's manifest id: its KV namespace and log tag.
    pub id: &'a str,
    /// The capabilities the operator granted (already narrowed by the host).
    pub capabilities: &'a [String],
    /// The operator's `[plugins.<id>]` values.
    pub config: &'a BTreeMap<String, String>,
    /// The services this plugin may publish into and resolve from.
    pub services: &'a Arc<ServiceRegistry>,
    /// The host backend: `log`, `storage.kv`, `config`, `supports`.
    pub backend: &'a Arc<dyn HostBackend>,
}

impl NativeContext<'_> {
    /// Whether `capability` was granted.
    pub fn allowed(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Write a line tagged with this plugin's id.
    pub fn log(&self, level: LogLevel, message: &str) {
        self.backend.log(self.id, level, message);
    }
}

/// An in-process plugin. Every event has a neutral default, so a plugin
/// overrides only what it cares about; the manifest still decides which events
/// the host calls (an unsubscribed event is never dispatched).
pub trait NativePlugin: Send {
    /// Register services and load first-time state. Returning `Err` refuses the
    /// load; the plugin is not added.
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String>;

    /// Best-effort teardown at shutdown.
    fn shutdown(&mut self, _ctx: &mut NativeContext) {}

    /// Drive per-tick work (only called when the manifest subscribes to
    /// `on-tick`, at the manifest's period).
    fn on_tick(&mut self, _ctx: &mut NativeContext, _n: u64) {}

    /// A command the plugin registered in its manifest. `None` falls through.
    fn on_command(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _name: &str,
        _args: &[String],
    ) -> Option<CommandResult> {
        None
    }

    // --- before-*: neutral default is `Allow` ------------------------------

    fn before_block_break(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: &str,
    ) -> Decision {
        Decision::Allow
    }

    fn before_block_place(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: &str,
    ) -> Decision {
        Decision::Allow
    }

    fn before_use_item(&mut self, _ctx: &mut NativeContext, _p: &Player, _item: &str) -> Decision {
        Decision::Allow
    }

    fn before_attack(&mut self, _ctx: &mut NativeContext, _p: &Player, _target: u64) -> Decision {
        Decision::Allow
    }

    fn before_chat(&mut self, _ctx: &mut NativeContext, _p: &Player, _message: &str) -> Decision {
        Decision::Allow
    }

    fn before_container_click(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _window: u32,
        _slot: i32,
    ) -> Decision {
        Decision::Allow
    }

    // --- after-*: observe-only ---------------------------------------------

    fn after_player_join(&mut self, _ctx: &mut NativeContext, _p: &Player) {}
    fn after_player_leave(&mut self, _ctx: &mut NativeContext, _p: &Player) {}
    fn after_block_break(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: &str,
    ) {
    }
    fn after_entity_death(&mut self, _ctx: &mut NativeContext, _entity: u64, _killer: Option<u64>) {
    }
    fn after_damage(
        &mut self,
        _ctx: &mut NativeContext,
        _entity: u64,
        _amount: f64,
        _source: &str,
    ) {
    }
    fn after_craft(&mut self, _ctx: &mut NativeContext, _p: &Player, _item: &str, _count: u32) {}
    fn after_enchant(&mut self, _ctx: &mut NativeContext, _p: &Player, _item: &str, _level: u32) {}
}

/// A native plugin as the build-time registry hands it to the host: the
/// manifest it loads under and a constructor.
///
/// There is no plugin file for a native plugin — it is compiled into the
/// server — so the manifest is code, and the server collects one
/// `NativeRegistration` per linked-in native plugin.
pub struct NativeRegistration {
    /// The manifest: id, priority, capabilities, `subscribe`, `requires`.
    pub manifest: crate::manifest::Manifest,
    /// A fresh instance. Called once when the host loads the plugin.
    pub new: fn() -> Box<dyn NativePlugin>,
}

impl std::fmt::Debug for NativeRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRegistration")
            .field("id", &self.manifest.id)
            .finish_non_exhaustive()
    }
}
