//! The **permissions admin** service: the typed surface the `/lp` plugin uses
//! to read and edit the core permission store.
//!
//! The store itself is the core's (CMD-01/CMD-02): the same snapshot every
//! command and hot-path gate resolves against. A native plugin cannot name the
//! server crate (that would be a cycle), so the server publishes this trait
//! into the [`ServiceRegistry`] before it loads native plugins and the
//! `permissions` plugin resolves it in `init`.
//!
//! The split is deliberate: the core owns the model, persistence, inheritance
//! and wildcard resolution; the plugin owns only the LuckPerms-style command
//! surface and its text. A write made here goes through the core's
//! copy-and-swap `update`, so a concurrent check sees either the old or the new
//! snapshot, never a half-applied edit.

use std::sync::Arc;

use crate::service::ServiceRegistry;

/// The service name, for the registry.
pub const SERVICE: &str = "permissions.admin";
/// The service version.
pub const VERSION: &str = "0.1";

/// One explicit node, with its optional context (a world key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEntry {
    /// The node name; a stored node may be a wildcard (`*`, `a.b.*`).
    pub node: String,
    /// `None` means everywhere.
    pub context: Option<String>,
    /// `true` grants, `false` denies.
    pub value: bool,
}

/// A user as `/lp user <name> info` sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserInfo {
    /// The name, as resolved by the store (lower-cased for lookup).
    pub name: String,
    /// The effective level (the highest group level).
    pub level: u8,
    /// Direct group memberships.
    pub groups: Vec<String>,
    /// Direct groups plus every group reached by inheritance, in resolution
    /// order.
    pub inherited_groups: Vec<String>,
    /// The user's own explicit nodes.
    pub nodes: Vec<NodeEntry>,
}

/// A group as `/lp group <id> info` sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupInfo {
    /// The group id.
    pub id: String,
    /// The level every member gets.
    pub level: u8,
    /// Whether this is the built-in, non-deletable `op` group.
    pub builtin: bool,
    /// Resolution priority (higher first).
    pub priority: i32,
    /// Direct parents.
    pub parents: Vec<String>,
    /// Parents plus every ancestor, in resolution order.
    pub inherited_parents: Vec<String>,
    /// The group's own explicit nodes.
    pub nodes: Vec<NodeEntry>,
    /// Member names, lower-cased by the store.
    pub members: Vec<String>,
}

/// The holder that decided a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The subject's own explicit node.
    User(String),
    /// A group in the subject's inheritance chain.
    Group(String),
    /// No explicit node; the node's default/op level decided.
    Default,
}

impl Source {
    /// The LuckPerms-style source label.
    pub fn label(&self) -> String {
        match self {
            Source::User(name) => format!("user {name}"),
            Source::Group(id) => format!("group {id}"),
            Source::Default => "default".to_string(),
        }
    }
}

/// The result of `/lp ... permission check`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Whether the node is granted.
    pub value: bool,
    /// Where the answer came from.
    pub source: Source,
}

/// The core permission store, opened for administration by the `/lp` plugin.
///
/// Every method is a snapshot read or a copy-and-swap write; none holds a lock
/// across the operation. `Err` is a user-facing reason (an unknown group, an
/// illegal node, a cycle) and leaves the store unchanged.
pub trait PermissionsAdmin: Send + Sync {
    /// Every group id, for completion and `/lp group`.
    fn groups(&self) -> Vec<String>;
    /// Every player name the store knows, for completion.
    fn users(&self) -> Vec<String>;
    /// Every known node name (declared defaults and explicit patterns).
    fn nodes(&self) -> Vec<String>;

    /// A user's groups and explicit nodes. `None` only for an empty name; any
    /// other name (online, offline, or new) gets a level-0 view.
    fn user(&self, name: &str) -> Option<UserInfo>;
    /// A group, or `None` when it does not exist.
    fn group(&self, id: &str) -> Option<GroupInfo>;

    /// Resolve `node` for a user, with the source that decided it.
    fn check_user(&self, name: &str, node: &str, context: Option<&str>) -> Check;
    /// Resolve `node` for a group (its own nodes, then its parents'), with the
    /// source that decided it.
    fn check_group(&self, id: &str, node: &str, context: Option<&str>) -> Check;

    /// Grant or deny a node on a user.
    fn set_user_node(
        &self,
        name: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String>;
    /// Remove a user's explicit node.
    fn unset_user_node(&self, name: &str, node: &str, context: Option<&str>) -> Result<(), String>;
    /// Grant or deny a node on a group.
    fn set_group_node(
        &self,
        id: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String>;
    /// Remove a group's explicit node.
    fn unset_group_node(&self, id: &str, node: &str, context: Option<&str>) -> Result<(), String>;

    /// Replace a group's parents (a `parent set`/`switch`/`clear`).
    fn set_group_parents(&self, id: &str, parents: Vec<String>) -> Result<(), String>;
    /// Add one parent to a group.
    fn add_group_parent(&self, id: &str, parent: &str) -> Result<(), String>;
    /// Remove one parent from a group.
    fn remove_group_parent(&self, id: &str, parent: &str) -> Result<(), String>;

    /// Replace a user's direct groups (a `parent set`/`switch`/`clear`).
    fn set_user_groups(&self, name: &str, groups: Vec<String>) -> Result<(), String>;
    /// Add one direct group to a user.
    fn add_user_group(&self, name: &str, group: &str) -> Result<(), String>;
    /// Remove one direct group from a user.
    fn remove_user_group(&self, name: &str, group: &str) -> Result<(), String>;

    /// Create a group with level 0. The built-in `op` group already exists.
    fn create_group(&self, id: &str) -> Result<(), String>;
    /// Delete a group. The built-in `op` group is refused; a group in use is
    /// deleted (its members simply lose it, as LuckPerms does).
    fn delete_group(&self, id: &str) -> Result<(), String>;

    /// Persist the current snapshot now. A no-op when the core was never
    /// installed with a world path (unit tests, tools).
    fn save(&self) -> Result<(), String>;
}

/// A concrete wrapper so the registry, which downcasts by concrete type, can
/// hold a trait object. Mirrors the SDK's `*Provider` newtypes.
struct AdminProvider(Arc<dyn PermissionsAdmin>);

/// Publish `service` as the permissions admin provider.
pub fn register(
    registry: &ServiceRegistry,
    service: Arc<dyn PermissionsAdmin>,
) -> Result<(), String> {
    registry.register(SERVICE, VERSION, Arc::new(AdminProvider(service)))
}

/// Resolve the registered permissions admin provider, if any.
pub fn get(registry: &ServiceRegistry) -> Option<Arc<dyn PermissionsAdmin>> {
    registry
        .get::<AdminProvider>(SERVICE, VERSION)
        .map(|p| Arc::clone(&p.0))
}
