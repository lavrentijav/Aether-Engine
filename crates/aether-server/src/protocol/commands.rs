//! The command tree the server declares to its clients.
//!
//! Since 1.13 the client keeps a local copy of the server's command grammar
//! and uses it for completion, for colouring, and for deciding where one
//! argument ends and the next begins. A server that never sends it leaves the
//! client with an empty tree: every slash command shows up red, nothing
//! completes, and the player has no way to discover what the server answers.
//! The commands still *run* — the client sends what was typed regardless — so
//! this is discoverability rather than function.
//!
//! The tree here is deliberately shallow. Every command is one literal node,
//! and the ones that take arguments get a single greedy string child rather
//! than a faithful per-argument grammar. That is enough for completion of the
//! command names — the part a player cannot guess — and it costs one node
//! each instead of a parser tree that would have to be kept in step with
//! every dispatcher by hand.

use crate::proto::PacketOut;

/// One command, and whether anything may follow its name.
pub struct Command {
    /// The word after the slash.
    pub name: &'static str,
    /// Whether it accepts arguments.
    pub args: bool,
}

const fn c(name: &'static str, args: bool) -> Command {
    Command { name, args }
}

/// Every command the dispatchers answer, aliases included.
///
/// Kept beside the dispatchers by a test rather than by discipline: see
/// [`tests::every_declared_command_is_answered`].
pub const COMMANDS: &[Command] = &[
    // crate::commands
    c("inspect", false),
    c("i", false),
    c("lookup", true),
    c("l", true),
    c("rollback", true),
    c("rb", true),
    c("page", true),
    c("pg", true),
    c("inv", false),
    c("inventory", false),
    c("stash", false),
    c("audit", false),
    c("cache", false),
    // crate::economy::commands
    c("balance", true),
    c("bal", true),
    c("money", true),
    c("pay", true),
    c("sell", true),
    c("market", true),
    c("buy", true),
    c("listings", false),
    c("unlist", true),
    c("econ", true),
    // crate::game::commands
    c("gamemode", true),
    c("gm", true),
    c("time", true),
    c("give", true),
    c("summon", true),
    c("killall", false),
    c("kill", false),
    c("spawn", false),
    c("tp", true),
    c("block", true),
    c("food", true),
    c("heal", false),
];

/// `brigadier:string`'s id in the parser registry.
///
/// Identical in every version from 1.19 to 1.21.11 — the first six parsers
/// have never been reordered. Checked against `minecraft-data` for all of
/// them before this was written down.
const PARSER_STRING: i32 = 5;

/// `brigadier:string`'s GREEDY_PHRASE mode: take the rest of the line.
const GREEDY_PHRASE: i32 = 2;

/// How this version names a parser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parsers {
    /// Before 1.19: an identifier string, `brigadier:string`.
    ByName,
    /// From 1.19 on: an index into the parser registry.
    ById,
}

