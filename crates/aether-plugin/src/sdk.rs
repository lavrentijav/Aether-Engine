//! The SDK service bridge: the host side of `aether:sdk@0.1`.
//!
//! A dependent WASM plugin imports the SDK interfaces; each call lands on the
//! generated host trait implemented here on [`crate::host::StoreData`], which
//! forwards it to the **provider** registered in the [`crate::service`]
//! registry — in practice the native `plugins/sdk` plugin. The caller's
//! plugin id (from the store) is passed along, so the provider can key state
//! and capability checks per dependent.
//!
//! The provider-side traits live here too ([`PermissionsService`],
//! [`CommandsService`]); a native provider implements them and calls the
//! `register_*` helpers in its `init`. A WASM dependent never sees these Rust
//! traits — it sees the WIT functions — but a native dependent may resolve
//! them directly from the registry.

use std::sync::Arc;

use crate::bindings::aether::plugin::types::Player;
use crate::host::StoreData;
use crate::service::ServiceRegistry;

/// The generated host bindings for the SDK interfaces.
///
/// `with` maps the shared `aether:plugin/types` interface onto the host's
/// existing bindings, so a `player` passed to a service is the *same* Rust
/// type as the one in an event — no conversion at the boundary.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: ["wit", "wit-sdk"],
        world: "aether:sdk/services",
        with: {
            "aether:plugin/types@0.1.0": crate::bindings::aether::plugin::types,
            "aether:plugin/host@0.1.0": crate::bindings::aether::plugin::host,
        },
    });
}

pub use bindings::aether::sdk::commands::{Arg, ArgKind, CommandSpec};
pub use bindings::Services;

/// The provider-side permission service the SDK plugin implements.
///
/// The `caller` is the id of the dependent that made the call, so the provider
/// can keep per-caller state and refuse with the right name in the message.
pub trait PermissionsService: Send + Sync {
    /// Whether `p` holds `node`.
    fn check(&self, caller: &str, p: &Player, node: &str) -> bool;
    /// `check` plus the standard denial reply.
    fn require(&self, caller: &str, p: &Player, node: &str) -> bool;
    /// `p`'s op level.
    fn level(&self, caller: &str, p: &Player) -> u8;
    /// The groups `p` is in.
    fn groups(&self, caller: &str, p: &Player) -> Vec<String>;
    /// A meta value.
    fn meta(&self, caller: &str, p: &Player, key: &str) -> Option<String>;
    /// Grant or deny a node to a subject.
    fn grant(&self, caller: &str, subject: &str, node: &str, value: bool) -> Result<(), String>;
    /// Remove an explicit node from a subject.
    fn revoke(&self, caller: &str, subject: &str, node: &str) -> Result<(), String>;

    /// Sugar over [`Self::level`].
    fn has_level(&self, caller: &str, p: &Player, level: u8) -> bool {
        self.level(caller, p) >= level
    }
    /// Sugar over [`Self::groups`].
    fn in_group(&self, caller: &str, p: &Player, group: &str) -> bool {
        self.groups(caller, p).iter().any(|g| g == group)
    }
}

/// The provider-side command service.
pub trait CommandsService: Send + Sync {
    /// Register a command spec.
    fn register(&self, caller: &str, spec: CommandSpec) -> Result<(), String>;
    /// The usage line for a registered command (empty if unknown).
    fn usage(&self, caller: &str, name: &str) -> String;
    /// Parse arguments against a registered spec.
    fn parse(&self, caller: &str, name: &str, args: &[String]) -> Vec<(String, String)>;
}

/// Erasure wrappers, so a trait object can live in the untyped registry and be
/// recovered by a known concrete type.
pub struct PermissionsProvider(pub Arc<dyn PermissionsService>);
pub struct CommandsProvider(pub Arc<dyn CommandsService>);

/// The service names under which the SDK examples are registered.
pub const PERMISSIONS_SERVICE: &str = "sdk.permissions";
pub const COMMANDS_SERVICE: &str = "sdk.commands";
/// The service version.
pub const SERVICE_VERSION: &str = "0.1";

/// Publish a permission provider.
pub fn register_permissions(
    registry: &ServiceRegistry,
    service: Arc<dyn PermissionsService>,
) -> Result<(), String> {
    registry.register(
        PERMISSIONS_SERVICE,
        SERVICE_VERSION,
        Arc::new(PermissionsProvider(service)),
    )
}

/// Publish a command provider.
pub fn register_commands(
    registry: &ServiceRegistry,
    service: Arc<dyn CommandsService>,
) -> Result<(), String> {
    registry.register(
        COMMANDS_SERVICE,
        SERVICE_VERSION,
        Arc::new(CommandsProvider(service)),
    )
}

