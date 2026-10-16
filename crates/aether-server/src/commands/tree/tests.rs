//! The command tree's own tests: the reader, every argument kind, the edge
//! cases the brief calls out, and one migration test per ported command run
//! through [`execute`].

use super::*;
use crate::players::{PosLook, Registry, SharedRegistry};
use crate::proto::Conn;
use crate::protocol::codec_for;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A handle in a registry that is returned too, so callers can look players up.
fn join(registry: &SharedRegistry, name: &str, entity_id: i32) -> Arc<PlayerHandle> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
    let _peer = listener.accept().unwrap();
    registry
        .join(
            entity_id,
            entity_id as u128,
            name.into(),
            None,
            Vec::new(),
            codec_for(763).unwrap(),
            &client,
            PosLook {
                x: 0.5,
                y: 64.0,
                z: 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
        )
        .unwrap()
}

/// A handle for parse/suggest tests that need no registry lookups.
fn bare_handle(name: &str) -> Arc<PlayerHandle> {
    join(&Arc::new(Registry::default()), name, 1)
}

/// Install the operator list.
///
/// Since CMD-01 the operator bit is not read from `config.operators` at check
/// time: the permission store is seeded from it and is authoritative
/// afterwards. So this sets `CONFIG` *and* seeds the store the way the server
/// does at startup ([`crate::permissions::seed_operators`]), or `tester` would
/// stay level 0 and every `Requirement::Level` would deny them. The seed is
/// repeated on every call because another test (`permissions::install`) can
/// replace the whole process-wide store; the caller holds
/// [`crate::permissions::test_lock`] so that replacement cannot race this.
fn install_config() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let cfg: crate::config::Config = toml::from_str("operators = [\"tester\"]\n").unwrap();
        let _ = crate::CONFIG.set(Arc::new(cfg));
    });
    crate::permissions::seed_operators(&["tester".to_string()]);
}

/// A throwaway world on a temp store, unique per call so tests can run in
/// parallel.
fn test_world() -> DemoWorld {
    use aether_api::{FjallStore, World};
    static N: AtomicU64 = AtomicU64::new(0);
    let tag = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("aether-cmd03-{}-{tag}", std::process::id()));
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

struct Env {
    // Held for the whole test so no other test can replace the permission
    // store while this one depends on `tester` being an operator.
    _guard: std::sync::MutexGuard<'static, ()>,
    world: DemoWorld,
    registry: SharedRegistry,
    tester: Arc<PlayerHandle>,
    bob: Arc<PlayerHandle>,
}

fn env() -> Env {
    let _guard = crate::permissions::test_lock();
    install_config();
    static IDS: AtomicI32 = AtomicI32::new(10);
    let world = test_world();
    let registry: SharedRegistry = Arc::new(Registry::default());
    let tester = join(&registry, "tester", IDS.fetch_add(2, Ordering::Relaxed));
    let bob = join(&registry, "bob", IDS.fetch_add(2, Ordering::Relaxed));
    Env {
        _guard,
        world,
        registry,
        tester,
        bob,
    }
}

/// Run a command through the one dispatcher and return its lines.
fn run(env: &Env, who: &PlayerHandle, text: &str) -> Vec<String> {
    execute(text, who, &env.registry, &env.world)
        .unwrap_or_else(|| panic!("`{text}` was not treated as a command"))
        .0
}

fn one(env: &Env, who: &PlayerHandle, text: &str) -> String {
    let lines = run(env, who, text);
    assert_eq!(lines.len(), 1, "`{text}` => {lines:?}");
    lines.into_iter().next().unwrap()
}

