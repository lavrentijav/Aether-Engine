//! `/tag`, `/team` and `/teammsg` — vanilla 1.21.11 (CMD-16).
//!
//! Tags live on the entity/player record (`PlayerState::tags`, `Entity::tags`)
//! and are saved with the player. Teams are the model in [`crate::team`],
//! persisted one JSON blob per team in the world store. The command grammar,
//! literals and messages are vanilla's; selectors go through CMD-06b, so
//! `@e[tag=x]`, `@a[team=red]`, relative coordinates and the rest work here as
//! they do in the other commands.

use std::collections::BTreeSet;

use aether_api::Vector3;

use super::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use super::tree::{self, Builder, Ctx, Node, Requirement, Value};
use super::Reply;
use crate::commands::coordinates::Rotation;
use crate::game::entities;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::nbt::Nbt;
use crate::protocol::scoreboard::TeamChange;
use crate::session::DemoWorld;
use crate::team::{self, CollisionRule, Color, Team, Visibility};

/// Vanilla 1.21.11 `en_us` strings this module emits, verbatim.
mod lang {
    pub const TAG_ADD_FAILED: &str = "Target either already has the tag or has too many tags";
    pub const TAG_REMOVE_FAILED: &str = "Target does not have this tag";
    pub const TAG_ADD_SINGLE: &str = "Added tag '%s' to %s";
    pub const TAG_ADD_MULTIPLE: &str = "Added tag '%s' to %s entities";
    pub const TAG_REMOVE_SINGLE: &str = "Removed tag '%s' from %s";
    pub const TAG_REMOVE_MULTIPLE: &str = "Removed tag '%s' from %s entities";
    pub const TAG_LIST_SINGLE: &str = "%s has %s tags: %s";
    pub const TAG_LIST_SINGLE_EMPTY: &str = "%s has no tags";
    pub const TAG_LIST_MULTIPLE: &str = "The %s entities have %s total tags: %s";
    pub const TAG_LIST_MULTIPLE_EMPTY: &str = "There are no tags on the %s entities";

    pub const TEAM_ADD_DUPLICATE: &str = "A team already exists by that name";
    pub const TEAM_ADD: &str = "Created team %s";
    pub const TEAM_REMOVE: &str = "Removed team %s";
    pub const TEAM_EMPTY: &str = "Removed %s member(s) from team %s";
    pub const TEAM_EMPTY_UNCHANGED: &str = "Nothing changed. That team is already empty";
    pub const TEAM_JOIN_SINGLE: &str = "Added %s to team %s";
    pub const TEAM_JOIN_MULTIPLE: &str = "Added %s members to team %s";
    pub const TEAM_LEAVE_SINGLE: &str = "Removed %s from any team";
    pub const TEAM_LEAVE_MULTIPLE: &str = "Removed %s members from any team";
    pub const TEAM_LIST_EMPTY: &str = "There are no teams";
    pub const TEAM_LIST: &str = "There are %s team(s): %s";
    pub const TEAM_MEMBERS_EMPTY: &str = "There are no members on team %s";
    pub const TEAM_MEMBERS: &str = "Team %s has %s member(s): %s";
    pub const TEAM_NOT_FOUND: &str = "Unknown team '%s'";

    pub const NAME_UNCHANGED: &str = "Nothing changed. That team already has that name";
    pub const NAME_SUCCESS: &str = "Updated the name of team %s";
    pub const COLOR_UNCHANGED: &str = "Nothing changed. That team already has that color";
    pub const COLOR_SUCCESS: &str = "Updated the color for team %s to %s";
    pub const FRIENDLY_ENABLED: &str = "Enabled friendly fire for team %s";
    pub const FRIENDLY_DISABLED: &str = "Disabled friendly fire for team %s";
    pub const FRIENDLY_ALREADY_ON: &str =
        "Nothing changed. Friendly fire is already enabled for that team";
    pub const FRIENDLY_ALREADY_OFF: &str =
        "Nothing changed. Friendly fire is already disabled for that team";
    pub const SIGHT_ENABLED: &str = "Team %s can now see invisible teammates";
    pub const SIGHT_DISABLED: &str = "Team %s can no longer see invisible teammates";
    pub const SIGHT_ALREADY_ON: &str =
        "Nothing changed. That team can already see invisible teammates";
    pub const SIGHT_ALREADY_OFF: &str =
        "Nothing changed. That team already can't see invisible teammates";
    pub const NAMETAG_UNCHANGED: &str = "Nothing changed. Nametag visibility is already that value";
    pub const NAMETAG_SUCCESS: &str = "Nametag visibility for team %s is now \"%s\"";
    pub const DEATH_UNCHANGED: &str =
        "Nothing changed. Death message visibility is already that value";
    pub const DEATH_SUCCESS: &str = "Death message visibility for team %s is now \"%s\"";
    pub const COLLISION_UNCHANGED: &str = "Nothing changed. Collision rule is already that value";
    pub const COLLISION_SUCCESS: &str = "Collision rule for team %s is now \"%s\"";
    pub const PREFIX_SUCCESS: &str = "Team prefix set to %s";
    pub const SUFFIX_SUCCESS: &str = "Team suffix set to %s";
    pub const TEAMMSG_NO_TEAM: &str = "You must be on a team to message your team";
}