/// Resolve the permission provider, if one is registered.
pub fn permissions(registry: &ServiceRegistry) -> Option<Arc<dyn PermissionsService>> {
    registry
        .get::<PermissionsProvider>(PERMISSIONS_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

/// Resolve the command provider, if one is registered.
pub fn commands(registry: &ServiceRegistry) -> Option<Arc<dyn CommandsService>> {
    registry
        .get::<CommandsProvider>(COMMANDS_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

impl StoreData {
    /// Forward a call to a registered service, or answer the neutral value.
    ///
    /// A missing provider is logged once per call: it means either the SDK is
    /// disabled (the manifest `requires` should have refused the load) or a
    /// service that was never registered was asked for.
    fn service_unavailable(&self, service: &str, what: &str) -> bool {
        self.warn(&format!(
            "SDK service `{service}` is not registered; refused `{what}` \
             (is the SDK plugin enabled?)"
        ));
        false
    }
}

impl bindings::aether::sdk::permissions::Host for StoreData {
    fn check(&mut self, p: Player, node: String) -> bool {
        match permissions(&self.services) {
            Some(svc) => svc.check(&self.plugin, &p, &node),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "check");
                false
            }
        }
    }

    fn require(&mut self, p: Player, node: String) -> bool {
        match permissions(&self.services) {
            Some(svc) => svc.require(&self.plugin, &p, &node),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "require");
                false
            }
        }
    }

    fn level(&mut self, p: Player) -> u8 {
        match permissions(&self.services) {
            Some(svc) => svc.level(&self.plugin, &p),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "level");
                0
            }
        }
    }

    fn has_level(&mut self, p: Player, level: u8) -> bool {
        match permissions(&self.services) {
            Some(svc) => svc.has_level(&self.plugin, &p, level),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "has-level");
                false
            }
        }
    }

    fn groups(&mut self, p: Player) -> Vec<String> {
        match permissions(&self.services) {
            Some(svc) => svc.groups(&self.plugin, &p),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "groups");
                Vec::new()
            }
        }
    }

    fn in_group(&mut self, p: Player, group: String) -> bool {
        match permissions(&self.services) {
            Some(svc) => svc.in_group(&self.plugin, &p, &group),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "in-group");
                false
            }
        }
    }

    fn meta(&mut self, p: Player, key: String) -> Option<String> {
        match permissions(&self.services) {
            Some(svc) => svc.meta(&self.plugin, &p, &key),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "meta");
                None
            }
        }
    }

    fn grant(&mut self, subject: String, node: String, value: bool) -> Result<(), String> {
        if !self.allowed("permissions.admin") {
            self.deny("permissions.admin", "permissions.grant");
            return Err("permissions.admin capability not granted".to_string());
        }
        match permissions(&self.services) {
            Some(svc) => svc.grant(&self.plugin, &subject, &node, value),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "grant");
                Err("permission service unavailable".to_string())
            }
        }
    }

    fn revoke(&mut self, subject: String, node: String) -> Result<(), String> {
        if !self.allowed("permissions.admin") {
            self.deny("permissions.admin", "permissions.revoke");
            return Err("permissions.admin capability not granted".to_string());
        }
        match permissions(&self.services) {
            Some(svc) => svc.revoke(&self.plugin, &subject, &node),
            None => {
                self.service_unavailable(PERMISSIONS_SERVICE, "revoke");
                Err("permission service unavailable".to_string())
            }
        }
    }
}

impl bindings::aether::sdk::commands::Host for StoreData {
    fn register(&mut self, spec: CommandSpec) -> Result<(), String> {
        match commands(&self.services) {
            Some(svc) => svc.register(&self.plugin, spec),
            None => {
                self.service_unavailable(COMMANDS_SERVICE, "register");
                Err("command service unavailable".to_string())
            }
        }
    }

    fn usage(&mut self, name: String) -> String {
        match commands(&self.services) {
            Some(svc) => svc.usage(&self.plugin, &name),
            None => {
                self.service_unavailable(COMMANDS_SERVICE, "usage");
                String::new()
            }
        }
    }

    fn parse(&mut self, name: String, args: Vec<String>) -> Vec<(String, String)> {
        match commands(&self.services) {
            Some(svc) => svc.parse(&self.plugin, &name, &args),
            None => {
                self.service_unavailable(COMMANDS_SERVICE, "parse");
                Vec::new()
            }
        }
    }
}
