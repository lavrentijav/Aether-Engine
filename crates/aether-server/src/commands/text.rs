//! Chat commands: `/say`, `/me`, `/msg` (and `/tell` `/w`) and `/tellraw`.
//!
//! These are the vanilla chat verbs, and the ownership rule is permanent: a
//! plugin may add `/r`, `/mail`, `/broadcast` … and hook chat, but it may not
//! re-register `/msg`, `/me`, `/say` or `/tellraw` (CMD-40).
//!
//! Security shape, in one place:
//!
//! * a **sender's name** is put into a component as a literal
//!   ([`TextComponent::text`]), never spliced into text that is parsed again;
//! * command message text goes through [`crate::chatlog::command_text`], which
//!   removes control characters and `§` and caps the length;
//! * `/tellraw` parses a bounded component ([`TextComponent::from_nbt`]) —
//!   depth, node count, text size and encoded size are all capped;
//! * a per-player broadcast limiter (separate from the log limiter) bounds how
//!   fast one client can make every other client render chat.
//!
//! `/msg` is private: the recipient sees the incoming line, the sender sees the
//! outgoing line as command feedback, and nobody else sees either. A message to
//! yourself is allowed, as in vanilla; a non-player target is refused.

use aether_api::Vector3;

use crate::commands::coordinates::Rotation;
use crate::commands::feedback::lang;
use crate::commands::selector::{EntityRef, Gamemode, LcgRng, Selector, SelectorSource};
use crate::commands::tree::{self, ArgKind, Ctx, Node, Requirement, Source, Value};
use crate::commands::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::text::TextComponent;
use crate::protocol::ServerEvent;

/// Register the chat commands on the tree.
pub fn nodes() -> Vec<Node> {
    vec![
        tree::Builder::literal("say")
            .requires(Requirement::Level {
                op: 2,
                node: "minecraft.command.say",
            })
            .usage("<message>")
            .executable(exec_say)
            .then(tree::Builder::arg("message", ArgKind::Message).executable(exec_say))
            .build(),
        tree::Builder::literal("me")
            .usage("<action>")
            .executable(exec_me)
            .then(tree::Builder::arg("action", ArgKind::Message).executable(exec_me))
            .build(),
        tree::Builder::literal("msg")
            .alias("tell")
            .alias("w")
            .usage("<targets> <message>")
            .executable(exec_msg)
            .then(
                tree::Builder::arg("targets", ArgKind::Entities)
                    .then(tree::Builder::arg("message", ArgKind::Message).executable(exec_msg)),
            )
            .build(),
        tree::Builder::literal("tellraw")
            .requires(Requirement::Level {
                op: 2,
                node: "minecraft.command.tellraw",
            })
            .usage("<targets> <message>")
            .executable(exec_tellraw)
            .then(
                tree::Builder::arg("targets", ArgKind::Entities).then(
                    tree::Builder::arg("message", ArgKind::Component).executable(exec_tellraw),
                ),
            )
            .build(),
    ]
}

fn exec_say(ctx: &mut Ctx) -> Reply {
    let Some(message) = text_arg(ctx, 0) else {
        return ctx.fail("Usage: /say <message>");
    };
    if !allow_broadcast(ctx) {
        return ctx.fail("You are sending messages too quickly.");
    }
    let message = crate::chatlog::command_text(message);
    let name = ctx.source.name();
    let line = announcement(name, &message);
    crate::log::info(&format!("[say] {name}: {message}"));
    ctx.registry
        .broadcast(&ServerEvent::ChatComponent(Box::new(line)), ctx.world);
    Reply(Vec::new())
}

fn exec_me(ctx: &mut Ctx) -> Reply {
    let Some(action) = text_arg(ctx, 0) else {
        return ctx.fail("Usage: /me <action>");
    };
    if !allow_broadcast(ctx) {
        return ctx.fail("You are sending messages too quickly.");
    }
    let action = crate::chatlog::command_text(action);
    let name = ctx.source.name();
    let line = emote(name, &action);
    crate::log::info(&format!("[me] {name} {action}"));
    ctx.registry
        .broadcast(&ServerEvent::ChatComponent(Box::new(line)), ctx.world);
    Reply(Vec::new())
}

