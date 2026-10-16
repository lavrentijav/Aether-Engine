//! The Brigadier-style command tree: one grammar, one parser, one dispatcher.
//!
//! This replaces the hand-written `match`-per-module dispatch the server used
//! to have. Every command — the history verbs, the gameplay verbs, the economy
//! and the not-yet-implemented core stubs — is a node in one tree built once
//! (a `OnceLock`) and only ever read afterwards. [`execute`] walks typed text
//! against the tree; [`suggest`] walks the partially-typed text for
//! tab-completion. There is exactly one place a command name is answered.
//!
//! The shape mirrors `com.mojang.brigadier`, but only the subset this server
//! needs: literal nodes (with aliases), argument nodes of the vanilla kinds,
//! an optional per-node requirement, an executor, and children. Redirects and
//! the full reader/redirect modifier set are CMD-22/CMD-21 and are not here.

use std::sync::OnceLock;

use super::feedback::Feedback;
use super::registry;
use super::Reply;
use crate::economy::Economy;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;

pub mod suggest;
pub use suggest::{suggest, suggest_in, SuggestCtx};

/// A closure that runs a command. Boxed (not a bare `fn`) so a node can carry
/// a value derived from a registry entry — the stubs capture their task id.
pub type ExecFn = dyn for<'a, 'b> Fn(&mut Ctx<'a, 'b>) -> Reply + Send + Sync;

/// A command source: who typed it, the live world and registry, the economy,
/// and the already-parsed arguments of the matched command.
pub struct Ctx<'a, 'e> {
    /// The player who typed the command.
    pub handle: &'a PlayerHandle,
    /// Online players, for name lookups and broadcasts.
    pub registry: &'a SharedRegistry,
    /// The world the command acts on.
    pub world: &'a DemoWorld,
    /// The installed economy, if any.
    pub econ: Option<&'e dyn Economy>,
    /// Parsed arguments, in the order their nodes were matched.
    pub args: Vec<ParsedArg>,
}

impl Ctx<'_, '_> {
    /// The raw text of every parsed argument, split on whitespace, for the
    /// bodies that still parse their own filters (`/lookup`, `/tp`, …). A
    /// greedy argument holds the whole remainder as one piece; the ported
    /// bodies expect the same word-split they always received.
    pub fn refs(&self) -> Vec<&str> {
        self.args
            .iter()
            .flat_map(|a| a.text.split_whitespace())
            .collect()
    }
}

/// One parsed argument: its typed value and the exact text it came from.
pub struct ParsedArg {
    /// The typed value, for executors that want it.
    pub value: Value,
    /// The source text, for executors that parse it themselves.
    pub text: String,
}

/// A parsed argument value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A string, resource location, entity name, …: text that stays text.
    Text(String),
    /// A parsed `integer`.
    Int(i32),
    /// A parsed `long`.
    Long(i64),
    /// A parsed `float`.
    Float(f32),
    /// A parsed `double`.
    Double(f64),
    /// A parsed `bool`.
    Bool(bool),
}

/// Every argument kind the tree understands. The set is the one the brief
/// names; the primitives parse and validate strictly, the rest are single
/// tokens today and gain real parsers as their CMD tasks land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgKind {
    /// `brigadier:string`, single word (quoted allowed).
    String,
    /// `brigadier:string`, quotable phrase.
    Quotable,
    /// `brigadier:string`, greedy phrase: takes the rest of the line.
    GreedyString,
    /// A chat message: greedy and quote-aware.
    Message,
    /// `brigadier:integer`.
    Integer,
    /// `brigadier:long`.
    Long,
    /// `brigadier:float`.
    Float,
    /// `brigadier:double`.
    Double,
    /// `brigadier:bool`.
    Bool,
    /// A fixed set of words.
    Enum(&'static [&'static str]),
    /// `minecraft:entity`.
    Entity,
    /// `minecraft:entities`.
    Entities,
    /// `minecraft:block_pos`.
    BlockPos,
    /// `minecraft:vec3`.
    Vec3,
    /// `minecraft:vec2`.
    Vec2,
    /// `minecraft:block_state`.
    BlockState,
    /// `minecraft:item_stack`.
    ItemStack,
    /// `minecraft:nbt_compound_tag`.
    NbtCompound,
    /// `minecraft:nbt_path`.
    NbtPath,
    /// `minecraft:component`.
    Component,
    /// `minecraft:resource_location`.
    ResourceLocation,
    /// `minecraft:resource_or_tag`.
    ResourceOrTag,
    /// `minecraft:game_profile`.
    GameProfile,
    /// `minecraft:time`.
    Time,
    /// `minecraft:gamemode`.
    Gamemode,
    /// `minecraft:difficulty`.
    Difficulty,
    /// `minecraft:particle`.
    Particle,
    /// `minecraft:score_holder`.
    ScoreHolder,
    /// `minecraft:swizzle`.
    Swizzle,
    /// `minecraft:angle`.
    Angle,
    /// `minecraft:operation`.
    Operation,
    /// `minecraft:rotation`.
    Rotation,
    /// `minecraft:uuid`.
    Uuid,
}

