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

/// Serialise the tests that mutate the process-wide world border.
fn border_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
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

/// Every `/help` line the caller can see, walking the whole pager.
fn all_help(env: &Env, who: &PlayerHandle) -> Vec<String> {
    let mut lines = run(env, who, "/help");
    let pages: usize = lines[0]
        .split("page 1 of ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("a /help header names its page count");
    for _ in 1..pages {
        lines.extend(run(env, who, "/page next"));
    }
    lines
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
        ArgKind::IntegerRange {
            min: Some(0),
            max: Some(10),
        },
        ArgKind::Long,
        ArgKind::Float,
        ArgKind::Double,
        ArgKind::Bool,
        ArgKind::Enum(&["a", "b"]),
        ArgKind::Entity,
        ArgKind::Entities,
        ArgKind::Players,
        ArgKind::BlockPos,
        ArgKind::Vec3(true),
        ArgKind::Vec2(true),
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
        ArgKind::Team,
        ArgKind::Color,
        ArgKind::Swizzle,
        ArgKind::Angle,
        ArgKind::Operation,
        ArgKind::Rotation,
        ArgKind::Uuid,
    ];
    for &kind in kinds {
        let text = match kind {
            ArgKind::Integer
            | ArgKind::IntegerRange { .. }
            | ArgKind::Long
            | ArgKind::Float
            | ArgKind::Double => "1",
            ArgKind::Bool => "true",
            ArgKind::Enum(_) => "a",
            ArgKind::BlockPos | ArgKind::Vec3(_) => "~0 ~ ~",
            ArgKind::Vec2(_) => "1 2",
            ArgKind::Rotation => "~ ~",
            ArgKind::Swizzle => "xyz",
            ArgKind::Angle => "~90",
            ArgKind::BlockState => "minecraft:stone",
            ArgKind::ItemStack => "minecraft:stone",
            ArgKind::NbtCompound => "{a:1}",
            ArgKind::Component => "\"hi\"",
            ArgKind::Time => "1s",
            ArgKind::Gamemode => "creative",
            ArgKind::Difficulty => "normal",
            ArgKind::Operation => "+=",
            ArgKind::Color => "red",
            ArgKind::Particle => "minecraft:flame",
            _ => "word",
        };
        let mut r = StringReader::new(text);
        let out = kind.parse(&mut r);
        assert!(out.is_ok(), "{kind:?} rejected `{text}`: {out:?}");
        let _ = kind.parser_name();
        let _ = kind.registry_name();
        let _ = kind.properties();
    }
}

#[test]
fn the_wire_parser_and_properties_match_each_kind() {
    // The by-id indices themselves are checked against `minecraft-data` in
    // `protocol::commands`, where the per-release registries live. Here the
    // tree's own half: the pre-1.19 identifier, the property shape, and that
    // every argument kind names a parser at all.
    assert_eq!(ArgKind::Bool.parser_name(), "brigadier:bool");
    assert_eq!(ArgKind::Float.parser_name(), "brigadier:float");
    assert_eq!(ArgKind::Double.parser_name(), "brigadier:double");
    assert_eq!(ArgKind::Integer.parser_name(), "brigadier:integer");
    assert_eq!(ArgKind::Long.parser_name(), "brigadier:long");
    assert_eq!(ArgKind::String.parser_name(), "brigadier:string");
    assert_eq!(ArgKind::GreedyString.parser_name(), "brigadier:string");
    // The string modes: word=0, quotable=1, greedy=2, as a VarInt property.
    assert_eq!(ArgKind::String.properties(), ArgProperties::VarInt(0));
    assert_eq!(ArgKind::Quotable.properties(), ArgProperties::VarInt(1));
    assert_eq!(ArgKind::GreedyString.properties(), ArgProperties::VarInt(2));
    // A parser with no properties writes nothing at all, so no stray byte
    // shifts the next node.
    assert_eq!(ArgKind::Bool.properties(), ArgProperties::None);
    assert_eq!(ArgKind::BlockPos.properties(), ArgProperties::None);
    // The numeric bounds flags, and the entity/score-holder flags.
    assert_eq!(ArgKind::Integer.properties(), ArgProperties::Flags(0));
    assert_eq!(
        ArgKind::IntegerRange {
            min: Some(0),
            max: Some(65535)
        }
        .properties(),
        ArgProperties::IntegerBounds {
            min: Some(0),
            max: Some(65535)
        }
    );
    assert_eq!(ArgKind::Long.properties(), ArgProperties::Flags(0));
    assert_eq!(ArgKind::Float.properties(), ArgProperties::Flags(0));
    assert_eq!(ArgKind::Double.properties(), ArgProperties::Flags(0));
    assert_eq!(ArgKind::Entity.properties(), ArgProperties::Flags(0x01));
    assert_eq!(ArgKind::Entities.properties(), ArgProperties::Flags(0x00));
    assert_eq!(ArgKind::Players.properties(), ArgProperties::Flags(0x02));
    assert_eq!(
        ArgKind::ScoreHolder.properties(),
        ArgProperties::Flags(0x00)
    );
    assert_eq!(ArgKind::Time.properties(), ArgProperties::Time);
    // Every kind names a parser, and a registry name that is either present
    // in the releases or the string fallback.
    for kind in ALL_KINDS {
        assert!(!kind.parser_name().is_empty(), "{kind:?}");
        assert!(!kind.registry_name().is_empty(), "{kind:?}");
    }
}

/// Every argument kind, for the exhaustive tree-level checks.
const ALL_KINDS: &[ArgKind] = &[
    ArgKind::String,
    ArgKind::Quotable,
    ArgKind::GreedyString,
    ArgKind::Message,
    ArgKind::Integer,
    ArgKind::IntegerRange {
        min: Some(0),
        max: Some(10),
    },
    ArgKind::Long,
    ArgKind::Float,
    ArgKind::Double,
    ArgKind::Bool,
    ArgKind::Enum(&["a", "b"]),
    ArgKind::Entity,
    ArgKind::Entities,
    ArgKind::Players,
    ArgKind::BlockPos,
    ArgKind::Vec3(true),
    ArgKind::Vec2(true),
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
    ArgKind::Team,
    ArgKind::Color,
    ArgKind::Swizzle,
    ArgKind::Angle,
    ArgKind::Operation,
    ArgKind::Rotation,
    ArgKind::Uuid,
];

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
        usage: None,
        bounds: None,
        task: None,
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
fn a_coordinate_argument_consumes_three_tokens_in_the_tree() {
    let noop = |_ctx: &mut Ctx| Reply(Vec::new());
    let tree = CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            usage: None,
            bounds: None,
            task: None,
            children: vec![Builder::literal("setblock")
                .then(Builder::arg("pos", ArgKind::BlockPos).executable(noop))
                .build()],
        },
    };
    let handle = bare_handle("bob");
    let m = tree.parse("setblock ~0 ~ ~", &handle).unwrap();
    assert_eq!(m.args.len(), 1);
    // The three tokens are kept as one argument's text, so `ctx.refs()`
    // hands the executor the same three words it always saw.
    assert_eq!(m.args[0].text, "~0 ~ ~");
    assert!(matches!(
        m.args[0].value,
        Value::Coordinates(c) if c.is_relative()
    ));
    // Garbage is a rendered error, never a panic — and no trailing token is
    // left behind for a later argument to misread.
    assert!(matches!(
        parse_err(&tree, "setblock x y z", &handle),
        ParseError::Argument(_)
    ));
    assert!(matches!(
        parse_err(&tree, "setblock 1 2", &handle),
        ParseError::IncompletePos { .. }
    ));
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
            usage: None,
            bounds: None,
            task: None,
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

fn always_hidden(_h: &Source) -> Result<(), Deny> {
    Err(Deny::Hidden)
}

