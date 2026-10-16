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

/// Log a public chat line and journal it.
pub fn log_chat(handle: &PlayerHandle, text: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log {
        return;
    }
    crate::log::info(&format!("[chat] <{}> {}", handle.name, text));
    append(world, handle, ChatKind::Public, ActorId::SERVER, text);
}

/// Log a private message and journal it (with the recipient as `peer`).
pub fn log_private(handle: &PlayerHandle, target: &PlayerHandle, text: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log {
        return;
    }
    crate::log::info(&format!(
        "[pm] {} -> {}: {}",
        handle.name, target.name, text
    ));
    append(world, handle, ChatKind::Private, ActorId(target.uuid), text);
}

/// Log a command line (redacted) and journal it.
pub fn log_command(handle: &PlayerHandle, line: &str, world: &DemoWorld) {
    let cfg = crate::chat_config();
    if !cfg.log_commands {
        return;
    }
    let safe = redact(line, &cfg);
    crate::log::info(&format!("[cmd] {}: {}", handle.name, safe));
    append(world, handle, ChatKind::Command, ActorId::SERVER, &safe);
}

/// The command name of a typed line, without the leading slash: `msg` for
/// `/msg Bob hi`. Empty for a line that is only a slash.
pub fn command_name(line: &str) -> &str {
    line.trim_start()
        .strip_prefix('/')
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("")
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
        }
    }

    #[test]
    fn a_command_name_is_its_first_token_without_the_slash() {
        assert_eq!(command_name("/msg Bob hi"), "msg");
        assert_eq!(command_name("  /give stone"), "give");
        assert_eq!(command_name("/"), "");
        assert_eq!(command_name("not a command"), "");
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
}