impl ArgKind {
    /// The parser identifier a **pre-1.19** Declare Commands packet names for
    /// this kind (`brigadier:string`, `minecraft:entity`, …).
    ///
    /// The string kinds — the only ones the tree registers today — are exact.
    /// The rest are named from `minecraft-data`'s `command_node` table for
    /// 1.18.2, the last release before parser ids replaced names. The kinds
    /// that stand in for a fixed set of words, or for a parser that carries a
    /// registry the kind does not have, are declared as a plain
    /// `brigadier:string`: the client then colours them but does not
    /// misparse a property field it was never sent.
    pub fn parser_name(self) -> &'static str {
        match self {
            // Fixed word sets are a single-word string on the wire.
            ArgKind::String | ArgKind::Quotable | ArgKind::GreedyString | ArgKind::Enum(_) => {
                "brigadier:string"
            }
            // No release registers a `minecraft:difficulty` parser; vanilla
            // spells `/difficulty` as a string.
            ArgKind::Difficulty => "brigadier:string",
            // `minecraft:resource_or_tag` carries a registry identifier the
            // kind cannot supply yet, so it is declared as a string until a
            // CMD task gives the kind that data.
            ArgKind::ResourceOrTag => "brigadier:string",
            ArgKind::Message => "minecraft:message",
            ArgKind::Integer => "brigadier:integer",
            ArgKind::Long => "brigadier:long",
            ArgKind::Float => "brigadier:float",
            ArgKind::Double => "brigadier:double",
            ArgKind::Bool => "brigadier:bool",
            ArgKind::Entity | ArgKind::Entities => "minecraft:entity",
            ArgKind::BlockPos => "minecraft:block_pos",
            ArgKind::Vec3 => "minecraft:vec3",
            ArgKind::Vec2 => "minecraft:vec2",
            ArgKind::BlockState => "minecraft:block_state",
            ArgKind::ItemStack => "minecraft:item_stack",
            // Renamed `minecraft:nbt` when parser ids arrived in 1.19; the
            // old spelling is what a ByName codec expects.
            ArgKind::NbtCompound => "minecraft:nbt_compound_tag",
            ArgKind::NbtPath => "minecraft:nbt_path",
            ArgKind::Component => "minecraft:component",
            ArgKind::ResourceLocation => "minecraft:resource_location",
            ArgKind::GameProfile => "minecraft:game_profile",
            ArgKind::Time => "minecraft:time",
            ArgKind::Gamemode => "minecraft:gamemode",
            ArgKind::Particle => "minecraft:particle",
            ArgKind::ScoreHolder => "minecraft:score_holder",
            ArgKind::Swizzle => "minecraft:swizzle",
            ArgKind::Angle => "minecraft:angle",
            ArgKind::Operation => "minecraft:operation",
            ArgKind::Rotation => "minecraft:rotation",
            ArgKind::Uuid => "minecraft:uuid",
        }
    }

    /// The name a **1.19+** parser registry uses for this kind. It differs
    /// from [`ArgKind::parser_name`] only for the NBT compound tag, which
    /// `minecraft-data` lists as `minecraft:nbt` from 1.19 on.
    pub fn registry_name(self) -> &'static str {
        match self {
            ArgKind::NbtCompound => "minecraft:nbt",
            other => other.parser_name(),
        }
    }

    /// The property field this kind writes after its parser.
    ///
    /// Bounds and registry names the tree does not model stay at their
    /// defaults — an unbounded number, a single entity, a non-multiple score
    /// holder — because a node cannot express anything else yet. A parser with
    /// no properties writes nothing at all; writing the zero byte the old
    /// shallow tree always wrote is what made a client read the next field
    /// from the wrong offset.
    pub fn properties(self) -> ArgProperties {
        match self {
            ArgKind::String | ArgKind::Enum(_) | ArgKind::Difficulty | ArgKind::ResourceOrTag => {
                ArgProperties::VarInt(0)
            }
            ArgKind::Quotable => ArgProperties::VarInt(1),
            ArgKind::GreedyString => ArgProperties::VarInt(2),
            // Void properties: nothing follows the parser.
            ArgKind::Message
            | ArgKind::Bool
            | ArgKind::BlockPos
            | ArgKind::Vec3
            | ArgKind::Vec2
            | ArgKind::BlockState
            | ArgKind::ItemStack
            | ArgKind::NbtCompound
            | ArgKind::NbtPath
            | ArgKind::Component
            | ArgKind::ResourceLocation
            | ArgKind::GameProfile
            | ArgKind::Gamemode
            | ArgKind::Particle
            | ArgKind::Swizzle
            | ArgKind::Angle
            | ArgKind::Operation
            | ArgKind::Rotation
            | ArgKind::Uuid => ArgProperties::None,
            // The numeric kinds write a flags byte with the min/max bits;
            // neither is present.
            ArgKind::Integer | ArgKind::Long | ArgKind::Float | ArgKind::Double => {
                ArgProperties::Flags(0)
            }
            // `0x01` is "single", as vanilla writes `EntityArgumentType`.
            ArgKind::Entity => ArgProperties::Flags(0x01),
            ArgKind::Entities => ArgProperties::Flags(0x00),
            ArgKind::ScoreHolder => ArgProperties::Flags(0x00),
            // An `i32` minimum from 1.19.4 on, and no field before that; the
            // serializer decides from the codec's parser generation.
            ArgKind::Time => ArgProperties::Time,
        }
    }

    /// Parse one argument from `r`, returning its typed value and source text.
    fn parse(self, r: &mut StringReader) -> Result<(Value, String), ParseError> {
        match self {
            ArgKind::GreedyString | ArgKind::Message => {
                let t = r.read_remainder()?;
                Ok((Value::Text(t.clone()), t))
            }
            ArgKind::String | ArgKind::Quotable => {
                // A string argument is quoted-aware: an unterminated quote is
                // `InvalidQuoted`, never a silently empty word.
                let t = r.read_string_arg()?;
                Ok((Value::Text(t.clone()), t))
            }
            ArgKind::Integer => {
                let t = r.read_token();
                t.parse::<i32>()
                    .map(|n| (Value::Int(n), t.clone()))
                    .map_err(|_| ParseError::InvalidInteger(t))
            }
            ArgKind::Long => {
                let t = r.read_token();
                t.parse::<i64>()
                    .map(|n| (Value::Long(n), t.clone()))
                    .map_err(|_| ParseError::InvalidLong(t))
            }
            ArgKind::Float => {
                let t = r.read_token();
                t.parse::<f32>()
                    .map(|n| (Value::Float(n), t.clone()))
                    .map_err(|_| ParseError::InvalidFloat(t))
            }
            ArgKind::Double => {
                let t = r.read_token();
                t.parse::<f64>()
                    .map(|n| (Value::Double(n), t.clone()))
                    .map_err(|_| ParseError::InvalidDouble(t))
            }
            ArgKind::Bool => {
                let t = r.read_token();
                match t.as_str() {
                    "true" => Ok((Value::Bool(true), t)),
                    "false" => Ok((Value::Bool(false), t)),
                    _ => Err(ParseError::InvalidBool(t)),
                }
            }
            ArgKind::Enum(words) => {
                let t = r.read_token();
                if words.contains(&t.as_str()) {
                    Ok((Value::Text(t.clone()), t))
                } else {
                    Err(ParseError::IncorrectArg)
                }
            }
            _ => {
                let t = r.read_token();
                Ok((Value::Text(t.clone()), t))
            }
        }
    }
}

