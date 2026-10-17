use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use aether_plugin::permissions::{Check, Membership, PermissionsAdmin};
use aether_plugin::service::ServiceRegistry;
use aether_plugin::HostBackend;

#[derive(Clone, Default)]
struct MockUser {
    nodes: Vec<NodeEntry>,
}

#[derive(Clone, Default)]
struct MockGroup {
    parents: Vec<String>,
    nodes: Vec<NodeEntry>,
    /// Member name -> optional required address.
    members: BTreeMap<String, Option<String>>,
    builtin: bool,
}

#[derive(Default)]
struct State {
    users: BTreeMap<String, MockUser>,
    groups: BTreeMap<String, MockGroup>,
    declared: BTreeSet<String>,
}

struct MockAdmin {
    state: Mutex<State>,
    saves: AtomicUsize,
}

impl MockAdmin {
    fn new() -> MockAdmin {
        let mut state = State::default();
        state.groups.insert(
            OP_GROUP.to_string(),
            MockGroup {
                builtin: true,
                ..MockGroup::default()
            },
        );
        MockAdmin {
            state: Mutex::new(state),
            saves: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn lookup(&self, nodes: &[NodeEntry], node: &str, context: Option<&str>) -> Option<bool> {
        if let Some(ctx) = context.filter(|c| !c.is_empty()) {
            if let Some(entry) = nodes
                .iter()
                .find(|e| e.node == node && e.context.as_deref() == Some(ctx))
            {
                return Some(entry.value);
            }
        }
        if let Some(entry) = nodes.iter().find(|e| e.node == node && e.context.is_none()) {
            return Some(entry.value);
        }
        let mut best: Option<(usize, bool, bool)> = None;
        for entry in nodes {
            if !wildcard_matches(&entry.node, node) {
                continue;
            }
            let contextual = entry.context.as_deref() == context.filter(|c| !c.is_empty());
            if entry.context.is_some() && !contextual {
                continue;
            }
            let len = if entry.node == "*" {
                0
            } else {
                entry.node.len()
            };
            // Specificity (prefix length) first, context only on a tie.
            let rank = (len, contextual, entry.value);
            let better = match best {
                None => true,
                Some((bl, bc, _)) => match rank.0.cmp(&bl) {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    std::cmp::Ordering::Equal => rank.1 && !bc,
                },
            };
            if better {
                best = Some(rank);
            }
        }
        best.map(|(_, _, value)| value)
    }

    /// A user's direct groups that are effective from `addr` (a bare member
    /// matches any address; a bound member only its own).
    fn direct_groups(state: &State, name: &str, addr: Option<&str>) -> Vec<String> {
        let key = name.to_ascii_lowercase();
        state
            .groups
            .iter()
            .filter(|(_, group)| match group.members.get(&key) {
                Some(None) => true,
                Some(Some(bound)) => addr == Some(bound.as_str()),
                None => false,
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn inherited(&self, roots: &[String], state: &State) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue: Vec<(String, usize)> = roots.iter().map(|g| (g.clone(), 0)).collect();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        while let Some((id, depth)) = queue.pop() {
            if let Some(&d) = seen.get(&id) {
                if d <= depth {
                    continue;
                }
            }
            seen.insert(id.clone(), depth);
            if let Some(group) = state.groups.get(&id) {
                queue.extend(group.parents.iter().map(|p| (p.clone(), depth + 1)));
            }
        }
        let mut order: Vec<(String, usize)> = seen.into_iter().collect();
        order.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out.extend(order.into_iter().map(|(id, _)| id));
        out
    }

    fn check(
        &self,
        roots: &[String],
        own: &[NodeEntry],
        node: &str,
        context: Option<&str>,
    ) -> Check {
        if let Some(value) = self.lookup(own, node, context) {
            return Check {
                value,
                source: Source::User(roots.first().cloned().unwrap_or_default()),
            };
        }
        let state = self.state();
        for id in self.inherited(roots, &state) {
            if let Some(group) = state.groups.get(&id) {
                if let Some(value) = self.lookup(&group.nodes, node, context) {
                    return Check {
                        value,
                        source: Source::Group(id),
                    };
                }
            }
        }
        Check {
            value: false,
            source: Source::Default,
        }
    }
}

impl PermissionsAdmin for MockAdmin {
    fn groups(&self) -> Vec<String> {
        self.state().groups.keys().cloned().collect()
    }

    fn users(&self) -> Vec<String> {
        let state = self.state();
        let mut names: BTreeSet<String> = state.users.keys().cloned().collect();
        for group in state.groups.values() {
            names.extend(group.members.keys().cloned());
        }
        names.into_iter().collect()
    }

    fn nodes(&self) -> Vec<String> {
        let state = self.state();
        let mut out: BTreeSet<String> = state.declared.clone();
        for user in state.users.values() {
            out.extend(user.nodes.iter().map(|n| n.node.clone()));
        }
        for group in state.groups.values() {
            out.extend(group.nodes.iter().map(|n| n.node.clone()));
        }
        out.into_iter().collect()
    }

    fn user(&self, name: &str, addr: Option<&str>) -> Option<UserInfo> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let key = name.to_ascii_lowercase();
        let state = self.state();
        let user = state.users.get(&key).cloned().unwrap_or_default();
        let groups = Self::direct_groups(&state, &key, addr);
        let inherited = self.inherited(&groups, &state);
        let level = if inherited.iter().any(|g| g == OP_GROUP) {
            4
        } else {
            0
        };
        let mut memberships: Vec<Membership> = Vec::new();
        for (id, group) in &state.groups {
            if let Some(binding) = group.members.get(&key) {
                memberships.push(Membership {
                    group: id.clone(),
                    addr: binding.clone(),
                });
            }
        }
        Some(UserInfo {
            name: key,
            level,
            groups,
            inherited_groups: inherited,
            memberships,
            nodes: user.nodes,
        })
    }

    fn group(&self, id: &str) -> Option<GroupInfo> {
        let state = self.state();
        let group = state.groups.get(id)?;
        let inherited = self
            .inherited(&[id.to_string()], &state)
            .into_iter()
            .filter(|g| g != id)
            .collect();
        Some(GroupInfo {
            id: id.to_string(),
            level: if group.builtin { 4 } else { 0 },
            builtin: group.builtin,
            priority: 0,
            parents: group.parents.clone(),
            inherited_parents: inherited,
            nodes: group.nodes.clone(),
            members: group.members.keys().cloned().collect(),
        })
    }

    fn check_user(
        &self,
        name: &str,
        addr: Option<&str>,
        node: &str,
        context: Option<&str>,
    ) -> Check {
        let key = name.to_ascii_lowercase();
        let state = self.state();
        let user = state.users.get(&key).cloned().unwrap_or_default();
        let groups = Self::direct_groups(&state, &key, addr);
        drop(state);
        let check = self.check(&groups, &user.nodes, node, context);
        if let Source::User(_) = check.source {
            Check {
                value: check.value,
                source: Source::User(key),
            }
        } else {
            check
        }
    }

    fn check_group(&self, id: &str, node: &str, context: Option<&str>) -> Check {
        let state = self.state();
        let Some(group) = state.groups.get(id) else {
            return Check {
                value: false,
                source: Source::Default,
            };
        };
        let check = self.check(&[id.to_string()], &group.nodes, node, context);
        if let Source::User(_) = check.source {
            Check {
                value: check.value,
                source: Source::Group(id.to_string()),
            }
        } else {
            check
        }
    }

    fn set_user_node(
        &self,
        name: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let key = name.trim().to_ascii_lowercase();
        if key.is_empty() {
            return Err("a player name is empty".to_string());
        }
        let mut state = self.state();
        let user = state.users.entry(key).or_default();
        upsert(&mut user.nodes, node, context, Some(value));
        Ok(())
    }

    fn unset_user_node(&self, name: &str, node: &str, context: Option<&str>) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let key = name.trim().to_ascii_lowercase();
        let mut state = self.state();
        if let Some(user) = state.users.get_mut(&key) {
            upsert(&mut user.nodes, node, context, None);
        }
        Ok(())
    }

    fn set_group_node(
        &self,
        id: &str,
        node: &str,
        context: Option<&str>,
        value: bool,
    ) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let mut state = self.state();
        let group = state
            .groups
            .get_mut(id)
            .ok_or_else(|| format!("no group `{id}`"))?;
        upsert(&mut group.nodes, node, context, Some(value));
        Ok(())
    }

    fn unset_group_node(&self, id: &str, node: &str, context: Option<&str>) -> Result<(), String> {
        if !valid_node(node) {
            return Err(format!("`{node}` is not a legal permission node"));
        }
        let mut state = self.state();
        let group = state
            .groups
            .get_mut(id)
            .ok_or_else(|| format!("no group `{id}`"))?;
        upsert(&mut group.nodes, node, context, None);
        Ok(())
    }

    fn set_group_parents(&self, id: &str, parents: Vec<String>) -> Result<(), String> {
        let mut state = self.state();
        if !state.groups.contains_key(id) {
            return Err(format!("no group `{id}`"));
        }
        for parent in &parents {
            if !state.groups.contains_key(parent) {
                return Err(format!("no group `{parent}`"));
            }
        }
        let mut unique: Vec<String> = Vec::new();
        for parent in parents {
            if !unique.contains(&parent) {
                unique.push(parent);
            }
        }
        unique.sort();
        // A cycle check against the proposed edges.
        if unique.iter().any(|p| p == id) {
            return Err(format!(
                "adding parent `{id}` to `{id}` would create an inheritance cycle"
            ));
        }
        let mut trial = state.groups.clone();
        trial.get_mut(id).expect("checked").parents = unique.clone();
        if has_cycle(&trial) {
            return Err(format!(
                "adding a parent to `{id}` would create an inheritance cycle"
            ));
        }
        state.groups.get_mut(id).expect("checked").parents = unique;
        Ok(())
    }

    fn add_group_parent(&self, id: &str, parent: &str) -> Result<(), String> {
        let mut state = self.state();
        if !state.groups.contains_key(id) {
            return Err(format!("no group `{id}`"));
        }
        if !state.groups.contains_key(parent) {
            return Err(format!("no group `{parent}`"));
        }
        if id == parent {
            return Err(format!(
                "adding parent `{parent}` to `{id}` would create an inheritance cycle"
            ));
        }
        let mut trial = state.groups.clone();
        let parents = &mut trial.get_mut(id).expect("checked").parents;
        if !parents.iter().any(|p| p == parent) {
            parents.push(parent.to_string());
        }
        if has_cycle(&trial) {
            return Err(format!(
                "adding parent `{parent}` to `{id}` would create an inheritance cycle"
            ));
        }
        let group = state.groups.get_mut(id).expect("checked");
        if !group.parents.iter().any(|p| p == parent) {
            group.parents.push(parent.to_string());
            group.parents.sort();
        }
        Ok(())
    }

    fn remove_group_parent(&self, id: &str, parent: &str) -> Result<(), String> {
        let mut state = self.state();
        let group = state
            .groups
            .get_mut(id)
            .ok_or_else(|| format!("no group `{id}`"))?;
        group.parents.retain(|p| p != parent);
        Ok(())
    }

    fn set_user_groups(&self, name: &str, groups: Vec<String>) -> Result<(), String> {
        let key = name.trim().to_ascii_lowercase();
        if key.is_empty() {
            return Err("a player name is empty".to_string());
        }
        let mut state = self.state();
        for group in &groups {
            if !state.groups.contains_key(group) {
                return Err(format!("no group `{group}`"));
            }
        }
        // Preserve the address binding of a group the user is already in.
        let mut bindings: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (id, group) in &state.groups {
            if let Some(addr) = group.members.get(&key) {
                bindings.insert(id.clone(), addr.clone());
            }
        }
        for group in state.groups.values_mut() {
            group.members.remove(&key);
        }
        let mut unique = Vec::new();
        for group in groups {
            if !unique.contains(&group) {
                unique.push(group);
            }
        }
        for group in &unique {
            let addr = bindings.get(group).cloned().flatten();
            state
                .groups
                .get_mut(group)
                .expect("validated")
                .members
                .insert(key.clone(), addr);
        }
        Ok(())
    }

    fn add_user_group(&self, name: &str, group: &str) -> Result<(), String> {
        let key = name.trim().to_ascii_lowercase();
        let mut state = self.state();
        let entry = state
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        // A bare add never clears an existing binding.
        entry.members.entry(key).or_insert(None);
        Ok(())
    }

    fn remove_user_group(&self, name: &str, group: &str) -> Result<(), String> {
        let key = name.trim().to_ascii_lowercase();
        let mut state = self.state();
        let entry = state
            .groups
            .get_mut(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        entry.members.remove(&key);
        Ok(())
    }

    fn create_group(&self, id: &str) -> Result<(), String> {
        if !valid_group_id(id) {
            return Err(format!("`{id}` is not a legal group name"));
        }
        let mut state = self.state();
        if state.groups.contains_key(id) {
            return Err(format!("group `{id}` already exists"));
        }
        state.groups.insert(id.to_string(), MockGroup::default());
        Ok(())
    }

    fn delete_group(&self, id: &str) -> Result<(), String> {
        let mut state = self.state();
        match state.groups.get(id) {
            None => return Err(format!("no group `{id}`")),
            Some(group) if group.builtin => {
                return Err(format!("the built-in group `{id}` cannot be deleted"))
            }
            Some(_) => {}
        }
        state.groups.remove(id);
        for group in state.groups.values_mut() {
            group.parents.retain(|p| p != id);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn upsert(nodes: &mut Vec<NodeEntry>, node: &str, context: Option<&str>, value: Option<bool>) {
    let context = context.map(str::to_string);
    let key = node.to_string();
    match value {
        Some(value) => {
            if let Some(entry) = nodes
                .iter_mut()
                .find(|e| e.node == key && e.context == context)
            {
                entry.value = value;
            } else {
                nodes.push(NodeEntry {
                    node: key,
                    context,
                    value,
                });
            }
        }
        None => nodes.retain(|e| !(e.node == key && e.context == context)),
    }
}

fn valid_node(node: &str) -> bool {
    if node.is_empty() {
        return false;
    }
    let mut segments = node.split('.').peekable();
    while let Some(seg) = segments.next() {
        if seg == "*" {
            return segments.peek().is_none();
        }
        if seg.is_empty()
            || !seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return false;
        }
    }
    true
}

fn valid_group_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn wildcard_matches(pattern: &str, node: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => {
            node.len() > prefix.len()
                && node.starts_with(prefix)
                && node.as_bytes()[prefix.len()] == b'.'
        }
        None => false,
    }
}

fn has_cycle(groups: &BTreeMap<String, MockGroup>) -> bool {
    for start in groups.keys() {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<String> = groups[start].parents.clone();
        while let Some(id) = stack.pop() {
            if &id == start {
                return true;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(group) = groups.get(&id) {
                stack.extend(group.parents.iter().cloned());
            }
        }
    }
    false
}

#[derive(Default)]
struct TestBackend {
    allowed: AtomicBool,
    permissions: Mutex<BTreeSet<(u64, String)>>,
    online: Mutex<Vec<Player>>,
    addresses: Mutex<BTreeMap<u64, String>>,
    journal: Mutex<Vec<(String, String)>>,
    position: Mutex<Option<(String, f64, f64, f64)>>,
}

impl TestBackend {
    fn operator() -> TestBackend {
        let backend = TestBackend::default();
        backend.allowed.store(true, Ordering::SeqCst);
        backend
    }

    fn deny_all(&self) {
        self.allowed.store(false, Ordering::SeqCst);
    }

    fn journal(&self) -> Vec<(String, String)> {
        self.journal.lock().unwrap().clone()
    }

    fn bind(&self, player: u64, addr: &str) {
        self.addresses
            .lock()
            .unwrap()
            .insert(player, addr.to_string());
    }
}

impl HostBackend for TestBackend {
    fn kv_get(&self, _plugin: &str, _key: &str) -> Option<Vec<u8>> {
        None
    }
    fn kv_put(&self, _plugin: &str, _key: &str, _value: &[u8]) {}
    fn kv_delete(&self, _plugin: &str, _key: &str) {}
    fn kv_list(&self, _plugin: &str, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
    fn log(&self, _plugin: &str, _level: aether_plugin::LogLevel, _message: &str) {}
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        true
    }
    fn permission_check(&self, _plugin: &str, id: u64, node: &str, _op: u8) -> bool {
        self.allowed.load(Ordering::SeqCst)
            || self
                .permissions
                .lock()
                .unwrap()
                .contains(&(id, node.to_string()))
    }
    fn online_players(&self, _plugin: &str) -> Vec<Player> {
        self.online.lock().unwrap().clone()
    }
    fn player_address(&self, _plugin: &str, player: u64) -> Option<String> {
        self.addresses.lock().unwrap().get(&player).cloned()
    }
    fn player_position(&self, _plugin: &str, _player: u64) -> Option<(String, f64, f64, f64)> {
        self.position.lock().unwrap().clone()
    }
    fn permissions_journal(&self, _plugin: &str, _actor: u64, action: &str, detail: &str) {
        self.journal
            .lock()
            .unwrap()
            .push((action.to_string(), detail.to_string()));
    }
    fn send_message(&self, _plugin: &str, _player: u64, _text: &str) {}
}

fn player(id: u64, name: &str) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator: false,
    }
}

fn ctx<'a>(
    backend: &'a Arc<dyn HostBackend>,
    services: &'a Arc<ServiceRegistry>,
    config: &'a BTreeMap<String, String>,
) -> NativeContext<'a> {
    NativeContext {
        id: ID,
        capabilities: &[],
        config,
        services,
        backend,
    }
}