fn always_denied(_h: &Source) -> Result<(), Deny> {
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
            usage: None,
            bounds: None,
            task: None,
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
            usage: None,
            bounds: None,
            task: None,
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
fn registry_arguments_suggest_from_the_vanilla_tables() {
    let noop = |_ctx: &mut Ctx| Reply(Vec::new());
    let tree = CommandTree {
        root: Node {
            name: "",
            aliases: Vec::new(),
            kind: Kind::Root,
            executable: false,
            requirement: Requirement::None,
            executor: None,
            usage: None,
            bounds: None,
            task: None,
            children: vec![Builder::literal("setblock")
                .then(Builder::arg("block", ArgKind::BlockState).executable(noop))
                .build()],
        },
    };
    let registry: SharedRegistry = Arc::new(Registry::default());
    let handle = join(&registry, "bob", 1);
    let ctx = SuggestCtx {
        registry: &registry,
        world: None,
    };
    let s = suggest_in(&tree, "/setblock minecraft:sto", &handle, &ctx);
    assert!(s.contains(&"minecraft:stone".to_string()), "{s:?}");
    assert!(s.iter().all(|w| w.starts_with("minecraft:sto")), "{s:?}");
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

#[cfg(feature = "plugins")]
#[test]
fn a_plugin_command_completes_through_the_plugin_host() {
    use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
    use aether_plugin::{
        HostBackend, HostConfig, LogLevel, ManifestFile, NativeContext, NativePlugin, Player,
        PluginHost,
    };

    #[derive(Default)]
    struct Backend;

    impl HostBackend for Backend {
        fn kv_get(&self, _plugin: &str, _key: &str) -> Option<Vec<u8>> {
            None
        }
        fn kv_put(&self, _plugin: &str, _key: &str, _value: &[u8]) {}
        fn kv_delete(&self, _plugin: &str, _key: &str) {}
        fn kv_list(&self, _plugin: &str, _prefix: &str) -> Vec<String> {
            Vec::new()
        }
        fn log(&self, _plugin: &str, _level: LogLevel, _message: &str) {}
        fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
            None
        }
        fn supports(&self, _plugin: &str, _feature: &str) -> bool {
            true
        }
    }

    struct CompletePlugin;

    impl NativePlugin for CompletePlugin {
        fn init(&mut self, _ctx: &mut NativeContext) -> Result<(), String> {
            Ok(())
        }
        fn on_complete(
            &mut self,
            _ctx: &mut NativeContext,
            _p: &Player,
            _name: &str,
            args: &[String],
        ) -> Vec<String> {
            let partial = args.last().map(String::as_str).unwrap_or("");
            ["alpha", "beta", "gamma"]
                .into_iter()
                .filter(|word| word.starts_with(partial))
                .map(str::to_string)
                .collect()
        }
    }

    let _guard = crate::permissions::test_lock();
    install_config();
    let mut host = PluginHost::new(HostConfig::default(), Arc::new(Backend)).unwrap();
    let file = ManifestFile {
        dir: std::path::PathBuf::new(),
        manifest: Manifest {
            id: "complete_test_plugin".to_string(),
            version: "0.1.0".to_string(),
            api: "0.1".to_string(),
            entry: String::new(),
            name: "Complete Test".to_string(),
            priority: 0,
            capabilities: Vec::new(),
            subscribe: vec!["on-command".to_string()],
            commands: vec![CommandDecl::Detailed {
                name: "complete_test_cmd".to_string(),
                node: "plugin.complete_test.cmd".to_string(),
                op: 0,
            }],
            tick_period: 1,
            deny_on_fault: false,
            tier: PluginTier::Native,
            requires: Vec::new(),
        },
    };
    host.add_native(&file, &[], &Default::default(), Box::new(CompletePlugin))
        .unwrap();
    crate::plugins::install_test_host(host);
    crate::commands::registry::register_plugin(
        "complete_test_cmd",
        "complete_test_plugin",
        "plugin.complete_test.cmd",
        0,
    )
    .unwrap();

    let registry: SharedRegistry = Arc::new(Registry::default());
    let handle = join(&registry, "completer", 1);
    let ctx = SuggestCtx {
        registry: &registry,
        world: None,
    };
    // The core tree does not own `complete_test_cmd`; the plugin answers the
    // token through `plugins::on_complete`, so the core fallback (which would
    // return nothing for an unknown name) is not reached.
    assert_eq!(
        suggest("/complete_test_cmd al", &handle, &ctx),
        vec!["alpha"]
    );
    assert_eq!(
        suggest("/complete_test_cmd ", &handle, &ctx),
        vec!["alpha", "beta", "gamma"]
    );
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
            usage: None,
            bounds: None,
            task: None,
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
        "You do not have permission to run this command."
    );

    // An op runs them. The replies are vanilla's en_us strings (CMD-05).
    assert_eq!(
        one(&env, &env.tester, "/gamemode creative"),
        "Set own game mode to Creative Mode"
    );
    assert_eq!(
        one(&env, &env.tester, "/gm s"),
        "Set own game mode to Survival Mode"
    );
    assert_eq!(
        one(&env, &env.tester, "/time set day"),
        "Set the time to 1000"
    );
    assert!(one(&env, &env.tester, "/time").starts_with("The time is "));
    assert_eq!(
        one(&env, &env.tester, "/give stone 3"),
        "Gave 3 minecraft:stone to tester"
    );
    assert_eq!(
        one(&env, &env.tester, "/summon pig"),
        "Summoned new minecraft:pig"
    );
    assert!(one(&env, &env.tester, "/killall").starts_with("Removed "));
}

#[test]
fn time_has_the_full_add_set_query_grammar_and_wraps() {
    use crate::game;

    let env = env();
    let who = &env.tester;

    for (word, value) in [
        ("day", 1000),
        ("noon", 6000),
        ("night", 13000),
        ("midnight", 18000),
    ] {
        assert_eq!(
            one(&env, who, &format!("/time set {word}")),
            format!("Set the time to {value}")
        );
        assert_eq!(
            one(&env, who, "/time query daytime"),
            format!("The time is {value}")
        );
    }

    // `set` and `add` work on the absolute daylight counter, so the day keeps
    // counting even though `daytime` wraps at 24,000.
    assert_eq!(one(&env, who, "/time set 1000"), "Set the time to 1000");
    assert_eq!(one(&env, who, "/time add 24000"), "Set the time to 1000");
    assert_eq!(one(&env, who, "/time query daytime"), "The time is 1000");
    assert_eq!(one(&env, who, "/time query day"), "The time is 1");
    assert_eq!(one(&env, who, "/time add 23000"), "Set the time to 0");
    assert_eq!(one(&env, who, "/time query daytime"), "The time is 0");
    assert_eq!(one(&env, who, "/time query day"), "The time is 2");

    // `set` may name a raw tick count; the reply echoes it, `daytime` wraps.
    assert_eq!(one(&env, who, "/time set 25000"), "Set the time to 25000");
    assert_eq!(one(&env, who, "/time query daytime"), "The time is 1000");
    assert_eq!(one(&env, who, "/time query day"), "The time is 1");

    // Time suffixes are ticks (`1s` = 20), as vanilla's TimeArgumentType.
    assert_eq!(one(&env, who, "/time set 1000"), "Set the time to 1000");
    assert_eq!(one(&env, who, "/time add 1s"), "Set the time to 1020");

    // The pre-tree forms still work: `/time <ticks>`, `/time day`, bare query.
    assert_eq!(one(&env, who, "/time 12000"), "Set the time to 12000");
    assert_eq!(one(&env, who, "/time day"), "Set the time to 1000");
    assert!(one(&env, who, "/time").starts_with("The time is "));
    assert!(one(&env, who, "/time query gametime").starts_with("The time is "));

    // A negative amount is refused by the time argument, with vanilla's text.
    assert_eq!(
        one(&env, who, "/time add -1000"),
        "The tick count must not be less than 0: found -1000"
    );

    // An out-of-range absolute tick count saturates instead of wrapping
    // negative.
    assert_eq!(
        one(&env, who, "/time set 3000000000"),
        format!("Set the time to {}", i32::MAX)
    );
    assert!(game::daylight_ticks() > 0);
}