/// The property field that follows a parser on the wire.
///
/// Only a few argument types carry one. The serializer writes the right
/// number of bytes per variant, so a node can never emit the stray zero byte
/// that shifts every field after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgProperties {
    /// Nothing follows the parser.
    None,
    /// A VarInt: the string mode (`0` word, `1` quotable, `2` greedy).
    VarInt(i32),
    /// A single flags byte: the numeric bounds bits, or the entity and
    /// score-holder flags.
    Flags(u8),
    /// An `i32` minimum — `minecraft:time` from 1.19.4 on, and no field at all
    /// before that.
    Time,
}

/// A requirement's verdict.
pub enum Deny {
    /// Refuse with this message.
    Message(String),
    /// Pretend the node does not exist — vanilla hides commands the caller
    /// cannot see. Used by tests and, later, by fully hidden nodes.
    Hidden,
}

/// A node's permission requirement.
#[derive(Clone, Copy)]
pub enum Requirement {
    /// No requirement.
    None,
    /// A named check, e.g. "is an operator", with its own denial message.
    Fn(fn(&PlayerHandle) -> Result<(), Deny>),
    /// An op-level gate from the core registry (CMD-01/CMD0b), with the
    /// generic permission message.
    Level {
        /// Required op level.
        op: u8,
        /// Permission node, for the resolver.
        node: &'static str,
    },
}