struct Fixture {
    plugin: PermissionsPlugin,
    admin: Arc<MockAdmin>,
    backend: Arc<TestBackend>,
    backend_dyn: Arc<dyn HostBackend>,
    services: Arc<ServiceRegistry>,
    config: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Fixture {
        let admin = Arc::new(MockAdmin::new());
        let backend = Arc::new(TestBackend::operator());
        let backend_dyn: Arc<dyn HostBackend> = backend.clone();
        let services = Arc::new(ServiceRegistry::new());
        let config = BTreeMap::new();
        Fixture {
            plugin: PermissionsPlugin::with_admin(admin.clone()),
            admin,
            backend,
            backend_dyn,
            services,
            config,
        }
    }

    fn run(&mut self, args: &[&str]) -> Vec<String> {
        let p = player(1, "Admin");
        let mut ctx = ctx(&self.backend_dyn, &self.services, &self.config);
        self.plugin
            .on_command(
                &mut ctx,
                &p,
                COMMAND,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
            .expect("the plugin handles /lp")
            .lines
    }

    fn complete(&mut self, args: &[&str]) -> Vec<String> {
        let p = player(1, "Admin");
        let mut ctx = ctx(&self.backend_dyn, &self.services, &self.config);
        self.plugin.on_complete(
            &mut ctx,
            &p,
            COMMAND,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }
}

fn assert_contains(lines: &[String], needle: &str) {
    assert!(
        lines.iter().any(|l| l.contains(needle)),
        "expected `{needle}` in {lines:?}"
    );
}

#[test]
fn set_unset_and_check_a_user_node() {
    let mut f = Fixture::new();
    let lines = f.run(&["user", "Alice", "permission", "set", "test.node", "true"]);
    assert_contains(&lines, "Set test.node to true for Alice in context global.");

    let lines = f.run(&["user", "Alice", "permission", "check", "test.node"]);
    assert_eq!(lines[0], "Permission check for test.node:");
    assert_eq!(lines[1], "    Result: true");
    assert_eq!(lines[2], "    Processor: user alice");
    assert_eq!(f.backend.journal().len(), 1);

    let lines = f.run(&["user", "Alice", "permission", "unset", "test.node"]);
    assert_contains(&lines, "Unset test.node for Alice in context global.");
    let lines = f.run(&["user", "Alice", "permission", "check", "test.node"]);
    assert_eq!(lines[0], "Permission check for test.node:");
    assert_eq!(lines[1], "    Result: false");
    assert_eq!(lines[2], "    Processor: default");
}

#[test]
fn group_inheritance_and_wildcards_decide_a_child_node() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "staff"]);
    f.run(&["group", "staff", "parent", "set", "op"]);
    f.run(&["user", "Alice", "parent", "add", "staff"]);
    let lines = f.run(&["group", "staff", "permission", "set", "test.*", "true"]);
    assert_contains(&lines, "Set test.* to true for staff in context global.");

    let lines = f.run(&["user", "Alice", "permission", "check", "test.child"]);
    assert_eq!(lines[0], "Permission check for test.child:");
    assert_eq!(lines[1], "    Result: true");
    assert_eq!(lines[2], "    Processor: group staff");

    let lines = f.run(&["group", "staff", "permission", "check", "test.deep.node"]);
    assert_eq!(lines[0], "Permission check for test.deep.node:");
    assert_eq!(lines[1], "    Result: true");
    assert_eq!(lines[2], "    Processor: group staff");

    // Set changes back to the exact node, leaving the wildcard list intact.
    f.run(&["group", "staff", "permission", "unset", "test.*"]);
    let lines = f.run(&["user", "Alice", "permission", "check", "test.child"]);
    assert_eq!(lines[1], "    Result: false");
}