/// Fill `%s` placeholders from left to right.
fn format(template: &str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut rest = template;
    for arg in args {
        match rest.split_once("%s") {
            Some((head, tail)) => {
                out.push_str(head);
                out.push_str(arg);
                rest = tail;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Every CMD-16 command, for the command tree.
pub fn nodes() -> Vec<Node> {
    vec![tag_node(), team_node(), teammsg_node()]
}

fn tag_node() -> Node {
    let add = Builder::literal("add")
        .then(Builder::arg("name", tree::ArgKind::String).executable(exec_tag_add));
    let remove = Builder::literal("remove")
        .then(Builder::arg("name", tree::ArgKind::String).executable(exec_tag_remove));
    let list = Builder::literal("list").executable(exec_tag_list);
    tree::with_usage(
        Builder::literal("tag")
            .requires(Requirement::Level {
                op: 2,
                node: "minecraft.command.tag",
            })
            .executable(exec_tag_usage)
            .then(
                Builder::arg("targets", tree::ArgKind::Entities)
                    .then(add)
                    .then(remove)
                    .then(list),
            )
            .build(),
        "<targets> <add|remove> <name> | <targets> list",
    )
}

fn team_node() -> Node {
    let req = Requirement::Level {
        op: 2,
        node: "minecraft.command.team",
    };

    let list = Builder::literal("list")
        .executable(exec_team_list)
        .then(Builder::arg("team", tree::ArgKind::String).executable(exec_team_list));

    let add = Builder::literal("add").then(
        Builder::arg("team", tree::ArgKind::String)
            .executable(exec_team_add)
            .then(Builder::arg("displayName", tree::ArgKind::Component).executable(exec_team_add)),
    );

    let remove = Builder::literal("remove")
        .then(Builder::arg("team", tree::ArgKind::String).executable(exec_team_remove));

    let empty = Builder::literal("empty")
        .then(Builder::arg("team", tree::ArgKind::String).executable(exec_team_empty));

    let join = Builder::literal("join").then(
        Builder::arg("team", tree::ArgKind::String)
            .executable(exec_team_join_self)
            .then(Builder::arg("members", tree::ArgKind::Entities).executable(exec_team_join)),
    );

    let leave = Builder::literal("leave")
        .then(Builder::arg("members", tree::ArgKind::Entities).executable(exec_team_leave));

    let visibility = |name: &'static str, set: fn(&mut Ctx, Visibility) -> Reply| {
        let mut b = Builder::literal(name);
        for (word, value) in [
            ("always", Visibility::Always),
            ("never", Visibility::Never),
            ("hideForOtherTeams", Visibility::HideForOtherTeams),
            ("hideForOwnTeam", Visibility::HideForOwnTeam),
        ] {
            b = b.then(Builder::literal(word).executable(move |ctx| set(ctx, value)));
        }
        b
    };

    let collision = {
        let mut b = Builder::literal("collisionRule");
        for (word, value) in [
            ("always", CollisionRule::Always),
            ("never", CollisionRule::Never),
            ("pushOtherTeams", CollisionRule::PushOtherTeams),
            ("pushOwnTeam", CollisionRule::PushOwnTeam),
        ] {
            b = b.then(Builder::literal(word).executable(move |ctx| exec_collision(ctx, value)));
        }
        b
    };

    let modify = Builder::literal("modify").then(
        Builder::arg("team", tree::ArgKind::String)
            .then(Builder::literal("displayName").then(
                Builder::arg("displayName", tree::ArgKind::Component).executable(exec_display_name),
            ))
            .then(
                Builder::literal("color")
                    .then(Builder::arg("value", tree::ArgKind::String).executable(exec_color)),
            )
            .then(
                Builder::literal("friendlyFire").then(
                    Builder::arg("allowed", tree::ArgKind::Bool).executable(exec_friendly_fire),
                ),
            )
            .then(
                Builder::literal("seeFriendlyInvisibles").then(
                    Builder::arg("allowed", tree::ArgKind::Bool)
                        .executable(exec_see_friendly_invisibles),
                ),
            )
            .then(visibility("nametagVisibility", exec_nametag_visibility))
            .then(visibility(
                "deathMessageVisibility",
                exec_death_message_visibility,
            ))
            .then(collision)
            .then(
                Builder::literal("prefix")
                    .then(Builder::arg("prefix", tree::ArgKind::Component).executable(exec_prefix)),
            )
            .then(
                Builder::literal("suffix")
                    .then(Builder::arg("suffix", tree::ArgKind::Component).executable(exec_suffix)),
            ),
    );

    tree::with_usage(
        Builder::literal("team")
            .requires(req)
            .executable(exec_team_usage)
            .then(list)
            .then(add)
            .then(remove)
            .then(empty)
            .then(join)
            .then(leave)
            .then(modify)
            .build(),
        "<list|add|remove|empty|join|leave|modify> [...]",
    )
}

fn teammsg_node() -> Node {
    Builder::literal("teammsg")
        .alias("tm")
        .executable(exec_teammsg_usage)
        .then(Builder::arg("message", tree::ArgKind::Message).executable(exec_teammsg))
        .build()
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn text_at<'a>(ctx: &'a Ctx<'_, '_>, i: usize) -> Option<&'a str> {
    match &ctx.args.get(i)?.value {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

fn source_at<'a>(ctx: &'a Ctx<'_, '_>, i: usize) -> Option<&'a str> {
    ctx.args.get(i).map(|a| a.text.as_str())
}

fn bool_at(ctx: &Ctx, i: usize) -> Option<bool> {
    match ctx.args.get(i)?.value {
        Value::Bool(b) => Some(b),
        _ => None,
    }
}

fn usage(ctx: &mut Ctx, text: &str) -> Reply {
    ctx.fail(text)
}

// ---------------------------------------------------------------------------
// /tag
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Target {
    Player(String),
    Entity(i32),
}

impl Target {
    fn display(&self, registry: &SharedRegistry) -> String {
        match self {
            Target::Player(name) => name.clone(),
            Target::Entity(id) => registry
                .by_entity(*id)
                .map(|p| p.name.clone())
                .or_else(|| entities::kind_of(*id).map(pretty))
                .unwrap_or_else(|| format!("entity {id}")),
        }
    }

    fn tags(&self, registry: &SharedRegistry) -> Vec<String> {
        match self {
            Target::Player(name) => registry
                .snapshot()
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.game().tags.iter().cloned().collect())
                .unwrap_or_default(),
            Target::Entity(id) => entities::tags(*id).unwrap_or_default(),
        }
    }

    fn add(&self, tag: &str, registry: &SharedRegistry, world: &DemoWorld) -> bool {
        match self {
            Target::Player(name) => {
                let Some(handle) = registry
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                else {
                    return false;
                };
                let inserted = handle.game().tags.insert(tag.to_string());
                if inserted {
                    crate::session::save_tags(&handle, world);
                }
                inserted
            }
            Target::Entity(id) => entities::add_tag(*id, tag),
        }
    }

    fn remove(&self, tag: &str, registry: &SharedRegistry, world: &DemoWorld) -> bool {
        match self {
            Target::Player(name) => {
                let Some(handle) = registry
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                else {
                    return false;
                };
                let removed = handle.game().tags.remove(tag);
                if removed {
                    crate::session::save_tags(&handle, world);
                }
                removed
            }
            Target::Entity(id) => entities::remove_tag(*id, tag),
        }
    }
}

/// `minecraft:cave_spider` → `Cave Spider`.
fn pretty(kind: &str) -> String {
    kind.strip_prefix("minecraft:")
        .unwrap_or(kind)
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn hyphenated_uuid(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

struct WorldSource<'a> {
    registry: &'a SharedRegistry,
    world: &'a DemoWorld,
    executor: Option<EntityRef>,
}

fn player_ref(handle: &PlayerHandle, world: &DemoWorld) -> EntityRef {
    let pos = handle.pos();
    // One guard for every field read from the player state: two `handle.game()`
    // temporaries in one struct literal would live to the end of the
    // expression and self-deadlock on the non-reentrant mutex.
    let (tags, mode, level) = {
        let st = handle.game();
        (st.tags.iter().cloned().collect(), st.mode, st.xp_total)
    };
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: Rotation::new(pos.yaw, pos.pitch),
        tags,
        team: team::team_name_of(world, &handle.name),
        gamemode: Some(Gamemode::from(mode)),
        level,
        scores: Vec::new(),
        advancements: Vec::new(),
        predicates: Vec::new(),
        nbt: Nbt::Compound(Vec::new()),
    }
}

impl SelectorSource for WorldSource<'_> {
    fn executor(&self) -> Option<EntityRef> {
        self.executor.clone()
    }

    fn players(&self) -> Vec<EntityRef> {
        self.registry
            .snapshot()
            .iter()
            .map(|p| player_ref(p, self.world))
            .collect()
    }

    fn entities(&self) -> Vec<EntityRef> {
        entities::selectable()
            .into_iter()
            .map(|e| EntityRef {
                id: e.id,
                uuid: e.uuid,
                kind: e.kind.to_string(),
                is_player: false,
                alive: e.alive,
                name: None,
                pos: e.pos,
                rotation: Rotation::new(e.yaw, e.pitch),
                tags: e.tags,
                team: None,
                gamemode: None,
                level: 0,
                scores: Vec::new(),
                advancements: Vec::new(),
                predicates: Vec::new(),
                nbt: Nbt::Compound(Vec::new()),
            })
            .collect()
    }
}