/// Whether `req` lets `handle` see and run the node.
pub fn check(req: &Requirement, handle: &PlayerHandle) -> Result<(), Deny> {
    match req {
        Requirement::None => Ok(()),
        Requirement::Fn(f) => f(handle),
        Requirement::Level { op, node } => {
            // The CMD-01 core: the player's level comes from the persisted
            // store (seeded from `config.operators`), the node's default
            // resolves from the store, and the command's `op` level is the
            // floor. This is the same gate `commands::permitted` exposes; the
            // tree calls the core directly so a `Level` node is unambiguous.
            let level = crate::permissions::level(&handle.name, handle.ip);
            if crate::permissions::check(level, node, *op) {
                Ok(())
            } else {
                Err(Deny::Message(
                    "You do not have permission to run this command.".into(),
                ))
            }
        }
    }
}

/// A tree node.
pub struct Node {
    /// Literal word, or argument name.
    pub name: &'static str,
    /// Alternative literal words that address the same node.
    pub aliases: Vec<&'static str>,
    /// What the node is.
    pub kind: Kind,
    /// Whether the grammar may end here.
    pub executable: bool,
    /// The permission gate.
    pub requirement: Requirement,
    /// The body, when executable.
    pub executor: Option<Box<ExecFn>>,
    /// Following nodes.
    pub children: Vec<Node>,
    /// Vanilla-style usage text without the leading slash (`<gamemode>
    /// [<targets>]`), when the tree's own grammar cannot express it because the
    /// ported body still parses a single greedy remainder. `None` means "derive
    /// it from the children", which is what every real argument node gets.
    pub usage: Option<&'static str>,
    /// For a specified-but-unimplemented command, the CMD task that will
    /// implement it. `/help` marks these; `None` is a working command. This is
    /// authoritative where the registry is not: `/aether` has a registry entry
    /// (CMD-24) but a real body today.
    pub task: Option<&'static str>,
}

/// The node's category.
pub enum Kind {
    /// The tree root.
    Root,
    /// A fixed word.
    Literal,
    /// A typed argument.
    Argument(ArgKind),
}

impl Kind {
    fn is_literal(&self) -> bool {
        matches!(self, Kind::Literal)
    }
}

impl Node {
    fn matches_literal(&self, word: &str) -> bool {
        self.kind.is_literal() && (self.name == word || self.aliases.contains(&word))
    }

    /// Run the node's executor, if it has one.
    pub fn run(&self, ctx: &mut Ctx) -> Option<Reply> {
        self.executor.as_deref().map(|f| f(ctx))
    }
}

/// The immutable command tree.
pub struct CommandTree {
    /// The root node; its children are the command literals.
    pub root: Node,
}

impl CommandTree {
    /// The command literals, one node per command (aliases live on the node).
    pub fn commands(&self) -> &[Node] {
        &self.root.children
    }

    /// The command node addressed by `word` — its canonical name or any alias.
    ///
    /// This is how `/help <word>` resolves an alias to the one node it shares
    /// with the canonical name.
    pub fn find_command(&self, word: &str) -> Option<&Node> {
        self.root.children.iter().find(|c| c.matches_literal(word))
    }

    /// The first name or alias declared by two different commands, if any.
    ///
    /// The tree's one invariant is that every literal word (canonical or
    /// alias) names exactly one node: an alias that collided with a literal
    /// would make dispatch depend on declaration order. Tests assert this is
    /// `None`; [`build`] debug-asserts it too.
    pub fn find_duplicate_name(&self) -> Option<&'static str> {
        let mut seen: Vec<&'static str> = Vec::new();
        for command in self.commands() {
            for name in std::iter::once(command.name).chain(command.aliases.iter().copied()) {
                if seen.contains(&name) {
                    return Some(name);
                }
                seen.push(name);
            }
        }
        None
    }

    /// Parse `text` and return the matched executable node plus its arguments.
    pub fn parse<'s>(&'s self, text: &str, handle: &PlayerHandle) -> Result<Match<'s>, ParseError> {
        let text = text.trim();
        let text = text.strip_prefix('/').unwrap_or(text);
        let mut r = StringReader::new(text);
        let mut args = Vec::new();
        let node = parse_node(&self.root, &mut r, handle, &mut args)?;
        Ok(Match { node, args })
    }
}

