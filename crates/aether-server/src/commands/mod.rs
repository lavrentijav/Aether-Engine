//! Chat commands over the world history: inspection, lookup, rollback and the
//! item audit.
//!
//! These are the operator-facing half of [`aether_world::journal`]. Everything
//! here is a read or a rollback — there is no command that edits history,
//! because a history that can be edited answers no question worth asking.
//!
//! ```text
//! /inspect                       toggle inspect mode
//! /lookup <filters>              search the history
//! /rollback <filters>            undo what a lookup would have shown
//! /page next|prev|<n>            move through the last answer
//! /stash                         open the recovered-blocks container
//! /audit                         item duplication report
//! ```
//!
//! Filters are documented in [`query`]. In inspect mode, hitting or
//! right-clicking a block reports its history **instead of** changing it —
//! which is why the mode has to be per-player state and cannot be inferred
//! from the packet.

pub mod pager;
pub mod query;
pub mod registry;

use aether_api::{JournalActor, Ledger};
use aether_world::journal::{now_ms, Event, EventBody};
use aether_world::BlockStateId;

use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;
use pager::Pager;
use query::{Context, Query};

/// Per-player command state: inspect mode, and the last answer to page
/// through.
#[derive(Default)]
pub struct Session {
    /// While set, digging and placing inspect instead of editing.
    pub inspecting: bool,
    /// The last multi-line answer, parked for `/page`.
    pub pager: Pager,
}

/// A command's reply: lines to send back to whoever typed it.
pub struct Reply(pub Vec<String>);

impl Reply {
    fn one(s: impl Into<String>) -> Reply {
        Reply(vec![s.into()])
    }
}

/// The usage text, sent for an unrecognised or malformed command.
fn usage() -> Reply {
    Reply(vec![
        "History commands:".into(),
        "  /inspect — toggle: hitting a block reports its history".into(),
        "  /lookup <filters> — search; /rollback <filters> — undo".into(),
        "  /page next|prev|<n> — move through the last answer".into(),
        "  /inv — what the server thinks you are carrying".into(),
        "  /stash — open the recovered blocks from your last rollback".into(),
        "  /audit — item duplication report".into(),
        "Filters: player:<name> time:30m block:stone action:place radius:20".into(),
        "Game: /gamemode /time /give /summon /tp /heal /killall (operators), /kill /spawn".into(),
        "Economy:".into(),
        "  /balance [player]  /pay <player> <amount>".into(),
        "  /sell <count> <price> [duration]  — sells what you hold".into(),
        "  /market [item:diamond] [max:5]  /buy <id> [count]".into(),
        "  /listings  /unlist <id>".into(),
    ])
}

/// Every built-in command name, across the game, economy and history layers.
///
/// Passed to the plugin host as its reserved set, so a plugin can never be
/// handed one of these names even if the dispatch order below ever changed.
/// Keep it in step with the `match`es that handle commands.
// Only consulted by the plugin host, which is absent without the `plugins`
// feature; kept in every build so the list has one home.
#[allow(dead_code)]
pub const RESERVED_COMMANDS: &[&str] = &[
    // game
    "gamemode",
    "gm",
    "time",
    "give",
    "summon",
    "killall",
    "kill",
    "spawn",
    "tp",
    "block",
    "food",
    "heal",
    // history
    "inspect",
    "i",
    "lookup",
    "l",
    "rollback",
    "rb",
    "page",
    "pg",
    "inv",
    "inventory",
    "stash",
    "audit",
    "cache",
    // economy
    "balance",
    "bal",
    "money",
    "pay",
    "sell",
    "market",
    "buy",
    "listings",
    "unlist",
    "econ",
];