#[test]
fn parent_set_add_remove_clear_and_cycles() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "parent"]);
    f.run(&["creategroup", "child"]);
    let lines = f.run(&["group", "child", "parent", "add", "parent"]);
    assert_contains(
        &lines,
        "child now inherits permissions from parent in context global.",
    );
    let lines = f.run(&["group", "child", "parent", "add", "parent"]);
    assert_contains(
        &lines,
        "child already inherits from parent in context global.",
    );

    // The reverse edge would close a cycle and is refused.
    let lines = f.run(&["group", "parent", "parent", "add", "child"]);
    assert_contains(&lines, "Error:");
    assert_contains(&lines, "cycle");
    assert!(f.admin.group("parent").unwrap().parents.is_empty());

    let lines = f.run(&["group", "child", "parent", "remove", "parent"]);
    assert_contains(
        &lines,
        "child no longer inherits permissions from parent in context global.",
    );
    let lines = f.run(&["group", "child", "parent", "remove", "parent"]);
    assert_contains(
        &lines,
        "child does not inherit from parent in context global.",
    );

    f.run(&["group", "child", "parent", "set", "parent"]);
    assert_eq!(
        f.admin.group("child").unwrap().parents,
        vec!["parent".to_string()]
    );
    let lines = f.run(&["group", "child", "parent", "clear"]);
    assert_contains(&lines, "child's parents were cleared in context global.");
    assert!(f.admin.group("child").unwrap().parents.is_empty());
}