/// A parsed command: the matched node and the arguments it collected.
pub struct Match<'s> {
    /// The matched executable node.
    pub node: &'s Node,
    /// The parsed arguments, in order.
    pub args: Vec<ParsedArg>,
}

/// Literal node flag: the node is a fixed word.
pub const NODE_LITERAL: u8 = 0x01;
/// Argument node flag: the node is a typed argument.
pub const NODE_ARGUMENT: u8 = 0x02;
/// Executable node flag: the grammar may end here.
pub const NODE_EXECUTABLE: u8 = 0x04;

/// A node flattened for the Declare Commands packet.
///
/// This is the serialization view of the tree: the protocol serializer never
/// walks [`Node`]s itself, so the wire shape is described in exactly one
/// place here and the per-version parser lookup stays in `protocol`.
pub struct WireNode {
    /// The node's flag byte: [`NODE_LITERAL`], [`NODE_ARGUMENT`] and
    /// [`NODE_EXECUTABLE`]. Redirect (`0x08`) and suggestion-provider (`0x10`)
    /// are not modelled yet.
    pub flags: u8,
    /// The literal word or argument name. Absent for the root.
    pub name: Option<&'static str>,
    /// The argument parser, for an argument node.
    pub parser: Option<ArgKind>,
    /// Indices into the same vector of the node's children.
    pub children: Vec<usize>,
}

/// Flatten `t` for the wire. Index 0 is the root; a command and each of its
/// aliases is its own literal node over the same children, and an argument
/// node records its parser. Requirement-gated nodes are declared like any
/// other: visibility is the client's decision from the tree, and permission
/// is enforced on execution.
pub fn wire_nodes(t: &CommandTree) -> (Vec<WireNode>, usize) {
    let mut nodes = vec![WireNode {
        flags: 0,
        name: None,
        parser: None,
        children: Vec::new(),
    }];
    let mut root_children = Vec::new();
    for command in t.commands() {
        let children: Vec<usize> = command
            .children
            .iter()
            .map(|child| flatten_node(&mut nodes, child))
            .collect();
        let mut names = vec![command.name];
        names.extend(command.aliases.iter().copied());
        for name in names {
            let idx = nodes.len();
            nodes.push(WireNode {
                flags: NODE_LITERAL
                    | if command.executable {
                        NODE_EXECUTABLE
                    } else {
                        0
                    },
                name: Some(name),
                parser: None,
                children: children.clone(),
            });
            root_children.push(idx);
        }
    }
    nodes[0].children = root_children;
    (nodes, 0)
}

fn flatten_node(nodes: &mut Vec<WireNode>, node: &Node) -> usize {
    let children: Vec<usize> = node
        .children
        .iter()
        .map(|child| flatten_node(nodes, child))
        .collect();
    let executable = if node.executable { NODE_EXECUTABLE } else { 0 };
    let (flags, parser) = match &node.kind {
        Kind::Argument(kind) => (NODE_ARGUMENT | executable, Some(*kind)),
        _ => (NODE_LITERAL | executable, None),
    };
    let idx = nodes.len();
    nodes.push(WireNode {
        flags,
        name: Some(node.name),
        parser,
        children,
    });
    idx
}

/// The one shared tree, built once.
pub fn tree() -> &'static CommandTree {
    static TREE: OnceLock<CommandTree> = OnceLock::new();
    TREE.get_or_init(build)
}

/// Build the whole tree: every command the server answers.
fn build() -> CommandTree {
    let mut children = Vec::new();
    children.extend(super::core_nodes());
    children.extend(crate::game::commands::nodes());
    children.extend(crate::economy::commands::nodes());
    // The not-yet-implemented core commands: registered now so completion and
    // permission checks are right from day one. A name a real command already
    // answers is skipped — there must be exactly one owner.
    for entry in registry::CORE {
        if children
            .iter()
            .any(|n| n.name == entry.name || entry.aliases.iter().any(|a| n.aliases.contains(a)))
        {
            continue;
        }
        children.push(stub(entry));
    }
    let mut root = Builder::literal("");
    root.node.kind = Kind::Root;
    root.node.children = children;
    let tree = CommandTree { root: root.node };
    // An alias that collided with another command's literal (or an implemented
    // name that shadowed a stub's alias) would make dispatch depend on
    // declaration order; refuse it in every build, including tests.
    debug_assert!(
        tree.find_duplicate_name().is_none(),
        "a command name or alias is declared twice: {:?}",
        tree.find_duplicate_name()
    );
    tree
}