/// Handle `text` if it is a command. `None` means it was ordinary chat.
pub fn dispatch(
    text: &str,
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Option<Reply> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }
    let mut words = text[1..].split_whitespace();
    let cmd = words.next().unwrap_or_default();
    let args: Vec<&str> = words.collect();
    // **Built-ins first.** A plugin command is only tried once every built-in
    // verb — including the operator-gated game commands — has declined the
    // name, so a plugin can never shadow a built-in and its permission check.
    // `[plugins]`-registered verbs that do not collide are unaffected.
    if let Some(lines) = crate::game::commands::dispatch(cmd, &args, handle, registry, world) {
        return Some(Reply(lines));
    }
    // The economy owns its own verbs; it answers `None` for anything else, so
    // a command name never means two things.
    if let Some(reply) = crate::economy::commands::dispatch(
        cmd,
        &args,
        handle,
        registry,
        world,
        crate::economy::get(),
    ) {
        return Some(reply);
    }
    Some(match cmd {
        "inspect" | "i" => toggle_inspect(handle),
        "lookup" | "l" => lookup(&args, handle, registry, world),
        "rollback" | "rb" => rollback(&args, handle, registry, world),
        "page" | "pg" => page(&args, handle),
        "inv" | "inventory" => inventory(handle, world),
        "stash" => crate::stash::open_command(handle, world),
        "audit" => audit(world),
        "cache" => cache(world),
        "help" | "?" => help(&args, handle),
        "aether" => aether_admin(&args, handle, world),
        // Everything else is a fallback. Plugin commands are resolved through
        // the registry here — **after** every core name has been offered, and
        // with the same permission gate — so they can never shadow a core
        // command; core stubs and usage are the last resort.
        _ => fallback(cmd, &args, handle),
    })
}

/// The last-resort dispatcher: a plugin command (through the registry, with
/// its permission check), else a core stub or the usage text.
///
/// This is the one place a plugin command is reached from. Because
/// `registry::register_plugin` refuses core names, `registry::plugin_command`
/// here can only answer a name the core does not own, so the core's gate is
/// never bypassed.
fn fallback(cmd: &str, args: &[&str], handle: &PlayerHandle) -> Reply {
    if let Some(entry) = registry::plugin_command(cmd) {
        if !permitted(handle, &entry.node, entry.op) {
            return Reply::one("You do not have permission to run this command.");
        }
        let plugin_args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        return match crate::plugins::on_command(handle, cmd, &plugin_args) {
            Some(lines) => Reply(lines),
            // Registered but no live plugin answered — the plugin is disabled
            // or failed to load; say so rather than showing the usage text.
            None => Reply::one(format!("/{cmd} is not available right now")),
        };
    }
    stub(cmd, handle)
}

/// `/aether <subcommand>` — server administration that does not belong to any
/// other command.
///
/// Today only `auth migrate` (AU1): moving an offline player's saved state to
/// the Mojang uuid they now own. Migration is deliberately manual — the
/// server cannot know a fresh online login is the same person as an old
/// offline one.
fn aether_admin(args: &[&str], handle: &PlayerHandle, world: &DemoWorld) -> Reply {
    if !crate::is_operator(handle) {
        return Reply::one("You do not have permission to run this command.");
    }
    match args {
        ["auth", "migrate", name] => migrate_offline_player(name, world),
        _ => Reply::one("Usage: /aether auth migrate <name>"),
    }
}

/// Move a name's saved player state and inventory from its offline uuid to
/// the premium uuid the lock table records.
///
/// An existing premium record is never overwritten: if the player has already
/// played online, their new data is the truth and the old offline blob is left
/// where it is.
fn migrate_offline_player(name: &str, world: &DemoWorld) -> Reply {
    let Some(lock) = crate::auth::store::get_by_name(world, name) else {
        return Reply::one(format!(
            "`{name}` is not a locked premium name; nothing to migrate."
        ));
    };
    let offline = aether_api::player::offline_uuid(name);
    if offline == lock.uuid {
        return Reply::one(format!(
            "`{name}` already keys on its premium uuid {}; nothing to migrate.",
            crate::auth::api::hyphenated_uuid(lock.uuid)
        ));
    }
    let pairs = [
        (
            crate::game::player::key(offline),
            crate::game::player::key(lock.uuid),
        ),
        (
            crate::inventory::key(offline),
            crate::inventory::key(lock.uuid),
        ),
    ];
    let mut moved = 0;
    let mut skipped = 0;
    for (from, to) in pairs {
        if world.get_meta(&to).ok().flatten().is_some() {
            skipped += 1;
            continue;
        }
        if let Ok(Some(blob)) = world.get_meta(&from) {
            if world.put_meta(&to, &blob).is_ok() {
                moved += 1;
            }
        }
    }
    Reply::one(format!(
        "migrated {moved} record(s) for `{name}` from offline uuid {} to premium uuid {}{}.",
        crate::auth::api::hyphenated_uuid(offline),
        crate::auth::api::hyphenated_uuid(lock.uuid),
        if skipped > 0 {
            format!(" ({skipped} left untouched: the premium uuid already has data)")
        } else {
            String::new()
        }
    ))
}