/// The parse error for `text`, or a panic if it parsed.
fn parse_err(tree: &CommandTree, text: &str, handle: &PlayerHandle) -> ParseError {
    match tree.parse(text, handle) {
        Ok(_) => panic!("`{text}` parsed but should not have"),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// Reader and argument kinds
// ---------------------------------------------------------------------------

#[test]
fn the_reader_reads_words_quotes_and_greedy_remainders() {
    let mut r = StringReader::new("alpha beta");
    assert_eq!(r.read_token(), "alpha");
    r.skip_whitespace();
    assert_eq!(r.read_token(), "beta");
    assert!(!r.can_read());

    // Any whitespace separates, not just a space.
    let mut r = StringReader::new("alpha\tbeta\ngamma");
    assert_eq!(r.read_token(), "alpha");
    r.skip_whitespace();
    assert_eq!(r.read_token(), "beta");
    r.skip_whitespace();
    assert_eq!(r.read_token(), "gamma");

    let mut r = StringReader::new("\"hello world\" /tail");
    assert_eq!(r.read_remainder().unwrap(), "hello world /tail");

    // A string argument honours quotes; a token does not.
    let mut r = StringReader::new("\"a b\" c");
    assert_eq!(r.read_string_arg().unwrap(), "a b");
    let mut r = StringReader::new("\"a\tb\"");
    assert_eq!(r.read_token(), "\"a");
}

#[test]
fn an_unterminated_quote_is_an_error_not_an_empty_argument() {
    let mut r = StringReader::new("\"unterminated");
    assert_eq!(r.read_string_arg().unwrap_err(), ParseError::InvalidQuoted);
    let mut r = StringReader::new("\"unterminated tail");
    assert_eq!(r.read_remainder().unwrap_err(), ParseError::InvalidQuoted);
    assert_eq!(ParseError::InvalidQuoted.message(), "Invalid quoted string");
}

#[test]
fn every_argument_kind_parses_a_valid_word() {
    let kinds: &[ArgKind] = &[
        ArgKind::String,
        ArgKind::Quotable,
        ArgKind::GreedyString,
        ArgKind::Message,
        ArgKind::Integer,
        ArgKind::Long,
        ArgKind::Float,
        ArgKind::Double,
        ArgKind::Bool,
        ArgKind::Enum(&["a", "b"]),
        ArgKind::Entity,
        ArgKind::Entities,
        ArgKind::BlockPos,
        ArgKind::Vec3,
        ArgKind::Vec2,
        ArgKind::BlockState,
        ArgKind::ItemStack,
        ArgKind::NbtCompound,
        ArgKind::NbtPath,
        ArgKind::Component,
        ArgKind::ResourceLocation,
        ArgKind::ResourceOrTag,
        ArgKind::GameProfile,
        ArgKind::Time,
        ArgKind::Gamemode,
        ArgKind::Difficulty,
        ArgKind::Particle,
        ArgKind::ScoreHolder,
        ArgKind::Swizzle,
        ArgKind::Angle,
        ArgKind::Operation,
        ArgKind::Rotation,
        ArgKind::Uuid,
    ];
    for &kind in kinds {
        let text = match kind {
            ArgKind::Integer | ArgKind::Long | ArgKind::Float | ArgKind::Double => "1",
            ArgKind::Bool => "true",
            ArgKind::Enum(_) => "a",
            _ => "word",
        };
        let mut r = StringReader::new(text);
        let out = kind.parse(&mut r);
        assert!(out.is_ok(), "{kind:?} rejected `{text}`: {out:?}");
        let _ = kind.parser();
    }
}

#[test]
fn the_by_id_parser_indices_match_the_vanilla_registry() {
    // Vanilla's order is not the order the kinds are written in: bool=0,
    // float=1, double=2, integer=3, long=4, string=5. Getting these wrong is
    // invisible until an integer-typed node reaches a client.
    assert_eq!(ArgKind::Bool.parser().id, 0);
    assert_eq!(ArgKind::Float.parser().id, 1);
    assert_eq!(ArgKind::Double.parser().id, 2);
    assert_eq!(ArgKind::Integer.parser().id, 3);
    assert_eq!(ArgKind::Long.parser().id, 4);
    assert_eq!(ArgKind::String.parser().id, 5);
    assert_eq!(ArgKind::GreedyString.parser().id, 5);
    // The string modes: word=0, quotable=1, greedy=2.
    assert_eq!(ArgKind::String.parser().properties, 0);
    assert_eq!(ArgKind::Quotable.parser().properties, 1);
    assert_eq!(ArgKind::GreedyString.parser().properties, 2);
    // The by-name spelling stays the pre-1.19 identifier.
    assert_eq!(ArgKind::Integer.parser().name, "brigadier:integer");
}

#[test]
fn integer_types_parse_and_reject_with_vanilla_text() {
    let mut r = StringReader::new("42");
    assert_eq!(ArgKind::Integer.parse(&mut r).unwrap().0, Value::Int(42));

    // `2147483648` is one past i32::MAX: overflow, not a panic.
    let mut r = StringReader::new("2147483648");
    assert_eq!(
        ArgKind::Integer.parse(&mut r).unwrap_err(),
        ParseError::InvalidInteger("2147483648".into())
    );
    assert_eq!(
        ParseError::InvalidInteger("x".into()).message(),
        "Invalid integer 'x'"
    );

    let mut r = StringReader::new("999999999999999999999");
    assert!(matches!(
        ArgKind::Long.parse(&mut r),
        Err(ParseError::InvalidLong(_))
    ));

    let mut r = StringReader::new("nope");
    assert_eq!(
        ArgKind::Double.parse(&mut r).unwrap_err(),
        ParseError::InvalidDouble("nope".into())
    );
    let mut r = StringReader::new("true");
    assert_eq!(ArgKind::Bool.parse(&mut r).unwrap().0, Value::Bool(true));
    let mut r = StringReader::new("yes");
    assert_eq!(
        ArgKind::Bool.parse(&mut r).unwrap_err(),
        ParseError::InvalidBool("yes".into())
    );
}

/// A small synthetic tree with one literal per typed-argument shape.
fn sample_tree() -> CommandTree {
    let none = Requirement::None;
    let noop = |_ctx: &mut Ctx| Reply(Vec::new());
    // The literal carries no executor: only the argument node may end the
    // grammar, so a missing argument is "unknown or incomplete".
    let num = Builder::literal("num").then(Builder::arg("n", ArgKind::Integer).executable(noop));
    let mode = Builder::literal("mode")
        .then(Builder::arg("m", ArgKind::Enum(&["survival", "creative"])).executable(noop));
    let say = Builder::literal("say").then(Builder::arg("msg", ArgKind::Message).executable(noop));
    let flag = Builder::literal("flag").then(Builder::arg("b", ArgKind::Bool).executable(noop));
    let root = Node {
        name: "",
        aliases: Vec::new(),
        kind: Kind::Root,
        executable: false,
        requirement: none,
        executor: None,
        children: vec![num.build(), mode.build(), say.build(), flag.build()],
    };
    CommandTree { root }
}

#[test]
fn typed_arguments_flow_through_the_parser() {
    let tree = sample_tree();
    let handle = bare_handle("bob");
    let m = tree.parse("num 42", &handle).unwrap();
    assert_eq!(m.args.len(), 1);
    assert_eq!(m.args[0].value, Value::Int(42));
    assert_eq!(m.args[0].text, "42");

    assert_eq!(
        parse_err(&tree, "num 2147483648", &handle),
        ParseError::InvalidInteger("2147483648".into())
    );
    assert!(tree.parse("mode survival", &handle).is_ok());
    assert_eq!(
        parse_err(&tree, "mode sideways", &handle),
        ParseError::IncorrectArg
    );
    let m = tree.parse("say \"hello world\"", &handle).unwrap();
    assert_eq!(m.args[0].value, Value::Text("hello world".into()));
    assert_eq!(
        tree.parse("flag true", &handle).unwrap().args[0].value,
        Value::Bool(true)
    );
}

#[test]
fn unknown_literal_and_trailing_input_use_the_vanilla_errors() {
    let tree = sample_tree();
    let handle = bare_handle("bob");
    assert_eq!(parse_err(&tree, "nothing", &handle), ParseError::Incomplete);
    assert_eq!(parse_err(&tree, "num", &handle), ParseError::Incomplete);
    // `/say` takes a greedy remainder, so this one succeeds...
    assert!(tree.parse("say a b c", &handle).is_ok());
    // ...but a bare literal with no argument does not accept trailing words.
    let bare = CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            children: vec![Builder::literal("toggle")
                .executable(|_| Reply(Vec::new()))
                .build()],
        },
    };
    assert_eq!(
        parse_err(&bare, "toggle now", &handle),
        ParseError::IncorrectArg
    );
    assert_eq!(
        ParseError::Incomplete.message(),
        "Unknown or incomplete command"
    );
    assert_eq!(
        ParseError::IncorrectArg.message(),
        "Incorrect argument for command"
    );
}

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

