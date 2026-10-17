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
//!   vanilla does it*, which is **per source and per success/error**, not as
//!   one switch. See [`accepts`] and its table.
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

/// Whether `source` should be shown a reply of `kind`, as vanilla decides it.
///
/// The rule is **not** one switch: it depends on the source *and* on whether
/// the reply is a success or an error. Vanilla 1.21.x
/// (`ServerPlayer`/`MinecraftServer`/`BaseCommandBlock`/`FunctionCommand`):
///
/// | source        | success                              | error             |
/// |---------------|--------------------------------------|-------------------|
/// | player        | `sendCommandFeedback`                | always            |
/// | console / RCON| always                               | always            |
/// | command block | `sendCommandFeedback && trackOutput` | `trackOutput`     |
/// | function      | always suppressed                    | always suppressed |
///
/// The two consequences worth stating: a player's *errors* are never silenced
/// by the rule (only their successes are), and a datapack function is always
/// suppressed (`withSuppressedOutput`), rule or no rule. `track_output` is the
/// command block's own setting; it is ignored for every other source.
pub fn accepts(
    source: super::source::SourceKind,
    kind: FeedbackKind,
    rules: GameRules,
    track_output: bool,
) -> bool {
    use super::source::SourceKind;
    let rule = rules.send_command_feedback;
    match (source, kind) {
        // A player sees their own failures always; the rule gates only
        // success (`ServerPlayer.acceptsSuccess()` / `acceptsFailure()`).
        (SourceKind::Player, FeedbackKind::Error) => true,
        (SourceKind::Player, FeedbackKind::Success) => rule,
        // The console and RCON are the server itself: always.
        (SourceKind::Console | SourceKind::Rcon, _) => true,
        // A command block with output tracking off is silent; with it on, the
        // rule gates success but never failure.
        (SourceKind::CommandBlock, FeedbackKind::Error) => track_output,
        (SourceKind::CommandBlock, FeedbackKind::Success) => rule && track_output,
        // A function always runs with output suppressed.
        (SourceKind::Function, _) => false,
    }
}

/// Render one line for chat at `kind`'s colour.
pub fn render(kind: FeedbackKind, line: &str) -> String {
    match kind {
        FeedbackKind::Success => line.to_string(),
        FeedbackKind::Error => format!("{RED}{line}"),
    }
}

