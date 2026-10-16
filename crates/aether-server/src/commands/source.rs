//! Who issued a command, and therefore where its feedback is allowed to go.
//!
//! The executor itself is the same whatever the source: it reads the parsed
//! arguments, changes the world, and returns a [`Reply`](super::Reply). What
//! differs is the *policy* around that reply — whether it reaches the source,
//! and under whose permission the command was allowed to run. Putting the
//! source in one type keeps that policy out of every command body.
//!
//! Today every live dispatch is a player, so [`CommandSource::Player`] is the
//! only variant the server constructs. Console and RCON arrive with CMD-07 and
//! command blocks and functions with CMD-21/CMD-22; the variants and the
//! feedback rule they feed ([`super::feedback::emits`]) are defined now so
//! those tasks only have to construct the source, not re-decide the rule.

use crate::players::PlayerHandle;

/// The kind of source, without borrows — what the feedback policy keys on.
///
/// Kept separate from [`CommandSource`] so a test (or a later task) can reason
/// about a source it has no handle for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A connected player.
    Player,
    /// The server console.
    Console,
    /// A remote-console connection.
    Rcon,
    /// A command block.
    CommandBlock,
    /// A datapack function.
    Function,
}

impl SourceKind {
    /// Whether vanilla always shows this source its own command feedback.
    ///
    /// Players and the console always see it — a player who cannot see the
    /// result of their own command is being lied to. Command blocks and
    /// functions are only shown it when the `sendCommandFeedback` game rule is
    /// on, because their output is a log/observability concern rather than
    /// something a person is watching. See [`super::feedback::emits`].
    pub fn always_receives_feedback(self) -> bool {
        matches!(
            self,
            SourceKind::Player | SourceKind::Console | SourceKind::Rcon
        )
    }
}

/// Where a command came from.
///
/// Borrows the player for a [`CommandSource::Player`]; the non-player variants
/// carry only the little identity they need (a block position, a function
/// name). Not `Debug`: the player handle it may borrow is a live lock-holding
/// value, not something to print.
#[derive(Clone, Copy)]
pub enum CommandSource<'a> {
    /// A connected player.
    Player(&'a PlayerHandle),
    /// The server console.
    Console,
    /// A remote-console connection.
    Rcon,
    /// A command block at `(x, y, z)`.
    CommandBlock { x: i32, y: i32, z: i32 },
    /// A datapack function called `name`.
    Function { name: &'a str },
}

impl<'a> CommandSource<'a> {
    /// The source's kind, for the feedback policy.
    pub fn kind(&self) -> SourceKind {
        match self {
            CommandSource::Player(_) => SourceKind::Player,
            CommandSource::Console => SourceKind::Console,
            CommandSource::Rcon => SourceKind::Rcon,
            CommandSource::CommandBlock { .. } => SourceKind::CommandBlock,
            CommandSource::Function { .. } => SourceKind::Function,
        }
    }

    /// The player behind this source, when there is one.
    pub fn handle(&self) -> Option<&'a PlayerHandle> {
        match self {
            CommandSource::Player(h) => Some(h),
            _ => None,
        }
    }

    /// A short name for logs and messages.
    pub fn name(&self) -> String {
        match self {
            CommandSource::Player(h) => h.name.clone(),
            CommandSource::Console => "CONSOLE".into(),
            CommandSource::Rcon => "RCON".into(),
            CommandSource::CommandBlock { x, y, z } => format!("command block ({x}, {y}, {z})"),
            CommandSource::Function { name } => format!("function {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_humans_and_consoles_always_see_feedback() {
        assert!(SourceKind::Player.always_receives_feedback());
        assert!(SourceKind::Console.always_receives_feedback());
        assert!(SourceKind::Rcon.always_receives_feedback());
        assert!(!SourceKind::CommandBlock.always_receives_feedback());
        assert!(!SourceKind::Function.always_receives_feedback());
    }

    #[test]
    fn the_non_player_sources_name_themselves() {
        assert_eq!(CommandSource::Console.name(), "CONSOLE");
        assert_eq!(CommandSource::Rcon.name(), "RCON");
        assert_eq!(
            CommandSource::CommandBlock { x: 1, y: 64, z: -2 }.name(),
            "command block (1, 64, -2)"
        );
        assert_eq!(
            CommandSource::Function { name: "tick" }.name(),
            "function tick"
        );
        assert_eq!(CommandSource::Console.kind(), SourceKind::Console);
        assert!(CommandSource::Console.handle().is_none());
    }
}