#[test]
fn weather_sets_the_live_state_and_takes_tick_durations() {
    use crate::commands::world_env::{weather_state, Weather};

    let env = env();
    let who = &env.tester;

    assert_eq!(one(&env, who, "/weather rain"), "Set the weather to rain");
    let state = weather_state();
    assert_eq!(state.weather, Weather::Rain);
    assert!((12_000..=24_000).contains(&state.remaining), "{state:?}");

    assert_eq!(
        one(&env, who, "/weather thunder"),
        "Set the weather to rain & thunder"
    );
    assert_eq!(weather_state().weather, Weather::Thunder);

    // A duration is a TimeArgumentType: ticks, with `s`/`d` suffixes.
    assert_eq!(
        one(&env, who, "/weather clear 24000"),
        "Set the weather to clear"
    );
    assert_eq!(
        weather_state(),
        crate::commands::world_env::WeatherState {
            weather: Weather::Clear,
            remaining: 24_000,
        }
    );
    assert_eq!(
        one(&env, who, "/weather rain 1s"),
        "Set the weather to rain"
    );
    assert_eq!(weather_state().remaining, 20);
    assert_eq!(
        one(&env, who, "/weather rain 1d"),
        "Set the weather to rain"
    );
    assert_eq!(weather_state().remaining, 24_000);

    // There is no upper clamp beyond the `int` argument itself.
    assert_eq!(
        one(&env, who, "/weather rain 2000000"),
        "Set the weather to rain"
    );
    assert_eq!(weather_state().remaining, 2_000_000);

    // The duration's minimum is one tick, including for a negative value.
    assert_eq!(
        one(&env, who, "/weather clear 0"),
        "The tick count must not be less than 1: found 0"
    );
    assert_eq!(
        one(&env, who, "/weather clear -5"),
        "The tick count must not be less than 1: found -5"
    );
}

#[test]
fn worldborder_has_the_full_grammar_and_reports_its_size() {
    use crate::commands::world_env::{reset_world_border, tick_border, world_border};

    let env = env();
    let _guard = border_test_lock();
    reset_world_border();
    let who = &env.tester;

    assert_eq!(
        one(&env, who, "/worldborder get"),
        "The world border is currently 59999968 block(s) wide"
    );

    // Vanilla formats `set` with one decimal and `get` with none.
    assert_eq!(
        one(&env, who, "/worldborder set 100"),
        "Set the world border to 100.0 block(s) wide"
    );
    assert_eq!(
        one(&env, who, "/worldborder get"),
        "The world border is currently 100 block(s) wide"
    );
    assert_eq!(
        one(&env, who, "/worldborder add 50"),
        "Set the world border to 150.0 block(s) wide"
    );

    // A timed resize is in ticks (`200` = 10 s), as 1.21.11's
    // TimeArgumentType defines it; both growing and shrinking interpolate.
    assert_eq!(
        one(&env, who, "/worldborder set 200 200"),
        "Growing the world border to 200.0 blocks wide over 10.00 seconds"
    );
    for _ in 0..100 {
        tick_border();
    }
    assert!((world_border().size - 175.0).abs() < 1e-9);
    for _ in 0..100 {
        tick_border();
    }
    assert_eq!(world_border().size, 200.0);

    assert_eq!(
        one(&env, who, "/worldborder set 50 1s"),
        "Shrinking the world border to 50.0 block(s) wide over 1.00 second(s)"
    );
    for _ in 0..20 {
        tick_border();
    }
    assert_eq!(world_border().size, 50.0);
    assert_eq!(
        one(&env, who, "/worldborder add -49"),
        "Set the world border to 1.0 block(s) wide"
    );

    // Bounds and no-change refusals. A distance outside ±59,999,968 is an
    // argument error; the size bound is reached through `add`.
    assert_eq!(
        one(&env, who, "/worldborder set 0"),
        "World border cannot be smaller than 1 block wide"
    );
    assert_eq!(
        one(&env, who, "/worldborder set 59999969"),
        "Double must not be more than 5.9999968E7: found 5.9999969E7"
    );
    assert_eq!(
        one(&env, who, "/worldborder add 59999968"),
        "World border cannot be bigger than 5.999997E7 blocks wide"
    );
    assert_eq!(
        one(&env, who, "/worldborder set 1"),
        "Nothing changed. The world border is already that size"
    );
    assert_eq!(
        one(&env, who, "/worldborder add 0"),
        "Nothing changed. The world border is already that size"
    );

    // Centre, damage and warning settings, including their no-change lines.
    assert_eq!(
        one(&env, who, "/worldborder center 10 20"),
        "Set the center of the world border to 10.00, 20.00"
    );
    let border = world_border();
    assert_eq!((border.center_x, border.center_z), (10.0, 20.0));
    assert_eq!(
        one(&env, who, "/worldborder center 10 20"),
        "Nothing changed. The world border is already centered there"
    );
    assert_eq!(
        one(&env, who, "/worldborder center 30000000 0"),
        "World border cannot be further out than 2.9999984E7 blocks"
    );
    assert_eq!(
        one(&env, who, "/worldborder damage amount 0.5"),
        "Set the world border damage to 0.50 per block each second"
    );
    assert_eq!(
        one(&env, who, "/worldborder damage amount 0.5"),
        "Nothing changed. The world border damage is already that amount"
    );
    assert_eq!(
        one(&env, who, "/worldborder damage buffer 10"),
        "Set the world border damage buffer to 10.00 block(s)"
    );
    assert_eq!(
        one(&env, who, "/worldborder damage amount -1"),
        "Float must not be less than 0.0: found -1.0"
    );
    assert_eq!(
        one(&env, who, "/worldborder warning distance 10"),
        "Set the world border warning distance to 10 block(s)"
    );
    assert_eq!(
        one(&env, who, "/worldborder warning distance -1"),
        "Integer must not be less than 0: found -1"
    );
    // `warning time` is stored and sent in ticks: 30 s is 600 ticks, while a
    // raw `200` is ten seconds.
    assert_eq!(
        one(&env, who, "/worldborder warning time 30s"),
        "Set the world border warning time to 30.00 second(s)"
    );
    assert_eq!(world_border().warning_time, 600);
    assert_eq!(
        one(&env, who, "/worldborder warning time 200"),
        "Set the world border warning time to 10.00 second(s)"
    );
    let border = world_border();
    assert_eq!(border.damage_per_block, 0.5);
    assert_eq!(border.damage_buffer, 10.0);
    assert_eq!(border.warning_distance, 10);
    assert_eq!(border.warning_time, 200);
}

