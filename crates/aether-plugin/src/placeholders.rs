//! The `placeholders` service: one shared `%name%` registry and resolver for
//! **every** consumer that draws text — the sidebar, chest menus, chat, titles.
//!
//! The registry lives in the SDK plugin (`plugins/sdk`), so a value is
//! registered once and shows up wherever the token appears, instead of each
//! consumer keeping its own table. Two ways to fill a token:
//!
//! * a **native** provider hands over a closure ([`PlaceholderFn`]) resolved
//!   against a [`PlaceholderQuery`];
//! * a **WASM** provider cannot hand over a closure, so it declares a name and
//!   pushes a per-player value (`register` + `set`), the same policy/value
//!   split the plugin world requires.
//!
//! This module is the shared surface — the trait, the query type and the
//! registry plumbing, the same shape the other SDK services use. The runtime
//! (built-ins, the tokenizer and a short-lived cache) is the SDK plugin's.

use std::sync::Arc;

use crate::service::ServiceRegistry;
use crate::Player;

/// The extra per-player facts a placeholder may need that are not on the
/// minimal [`Player`] record: position, world, ping and survival stats. A host
/// that cannot answer returns `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerExtra {
    /// Round-trip latency in milliseconds, as the last keep-alive measured.
    pub ping: i32,
    /// `minecraft:`-qualified dimension name.
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub health: f32,
    /// Experience level.
    pub level: i32,
    /// The player's own display name, if a nick or prefix changed it.
    pub display_name: String,
}

/// The facts a [`PlaceholderFn`] is handed when it is asked for a value.
#[derive(Debug, Clone)]
pub struct PlaceholderQuery {
    /// The session entity id.
    pub player: u64,
    /// The requesting player's login name.
    pub name: String,
    /// The extra facts, when the host provides them.
    pub extra: Option<PlayerExtra>,
    /// Current online players.
    pub online: usize,
    /// The configured slot cap.
    pub max_players: u32,
    /// Ticks per second, as the host measures it.
    pub tps: f64,
    /// World time of day, in ticks, for the world clock (`%time%` /
    /// `%world_time%`).
    pub time: i64,
    /// Seconds since the Unix epoch, for the server clock (`%server_time%`).
    pub unix_time: i64,
}

/// A closure that resolves a `%name%` placeholder, or `None` to leave the token
/// as-is. Shared and callable from the tick thread; it must not block.
pub type PlaceholderFn = Arc<dyn Fn(&PlaceholderQuery) -> Option<String> + Send + Sync>;

/// The shared placeholder registry other plugins resolve to feed and read
/// tokens.
///
/// A token is resolved in this order: a **native** provider, then a **pushed**
/// per-player value, then a built-in. An unknown token is left literal, so a
/// typo is visible rather than vanishing.
pub trait PlaceholdersService: Send + Sync {
    /// Register (or replace) `name` with a **native closure**. Returns whether
    /// an existing provider was replaced.
    fn register(&self, caller: &str, name: String, provider: PlaceholderFn) -> bool;

    /// Declare `name` (without `%`) that `caller` will fill with
    /// [`Self::set_value`], so it counts as known before the first value
    /// arrives. Returns whether it was already known.
    fn register_name(&self, caller: &str, name: String) -> bool;

    /// Set `name`'s value for `player`, from `caller`. Implies [`Self::register_name`].
    fn set_value(&self, caller: &str, name: String, player: u64, value: String);

    /// Resolve every `%name%` in `text` for `player`, on `caller`'s behalf.
    fn resolve(&self, caller: &str, player: &Player, text: &str) -> String;
}

/// The service name, for the registry.
pub const PLACEHOLDERS_SERVICE: &str = "sdk.placeholders";
/// The service version.
pub const SERVICE_VERSION: &str = "0.1";

/// A concrete wrapper so the registry, which downcasts by concrete type, can
/// hold a trait object. Mirrors the SDK's `*Provider` newtypes.
struct PlaceholdersProvider(Arc<dyn PlaceholdersService>);

/// Publish a placeholders provider. Two providers under one identity is a
/// configuration error, so the second is refused.
pub fn register_placeholders(
    reg: &ServiceRegistry,
    provider: Arc<dyn PlaceholdersService>,
) -> Result<(), String> {
    reg.register(
        PLACEHOLDERS_SERVICE,
        SERVICE_VERSION,
        Arc::new(PlaceholdersProvider(provider)),
    )
}

/// Resolve the placeholders provider, if one is registered.
pub fn placeholders(reg: &ServiceRegistry) -> Option<Arc<dyn PlaceholdersService>> {
    reg.get::<PlaceholdersProvider>(PLACEHOLDERS_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}