#[test]
fn user_parent_set_add_remove_clear_and_duplicates() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "one"]);
    f.run(&["creategroup", "two"]);
    let lines = f.run(&["user", "Alice", "parent", "add", "one"]);
    assert_contains(
        &lines,
        "Alice now inherits permissions from one in context global.",
    );
    let lines = f.run(&["user", "Alice", "parent", "add", "one"]);
    assert_contains(&lines, "Alice already inherits from one in context global.");

    f.run(&["user", "Alice", "parent", "add", "two"]);
    let lines = f.run(&["user", "Alice", "parent", "set", "two"]);
    assert_contains(&lines, "Alice had their existing parent groups cleared, and now only inherits two in context global.");
    assert_eq!(
        f.admin.user("alice", None).unwrap().groups,
        vec!["two".to_string()]
    );

    let lines = f.run(&["user", "Alice", "parent", "remove", "two"]);
    assert_contains(
        &lines,
        "Alice no longer inherits permissions from two in context global.",
    );
    let lines = f.run(&["user", "Alice", "parent", "remove", "two"]);
    assert_contains(&lines, "Alice does not inherit from two in context global.");
    assert!(f.admin.user("alice", None).unwrap().groups.is_empty());

    f.run(&["user", "Alice", "parent", "add", "one"]);
    let lines = f.run(&["user", "Alice", "parent", "clear"]);
    assert_contains(&lines, "Alice's parents were cleared in context global.");
    assert_contains(&lines, "groups were removed");
    assert!(f.admin.user("alice", None).unwrap().groups.is_empty());
}