#[test]
fn worldborder_add_extends_a_running_lerp_and_non_size_edits_keep_it() {
    use crate::commands::world_env::{reset_world_border, tick_border, world_border};

    let env = env();
    let _guard = border_test_lock();
    reset_world_border();
    let who = &env.tester;

    assert_eq!(
        one(&env, who, "/worldborder set 100"),
        "Set the world border to 100.0 block(s) wide"
    );
    assert_eq!(
        one(&env, who, "/worldborder set 200 200"),
        "Growing the world border to 200.0 blocks wide over 10.00 seconds"
    );
    // `add` with a time extends the resize already in flight: 200 remaining
    // plus 100 new ticks is 15 seconds in total.
    // `add` measures from the current (still 100) size, while its time
    // extends the resize already in flight: 200 remaining plus 100 new ticks.
    assert_eq!(
        one(&env, who, "/worldborder add 50 100"),
        "Growing the world border to 150.0 blocks wide over 15.00 seconds"
    );
    for _ in 0..100 {
        tick_border();
    }
    // 100 of 300 ticks from 100 to 150 is one third of the way.
    assert!((world_border().size - 116.666_666_666_666_67).abs() < 1e-9);
    assert_eq!(world_border().target_size, 150.0);
    assert_eq!(world_border().lerp_ticks(), 200);

    // A centre edit must not cancel the resize.
    assert_eq!(
        one(&env, who, "/worldborder center 10 20"),
        "Set the center of the world border to 10.00, 20.00"
    );
    assert!(world_border().resizing(), "the centre edit keeps the lerp");
    assert_eq!(world_border().target_size, 150.0);

    for _ in 0..200 {
        tick_border();
    }
    assert_eq!(world_border().size, 150.0);
    assert!(!world_border().resizing());
}

#[test]
fn setworldspawn_changes_the_runtime_spawn_and_defaultgamemode_the_next_join() {
    use crate::game;

    let env = env();
    let who = &env.tester;

    assert_eq!(
        one(&env, who, "/setworldspawn 10 64 10 90"),
        "Set the world spawn point to 10, 64, 10 [90.0]"
    );
    let spawn = game::world_spawn_override().expect("spawn set");
    assert_eq!((spawn.x, spawn.y, spawn.z), (10, 64, 10));
    assert_eq!(spawn.angle, 90.0);

    // A relative angle resolves against the caller's yaw (the harness joins
    // facing 0).
    assert_eq!(
        one(&env, who, "/setworldspawn 10 64 10 ~90"),
        "Set the world spawn point to 10, 64, 10 [90.0]"
    );

    // With no position, vanilla uses the caller's block position and angle 0.
    assert_eq!(
        one(&env, who, "/setworldspawn"),
        "Set the world spawn point to 0, 64, 0 [0.0]"
    );

    // `/spawn` follows the world spawn an operator has just moved, even for
    // a player who was already online.
    assert_eq!(
        one(&env, who, "/setworldspawn 10 64 10"),
        "Set the world spawn point to 10, 64, 10 [0.0]"
    );
    assert_eq!(one(&env, who, "/spawn"), "Teleported to spawn");
    let pos = env.tester.pos();
    assert_eq!(
        (pos.x.floor() as i32, pos.z.floor() as i32),
        (10, 10),
        "the player was moved to the new world spawn"
    );

    for (mode, display) in [
        ("survival", "Survival Mode"),
        ("creative", "Creative Mode"),
        ("adventure", "Adventure Mode"),
        ("spectator", "Spectator Mode"),
    ] {
        assert_eq!(
            one(&env, who, &format!("/defaultgamemode {mode}")),
            format!("The default game mode is now {display}")
        );
    }
    assert_eq!(
        game::default_game_mode(),
        crate::protocol::GameMode::Spectator
    );
    assert_eq!(crate::protocol::GameMode::Adventure.wire(), 2);
}

#[test]
fn the_world_environment_commands_are_node_gated() {
    let env = env();
    for text in [
        "/time set day",
        "/weather clear",
        "/worldborder get",
        "/setworldspawn",
        "/defaultgamemode creative",
    ] {
        assert_eq!(
            one(&env, &env.bob, text),
            "You do not have permission to run this command.",
            "`{text}`"
        );
    }
}

