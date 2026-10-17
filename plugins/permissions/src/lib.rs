//! The LuckPerms-style **permissions** plugin (CMD-33a): `/lp user|group …`.
//!
//! The model is the core's (CMD-01/CMD-02): groups, inheritance, per-world
//! context, wildcard nodes and the snapshot/persist store. This plugin is the
//! management surface only. It resolves the server's `permissions.admin`
//! service at load and never keeps a second copy of the model.
//!
//! ```text
//! /lp user <user> info
//! /lp user <user> permission set <node> <true|false> [world=<world>]
//! /lp user <user> permission unset <node> [world=<world>]
//! /lp user <user> permission check <node> [world=<world>]
//! /lp user <user> permission list
//! /lp user <user> parent set|add|remove|switch|clear <group>
//! /lp group <group> info
//! /lp group <group> permission set|unset|list …
//! /lp group <group> parent set|add|remove|switch|clear <group>
//! /lp creategroup <name>        /lp deletegroup <name>
//! /lp info                      /lp help
//! ```
//!
//! `lp` is also reachable as `/luckperms` and `/perms`. Op level 4 is the
//! command gate; each subcommand additionally checks its `luckperms.*` node.
//! Reads are snapshot reads and writes are the core's copy-and-swap updates,
//! persisted immediately; every successful write is also journalled as an
//! audit event, so who changed which node/parent is recoverable.

use std::sync::Arc;

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::permissions::{
    self, Check, GroupInfo, NodeEntry, PermissionsAdmin, Source, UserInfo,
};
use aether_plugin::{
    CommandResult, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

/// The plugin id: its log tag and config key.
pub const ID: &str = "permissions";

/// The short command (`/lp`).
pub const COMMAND: &str = "lp";
/// The long alias (`/luckperms`).
pub const COMMAND_LONG: &str = "luckperms";
/// The other LuckPerms alias (`/perms`).
pub const COMMAND_SHORT_ALT: &str = "perms";

/// The command gate: op level 4, then the per-subcommand `luckperms.*` node.
pub const NODE_COMMAND: &str = "luckperms.command";

/// The `op` group, which cannot be deleted.
pub const OP_GROUP: &str = "op";

/// Permission nodes, mirroring LuckPerms' layout.
pub const NODE_INFO: &str = "luckperms.info";
pub const NODE_CREATE_GROUP: &str = "luckperms.creategroup";
pub const NODE_DELETE_GROUP: &str = "luckperms.deletegroup";
pub const NODE_USER_INFO: &str = "luckperms.user.info";
pub const NODE_USER_SET: &str = "luckperms.user.permission.set";
pub const NODE_USER_UNSET: &str = "luckperms.user.permission.unset";
pub const NODE_USER_CHECK: &str = "luckperms.user.permission.check";
pub const NODE_USER_LIST: &str = "luckperms.user.permission.list";
pub const NODE_USER_PARENT: &str = "luckperms.user.parent";
pub const NODE_GROUP_INFO: &str = "luckperms.group.info";
pub const NODE_GROUP_SET: &str = "luckperms.group.permission.set";
pub const NODE_GROUP_UNSET: &str = "luckperms.group.permission.unset";
pub const NODE_GROUP_CHECK: &str = "luckperms.group.permission.check";
pub const NODE_GROUP_LIST: &str = "luckperms.group.permission.list";
pub const NODE_GROUP_PARENT: &str = "luckperms.group.parent";

/// The plugin manifest.
pub fn manifest() -> Manifest {
    let command = |name: &str| CommandDecl::Detailed {
        name: name.to_string(),
        node: NODE_COMMAND.to_string(),
        op: 4,
    };
    Manifest {
        id: ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Permissions".to_string(),
        priority: 50,
        capabilities: vec!["log".to_string(), "players".to_string()],
        subscribe: vec!["on-command".to_string()],
        commands: vec![
            command(COMMAND),
            command(COMMAND_LONG),
            command(COMMAND_SHORT_ALT),
        ],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: vec![format!("{}@{}", permissions::SERVICE, permissions::VERSION)],
    }
}

/// The build-time registration the server links in.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(PermissionsPlugin::default()),
    }
}

/// The `/lp` command handler.
#[derive(Default)]
pub struct PermissionsPlugin {
    admin: Option<Arc<dyn PermissionsAdmin>>,
}