/// Whether `handle` may run a command needing `op`.
///
/// The check goes through the permission core ([`crate::permissions`]): the
/// player's level comes from the store (seeded from `config.operators`), the
/// node's default resolves from the store, and the command's `op` level is the
/// floor. This is the single gate the core stubs and plugin commands both use.
pub fn permitted(handle: &PlayerHandle, node: &str, op: u8) -> bool {
    crate::permissions::check(crate::permissions::level(&handle.name, handle.ip), node, op)
}

/// The level half of [`permitted`], without a handle: the decision itself, so
/// it can be tested directly.
pub fn permitted_level(level: u8, node: &str, op: u8) -> bool {
    crate::permissions::check(crate::permissions::Level::new(level), node, op)
}

/// `/help [command]` — list the commands that are specified but not implemented yet, or explain one.
///
/// This is a stop-gap until the real tree-driven `/help` (CMD-05): it makes the catalogue
/// discoverable while each command is stubbed with its own `not implemented yet (CMD-nn)`.
fn help(args: &[&str], handle: &PlayerHandle) -> Reply {
    if let Some(name) = args.first() {
        return match registry::find(name) {
            Some(e) => Reply::one(format!(
                "/{} — {} (op level {}), planned in {}",
                e.name, e.node, e.op, e.task
            )),
            None => Reply::one(format!(
                "no entry for `{name}`; /help lists the commands still to come"
            )),
        };
    }
    let mut lines = registry::help_lines();
    lines.push(format!(
        "({} commands pending, {} runnable now)",
        registry::CORE.len(),
        if crate::is_operator(handle) {
            "all"
        } else {
            "level-0"
        }
    ));
    Reply(lines)
}

/// The fallback for a name no dispatcher answered: a known-but-unimplemented command replies with
/// its task id (and enforces its permission), anything else is usage.
fn stub(cmd: &str, handle: &PlayerHandle) -> Reply {
    let Some(e) = registry::find(cmd) else {
        return usage();
    };
    if !permitted(handle, e.node, e.op) {
        return Reply::one("You do not have permission to run this command.");
    }
    Reply::one(format!("/{} is not implemented yet ({})", e.name, e.task))
}

/// `/cache` — how the generated-column cache is doing.
///
/// Worth a command rather than a log line: the cache is invisible when it
/// works, so the only way to know whether it is working is to ask.
fn cache(world: &DemoWorld) -> Reply {
    Reply::one(format!("column cache: {}", world.generator().summary()))
}

fn toggle_inspect(handle: &PlayerHandle) -> Reply {
    let mut s = handle.session();
    s.inspecting = !s.inspecting;
    Reply::one(if s.inspecting {
        "Inspect mode ON — hit or right-click a block to read its history. /inspect again to stop."
    } else {
        "Inspect mode OFF."
    })
}

/// Where the caller is standing, floored to block coordinates.
fn here(handle: &PlayerHandle) -> (i32, i32, i32) {
    let p = handle.pos();
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}

/// Format one event as a chat line, with its cause if it has one.
fn describe(e: &Event, world: &DemoWorld, actor_name: &dyn Fn(u128) -> String) -> Option<String> {
    let line = describe_body(e, world, actor_name)?;
    Some(match e.cause {
        Some(c) => format!("{line} (because of #{c})"),
        None => line,
    })
}