fn world_source<'a>(ctx: &'a Ctx) -> WorldSource<'a> {
    WorldSource {
        registry: ctx.registry,
        world: ctx.world,
        executor: ctx.player().map(|p| player_ref(p, ctx.world)),
    }
}

fn selector_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

fn resolve_targets(ctx: &Ctx, text: &str) -> Result<Vec<Target>, String> {
    let text = text.trim();
    if text.starts_with('@') {
        let selector = Selector::parse(text).map_err(|e| e.message())?;
        let source = world_source(ctx);
        let found = selector
            .select(&source, &mut LcgRng::new(selector_seed()))
            .map_err(|e| e.message())?;
        let mut out = Vec::with_capacity(found.len());
        for e in found {
            if e.is_player {
                out.push(Target::Player(e.name.unwrap_or_default()));
            } else {
                out.push(Target::Entity(e.id));
            }
        }
        return Ok(out);
    }
    // A plain name (or UUID) is the single-entity form of the argument.
    if let Some(handle) = ctx
        .registry
        .snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(text))
    {
        return Ok(vec![Target::Player(handle.name.clone())]);
    }
    if let Ok(uuid) = text.parse::<u128>() {
        if let Some(handle) = ctx.registry.by_uuid(uuid) {
            return Ok(vec![Target::Player(handle.name.clone())]);
        }
    }
    if let Some(e) = entities::selectable()
        .into_iter()
        .find(|e| e.uuid.to_string() == text || hyphenated_uuid(e.uuid) == text)
    {
        return Ok(vec![Target::Entity(e.id)]);
    }
    Err("No entity was found".into())
}

