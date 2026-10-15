//! WebAssembly plugins for Aether Engine.
//!
//! A plugin is a `wasm32-wasip2` component exporting the `aether:plugin@0.1`
//! world in [`wit/aether-plugin.wit`](wit/aether-plugin.wit). This crate is the
//! host half: it loads a [`ManifestFile`], instantiates the component behind
//! the sandbox limits in [`HostConfig`], and offers the [`PluginHost`] event
//! bus the server calls at its hook points.
//!
//! The engine is deliberately kept in its own crate and behind the server's
//! `plugins` cargo feature: Wasmtime is a large dependency and the parts of the
//! server that never touch plugins should not have to build it.
//!
//! ```no_run
//! use std::path::Path;
//! use aether_plugin::{HostConfig, PluginHost, discover};
//!
//! # fn my_backend() -> std::sync::Arc<dyn aether_plugin::HostBackend> { todo!() }
//! let host = PluginHost::new(HostConfig::default(), my_backend()).unwrap();
//! for m in discover(Path::new("plugins")) {
//!     // The caller decides enable/grant from operator config.
//! }
//! ```

mod host;
pub mod manifest;
pub mod native;
pub mod sdk;
pub mod service;

/// The generated component-model bindings for the `plugin` world.
///
/// Kept public because callers need the event argument types
/// (`aether::plugin::types::*`); the generated host traits are implemented here
/// and are not part of the stable surface.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
    });
}

use std::fmt;

pub use host::{discover, HostBackend, HostConfig, PluginHost};
pub use manifest::{Manifest, ManifestFile, PluginTier};
pub use native::{NativeContext, NativePlugin, NativeRegistration};
pub use service::{ServiceRef, ServiceRegistry};

/// Re-exported event types, so the server and the examples name the same WIT
/// types this host binds.
pub use bindings::aether::plugin::types::{CommandResult, Decision, LogLevel, ManifestAck, Player};

/// The plugin host's error type: a message, or a Wasmtime error rendered into
/// one. Plugins are best-effort, so callers log and carry on rather than
/// threading a typed error through the game.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    /// Build an error from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

impl From<wasmtime::Error> for Error {
    fn from(e: wasmtime::Error) -> Self {
        Error(e.to_string())
    }
}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;