/// A place, short: `inv:36`, `ground 1 64 2`, `chest 4 70 9:3`.
fn short_place(p: &aether_world::journal::Place) -> String {
    use aether_world::journal::Place;
    match p {
        Place::Inventory { slot, .. } => match *slot {
            crate::game::provenance::CURSOR_SLOT => "cursor".into(),
            s if s <= crate::game::provenance::GRID_SLOT => "crafting grid".into(),
            s => format!("inv:{s}"),
        },
        Place::Ground { x, y, z } => format!("ground {x} {y} {z}"),
        Place::Container { x, y, z, slot } => format!("container {x} {y} {z}:{slot}"),
        Place::Nowhere => "nowhere".into(),
    }
}

/// The display name of an item id, resolved through the world's registry so a
/// modded item reads by its real name.
fn item_name(world: &DemoWorld, id: aether_world::ItemTypeId) -> String {
    world
        .item_name(id)
        .unwrap_or_else(|| format!("item#{}", id.0))
}

fn describe_body(
    e: &Event,
    world: &DemoWorld,
    actor_name: &dyn Fn(u128) -> String,
) -> Option<String> {
    let ago = describe_age(now_ms().saturating_sub(e.at_ms));
    match &e.body {
        EventBody::BlockSet { x, y, z, from, to } => {
            let name = |b: BlockStateId| {
                world
                    .block_name_of(b)
                    .unwrap_or_else(|| format!("#{}", b.0))
            };
            // Read as "who did what", with the verb chosen from the change so
            // the common cases (place, break) do not read as a state diff.
            let what = if *to == BlockStateId::AIR {
                format!("broke {}", name(*from))
            } else if *from == BlockStateId::AIR {
                format!("placed {}", name(*to))
            } else {
                format!("{} -> {}", name(*from), name(*to))
            };
            Some(format!(
                "  {ago} {} {what} at {x} {y} {z}  #{}",
                actor_name(e.actor.0),
                e.seq
            ))
        }
        EventBody::ItemMint {
            item,
            count,
            to: aether_world::journal::Place::Ground { x, y, z },
            ..
        } => Some(format!(
            "  {ago} {count}x {} dropped at {x} {y} {z}  #{}",
            item_name(world, *item),
            e.seq
        )),
        EventBody::ItemMint { item, count, .. } => Some(format!(
            "  {ago} {} received {count}x {}  #{}",
            actor_name(e.actor.0),
            item_name(world, *item),
            e.seq
        )),
        EventBody::ItemMove {
            uid,
            from,
            to,
            count,
        } => Some(format!(
            "  {ago} {} moved {count}x item {} {} → {}  #{}",
            actor_name(e.actor.0),
            uid.0,
            short_place(from),
            short_place(to),
            e.seq
        )),
        EventBody::ItemDestroy { uid, reason, .. } => Some(format!(
            "  {ago} item {} destroyed ({})  #{}",
            uid.0,
            reason.name(),
            e.seq
        )),
        EventBody::ItemMerge { into, from, count } => Some(format!(
            "  {ago} {count}x item {} merged into {}  #{}",
            from.0, into.0, e.seq
        )),
        EventBody::ItemDerive {
            item, count, from, ..
        } => Some(format!(
            "  {ago} {} made {count}x {} from {} item(s)  #{}",
            actor_name(e.actor.0),
            item_name(world, *item),
            from.len(),
            e.seq
        )),
        EventBody::Custom { tag, bytes } => Some(format!(
            "  {ago} {} custom event tag {tag} ({} bytes)  #{}",
            actor_name(e.actor.0),
            bytes.len(),
            e.seq
        )),
    }
}