fn exec_msg(ctx: &mut Ctx) -> Reply {
    let Some(targets_arg) = text_arg(ctx, 0) else {
        return ctx.fail("Usage: /msg <targets> <message>");
    };
    let Some(message) = text_arg(ctx, 1) else {
        return ctx.fail("Usage: /msg <targets> <message>");
    };
    if !allow_broadcast(ctx) {
        return ctx.fail("You are sending messages too quickly.");
    }
    let targets = match resolve_targets(ctx.registry, ctx.source, targets_arg) {
        Ok(t) => t,
        Err(e) => return ctx.fail(e),
    };
    let message = crate::chatlog::command_text(message);
    let source_name = ctx.source.name().to_string();
    for target in &targets {
        let line = whisper_incoming(&source_name, &message);
        target.emit(&ServerEvent::ChatComponent(Box::new(line)), ctx.world);
        if let Some(sender) = ctx.source.player() {
            crate::chatlog::log_private(sender, target, &message, ctx.world);
        }
    }
    Reply(
        targets
            .iter()
            .map(|target| lang::format(lang::MESSAGE_OUTGOING, &[&target.name, &message]))
            .collect(),
    )
}

fn exec_tellraw(ctx: &mut Ctx) -> Reply {
    let Some(targets_arg) = text_arg(ctx, 0) else {
        return ctx.fail("Usage: /tellraw <targets> <message>");
    };
    let nbt = match ctx.args.get(1).map(|a| &a.value) {
        Some(Value::Nbt(nbt)) => nbt.clone(),
        Some(Value::Text(text)) => match crate::commands::snbt::parse(text) {
            Ok(nbt) => nbt,
            Err(e) => return ctx.fail(format!("Invalid component: {e}")),
        },
        _ => return ctx.fail("Invalid component"),
    };
    let component = match TextComponent::from_nbt(&nbt) {
        Ok(c) => c,
        Err(e) => return ctx.fail(format!("Invalid component: {e}")),
    };
    if let Err(e) = component.check_limits() {
        return ctx.fail(format!("Invalid component: {e}"));
    }
    let targets = match resolve_targets(ctx.registry, ctx.source, targets_arg) {
        Ok(t) => t,
        Err(e) => return ctx.fail(e),
    };
    crate::log::info(&format!(
        "[tellraw] {} -> {} player(s): {}",
        ctx.source.name(),
        targets.len(),
        clip_for_log(&component.to_json())
    ));
    for target in &targets {
        target.emit(
            &ServerEvent::ChatComponent(Box::new(component.clone())),
            ctx.world,
        );
    }
    Reply(Vec::new())
}

/// `chat.type.announcement` — `[Server] message` for the console, `[name]
/// message` for a player. The name and the message are separate literal
/// components, so neither can introduce formatting or a nested component.
pub(crate) fn announcement(name: &str, message: &str) -> TextComponent {
    TextComponent::translate(
        "chat.type.announcement",
        vec![TextComponent::text(name), TextComponent::text(message)],
    )
}

/// `chat.type.emote` — `* name action`.
pub(crate) fn emote(name: &str, action: &str) -> TextComponent {
    TextComponent::translate(
        "chat.type.emote",
        vec![TextComponent::text(name), TextComponent::text(action)],
    )
}

/// `commands.message.display.outgoing`.
#[allow(dead_code)]
pub(crate) fn whisper_outgoing(target: &str, message: &str) -> TextComponent {
    TextComponent::translate(
        "commands.message.display.outgoing",
        vec![TextComponent::text(target), TextComponent::text(message)],
    )
}

/// `commands.message.display.incoming`.
pub(crate) fn whisper_incoming(source: &str, message: &str) -> TextComponent {
    TextComponent::translate(
        "commands.message.display.incoming",
        vec![TextComponent::text(source), TextComponent::text(message)],
    )
}