impl PermissionsPlugin {
    /// Build a plugin with an injected admin store (tests).
    pub fn with_admin(admin: Arc<dyn PermissionsAdmin>) -> PermissionsPlugin {
        PermissionsPlugin { admin: Some(admin) }
    }

    fn admin(&self) -> Result<&Arc<dyn PermissionsAdmin>, String> {
        self.admin
            .as_ref()
            .ok_or_else(|| "permissions admin service is not available".to_string())
    }

    /// The op-4 gate then the subcommand's `luckperms.*` node. An explicit deny
    /// on that node always wins, exactly as the core resolves it.
    fn may(ctx: &NativeContext, p: &Player, node: &str) -> bool {
        ctx.backend.permission_check(ctx.id, p.id, node, 4)
    }

    fn denied() -> String {
        "You do not have permission to use this command!".to_string()
    }

    fn audit(ctx: &NativeContext, p: &Player, action: &str, detail: &str) {
        ctx.backend
            .permissions_journal(ctx.id, p.id, action, detail);
    }

    /// The address a target user is online from, when known. An address-bound
    /// membership only applies to that address, so the admin view/check and
    /// the add/remove messages must use it instead of a blind name lookup.
    fn subject_addr(ctx: &NativeContext, name: &str) -> Option<String> {
        ctx.backend
            .online_players(ctx.id)
            .into_iter()
            .find(|online| online.name.eq_ignore_ascii_case(name))
            .and_then(|online| ctx.backend.player_address(ctx.id, online.id))
    }

    fn dispatch(&self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        match args.first().map(String::as_str) {
            None => {
                if !Self::may(ctx, p, NODE_INFO) {
                    return vec![Self::denied()];
                }
                info_lines()
            }
            Some("help") => {
                if !Self::may(ctx, p, NODE_INFO) {
                    return vec![Self::denied()];
                }
                usage()
            }
            Some("info") => {
                if !Self::may(ctx, p, NODE_INFO) {
                    return vec![Self::denied()];
                }
                info_lines()
            }
            Some("user") => self.user(ctx, p, args),
            Some("group") => self.group(ctx, p, args),
            Some("creategroup") => self.create_group(ctx, p, args),
            Some("deletegroup") => self.delete_group(ctx, p, args),
            Some(other) => vec![format!("Unknown command `{other}`. Try /lp help.")],
        }
    }