fn always_hidden(_h: &PlayerHandle) -> Result<(), Deny> {
    Err(Deny::Hidden)
}

fn always_denied(_h: &PlayerHandle) -> Result<(), Deny> {
    Err(Deny::Message("nope".into()))
}

fn requirement_tree(req: Requirement) -> CommandTree {
    CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            children: vec![Builder::literal("secret")
                .requires(req)
                .executable(|_| Reply(vec!["ok".into()]))
                .build()],
        },
    }
}

#[test]
fn a_hidden_node_reads_as_unknown_and_is_not_run() {
    let tree = requirement_tree(Requirement::Fn(always_hidden));
    let handle = bare_handle("bob");
    assert_eq!(parse_err(&tree, "secret", &handle), ParseError::Incomplete);
}

#[test]
fn a_denied_node_carries_its_message() {
    let tree = requirement_tree(Requirement::Fn(always_denied));
    let handle = bare_handle("bob");
    assert_eq!(
        parse_err(&tree, "secret", &handle),
        ParseError::Denied("nope".into())
    );
}

#[test]
fn the_level_requirement_matches_ops_against_the_registry() {
    let _guard = crate::permissions::test_lock();
    install_config();
    let tree = requirement_tree(Requirement::Level {
        op: 1,
        node: "minecraft.command.secret",
    });
    let op = bare_handle("tester");
    let bob = bare_handle("bob");
    assert!(tree.parse("secret", &op).is_ok());
    assert_eq!(
        parse_err(&tree, "secret", &bob),
        ParseError::Denied("You do not have permission to run this command.".into())
    );
}

