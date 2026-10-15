//! The `sidebar` service: the typed surface other plugins call to put text on
//! a player's sidebar.
//!
//! The **policy** — which lines, resolved from config and placeholders, and
//! when to redraw — lives in the sidebar plugin (`plugins/sidebar`). This module
//! is only the shared type and registry plumbing, the same shape the SDK's
//! services use: a provider registers into the [`ServiceRegistry`], a consumer
//! resolves it once and calls a method.
//!
//! A **placeholder** is a `%name%` token another plugin fills in. Because a
//! native provider can hand over a closure, the service takes one directly; a
//! WASM provider would need a host import and a new package version, so only
//! native consumers register placeholders today (documented in the plugin
//! guide).

use std::sync::Arc;

use crate::service::ServiceRegistry;

/// A player's sidebar as the core draws it: a title and up to fifteen lines of
/// plain text with legacy `§` colour codes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidebarBoard {
    pub title: String,
    pub lines: Vec<String>,
}

/// The extra per-player facts a sidebar can show that are not on the minimal
/// [`crate::Player`] record: the position, the world, the ping and the survival
/// stats. A host that cannot answer returns `None`.
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
    /// World time of day, in ticks, for `%time%`.
    pub time: i64,
}

/// A closure that resolves a `%name%` placeholder, or `None` to leave the token
/// as-is. Shared and callable from the tick thread; it must not block.
pub type PlaceholderFn = Arc<dyn Fn(&PlaceholderQuery) -> Option<String> + Send + Sync>;

/// The service other plugins resolve to put text on a sidebar.
pub trait SidebarService: Send + Sync {
    /// Register (or replace) a placeholder. `name` is the token without its
    /// surrounding `%`. Returns whether an existing provider was replaced.
    fn register_placeholder(&self, name: String, provider: PlaceholderFn) -> bool;

    /// Set line `index` (0 is the top) for `player`, overriding config.
    fn set_line(&self, player: u64, index: usize, text: String);

    /// Drop an override, so config's line shows again.
    fn clear_line(&self, player: u64, index: usize);

    /// Show the sidebar to `player`.
    fn show(&self, player: u64);

    /// Hide the sidebar from `player`.
    fn hide(&self, player: u64);

    /// Toggle and return the new visibility.
    fn toggle(&self, player: u64) -> bool;
}

/// The service name, for the registry.
pub const SIDEBAR_SERVICE: &str = "sidebar";
/// The service version.
pub const SERVICE_VERSION: &str = "0.1";

/// A concrete wrapper so the registry, which downcasts by concrete type, can
/// hold a trait object. Mirrors the SDK's `*Provider` newtypes.
struct SidebarProvider(Arc<dyn SidebarService>);

/// Publish a sidebar provider. Two providers under one identity is a
/// configuration error, so the second is refused.
pub fn register_sidebar(
    reg: &ServiceRegistry,
    provider: Arc<dyn SidebarService>,
) -> Result<(), String> {
    reg.register(
        SIDEBAR_SERVICE,
        SERVICE_VERSION,
        Arc::new(SidebarProvider(provider)),
    )
}

/// Resolve the sidebar provider, if one is registered.
pub fn sidebar(reg: &ServiceRegistry) -> Option<Arc<dyn SidebarService>> {
    reg.get::<SidebarProvider>(SIDEBAR_SERVICE, SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}
