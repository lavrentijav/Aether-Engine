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
//! The tree is now generated from the same [`crate::commands::tree`] the
//! server parses against, so the declared grammar cannot drift from what the
//! dispatcher answers. Argument *types* are still declared as one string each:
//! the faithful per-version mapping is CMD-04, which replaces the parser
//! lookup [`crate::commands::tree::ArgKind::parser`] hands it.

use crate::commands::tree::{tree, CommandTree, Kind, Node};
use crate::proto::PacketOut;

/// How this version names a parser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parsers {
    /// Before 1.19: an identifier string, `brigadier:string`.
    ByName,
    /// From 1.19 on: an index into the parser registry.
    ById,
}

/// A node flattened for the wire: its flags, name, parser and child indices.
struct Flat {
    flags: u8,
    name: Option<String>,
    parser: Option<crate::commands::tree::ParserRef>,
    children: Vec<usize>,
}

/// Write the body of the "Declare Commands" packet: the node array and the
/// index of the root.
pub fn write_tree(p: &mut PacketOut, parsers: Parsers) {
    let (nodes, root) = flatten(tree());
    p.var_int(nodes.len() as i32);

    for node in &nodes {
        p.u8(node.flags);
        p.var_int(node.children.len() as i32);
        for child in &node.children {
            p.var_int(*child as i32);
        }
        // Only a literal (1) or an argument (2) carries a name; a root (0)
        // writes none. The name byte is gated on the node type, exactly as
        // `command_node` gates it, or the client reads the next node's flags
        // as a string and the whole tree is misparsed.
        if node.flags & 0x03 != 0 {
            p.string(node.name.as_deref().unwrap_or(""));
        }
        if let Some(parser) = &node.parser {
            match parsers {
                Parsers::ByName => {
                    p.string(parser.name);
                }
                Parsers::ById => {
                    p.var_int(parser.id);
                }
            }
            p.var_int(parser.properties);
        }
    }
    p.var_int(root as i32);
}

/// Flatten the tree into the packet's node array. The root is index 0; every
/// command name and alias is its own literal node over the same children.
fn flatten(t: &CommandTree) -> (Vec<Flat>, usize) {
    let mut nodes = vec![Flat {
        flags: 0x00,
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
            nodes.push(Flat {
                flags: 0x01 | if command.executable { 0x04 } else { 0 },
                name: Some(name.to_string()),
                parser: None,
                children: children.clone(),
            });
            root_children.push(idx);
        }
    }
    nodes[0].children = root_children;
    (nodes, 0)
}

