//! The `sidebar` service: the typed surface other plugins call to put text on
//! a player's sidebar.
//!
//! The **policy** — which lines, resolved from config and placeholders, and
//! when to redraw — lives in the sidebar plugin (`plugins/sidebar`). This module
//! is only the shared type and registry plumbing, the same shape the SDK's
//! services use: a provider registers into the [`ServiceRegistry`], a consumer
//! resolves it once and calls a method.
//!
//! Placeholders are **not** here: they are shared with menus, chat and titles,
//! so their registry is the SDK's `placeholders` service (see
//! [`crate::placeholders`]).

use std::sync::Arc;

use crate::service::ServiceRegistry;

/// A player's sidebar as the core draws it: a title and up to fifteen lines of
/// plain text with legacy `§` colour codes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidebarBoard {
    pub title: String,
    pub lines: Vec<String>,
}

/// The service other plugins resolve to put text on a sidebar.
pub trait SidebarService: Send + Sync {
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