/// Deliver `feedback` to `source`, through `emit`, honouring the game rules.
///
/// The one place feedback leaves the command layer. `emit` receives already
/// styled lines, so the caller only has to know how to send a chat line. The
/// source's own `trackOutput` is read here, so a caller never has to pass it.
pub fn send(
    source: &super::source::CommandSource<'_>,
    feedback: &Feedback,
    rules: GameRules,
    mut emit: impl FnMut(&str),
) {
    if !accepts(source.kind(), feedback.kind, rules, source.track_output()) {
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
        // Vanilla's default. When off, the rule suppresses only *success*
        // feedback (a player's errors still show, a command block's errors
        // still follow `trackOutput`); a function is always suppressed either
        // way. See [`accepts`].
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

/// Whether the `sendCommandFeedback` rule is currently on.
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
    /// `commands.clear.success.single`.
    pub const CLEAR_SUCCESS_SINGLE: &str = "Removed %s item(s) from player %s";
    /// `commands.clear.success.multiple`.
    pub const CLEAR_SUCCESS_MULTIPLE: &str = "Removed %s item(s) from %s players";
    /// `clear.failed.single`.
    pub const CLEAR_FAILED_SINGLE: &str = "No items were found on player %s";
    /// `clear.failed.multiple`.
    pub const CLEAR_FAILED_MULTIPLE: &str = "No items were found on %s players";
    /// `commands.effect.give.success.single`.
    pub const EFFECT_GIVE_SUCCESS_SINGLE: &str = "Applied effect %s to %s";
    /// `commands.effect.give.success.multiple`.
    pub const EFFECT_GIVE_SUCCESS_MULTIPLE: &str = "Applied effect %s to %s targets";
    /// `commands.effect.give.failed`.
    pub const EFFECT_GIVE_FAILED: &str =
        "Unable to apply this effect (target is either immune to effects, or has something stronger)";
    /// `commands.effect.clear.specific.success.single`.
    pub const EFFECT_CLEAR_SPECIFIC_SUCCESS_SINGLE: &str = "Removed effect %s from %s";
    /// `commands.effect.clear.specific.success.multiple`.
    pub const EFFECT_CLEAR_SPECIFIC_SUCCESS_MULTIPLE: &str = "Removed effect %s from %s targets";
    /// `commands.effect.clear.everything.success.single`.
    pub const EFFECT_CLEAR_EVERYTHING_SUCCESS_SINGLE: &str = "Removed every effect from %s";
    /// `commands.effect.clear.everything.success.multiple`.
    pub const EFFECT_CLEAR_EVERYTHING_SUCCESS_MULTIPLE: &str =
        "Removed every effect from %s targets";
    /// `commands.effect.clear.specific.failed`.
    pub const EFFECT_CLEAR_SPECIFIC_FAILED: &str = "Target doesn't have the requested effect";
    /// `commands.effect.clear.everything.failed`.
    pub const EFFECT_CLEAR_EVERYTHING_FAILED: &str = "Target has no effects to remove";
    /// `commands.enchant.success.single`.
    pub const ENCHANT_SUCCESS_SINGLE: &str = "Applied enchantment %s to %s's item";
    /// `commands.enchant.success.multiple`.
    pub const ENCHANT_SUCCESS_MULTIPLE: &str = "Applied enchantment %s to %s entities";
    /// `commands.enchant.failed`.
    pub const ENCHANT_FAILED: &str =
        "Nothing changed. Targets either have no item in their hands or the enchantment could not be applied";
    /// `commands.enchant.failed.itemless`.
    pub const ENCHANT_FAILED_ITEMLESS: &str = "%s is not holding any item";
    /// `commands.enchant.failed.incompatible`.
    pub const ENCHANT_FAILED_INCOMPATIBLE: &str = "%s cannot support that enchantment";
    /// `commands.enchant.failed.level`.
    pub const ENCHANT_FAILED_LEVEL: &str =
        "%s is higher than the maximum level of %s supported by that enchantment";
    /// `commands.enchant.failed.entity`.
    pub const ENCHANT_FAILED_ENTITY: &str = "%s is not a valid entity for this command";

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
    /// `commands.weather.set.clear`.
    pub const WEATHER_CLEAR: &str = "Set the weather to clear";
    /// `commands.weather.set.rain`.
    pub const WEATHER_RAIN: &str = "Set the weather to rain";
    /// `commands.weather.set.thunder`.
    pub const WEATHER_THUNDER: &str = "Set the weather to rain & thunder";
    /// `commands.worldborder.get`.
    pub const WORLDBORDER_GET: &str = "The world border is currently %s block(s) wide";
    /// `commands.worldborder.center.success`.
    pub const WORLDBORDER_CENTER_SUCCESS: &str = "Set the center of the world border to %s, %s";
    /// `commands.worldborder.center.failed`.
    pub const WORLDBORDER_CENTER_FAILED: &str =
        "Nothing changed. The world border is already centered there";
    /// `commands.worldborder.set.failed.small`.
    pub const WORLDBORDER_FAILED_SMALL: &str = "World border cannot be smaller than 1 block wide";
    /// `commands.worldborder.set.failed.big`.
    pub const WORLDBORDER_FAILED_BIG: &str = "World border cannot be bigger than %s blocks wide";
    /// `commands.worldborder.set.failed.far`.
    pub const WORLDBORDER_FAILED_FAR: &str = "World border cannot be further out than %s blocks";
    /// `commands.worldborder.set.failed.nochange`.
    pub const WORLDBORDER_SET_NOCHANGE: &str =
        "Nothing changed. The world border is already that size";
    /// `commands.worldborder.set.immediate`.
    pub const WORLDBORDER_SET_IMMEDIATE: &str = "Set the world border to %s block(s) wide";
    /// `commands.worldborder.set.grow`.
    pub const WORLDBORDER_SET_GROW: &str =
        "Growing the world border to %s blocks wide over %s seconds";
    /// `commands.worldborder.set.shrink`.
    pub const WORLDBORDER_SET_SHRINK: &str =
        "Shrinking the world border to %s block(s) wide over %s second(s)";
    /// `commands.worldborder.damage.amount.success`.
    pub const WORLDBORDER_DAMAGE_AMOUNT_SUCCESS: &str =
        "Set the world border damage to %s per block each second";
    /// `commands.worldborder.damage.amount.failed`.
    pub const WORLDBORDER_DAMAGE_AMOUNT_FAILED: &str =
        "Nothing changed. The world border damage is already that amount";
    /// `commands.worldborder.damage.buffer.success`.
    pub const WORLDBORDER_DAMAGE_BUFFER_SUCCESS: &str =
        "Set the world border damage buffer to %s block(s)";
    /// `commands.worldborder.damage.buffer.failed`.
    pub const WORLDBORDER_DAMAGE_BUFFER_FAILED: &str =
        "Nothing changed. The world border damage buffer is already that distance";
    /// `commands.worldborder.warning.distance.success`.
    pub const WORLDBORDER_WARNING_DISTANCE_SUCCESS: &str =
        "Set the world border warning distance to %s block(s)";
    /// `commands.worldborder.warning.distance.failed`.
    pub const WORLDBORDER_WARNING_DISTANCE_FAILED: &str =
        "Nothing changed. The world border warning is already that distance";
    /// `commands.worldborder.warning.time.success`.
    pub const WORLDBORDER_WARNING_TIME_SUCCESS: &str =
        "Set the world border warning time to %s second(s)";
    /// `commands.worldborder.warning.time.failed`.
    pub const WORLDBORDER_WARNING_TIME_FAILED: &str =
        "Nothing changed. The world border warning is already that amount of time";
    /// `commands.setworldspawn.success`.
    pub const SETWORLDSPAWN_SUCCESS: &str = "Set the world spawn point to %s, %s, %s [%s]";
    /// `commands.defaultgamemode.success`.
    pub const DEFAULTGAMEMODE_SUCCESS: &str = "The default game mode is now %s";
    /// `commands.particle.success`.
    pub const PARTICLE_SUCCESS: &str = "Displaying particle %s";
    /// `commands.particle.failed`.
    pub const PARTICLE_FAILED: &str = "The particle was not visible for anybody";
    /// `particle.notFound`.
    pub const PARTICLE_NOT_FOUND: &str = "Unknown particle: %s";
    /// `particle.invalidOptions`.
    pub const PARTICLE_INVALID_OPTIONS: &str = "Can't parse particle options: %s";
    /// `commands.playsound.success.single`.
    pub const PLAYSOUND_SUCCESS_SINGLE: &str = "Played sound %s to %s";
    /// `commands.playsound.success.multiple`.
    pub const PLAYSOUND_SUCCESS_MULTIPLE: &str = "Played sound %s to %s players";
    /// `commands.playsound.failed`.
    pub const PLAYSOUND_FAILED: &str = "The sound is too far away to be heard";
    /// `commands.stopsound.success.source.any`.
    pub const STOPSOUND_SOURCE_ANY: &str = "Stopped all '%s' sounds";
    /// `commands.stopsound.success.source.sound`.
    pub const STOPSOUND_SOURCE_SOUND: &str = "Stopped sound '%s' on source '%s'";
    /// `commands.stopsound.success.sourceless.any`.
    pub const STOPSOUND_ANY: &str = "Stopped all sounds";
    /// `commands.stopsound.success.sourceless.sound`.
    pub const STOPSOUND_SOUND: &str = "Stopped sound '%s'";

    /// `argument.integer.low`.
    pub const ARG_INTEGER_LOW: &str = "Integer must not be less than %s: found %s";
    /// `argument.integer.big`.
    pub const ARG_INTEGER_BIG: &str = "Integer must not be more than %s: found %s";
    /// `argument.float.low`.
    pub const ARG_FLOAT_LOW: &str = "Float must not be less than %s: found %s";
    /// `argument.float.big`.
    pub const ARG_FLOAT_BIG: &str = "Float must not be more than %s: found %s";
    /// `argument.double.low`.
    pub const ARG_DOUBLE_LOW: &str = "Double must not be less than %s: found %s";
    /// `argument.double.big`.
    pub const ARG_DOUBLE_BIG: &str = "Double must not be more than %s: found %s";
    /// `argument.time.tick_count_too_low`.
    pub const ARG_TIME_TOO_LOW: &str = "The tick count must not be less than %s: found %s";
    /// `argument.pos.outofbounds`.
    pub const POS_OUT_OF_BOUNDS: &str = "That position is outside the allowed boundaries.";
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

    fn rules(on: bool) -> GameRules {
        GameRules {
            send_command_feedback: on,
        }
    }

    #[test]
    fn the_feedback_policy_matches_vanilla_per_source_and_kind() {
        use FeedbackKind::{Error, Success};
        use SourceKind::{CommandBlock, Console, Function, Player, Rcon};

        // A player: the rule gates success, never error.
        assert!(accepts(Player, Success, rules(true), false));
        assert!(!accepts(Player, Success, rules(false), false));
        assert!(accepts(Player, Error, rules(true), false));
        assert!(
            accepts(Player, Error, rules(false), false),
            "errors always show"
        );

        // Console and RCON: always, either kind, rule irrelevant.
        for on in [true, false] {
            for kind in [Success, Error] {
                assert!(accepts(Console, kind, rules(on), false));
                assert!(accepts(Rcon, kind, rules(on), false));
            }
        }

        // A command block: `trackOutput` gates both; the rule gates only
        // success.
        assert!(accepts(CommandBlock, Success, rules(true), true));
        assert!(!accepts(CommandBlock, Success, rules(false), true), "rule");
        assert!(
            !accepts(CommandBlock, Success, rules(true), false),
            "trackOutput"
        );
        assert!(accepts(CommandBlock, Error, rules(true), true));
        assert!(
            accepts(CommandBlock, Error, rules(false), true),
            "error ignores the rule"
        );
        assert!(
            !accepts(CommandBlock, Error, rules(true), false),
            "trackOutput"
        );

        // A function: always suppressed, either kind, rule irrelevant.
        for on in [true, false] {
            for kind in [Success, Error] {
                assert!(!accepts(Function, kind, rules(on), false));
            }
        }
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
        send(&src, &Feedback::success(Vec::new()), rules(true), |l| {
            seen.push(l.to_string())
        });
        assert!(seen.is_empty(), "a silent success must stay silent");
    }

    #[test]
    fn send_styles_the_lines_and_honours_the_policy() {
        use crate::commands::source::CommandSource;

        // A command block with output on: success follows the rule, error
        // does not.
        let block = CommandSource::CommandBlock {
            x: 0,
            y: 0,
            z: 0,
            track_output: true,
        };
        let success = Feedback::success(vec!["ok".into()]);
        let error = Feedback::error(vec!["nope".into()]);

        let mut seen = Vec::new();
        send(&block, &success, rules(false), |l| seen.push(l.to_string()));
        assert!(seen.is_empty(), "the rule silences a block's success");
        send(&block, &success, rules(true), |l| seen.push(l.to_string()));
        assert_eq!(seen, vec!["ok".to_string()]);

        seen.clear();
        send(&block, &error, rules(false), |l| seen.push(l.to_string()));
        assert_eq!(seen, vec!["§cnope".to_string()], "error ignores the rule");

        // A function is always suppressed, rule or no rule.
        let function = CommandSource::Function { name: "tick" };
        seen.clear();
        send(&function, &error, rules(true), |l| seen.push(l.to_string()));
        assert!(seen.is_empty(), "a function is always suppressed");
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