#[test]
fn self_and_world_commands_route_through_the_tree() {
    let env = env();
    let who = &env.tester;
    assert!(run(&env, who, "/kill").is_empty());
    assert_eq!(one(&env, who, "/spawn"), "Teleported to spawn");
    assert_eq!(one(&env, who, "/tp 1 2 3"), "Teleported tester to 1, 2, 3");
    assert_eq!(one(&env, who, "/tp bob"), "Teleported tester to bob");
    assert!(one(&env, who, "/block 0 64 0").contains(':'));
    assert_eq!(one(&env, who, "/food"), "Food set to 20");
    assert_eq!(one(&env, who, "/heal"), "Healed");
    // `/visibility` toggles and reads back.
    assert_eq!(one(&env, who, "/visibility"), "Visibility culling is on");
    assert_eq!(
        one(&env, who, "/visibility off"),
        "Visibility culling off: you are sent every cell"
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
fn help_renders_from_the_tree_with_usages_and_stub_marks() {
    let env = env();
    // A real command shows its tree usage, with its aliases.
    assert_eq!(
        one(&env, &env.tester, "/help gamemode"),
        "/gamemode (gm) <gamemode> [<targets>]"
    );
    // An alias resolves to the canonical usage.
    assert_eq!(
        one(&env, &env.tester, "/help gm"),
        "/gamemode (gm) <gamemode> [<targets>]"
    );
    // A specified-but-unimplemented command is listed and marked.
    assert_eq!(
        one(&env, &env.tester, "/help xp"),
        "/experience (xp) — not implemented yet (CMD-10)"
    );
    // The `?` alias routes to the same node.
    assert_eq!(
        one(&env, &env.tester, "/? gm"),
        "/gamemode (gm) <gamemode> [<targets>]"
    );
    // Unknown command -> exactly one line.
    assert_eq!(
        run(&env, &env.tester, "/help definitely-not-a-command"),
        vec!["Unknown command. Try /help".to_string()]
    );
    // A category word lists just that category from the catalogue (CMD-48):
    // the admin verbs appear, the gameplay verbs do not.
    let admin = run(&env, &env.tester, "/help admin");
    assert!(
        admin.iter().any(|l| l.contains("/op")),
        "/help admin lists the admin category: {admin:?}"
    );
    assert!(
        !admin.iter().any(|l| l.contains("/gamemode")),
        "/help admin must not list another category: {admin:?}"
    );
}

#[test]
fn help_with_no_argument_lists_the_visible_tree_paged() {
    let env = env();
    let first = run(&env, &env.tester, "/help");
    // The pager header, one entry per line, and a nav footer when there is
    // more than one page.
    assert!(first[0].starts_with("Commands — page 1 of "), "{first:?}");
    // The whole catalogue is reachable by paging; `/gamemode` and the stub
    // mark sit past the first page, so a single page would miss them.
    let all = all_help(&env, &env.tester);
    assert!(
        all.iter()
            .any(|l| l.contains("/gamemode (gm) <gamemode> [<targets>]")),
        "{all:?}"
    );
    assert!(
        all.iter()
            .any(|l| l.contains("/experience (xp) (not implemented yet)")),
        "{all:?}"
    );
    // `all_help` left the pager on the last page; run `/help` again so the
    // navigation below starts from page 1.
    let _ = run(&env, &env.tester, "/help");
    // Paging forward reaches a later page, and going back reaches page 1.
    let next = run(&env, &env.tester, "/page next");
    assert!(next[0].contains("page 2 of "), "{next:?}");
    let back = run(&env, &env.tester, "/page prev");
    assert!(back[0].contains("page 1 of "), "{back:?}");
    // `goto` out of range clamps to the last page rather than wrapping or
    // panicking.
    let last = run(&env, &env.tester, "/page 99999");
    let pages = first[0]
        .split("page 1 of ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .unwrap()
        .to_string();
    assert!(
        last[0].contains(&format!("page {pages} of {pages}")),
        "{last:?}"
    );
}

#[test]
fn help_does_not_leak_op_only_commands_to_a_non_op() {
    let env = env();
    // An alias of an op-only command is also hidden.
    assert_eq!(
        run(&env, &env.bob, "/help gamemode"),
        vec!["Unknown command. Try /help".to_string()]
    );
    assert_eq!(
        run(&env, &env.bob, "/help gm"),
        vec!["Unknown command. Try /help".to_string()]
    );
    // And the whole paged catalogue must not name them either.
    let bob_help = all_help(&env, &env.bob);
    assert!(
        !bob_help.iter().any(|l| l.contains("/gamemode")),
        "{bob_help:?}"
    );
    assert!(
        !bob_help.iter().any(|l| l.contains("/stop")),
        "{bob_help:?}"
    );
    // A public command is still there.
    assert!(
        bob_help.iter().any(|l| l.starts_with("/kill ")) || bob_help.iter().any(|l| l == "/kill")
    );
    // The operator sees it, so the filter is the caller, not the tree.
    // (`/stop` is implemented by CMD-07 and has no usage, so it lists bare.)
    assert!(all_help(&env, &env.tester).iter().any(|l| l == "/stop"));
}

#[test]
fn an_alias_can_never_collide_with_a_literal() {
    // The tree's one dispatch invariant, checked over the whole real tree.
    assert_eq!(tree().find_duplicate_name(), None);
}

#[test]
fn send_command_feedback_defaults_on_and_survives_the_world() {
    let env = env();
    use crate::commands::feedback;
    assert!(
        feedback::send_command_feedback(&env.world),
        "vanilla default"
    );
    feedback::set_send_command_feedback(&env.world, false);
    assert!(!feedback::send_command_feedback(&env.world));
    feedback::set_send_command_feedback(&env.world, true);
    assert!(feedback::send_command_feedback(&env.world));
}

#[test]
fn a_function_is_silenced_while_a_player_keeps_their_errors() {
    let env = env();
    use crate::commands::feedback::{self, Feedback, GameRules};
    use crate::commands::source::CommandSource;
    let off = GameRules {
        send_command_feedback: false,
    };
    let on = GameRules {
        send_command_feedback: true,
    };
    let player = CommandSource::Player(&env.tester);
    let function = CommandSource::Function { name: "tick" };
    let success = Feedback::success(vec!["done".into()]);
    let error = Feedback::error(vec!["nope".into()]);

    // A player: the rule suppresses success but never an error.
    let mut seen = Vec::new();
    feedback::send(&player, &success, off, |l| seen.push(l.to_string()));
    assert!(seen.is_empty(), "the rule silences a player's success");
    feedback::send(&player, &success, on, |l| seen.push(l.to_string()));
    assert_eq!(seen, vec!["done".to_string()]);
    seen.clear();
    feedback::send(&player, &error, off, |l| seen.push(l.to_string()));
    assert_eq!(seen, vec!["§cnope".to_string()], "errors always show");

    // A function: always suppressed, rule or no rule, success or error.
    for rules in [off, on] {
        for feedback in [&success, &error] {
            seen.clear();
            feedback::send(&function, feedback, rules, |l| seen.push(l.to_string()));
            assert!(seen.is_empty(), "a function is always suppressed");
        }
    }
}

#[test]
fn the_stub_registry_is_on_the_tree() {
    let env = env();
    // A specified-but-unimplemented command answers with its task id, and its
    // op level is enforced by the tree.
    assert_eq!(
        one(&env, &env.tester, "/xp"),
        "/experience is not implemented yet (CMD-10)"
    );
    assert_eq!(
        one(&env, &env.bob, "/xp"),
        "You do not have permission to run this command."
    );
    assert_eq!(
        one(&env, &env.tester, "/clear"),
        "/clear is not implemented yet (CMD-10)"
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

#[test]
fn a_double_slash_command_falls_through_the_tree_to_the_plugin_registry() {
    // The tree parses the body after stripping one `/`, so `//set` becomes
    // `/set` there. No core node matches; the only other owner is the plugin
    // registry, and it must see the bare name (the worldedit commands are
    // declared without a slash). This drives the real dispatcher, not the
    // registry helper directly.
    let env = env();
    let name = "tree_double_slash_fixture";
    crate::commands::registry::register_plugin(
        name,
        "tree_fixture",
        "plugin.tree_fixture.double_slash",
        0,
    )
    .unwrap();
    let lines = run(&env, &env.tester, &format!("//{name}"));
    assert_eq!(
        lines,
        vec![format!("/{name} is not available right now")],
        "the fallback reached the plugin registry, not a parse error"
    );
}

// ---------------------------------------------------------------------------
// CMD-16: tags, teams and team messages
// ---------------------------------------------------------------------------

#[test]
fn tag_add_list_remove_and_selector_matching() {
    let env = env();
    assert_eq!(
        one(&env, &env.tester, "/tag @s add here"),
        "Added tag 'here' to tester"
    );
    // Adding the same tag twice changes nothing, exactly as vanilla throws.
    assert_eq!(
        one(&env, &env.tester, "/tag @s add here"),
        "Target either already has the tag or has too many tags"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @s list"),
        "tester has 1 tags: here"
    );
    // The selector filter sees the tag the command just wrote.
    assert_eq!(
        one(&env, &env.tester, "/tag @e[tag=here] list"),
        "tester has 1 tags: here"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @e[tag=!here] list"),
        "bob has no tags"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @s remove here"),
        "Removed tag 'here' from tester"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @s remove here"),
        "Target does not have this tag"
    );
    assert_eq!(one(&env, &env.tester, "/tag @s list"), "tester has no tags");
}

#[test]
fn tags_are_saved_with_the_player_record() {
    let env = env();
    one(&env, &env.tester, "/tag @s add saved");
    let blob = env
        .world
        .get_meta(&crate::game::player::key(env.tester.uuid))
        .unwrap()
        .expect("the command saved the player");
    let saved = crate::game::player::decode(&blob).expect("a version 3 save");
    assert_eq!(saved.tags, vec!["saved".to_string()]);
}

#[test]
fn team_lifecycle_moves_members_and_lists_them_sorted() {
    let env = env();
    assert_eq!(
        one(&env, &env.tester, "/team add red"),
        "Created team [red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team add red"),
        "A team already exists by that name"
    );
    assert_eq!(
        one(&env, &env.tester, "/team join red"),
        "Added tester to team [red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team join red bob"),
        "Added bob to team [red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team list red"),
        "Team [red] has 2 member(s): bob, tester"
    );
    assert_eq!(
        one(&env, &env.tester, "/team list"),
        "There are 1 team(s): [red]"
    );

    one(&env, &env.tester, "/team add blue");
    assert_eq!(
        one(&env, &env.tester, "/team join blue tester"),
        "Added tester to team [blue]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team list red"),
        "Team [red] has 1 member(s): bob"
    );
    assert_eq!(
        one(&env, &env.tester, "/team list blue"),
        "Team [blue] has 1 member(s): tester"
    );

    assert_eq!(
        one(&env, &env.tester, "/team empty blue"),
        "Removed 1 member(s) from team [blue]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team empty blue"),
        "Nothing changed. That team is already empty"
    );
    assert_eq!(
        one(&env, &env.tester, "/team leave bob"),
        "Removed bob from any team"
    );
    assert_eq!(
        one(&env, &env.tester, "/team leave bob"),
        "Removed bob from any team"
    );
    assert_eq!(
        one(&env, &env.tester, "/team remove red"),
        "Removed team [red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team remove red"),
        "Unknown team 'red'"
    );
}

