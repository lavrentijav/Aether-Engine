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
pub use bindings::aether::sdk::messages::TitleTimes;
pub use bindings::aether::sdk::players::{Pos, TeleportCause};
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

/// The provider-side message service.
pub trait MessagesService: Send + Sync {
    /// A chat line to one player.
    fn send(&self, caller: &str, p: &Player, text: &str);
    /// A translatable message by key with `(name, value)` args.
    fn send_key(&self, caller: &str, p: &Player, key: &str, args: &[(String, String)]);
    /// An action-bar line.
    fn actionbar(&self, caller: &str, p: &Player, text: &str);
    /// A title and optional subtitle.
    fn title(
        &self,
        caller: &str,
        p: &Player,
        title: &str,
        subtitle: Option<&str>,
        times: TitleTimes,
    );
    /// A line to everyone.
    fn broadcast(&self, caller: &str, text: &str);
    /// A player's formatted prefix (empty if none).
    fn prefix(&self, caller: &str, p: &Player) -> String;
}

/// The provider-side player service.
///
/// Most of these front core APIs (CMD-31/32) that are not on this branch, so
/// the SDK's implementation reports the gap rather than guessing; the trait
/// still exists so the surface is final.
pub trait PlayersService: Send + Sync {
    /// Everyone online.
    fn online(&self, caller: &str) -> Vec<Player>;
    /// An online player by name.
    fn by_name(&self, caller: &str, name: &str) -> Option<Player>;
    /// An online player by session entity id.
    fn by_id(&self, caller: &str, id: u64) -> Option<Player>;
    /// A possibly-offline player's last known name.
    fn offline_name(&self, caller: &str, uuid_low: u64, uuid_high: u64) -> Option<String>;
    /// Move a player, searching for a safe spot.
    fn teleport(
        &self,
        caller: &str,
        p: &Player,
        to: Pos,
        cause: TeleportCause,
    ) -> Result<(), String>;
    /// Give items, returning how many did not fit.
    fn give(&self, caller: &str, p: &Player, item: &str, count: u32) -> u64;
}

/// Erasure wrappers, so a trait object can live in the untyped registry and be
/// recovered by a known concrete type.
pub struct PermissionsProvider(pub Arc<dyn PermissionsService>);
pub struct CommandsProvider(pub Arc<dyn CommandsService>);
pub struct MessagesProvider(pub Arc<dyn MessagesService>);
pub struct PlayersProvider(pub Arc<dyn PlayersService>);
pub struct SchedulerProvider(pub Arc<dyn SchedulerService>);
pub struct CooldownsProvider(pub Arc<dyn CooldownsService>);
pub struct ConfirmProvider(pub Arc<dyn ConfirmService>);

/// The provider-side timer service.
pub trait SchedulerService: Send + Sync {
    /// A one-shot timer, `ticks` from now.
    fn after(&self, caller: &str, ticks: u64) -> u64;
    /// A repeating timer.
    fn every(&self, caller: &str, period: u64) -> u64;
    /// Cancel a timer.
    fn cancel(&self, caller: &str, id: u64) -> bool;
    /// Take the ids due since the last poll.
    fn poll(&self, caller: &str) -> Vec<u64>;
}

/// The provider-side cooldown service.
pub trait CooldownsService: Send + Sync {
    /// Whether `key` is ready, arming it for `ticks` when it is.
    fn ready(&self, caller: &str, key: &str, ticks: u64) -> bool;
    /// Ticks left on `key`.
    fn remaining(&self, caller: &str, key: &str) -> u64;
}

/// The provider-side confirmation service.
pub trait ConfirmService: Send + Sync {
    /// Record a pending confirmation and send its prompt.
    fn request(&self, caller: &str, p: &Player, prompt: &str, token: &str) -> bool;
    /// Consume a pending confirmation.
    fn check(&self, caller: &str, p: &Player, token: &str) -> bool;
}

/// The service names under which the SDK services are registered.
pub const PERMISSIONS_SERVICE: &str = "sdk.permissions";
pub const COMMANDS_SERVICE: &str = "sdk.commands";
pub const MESSAGES_SERVICE: &str = "sdk.messages";
pub const PLAYERS_SERVICE: &str = "sdk.players";
pub const SCHEDULER_SERVICE: &str = "sdk.scheduler";
pub const COOLDOWNS_SERVICE: &str = "sdk.cooldowns";
pub const CONFIRM_SERVICE: &str = "sdk.confirm";
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

