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
//! The tree is generated from the same [`crate::commands::tree`] the server
//! parses against, so the declared grammar cannot drift from what the
//! dispatcher answers. The shape is per-release, because the packet is:
//!
//! * **1.13 – 1.18** name a parser by identifier (`brigadier:string`); this
//!   build's four [`Parsers::ByName`] codecs are 1.17 – 1.18.2, since the
//!   1.13 – 1.16 releases are not spoken here.
//! * **1.19+** name it by its index into the argument-type registry, and that
//!   registry is *not* the same list in every release: `minecraft:gamemode`
//!   arrived in 1.19.3, `minecraft:style` in 1.20.3, `minecraft:item_slots`
//!   in 1.20.5, `minecraft:resource_selector` in 1.21.5, `minecraft:hex_color`
//!   and `minecraft:dialog` in 1.21.6. Each [`ParserRegistry`] below is that
//!   release's list in id order, read from PrismarineJS `minecraft-data`'s
//!   `command_node` table.
//! * **1.8.9** predates the packet entirely and sends nothing; see the 1.8
//!   codec.
//!
//! The property field after a parser also depends on the argument type: a
//! string carries a mode VarInt, a number a bounds flags byte, an entity and a
//! score holder a flags byte, `minecraft:time` an `i32` minimum from 1.19.4 on,
//! and everything else carries nothing at all. Writing a property for a type
//! that has none — as the first, shallow version of this serializer did — puts
//! a stray byte in front of the next node's flags and the client misparses the
//! whole tree.

use crate::commands::tree::{
    tree, wire_nodes, ArgProperties, CommandTree, NODE_ARGUMENT, NODE_LITERAL,
};
use crate::proto::PacketOut;

/// The registry id of `brigadier:string`, present in every `ById` release and
/// the safe fallback for a kind whose parser a release does not register (a
/// `minecraft:gamemode` argument on 1.19, say).
const STRING_ID: i32 = 5;

/// One release's argument-type registry: the parser names in id order, exactly
/// as `minecraft-data`'s `command_node` lists them.
pub struct ParserRegistry {
    /// The names, index = wire id.
    pub names: &'static [&'static str],
    /// Whether `minecraft:time` writes an `i32` minimum. 1.19.4 added it, and
    /// before that the parser took no properties at all.
    pub time_min: bool,
}

impl ParserRegistry {
    /// Whether this release registers `name` at all.
    pub fn has(&self, name: &str) -> bool {
        self.names.contains(&name)
    }

    /// The id of `name`, or [`STRING_ID`] when this release does not register
    /// it. The fallback is deliberate: the client then reads a word rather
    /// than a parser it does not know, which would disconnect it.
    pub fn id(&self, name: &str) -> i32 {
        self.names
            .iter()
            .position(|n| *n == name)
            .map(|i| i as i32)
            .unwrap_or(STRING_ID)
    }
}

/// 1.19.
pub static PARSERS_1_19: ParserRegistry = ParserRegistry {
    time_min: false,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:resource_location",
        "minecraft:mob_effect",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:item_enchantment",
        "minecraft:entity_summon",
        "minecraft:dimension",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:uuid",
    ],
};

/// 1.19.3 (`minecraft:gamemode` arrives).
pub static PARSERS_1_19_3: ParserRegistry = ParserRegistry {
    time_min: false,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:uuid",
    ],
};

/// 1.19.4 – 1.20.2 (`minecraft:heightmap` and the time minimum arrive).
pub static PARSERS_1_19_4: ParserRegistry = ParserRegistry {
    time_min: true,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:heightmap",
        "minecraft:uuid",
    ],
};

/// 1.20.3/1.20.4 (`minecraft:style` arrives).
pub static PARSERS_1_20_3: ParserRegistry = ParserRegistry {
    time_min: true,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:style",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:heightmap",
        "minecraft:uuid",
    ],
};