#[test]
fn team_modify_covers_every_option_and_rejects_bad_input() {
    let env = env();
    one(&env, &env.tester, "/team add red");
    assert_eq!(
        one(&env, &env.tester, "/team modify red prefix \"<\""),
        "Team prefix set to <"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red suffix \">\""),
        "Team suffix set to >"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red color red"),
        "Updated the color for team §c[red] to red"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red color red"),
        "Nothing changed. That team already has that color"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red color nope"),
        "Unknown color 'nope'"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red friendlyFire false"),
        "Disabled friendly fire for team §c[red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team modify red friendlyFire false"),
        "Nothing changed. Friendly fire is already disabled for that team"
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red seeFriendlyInvisibles false"
        ),
        "Team §c[red] can no longer see invisible teammates"
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red seeFriendlyInvisibles true"
        ),
        "Team §c[red] can now see invisible teammates"
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red nametagVisibility never"
        ),
        "Nametag visibility for team §c[red] is now \"Never\""
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red deathMessageVisibility hideForOwnTeam"
        ),
        "Death message visibility for team §c[red] is now \"Hide for own team\""
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red collisionRule pushOwnTeam"
        ),
        "Collision rule for team §c[red] is now \"Push own team\""
    );
    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red displayName {\"text\":\"Red Team\",\"color\":\"gold\"}"
        ),
        "Updated the name of team §c[§6Red Team]"
    );
    // A malformed component is an error, not a panic.
    assert_eq!(
        one(&env, &env.tester, "/team modify red prefix {"),
        "Invalid component: unterminated compound at byte 1"
    );

    let team = crate::team::find(&env.world, "red").expect("team still exists");
    assert!(!team.friendly_fire);
    assert_eq!(team.prefix, "<");
    assert_eq!(team.suffix, ">");
    assert_eq!(team.color, crate::team::Color::Red);
    assert_eq!(team.nametag_visibility, crate::team::Visibility::Never);
    assert_eq!(
        team.death_message_visibility,
        crate::team::Visibility::HideForOwnTeam
    );
    assert_eq!(team.collision_rule, crate::team::CollisionRule::PushOwnTeam);
    assert_eq!(team.display_name, "§6Red Team");
}

#[test]
fn teammsg_needs_a_team_and_the_tm_alias_works() {
    let env = env();
    assert_eq!(
        one(&env, &env.tester, "/teammsg hello"),
        "You must be on a team to message your team"
    );
    one(&env, &env.tester, "/team add red");
    one(&env, &env.tester, "/team join red");
    assert_eq!(
        one(&env, &env.tester, "/teammsg hello team"),
        "-> [red] <tester> hello team"
    );
    assert_eq!(
        one(&env, &env.tester, "/tm hello again"),
        "-> [red] <tester> hello again"
    );
}

#[test]
fn team_join_accepts_a_selector_and_rejects_an_unknown_team() {
    let env = env();
    one(&env, &env.tester, "/team add red");
    assert_eq!(
        one(&env, &env.tester, "/team join red @a[name=bob]"),
        "Added bob to team [red]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team join missing @a"),
        "Unknown team 'missing'"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @e[type=player] list"),
        "There are no tags on the 2 entities"
    );
}

#[test]
fn collision_rules_decide_whether_players_collide() {
    let env = env();
    one(&env, &env.tester, "/team add red");
    one(&env, &env.tester, "/team add blue");
    assert_eq!(
        one(&env, &env.tester, "/team join blue bob"),
        "Added bob to team [blue]"
    );
    assert_eq!(
        one(&env, &env.tester, "/team join red"),
        "Added tester to team [red]"
    );

    // Different teams, default ALWAYS.
    assert!(crate::team::collides(&env.world, "tester", "bob"));

    assert_eq!(
        one(&env, &env.tester, "/team modify red collisionRule never"),
        "Collision rule for team [red] is now \"Never\""
    );
    assert!(!crate::team::collides(&env.world, "tester", "bob"));

    one(
        &env,
        &env.tester,
        "/team modify red collisionRule pushOwnTeam",
    );
    // `pushOwnTeam` refuses only teammates: Bob is on another team, so the
    // two still collide (vanilla's contrary-to-the-name rule).
    assert!(crate::team::collides(&env.world, "tester", "bob"));

    one(&env, &env.tester, "/team join red bob");
    // Now they are teammates, and `pushOwnTeam` stops the push.
    assert!(!crate::team::collides(&env.world, "tester", "bob"));

    one(
        &env,
        &env.tester,
        "/team modify red collisionRule pushOtherTeams",
    );
    // `pushOtherTeams` refuses outsiders: a teammate now collides.
    assert!(crate::team::collides(&env.world, "tester", "bob"));

    one(&env, &env.tester, "/team modify red collisionRule always");
    assert!(crate::team::collides(&env.world, "tester", "bob"));
}

#[test]
fn death_message_visibility_filters_viewers() {
    let env = env();
    one(&env, &env.tester, "/team add red");
    one(&env, &env.tester, "/team join red @a");

    assert_eq!(
        one(
            &env,
            &env.tester,
            "/team modify red deathMessageVisibility never"
        ),
        "Death message visibility for team [red] is now \"Never\""
    );
    assert!(!crate::team::death_message_visible(
        &env.world, "tester", "bob"
    ));
    assert!(!crate::team::death_message_visible(
        &env.world, "tester", "carol"
    ));

    one(
        &env,
        &env.tester,
        "/team modify red deathMessageVisibility hideForOwnTeam",
    );
    assert!(!crate::team::death_message_visible(
        &env.world, "tester", "bob"
    ));
    assert!(crate::team::death_message_visible(
        &env.world, "tester", "carol"
    ));

    one(
        &env,
        &env.tester,
        "/team modify red deathMessageVisibility hideForOtherTeams",
    );
    assert!(crate::team::death_message_visible(
        &env.world, "tester", "bob"
    ));
    assert!(!crate::team::death_message_visible(
        &env.world, "tester", "carol"
    ));

    one(
        &env,
        &env.tester,
        "/team modify red deathMessageVisibility always",
    );
    assert!(crate::team::death_message_visible(
        &env.world, "tester", "carol"
    ));
}

#[test]
fn the_level_selector_compares_experience_level_not_total_xp() {
    let env = env();
    {
        let mut st = env.tester.game();
        st.xp_total = 352; // 16 levels, but 352 total XP
    }
    assert_eq!(
        one(&env, &env.tester, "/tag @a[level=16] add lvl"),
        "Added tag 'lvl' to tester"
    );
    // No player has 352 levels; 352 is only this player's total XP.
    assert_eq!(
        one(&env, &env.tester, "/tag @a[level=352] add nope"),
        "No player was found"
    );
    assert_eq!(
        one(&env, &env.tester, "/tag @s list"),
        "tester has 1 tags: lvl"
    );
}

#[test]
fn non_player_team_members_are_visible_to_selectors() {
    let env = env();
    one(&env, &env.tester, "/team add red");
    let uuid = 0xae7e_0000_0000_4000_8000_0000_dead_beefu128;
    let name = crate::team::uuid_name(uuid);
    assert_eq!(
        one(&env, &env.tester, &format!("/team join red {name}")),
        format!("Added {name} to team [red]")
    );
    let entity = crate::game::entities::Selectable {
        id: 1234,
        uuid,
        kind: "minecraft:zombie",
        pos: aether_api::Vector3::new(1.0, 64.0, 1.0),
        yaw: 0.0,
        pitch: 0.0,
        alive: true,
        tags: Vec::new(),
    };
    let snapshot =
        crate::commands::team_tag::entity_ref(entity, &crate::team::TeamIndex::load(&env.world));
    assert_eq!(snapshot.team.as_deref(), Some("red"));
}

#[test]
fn the_tag_cap_is_vanillas_1024() {
    let env = env();
    for i in 0..crate::game::player::MAX_TAGS {
        env.tester.game().tags.insert(format!("t{i}"));
    }
    assert_eq!(
        one(&env, &env.tester, "/tag @s add overflow"),
        "Target either already has the tag or has too many tags"
    );
}