fn flatten_node(nodes: &mut Vec<Flat>, node: &Node) -> usize {
    let children: Vec<usize> = node
        .children
        .iter()
        .map(|child| flatten_node(nodes, child))
        .collect();
    let executable = if node.executable { 0x04 } else { 0 };
    let (flags, parser) = match &node.kind {
        Kind::Argument(kind) => (0x02 | executable, Some(kind.parser())),
        _ => (0x01 | executable, None),
    };
    let idx = nodes.len();
    nodes.push(Flat {
        flags,
        name: Some(node.name.to_string()),
        parser,
        children,
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoded wire node: flags, children and name/parser.
    struct Decoded {
        flags: u8,
        children: Vec<i32>,
        name: Option<String>,
        parser: Option<(String, i32)>,
    }

    /// Decode the tree back the way a client does.
    fn decode(parsers: Parsers) -> (Vec<Decoded>, i32) {
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
            // Only a literal or an argument carries a name — a root writes
            // none. Reading one unconditionally is what let the root-extra-byte
            // regression through before.
            let (name, parser) = match flags & 0x03 {
                0 => (None, None),
                1 => (Some(pin.string().unwrap()), None),
                2 => {
                    let name = pin.string().unwrap();
                    let parser = match parsers {
                        Parsers::ByName => (pin.string().unwrap(), -1),
                        Parsers::ById => (String::new(), pin.var_int().unwrap()),
                    };
                    let _properties = pin.var_int().unwrap();
                    (Some(name), Some(parser))
                }
                _ => panic!("no node type 3 exists"),
            };
            nodes.push(Decoded {
                flags,
                children,
                name,
                parser,
            });
        }
        let root = pin.var_int().unwrap();
        // The packet ends here: a leftover byte means a node wrote more than
        // the format allows.
        assert_eq!(
            pin.remaining(),
            0,
            "trailing bytes after the root index — a node wrote too much"
        );
        (nodes, root)
    }

    #[test]
    fn the_root_node_writes_no_name() {
        // The exact regression the first review caught: the root must write
        // flags, children and nothing else. Decode raw and check that the byte
        // after the root's children is the next node's flags, not a name.
        let mut p = PacketOut::new(0x10);
        write_tree(&mut p, Parsers::ById);
        let mut wire = Vec::new();
        p.write_to(&mut wire, None).unwrap();
        let mut pin = crate::proto::PacketIn::new(&wire);
        let _len = pin.var_int().unwrap();
        let _id = pin.var_int().unwrap();
        let count = pin.var_int().unwrap();
        // Node 0 is the root.
        let root_flags = pin.u8().unwrap();
        assert_eq!(root_flags, 0x00);
        let children = pin.var_int().unwrap();
        for _ in 0..children {
            let _ = pin.var_int().unwrap();
        }
        // Node 1 begins immediately: a literal's flags.
        let next = pin.u8().unwrap();
        assert_eq!(next & 0x03, 1, "expected the next node's literal flags");
    }

    #[test]
    fn the_root_names_every_command_and_alias_once() {
        for parsers in [Parsers::ByName, Parsers::ById] {
            let (nodes, root) = decode(parsers);
            assert_eq!(root, 0);
            let mut seen: Vec<&str> = Vec::new();
            for &i in &nodes[0].children {
                let node = &nodes[i as usize];
                assert_eq!(node.flags & 0x03, 1, "a root child is a literal");
                let name = node.name.as_deref().unwrap();
                assert!(!seen.contains(&name), "{name} declared twice");
                seen.push(name);
            }
            // A name and one of its aliases both point at the same executor.
            assert!(seen.contains(&"inspect") && seen.contains(&"i"));
            assert!(seen.contains(&"pay"));
            assert!(seen.contains(&"gamemode") && seen.contains(&"gm"));
        }
    }

    #[test]
    fn a_command_that_takes_arguments_has_an_argument_child_and_still_runs_alone() {
        let (nodes, _) = decode(Parsers::ById);
        let find = |want: &str| {
            nodes
                .iter()
                .find(|n| n.flags & 0x03 == 1 && n.name.as_deref() == Some(want))
                .expect("declared")
        };
        let give = find("give");
        assert_eq!(give.flags & 0x04, 0x04, "/give runs bare too");
        assert_eq!(give.children.len(), 1);
        let child = &nodes[give.children[0] as usize];
        assert_eq!(child.flags & 0x03, 2, "the child is an argument");
        assert_eq!(child.flags & 0x04, 0x04, "and is executable");
        assert!(child.children.is_empty(), "greedy: nothing follows");
        let (name, id) = child.parser.clone().unwrap();
        assert_eq!(id, 5, "brigadier:string");
        assert!(name.is_empty() || name == "brigadier:string");

        let stash = find("stash");
        assert_eq!(stash.flags & 0x04, 0x04);
        assert!(stash.children.is_empty(), "/stash takes nothing");
    }

    #[test]
    fn every_declared_command_is_answered_by_the_tree() {
        // The tree is a promise: a declared name with no executor is a
        // completion that leads nowhere.
        for command in tree().commands() {
            assert!(
                command.executable && command.executor.is_some(),
                "/{} is declared but not executable",
                command.name
            );
        }
    }
}