// ---------------------------------------------------------------------------
// Suggestions
// ---------------------------------------------------------------------------

fn suggestion_tree() -> CommandTree {
    let noop = |_ctx: &mut Ctx| Reply(Vec::new());
    let mut mode = Builder::literal("mode").executable(noop);
    mode = mode.then(Builder::arg("m", ArgKind::Enum(&["survival", "creative"])).executable(noop));
    CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            children: vec![
                Builder::literal("time").executable(noop).build(),
                Builder::literal("tp").executable(noop).build(),
                Builder::literal("teleport").executable(noop).build(),
                Builder::literal("tell").executable(noop).build(),
                mode.build(),
            ],
        },
    }
}

#[test]
fn suggestions_filter_literals_by_prefix_and_never_run_a_command() {
    install_config();
    let tree = suggestion_tree();
    let registry: SharedRegistry = Arc::new(Registry::default());
    let handle = join(&registry, "bob", 1);
    let ctx = SuggestCtx {
        registry: &registry,
        world: None,
    };
    // `/t` is ambiguous: every `t…` command is offered, and nothing runs.
    assert_eq!(
        suggest_in(&tree, "/t", &handle, &ctx),
        vec!["teleport", "tell", "time", "tp"]
    );
    assert_eq!(suggest_in(&tree, "/mo", &handle, &ctx), vec!["mode"]);
    assert_eq!(
        suggest_in(&tree, "/mode ", &handle, &ctx),
        vec!["creative", "survival"]
    );
    assert_eq!(
        suggest_in(&tree, "/mode s", &handle, &ctx),
        vec!["survival"]
    );
}

#[test]
fn the_public_suggest_uses_the_global_tree() {
    let _guard = crate::permissions::test_lock();
    install_config();
    let registry: SharedRegistry = Arc::new(Registry::default());
    // An operator, so the permission-gated gameplay names complete.
    let handle = join(&registry, "tester", 1);
    let ctx = SuggestCtx {
        registry: &registry,
        world: None,
    };
    // The real tree: `/gamemode` and its alias, `/give`.
    assert!(suggest("/ga", &handle, &ctx).contains(&"gamemode".to_string()));
    assert!(suggest("/giv", &handle, &ctx).contains(&"give".to_string()));
    // An exact alias match is a complete command, so only its arguments
    // remain to complete — here, none.
    assert!(suggest("/gm", &handle, &ctx).is_empty());
    // `/t` suggests several real commands and runs nothing.
    let t = suggest("/t", &handle, &ctx);
    assert!(t.contains(&"time".to_string()) && t.contains(&"tp".to_string()));
    // A non-op does not even see `/gamemode`.
    let bob = join(&registry, "bob", 2);
    assert!(!suggest("/ga", &bob, &ctx).contains(&"gamemode".to_string()));
}

// ---------------------------------------------------------------------------
// The built tree
// ---------------------------------------------------------------------------

fn count_nodes(node: &Node) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

#[test]
fn the_tree_is_built_once() {
    let a = tree() as *const CommandTree;
    let b = tree() as *const CommandTree;
    assert_eq!(a, b, "the tree must be built once");
}

#[test]
fn a_two_hundred_node_tree_parses() {
    // The brief's stress case: a wide tree still resolves one command.
    let noop = |_ctx: &mut Ctx| Reply(Vec::new());
    let mut children = Vec::new();
    for i in 0..250 {
        let name: &'static str = Box::leak(format!("cmd{i}").into_boxed_str());
        children.push(
            Builder::literal(name)
                .executable(noop)
                .then(Builder::arg("n", ArgKind::Integer).executable(noop))
                .build(),
        );
    }
    let tree = CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            children,
        },
    };
    assert!(count_nodes(&tree.root) >= 500);
    let handle = bare_handle("bob");
    assert!(tree.parse("cmd123 7", &handle).is_ok());
    assert_eq!(
        tree.parse("cmd123 7", &handle).unwrap().args[0].value,
        Value::Int(7)
    );
}