    fn user(&self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        let Some(name) = args.get(1).filter(|n| !n.is_empty()) else {
            return vec!["Usage: /lp user <user> info|permission|parent".to_string()];
        };
        match args.get(2).map(String::as_str) {
            None | Some("info") => {
                if !Self::may(ctx, p, NODE_USER_INFO) {
                    return vec![Self::denied()];
                }
                let addr = Self::subject_addr(ctx, name);
                match self
                    .admin()
                    .and_then(|a| a.user(name, addr.as_deref()).ok_or_else(none))
                {
                    Ok(info) => user_info_lines(&info),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            Some("permission") => {
                self.user_permission(ctx, p, name, args.get(3..).unwrap_or_default())
            }
            Some("parent") => self.user_parent(ctx, p, name, args.get(3..).unwrap_or_default()),
            Some(other) => vec![format!(
                "Unknown user command `{other}`. Try /lp user <user> info|permission|parent."
            )],
        }
    }

    fn group(&self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        let Some(id) = args.get(1).filter(|n| !n.is_empty()) else {
            return vec!["Usage: /lp group <group> info|permission|parent".to_string()];
        };
        match args.get(2).map(String::as_str) {
            None | Some("info") => {
                if !Self::may(ctx, p, NODE_GROUP_INFO) {
                    return vec![Self::denied()];
                }
                match self.admin().and_then(|a| a.group(id).ok_or_else(none)) {
                    Ok(info) => group_info_lines(&info),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            Some("permission") => {
                self.group_permission(ctx, p, id, args.get(3..).unwrap_or_default())
            }
            Some("parent") => self.group_parent(ctx, p, id, args.get(3..).unwrap_or_default()),
            Some(other) => vec![format!(
                "Unknown group command `{other}`. Try /lp group <group> info|permission|parent."
            )],
        }
    }

    fn user_permission(
        &self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        rest: &[String],
    ) -> Vec<String> {
        let Some(action) = rest.first().map(String::as_str) else {
            return vec!["Usage: /lp user <user> permission set|unset|check|list".to_string()];
        };
        match action {
            "set" => {
                if !Self::may(ctx, p, NODE_USER_SET) {
                    return vec![Self::denied()];
                }
                let Some((node, value)) = parse_node_value(rest) else {
                    return vec![
                        "Usage: /lp user <user> permission set <node> <true|false> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[3..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                match self
                    .admin()
                    .and_then(|a| a.set_user_node(name, node, context.as_deref(), value))
                {
                    Ok(()) => {
                        Self::audit(
                            ctx,
                            p,
                            "permission-set",
                            &format!(
                                "user {name} {node} {value}{}",
                                audit_context(context.as_deref())
                            ),
                        );
                        vec![format!(
                            "Set {node} to {value} for {name} in context {}.",
                            context_label(context.as_deref())
                        )]
                    }
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "unset" => {
                if !Self::may(ctx, p, NODE_USER_UNSET) {
                    return vec![Self::denied()];
                }
                let Some(node) = rest.get(1) else {
                    return vec![
                        "Usage: /lp user <user> permission unset <node> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[2..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                match self
                    .admin()
                    .and_then(|a| a.unset_user_node(name, node, context.as_deref()))
                {
                    Ok(()) => {
                        Self::audit(
                            ctx,
                            p,
                            "permission-unset",
                            &format!("user {name} {node}{}", audit_context(context.as_deref())),
                        );
                        vec![format!(
                            "Unset {node} for {name} in context {}.",
                            context_label(context.as_deref())
                        )]
                    }
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "check" => {
                if !Self::may(ctx, p, NODE_USER_CHECK) {
                    return vec![Self::denied()];
                }
                let Some(node) = rest.get(1) else {
                    return vec![
                        "Usage: /lp user <user> permission check <node> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[2..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                let addr = Self::subject_addr(ctx, name);
                match self.admin() {
                    Ok(admin) => check_lines(
                        node,
                        context.as_deref(),
                        admin.check_user(name, addr.as_deref(), node, context.as_deref()),
                    ),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "list" => {
                if !Self::may(ctx, p, NODE_USER_LIST) {
                    return vec![Self::denied()];
                }
                let addr = Self::subject_addr(ctx, name);
                match self
                    .admin()
                    .and_then(|a| a.user(name, addr.as_deref()).ok_or_else(none))
                {
                    Ok(info) => self.user_permission_lines(name, &info),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            other => vec![format!(
                "Unknown permission command `{other}`. Try set|unset|check|list."
            )],
        }
    }

    fn group_permission(
        &self,
        ctx: &mut NativeContext,
        p: &Player,
        id: &str,
        rest: &[String],
    ) -> Vec<String> {
        let Some(action) = rest.first().map(String::as_str) else {
            return vec!["Usage: /lp group <group> permission set|unset|check|list".to_string()];
        };
        match action {
            "set" => {
                if !Self::may(ctx, p, NODE_GROUP_SET) {
                    return vec![Self::denied()];
                }
                let Some((node, value)) = parse_node_value(rest) else {
                    return vec![
                        "Usage: /lp group <group> permission set <node> <true|false> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[3..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                match self
                    .admin()
                    .and_then(|a| a.set_group_node(id, node, context.as_deref(), value))
                {
                    Ok(()) => {
                        Self::audit(
                            ctx,
                            p,
                            "permission-set",
                            &format!(
                                "group {id} {node} {value}{}",
                                audit_context(context.as_deref())
                            ),
                        );
                        vec![format!(
                            "Set {node} to {value} for {id} in context {}.",
                            context_label(context.as_deref())
                        )]
                    }
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "unset" => {
                if !Self::may(ctx, p, NODE_GROUP_UNSET) {
                    return vec![Self::denied()];
                }
                let Some(node) = rest.get(1) else {
                    return vec![
                        "Usage: /lp group <group> permission unset <node> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[2..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                match self
                    .admin()
                    .and_then(|a| a.unset_group_node(id, node, context.as_deref()))
                {
                    Ok(()) => {
                        Self::audit(
                            ctx,
                            p,
                            "permission-unset",
                            &format!("group {id} {node}{}", audit_context(context.as_deref())),
                        );
                        vec![format!(
                            "Unset {node} for {id} in context {}.",
                            context_label(context.as_deref())
                        )]
                    }
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "check" => {
                if !Self::may(ctx, p, NODE_GROUP_CHECK) {
                    return vec![Self::denied()];
                }
                let Some(node) = rest.get(1) else {
                    return vec![
                        "Usage: /lp group <group> permission check <node> [world=<world>]"
                            .to_string(),
                    ];
                };
                let context = match parse_context(&rest[2..]) {
                    Ok(c) => c,
                    Err(e) => return vec![e],
                };
                match self.admin() {
                    Ok(admin) => check_lines(
                        node,
                        context.as_deref(),
                        admin.check_group(id, node, context.as_deref()),
                    ),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            "list" => {
                if !Self::may(ctx, p, NODE_GROUP_LIST) {
                    return vec![Self::denied()];
                }
                match self.admin().and_then(|a| a.group(id).ok_or_else(none)) {
                    Ok(info) => self.group_permission_lines(id, &info),
                    Err(e) => vec![format!("Error: {e}")],
                }
            }
            other => vec![format!(
                "Unknown permission command `{other}`. Try set|unset|check|list."
            )],
        }
    }

    fn user_parent(
        &self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        rest: &[String],
    ) -> Vec<String> {
        if !Self::may(ctx, p, NODE_USER_PARENT) {
            return vec![Self::denied()];
        }
        let Some(action) = rest.first().map(String::as_str) else {
            return vec![
                "Usage: /lp user <user> parent set|add|remove|switch|clear <group>".to_string(),
            ];
        };
        let Some(admin) = self.admin().ok() else {
            return vec!["Error: permissions admin service is not available".to_string()];
        };
        let addr = Self::subject_addr(ctx, name);
        let Some(current) = admin.user(name, addr.as_deref()) else {
            return vec!["Error: a user name is empty".to_string()];
        };
        if action == "clear" {
            return match admin.set_user_groups(name, Vec::new()) {
                Ok(()) => {
                    Self::audit(ctx, p, "parent-clear", &format!("user {name}"));
                    vec![format!(
                        "{name}'s parents were cleared in context global. ({} groups were removed.)",
                        current.groups.len()
                    )]
                }
                Err(e) => vec![format!("Error: {e}")],
            };
        }
        let Some(group) = rest.get(1).filter(|g| !g.is_empty()) else {
            return vec!["Usage: /lp user <user> parent set|add|remove|switch <group>".to_string()];
        };
        let already = current.groups.iter().any(|g| g == group);
        let result = match action {
            "set" | "switch" => admin.set_user_groups(name, vec![group.clone()]),
            "add" => admin.add_user_group(name, group),
            "remove" => admin.remove_user_group(name, group),
            other => {
                return vec![format!(
                    "Unknown parent command `{other}`. Try set|add|remove|switch|clear."
                )]
            }
        };
        match result {
            Ok(()) => {
                Self::audit(ctx, p, "parent", &format!("user {name} {action} {group}"));
                match action {
                    "add" if already => {
                        vec![format!("{name} already inherits from {group} in context global.")]
                    }
                    "remove" if !already => {
                        vec![format!("{name} does not inherit from {group} in context global.")]
                    }
                    "add" => vec![format!(
                        "{name} now inherits permissions from {group} in context global."
                    )],
                    "remove" => vec![format!(
                        "{name} no longer inherits permissions from {group} in context global."
                    )],
                    _ => vec![format!(
                        "{name} had their existing parent groups cleared, and now only inherits {group} in context global."
                    )],
                }
            }
            Err(e) => vec![format!("Error: {e}")],
        }
    }

    fn group_parent(
        &self,
        ctx: &mut NativeContext,
        p: &Player,
        id: &str,
        rest: &[String],
    ) -> Vec<String> {
        if !Self::may(ctx, p, NODE_GROUP_PARENT) {
            return vec![Self::denied()];
        }
        let Some(action) = rest.first().map(String::as_str) else {
            return vec![
                "Usage: /lp group <group> parent set|add|remove|switch|clear <group>".to_string(),
            ];
        };
        let Some(admin) = self.admin().ok() else {
            return vec!["Error: permissions admin service is not available".to_string()];
        };
        let Some(current) = admin.group(id) else {
            return vec![format!("Error: no group `{id}`")];
        };
        if action == "clear" {
            return match admin.set_group_parents(id, Vec::new()) {
                Ok(()) => {
                    Self::audit(ctx, p, "parent-clear", &format!("group {id}"));
                    vec![format!(
                        "{id}'s parents were cleared in context global. ({} parent groups were removed.)",
                        current.parents.len()
                    )]
                }
                Err(e) => vec![format!("Error: {e}")],
            };
        }
        let Some(parent) = rest.get(1).filter(|g| !g.is_empty()) else {
            return vec![
                "Usage: /lp group <group> parent set|add|remove|switch <group>".to_string(),
            ];
        };
        let already = current.parents.iter().any(|g| g == parent);
        let result = match action {
            "set" | "switch" => admin.set_group_parents(id, vec![parent.clone()]),
            "add" => admin.add_group_parent(id, parent),
            "remove" => admin.remove_group_parent(id, parent),
            other => {
                return vec![format!(
                    "Unknown parent command `{other}`. Try set|add|remove|switch|clear."
                )]
            }
        };
        match result {
            Ok(()) => {
                Self::audit(ctx, p, "parent", &format!("group {id} {action} {parent}"));
                match action {
                    "add" if already => {
                        vec![format!("{id} already inherits from {parent} in context global.")]
                    }
                    "remove" if !already => {
                        vec![format!("{id} does not inherit from {parent} in context global.")]
                    }
                    "add" => vec![format!(
                        "{id} now inherits permissions from {parent} in context global."
                    )],
                    "remove" => vec![format!(
                        "{id} no longer inherits permissions from {parent} in context global."
                    )],
                    _ => vec![format!(
                        "{id} had their existing parent groups cleared, and now only inherits {parent} in context global."
                    )],
                }
            }
            Err(e) => vec![format!("Error: {e}")],
        }
    }

    fn create_group(&self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if !Self::may(ctx, p, NODE_CREATE_GROUP) {
            return vec![Self::denied()];
        }
        let Some(name) = args.get(1).filter(|n| !n.is_empty()) else {
            return vec!["Usage: /lp creategroup <name>".to_string()];
        };
        match self.admin().and_then(|a| a.create_group(name)) {
            Ok(()) => {
                Self::audit(ctx, p, "creategroup", name);
                vec![format!("{name} was successfully created.")]
            }
            Err(e) => vec![format!("Error: {e}")],
        }
    }

    fn delete_group(&self, ctx: &mut NativeContext, p: &Player, args: &[String]) -> Vec<String> {
        if !Self::may(ctx, p, NODE_DELETE_GROUP) {
            return vec![Self::denied()];
        }
        let Some(name) = args.get(1).filter(|n| !n.is_empty()) else {
            return vec!["Usage: /lp deletegroup <name>".to_string()];
        };
        match self.admin().and_then(|a| a.delete_group(name)) {
            Ok(()) => {
                Self::audit(ctx, p, "deletegroup", name);
                vec![format!("{name} was successfully deleted.")]
            }
            Err(e) => vec![format!("Error: {e}")],
        }
    }

    fn user_permission_lines(&self, name: &str, info: &UserInfo) -> Vec<String> {
        let Ok(admin) = self.admin() else {
            return vec!["Error: permissions admin service is not available".to_string()];
        };
        let mut out = vec![format!("{name}'s permissions:")];
        let mut seen: Vec<(String, Option<String>)> = Vec::new();
        for entry in &info.nodes {
            seen.push((entry.node.clone(), entry.context.clone()));
            out.push(entry_line(entry, &Source::User(info.name.clone())));
        }
        for group_id in &info.inherited_groups {
            if let Some(group) = admin.group(group_id) {
                for entry in &group.nodes {
                    let key = (entry.node.clone(), entry.context.clone());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    out.push(entry_line(entry, &Source::Group(group_id.clone())));
                }
            }
        }
        if out.len() == 1 {
            out.push("No permissions.".to_string());
        }
        out
    }

    fn group_permission_lines(&self, id: &str, info: &GroupInfo) -> Vec<String> {
        let Ok(admin) = self.admin() else {
            return vec!["Error: permissions admin service is not available".to_string()];
        };
        let mut out = vec![format!("{id}'s permissions:")];
        let mut seen: Vec<(String, Option<String>)> = Vec::new();
        for entry in &info.nodes {
            seen.push((entry.node.clone(), entry.context.clone()));
            out.push(entry_line(entry, &Source::Group(id.to_string())));
        }
        for parent in &info.inherited_parents {
            if let Some(group) = admin.group(parent) {
                for entry in &group.nodes {
                    let key = (entry.node.clone(), entry.context.clone());
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    out.push(entry_line(entry, &Source::Group(parent.clone())));
                }
            }
        }
        if out.len() == 1 {
            out.push("No permissions.".to_string());
        }
        out
    }
}

impl NativePlugin for PermissionsPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let admin = permissions::get(ctx.services).ok_or_else(|| {
            format!(
                "required service `{}@{}` is not registered",
                permissions::SERVICE,
                permissions::VERSION
            )
        })?;
        self.admin = Some(admin);
        ctx.log(LogLevel::Info, "permissions ready: /lp (CMD-33a)");
        Ok(())
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        if ![COMMAND, COMMAND_LONG, COMMAND_SHORT_ALT].contains(&name) {
            return None;
        }
        Some(CommandResult {
            handled: true,
            lines: self.dispatch(ctx, p, args),
        })
    }

    fn on_complete(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Vec<String> {
        if ![COMMAND, COMMAND_LONG, COMMAND_SHORT_ALT].contains(&name) {
            return Vec::new();
        }
        if !Self::may(ctx, p, NODE_COMMAND) {
            return Vec::new();
        }
        let Ok(admin) = self.admin() else {
            return Vec::new();
        };
        let mut users = admin.users();
        for online in ctx.backend.online_players(ctx.id) {
            if !users.iter().any(|u| u.eq_ignore_ascii_case(&online.name)) {
                users.push(online.name);
            }
        }
        let groups = admin.groups();
        let nodes = admin.nodes();
        let worlds: Vec<String> = ctx
            .backend
            .player_position(ctx.id, p.id)
            .map(|(world, _, _, _)| vec![world])
            .unwrap_or_default();
        complete(args, &users, &groups, &nodes, &worlds)
    }
}

fn none() -> String {
    "not found".to_string()
}

/// Parse `set <node> <true|false>` from the tail after `permission`.
fn parse_node_value(rest: &[String]) -> Option<(&str, bool)> {
    let node = rest.get(1)?.as_str();
    let value = match rest.get(2)?.as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some((node, value))
}

/// Parse the optional `world=<world>` (or `context=<world>`) tail.
fn parse_context(rest: &[String]) -> Result<Option<String>, String> {
    let mut context = None;
    for arg in rest {
        if arg.is_empty() {
            continue;
        }
        match arg.split_once('=') {
            Some(("world" | "context", value)) if !value.is_empty() => {
                context = Some(value.to_string());
            }
            Some(("world" | "context", _)) => {
                return Err("Error: a context value is empty.".to_string())
            }
            _ => return Err(format!("Error: unknown argument `{arg}`.")),
        }
    }
    Ok(context)
}

fn info_lines() -> Vec<String> {
    vec![
        "Aether Permissions (LuckPerms-compatible /lp)".to_string(),
        "Use /lp user <user> info or /lp group <group> info.".to_string(),
    ]
}

fn usage() -> Vec<String> {
    vec![
        "Permissions commands:".to_string(),
        "  /lp user <user> info".to_string(),
        "  /lp user <user> permission set <node> <true|false> [world=<world>]".to_string(),
        "  /lp user <user> permission unset <node> [world=<world>]".to_string(),
        "  /lp user <user> permission check <node> [world=<world>]".to_string(),
        "  /lp user <user> permission list".to_string(),
        "  /lp user <user> parent set|add|remove|switch|clear <group>".to_string(),
        "  /lp group <group> info".to_string(),
        "  /lp group <group> permission set|unset|check|list …".to_string(),
        "  /lp group <group> parent set|add|remove|switch|clear <group>".to_string(),
        "  /lp creategroup <name>   /lp deletegroup <name>".to_string(),
        "  /lp info                 /lp help".to_string(),
    ]
}

fn user_info_lines(info: &UserInfo) -> Vec<String> {
    let groups = if info.groups.is_empty() {
        "none".to_string()
    } else {
        info.groups.join(", ")
    };
    let inherited = if info.inherited_groups.is_empty() {
        "none".to_string()
    } else {
        info.inherited_groups.join(", ")
    };
    let primary = info
        .groups
        .first()
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    let memberships = if info.memberships.is_empty() {
        "none".to_string()
    } else {
        info.memberships
            .iter()
            .map(|membership| match &membership.addr {
                Some(addr) => format!("{}@{}", membership.group, addr),
                None => membership.group.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        format!("User: {}", info.name),
        format!("Level: {}", info.level),
        format!("Primary group: {primary}"),
        format!("Parent groups: {groups}"),
        format!("Memberships: {memberships}"),
        format!("Inherited groups: {inherited}"),
        format!("Nodes: {}", info.nodes.len()),
    ]
}

fn group_info_lines(info: &GroupInfo) -> Vec<String> {
    let parents = if info.parents.is_empty() {
        "none".to_string()
    } else {
        info.parents.join(", ")
    };
    let inherited = if info.inherited_parents.is_empty() {
        "none".to_string()
    } else {
        info.inherited_parents.join(", ")
    };
    let members = if info.members.is_empty() {
        "none".to_string()
    } else {
        info.members.join(", ")
    };
    vec![
        format!("Group: {}", info.id),
        format!("Level: {}", info.level),
        format!("Priority: {}", info.priority),
        format!("Built-in: {}", if info.builtin { "yes" } else { "no" }),
        format!("Parents: {parents}"),
        format!("Inherited parents: {inherited}"),
        format!("Members: {members}"),
        format!("Nodes: {}", info.nodes.len()),
    ]
}

fn check_lines(node: &str, context: Option<&str>, check: Check) -> Vec<String> {
    vec![
        format!("Permission check for {node}:"),
        format!("    Result: {}", check.value),
        format!("    Processor: {}", check.source.label()),
        format!("    Context: {}", context_display(context)),
    ]
}

/// The LuckPerms context label: `global` when no world context was given, and
/// `world=<name>` when one was.
fn context_display(context: Option<&str>) -> String {
    match context {
        Some(context) => format!("world={context}"),
        None => "global".to_string(),
    }
}

/// The context label set/unset report.
fn context_label(context: Option<&str>) -> &str {
    context.unwrap_or("global")
}

/// The ` world=<name>` suffix for an audit detail (empty for a global write).
fn audit_context(context: Option<&str>) -> String {
    match context {
        Some(context) => format!(" world={context}"),
        None => String::new(),
    }
}

fn entry_line(entry: &NodeEntry, source: &Source) -> String {
    let node = match &entry.context {
        Some(context) => format!("{}#{}", entry.node, context),
        None => entry.node.clone(),
    };
    format!("- {node} = {} (from {})", entry.value, source.label())
}

/// Tab-completion for `/lp`. `args` are the tokens after the command name, the
/// last one possibly empty (the player typed a space). The candidate lists are
/// the live store's users/groups/nodes plus the online players.
pub fn complete(
    args: &[String],
    users: &[String],
    groups: &[String],
    nodes: &[String],
    worlds: &[String],
) -> Vec<String> {
    const TOP: &[&str] = &[
        "user",
        "group",
        "creategroup",
        "deletegroup",
        "info",
        "help",
    ];
    let partial = args.last().map(String::as_str).unwrap_or("");
    let before = if args.is_empty() {
        &args[..0]
    } else {
        &args[..args.len() - 1]
    };
    let words: Vec<&str> = before.iter().map(String::as_str).collect();
    let candidates: Vec<String> = match words.as_slice() {
        [] => TOP.iter().map(|s| s.to_string()).collect(),
        ["user"] => users.to_vec(),
        ["group"] => groups.to_vec(),
        ["user" | "group", _] => ["info", "permission", "parent"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ["user" | "group", _, "permission"] => ["set", "unset", "check", "list"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ["user" | "group", _, "permission", "set" | "unset" | "check"] => nodes.to_vec(),
        ["user" | "group", _, "permission", "set", _] => {
            let mut out = vec!["true".to_string(), "false".to_string()];
            out.extend(worlds.iter().map(|w| format!("world={w}")));
            out
        }
        ["user" | "group", _, "permission", "unset" | "check", _] => {
            worlds.iter().map(|w| format!("world={w}")).collect()
        }
        ["user" | "group", _, "parent"] => ["set", "add", "remove", "switch", "clear"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ["user" | "group", _, "parent", "set" | "add" | "remove" | "switch"] => groups.to_vec(),
        _ => Vec::new(),
    };
    let mut out: Vec<String> = candidates
        .into_iter()
        .filter(|c| {
            c.to_ascii_lowercase()
                .starts_with(&partial.to_ascii_lowercase())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests;