/// 1.20.5 – 1.21.4 (`minecraft:item_slots`, then the loot parsers, arrive).
pub static PARSERS_1_20_5: ParserRegistry = ParserRegistry {
    time_min: true,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:style",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:item_slots",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:heightmap",
        "minecraft:loot_table",
        "minecraft:loot_predicate",
        "minecraft:loot_modifier",
        "minecraft:uuid",
    ],
};

/// 1.21.5 (`minecraft:resource_selector` arrives).
pub static PARSERS_1_21_5: ParserRegistry = ParserRegistry {
    time_min: true,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:component",
        "minecraft:style",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:item_slots",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:resource_selector",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:heightmap",
        "minecraft:loot_table",
        "minecraft:loot_predicate",
        "minecraft:loot_modifier",
        "minecraft:uuid",
    ],
};

/// 1.21.6 on, the current shape (`minecraft:hex_color` and `minecraft:dialog`
/// arrive; 26.x still name parsers this way).
pub static PARSERS_1_21_6: ParserRegistry = ParserRegistry {
    time_min: true,
    names: &[
        "brigadier:bool",
        "brigadier:float",
        "brigadier:double",
        "brigadier:integer",
        "brigadier:long",
        "brigadier:string",
        "minecraft:entity",
        "minecraft:game_profile",
        "minecraft:block_pos",
        "minecraft:column_pos",
        "minecraft:vec3",
        "minecraft:vec2",
        "minecraft:block_state",
        "minecraft:block_predicate",
        "minecraft:item_stack",
        "minecraft:item_predicate",
        "minecraft:color",
        "minecraft:hex_color",
        "minecraft:component",
        "minecraft:style",
        "minecraft:message",
        "minecraft:nbt",
        "minecraft:nbt_tag",
        "minecraft:nbt_path",
        "minecraft:objective",
        "minecraft:objective_criteria",
        "minecraft:operation",
        "minecraft:particle",
        "minecraft:angle",
        "minecraft:rotation",
        "minecraft:scoreboard_slot",
        "minecraft:score_holder",
        "minecraft:swizzle",
        "minecraft:team",
        "minecraft:item_slot",
        "minecraft:item_slots",
        "minecraft:resource_location",
        "minecraft:function",
        "minecraft:entity_anchor",
        "minecraft:int_range",
        "minecraft:float_range",
        "minecraft:dimension",
        "minecraft:gamemode",
        "minecraft:time",
        "minecraft:resource_or_tag",
        "minecraft:resource_or_tag_key",
        "minecraft:resource",
        "minecraft:resource_key",
        "minecraft:resource_selector",
        "minecraft:template_mirror",
        "minecraft:template_rotation",
        "minecraft:heightmap",
        "minecraft:loot_table",
        "minecraft:loot_predicate",
        "minecraft:loot_modifier",
        "minecraft:dialog",
        "minecraft:uuid",
    ],
};

