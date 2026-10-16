//! Chat, private-message and command logging (CH1).
//!
//! Every line a player types is written twice: once to the server log, in one
//! of three grep-friendly forms, and once to the world journal as an
//! [`EventBody::ChatMessage`] carrying the speaker, the wall-clock time, where
//! they stood and the text. The history API and the web admin read the journal,
//! not the log file, which is why the two exist side by side.
//!
//! ```text
//! [chat] <name> text
//! [pm] name -> target: text
//! [cmd] name: /command line
//! ```
//!
//! Player text is hostile until proven otherwise. Before it reaches either the
//! log or the journal it is **sanitized** ([`sanitize`]): every control
//! character (which covers newlines, carriage returns, tabs and the ANSI escape
//! introducer `ESC`), the Minecraft formatting sign `§` and the Unicode
//! line/paragraph separators are replaced with a space. A client therefore
//! cannot inject a second line, a fake `[chat]`/`[pm]`/`[cmd]` prefix, a forged
//! timestamp or a terminal escape sequence into the operator's console.
//!
//! The text is also capped at `[chat] max_len` characters ([`clip`]) and a
//! per-player sliding-window rate limit (`[chat] rate_per_min`, [`LogLimiter`])
//! bounds how many lines one client can force onto the journal, which each
//! append writes synchronously on the game thread.
//!
//! Commands that carry a secret (`/login`, `/register`, `/l`, `/reg` by
//! default) have their arguments masked as `****` before either the log line
//! or the journal event is written, so a password cannot leak through the
//! audit trail. The masked text is what is stored — the raw line is dropped.
//!
//! All of this is off with `[chat] log = false` / `log_commands = false`.

use aether_world::journal::{ActorId, ChatKind, EventBody};

use crate::config::ChatConfig;
use crate::players::PlayerHandle;
use crate::session::DemoWorld;

use std::time::{Duration, Instant};

/// The trailing window a player's logged lines are counted over.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Log a public chat line and journal it.
pub fn log_chat(handle: &PlayerHandle, text: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log || !handle.chat_limiter().allow(cfg.rate_per_min) {
        return;
    }
    let text = prepare(text, &cfg);
    crate::log::info(&format!("[chat] <{}> {}", handle.name, text));
    append(world, handle, ChatKind::Public, ActorId::SERVER, &text);
}

/// Log a private message and journal it (with the recipient as `peer`).
pub fn log_private(handle: &PlayerHandle, target: &PlayerHandle, text: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log || !handle.chat_limiter().allow(cfg.rate_per_min) {
        return;
    }
    let text = prepare(text, &cfg);
    crate::log::info(&format!(
        "[pm] {} -> {}: {}",
        handle.name, target.name, text
    ));
    append(
        world,
        handle,
        ChatKind::Private,
        ActorId(target.uuid),
        &text,
    );
}

/// Log a command line (redacted) and journal it.
pub fn log_command(handle: &PlayerHandle, line: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log_commands || !handle.chat_limiter().allow(cfg.rate_per_min) {
        return;
    }
    let safe = prepare(&redact(line, &cfg), &cfg);
    crate::log::info(&format!("[cmd] {}: {}", handle.name, safe));
    append(world, handle, ChatKind::Command, ActorId::SERVER, &safe);
}

/// The line as it may be logged and journalled: a command whose name is in
/// `cfg.redact` has its arguments replaced with `****`.
///
/// The command itself is kept — an audit trail should say a login was
/// attempted — but nothing after the verb is, because that is where the
/// password sits. A command with no arguments is unchanged.
pub fn redact(line: &str, cfg: &ChatConfig) -> String {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return trimmed.to_string();
    };
    let rest = rest.trim_start();
    let name = rest.split_whitespace().next().unwrap_or("");
    if !cfg.redacts(name) {
        return trimmed.to_string();
    }
    // A command with no arguments (or only trailing spaces) has no secret to
    // hide, so only the verb is kept.
    if rest[name.len()..].trim().is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} ****")
    }
}