/// A stub node for a specified-but-unimplemented command. Its executor names
/// the CMD task; the requirement is the registry's op level.
fn stub(entry: &'static registry::Entry) -> Node {
    let mut b = Builder::literal(entry.name).requires(Requirement::Level {
        op: entry.op,
        node: entry.node,
    });
    for alias in entry.aliases {
        b = b.alias(alias);
    }
    b.executable(move |_ctx| {
        Reply(vec![format!(
            "/{} is not implemented yet ({})",
            entry.name, entry.task
        )])
    })
    .task(entry.task)
    .build()
}

/// A fluent node builder. Public so each module can register its commands.
pub struct Builder {
    node: Node,
}

impl Builder {
    /// Start a literal node.
    pub fn literal(name: &'static str) -> Builder {
        Builder {
            node: Node {
                name,
                aliases: Vec::new(),
                kind: Kind::Literal,
                executable: false,
                requirement: Requirement::None,
                executor: None,
                children: Vec::new(),
                usage: None,
                task: None,
            },
        }
    }

    /// Start an argument node.
    pub fn arg(name: &'static str, kind: ArgKind) -> Builder {
        Builder {
            node: Node {
                name,
                aliases: Vec::new(),
                kind: Kind::Argument(kind),
                executable: false,
                requirement: Requirement::None,
                executor: None,
                children: Vec::new(),
                usage: None,
                task: None,
            },
        }
    }

    /// Add an alias word.
    pub fn alias(mut self, alias: &'static str) -> Builder {
        self.node.aliases.push(alias);
        self
    }

    /// Set the node's usage text, for a grammar the tree does not carry.
    pub fn usage(mut self, usage: &'static str) -> Builder {
        self.node.usage = Some(usage);
        self
    }

    /// Mark the node as a stub for a not-yet-implemented command.
    pub fn task(mut self, task: &'static str) -> Builder {
        self.node.task = Some(task);
        self
    }

    /// Set the requirement.
    pub fn requires(mut self, req: Requirement) -> Builder {
        self.node.requirement = req;
        self
    }

    /// Make the node executable and give it a body.
    pub fn executable(
        mut self,
        f: impl for<'a, 'b> Fn(&mut Ctx<'a, 'b>) -> Reply + Send + Sync + 'static,
    ) -> Builder {
        self.node.executable = true;
        self.node.executor = Some(Box::new(f));
        self
    }

    /// Attach a child node.
    pub fn then(mut self, child: Builder) -> Builder {
        self.node.children.push(child.node);
        self
    }

    /// Finish the node.
    pub fn build(self) -> Node {
        self.node
    }
}

/// Attach a usage string to an already-built node.
///
/// The ported bodies parse one greedy remainder, so their grammar is not in
/// the tree; this gives `/help` the vanilla usage without a second table keyed
/// by name.
pub fn with_usage(mut node: Node, usage: &'static str) -> Node {
    node.usage = Some(usage);
    node
}

/// The usage text for `node`, after its name: what `/help <command>` prints.
///
/// A node with an explicit [`Node::usage`] returns it. Otherwise the grammar
/// is read off the children: a literal is its word (recursing for a
/// subcommand), a required argument is `<name>`, a greedy remainder is
/// `[name]`, and an executable optional argument is `[<name>]`.
pub fn usage_of(node: &Node) -> String {
    if let Some(usage) = node.usage {
        return usage.to_string();
    }
    let parts: Vec<String> = node
        .children
        .iter()
        .filter_map(|child| {
            let text = match &child.kind {
                Kind::Root => return None,
                Kind::Literal => {
                    let sub = usage_of(child);
                    if child.children.is_empty() {
                        child.name.to_string()
                    } else {
                        format!("{} {}", child.name, sub)
                    }
                }
                Kind::Argument(ArgKind::GreedyString | ArgKind::Message) => {
                    format!("[{}]", child.name)
                }
                Kind::Argument(_) if child.executable => format!("[<{}>]", child.name),
                Kind::Argument(_) => format!("<{}>", child.name),
            };
            Some(text)
        })
        .collect();
    parts.join(" ")
}

