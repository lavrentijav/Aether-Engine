//! The server console: stdin lines are commands.
//!
//! The vanilla console types commands **without** a leading slash (`stop`,
//! `save-all`), so every non-empty line here is a command rather than chat.
//! The line is dispatched through the one tree, as the console source: op
//! level 4, no entity and no position. Feedback goes to stdout through the
//! server's logger.
//!
//! The reader is split from the dispatch so it can be tested with an injected
//! reader and writer; the production path is [`spawn`], reading the process's
//! stdin on a thread of its own. EOF (a piped or closed stdin) ends the reader
//! cleanly — it must not spin.

use std::io::{self, BufRead};

use crate::commands::tree::execute_feedback_streaming;
use crate::commands::{tree::ServerKind, tree::Source};
use crate::players::SharedRegistry;
use crate::session::DemoWorld;

/// Run one console line and return its reply lines. An empty line produces
/// nothing (vanilla ignores a bare Enter).
pub fn run_line(line: &str, registry: &SharedRegistry, world: &DemoWorld) -> Vec<String> {
    if line.trim().is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    run_line_streaming(line, registry, world, &mut |line| lines.push(line));
    lines
}

/// Run one console line, handing each line to `emit` **as it is produced**.
///
/// `/save-all` reports `Saving the game …` before it writes and `Saved the
/// game` after, so a console shows the same progress a vanilla one does.
fn run_line_streaming(
    line: &str,
    registry: &SharedRegistry,
    world: &DemoWorld,
    emit: &mut dyn FnMut(String),
) {
    if line.trim().is_empty() {
        return;
    }
    let source = Source::Server(ServerKind::Console);
    // The console types commands without a slash; the streaming entry point
    // (like the player one) expects one.
    let text = if line.trim_start().starts_with('/') {
        line.to_string()
    } else {
        format!("/{}", line.trim_start())
    };
    let mut progress = |line: &str| emit(line.to_string());
    if let Some(feedback) =
        execute_feedback_streaming(&text, source, registry, world, &mut progress)
    {
        for line in feedback.lines {
            emit(line);
        }
    }
}

/// Read lines from `reader` until EOF, running each as a console command and
/// handing every reply line to `emit`.
///
/// Returns `Ok(())` at EOF (including a closed pipe); a genuine read error is
/// returned so the caller can log it. This is the testable core of [`spawn`].
pub fn serve<R: BufRead>(
    reader: R,
    registry: &SharedRegistry,
    world: &DemoWorld,
    mut emit: impl FnMut(String),
) -> io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        run_line_streaming(&line, registry, world, &mut emit);
    }
    Ok(())
}

/// Start the console reader on its own thread, reading the process's stdin and
/// logging replies through the server logger.
pub fn spawn(registry: SharedRegistry, world: std::sync::Arc<DemoWorld>) {
    let spawned = std::thread::Builder::new()
        .name("console".into())
        .spawn(move || {
            let stdin = io::stdin();
            let result = serve(stdin.lock(), &registry, &world, |reply| {
                // The logger prefixes the time and level, so console output is
                // indistinguishable from the server's own lines — which is
                // what "printed to stdout via the existing logger" means.
                crate::log::info(&reply);
            });
            if let Err(e) = result {
                crate::log::warn(&format!("console: stdin reader stopped: {e}"));
            }
        });
    if let Err(e) = spawned {
        crate::log::warn(&format!("console: cannot start the stdin reader: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::Registry;
    use std::io::Cursor;
    use std::sync::Arc;

    /// A throwaway world on a temp store, unique per call.
    fn test_world() -> DemoWorld {
        use aether_api::{FjallStore, World};
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let tag = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aether-console-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", tag).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, tag, revision, &cfg);
        World::new(store, generator)
    }

    fn registry() -> SharedRegistry {
        Arc::new(Registry::default())
    }

    #[test]
    fn a_console_line_runs_a_command_without_a_slash() {
        let world = test_world();
        let registry = registry();
        // `/help` is public and works from any source; the console's op level
        // is not needed to reach it. The point is that no leading slash is
        // required and the line still went through the tree. CMD-05 renders
        // `/help` from the tree, so the list names real commands.
        let out = run_line("help", &registry, &world);
        assert!(!out.is_empty(), "help produced output");
        assert!(
            out.iter().any(|l| l.starts_with("/gamemode")),
            "the help list is present: {out:?}"
        );

        // A leading slash is accepted too.
        let out2 = run_line("/help", &registry, &world);
        assert_eq!(out, out2, "a leading slash changes nothing");
    }

    #[test]
    fn an_unknown_console_command_gets_the_player_error() {
        let world = test_world();
        let registry = registry();
        // The same message a player gets for an unknown command.
        assert_eq!(
            run_line("definitely_not_a_command", &registry, &world),
            vec!["Unknown or incomplete command".to_string()]
        );
    }

    #[test]
    fn the_console_runs_an_op_level_4_command() {
        let world = test_world();
        let registry = registry();
        // `/save-all` is op level 4: only the console/RCON owner level reaches
        // it, which is what makes the console an admin source.
        assert_eq!(
            run_line("/save-all", &registry, &world),
            vec![
                "Saving the game (this may take a moment!)".to_string(),
                "Saved the game".to_string()
            ]
        );
    }

    #[test]
    fn a_console_only_command_needing_a_player_says_so() {
        let world = test_world();
        let registry = registry();
        // `/give` is a player command: from the console there is no target, so
        // it must not panic or silently act on nobody.
        let out = run_line("/give stone 1", &registry, &world);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("needs a player"), "{out:?}");
    }

    #[test]
    fn the_injected_reader_emits_replies_and_exits_at_eof() {
        let world = test_world();
        let registry = registry();
        let input = "help\n\n   \ndefinitely_not_a_command\n";
        let mut lines = Vec::new();
        serve(Cursor::new(input), &registry, &world, |l| lines.push(l)).unwrap();
        // The blank and whitespace-only lines produced nothing; the unknown
        // command produced its one error line.
        assert!(
            lines.iter().any(|l| l == "Unknown or incomplete command"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("/gamemode")),
            "{lines:?}"
        );
        // Exactly one error line: the blank lines were not turned into
        // commands.
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.as_str() == "Unknown or incomplete command")
                .count(),
            1
        );
    }

    #[test]
    fn an_empty_pipe_end_exits_cleanly() {
        let world = test_world();
        let registry = registry();
        let mut count = 0;
        // No lines at all: this must return immediately rather than loop.
        serve(Cursor::new(""), &registry, &world, |_| count += 1).unwrap();
        assert_eq!(count, 0);
    }
}