/// Write the body of the "Declare Commands" packet: the node array and the
/// index of the root.
///
/// Node layout, which the indices below depend on:
///
/// ```text
/// 0            the root
/// 1 ..= n      one literal per command, in `COMMANDS` order
/// n+1 ..       one greedy-string argument per command that takes arguments
/// ```
pub fn write_tree(p: &mut PacketOut, parsers: Parsers) {
    let n = COMMANDS.len();
    let with_args = COMMANDS.iter().filter(|c| c.args).count();
    p.var_int((1 + n + with_args) as i32);

    // The root: type 0, not executable, every command as a child.
    p.u8(0x00).var_int(n as i32);
    for i in 0..n {
        p.var_int((1 + i) as i32);
    }

    // The literals. Executable on their own even when they take arguments —
    // `/inspect` and `/market` are both whole commands by themselves.
    let mut arg_index = 1 + n;
    for cmd in COMMANDS {
        p.u8(0x01 | 0x04); // literal, executable
        if cmd.args {
            p.var_int(1).var_int(arg_index as i32);
            arg_index += 1;
        } else {
            p.var_int(0);
        }
        p.string(cmd.name);
    }

    // The arguments: one greedy string each, swallowing the rest of the line.
    for _ in 0..with_args {
        p.u8(0x02 | 0x04); // argument, executable
        p.var_int(0); // no children
        p.string("args");
        match parsers {
            Parsers::ByName => {
                p.string("brigadier:string");
            }
            Parsers::ById => {
                p.var_int(PARSER_STRING);
            }
        }
        p.var_int(GREEDY_PHRASE);
    }

    p.var_int(0); // root index
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the tree back the way a client does.
    fn decode(parsers: Parsers) -> (Vec<(u8, Vec<i32>, Option<String>)>, i32) {
        let mut p = PacketOut::new(0x10);
        write_tree(&mut p, parsers);
        let mut wire = Vec::new();
        p.write_to(&mut wire, None).unwrap();
        let mut pin = crate::proto::PacketIn::new(&wire);
        let _len = pin.var_int().unwrap();
        let _id = pin.var_int().unwrap();
        let count = pin.var_int().unwrap();
        let mut nodes = Vec::new();
        for _ in 0..count {
            let flags = pin.u8().unwrap();
            let children = (0..pin.var_int().unwrap())
                .map(|_| pin.var_int().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(flags & 0x08, 0, "nothing here redirects");
            let name = match flags & 0x03 {
                0 => None,
                1 => Some(pin.string().unwrap()),
                2 => {
                    let name = pin.string().unwrap();
                    match parsers {
                        Parsers::ByName => {
                            assert_eq!(pin.string().unwrap(), "brigadier:string");
                        }
                        Parsers::ById => {
                            assert_eq!(pin.var_int().unwrap(), PARSER_STRING);
                        }
                    }
                    assert_eq!(pin.var_int().unwrap(), GREEDY_PHRASE);
                    Some(name)
                }
                _ => panic!("no node type 3 exists"),
            };
            nodes.push((flags, children, name));
        }
        let root = pin.var_int().unwrap();
        (nodes, root)
    }

    #[test]
    fn the_root_names_every_command_once() {
        for parsers in [Parsers::ByName, Parsers::ById] {
            let (nodes, root) = decode(parsers);
            assert_eq!(root, 0);
            let (flags, children, name) = &nodes[0];
            assert_eq!(flags & 0x03, 0, "the root is a root node");
            assert_eq!(*name, None);
            assert_eq!(children.len(), COMMANDS.len());
            let mut seen: Vec<&str> = Vec::new();
            for &i in children {
                let (f, _, n) = &nodes[i as usize];
                assert_eq!(f & 0x03, 1, "a root child is a literal");
                let n = n.as_deref().unwrap();
                assert!(!seen.contains(&n), "{n} declared twice");
                seen.push(n);
            }
            assert!(seen.contains(&"inspect") && seen.contains(&"pay"));
        }
    }

    #[test]
    fn a_command_that_takes_arguments_has_a_greedy_child_and_still_runs_alone() {
        let (nodes, _) = decode(Parsers::ById);
        let find = |want: &str| {
            nodes
                .iter()
                .find(|(f, _, n)| f & 0x03 == 1 && n.as_deref() == Some(want))
                .expect("declared")
        };
        let (flags, children, _) = find("lookup");
        assert_eq!(flags & 0x04, 0x04, "/lookup runs with no arguments too");
        assert_eq!(children.len(), 1);
        let (af, ac, an) = &nodes[children[0] as usize];
        assert_eq!(af & 0x03, 2, "the child is an argument");
        assert_eq!(af & 0x04, 0x04, "and is executable");
        assert!(ac.is_empty(), "greedy: nothing can follow it");
        assert_eq!(an.as_deref(), Some("args"));

        let (flags, children, _) = find("stash");
        assert_eq!(flags & 0x04, 0x04);
        assert!(children.is_empty(), "/stash takes nothing");
    }

    #[test]
    fn every_declared_command_is_answered() {
        // The tree is a promise. A name here that no dispatcher answers is a
        // completion that leads to "unknown command", which is worse than no
        // completion at all.
        let src = concat!(
            include_str!("../commands/mod.rs"),
            include_str!("../economy/commands.rs"),
            include_str!("../game/commands.rs"),
        );
        for cmd in COMMANDS {
            let quoted = format!("\"{}\"", cmd.name);
            assert!(
                src.contains(&quoted),
                "/{} is declared to clients but no dispatcher matches it",
                cmd.name
            );
        }
    }
}