#[test]
fn switch_replaces_the_user_parent_set() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "one"]);
    f.run(&["creategroup", "two"]);
    f.run(&["user", "Alice", "parent", "add", "one"]);
    let lines = f.run(&["user", "Alice", "parent", "switch", "two"]);
    assert_contains(
        &lines,
        "Alice had their existing parent groups cleared, and now only inherits two in context global.",
    );
    assert_eq!(
        f.admin.user("alice", None).unwrap().groups,
        vec!["two".to_string()]
    );

    let lines = f.run(&["group", "two", "parent", "switch", "op"]);
    assert_contains(
        &lines,
        "two had their existing parent groups cleared, and now only inherits op in context global.",
    );
    assert_eq!(
        f.admin.group("two").unwrap().parents,
        vec![OP_GROUP.to_string()]
    );
}

#[test]
fn a_non_operator_is_refused_on_every_command_path() {
    let mut f = Fixture::new();
    f.backend.deny_all();
    for args in [
        vec!["info"],
        vec!["user", "Alice", "info"],
        vec!["user", "Alice", "permission", "set", "test.node", "true"],
        vec!["group", "op", "info"],
        vec!["creategroup", "new"],
        vec!["deletegroup", "op"],
        vec!["user", "Alice", "parent", "add", "op"],
    ] {
        let lines = f.run(&args);
        assert_eq!(
            lines,
            vec!["You do not have permission to use this command!"]
        );
    }
    assert_eq!(f.admin.saves.load(Ordering::SeqCst), 0);
}

