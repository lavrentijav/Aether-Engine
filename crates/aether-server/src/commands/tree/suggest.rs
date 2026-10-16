//! Suggestions: walk the partially-typed line to the deepest matching node,
//! then offer its literal children (prefix-filtered) and its argument type's
//! own candidates. Cheap by construction — it reads the shared tree and the
//! online-player list, and takes no lock on the world.

use super::{check, tree, ArgKind, CommandTree, Kind, Source, StringReader};
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;

/// The read-only world and registry a provider may consult.
pub struct SuggestCtx<'a> {
    /// Online players, for name completion.
    pub registry: &'a SharedRegistry,
    /// The world, for block/item completion (used by later CMD tasks). `None`
    /// in tests that only exercise literal completion.
    pub world: Option<&'a DemoWorld>,
}

/// Suggest completions for `text` to `handle`. The result is the set of words
/// that may replace the token currently being typed.
pub fn suggest(text: &str, handle: &PlayerHandle, ctx: &SuggestCtx) -> Vec<String> {
    suggest_in(tree(), text, handle, ctx)
}

/// The engine behind [`suggest`], against a given tree so tests can drive a
/// small synthetic one.
pub fn suggest_in(
    tree: &CommandTree,
    text: &str,
    handle: &PlayerHandle,
    ctx: &SuggestCtx,
) -> Vec<String> {
    let text = text.trim();
    let text = text.strip_prefix('/').unwrap_or(text);
    let mut r = StringReader::new(text);
    let mut node = &tree.root;
    loop {
        r.skip_whitespace();
        if !r.can_read() {
            return children(node, "", handle, ctx);
        }
        let start = r.cursor;
        let word = r.read_token();
        if let Some(child) = node.children.iter().find(|c| c.matches_literal(&word)) {
            if check(&child.requirement, &Source::Player(handle)).is_ok() {
                node = child;
                continue;
            }
            // A node the caller cannot see is not suggested at all.
            return Vec::new();
        }
        return children(node, &text[start..], handle, ctx);
    }
}

/// The completions offered at `node` for a token starting with `partial`.
fn children(
    node: &super::Node,
    partial: &str,
    handle: &PlayerHandle,
    ctx: &SuggestCtx,
) -> Vec<String> {
    let mut out = Vec::new();
    for child in &node.children {
        if check(&child.requirement, &Source::Player(handle)).is_err() {
            continue;
        }
        match &child.kind {
            Kind::Literal => {
                if child.name.starts_with(partial) {
                    out.push(child.name.to_string());
                }
                for alias in &child.aliases {
                    if alias.starts_with(partial) {
                        out.push((*alias).to_string());
                    }
                }
            }
            Kind::Argument(kind) => out.extend(argument(*kind, partial, ctx)),
            Kind::Root => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A type's candidates: its fixed words, or the online names for the
/// player-shaped kinds. The block and item kinds read the vanilla tables, so
/// completion works before any world exists; the coordinate kinds offer the
/// vanilla `~` / `^` shapes.
fn argument(kind: ArgKind, partial: &str, ctx: &SuggestCtx) -> Vec<String> {
    let words: Vec<&str> = match kind {
        ArgKind::Enum(w) => w.to_vec(),
        ArgKind::Bool => vec!["true", "false"],
        ArgKind::Gamemode => vec!["survival", "creative", "adventure", "spectator"],
        ArgKind::Difficulty => vec!["peaceful", "easy", "normal", "hard"],
        ArgKind::Operation => vec!["=", "+=", "-=", "*=", "/=", "%=", "<", ">", "><"],
        ArgKind::Swizzle => vec!["x", "y", "z", "xy", "xz", "yz", "xyz"],
        ArgKind::Time => vec!["1t", "1s", "1d"],
        ArgKind::BlockState => aether_world::registry::blocks::BLOCKS
            .iter()
            .map(|row| row.0)
            .collect(),
        ArgKind::ItemStack => aether_world::registry::items::ITEM_NAMES.to_vec(),
        ArgKind::ResourceLocation => vec!["minecraft:"],
        ArgKind::ResourceOrTag => vec!["#minecraft:"],
        ArgKind::BlockPos | ArgKind::Vec3(_) => vec!["~ ~ ~", "^ ^ ^"],
        ArgKind::Vec2(_) => vec!["~ ~", "^ ^"],
        ArgKind::Rotation => vec!["~ ~"],
        ArgKind::Angle => vec!["~"],
        ArgKind::Entity | ArgKind::Entities | ArgKind::GameProfile | ArgKind::ScoreHolder => {
            return ctx
                .registry
                .snapshot()
                .iter()
                .filter(|p| p.name.starts_with(partial))
                .map(|p| p.name.clone())
                .collect();
        }
        _ => return Vec::new(),
    };
    words
        .into_iter()
        .filter(|w| w.starts_with(partial))
        .map(|w| w.to_string())
        .collect()
}