/// "3m ago", "2h ago" — the coarsest unit that is still informative, because
/// the exact millisecond is in the sequence number if anyone needs it.
fn describe_age(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

/// How many of the newest matching events `/lookup` and `/inspect` read.
///
/// A bound, not the whole history: an operator pages through recent activity,
/// and loading 400k events to render the newest few would cost seconds and all
/// of it in RAM. The queries behind it ([`aether_world::journal::Journal`]'s
/// bounded reads) stop after this many and never materialise the rest.
const HISTORY_LIMIT: usize = 500;

fn lookup(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Reply {
    let lookup_player = |name: &str| registry.uuid_of(name);
    let ctx = Context {
        lookup_player: &lookup_player,
        here: here(handle),
        now_ms: now_ms(),
    };
    let q = match Query::parse(args, &ctx) {
        Ok(q) => q,
        Err(e) => return Reply::one(e.to_string()),
    };
    // Narrow by the player index when the query names one: that reads only
    // that player's events rather than the newest few of everyone's.
    let journal = world.journal();
    let events = match q.filter.actor {
        Some(a) => journal.actor_events_recent(a, HISTORY_LIMIT),
        None => journal.recent_events(HISTORY_LIMIT),
    };
    let events = match events {
        Ok(e) => e,
        Err(e) => return Reply::one(format!("history unreadable: {e}")),
    };
    let name_of = |b: BlockStateId| world.block_name_of(b).unwrap_or_default();
    let actor_name = |u: u128| registry.name_of(u).unwrap_or_else(|| short_uuid(u));
    let lines: Vec<String> = events
        .iter()
        .rev()
        .filter(|e| q.accepts(e, &name_of))
        .filter_map(|e| describe(e, world, &actor_name))
        .collect();
    let mut s = handle.session();
    s.pager.set("Lookup", lines);
    Reply(s.pager.render())
}

/// The first eight hex digits of a UUID: enough to tell two offline players
/// apart in a log line without turning every line into 32 characters of hex.
fn short_uuid(u: u128) -> String {
    format!("{:08x}", (u >> 96) as u32)
}

fn page(args: &[&str], handle: &PlayerHandle) -> Reply {
    let mut s = handle.session();
    if s.pager.is_empty() {
        return Reply::one("nothing to page through — run /lookup or /inspect first");
    }
    let lines = match args.first().copied() {
        Some("next") | Some("n") | None => s.pager.step(1),
        Some("prev") | Some("p") | Some("back") => s.pager.step(-1),
        Some(n) => match n.parse::<usize>() {
            Ok(n) => s.pager.goto(n),
            Err(_) => return Reply::one("usage: /page next | prev | <number>"),
        },
    };
    Reply(lines)
}

fn rollback(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Reply {
    let lookup_player = |name: &str| registry.uuid_of(name);
    let ctx = Context {
        lookup_player: &lookup_player,
        here: here(handle),
        now_ms: now_ms(),
    };
    let q = match Query::parse(args, &ctx) {
        Ok(q) => q,
        Err(e) => return Reply::one(e.to_string()),
    };
    if q.is_unrestricted() {
        return Reply(vec![
            "refusing to roll back the entire history with no filter.".into(),
            "add at least one: player:<name>, time:<duration>, radius:<n>.".into(),
            "to really undo everything, use  /rollback time:9999d".into(),
        ]);
    }
    let me = JournalActor(handle.uuid);
    let name_of = |b: BlockStateId| world.block_name_of(b).unwrap_or_default();

    match world.rollback_where(me, &q.filter, &|e| q.accepts(e, &name_of)) {
        Ok(plan) if plan.is_empty() => Reply::one("nothing matched — nothing rolled back"),
        Ok(plan) => {
            // Every restored block is pushed to everyone: a rollback only the
            // caller could see would look like it had failed.
            for r in &plan {
                // A rollback can reopen or reseal a cave, so the visibility
                // cache must forget the restored section too — otherwise the
                // graph still shows the old wall and the anti-xray mask keeps
                // hiding what is now visible (finding 3).
                crate::visibility::invalidate_block(r.x, r.y, r.z);
                let ev = ServerEvent::BlockChange {
                    x: r.x,
                    y: r.y,
                    z: r.z,
                    block: r.block,
                };
                handle.emit(&ev, world);
                registry.broadcast_except(handle.entity_id, &ev, world);
            }
            let n = crate::stash::deposit(handle, world, &plan);
            Reply(vec![
                format!("rolled back {} block(s)", plan.len()),
                format!("{n} recovered block type(s) waiting in /stash"),
            ])
        }
        Err(e) => Reply::one(format!("rollback failed: {e}")),
    }
}

/// Inspect the block at `(x, y, z)` and park the answer in the player's pager.
///
/// Called from the play loop when a dig or place arrives while inspect mode is
/// on. It must not touch the world: the caller is responsible for telling the
/// client what is really there, since the client has already predicted the
/// change it was denied.
pub fn inspect_block(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    x: i32,
    y: i32,
    z: i32,
) -> Reply {
    let events = match world
        .journal()
        .column_events_recent(x >> 4, z >> 4, HISTORY_LIMIT)
    {
        Ok(e) => e,
        Err(e) => return Reply::one(format!("history unreadable: {e}")),
    };
    let actor_name = |u: u128| registry.name_of(u).unwrap_or_else(|| short_uuid(u));
    let lines: Vec<String> = events
        .iter()
        .rev()
        .filter(|e| e.position() == Some((x, y, z)))
        .filter_map(|e| describe(e, world, &actor_name))
        .collect();
    let current = world
        .block_name_of(world.get_block(x, y, z))
        .unwrap_or_else(|| "?".into());
    let mut s = handle.session();
    if lines.is_empty() {
        s.pager.set(format!("{x} {y} {z}"), Vec::new());
        return Reply(vec![format!(
            "{x} {y} {z} is {current} — untouched since the world was generated"
        )]);
    }
    s.pager.set(format!("{x} {y} {z} (now {current})"), lines);
    Reply(s.pager.render())
}

/// What the server believes the player is carrying.
///
/// Worth having as a command rather than only as internal state: the server's
/// view and the client's can drift — in creative the client is the one filling
/// slots — and this is the only way to see that they have, short of reading a
/// database.
fn inventory(handle: &PlayerHandle, world: &DemoWorld) -> Reply {
    let name = |id: aether_world::ItemTypeId| {
        world
            .item_name(id)
            .unwrap_or_else(|| format!("item#{}", id.0))
    };
    let inv = handle.inventory();
    if inv.is_empty() {
        return Reply::one("the server has you carrying nothing");
    }
    let mut lines = vec![format!("Inventory (holding slot {}):", inv.held_slot() + 1)];
    for (slot, stack) in inv.occupied() {
        let where_ = if (crate::inventory::FIRST_HOTBAR..crate::inventory::OFFHAND).contains(&slot)
        {
            format!("hotbar {}", slot - crate::inventory::FIRST_HOTBAR + 1)
        } else if slot == crate::inventory::OFFHAND {
            "offhand".to_string()
        } else {
            format!("slot {slot}")
        };
        // The uid is shown in decimal: the same spelling the mirror stores and
        // `/audit` prints, so an operator can copy it between the two.
        lines.push(format!(
            "  {where_}: {}x {}  [{}]",
            stack.count,
            name(stack.item),
            stack.uid.0
        ));
    }
    // Release the inventory before touching the entity list: the tick holds
    // the entity lock and takes player locks, so the reverse here would be a
    // lock-order inversion. See docs/storage-v2/lock-order.md.
    drop(inv);
    // Anything of theirs on the floor is part of the answer to "what do I
    // have": it is theirs, it is just not in a slot yet.
    let mine = crate::game::entities::owned_items(handle.uuid);
    if !mine.is_empty() {
        lines.push(format!("On the ground ({} stack(s)):", mine.len()));
        for (item, count, p, left) in mine {
            lines.push(format!(
                "  {count}x {} at {:.0} {:.0} {:.0} — walk over it ({left}s left)",
                name(item),
                p.x,
                p.y,
                p.z
            ));
        }
    }
    let mut s = handle.session();
    s.pager.set("Inventory", lines.split_off(1));
    Reply(s.pager.render())
}

fn audit(world: &DemoWorld) -> Reply {
    // Streamed: the ledger still tracks every instance, but the events
    // themselves are decoded one at a time and never held as a `Vec`.
    let mut ledger = Ledger::default();
    if let Err(e) = world.journal().for_each_event(|ev| {
        ledger.apply(ev);
        Ok(())
    }) {
        return Reply::one(format!("history unreadable: {e}"));
    }
    if ledger.anomalies().is_empty() {
        return Reply::one(format!(
            "{} item instance(s) tracked, nothing anomalous",
            ledger.len()
        ));
    }
    let mut lines = vec![format!(
        "{} item instance(s), {} anomal(y/ies):",
        ledger.len(),
        ledger.anomalies().len()
    )];
    for a in ledger.anomalies().iter().take(10) {
        lines.push(format!("  {a:?}"));
    }
    Reply(lines)
}

#[cfg(test)]
mod tests {
    use super::query::{parse_duration_ms, Action, Context, ParseError, Query};
    use aether_world::journal::{ActorId, Event, EventBody};
    use aether_world::BlockStateId;

    const STONE: BlockStateId = BlockStateId(1);
    const AIR: BlockStateId = BlockStateId::AIR;

    fn ctx() -> Context<'static> {
        Context {
            lookup_player: &|n| (n == "steve").then_some(7),
            here: (0, 64, 0),
            now_ms: 1_000_000,
        }
    }

    fn ev(seq: u64, actor: u128, from: BlockStateId, to: BlockStateId, x: i32) -> Event {
        Event {
            seq,
            at_ms: 1_000_000,
            actor: ActorId(actor),
            cause: None,
            body: EventBody::BlockSet {
                x,
                y: 64,
                z: 0,
                from,
                to,
            },
        }
    }

    fn names(b: BlockStateId) -> String {
        match b.0 {
            0 => "minecraft:air".into(),
            1 => "minecraft:stone".into(),
            _ => "minecraft:unknown".into(),
        }
    }

    #[test]
    fn the_reserved_set_covers_the_operator_verbs() {
        // The host refuses these before any plugin sees them; if one were
        // dropped from the list a plugin could shadow it again.
        for verb in [
            "give", "tp", "gamemode", "summon", "killall", "time", "heal", "food", "pay", "econ",
        ] {
            assert!(
                super::RESERVED_COMMANDS.contains(&verb),
                "`{verb}` must be reserved"
            );
        }
    }

    #[test]
    fn the_permission_gate_is_level_against_op() {
        // A declared public node: the op level is the only remaining gate, so
        // a non-op (level 0) may run op-0 commands and an op (level 4) any.
        crate::permissions::declare_node("test.gate.allow", crate::permissions::NodeDefault::Allow)
            .unwrap();
        assert!(super::permitted_level(0, "test.gate.allow", 0));
        assert!(!super::permitted_level(0, "test.gate.allow", 1));
        assert!(!super::permitted_level(0, "test.gate.allow", 4));
        assert!(super::permitted_level(4, "test.gate.allow", 4));
        assert!(super::permitted_level(4, "test.gate.allow", 0));
        // An undeclared node fails closed for a non-op, even at op level 0.
        assert!(!super::permitted_level(0, "test.gate.unset", 0));
        assert!(super::permitted_level(4, "test.gate.unset", 0));
    }

    #[test]
    fn a_public_plugin_command_is_available_to_a_non_op() {
        // The historian fixture declares `/hist` plainly (`commands = ["hist"]`),
        // i.e. op level 0. Registered through the core registry the way the
        // server does at plugin load, its node must resolve so that an ordinary
        // player — no operator rights — may run it. This is the property the
        // SDK `history` e2e relies on: that test is about the `history.read`
        // capability, not about command rights.
        crate::commands::registry::register_plugin(
            "hist_fixture_test",
            "historian",
            "plugin.historian.hist",
            0,
        )
        .unwrap();
        assert!(
            super::permitted_level(0, "plugin.historian.hist", 0),
            "an op-0 plugin command must be available to a non-op"
        );
        assert!(super::permitted_level(4, "plugin.historian.hist", 0));
        // The CMD-01 gate is not weakened: a node nobody declared still fails
        // closed for a non-op.
        assert!(!super::permitted_level(0, "plugin.historian.unset", 0));
    }

    #[test]
    fn the_registry_owns_both_implemented_and_specified_core_names() {
        // `core_owns` is the ownership check a plugin registration faces: it
        // must reject an implemented name even if the reserved list drifted,
        // and a specified-but-unimplemented one.
        assert!(super::registry::core_owns("give"), "implemented");
        assert!(super::registry::core_owns("stop"), "specified stub");
        assert!(!super::registry::core_owns("definitely_a_plugin_name"));
    }

    #[test]
    fn durations_accept_every_documented_suffix() {
        assert_eq!(parse_duration_ms("90s"), Some(90_000));
        assert_eq!(parse_duration_ms("30m"), Some(1_800_000));
        assert_eq!(parse_duration_ms("2h"), Some(7_200_000));
        assert_eq!(parse_duration_ms("3d"), Some(259_200_000));
        // A bare number is minutes: what an operator types in a hurry.
        assert_eq!(parse_duration_ms("5"), Some(300_000));
        assert_eq!(parse_duration_ms("nonsense"), None);
        assert_eq!(parse_duration_ms(""), None);
    }

    #[test]
    fn a_mistyped_filter_is_an_error_and_not_silently_ignored() {
        // The failure this prevents: `/rollback playr:steve` parsing as "roll
        // back everything" because the unknown key was skipped.
        assert_eq!(
            Query::parse(&["playr:steve"], &ctx()).err(),
            Some(ParseError::UnknownKey("playr".into()))
        );
        assert!(matches!(
            Query::parse(&["steve"], &ctx()),
            Err(ParseError::NotAPair(_))
        ));
        assert!(matches!(
            Query::parse(&["action:sideways"], &ctx()),
            Err(ParseError::BadValue { .. })
        ));
        assert!(matches!(
            Query::parse(&["player:nobody"], &ctx()),
            Err(ParseError::NoSuchPlayer(_))
        ));
    }

    #[test]
    fn an_empty_query_is_recognised_as_unrestricted() {
        assert!(Query::parse(&[], &ctx()).unwrap().is_unrestricted());
        for one in [
            "player:steve",
            "time:5m",
            "radius:10",
            "block:stone",
            "action:place",
        ] {
            assert!(
                !Query::parse(&[one], &ctx()).unwrap().is_unrestricted(),
                "{one} should restrict the query"
            );
        }
    }

    #[test]
    fn the_block_filter_matches_either_side_of_the_change() {
        // Both directions matter: "who broke stone here" and "who placed
        // stone here" are the same question asked from opposite ends.
        let q = Query::parse(&["block:stone"], &ctx()).unwrap();
        assert!(q.accepts(&ev(1, 7, AIR, STONE, 0), &names), "placed");
        assert!(q.accepts(&ev(2, 7, STONE, AIR, 0), &names), "broke");
        assert!(!q.accepts(&ev(3, 7, AIR, BlockStateId(9), 0), &names));
    }

    #[test]
    fn the_short_block_name_is_expanded_to_the_namespaced_one() {
        let q = Query::parse(&["block:stone"], &ctx()).unwrap();
        assert_eq!(q.block.as_deref(), Some("minecraft:stone"));
        let q = Query::parse(&["block:mymod:widget"], &ctx()).unwrap();
        assert_eq!(q.block.as_deref(), Some("mymod:widget"));
    }

    #[test]
    fn action_place_and_break_are_complementary() {
        let place = Query::parse(&["action:place"], &ctx()).unwrap();
        let brk = Query::parse(&["action:break"], &ctx()).unwrap();
        let placed = ev(1, 7, AIR, STONE, 0);
        let broke = ev(2, 7, STONE, AIR, 0);
        assert!(place.accepts(&placed, &names) && !place.accepts(&broke, &names));
        assert!(brk.accepts(&broke, &names) && !brk.accepts(&placed, &names));
        assert_eq!(place.action, Some(Action::Place));
    }

    #[test]
    fn the_radius_filter_is_measured_from_the_caller() {
        let q = Query::parse(&["radius:5"], &ctx()).unwrap();
        assert!(q.accepts(&ev(1, 7, AIR, STONE, 3), &names));
        assert!(!q.accepts(&ev(2, 7, AIR, STONE, 40), &names));
    }

    #[test]
    fn filters_combine_with_and_not_or() {
        // `player:steve action:break` must mean "steve, breaking" and not
        // "steve, or anyone breaking" — the difference between undoing one
        // player's damage and undoing the server's.
        let q = Query::parse(&["player:steve", "action:break"], &ctx()).unwrap();
        assert!(q.accepts(&ev(1, 7, STONE, AIR, 0), &names));
        assert!(!q.accepts(&ev(2, 7, AIR, STONE, 0), &names), "wrong action");
        assert!(!q.accepts(&ev(3, 9, STONE, AIR, 0), &names), "wrong player");
    }
}