#[test]
fn the_builtin_op_group_cannot_be_deleted() {
    let mut f = Fixture::new();
    let lines = f.run(&["deletegroup", "op"]);
    assert_contains(&lines, "Error:");
    assert_contains(&lines, "cannot be deleted");
    assert!(f.admin.group("op").is_some());
}

#[test]
fn user_and_group_info_show_membership_and_nodes() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "moderator"]);
    f.run(&["user", "Alice", "parent", "add", "moderator"]);
    f.run(&[
        "group",
        "moderator",
        "permission",
        "set",
        "test.node",
        "true",
    ]);
    let lines = f.run(&["user", "Alice", "info"]);
    assert_contains(&lines, "User: alice");
    assert_contains(&lines, "Parent groups: moderator");
    assert_contains(&lines, "Memberships: moderator");
    assert_contains(&lines, "Inherited groups: moderator");
    assert_contains(&lines, "Nodes: 0");

    let lines = f.run(&["group", "moderator", "info"]);
    assert_contains(&lines, "Group: moderator");
    assert_contains(&lines, "Members: alice");
    assert_contains(&lines, "Nodes: 1");
    let lines = f.run(&["group", "moderator", "permission", "list"]);
    assert_contains(&lines, "test.node = true (from group moderator)");
}

#[test]
fn writes_are_persisted_immediately_and_survive_a_restart() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "saved"]);
    f.run(&["group", "saved", "permission", "set", "test.node", "true"]);

    // A new plugin over the same store sees the write: persistence is the
    // core's, not plugin-local state.
    let mut restarted = PermissionsPlugin::with_admin(f.admin.clone());
    let p = player(1, "Admin");
    let backend: Arc<dyn HostBackend> = f.backend_dyn.clone();
    let services = f.services.clone();
    let config = f.config.clone();
    let mut ctx = ctx(&backend, &services, &config);
    let lines = restarted
        .on_command(
            &mut ctx,
            &p,
            COMMAND,
            &[
                "user".into(),
                "Alice".into(),
                "permission".into(),
                "check".into(),
                "test.node".into(),
            ],
        )
        .unwrap()
        .lines;
    assert_eq!(lines[1], "    Result: false");
    let lines = restarted
        .on_command(
            &mut ctx,
            &p,
            COMMAND,
            &[
                "group".into(),
                "saved".into(),
                "permission".into(),
                "check".into(),
                "test.node".into(),
            ],
        )
        .unwrap()
        .lines;
    assert_eq!(lines[1], "    Result: true");
}