#[test]
fn the_declared_team_tree_uses_vanillas_argument_parsers() {
    use crate::commands::tree::{ArgKind, Kind, Node};
    fn child<'a>(node: &'a Node, name: &str) -> &'a Node {
        node.children
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no child `{name}` under `{}`", node.name))
    }
    fn arg(node: &Node) -> ArgKind {
        match node.kind {
            Kind::Argument(kind) => kind,
            _ => panic!("`{}` is not an argument", node.name),
        }
    }

    let commands = crate::commands::tree::tree();
    let tag = commands.find_command("tag").unwrap();
    assert!(!tag.executable, "vanilla's /tag root is not executable");
    assert_eq!(arg(child(tag, "targets")), ArgKind::Entities);

    let team = commands.find_command("team").unwrap();
    assert!(!team.executable, "vanilla's /team root is not executable");
    assert_eq!(arg(child(child(team, "list"), "team")), ArgKind::Team);
    assert_eq!(arg(child(child(team, "add"), "team")), ArgKind::String);
    assert_eq!(arg(child(child(team, "remove"), "team")), ArgKind::Team);
    assert_eq!(arg(child(child(team, "empty"), "team")), ArgKind::Team);
    assert_eq!(arg(child(child(team, "join"), "team")), ArgKind::Team);
    assert_eq!(
        arg(child(child(child(team, "join"), "team"), "members")),
        ArgKind::ScoreHolder
    );
    assert_eq!(
        arg(child(child(team, "leave"), "members")),
        ArgKind::ScoreHolder
    );
    let modify_team = child(child(team, "modify"), "team");
    assert_eq!(arg(modify_team), ArgKind::Team);
    assert_eq!(
        arg(child(child(modify_team, "color"), "value")),
        ArgKind::Color
    );

    let teammsg = commands.find_command("teammsg").unwrap();
    assert!(!teammsg.executable);
    assert_eq!(arg(child(teammsg, "message")), ArgKind::Message);
}

#[test]
fn team_arguments_resolve_before_selectors_and_components() {
    let env = env();
    // Unknown team beats a malformed selector and a malformed component, as
    // vanilla resolves the `minecraft:team` argument first.
    assert_eq!(
        one(&env, &env.tester, "/team join missing @e[type=nope]"),
        "Unknown team 'missing'"
    );
    // A component is parsed before any executor runs, so a broken component
    // is the parse error; only the selector resolution comes after the team
    // lookup, as vanilla's `TeamArgument.getTeam(...)` is evaluated first.
    assert_eq!(
        one(&env, &env.tester, "/team modify missing prefix {"),
        "Invalid component: unterminated compound at byte 1"
    );
}

#[test]
fn spectators_never_take_part_in_team_collision() {
    use crate::players::PosLook;
    use crate::protocol::GameMode;

    let env = env();
    let at = |x: f64| PosLook {
        x,
        y: 64.0,
        z: 0.5,
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    };
    env.tester.set_pos(at(0.5));
    env.bob.set_pos(at(2.5));

    // Two unteamed players collide under vanilla's default `always`.
    assert!(crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.5),
        at(2.5)
    ));

    // A spectator is not pushable.
    env.bob.game().mode = GameMode::Spectator;
    assert!(!crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.5),
        at(2.5)
    ));

    // A moving spectator is not blocked either.
    env.bob.game().mode = GameMode::Survival;
    env.tester.game().mode = GameMode::Spectator;
    assert!(!crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.5),
        at(2.5)
    ));

    // An already-overlapping pair can always walk apart.
    env.tester.game().mode = GameMode::Survival;
    env.bob.set_pos(at(0.7));
    assert!(!crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.5),
        at(0.7)
    ));
}

#[test]
fn word_arguments_do_not_accept_quoted_spans() {
    let env = env();
    let commands = crate::commands::tree::tree();
    // `brigadier:string` word: the quote is an ordinary character, so a
    // quoted phrase leaves trailing input and does not parse. This matches
    // vanilla's `StringArgumentType.word()`.
    assert!(commands
        .parse("/tag @s add \"two words\"", &env.tester)
        .is_err());
    assert!(commands
        .parse("/team add \"red team\"", &env.tester)
        .is_err());
    assert!(commands.parse("/tag @s add two", &env.tester).is_ok());
}

#[test]
fn ordinary_sub_block_steps_are_checked_against_other_players() {
    use crate::players::PosLook;

    let env = env();
    let at = |x: f64| PosLook {
        x,
        y: 64.0,
        z: 0.5,
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    };
    env.tester.set_pos(at(0.2));
    // Bob's box is [0.7, 1.3]; the mover's destination [0.15, 0.75] overlaps.
    env.bob.set_pos(at(1.0));

    // A normal 0.25-block step, far below the 0.6 box width, still blocks:
    // the earlier guard compared the mover's own before/to and never looked
    // at the other player.
    assert!(crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.2),
        at(0.45)
    ));

    // An already-overlapping pair may walk apart.
    env.tester.set_pos(at(0.7));
    env.bob.set_pos(at(1.0));
    assert!(!crate::session::team_collision_blocks(
        &env.tester,
        &env.registry,
        &env.world,
        at(0.7),
        at(0.75)
    ));
}

// ---------------------------------------------------------------------------
// CMD-19: client effects
// ---------------------------------------------------------------------------

#[test]
fn particle_recipients_are_distance_filtered() {
    let env = env();
    let who = &env.tester;
    let here = PosLook {
        x: 0.5,
        y: 64.0,
        z: 0.5,
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    };
    env.tester.set_pos(here);
    env.bob.set_pos(here);

    // 400 blocks away: outside the normal 32-block range, inside the forced
    // 512-block one. `force` widens the range; it does not mean "everyone".
    assert_eq!(
        one(&env, who, "/particle flame 400 64 0"),
        "The particle was not visible for anybody"
    );
    assert_eq!(
        one(&env, who, "/particle flame 400 64 0 0 0 0 1 1 force"),
        "Displaying particle minecraft:flame"
    );
    // An explicit viewer list goes through the same distance filter.
    assert_eq!(
        one(&env, who, "/particle flame 400 64 0 0 0 0 1 1 normal @a"),
        "The particle was not visible for anybody"
    );
    assert_eq!(
        one(&env, who, "/particle flame 400 64 0 0 0 0 1 1 force @a"),
        "Displaying particle minecraft:flame"
    );
}

#[test]
fn particle_and_playsound_skip_targets_in_another_level() {
    let env = env();
    let who = &env.tester;
    env.registry
        .insert_for_test(crate::players::PlayerHandle::for_test_in_world(
            991,
            "netherguy",
            "the_nether",
        ));
    // The two are at the same position, so only the level check can skip the
    // target.
    let here = PosLook {
        x: 0.5,
        y: 64.0,
        z: 0.5,
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    };
    env.tester.set_pos(here);
    assert_eq!(
        one(
            &env,
            who,
            "/particle flame 0 64 0 0 0 0 1 1 force @e[type=player,name=netherguy]"
        ),
        "The particle was not visible for anybody"
    );
    assert_eq!(
        one(
            &env,
            who,
            "/playsound block.note_block.harp master @e[type=player,name=netherguy]"
        ),
        "The sound is too far away to be heard"
    );
}

