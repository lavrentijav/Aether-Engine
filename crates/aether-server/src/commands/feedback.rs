//! The single path command output takes back to its source.
//!
//! Every command body returns a [`Reply`](super::Reply); the dispatcher wraps
//! it in a [`Feedback`] that records whether the command failed, and the caller
//! hands that to [`send`]. Doing it here, once, is what lets three things be
//! true in one place rather than at every call site:
//!
//! * a command that *succeeds without a message* emits nothing (an empty
//!   `Feedback`),
//! * an error is coloured the way vanilla colours it (red) and success is
//!   plain,
//! * the `sendCommandFeedback` game rule is honoured — and honoured *as
//!   vanilla does it*: it silences command blocks and functions only, never a
//!   player's own feedback.
//!
//! Success and error strings that vanilla has a translation for are copied
//! verbatim into [`lang`]; the ones this server invents carry a `TODO verify`
//! note. A "snapshot" test per message pins the exact text.

use crate::session::DemoWorld;

/// The four-five colours vanilla uses for command output: none, and red.
///
/// "Red/white styling" in the brief: an error is the client's legacy red, a
/// success is left as the default chat colour. No other colour is introduced.
pub const RED: &str = "§c";

/// Whether a line reports success or failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackKind {
    /// The command ran: plain text.
    Success,
    /// The command failed to parse, was denied, or reported an error: red.
    Error,
}

/// A command's whole answer, with its severity.
///
/// `lines` may be empty: a command that succeeds silently (self-`/kill`)
/// produces no message, and the feedback path must emit nothing rather than a
/// blank line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feedback {
    /// Success or error, for styling.
    pub kind: FeedbackKind,
    /// The lines to show, in order. May be empty.
    pub lines: Vec<String>,
}

impl Feedback {
    /// A successful answer.
    pub fn success(lines: Vec<String>) -> Feedback {
        Feedback {
            kind: FeedbackKind::Success,
            lines,
        }
    }

    /// A failed answer.
    pub fn error(lines: Vec<String>) -> Feedback {
        Feedback {
            kind: FeedbackKind::Error,
            lines,
        }
    }

    /// Whether there is anything to show at all.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Whether `source` should be shown its command feedback.
///
/// This is vanilla's rule exactly. Players, the console and RCON always see
/// feedback; command blocks and functions see it only when
/// `sendCommandFeedback` is on.
pub fn emits(source: super::source::SourceKind, send_command_feedback: bool) -> bool {
    source.always_receives_feedback() || send_command_feedback
}

/// Render one line for chat at `kind`'s colour.
pub fn render(kind: FeedbackKind, line: &str) -> String {
    match kind {
        FeedbackKind::Success => line.to_string(),
        FeedbackKind::Error => format!("{RED}{line}"),
    }
}

/// Deliver `feedback` to `source`, through `emit`, honouring the game rule.
///
/// The one place feedback leaves the command layer. `emit` receives already
/// styled lines, so the caller only has to know how to send a chat line.
pub fn send(
    source: &super::source::CommandSource<'_>,
    feedback: &Feedback,
    send_command_feedback: bool,
    mut emit: impl FnMut(&str),
) {
    if !emits(source.kind(), send_command_feedback) {
        return;
    }
    for line in &feedback.lines {
        emit(&render(feedback.kind, line));
    }
}

// ---------------------------------------------------------------------------
// Game rules
// ---------------------------------------------------------------------------

/// The subset of the world's game rules the server honours *today*.
///
/// CMD-13 owns `/gamerule` and the full rule set. This is the minimal store
/// CMD-05 needs: one rule, `sendCommandFeedback`, defaulting to vanilla's
/// `true`, persisted under [`aether_api::keys::GAME_RULES`] as one byte so a
/// later task can extend the blob without a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameRules {
    /// Whether command blocks and functions are shown command feedback.
    pub send_command_feedback: bool,
}

impl Default for GameRules {
    fn default() -> GameRules {
        // Vanilla's default. A player is never silenced by this rule; only
        // command blocks and functions are.
        GameRules {
            send_command_feedback: true,
        }
    }
}