#[test]
fn completion_offers_users_groups_nodes_values_and_worlds() {
    let mut f = Fixture::new();
    f.run(&["creategroup", "staff"]);
    f.run(&["user", "Alice", "permission", "set", "test.node", "true"]);
    f.backend.online.lock().unwrap().push(player(2, "Bob"));
    *f.backend.position.lock().unwrap() = Some(("overworld".into(), 0.0, 0.0, 0.0));

    let top = f.complete(&[""]);
    for word in [
        "user",
        "group",
        "creategroup",
        "deletegroup",
        "info",
        "help",
    ] {
        assert!(
            top.contains(&word.to_string()),
            "top completion missing {word}"
        );
    }
    let users = f.complete(&["user", ""]);
    assert!(users.contains(&"alice".to_string()));
    assert!(users.contains(&"Bob".to_string()));

    let groups = f.complete(&["group", ""]);
    assert!(groups.contains(&"staff".to_string()));
    assert!(groups.contains(&"op".to_string()));

    let nodes = f.complete(&["group", "staff", "permission", "set", ""]);
    assert!(nodes.contains(&"test.node".to_string()));

    let values = f.complete(&["group", "staff", "permission", "set", "test.node", ""]);
    assert!(values.contains(&"true".to_string()));
    assert!(values.contains(&"false".to_string()));
    assert!(values.contains(&"world=overworld".to_string()));

    let parents = f.complete(&["user", "alice", "parent", "set", ""]);
    assert!(parents.contains(&"staff".to_string()));
    let switched = f.complete(&["user", "alice", "parent", "switch", "sta"]);
    assert!(switched.contains(&"staff".to_string()));
}