fn exec_tag_usage(ctx: &mut Ctx) -> Reply {
    usage(
        ctx,
        "/tag <targets> <add|remove> <name> | /tag <targets> list",
    )
}

fn exec_tag_add(ctx: &mut Ctx) -> Reply {
    let targets = match resolve_targets(ctx, source_at(ctx, 0).unwrap_or("")) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let Some(tag) = text_at(ctx, 1).map(str::to_string) else {
        return usage(ctx, "/tag <targets> add <name>");
    };
    let changed = targets
        .iter()
        .filter(|t| t.add(&tag, ctx.registry, ctx.world))
        .count();
    if changed == 0 {
        return ctx.fail(lang::TAG_ADD_FAILED);
    }
    if targets.len() == 1 {
        Reply::one(format(
            lang::TAG_ADD_SINGLE,
            &[&tag, &targets[0].display(ctx.registry)],
        ))
    } else {
        Reply::one(format(
            lang::TAG_ADD_MULTIPLE,
            &[&tag, &targets.len().to_string()],
        ))
    }
}

fn exec_tag_remove(ctx: &mut Ctx) -> Reply {
    let targets = match resolve_targets(ctx, source_at(ctx, 0).unwrap_or("")) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let Some(tag) = text_at(ctx, 1).map(str::to_string) else {
        return usage(ctx, "/tag <targets> remove <name>");
    };
    let changed = targets
        .iter()
        .filter(|t| t.remove(&tag, ctx.registry, ctx.world))
        .count();
    if changed == 0 {
        return ctx.fail(lang::TAG_REMOVE_FAILED);
    }
    if targets.len() == 1 {
        Reply::one(format(
            lang::TAG_REMOVE_SINGLE,
            &[&tag, &targets[0].display(ctx.registry)],
        ))
    } else {
        Reply::one(format(
            lang::TAG_REMOVE_MULTIPLE,
            &[&tag, &targets.len().to_string()],
        ))
    }
}