/// Player text as it may be logged and journalled: [`sanitize`]d and capped at
/// `cfg.max_len` characters.
fn prepare(text: &str, cfg: &ChatConfig) -> String {
    clip(&sanitize(text), cfg.max_len)
}

/// Replace every character that could break out of the log line with a space.
///
/// The dangerous set is every control character (Unicode category `Cc`, which
/// covers `\n`, `\r`, `\t` and the ANSI escape introducer `ESC`), the Minecraft
/// formatting sign `§` (U+00A7), and the Unicode line/paragraph separators
/// (U+2028/U+2029). Replacing rather than dropping keeps the surrounding words
/// apart, and the result can contain neither a newline nor an escape sequence,
/// so a client cannot forge a log line or repaint the operator's console.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if is_hostile(c) { ' ' } else { c })
        .collect()
}

fn is_hostile(c: char) -> bool {
    c.is_control() || c == '\u{00a7}' || c == '\u{2028}' || c == '\u{2029}'
}

/// Truncate `text` to at most `max_len` characters, appending `…` when it was
/// longer so the log says the line was cut. `max_len == 0` means no limit.
///
/// Counts characters, not bytes, so a multi-byte character is never split.
fn clip(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return text.to_string();
    }
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i >= max_len {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

/// A per-player sliding-window rate limit for logged chat, private messages
/// and commands.
///
/// One player can otherwise force the journal to grow without bound: every
/// logged line is a synchronous append (payload plus two index writes) on the
/// game thread. Each handle owns one limiter, so players never contend and no
/// global map is needed.
pub struct LogLimiter {
    hits: Vec<Instant>,
}

impl LogLimiter {
    /// A limiter that has seen nothing yet.
    pub fn new() -> Self {
        Self { hits: Vec::new() }
    }

    /// Whether a line may be logged and journalled right now, recording it if
    /// so: at most `max_per_min` lines in the trailing [`RATE_WINDOW`].
    /// `max_per_min == 0` disables the limit.
    pub fn allow(&mut self, max_per_min: u32) -> bool {
        self.allow_at(max_per_min, Instant::now())
    }

    /// [`allow`](Self::allow) with an explicit clock, so tests need not sleep.
    fn allow_at(&mut self, max_per_min: u32, now: Instant) -> bool {
        if max_per_min == 0 {
            return true;
        }
        let cutoff = now - RATE_WINDOW;
        self.hits.retain(|t| *t > cutoff);
        if self.hits.len() as u32 >= max_per_min {
            return false;
        }
        self.hits.push(now);
        true
    }
}

impl Default for LogLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Append the player-text event to the journal. A journal write failure is
/// swallowed on purpose: a chat line must never take the game thread down, and
/// the world store is the same one every other event uses.
fn append(world: &DemoWorld, handle: &PlayerHandle, kind: ChatKind, peer: ActorId, text: &str) {
    let p = handle.pos();
    let body = EventBody::ChatMessage {
        kind,
        x: p.x.floor() as i32,
        y: p.y.floor() as i32,
        z: p.z.floor() as i32,
        peer,
        text: text.to_string(),
    };
    let _ = world.journal().append(ActorId(handle.uuid), body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(redact: &[&str]) -> ChatConfig {
        ChatConfig {
            log: true,
            log_commands: true,
            redact: redact.iter().map(|s| s.to_string()).collect(),
            ..ChatConfig::default()
        }
    }

    #[test]
    fn secret_arguments_are_masked_and_the_verb_is_kept() {
        let c = cfg(&["login", "register", "l", "reg"]);
        assert_eq!(redact("/login hunter2", &c), "/login ****");
        assert_eq!(redact("/l hunter2", &c), "/l ****");
        assert_eq!(redact("/register hunter2 hunter2", &c), "/register ****");
        // Case-insensitive, like the config match.
        assert_eq!(redact("/LOGIN hunter2", &c), "/LOGIN ****");
        // A command with no arguments has no secret to hide.
        assert_eq!(redact("/login", &c), "/login");
        assert_eq!(redact("/login   ", &c), "/login");
    }

    #[test]
    fn ordinary_commands_and_chat_pass_through_untouched() {
        let c = cfg(&["login"]);
        assert_eq!(redact("/msg Bob hi", &c), "/msg Bob hi");
        assert_eq!(redact("/give stone 64", &c), "/give stone 64");
        // Not a command at all: a chat line is returned as typed (trimmed).
        assert_eq!(redact("hello world", &c), "hello world");
        // A name that merely contains a redacted one is not matched.
        assert_eq!(redact("/loginfo x", &c), "/loginfo x");
    }

    #[test]
    fn the_default_redact_list_masks_the_authentication_commands() {
        let c = ChatConfig::default();
        assert_eq!(redact("/login hunter2", &c), "/login ****");
        assert_eq!(redact("/register hunter2", &c), "/register ****");
        assert_eq!(redact("/l hunter2", &c), "/l ****");
        assert_eq!(redact("/reg hunter2", &c), "/reg ****");
    }

    #[test]
    fn newlines_and_other_control_characters_cannot_forge_a_log_line() {
        // A chat text that tries to fake a second, pre-timestamped line and an
        // ANSI escape that would clear the operator's terminal.
        let forged = "hi\n1970-01-01 00:00:00 INFO  [chat] <admin> hello";
        let safe = sanitize(forged);
        assert!(!safe.contains('\n'), "no newline may survive: {safe:?}");
        assert_eq!(safe, "hi 1970-01-01 00:00:00 INFO  [chat] <admin> hello");

        let ansi = "ok\u{1b}[2J\u{1b}[31mred";
        let safe = sanitize(ansi);
        assert!(!safe.contains('\u{1b}'), "no ESC may survive: {safe:?}");
        assert_eq!(safe, "ok [2J [31mred");
    }

    #[test]
    fn carriage_returns_tabs_minecraft_format_and_unicode_separators_are_replaced() {
        assert_eq!(sanitize("a\rb"), "a b");
        assert_eq!(sanitize("a\tb"), "a b");
        assert_eq!(sanitize("a\u{00a7}cb"), "a cb", "§ is the Minecraft sign");
        assert_eq!(sanitize("a\u{2028}b\u{2029}c"), "a b c");
        // NEL (U+0085) is a control character too.
        assert_eq!(sanitize("a\u{0085}b"), "a b");
    }

    #[test]
    fn prepare_sanitizes_and_then_caps_the_length() {
        let c = ChatConfig {
            max_len: 5,
            ..ChatConfig::default()
        };
        assert_eq!(prepare("abc", &c), "abc");
        assert_eq!(
            prepare("abcdefgh", &c),
            "abcde…",
            "truncated with an ellipsis"
        );
        // The cap counts characters, not bytes: five `é`s are five characters.
        assert_eq!(prepare("éééééé", &c), "ééééé…");
        // Sanitizing happens first, so a newline does not eat the budget.
        assert_eq!(prepare("a\nbcdef", &c), "a bcd…");

        let unlimited = ChatConfig {
            max_len: 0,
            ..ChatConfig::default()
        };
        let long = "x".repeat(10_000);
        assert_eq!(prepare(&long, &unlimited).chars().count(), 10_000);
    }

    #[test]
    fn the_rate_limiter_allows_a_burst_then_refuses_the_over_bound() {
        let mut limiter = LogLimiter::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limiter.allow_at(3, t0));
        }
        assert!(
            !limiter.allow_at(3, t0),
            "the fourth in the window is refused"
        );
        // A later window frees the budget again.
        assert!(!limiter.allow_at(3, t0 + Duration::from_secs(59)));
        assert!(limiter.allow_at(3, t0 + RATE_WINDOW + Duration::from_secs(1)));
    }

    #[test]
    fn a_zero_rate_disables_the_limit() {
        let mut limiter = LogLimiter::new();
        let t0 = Instant::now();
        for _ in 0..10_000 {
            assert!(limiter.allow_at(0, t0));
        }
    }
}
