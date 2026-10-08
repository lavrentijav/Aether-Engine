//! Running the server as a service: a clean stop on SIGINT/SIGTERM, a
//! watchdog on the game tick, and a periodic status line.
//!
//! The process used to have no signal handler, so every stop was a kill that
//! lost up to an autosave interval of edits, and nothing noticed when the
//! world stopped ticking — the process was alive, the port listened, the CPU
//! sat at zero. Now a signal saves the players and flushes the world before
//! exiting, and a tick that does not advance is logged and, past
//! `watchdog_secs`, ends the process so a supervisor (`deploy/aether.service`)
//! can start a fresh one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::players::SharedRegistry;
use crate::session::DemoWorld;

static STOP: AtomicBool = AtomicBool::new(false);

/// Ask for SIGINT and SIGTERM to stop the server cleanly. A second signal,
/// once the first is being handled, kills the process the default way.
pub fn install_signal_handlers() {
    #[cfg(unix)]
    {
        extern "C" fn on_signal(sig: libc::c_int) {
            if STOP.swap(true, Ordering::SeqCst) {
                // Already stopping and asked again: give up waiting.
                // SAFETY: `_exit` is async-signal-safe.
                unsafe { libc::_exit(128 + sig) };
            }
        }
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for sig in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: the handler only stores to an atomic or calls `_exit`,
            // both async-signal-safe.
            unsafe { libc::signal(sig, handler) };
        }
    }
}

/// Watch for a stop request; on one, save everyone and the world and exit.
pub fn spawn_shutdown(registry: SharedRegistry, world: Arc<DemoWorld>) {
    std::thread::Builder::new()
        .name("shutdown".into())
        .spawn(move || {
            while !STOP.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
            crate::log::info("stopping: saving players and the world");
            let players = registry.snapshot();
            for p in &players {
                if p.full() {
                    crate::session::save_inventory(p, &world);
                    crate::session::save_player(p, &world);
                }
                p.disconnect("server stopping");
            }
            crate::game::tick::save_time(&world);
            match world.flush() {
                Ok(()) => crate::log::info(&format!(
                    "stopped cleanly ({} player(s) saved)",
                    players.len()
                )),
                Err(e) => crate::log::error(&format!("stopping: the world did not flush: {e}")),
            }
            std::process::exit(0);
        })
        .expect("failed to start the shutdown watcher");
}

/// Watch the game tick, and log a status line every `status_secs`.
///
/// A stall of five seconds is logged as a warning (repeated while it lasts);
/// one of `watchdog_secs` exits the process with a failure, for a supervisor
/// to restart. `watchdog_secs = 0` only warns.
pub fn spawn_monitor(registry: SharedRegistry, watchdog_secs: u64, status_secs: u64) {
    std::thread::Builder::new()
        .name("monitor".into())
        .spawn(move || {
            let mut last_age = crate::game::now();
            let mut last_advance = Instant::now();
            let mut warned_at = 0u64;
            let mut status_from = (Instant::now(), last_age);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let age = crate::game::now();
                if age != last_age {
                    if warned_at > 0 {
                        crate::log::warn(&format!(
                            "game tick resumed after {}s",
                            last_advance.elapsed().as_secs()
                        ));
                    }
                    last_age = age;
                    last_advance = Instant::now();
                    warned_at = 0;
                } else {
                    let stalled = last_advance.elapsed().as_secs();
                    if stalled >= 5 && stalled >= warned_at + 5 {
                        warned_at = stalled;
                        crate::log::warn(&format!("game tick stalled for {stalled}s"));
                    }
                    if watchdog_secs > 0 && stalled >= watchdog_secs {
                        crate::log::error(&format!(
                            "game tick stalled for {stalled}s; exiting for a restart"
                        ));
                        std::process::exit(2);
                    }
                }
                if status_secs > 0 && status_from.0.elapsed() >= Duration::from_secs(status_secs) {
                    let secs = status_from.0.elapsed().as_secs_f64();
                    let tps = (age - status_from.1) as f64 / secs;
                    let slowest = crate::game::SLOWEST_TICK_US.swap(0, Ordering::Relaxed);
                    let players = registry.snapshot();
                    let backlog: usize = players.iter().map(|p| p.queued_bytes()).sum();
                    crate::log::info(&format!(
                        "status: {} player(s), {tps:.1} TPS, slowest tick {:.1} ms, \
                         {} KiB waiting to be sent",
                        players.len(),
                        slowest as f64 / 1000.0,
                        backlog / 1024
                    ));
                    status_from = (Instant::now(), age);
                }
            }
        })
        .expect("failed to start the monitor");
}