fn exec_tag_list(ctx: &mut Ctx) -> Reply {
    let targets = match resolve_targets(ctx, source_at(ctx, 0).unwrap_or("")) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let mut tags = BTreeSet::new();
    for target in &targets {
        tags.extend(target.tags(ctx.registry));
    }
    let joined = tags.iter().cloned().collect::<Vec<_>>().join(", ");
    if targets.len() == 1 {
        if tags.is_empty() {
            Reply::one(format(
                lang::TAG_LIST_SINGLE_EMPTY,
                &[&targets[0].display(ctx.registry)],
            ))
        } else {
            Reply::one(format(
                lang::TAG_LIST_SINGLE,
                &[
                    &targets[0].display(ctx.registry),
                    &tags.len().to_string(),
                    &joined,
                ],
            ))
        }
    } else if tags.is_empty() {
        Reply::one(format(
            lang::TAG_LIST_MULTIPLE_EMPTY,
            &[&targets.len().to_string()],
        ))
    } else {
        Reply::one(format(
            lang::TAG_LIST_MULTIPLE,
            &[&targets.len().to_string(), &tags.len().to_string(), &joined],
        ))
    }
}

// ---------------------------------------------------------------------------
// /team helpers
// ---------------------------------------------------------------------------

fn not_found(ctx: &mut Ctx, name: &str) -> Reply {
    ctx.fail(format(lang::TEAM_NOT_FOUND, &[name]))
}

fn commit(ctx: &Ctx, team: &Team) {
    team::save(ctx.world, team);
    team::broadcast(ctx.registry, ctx.world, team::update_change(team));
}

fn exec_team_usage(ctx: &mut Ctx) -> Reply {
    usage(ctx, "/team <list|add|remove|empty|join|leave|modify> [...]")
}

fn exec_team_add(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team add <name> [<displayName>]");
    };
    if team::find(ctx.world, &name).is_some() {
        return ctx.fail(lang::TEAM_ADD_DUPLICATE);
    }
    let display = match source_at(ctx, 1) {
        Some(raw) => match component_text(raw) {
            Ok(text) => text,
            Err(e) => return ctx.fail(e),
        },
        None => name.clone(),
    };
    let mut team = Team::new(name);
    team.display_name = display;
    team::save(ctx.world, &team);
    team::broadcast(ctx.registry, ctx.world, team::add_change(&team));
    Reply::one(format(lang::TEAM_ADD, &[&team.formatted_display_name()]))
}

fn exec_team_remove(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team remove <name>");
    };
    let Some(team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    team::remove(ctx.world, &name);
    team::broadcast(
        ctx.registry,
        ctx.world,
        TeamChange::Remove { name: name.clone() },
    );
    Reply::one(format(lang::TEAM_REMOVE, &[&team.formatted_display_name()]))
}

fn exec_team_empty(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team empty <name>");
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.members.is_empty() {
        return ctx.fail(lang::TEAM_EMPTY_UNCHANGED);
    }
    let members: Vec<String> = team.members.iter().cloned().collect();
    team.members.clear();
    team::save(ctx.world, &team);
    team::broadcast(
        ctx.registry,
        ctx.world,
        TeamChange::Leave {
            name: team.name.clone(),
            members: members.clone(),
        },
    );
    Reply::one(format(
        lang::TEAM_EMPTY,
        &[&members.len().to_string(), &team.formatted_display_name()],
    ))
}

fn exec_team_join_self(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team join <name> [<members>]");
    };
    let Some(handle) = ctx.player() else {
        return ctx.needs_player();
    };
    join_members(ctx, &name, vec![handle.name.clone()])
}

fn exec_team_join(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team join <name> [<members>]");
    };
    let members = match resolve_score_holders(ctx, source_at(ctx, 1).unwrap_or("")) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    join_members(ctx, &name, members)
}