/// The plain text of a component, for logs. This is only for the operator log;
/// the wire always carries the rich component.
fn clip_for_log(text: &str) -> String {
    const LOG_LIMIT: usize = 512;
    if text.chars().count() <= LOG_LIMIT {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(LOG_LIMIT).collect();
        out.push('…');
        out
    }
}

fn text_arg<'a>(ctx: &'a Ctx, index: usize) -> Option<&'a str> {
    match ctx.args.get(index).map(|a| &a.value) {
        Some(Value::Text(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// The per-player broadcast limiter. The console and RCON have no client to
/// spam and no per-player session, so they are not limited.
fn allow_broadcast(ctx: &Ctx) -> bool {
    let Some(handle) = ctx.source.player() else {
        return true;
    };
    let rate = crate::chat_config().rate_per_min;
    handle.session().broadcast_limiter.allow(rate)
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

/// The CMD-06b selector source over the live player registry.
struct OnlineSource<'a> {
    registry: &'a SharedRegistry,
    executor: Option<EntityRef>,
}

fn player_ref(handle: &PlayerHandle) -> EntityRef {
    let pos = handle.pos();
    EntityRef {
        id: handle.entity_id,
        uuid: handle.uuid,
        kind: "minecraft:player".into(),
        is_player: true,
        alive: true,
        name: Some(handle.name.clone()),
        pos: Vector3::new(pos.x, pos.y, pos.z),
        rotation: Rotation::new(pos.yaw, pos.pitch),
        tags: Vec::new(),
        team: None,
        gamemode: Some(Gamemode::from(handle.game().mode)),
        level: 0,
        scores: Vec::new(),
        advancements: Vec::new(),
        predicates: Vec::new(),
        nbt: crate::protocol::nbt::Nbt::Compound(Vec::new()),
    }
}

impl SelectorSource for OnlineSource<'_> {
    fn executor(&self) -> Option<EntityRef> {
        self.executor.clone()
    }

    fn players(&self) -> Vec<EntityRef> {
        self.registry
            .snapshot()
            .iter()
            .map(|handle| player_ref(handle))
            .collect()
    }
}

fn online_source<'a>(registry: &'a SharedRegistry, source: Source<'a>) -> OnlineSource<'a> {
    OnlineSource {
        registry,
        executor: source.player().map(player_ref),
    }
}

/// Resolve a `targets` argument: a CMD-06b player selector, or one plain
/// player name. Only online players are eligible, as in vanilla; an unknown or
/// non-player target is `No player was found` (or the selector's own wording
/// when it can match a mob).
pub(crate) fn resolve_targets(
    registry: &SharedRegistry,
    source: Source<'_>,
    text: &str,
) -> Result<Vec<std::sync::Arc<PlayerHandle>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("No player was found".into());
    }
    if text.starts_with('@') {
        let selector = Selector::parse_players(text).map_err(|e| e.message())?;
        let source = online_source(registry, source);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let found = selector
            .select(&source, &mut LcgRng::new(seed))
            .map_err(|e| e.message())?;
        let mut out = Vec::with_capacity(found.len());
        for entity in found {
            let handle = registry
                .by_uuid(entity.uuid)
                .or_else(|| {
                    let name = entity.name.clone().unwrap_or_default();
                    registry
                        .snapshot()
                        .into_iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&name))
                })
                .ok_or_else(|| "No player was found".to_string())?;
            out.push(handle);
        }
        if out.is_empty() {
            return Err("No player was found".into());
        }
        return Ok(out);
    }
    registry
        .snapshot()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(text))
        .map(|handle| vec![handle])
        .ok_or_else(|| "No player was found".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::{PlayerHandle, Registry};
    use crate::protocol::text::Content;
    use crate::session::DemoWorld;
    use std::sync::Arc;

    fn registry_with(names: &[(&str, i32)]) -> (SharedRegistry, Vec<Arc<PlayerHandle>>) {
        let registry = Arc::new(Registry::default());
        let mut handles = Vec::new();
        for (name, id) in names {
            let handle = PlayerHandle::for_test(*id, name);
            registry.insert_for_test(Arc::clone(&handle));
            handles.push(handle);
        }
        (registry, handles)
    }

    fn names(targets: &[Arc<PlayerHandle>]) -> Vec<String> {
        targets.iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn a_named_target_and_a_selector_resolve_to_the_same_player() {
        let (registry, handles) = registry_with(&[("Alice", 1), ("Bob", 2)]);
        let source = Source::Player(&handles[0]);
        let by_name = resolve_targets(&registry, source, "Bob").unwrap();
        assert_eq!(names(&by_name), vec!["Bob"]);

        let mut all = names(&resolve_targets(&registry, source, "@a").unwrap());
        all.sort();
        assert_eq!(all, vec!["Alice", "Bob"]);
    }

    #[test]
    fn an_unknown_or_non_player_target_is_no_player_was_found() {
        let (registry, handles) = registry_with(&[("Alice", 1)]);
        let source = Source::Player(&handles[0]);
        assert_eq!(
            resolve_targets(&registry, source, "Nobody")
                .err()
                .as_deref(),
            Some("No player was found")
        );
        // A selector that can include a mob is refused by the player-only
        // parse, not silently filtered.
        assert!(resolve_targets(&registry, source, "@e").is_err());
    }

    #[test]
    fn the_vanilla_message_components_carry_the_exact_keys() {
        assert_eq!(
            announcement("Server", "hello").content,
            Content::Translate {
                key: "chat.type.announcement".into(),
                fallback: None,
                with: vec![TextComponent::text("Server"), TextComponent::text("hello")],
            }
        );
        assert_eq!(
            emote("Alice", "waves").content,
            Content::Translate {
                key: "chat.type.emote".into(),
                fallback: None,
                with: vec![TextComponent::text("Alice"), TextComponent::text("waves")],
            }
        );
        assert_eq!(
            whisper_outgoing("Bob", "hi").content,
            Content::Translate {
                key: "commands.message.display.outgoing".into(),
                fallback: None,
                with: vec![TextComponent::text("Bob"), TextComponent::text("hi")],
            }
        );
        assert_eq!(
            whisper_incoming("Alice", "hi").content,
            Content::Translate {
                key: "commands.message.display.incoming".into(),
                fallback: None,
                with: vec![TextComponent::text("Alice"), TextComponent::text("hi")],
            }
        );
    }

    #[test]
    fn names_are_literals_not_reparsed_components() {
        // A hostile name is a literal, so it cannot add a hover/click or a
        // translation. (Login validation already refuses most of this; the
        // command layer must not be the weak link.)
        let hostile = r#"<Click me>"#;
        let line = emote(hostile, "hi");
        let json = line.to_json();
        assert!(json.contains(r#""text":"<Click me>""#));
        assert!(!json.contains("clickEvent"));
    }

    #[test]
    fn message_text_is_sanitized_and_bounded() {
        assert_eq!(crate::chatlog::command_text("a\nb\u{00a7}c"), "a b c");
        let long = "x".repeat(crate::chatlog::MAX_COMMAND_TEXT + 10);
        assert_eq!(
            crate::chatlog::command_text(&long).chars().count(),
            crate::chatlog::MAX_COMMAND_TEXT + 1,
            "clipped with the ellipsis"
        );
    }

    #[test]
    fn say_and_tellraw_are_op_level_two_and_me_is_public() {
        let t = tree::tree();
        let say = t.find_command("say").unwrap();
        assert!(matches!(
            say.requirement,
            Requirement::Level {
                op: 2,
                node: "minecraft.command.say"
            }
        ));
        let tellraw = t.find_command("tellraw").unwrap();
        assert!(matches!(
            tellraw.requirement,
            Requirement::Level {
                op: 2,
                node: "minecraft.command.tellraw"
            }
        ));
        assert!(matches!(
            t.find_command("me").unwrap().requirement,
            Requirement::None
        ));
        assert!(matches!(
            t.find_command("msg").unwrap().requirement,
            Requirement::None
        ));
        // The aliases address the same node.
        assert_eq!(t.find_command("tell").map(|n| n.name), Some("msg"));
        assert_eq!(t.find_command("w").map(|n| n.name), Some("msg"));
    }

    /// A throwaway world on a temp store, unique per call.
    fn test_world() -> DemoWorld {
        use aether_api::{FjallStore, World};
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let tag = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aether-text-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", tag).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, tag, revision, &cfg);
        World::new(store, generator)
    }

    fn run(
        world: &DemoWorld,
        registry: &SharedRegistry,
        source: Source<'_>,
        line: &str,
    ) -> Vec<String> {
        tree::run_command(line, source, registry, world).0
    }

    #[test]
    fn msg_prints_the_vanilla_outgoing_line_and_refuses_unknown_targets() {
        let world = test_world();
        let (registry, handles) = registry_with(&[("Alice", 1), ("Bob", 2)]);
        let source = Source::Player(&handles[0]);
        assert_eq!(
            run(&world, &registry, source, "msg Bob hello there"),
            vec!["You whisper to Bob: hello there"]
        );
        // A selector fans out to one outgoing line per target.
        let mut all = run(&world, &registry, source, "msg @a hi");
        all.sort();
        assert_eq!(
            all,
            vec!["You whisper to Alice: hi", "You whisper to Bob: hi"]
        );
        // Vanilla allows messaging yourself: one outgoing line, and the
        // incoming side is delivered to the same client.
        assert_eq!(
            run(&world, &registry, source, "msg Alice hi"),
            vec!["You whisper to Alice: hi"]
        );
        assert_eq!(
            run(&world, &registry, source, "msg Nobody hi"),
            vec!["No player was found"]
        );
    }

    #[test]
    fn say_me_and_tellraw_render_silently() {
        let world = test_world();
        let (registry, handles) = registry_with(&[("Alice", 1)]);
        // `/say` and `/tellraw` are op level 2, so the console (an owner)
        // drives them here; `/me` is public and runs as the player.
        let console = Source::Server(tree::ServerKind::Console);
        assert!(run(&world, &registry, console, "say hello world").is_empty());
        assert!(run(&world, &registry, Source::Player(&handles[0]), "me waves").is_empty());
        assert!(run(
            &world,
            &registry,
            console,
            r#"tellraw @a {text:"hi",color:"red",hoverEvent:{action:"show_text",value:"tip"}}"#,
        )
        .is_empty());
    }

    #[test]
    fn an_invalid_tellraw_component_is_an_error_and_nothing_is_sent() {
        let world = test_world();
        let (registry, _handles) = registry_with(&[("Alice", 1)]);
        let source = Source::Server(tree::ServerKind::Console);
        let out = run(&world, &registry, source, r#"tellraw @a {text:1}"#);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("Invalid component:"), "{out:?}");
    }

    #[test]
    fn console_say_uses_the_server_name() {
        let world = test_world();
        let (registry, _handles) = registry_with(&[("Alice", 1)]);
        let source = Source::Server(tree::ServerKind::Console);
        assert!(run(&world, &registry, source, "say hi").is_empty());
        assert!(run(&world, &registry, source, "me waves").is_empty());
        // The console is an owner, so both op-2 verbs are reachable.
        assert!(run(&world, &registry, source, r#"tellraw @a {text:"hi"}"#,).is_empty());
    }

    #[test]
    fn long_and_nested_components_survive_parsing() {
        let long = "x".repeat(10_000);
        let text =
            format!(r#"{{text:"{long}",extra:[{{text:"a",bold:true,extra:[{{text:"b"}}]}}]}}"#);
        let nbt = crate::commands::snbt::parse(&text).unwrap();
        let c = TextComponent::from_nbt(&nbt).unwrap();
        assert_eq!(c.extra.len(), 1);
        assert!(c.to_json().contains(&long));
    }
}