/// Publish a message provider.
pub fn register_messages(
    registry: &ServiceRegistry,
    service: Arc<dyn MessagesService>,
) -> Result<(), String> {
    registry.register(
        MESSAGES_SERVICE,
        SERVICE_VERSION,
        Arc::new(MessagesProvider(service)),
    )
}

/// Publish a player provider.
pub fn register_players(
    registry: &ServiceRegistry,
    service: Arc<dyn PlayersService>,
) -> Result<(), String> {
    registry.register(
        PLAYERS_SERVICE,
        SERVICE_VERSION,
        Arc::new(PlayersProvider(service)),
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

/// Resolve the message provider, if one is registered.
pub fn messages(registry: &ServiceRegistry) -> Option<Arc<dyn MessagesService>> {
    registry
        .get::<MessagesProvider>(MESSAGES_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

/// Resolve the player provider, if one is registered.
pub fn players(registry: &ServiceRegistry) -> Option<Arc<dyn PlayersService>> {
    registry
        .get::<PlayersProvider>(PLAYERS_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

/// Publish a scheduler provider.
pub fn register_scheduler(
    registry: &ServiceRegistry,
    service: Arc<dyn SchedulerService>,
) -> Result<(), String> {
    registry.register(
        SCHEDULER_SERVICE,
        SERVICE_VERSION,
        Arc::new(SchedulerProvider(service)),
    )
}

/// Publish a cooldown provider.
pub fn register_cooldowns(
    registry: &ServiceRegistry,
    service: Arc<dyn CooldownsService>,
) -> Result<(), String> {
    registry.register(
        COOLDOWNS_SERVICE,
        SERVICE_VERSION,
        Arc::new(CooldownsProvider(service)),
    )
}

/// Publish a confirmation provider.
pub fn register_confirm(
    registry: &ServiceRegistry,
    service: Arc<dyn ConfirmService>,
) -> Result<(), String> {
    registry.register(
        CONFIRM_SERVICE,
        SERVICE_VERSION,
        Arc::new(ConfirmProvider(service)),
    )
}

/// Resolve the scheduler provider, if one is registered.
pub fn scheduler(registry: &ServiceRegistry) -> Option<Arc<dyn SchedulerService>> {
    registry
        .get::<SchedulerProvider>(SCHEDULER_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

/// Resolve the cooldown provider, if one is registered.
pub fn cooldowns(registry: &ServiceRegistry) -> Option<Arc<dyn CooldownsService>> {
    registry
        .get::<CooldownsProvider>(COOLDOWNS_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}

/// Resolve the confirmation provider, if one is registered.
pub fn confirm(registry: &ServiceRegistry) -> Option<Arc<dyn ConfirmService>> {
    registry
        .get::<ConfirmProvider>(CONFIRM_SERVICE, SERVICE_VERSION)
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

impl bindings::aether::sdk::messages::Host for StoreData {
    fn send(&mut self, p: Player, text: String) {
        match messages(&self.services) {
            Some(svc) => svc.send(&self.plugin, &p, &text),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "send");
            }
        }
    }

    fn send_key(&mut self, p: Player, key: String, args: Vec<(String, String)>) {
        match messages(&self.services) {
            Some(svc) => svc.send_key(&self.plugin, &p, &key, &args),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "send-key");
            }
        }
    }

    fn actionbar(&mut self, p: Player, text: String) {
        match messages(&self.services) {
            Some(svc) => svc.actionbar(&self.plugin, &p, &text),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "actionbar");
            }
        }
    }

    fn title(&mut self, p: Player, title: String, subtitle: Option<String>, times: TitleTimes) {
        match messages(&self.services) {
            Some(svc) => svc.title(&self.plugin, &p, &title, subtitle.as_deref(), times),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "title");
            }
        }
    }

    fn broadcast(&mut self, text: String) {
        match messages(&self.services) {
            Some(svc) => svc.broadcast(&self.plugin, &text),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "broadcast");
            }
        }
    }

    fn prefix(&mut self, p: Player) -> String {
        match messages(&self.services) {
            Some(svc) => svc.prefix(&self.plugin, &p),
            None => {
                self.service_unavailable(MESSAGES_SERVICE, "prefix");
                String::new()
            }
        }
    }
}