/// How this version names a parser.
#[derive(Clone, Copy)]
pub enum Parsers {
    /// Before 1.19: an identifier string, `brigadier:string`.
    ByName,
    /// From 1.19 on: an index into this release's registry.
    ById(&'static ParserRegistry),
}

impl Parsers {
    /// Whether `minecraft:time` writes its `i32` minimum on this version.
    fn time_min(self) -> bool {
        match self {
            Parsers::ByName => false,
            Parsers::ById(registry) => registry.time_min,
        }
    }
}

/// Write the body of the "Declare Commands" packet: the node array and the
/// index of the root.
pub fn write_tree(p: &mut PacketOut, parsers: Parsers) {
    write_tree_for(p, tree(), parsers);
}

/// Write a given tree for the wire. [`write_tree`] passes the server's one
/// tree; tests drive a synthetic one through the same path.
fn write_tree_for(p: &mut PacketOut, t: &CommandTree, parsers: Parsers) {
    let (nodes, root) = wire_nodes(t);
    p.var_int(nodes.len() as i32);

    for node in &nodes {
        p.u8(node.flags);
        p.var_int(node.children.len() as i32);
        for child in &node.children {
            p.var_int(*child as i32);
        }
        // Only a literal (1) or an argument (2) carries a name; a root (0)
        // writes none. The name is gated on the node type, exactly as
        // `command_node` gates it, or the client reads the next node's flags
        // as a string and the whole tree is misparsed.
        if node.flags & (NODE_LITERAL | NODE_ARGUMENT) != 0 {
            p.string(node.name.unwrap_or(""));
        }
        let Some(kind) = node.parser else {
            continue;
        };
        // The parser and its properties are one decision: a kind that falls
        // back to the string parser (a release that does not register it, or a
        // kind the tree cannot express yet) must also write the string
        // parser's properties, or the client reads the wrong bytes after it.
        let properties = match parsers {
            Parsers::ByName => {
                p.string(kind.parser_name());
                kind.properties()
            }
            Parsers::ById(registry) => {
                if registry.has(kind.registry_name()) {
                    p.var_int(registry.id(kind.registry_name()));
                    kind.properties()
                } else {
                    p.var_int(STRING_ID);
                    ArgProperties::VarInt(0)
                }
            }
        };
        match properties {
            ArgProperties::None => {}
            ArgProperties::VarInt(mode) => {
                p.var_int(mode);
            }
            ArgProperties::Flags(flags) => {
                p.u8(flags);
            }
            ArgProperties::Time => {
                // No field at all on the releases before 1.19.4.
                if parsers.time_min() {
                    p.i32(0);
                }
            }
        }
    }
    p.var_int(root as i32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tree::{ArgKind, Builder, Kind, Node, Requirement, NODE_EXECUTABLE};
    use crate::commands::Reply;
    use crate::proto::PacketIn;

    /// A decoded wire node: flags, children and name/parser.
    struct Decoded {
        flags: u8,
        children: Vec<i32>,
        name: Option<String>,
        parser: Option<(String, i32)>,
        properties: Vec<i32>,
    }

    /// The bytes the property field of `name` occupies, as the serializer
    /// writes them. Independent of the serializer's own `ArgKind` mapping, so
    /// a node that writes the wrong number of bytes fails the round trip.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Prop {
        None,
        VarInt,
        Byte,
        Time,
    }

    fn prop_shape(name: &str, time_min: bool) -> Prop {
        match name {
            "brigadier:string" => Prop::VarInt,
            "brigadier:integer"
            | "brigadier:long"
            | "brigadier:float"
            | "brigadier:double"
            | "minecraft:entity"
            | "minecraft:score_holder" => Prop::Byte,
            "minecraft:time" if time_min => Prop::Time,
            _ => Prop::None,
        }
    }

    /// Decode the tree back the way a client does.
    fn decode(parsers: Parsers) -> (Vec<Decoded>, i32) {
        let mut p = PacketOut::new(0x10);
        write_tree(&mut p, parsers);
        let mut wire = Vec::new();
        p.write_to(&mut wire, None).unwrap();
        let mut pin = PacketIn::new(&wire);
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
                        Parsers::ById(registry) => {
                            let id = pin.var_int().unwrap();
                            (registry.names[id as usize].to_string(), id)
                        }
                    };
                    (Some(name), Some(parser))
                }
                _ => panic!("no node type 3 exists"),
            };
            let mut properties = Vec::new();
            if let Some((parser_name, _)) = &parser {
                match prop_shape(parser_name, parsers.time_min()) {
                    Prop::None => {}
                    Prop::VarInt => properties.push(pin.var_int().unwrap()),
                    Prop::Byte => properties.push(pin.u8().unwrap() as i32),
                    Prop::Time => properties.push(pin.i32().unwrap()),
                }
            }
            nodes.push(Decoded {
                flags,
                children,
                name,
                parser,
                properties,
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
        write_tree(&mut p, Parsers::ById(&PARSERS_1_21_6));
        let mut wire = Vec::new();
        p.write_to(&mut wire, None).unwrap();
        let mut pin = PacketIn::new(&wire);
        let _len = pin.var_int().unwrap();
        let _id = pin.var_int().unwrap();
        let _count = pin.var_int().unwrap();
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
        for parsers in [Parsers::ByName, Parsers::ById(&PARSERS_1_21_6)] {
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
    fn an_alias_shares_its_commands_children_and_is_executable() {
        for parsers in [Parsers::ByName, Parsers::ById(&PARSERS_1_19)] {
            let (nodes, _) = decode(parsers);
            let index = |want: &str| {
                *nodes[0]
                    .children
                    .iter()
                    .find(|&&i| nodes[i as usize].name.as_deref() == Some(want))
                    .unwrap_or_else(|| panic!("{want} declared")) as usize
            };
            let canonical = &nodes[index("gamemode")];
            let alias = &nodes[index("gm")];
            assert_eq!(canonical.children, alias.children, "same subtree");
            assert_ne!(canonical.flags & NODE_EXECUTABLE, 0);
            assert_ne!(alias.flags & NODE_EXECUTABLE, 0, "the alias runs too");
        }
    }

    #[test]
    fn a_command_that_takes_arguments_has_an_argument_child_and_still_runs_alone() {
        let (nodes, _) = decode(Parsers::ById(&PARSERS_1_21_6));
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
        assert_eq!(name, "brigadier:string");
        assert_eq!(child.properties, vec![2], "greedy string mode");

        let stash = find("stash");
        assert_eq!(stash.flags & 0x04, 0x04);
        assert!(stash.children.is_empty(), "/stash takes nothing");
    }

    #[test]
    fn requirement_gated_nodes_are_still_declared() {
        // The client decides visibility from the tree; the server enforces on
        // execution. `/stop` is op-4, and it is a declared, executable literal.
        let (nodes, _) = decode(Parsers::ById(&PARSERS_1_21_6));
        let stop = nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("stop"))
            .expect("/stop declared");
        assert_eq!(stop.flags & 0x03, 0x01);
        assert_ne!(stop.flags & NODE_EXECUTABLE, 0, "/stop executable");
    }

    #[test]
    fn every_declared_name_is_answered_by_the_tree() {
        // The tree is a promise: a declared name with no executor is a
        // completion that leads nowhere. Every root literal — canonical name
        // and alias alike — must resolve to a command node that runs.
        let (nodes, _) = decode(Parsers::ById(&PARSERS_1_21_6));
        for &i in &nodes[0].children {
            let name = nodes[i as usize].name.as_deref().unwrap();
            let command = tree()
                .commands()
                .iter()
                .find(|c| c.name == name || c.aliases.contains(&name));
            let command =
                command.unwrap_or_else(|| panic!("`{name}` is declared but the tree has no node"));
            assert!(
                command.executor.is_some(),
                "`{name}` is declared but not executable"
            );
            assert!(
                nodes[i as usize].flags & NODE_EXECUTABLE != 0,
                "`{name}` is declared but not marked executable"
            );
        }
    }

    #[test]
    fn every_stub_registry_name_is_declared() {
        // CMD0b: every specified-but-unimplemented core command is on the tree
        // (and so on the wire) from day one, so completion and permissions are
        // right before the command's own CMD task lands.
        let (nodes, _) = decode(Parsers::ById(&PARSERS_1_21_6));
        for entry in crate::commands::registry::CORE {
            assert!(
                nodes[0]
                    .children
                    .iter()
                    .any(|&i| nodes[i as usize].name.as_deref() == Some(entry.name)),
                "`/{}` ({}) is in the stub registry but not declared",
                entry.name,
                entry.task
            );
        }
    }

    #[test]
    fn the_parser_ids_match_minecraft_data() {
        // The ids a release's client expects, read from `minecraft-data`'s
        // `command_node` table. A wrong id is silent: the client parses the
        // node as a different argument type and only colours it wrongly.
        struct Case {
            registry: &'static ParserRegistry,
            kind: ArgKind,
            id: i32,
        }
        let cases = [
            // 1.19 has no `minecraft:gamemode`; it falls back to string.
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::String,
                id: 5,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::Entity,
                id: 6,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::Message,
                id: 18,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::NbtCompound,
                id: 19,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::NbtPath,
                id: 21,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::Uuid,
                id: 47,
            },
            Case {
                registry: &PARSERS_1_19,
                kind: ArgKind::Gamemode,
                id: 5,
            },
            // 1.19.3 adds gamemode at 39.
            Case {
                registry: &PARSERS_1_19_3,
                kind: ArgKind::Gamemode,
                id: 39,
            },
            // 1.19.4 adds heightmap and shifts uuid.
            Case {
                registry: &PARSERS_1_19_4,
                kind: ArgKind::Gamemode,
                id: 39,
            },
            Case {
                registry: &PARSERS_1_19_4,
                kind: ArgKind::Uuid,
                id: 48,
            },
            // 1.20.3 inserts style, shifting message and everything after.
            Case {
                registry: &PARSERS_1_20_3,
                kind: ArgKind::Message,
                id: 19,
            },
            Case {
                registry: &PARSERS_1_20_3,
                kind: ArgKind::Gamemode,
                id: 40,
            },
            Case {
                registry: &PARSERS_1_20_3,
                kind: ArgKind::Time,
                id: 41,
            },
            Case {
                registry: &PARSERS_1_20_3,
                kind: ArgKind::Uuid,
                id: 49,
            },
            // 1.20.5 adds item_slots and the loot parsers.
            Case {
                registry: &PARSERS_1_20_5,
                kind: ArgKind::Gamemode,
                id: 41,
            },
            Case {
                registry: &PARSERS_1_20_5,
                kind: ArgKind::NbtCompound,
                id: 20,
            },
            Case {
                registry: &PARSERS_1_20_5,
                kind: ArgKind::Uuid,
                id: 53,
            },
            // 1.21.5 adds resource_selector.
            Case {
                registry: &PARSERS_1_21_5,
                kind: ArgKind::Uuid,
                id: 54,
            },
            // 1.21.6 adds hex_color and dialog; this is the 26.x shape too.
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::Component,
                id: 18,
            },
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::Message,
                id: 20,
            },
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::Gamemode,
                id: 42,
            },
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::Time,
                id: 43,
            },
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::NbtCompound,
                id: 21,
            },
            Case {
                registry: &PARSERS_1_21_6,
                kind: ArgKind::Uuid,
                id: 56,
            },
        ];
        for case in cases {
            let got = case.registry.id(case.kind.registry_name());
            assert_eq!(
                got,
                case.id,
                "{:?} in a {} parser registry should be id {}",
                case.kind,
                case.registry.names.len(),
                case.id
            );
        }
    }

    #[test]
    fn the_by_name_generation_uses_the_pre_1_19_identifiers() {
        assert_eq!(ArgKind::String.parser_name(), "brigadier:string");
        assert_eq!(ArgKind::Integer.parser_name(), "brigadier:integer");
        assert_eq!(ArgKind::Entity.parser_name(), "minecraft:entity");
        // The NBT compound tag was `nbt_compound_tag` before 1.19.
        assert_eq!(
            ArgKind::NbtCompound.parser_name(),
            "minecraft:nbt_compound_tag"
        );
        assert_eq!(ArgKind::NbtCompound.registry_name(), "minecraft:nbt");
        // Kinds with no wire parser fall back to a plain word.
        assert_eq!(ArgKind::Enum(&[]).parser_name(), "brigadier:string");
        assert_eq!(ArgKind::Difficulty.parser_name(), "brigadier:string");
    }

    #[test]
    fn every_argument_kind_round_trips_through_both_parser_modes() {
        // A synthetic tree with one literal per argument kind, so the
        // serializer is exercised for every parser, not just the greedy string
        // the real tree happens to use today.
        let noop = |_ctx: &mut crate::commands::tree::Ctx| Reply(Vec::new());
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
            ArgKind::Entity,
            ArgKind::Entities,
            ArgKind::BlockPos,
            ArgKind::Vec3(true),
            ArgKind::Vec2(true),
            ArgKind::BlockState,
            ArgKind::ItemStack,
            ArgKind::NbtCompound,
            ArgKind::NbtPath,
            ArgKind::Component,
            ArgKind::ResourceLocation,
            ArgKind::GameProfile,
            ArgKind::Time,
            ArgKind::Gamemode,
            ArgKind::Particle,
            ArgKind::ScoreHolder,
            ArgKind::Swizzle,
            ArgKind::Angle,
            ArgKind::Operation,
            ArgKind::Rotation,
            ArgKind::Uuid,
        ];
        let mut children = Vec::new();
        for (i, &kind) in kinds.iter().enumerate() {
            let name: &'static str = Box::leak(format!("kind{i}").into_boxed_str());
            children.push(
                Builder::literal(name)
                    .then(Builder::arg("a", kind).executable(noop))
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
                task: None,
                children,
            },
        };
        for parsers in [
            Parsers::ByName,
            Parsers::ById(&PARSERS_1_19),
            Parsers::ById(&PARSERS_1_19_4),
            Parsers::ById(&PARSERS_1_21_6),
        ] {
            // Encode this synthetic tree, not the global one.
            let mut p = PacketOut::new(0x10);
            write_tree_for(&mut p, &tree, parsers);
            let mut wire = Vec::new();
            p.write_to(&mut wire, None).unwrap();
            let mut pin = PacketIn::new(&wire);
            let _len = pin.var_int().unwrap();
            let _id = pin.var_int().unwrap();
            let count = pin.var_int().unwrap();
            let mut arguments = 0;
            for _ in 0..count {
                let flags = pin.u8().unwrap();
                for _ in 0..pin.var_int().unwrap() {
                    let _ = pin.var_int().unwrap();
                }
                match flags & 0x03 {
                    0 => {}
                    1 => {
                        let _ = pin.string().unwrap();
                    }
                    2 => {
                        let _ = pin.string().unwrap();
                        let parser_name = match parsers {
                            Parsers::ByName => pin.string().unwrap(),
                            Parsers::ById(registry) => {
                                let id = pin.var_int().unwrap();
                                registry.names[id as usize].to_string()
                            }
                        };
                        match prop_shape(&parser_name, parsers.time_min()) {
                            Prop::None => {}
                            Prop::VarInt => {
                                let _ = pin.var_int().unwrap();
                            }
                            Prop::Byte => {
                                let _ = pin.u8().unwrap();
                            }
                            Prop::Time => {
                                let _ = pin.i32().unwrap();
                            }
                        }
                        arguments += 1;
                    }
                    _ => panic!("bad node type"),
                }
            }
            let _root = pin.var_int().unwrap();
            assert_eq!(pin.remaining(), 0, "trailing bytes for {count} nodes");
            assert_eq!(arguments, kinds.len(), "not every argument was declared");
        }
    }

    #[test]
    fn the_whole_tree_fits_in_one_frame() {
        // A few hundred nodes is a few KB; the normal length prefix carries it
        // and no special handling is needed.
        for parsers in [Parsers::ByName, Parsers::ById(&PARSERS_1_21_6)] {
            let mut p = PacketOut::new(0x10);
            write_tree(&mut p, parsers);
            let mut wire = Vec::new();
            p.write_to(&mut wire, None).unwrap();
            assert!(wire.len() > 500, "the real tree is bigger than this");
            assert!(
                wire.len() < 0x20_0000,
                "a Declare Commands packet may not exceed 2^21-1 bytes"
            );
        }
    }
}