/// A command that takes no arguments.
pub fn simple(
    name: &'static str,
    aliases: &[&'static str],
    req: Requirement,
    f: fn(&mut Ctx) -> Reply,
) -> Node {
    let mut b = Builder::literal(name).requires(req);
    for alias in aliases {
        b = b.alias(alias);
    }
    b.executable(f).build()
}

/// A command whose arguments are taken as the greedy remainder: what every
/// ported command uses today, since its body still parses its own filters.
pub fn greedy(
    name: &'static str,
    aliases: &[&'static str],
    req: Requirement,
    f: fn(&mut Ctx) -> Reply,
) -> Node {
    let mut b = Builder::literal(name).requires(req);
    for alias in aliases {
        b = b.alias(alias);
    }
    b.executable(f)
        .then(Builder::arg("args", ArgKind::GreedyString).executable(f))
        .build()
}

/// A parse failure, rendered the way vanilla renders it.
#[derive(Clone, Debug, PartialEq)]
pub enum ParseError {
    /// No node matched, or the grammar ended too early.
    Incomplete,
    /// A token was there but no argument accepted it.
    IncorrectArg,
    /// `Invalid integer 'x'`.
    InvalidInteger(String),
    /// `Invalid long 'x'`.
    InvalidLong(String),
    /// `Invalid float 'x'`.
    InvalidFloat(String),
    /// `Invalid double 'x'`.
    InvalidDouble(String),
    /// `Invalid bool 'x'`.
    InvalidBool(String),
    /// A quoted string never closed.
    InvalidQuoted,
    /// A requirement refused: the denial reply.
    Denied(String),
}

impl ParseError {
    /// The single chat line this error produces.
    pub fn message(&self) -> String {
        match self {
            ParseError::Incomplete => "Unknown or incomplete command".into(),
            ParseError::IncorrectArg => "Incorrect argument for command".into(),
            ParseError::InvalidInteger(x) => format!("Invalid integer '{x}'"),
            ParseError::InvalidLong(x) => format!("Invalid long '{x}'"),
            ParseError::InvalidFloat(x) => format!("Invalid float '{x}'"),
            ParseError::InvalidDouble(x) => format!("Invalid double '{x}'"),
            ParseError::InvalidBool(x) => format!("Invalid bool '{x}'"),
            ParseError::InvalidQuoted => "Invalid quoted string".into(),
            ParseError::Denied(m) => m.clone(),
        }
    }
}

fn parse_node<'s>(
    node: &'s Node,
    r: &mut StringReader,
    handle: &PlayerHandle,
    args: &mut Vec<ParsedArg>,
) -> Result<&'s Node, ParseError> {
    r.skip_whitespace();
    if !r.can_read() {
        return if node.executable {
            Ok(node)
        } else {
            Err(ParseError::Incomplete)
        };
    }
    let start = r.cursor;
    let mut best: Option<ParseError> = None;
    for child in &node.children {
        r.cursor = start;
        let args_len = args.len();
        match consume(child, r, args) {
            Ok(()) => match check(&child.requirement, handle) {
                Err(Deny::Hidden) => {}
                Err(Deny::Message(m)) => return Err(ParseError::Denied(m)),
                Ok(()) => match parse_node(child, r, handle, args) {
                    Ok(found) => return Ok(found),
                    Err(ParseError::Denied(m)) => return Err(ParseError::Denied(m)),
                    Err(e) => best = best.or(Some(e)),
                },
            },
            Err(ConsumeError::NoMatch) => {}
            Err(ConsumeError::Parse(e)) => best = best.or(Some(e)),
        }
        r.cursor = start;
        args.truncate(args_len);
    }
    if node.children.is_empty() {
        return Err(ParseError::IncorrectArg);
    }
    Err(best.unwrap_or(ParseError::Incomplete))
}

enum ConsumeError {
    NoMatch,
    Parse(ParseError),
}

fn consume(
    node: &Node,
    r: &mut StringReader,
    args: &mut Vec<ParsedArg>,
) -> Result<(), ConsumeError> {
    match node.kind {
        Kind::Root => Ok(()),
        Kind::Literal => {
            let start = r.cursor;
            let word = r.read_token();
            if node.matches_literal(&word) {
                Ok(())
            } else {
                r.cursor = start;
                Err(ConsumeError::NoMatch)
            }
        }
        Kind::Argument(kind) => {
            if !r.can_read() {
                return Err(ConsumeError::NoMatch);
            }
            match kind.parse(r) {
                Ok((value, text)) => {
                    args.push(ParsedArg { value, text });
                    Ok(())
                }
                Err(e) => Err(ConsumeError::Parse(e)),
            }
        }
    }
}