impl GameRules {
    /// Decode the rule blob. A missing or malformed blob is the default, so a
    /// corrupt key can never turn feedback *off*.
    pub fn from_bytes(bytes: Option<&[u8]>) -> GameRules {
        match bytes {
            Some(b) if !b.is_empty() => GameRules {
                send_command_feedback: b[0] != 0,
            },
            _ => GameRules::default(),
        }
    }

    /// Encode the rule blob.
    pub fn to_bytes(self) -> [u8; 1] {
        [self.send_command_feedback as u8]
    }

    /// Read the rules from the world.
    pub fn load(world: &DemoWorld) -> GameRules {
        let bytes = world.get_meta(aether_api::keys::GAME_RULES).ok().flatten();
        GameRules::from_bytes(bytes.as_deref())
    }

    /// Write the rules back to the world. A storage failure is swallowed, as
    /// elsewhere: the next `/gamerule` retries, and the in-memory default is
    /// still correct.
    pub fn save(self, world: &DemoWorld) {
        let _ = world.put_meta(aether_api::keys::GAME_RULES, &self.to_bytes());
    }
}

/// Whether command blocks and functions are currently shown feedback.
pub fn send_command_feedback(world: &DemoWorld) -> bool {
    GameRules::load(world).send_command_feedback
}

/// Set `sendCommandFeedback` in the world.
pub fn set_send_command_feedback(world: &DemoWorld, on: bool) {
    let mut rules = GameRules::load(world);
    rules.send_command_feedback = on;
    rules.save(world);
}

// ---------------------------------------------------------------------------
// Vanilla messages
// ---------------------------------------------------------------------------

/// Vanilla 1.21.11 `en_us` strings this server emits, copied verbatim.
///
/// Source: `assets/minecraft/lang/en_us.json` for 1.21.11. Placeholders are
/// vanilla's `%s` (and the occasional positional `%n$s`); [`format`] fills
/// them. A string that is *not* in vanilla's file is marked `TODO verify`
/// so the next reader knows it is this server's wording, not Mojang's.
pub mod lang {
    /// `commands.give.success.single`.
    pub const GIVE_SUCCESS_SINGLE: &str = "Gave %s %s to %s";
    /// `commands.teleport.success.location.single`.
    pub const TELEPORT_SUCCESS_LOCATION_SINGLE: &str = "Teleported %s to %s, %s, %s";
    /// `commands.teleport.success.entity.single`.
    pub const TELEPORT_SUCCESS_ENTITY_SINGLE: &str = "Teleported %s to %s";
    /// `commands.gamemode.success.self`.
    pub const GAMEMODE_SUCCESS_SELF: &str = "Set own game mode to %s";
    /// `commands.gamemode.success.other`.
    pub const GAMEMODE_SUCCESS_OTHER: &str = "Set %s's game mode to %s";
    /// `commands.time.set`.
    pub const TIME_SET: &str = "Set the time to %s";
    /// `commands.time.query`.
    pub const TIME_QUERY: &str = "The time is %s";
    /// `commands.summon.success`.
    pub const SUMMON_SUCCESS: &str = "Summoned new %s";
    /// `commands.kill.success.single`.
    pub const KILL_SUCCESS_SINGLE: &str = "Killed %s";
    /// `commands.message.display.outgoing`.
    pub const MESSAGE_OUTGOING: &str = "You whisper to %s: %s";
    /// `commands.message.display.incoming`.
    pub const MESSAGE_INCOMING: &str = "%s whispers to you: %s";

    /// `gameMode.survival`.
    pub const GAMEMODE_SURVIVAL: &str = "Survival Mode";
    /// `gameMode.creative`.
    pub const GAMEMODE_CREATIVE: &str = "Creative Mode";
    /// `gameMode.adventure`.
    pub const GAMEMODE_ADVENTURE: &str = "Adventure Mode";
    /// `gameMode.spectator`.
    pub const GAMEMODE_SPECTATOR: &str = "Spectator Mode";