fn join_members(ctx: &mut Ctx, name: &str, names: Vec<String>) -> Reply {
    let Some(mut target) = team::find(ctx.world, name) else {
        return not_found(ctx, name);
    };
    let mut teams = team::all(ctx.world);
    let mut old_changes: Vec<(String, Vec<String>)> = Vec::new();
    for member in &names {
        for team in &mut teams {
            if team.name == target.name {
                continue;
            }
            if team.members.remove(member) {
                old_changes.push((team.name.clone(), vec![member.clone()]));
            }
        }
    }
    let mut joined = Vec::new();
    for member in &names {
        target.members.insert(member.clone());
        joined.push(member.clone());
    }
    // Save and announce the teams members were moved out of.
    for (old_name, members) in old_changes {
        if let Some(old) = teams.iter().find(|t| t.name == old_name) {
            team::save(ctx.world, old);
        }
        team::broadcast(
            ctx.registry,
            ctx.world,
            TeamChange::Leave {
                name: old_name,
                members,
            },
        );
    }
    team::save(ctx.world, &target);
    team::broadcast(
        ctx.registry,
        ctx.world,
        TeamChange::Join {
            name: target.name.clone(),
            members: joined.clone(),
        },
    );
    if joined.len() == 1 {
        let display = ctx
            .registry
            .snapshot()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(&joined[0]))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| joined[0].clone());
        Reply::one(format(
            lang::TEAM_JOIN_SINGLE,
            &[&display, &target.formatted_display_name()],
        ))
    } else {
        Reply::one(format(
            lang::TEAM_JOIN_MULTIPLE,
            &[&joined.len().to_string(), &target.formatted_display_name()],
        ))
    }
}

fn exec_team_leave(ctx: &mut Ctx) -> Reply {
    let Some(text) = source_at(ctx, 0) else {
        return usage(ctx, "/team leave <members>");
    };
    let names = match resolve_score_holders(ctx, text) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let mut teams = team::all(ctx.world);
    let mut changes: Vec<(String, Vec<String>)> = Vec::new();
    for name in &names {
        for team in &mut teams {
            if team.members.remove(name) {
                changes.push((team.name.clone(), vec![name.clone()]));
            }
        }
    }
    for (team_name, members) in changes {
        if let Some(team) = teams.iter().find(|t| t.name == team_name) {
            team::save(ctx.world, team);
        }
        team::broadcast(
            ctx.registry,
            ctx.world,
            TeamChange::Leave {
                name: team_name,
                members,
            },
        );
    }
    if names.len() == 1 {
        Reply::one(format(lang::TEAM_LEAVE_SINGLE, &[&names[0]]))
    } else {
        Reply::one(format(
            lang::TEAM_LEAVE_MULTIPLE,
            &[&names.len().to_string()],
        ))
    }
}

fn resolve_score_holders(ctx: &Ctx, text: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    if text == "*" {
        return Ok(ctx
            .registry
            .snapshot()
            .into_iter()
            .map(|p| p.name.clone())
            .collect());
    }
    if text.starts_with('@') {
        let selector = Selector::parse(text).map_err(|e| e.message())?;
        let source = world_source(ctx);
        let found = selector
            .select(&source, &mut LcgRng::new(selector_seed()))
            .map_err(|e| e.message())?;
        let mut out = Vec::new();
        for e in found {
            if e.is_player {
                out.push(e.name.unwrap_or_default());
            } else {
                out.push(hyphenated_uuid(e.uuid));
            }
        }
        return Ok(out);
    }
    Ok(vec![text.to_string()])
}

fn exec_team_list(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        let teams = team::all(ctx.world);
        if teams.is_empty() {
            return Reply::one(lang::TEAM_LIST_EMPTY);
        }
        let list = teams
            .iter()
            .map(|t| t.formatted_display_name())
            .collect::<Vec<_>>()
            .join(", ");
        return Reply::one(format(lang::TEAM_LIST, &[&teams.len().to_string(), &list]));
    };
    let Some(team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.members.is_empty() {
        Reply::one(format(
            lang::TEAM_MEMBERS_EMPTY,
            &[&team.formatted_display_name()],
        ))
    } else {
        let members = team.members.iter().cloned().collect::<Vec<_>>().join(", ");
        Reply::one(format(
            lang::TEAM_MEMBERS,
            &[
                &team.formatted_display_name(),
                &team.members.len().to_string(),
                &members,
            ],
        ))
    }
}

fn exec_display_name(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> displayName <name>");
    };
    let Some(raw) = source_at(ctx, 1) else {
        return usage(ctx, "/team modify <team> displayName <name>");
    };
    let display = match component_text(raw) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.display_name == display {
        return ctx.fail(lang::NAME_UNCHANGED);
    }
    team.display_name = display;
    commit(ctx, &team);
    Reply::one(format(
        lang::NAME_SUCCESS,
        &[&team.formatted_display_name()],
    ))
}

