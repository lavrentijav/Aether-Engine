//! Thin typed client for **dependent** plugins.
//!
//! This crate carries no logic. It generates the guest half of the component
//! bindings for the `dependent` world — the frozen `aether:plugin@0.1.0` world
//! plus the `aether:sdk@0.1.0` service imports — and re-exports them through a
//! [`prelude`]. The SDK's behaviour lives in the native `plugins/sdk` plugin;
//! a call here is one host import that the host forwards to that plugin.
//!
//! A dependent plugin therefore:
//!
//! 1. builds with the `dependent` world (this crate's WIT),
//! 2. declares the services it needs in its `plugin.toml` `requires` — see
//!    [`requires!`],
//! 3. calls the prelude's `permissions()`, `commands()`, … accessors and
//!    exports itself with the prelude's `export!`.
//!
//! The WIT is read from the host's own `crates/aether-plugin` copy, so the
//! guest and host cannot drift.

/// The generated guest bindings for the `dependent` world.
///
/// A module rather than the crate root, so the generated `export!` macro (which
/// `wit-bindgen` marks `#[macro_export]`) can be re-exported from here without
/// colliding with its crate-root definition.
pub mod bindings {
    wit_bindgen::generate!({
        world: "aether:sdk-client/dependent",
        // The world includes a foreign world (the frozen plugin world) and
        // imports foreign interfaces, so every imported interface must be
        // generated here rather than remapped.
        generate_all,
        // Reach the generated `export!` macro from a dependent crate.
        pub_export_macro: true,
        path: [
            "../aether-plugin/wit",
            "../aether-plugin/wit-sidebar",
            "../aether-plugin/wit-sdk",
            "wit",
        ],
    });
}

/// The names a dependent declares in its `plugin.toml` `requires`.
///
/// The macro emits a `REQUIRES` constant from the same list the manifest uses,
/// so the Rust-side list and the manifest cannot drift:
///
/// ```ignore
/// aether_plugin_sdk::requires!("sdk.permissions@0.1", "sdk.commands@0.1");
/// // plugin.toml: requires = ["sdk.permissions@0.1", "sdk.commands@0.1"]
/// ```
#[macro_export]
macro_rules! requires {
    ($($service:literal),* $(,)?) => {
        /// Services this plugin requires at load, mirroring its
        /// `plugin.toml` `requires` list.
        pub const REQUIRES: &[&str] = &[$($service),*];
    };
}

/// Ergonomic re-exports: the SDK service modules, the host import, the shared
/// event types, the frozen `Guest` trait and the `export!` macro.
pub mod prelude {
    pub use crate::bindings::aether::plugin::host;
    pub use crate::bindings::aether::plugin::types::{
        CommandResult, Decision, LogLevel, ManifestAck, Player,
    };
    pub use crate::bindings::aether::sdk::{
        commands, config, confirm, cooldowns, history, messages, permissions, placeholders,
        players, scheduler,
    };
    /// The sidebar service, so a dependent can override a line or toggle a
    /// player's panel.
    pub use crate::bindings::aether::sidebar::sidebar;
    pub use crate::bindings::{export, Guest};
}