#[test]
fn particle_and_sound_commands_route_through_the_tree() {
    let env = env();
    let who = &env.tester;

    // `/particle` reports vanilla's `Displaying particle …`; the optional
    // position/delta/speed/count and the `force` flag all parse, and the name
    // may omit `minecraft:`.
    for (text, shown) in [
        ("/particle minecraft:flame", "minecraft:flame"),
        ("/particle flame 0 64 0 0 0 0 0 4 force", "minecraft:flame"),
        (
            "/particle flame 0 64 0 0.1 0.2 0.3 0 0 normal @a",
            "minecraft:flame",
        ),
        (
            "/particle dust{color:[1.0,0.0,0.0],scale:1.0}",
            "minecraft:dust",
        ),
        (
            "/particle minecraft:block{block_state:\"minecraft:stone\"}",
            "minecraft:block",
        ),
        (
            "/particle item{item:{id:\"minecraft:stone\",count:1}}",
            "minecraft:item",
        ),
        (
            "/particle vibration{destination:{type:\"minecraft:block\",pos:[1,2,3]},arrival_in_ticks:10}",
            "minecraft:vibration",
        ),
        // An integer colour is accepted as well as a float list.
        ("/particle dust{color:16711680,scale:1.0}", "minecraft:dust"),
        // Vanilla's `MapCodec.unit` ignores an options object on a simple
        // particle.
        ("/particle flame{color:1}", "minecraft:flame"),
        // count 0 means "as many as the client can render".
        ("/particle flame 0 64 0 0.5 0.5 0.5 0 0", "minecraft:flame"),
        // 1.21.11's registry name for ids 106/107 is `detection`.
        (
            "/particle minecraft:trial_spawner_detection",
            "minecraft:trial_spawner_detection",
        ),
    ] {
        assert_eq!(
            one(&env, who, text),
            format!("Displaying particle {shown}"),
            "`{text}`"
        );
    }
    assert_eq!(
        one(&env, who, "/particle minecraft:not_a_particle"),
        "Unknown particle: minecraft:not_a_particle"
    );
    // The stale `minecraft-data` spelling is not a 1.21.11 name.
    assert_eq!(
        one(
            &env,
            who,
            "/particle minecraft:trial_spawner_detected_player"
        ),
        "Unknown particle: minecraft:trial_spawner_detected_player"
    );
    // Vanilla's `particle.invalidOptions`, for a missing or bad object.
    assert_eq!(
        one(&env, who, "/particle dust"),
        "Can't parse particle options: Expected particle options"
    );
    assert!(
        one(&env, who, "/particle dust{color:[1.0],scale:1.0}")
            .starts_with("Can't parse particle options:"),
        "a bad colour list is an options error"
    );
    // No whitespace may separate the id from its options object, so the
    // parameterised particle is read without one and fails as vanilla's
    // `ParticleArgument` does.
    assert_eq!(
        one(&env, who, "/particle dust {color:[1.0,0.0,0.0],scale:1.0}"),
        "Can't parse particle options: Expected particle options"
    );
    for bad in [
        "/particle block{block_state:\"minecraft:not_a_block\"}",
        "/particle item{item:{id:\"minecraft:not_an_item\"}}",
        "/particle dust{color:[1",
        "/particle vibration{destination:{type:\"minecraft:entity\",pos:[1,2,3]},arrival_in_ticks:1}",
    ] {
        assert!(
            one(&env, who, bad).starts_with("Can't parse particle options:"),
            "`{bad}`"
        );
    }
    assert_eq!(
        one(&env, who, "/particle flame 0 64 0 0 0 0 0 -1"),
        "Integer must not be less than 0: found -1"
    );
    // Brigadier reads left to right: the particle name and its options are
    // resolved before `pos`/`delta`/`speed`/`count`, so their errors win.
    assert_eq!(
        one(
            &env,
            who,
            "/particle minecraft:not_a_particle 0 64 0 0 0 0 0 -1"
        ),
        "Unknown particle: minecraft:not_a_particle"
    );
    assert_eq!(
        one(&env, who, "/particle minecraft:not_a_particle 0 64"),
        "Unknown particle: minecraft:not_a_particle"
    );
    assert!(
        one(&env, who, "/particle dust{bad} 0 64 0 0 0 0 0 -1")
            .starts_with("Can't parse particle options:"),
        "the options error wins over the later count error"
    );
    assert_eq!(
        one(&env, who, "/particle flame 0 64 0 0 0 0 -1 0"),
        "Float must not be less than 0.0: found -1.0"
    );
    // `delta` and `speed` are not executable in vanilla's grammar.
    assert_eq!(
        one(&env, who, "/particle flame 0 64 0 0 0 0"),
        "Unknown or incomplete command"
    );
    assert_eq!(
        one(&env, who, "/particle flame 0 64 0 0"),
        "Incomplete (expected 3 coordinates)"
    );

    // `/playsound`: the default is the caller as the only target, and an
    // explicit target list picks single/multiple wording.
    assert_eq!(
        one(&env, who, "/playsound minecraft:block.note_block.harp"),
        "Played sound minecraft:block.note_block.harp to tester"
    );
    assert_eq!(
        one(&env, who, "/playsound block.note_block.harp master tester"),
        "Played sound minecraft:block.note_block.harp to tester"
    );
    assert_eq!(
        one(&env, who, "/playsound block.note_block.harp master @a"),
        "Played sound minecraft:block.note_block.harp to 2 players"
    );
    assert_eq!(
        one(
            &env,
            who,
            "/playsound block.note_block.harp master @a 0 64 0 0 1"
        ),
        "Played sound minecraft:block.note_block.harp to 2 players"
    );
    // `/playsound` takes a plain resource location: a custom id a pack
    // defines is accepted even though it is not in the vanilla registry.
    assert_eq!(
        one(&env, who, "/playsound minecraft:not_a_sound"),
        "Played sound minecraft:not_a_sound to tester"
    );
    assert_eq!(
        one(&env, who, "/playsound Bad:Name"),
        "Invalid resource location 'Bad:Name'"
    );
    // Bounds are Brigadier argument errors, so they win over resource and
    // position errors, and the values print as floats.
    assert_eq!(
        one(
            &env,
            who,
            "/playsound minecraft:not_a_sound master @a 0 64 0 -1"
        ),
        "Float must not be less than 0.0: found -1.0"
    );
    assert_eq!(
        one(
            &env,
            who,
            "/playsound block.note_block.harp master @a 0 64 0 1 3"
        ),
        "Float must not be more than 2.0: found 3.0"
    );
    // Nobody inside the 16-block range and minVolume 0: the command fails.
    assert_eq!(
        one(
            &env,
            who,
            "/playsound block.note_block.harp master @a 1000 64 0"
        ),
        "The sound is too far away to be heard"
    );
    // With minVolume the sound is relayed two blocks from each target and
    // both are counted as hearing it.
    assert_eq!(
        one(
            &env,
            who,
            "/playsound block.note_block.harp master @a 1000 64 0 1 1 0.5"
        ),
        "Played sound minecraft:block.note_block.harp to 2 players"
    );

    // `/stopsound` has one line per combination of source and sound.
    assert_eq!(one(&env, who, "/stopsound tester"), "Stopped all sounds");
    assert_eq!(
        one(&env, who, "/stopsound tester master"),
        "Stopped all 'master' sounds"
    );
    assert_eq!(
        one(
            &env,
            who,
            "/stopsound tester master minecraft:block.note_block.harp"
        ),
        "Stopped sound 'minecraft:block.note_block.harp' on source 'master'"
    );
    assert_eq!(
        one(&env, who, "/stopsound @a * minecraft:block.note_block.harp"),
        "Stopped sound 'minecraft:block.note_block.harp'"
    );
    // `*` with no sound is not a complete command in vanilla.
    assert_eq!(
        one(&env, who, "/stopsound @a *"),
        "Unknown or incomplete command"
    );
    // A target list requires a source literal in 1.21.11's grammar.
    assert_eq!(
        one(&env, who, "/playsound block.note_block.harp @a"),
        "Unknown or incomplete command"
    );

    // Op level 2: a non-operator is refused by the tree.
    for text in [
        "/particle flame",
        "/playsound block.note_block.harp",
        "/stopsound tester",
    ] {
        assert_eq!(
            one(&env, &env.bob, text),
            "You do not have permission to run this command.",
            "`{text}`"
        );
    }
}