/// Run `text` against the tree. `None` means ordinary chat (no leading `/`).
///
/// The lines-only view of [`execute_feedback`], for tests and callers that do
/// not care about the success/error distinction. The server's connection path
/// uses [`execute_feedback`] so the feedback it delivers can be styled and
/// gated.
pub fn execute(
    text: &str,
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Option<Reply> {
    execute_feedback(text, handle, registry, world).map(|f| Reply(f.lines))
}

/// Run `text` against the tree, keeping whether it succeeded or failed.
///
/// `None` means ordinary chat (no leading `/`). A parse error or a permission
/// denial is an [`Feedback::error`]; a body's output is a success even when it
/// is empty (self-`/kill` is not an error).
pub fn execute_feedback(
    text: &str,
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Option<Feedback> {
    let trimmed = text.trim();
    let body = trimmed.strip_prefix('/')?;
    match tree().parse(body, handle) {
        Ok(m) => {
            let mut ctx = Ctx {
                handle,
                registry,
                world,
                econ: crate::economy::get(),
                args: m.args,
            };
            let reply = m.node.run(&mut ctx).unwrap_or_else(|| Reply(Vec::new()));
            Some(Feedback::success(reply.0))
        }
        Err(ParseError::Denied(m)) => Some(Feedback::error(vec![m])),
        Err(e) => {
            // A name the tree does not own might belong to a plugin; the
            // plugin path is the only other place a verb can be answered, and
            // it refuses every core name, so the core gate is never bypassed.
            if let Some(reply) = super::plugin_command(body, handle) {
                return Some(Feedback::success(reply.0));
            }
            Some(Feedback::error(vec![e.message()]))
        }
    }
}

/// A byte-and-char reader over a command line, the way Brigadier's
/// `StringReader` works but on `&str`.
pub struct StringReader<'a> {
    text: &'a str,
    /// Byte cursor into `text`.
    pub cursor: usize,
}

impl<'a> StringReader<'a> {
    /// Wrap a line.
    pub fn new(text: &'a str) -> StringReader<'a> {
        StringReader { text, cursor: 0 }
    }

    fn can_read(&self) -> bool {
        self.cursor < self.text.len()
    }

    fn peek(&self) -> Option<char> {
        self.text[self.cursor..].chars().next()
    }

    fn read(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.cursor += c.len_utf8();
        Some(c)
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if !c.is_whitespace() {
                break;
            }
            self.cursor += c.len_utf8();
        }
    }

    /// One unquoted token: up to the next whitespace, quote characters taken
    /// literally. This is what literal matching and the plain `string`
    /// arguments read, and `split_whitespace` is its word-splitting peer.
    fn read_token(&mut self) -> String {
        let start = self.cursor;
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                break;
            }
            self.read();
        }
        self.text[start..self.cursor].to_string()
    }

    /// A `string` argument: a quoted string if it opens with `"`, else a
    /// token. An unterminated quote is an error.
    fn read_string_arg(&mut self) -> Result<String, ParseError> {
        if self.peek() == Some('"') {
            self.read_quoted()
        } else {
            Ok(self.read_token())
        }
    }

    /// The rest of the line. A leading quote is read as a quoted string, the
    /// remainder is appended verbatim (so a trailing slash survives). An
    /// unterminated quote is an error, not an empty argument.
    fn read_remainder(&mut self) -> Result<String, ParseError> {
        if self.peek() == Some('"') {
            let quoted = self.read_quoted()?;
            let rest = &self.text[self.cursor..];
            self.cursor = self.text.len();
            Ok(format!("{quoted}{rest}"))
        } else {
            let rest = self.text[self.cursor..].to_string();
            self.cursor = self.text.len();
            Ok(rest)
        }
    }

    fn read_quoted(&mut self) -> Result<String, ParseError> {
        self.read(); // opening quote
        let mut out = String::new();
        loop {
            match self.read() {
                None => return Err(ParseError::InvalidQuoted),
                Some('"') => return Ok(out),
                Some('\\') => match self.read() {
                    Some(c) => out.push(c),
                    None => return Err(ParseError::InvalidQuoted),
                },
                Some(c) => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests;