fn exec_color(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> color <color>");
    };
    let Some(raw) = text_at(ctx, 1) else {
        return usage(ctx, "/team modify <team> color <color>");
    };
    let Some(color) = Color::parse(raw) else {
        return ctx.fail(format!("Unknown color '{raw}'"));
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.color == color {
        return ctx.fail(lang::COLOR_UNCHANGED);
    }
    team.color = color;
    commit(ctx, &team);
    Reply::one(format(
        lang::COLOR_SUCCESS,
        &[&team.formatted_display_name(), color.name()],
    ))
}

fn exec_friendly_fire(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> friendlyFire <true|false>");
    };
    let Some(allowed) = bool_at(ctx, 1) else {
        return usage(ctx, "/team modify <team> friendlyFire <true|false>");
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.friendly_fire == allowed {
        return ctx.fail(if allowed {
            lang::FRIENDLY_ALREADY_ON
        } else {
            lang::FRIENDLY_ALREADY_OFF
        });
    }
    team.friendly_fire = allowed;
    commit(ctx, &team);
    Reply::one(format(
        if allowed {
            lang::FRIENDLY_ENABLED
        } else {
            lang::FRIENDLY_DISABLED
        },
        &[&team.formatted_display_name()],
    ))
}

fn exec_see_friendly_invisibles(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(
            ctx,
            "/team modify <team> seeFriendlyInvisibles <true|false>",
        );
    };
    let Some(allowed) = bool_at(ctx, 1) else {
        return usage(
            ctx,
            "/team modify <team> seeFriendlyInvisibles <true|false>",
        );
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.see_friendly_invisibles == allowed {
        return ctx.fail(if allowed {
            lang::SIGHT_ALREADY_ON
        } else {
            lang::SIGHT_ALREADY_OFF
        });
    }
    team.see_friendly_invisibles = allowed;
    commit(ctx, &team);
    Reply::one(format(
        if allowed {
            lang::SIGHT_ENABLED
        } else {
            lang::SIGHT_DISABLED
        },
        &[&team.formatted_display_name()],
    ))
}

fn exec_nametag_visibility(ctx: &mut Ctx, value: Visibility) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> nametagVisibility <value>");
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.nametag_visibility == value {
        return ctx.fail(lang::NAMETAG_UNCHANGED);
    }
    team.nametag_visibility = value;
    commit(ctx, &team);
    Reply::one(format(
        lang::NAMETAG_SUCCESS,
        &[&team.formatted_display_name(), value.display()],
    ))
}

fn exec_death_message_visibility(ctx: &mut Ctx, value: Visibility) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> deathMessageVisibility <value>");
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.death_message_visibility == value {
        return ctx.fail(lang::DEATH_UNCHANGED);
    }
    team.death_message_visibility = value;
    commit(ctx, &team);
    Reply::one(format(
        lang::DEATH_SUCCESS,
        &[&team.formatted_display_name(), value.display()],
    ))
}

fn exec_collision(ctx: &mut Ctx, value: CollisionRule) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> collisionRule <value>");
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    if team.collision_rule == value {
        return ctx.fail(lang::COLLISION_UNCHANGED);
    }
    team.collision_rule = value;
    commit(ctx, &team);
    Reply::one(format(
        lang::COLLISION_SUCCESS,
        &[&team.formatted_display_name(), value.display()],
    ))
}

fn exec_prefix(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> prefix <prefix>");
    };
    let Some(raw) = source_at(ctx, 1) else {
        return usage(ctx, "/team modify <team> prefix <prefix>");
    };
    let prefix = match component_text(raw) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    team.prefix = prefix;
    commit(ctx, &team);
    Reply::one(format(lang::PREFIX_SUCCESS, &[&team.prefix]))
}

fn exec_suffix(ctx: &mut Ctx) -> Reply {
    let Some(name) = text_at(ctx, 0).map(str::to_string) else {
        return usage(ctx, "/team modify <team> suffix <suffix>");
    };
    let Some(raw) = source_at(ctx, 1) else {
        return usage(ctx, "/team modify <team> suffix <suffix>");
    };
    let suffix = match component_text(raw) {
        Ok(v) => v,
        Err(e) => return ctx.fail(e),
    };
    let Some(mut team) = team::find(ctx.world, &name) else {
        return not_found(ctx, &name);
    };
    team.suffix = suffix;
    commit(ctx, &team);
    Reply::one(format(lang::SUFFIX_SUCCESS, &[&team.suffix]))
}

// ---------------------------------------------------------------------------
// /teammsg
// ---------------------------------------------------------------------------

fn exec_teammsg_usage(ctx: &mut Ctx) -> Reply {
    usage(ctx, "/teammsg <message>")
}