#[test]
fn context_is_parsed_and_reported_by_check() {
    let mut f = Fixture::new();
    let lines = f.run(&[
        "user",
        "Alice",
        "permission",
        "set",
        "test.node",
        "true",
        "world=overworld",
    ]);
    assert_contains(
        &lines,
        "Set test.node to true for Alice in context overworld.",
    );
    let lines = f.run(&[
        "user",
        "Alice",
        "permission",
        "check",
        "test.node",
        "world=overworld",
    ]);
    assert_eq!(lines[1], "    Result: true");
    assert_eq!(lines[3], "    Context: world=overworld");
    let lines = f.run(&["user", "Alice", "permission", "check", "test.node"]);
    assert_eq!(lines[1], "    Result: false");
    assert_eq!(lines[3], "    Context: global");

    let lines = f.run(&["user", "Alice", "permission", "set", "test.node", "yes"]);
    assert_contains(&lines, "Usage:");
    let lines = f.run(&[
        "user",
        "Alice",
        "permission",
        "set",
        "test.node",
        "true",
        "server=x",
    ]);
    assert_contains(&lines, "unknown argument");
}

#[test]
fn an_illegal_node_is_refused_by_the_store() {
    let mut f = Fixture::new();
    let lines = f.run(&["user", "Alice", "permission", "set", "Bad.Node", "true"]);
    assert_contains(&lines, "Error:");
    assert_contains(&lines, "not a legal permission node");
    let lines = f.run(&["group", "op", "permission", "set", "a..b", "false"]);
    assert_contains(&lines, "Error:");
}

#[test]
fn init_refuses_to_load_without_the_admin_service() {
    let backend: Arc<dyn HostBackend> = Arc::new(TestBackend::operator());
    let services = Arc::new(ServiceRegistry::new());
    let config = BTreeMap::new();
    let mut plugin = PermissionsPlugin::default();
    let mut ctx = ctx(&backend, &services, &config);
    let err = plugin.init(&mut ctx).unwrap_err();
    assert!(err.contains("permissions.admin@0.1"), "{err}");
}

#[test]
fn a_bound_member_is_visible_and_checked_from_the_online_address() {
    let mut f = Fixture::new();
    {
        let mut state = f.admin.state();
        let op = state.groups.get_mut(OP_GROUP).unwrap();
        op.members
            .insert("root".to_string(), Some("10.0.0.7".to_string()));
        op.nodes.push(NodeEntry {
            node: "bound.node".to_string(),
            context: None,
            value: true,
        });
    }
    f.backend.online.lock().unwrap().push(player(7, "Root"));
    f.backend.bind(7, "10.0.0.7");

    let lines = f.run(&["user", "Root", "info"]);
    assert_contains(&lines, "Level: 4");
    assert_contains(&lines, "Parent groups: op");
    assert_contains(&lines, "Memberships: op@10.0.0.7");

    let lines = f.run(&["user", "Root", "permission", "check", "bound.node"]);
    assert_eq!(lines[1], "    Result: true");
    assert_eq!(lines[2], "    Processor: group op");

    // From another address the binding does not apply; the info still shows
    // the binding so the operator can see why.
    f.backend.online.lock().unwrap().clear();
    let lines = f.run(&["user", "Root", "info"]);
    assert_contains(&lines, "Level: 0");
    assert_contains(&lines, "Memberships: op@10.0.0.7");

    // Back online, remove is reported as a real removal (not "does not
    // inherit", which was the IP-blind bug).
    f.backend.online.lock().unwrap().push(player(7, "Root"));
    let lines = f.run(&["user", "Root", "parent", "remove", "op"]);
    assert_contains(
        &lines,
        "Root no longer inherits permissions from op in context global.",
    );
    assert!(f.admin.user("root", None).unwrap().memberships.is_empty());
}

#[test]
fn a_context_write_and_unset_carry_the_context_into_the_audit_detail() {
    let mut f = Fixture::new();
    f.run(&[
        "user",
        "Alice",
        "permission",
        "set",
        "test.node",
        "true",
        "world=overworld",
    ]);
    let journal = f.backend.journal();
    assert!(
        journal
            .last()
            .is_some_and(|(_, detail)| detail.contains("world=overworld")),
        "{journal:?}"
    );

    f.run(&[
        "user",
        "Alice",
        "permission",
        "unset",
        "test.node",
        "world=overworld",
    ]);
    let journal = f.backend.journal();
    assert!(
        journal
            .last()
            .is_some_and(|(_, detail)| detail.contains("world=overworld")),
        "{journal:?}"
    );
}