#[test]
fn every_command_node_has_a_unique_name_or_alias() {
    let mut seen: Vec<&str> = Vec::new();
    for command in tree().commands() {
        for name in std::iter::once(command.name).chain(command.aliases.iter().copied()) {
            assert!(!seen.contains(&name), "`{name}` is declared twice");
            seen.push(name);
        }
    }
}

// ---------------------------------------------------------------------------
// Migration: every command the server answered before still answers through
// the one tree.
// ---------------------------------------------------------------------------

#[test]
fn history_commands_route_through_the_tree() {
    let env = env();
    let who = &env.tester;
    assert_eq!(
        one(&env, who, "/inspect"),
        "Inspect mode ON — hit or right-click a block to read its history. /inspect again to stop."
    );
    assert_eq!(one(&env, who, "/inspect"), "Inspect mode OFF.");
    assert_eq!(
        one(&env, who, "/page"),
        "nothing to page through — run /lookup or /inspect first"
    );
    // `/lookup` with no filter still parks an answer for `/page`.
    let _ = run(&env, who, "/lookup");
    assert!(!run(&env, who, "/page").is_empty());
    assert_eq!(
        one(&env, who, "/inv"),
        "the server has you carrying nothing"
    );
    assert_eq!(
        one(&env, who, "/stash"),
        "your stash is empty — nothing has been rolled back"
    );
    assert!(one(&env, who, "/audit").contains("item instance"));
    assert!(one(&env, who, "/cache").starts_with("column cache:"));
    // Aliases route to the same nodes.
    assert_eq!(
        one(&env, who, "/i"),
        "Inspect mode ON — hit or right-click a block to read its history. /inspect again to stop."
    );
    assert!(one(&env, who, "/l").contains("Lookup") || run(&env, who, "/l").is_empty());
    let _ = run(&env, who, "/pg");
    // `/inventory` is an alias of `/inv`.
    assert_eq!(
        one(&env, who, "/inventory"),
        "the server has you carrying nothing"
    );
    // `/rollback` with no filter refuses, and `/rb` routes to the same node.
    let refusal = vec![
        "refusing to roll back the entire history with no filter.".to_string(),
        "add at least one: player:<name>, time:<duration>, radius:<n>.".to_string(),
        "to really undo everything, use  /rollback time:9999d".to_string(),
    ];
    assert_eq!(run(&env, who, "/rollback"), refusal);
    assert_eq!(run(&env, who, "/rb"), refusal);
}

#[test]
fn game_commands_gate_on_operator_through_the_tree() {
    let env = env();
    // A non-op is refused by the tree, with the historical message.
    assert_eq!(
        one(&env, &env.bob, "/gamemode creative"),
        "You are not an operator."
    );
    assert_eq!(
        one(&env, &env.bob, "/give stone 1"),
        "You are not an operator."
    );
    assert_eq!(
        one(&env, &env.bob, "/time set day"),
        "You are not an operator."
    );

    // An op runs them.
    assert_eq!(
        one(&env, &env.tester, "/gamemode creative"),
        "tester is now in Creative mode"
    );
    assert_eq!(
        one(&env, &env.tester, "/gm s"),
        "tester is now in Survival mode"
    );
    assert_eq!(one(&env, &env.tester, "/time set day"), "Time set to 1000");
    assert!(one(&env, &env.tester, "/time").starts_with("It is "));
    assert_eq!(
        one(&env, &env.tester, "/give stone 3"),
        "Gave 3 minecraft:stone"
    );
    assert!(one(&env, &env.tester, "/summon pig").contains("pig"));
    assert!(one(&env, &env.tester, "/killall").starts_with("Removed "));
}