fn exec_teammsg(ctx: &mut Ctx) -> Reply {
    let Some(handle) = ctx.player() else {
        return ctx.needs_player();
    };
    let Some(team) = team::team_of(ctx.world, &handle.name) else {
        return ctx.fail(lang::TEAMMSG_NO_TEAM);
    };
    let message = text_at(ctx, 0).unwrap_or("").to_string();
    let target = team.formatted_display_name();
    for other in ctx.registry.snapshot() {
        if other.entity_id == handle.entity_id || !team.members.contains(&other.name) {
            continue;
        }
        other.emit(
            &crate::protocol::ServerEvent::Chat(format!(
                "{} <{}> {}",
                target, handle.name, message
            )),
            ctx.world,
        );
    }
    Reply::one(format!("-> {} <{}> {}", target, handle.name, message))
}

// ---------------------------------------------------------------------------
// Chat component parsing
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Style {
    color: Option<String>,
    bold: bool,
    italic: bool,
    underlined: bool,
    strikethrough: bool,
    obfuscated: bool,
}

impl Style {
    fn prefix(&self) -> String {
        let mut out = String::new();
        if let Some(color) = &self.color {
            out.push_str(color);
        }
        if self.bold {
            out.push_str("§l");
        }
        if self.italic {
            out.push_str("§o");
        }
        if self.underlined {
            out.push_str("§n");
        }
        if self.strikethrough {
            out.push_str("§m");
        }
        if self.obfuscated {
            out.push_str("§k");
        }
        out
    }
}

fn color_code(name: &str) -> Option<String> {
    Color::parse(name)
        .and_then(Color::legacy)
        .map(|c| format!("§{c}"))
}

fn field<'a>(fields: &'a [(String, Nbt)], key: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn string_field(fields: &[(String, Nbt)], key: &str) -> Option<String> {
    match field(fields, key) {
        Some(Nbt::Str(s)) => Some(s.clone()),
        Some(Nbt::Byte(v)) => Some(v.to_string()),
        Some(Nbt::Short(v)) => Some(v.to_string()),
        Some(Nbt::Int(v)) => Some(v.to_string()),
        Some(Nbt::Long(v)) => Some(v.to_string()),
        _ => None,
    }
}

fn bool_field(fields: &[(String, Nbt)], key: &str) -> bool {
    match field(fields, key) {
        Some(Nbt::Byte(v)) => *v != 0,
        Some(Nbt::Str(s)) => s == "true",
        _ => false,
    }
}

fn render_component(nbt: &Nbt, style: &Style, out: &mut String) {
    match nbt {
        Nbt::Str(s) => {
            out.push_str(&style.prefix());
            out.push_str(s);
        }
        Nbt::List(items) => {
            for item in items {
                render_component(item, style, out);
            }
        }
        Nbt::Compound(fields) => {
            let mut style = style.clone();
            if let Some(color) = string_field(fields, "color").and_then(|c| color_code(&c)) {
                style.color = Some(color);
            }
            style.bold |= bool_field(fields, "bold");
            style.italic |= bool_field(fields, "italic");
            style.underlined |= bool_field(fields, "underlined");
            style.strikethrough |= bool_field(fields, "strikethrough");
            style.obfuscated |= bool_field(fields, "obfuscated");
            if let Some(text) = string_field(fields, "text") {
                out.push_str(&style.prefix());
                out.push_str(&text);
            } else if let Some(key) = string_field(fields, "translate") {
                out.push_str(&style.prefix());
                out.push_str(&key);
            }
            if let Some(Nbt::List(extra)) = field(fields, "extra") {
                for item in extra {
                    render_component(item, &style, out);
                }
            }
        }
        Nbt::Byte(v) => out.push_str(&v.to_string()),
        Nbt::Short(v) => out.push_str(&v.to_string()),
        Nbt::Int(v) => out.push_str(&v.to_string()),
        Nbt::Long(v) => out.push_str(&v.to_string()),
        Nbt::Float(v) => out.push_str(&v.to_string()),
        Nbt::Double(v) => out.push_str(&v.to_string()),
        other => {
            // A component this server does not model still renders something
            // deterministic rather than being silently dropped.
            out.push_str(&format!("{other:?}"));
        }
    }
}

/// Parse a vanilla `ComponentArgument` (SNBT/JSON text) into legacy-styled
/// plain text. A malformed component is an error, never a panic.
fn component_text(raw: &str) -> Result<String, String> {
    let nbt = super::snbt::parse(raw.trim()).map_err(|e| format!("Invalid chat component: {e}"))?;
    let mut out = String::new();
    render_component(&nbt, &Style::default(), &mut out);
    Ok(out)
}