impl bindings::aether::sdk::players::Host for StoreData {
    fn online(&mut self) -> Vec<Player> {
        match players(&self.services) {
            Some(svc) => svc.online(&self.plugin),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "online");
                Vec::new()
            }
        }
    }

    fn by_name(&mut self, name: String) -> Option<Player> {
        match players(&self.services) {
            Some(svc) => svc.by_name(&self.plugin, &name),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "by-name");
                None
            }
        }
    }

    fn by_id(&mut self, id: u64) -> Option<Player> {
        match players(&self.services) {
            Some(svc) => svc.by_id(&self.plugin, id),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "by-id");
                None
            }
        }
    }

    fn offline_name(&mut self, uuid_low: u64, uuid_high: u64) -> Option<String> {
        match players(&self.services) {
            Some(svc) => svc.offline_name(&self.plugin, uuid_low, uuid_high),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "offline-name");
                None
            }
        }
    }

    fn teleport(&mut self, p: Player, to: Pos, cause: TeleportCause) -> Result<(), String> {
        match players(&self.services) {
            Some(svc) => svc.teleport(&self.plugin, &p, to, cause),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "teleport");
                Err("player service unavailable".to_string())
            }
        }
    }

    fn give(&mut self, p: Player, item: String, count: u32) -> u64 {
        match players(&self.services) {
            Some(svc) => svc.give(&self.plugin, &p, &item, count),
            None => {
                self.service_unavailable(PLAYERS_SERVICE, "give");
                count as u64
            }
        }
    }
}

/// Config is host-backed sugar rather than a registry service: the bridge reads
/// the caller's own `[plugins.<id>]` values directly, so it works whenever the
/// host does, independent of any provider.
impl bindings::aether::sdk::config::Host for StoreData {
    fn value(&mut self, key: String) -> Option<String> {
        self.config
            .get(&key)
            .cloned()
            .or_else(|| self.backend.config(&self.plugin, &key))
    }

    fn revision(&mut self) -> u64 {
        self.backend.config_revision(&self.plugin)
    }
}

impl bindings::aether::sdk::scheduler::Host for StoreData {
    fn after(&mut self, ticks: u64) -> u64 {
        match scheduler(&self.services) {
            Some(svc) => svc.after(&self.plugin, ticks),
            None => {
                self.service_unavailable(SCHEDULER_SERVICE, "after");
                0
            }
        }
    }

    fn every(&mut self, period: u64) -> u64 {
        match scheduler(&self.services) {
            Some(svc) => svc.every(&self.plugin, period),
            None => {
                self.service_unavailable(SCHEDULER_SERVICE, "every");
                0
            }
        }
    }

    fn cancel(&mut self, id: u64) -> bool {
        match scheduler(&self.services) {
            Some(svc) => svc.cancel(&self.plugin, id),
            None => {
                self.service_unavailable(SCHEDULER_SERVICE, "cancel");
                false
            }
        }
    }

    fn poll(&mut self) -> Vec<u64> {
        match scheduler(&self.services) {
            Some(svc) => svc.poll(&self.plugin),
            None => {
                self.service_unavailable(SCHEDULER_SERVICE, "poll");
                Vec::new()
            }
        }
    }
}

impl bindings::aether::sdk::cooldowns::Host for StoreData {
    fn ready(&mut self, key: String, ticks: u64) -> bool {
        match cooldowns(&self.services) {
            Some(svc) => svc.ready(&self.plugin, &key, ticks),
            None => {
                self.service_unavailable(COOLDOWNS_SERVICE, "ready");
                // Fail open on cooldowns would let a spam guard be bypassed;
                // fail closed by treating the call as on cooldown.
                false
            }
        }
    }

    fn remaining(&mut self, key: String) -> u64 {
        match cooldowns(&self.services) {
            Some(svc) => svc.remaining(&self.plugin, &key),
            None => {
                self.service_unavailable(COOLDOWNS_SERVICE, "remaining");
                0
            }
        }
    }
}

impl bindings::aether::sdk::confirm::Host for StoreData {
    fn request(&mut self, p: Player, prompt: String, token: String) -> bool {
        match confirm(&self.services) {
            Some(svc) => svc.request(&self.plugin, &p, &prompt, &token),
            None => {
                self.service_unavailable(CONFIRM_SERVICE, "request");
                false
            }
        }
    }

    fn check(&mut self, p: Player, token: String) -> bool {
        match confirm(&self.services) {
            Some(svc) => svc.check(&self.plugin, &p, &token),
            None => {
                self.service_unavailable(CONFIRM_SERVICE, "check");
                false
            }
        }
    }
}