    /// The line `/help <unknown>` gives.
    ///
    /// TODO verify: the brief specifies this text, but 1.21.11 `en_us` has no
    /// such key — the client shows it itself, and the closest server string is
    /// `commands.help.failed` ("Unknown command or insufficient permissions").
    /// Kept here so the wording lives with the other messages.
    pub const UNKNOWN_COMMAND: &str = "Unknown command. Try /help";

    /// Fill a `%s` / `%n$s` template with `args`.
    ///
    /// Vanilla's formatter is `String.format`, so `%s` consumes the next
    /// argument and `%1$s` names one by index; `%%` is a literal percent.
    /// Unknown escapes pass through unchanged rather than being dropped.
    pub fn format(template: &str, args: &[&str]) -> String {
        let mut out = String::with_capacity(template.len());
        let mut next = 0usize;
        let bytes = template.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'%' {
                // Copy the full UTF-8 char so multi-byte text survives.
                let ch = template[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
            // A `%`: parse `%[n$]s`.
            let mut j = i + 1;
            let mut index: Option<usize> = None;
            let digits_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start && j < bytes.len() && bytes[j] == b'$' {
                index = template[digits_start..j].parse().ok();
                j += 1;
            }
            match bytes.get(j) {
                Some(b's') => {
                    let arg = match index {
                        Some(n) => args.get(n.saturating_sub(1)),
                        None => {
                            let a = args.get(next);
                            next += 1;
                            a
                        }
                    };
                    if let Some(a) = arg {
                        out.push_str(a);
                    }
                    i = j + 1;
                }
                Some(b'%') => {
                    out.push('%');
                    i = j + 1;
                }
                _ => {
                    out.push('%');
                    i += 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::lang::format;
    use super::*;
    use crate::commands::source::SourceKind;

    #[test]
    fn players_and_consoles_ignore_the_gamerule_and_blocks_obey_it() {
        for on in [true, false] {
            assert!(emits(SourceKind::Player, on), "a player always sees it");
            assert!(emits(SourceKind::Console, on), "a console always sees it");
            assert!(emits(SourceKind::Rcon, on), "an RCON always sees it");
        }
        assert!(emits(SourceKind::CommandBlock, true));
        assert!(!emits(SourceKind::CommandBlock, false));
        assert!(emits(SourceKind::Function, true));
        assert!(!emits(SourceKind::Function, false));
    }

    #[test]
    fn errors_are_red_and_success_is_plain() {
        assert_eq!(render(FeedbackKind::Success, "ok"), "ok");
        assert_eq!(render(FeedbackKind::Error, "bad"), "§cbad");
    }

    #[test]
    fn the_rule_defaults_on_and_survives_a_round_trip() {
        assert!(GameRules::default().send_command_feedback);
        // A missing blob is the default.
        assert_eq!(GameRules::from_bytes(None), GameRules::default());
        assert!(GameRules::from_bytes(None).send_command_feedback);
        // A zero-length blob does not turn feedback off.
        assert!(GameRules::from_bytes(Some(&[])).send_command_feedback);
        let off = GameRules {
            send_command_feedback: false,
        };
        assert_eq!(GameRules::from_bytes(Some(&off.to_bytes())), off);
        let on = GameRules {
            send_command_feedback: true,
        };
        assert_eq!(GameRules::from_bytes(Some(&on.to_bytes())), on);
    }

    #[test]
    fn an_empty_feedback_emits_nothing() {
        let src = crate::commands::source::CommandSource::Console;
        let mut seen = Vec::new();
        send(&src, &Feedback::success(Vec::new()), true, |l| {
            seen.push(l.to_string())
        });
        assert!(seen.is_empty(), "a silent success must stay silent");
    }

    #[test]
    fn send_honours_the_gamerule_and_styles_the_lines() {
        let src = crate::commands::source::CommandSource::Function { name: "tick" };
        let feedback = Feedback::error(vec!["nope".into()]);
        let mut seen = Vec::new();
        send(&src, &feedback, false, |l| seen.push(l.to_string()));
        assert!(
            seen.is_empty(),
            "a function is silenced when the rule is off"
        );
        send(&src, &feedback, true, |l| seen.push(l.to_string()));
        assert_eq!(seen, vec!["§cnope".to_string()]);
    }

    #[test]
    fn the_plain_formatter_fills_sequential_and_positional_placeholders() {
        assert_eq!(format("a %s b %s", &["1", "2"]), "a 1 b 2");
        assert_eq!(format("%2$s %1$s", &["one", "two"]), "two one");
        assert_eq!(format("100%% %s", &["x"]), "100% x");
        // A missing argument leaves nothing behind, never a panic.
        assert_eq!(format("%s %s", &["only"]), "only ");
    }

    /// A snapshot per vanilla string used: the template verbatim, then the
    /// fully rendered form. If a message is edited by accident these fail.
    #[test]
    fn the_vanilla_messages_are_snapshotted() {
        assert_eq!(lang::GIVE_SUCCESS_SINGLE, "Gave %s %s to %s");
        assert_eq!(
            format(
                lang::GIVE_SUCCESS_SINGLE,
                &["3", "minecraft:stone", "tester"]
            ),
            "Gave 3 minecraft:stone to tester"
        );

        assert_eq!(
            lang::TELEPORT_SUCCESS_LOCATION_SINGLE,
            "Teleported %s to %s, %s, %s"
        );
        assert_eq!(
            format(
                lang::TELEPORT_SUCCESS_LOCATION_SINGLE,
                &["tester", "1", "2", "3"]
            ),
            "Teleported tester to 1, 2, 3"
        );
        assert_eq!(lang::TELEPORT_SUCCESS_ENTITY_SINGLE, "Teleported %s to %s");
        assert_eq!(
            format(lang::TELEPORT_SUCCESS_ENTITY_SINGLE, &["tester", "bob"]),
            "Teleported tester to bob"
        );

        assert_eq!(lang::GAMEMODE_SUCCESS_SELF, "Set own game mode to %s");
        assert_eq!(
            format(lang::GAMEMODE_SUCCESS_SELF, &[lang::GAMEMODE_CREATIVE]),
            "Set own game mode to Creative Mode"
        );
        assert_eq!(lang::GAMEMODE_SUCCESS_OTHER, "Set %s's game mode to %s");
        assert_eq!(
            format(
                lang::GAMEMODE_SUCCESS_OTHER,
                &["bob", lang::GAMEMODE_SURVIVAL]
            ),
            "Set bob's game mode to Survival Mode"
        );

        assert_eq!(lang::TIME_SET, "Set the time to %s");
        assert_eq!(format(lang::TIME_SET, &["1000"]), "Set the time to 1000");
        assert_eq!(lang::TIME_QUERY, "The time is %s");
        assert_eq!(format(lang::TIME_QUERY, &["6000"]), "The time is 6000");

        assert_eq!(lang::SUMMON_SUCCESS, "Summoned new %s");
        assert_eq!(
            format(lang::SUMMON_SUCCESS, &["minecraft:pig"]),
            "Summoned new minecraft:pig"
        );
        assert_eq!(lang::KILL_SUCCESS_SINGLE, "Killed %s");
        assert_eq!(
            format(lang::KILL_SUCCESS_SINGLE, &["tester"]),
            "Killed tester"
        );
        assert_eq!(lang::MESSAGE_OUTGOING, "You whisper to %s: %s");
        assert_eq!(
            format(lang::MESSAGE_OUTGOING, &["bob", "hi"]),
            "You whisper to bob: hi"
        );
        assert_eq!(lang::MESSAGE_INCOMING, "%s whispers to you: %s");
        assert_eq!(
            format(lang::MESSAGE_INCOMING, &["tester", "hi"]),
            "tester whispers to you: hi"
        );

        // Not vanilla: pinned so an edit is caught, but marked in the table.
        assert_eq!(lang::UNKNOWN_COMMAND, "Unknown command. Try /help");
    }
}