#[test]
fn self_and_world_commands_route_through_the_tree() {
    let env = env();
    let who = &env.tester;
    assert!(run(&env, who, "/kill").is_empty());
    assert_eq!(one(&env, who, "/spawn"), "Teleported to spawn");
    assert_eq!(one(&env, who, "/tp 1 2 3"), "Teleported to 1 2 3");
    assert_eq!(one(&env, who, "/tp bob"), "Teleported to bob");
    assert!(one(&env, who, "/block 0 64 0").contains(':'));
    assert_eq!(one(&env, who, "/food"), "Food set to 20");
    assert_eq!(one(&env, who, "/heal"), "Healed");
    // `/visibility` toggles and reads back.
    assert_eq!(one(&env, who, "/visibility"), "Visibility culling is on");
    assert_eq!(
        one(&env, who, "/visibility off"),
        "Visibility culling off: you are sent every section"
    );
    assert_eq!(one(&env, who, "/visibility on"), "Visibility culling on");
    // `/visibility status` reports the effective anti-xray state for the
    // player (SP1); the wording depends on the server config, so assert only
    // that it is about anti-xray and never a toggle acknowledgement.
    let status = one(&env, who, "/visibility status");
    assert!(status.contains("anti-xray"), "{status}");
    assert!(!status.starts_with("Visibility culling"), "{status}");
}

#[test]
fn economy_commands_are_unchanged_while_the_economy_is_disabled() {
    let env = env();
    let who = &env.tester;
    let disabled = vec![
        "the economy is not enabled on this server.".to_string(),
        "it needs a [database] url and a build with --features postgres.".to_string(),
    ];
    for text in [
        "/balance",
        "/bal",
        "/money",
        "/listings",
        "/pay bob 5",
        "/econ",
        "/sell 1 5",
        "/market",
        "/buy 1",
        "/unlist 1",
    ] {
        assert_eq!(run(&env, who, text), disabled, "`{text}`");
    }
    // `/econ` is permission-gated for a non-op only once an economy exists —
    // while it is disabled everyone sees the disabled reply, as before.
    assert_eq!(run(&env, &env.bob, "/econ"), disabled);
}

#[test]
fn non_commands_and_unknown_commands_are_handled_by_the_tree() {
    let env = env();
    assert!(execute("hello world", &env.tester, &env.registry, &env.world).is_none());
    assert_eq!(
        one(&env, &env.tester, "/definitely-not-a-command"),
        "Unknown or incomplete command"
    );
    // A known command name with a bad argument gets the vanilla error.
    assert_eq!(
        one(&env, &env.tester, "/inspect extra"),
        "Incorrect argument for command"
    );
    // An unterminated quote is a syntax error, not a silently empty argument.
    assert_eq!(
        one(&env, &env.tester, "/lookup \"unterminated"),
        "Invalid quoted string"
    );
}

#[test]
fn help_still_explains_a_single_command() {
    let env = env();
    // `/help <command>` works again: the node is greedy, so an argument is
    // accepted rather than rejected as "incorrect argument".
    assert_eq!(
        one(&env, &env.tester, "/help stop"),
        "/stop — minecraft.command.stop (op level 4), planned in CMD-09"
    );
    assert!(
        run(&env, &env.tester, "/help")
            .iter()
            .any(|l| l.contains("not implemented yet")),
        "/help with no argument lists the stubs"
    );
    // The `?` alias routes to the same node.
    assert_eq!(
        one(&env, &env.tester, "/? stop"),
        "/stop — minecraft.command.stop (op level 4), planned in CMD-09"
    );
}

#[test]
fn the_stub_registry_is_on_the_tree() {
    let env = env();
    // A specified-but-unimplemented command answers with its task id, and its
    // op level is enforced by the tree.
    assert_eq!(
        one(&env, &env.tester, "/stop"),
        "/stop is not implemented yet (CMD-09)"
    );
    assert_eq!(
        one(&env, &env.bob, "/stop"),
        "You do not have permission to run this command."
    );
    assert_eq!(
        one(&env, &env.tester, "/xp"),
        "/experience is not implemented yet (CMD-10)"
    );
}

// ---------------------------------------------------------------------------
// The one-dispatcher guarantee
// ---------------------------------------------------------------------------

#[test]
fn no_second_dispatcher_remains() {
    let game = include_str!("../../game/commands.rs");
    let economy = include_str!("../../economy/commands.rs");
    let history = include_str!("../mod.rs");
    for (name, src) in [("game", game), ("economy", economy)] {
        assert!(
            !src.contains("match cmd") && !src.contains("match command"),
            "{name}/commands.rs still matches on the command name"
        );
        assert!(
            !src.contains("fn dispatch("),
            "{name}/commands.rs still has a dispatcher"
        );
    }
    assert!(
        !history.contains("match cmd") && !history.contains("match command"),
        "commands/mod.rs still matches on the command name"
    );
    assert!(
        history.contains("tree::execute"),
        "commands/mod.rs must dispatch through the tree"
    );
}
