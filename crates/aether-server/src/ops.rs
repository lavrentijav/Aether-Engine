//! Running the server as a service: a clean stop on SIGINT/SIGTERM, a
//! watchdog on the game tick, a periodic status line, and columns leaving
//! memory once no player needs them.
//!
//! The process used to have no signal handler, so every stop was a kill that
//! lost up to an autosave interval of edits, and nothing noticed when the
//! world stopped ticking — the process was alive, the port listened, the CPU
//! sat at zero. Now a signal saves the players and flushes the world before
//! exiting, and a tick that does not advance is logged and, past
//! `watchdog_secs`, ends the process so a supervisor (`deploy/aether.service`)
//! can start a fresh one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::players::SharedRegistry;
use crate::session::DemoWorld;

static STOP: AtomicBool = AtomicBool::new(false);

/// Columns unloaded since startup, for the status line.
static UNLOADED: AtomicU64 = AtomicU64::new(0);

/// How often columns are swept out of memory.
const SWEEP: Duration = Duration::from_secs(10);
/// Sweeps a column must go untouched before it may leave for not being
/// needed: half a minute, so walking back and forth across a boundary does
/// not load the same columns again and again.
const IDLE_SWEEPS: u64 = 2;

/// Drop columns from memory that no player needs, every [`SWEEP`], and keep
/// at most `budget` resident (`0`: no limit).
///
/// A player needs the columns within their view radius, plus a ring for
/// what they are about to walk into. Columns with unsaved edits stay until
/// the autosave has written them; anything touched since the last sweeps —
/// by a mob, a falling block, a fluid — stays too.
pub fn spawn_residency(registry: SharedRegistry, world: Arc<DemoWorld>, budget: usize) {
    std::thread::Builder::new()
        .name("residency".into())
        .spawn(move || loop {
            std::thread::sleep(SWEEP);
            let views: Vec<(i32, i32, i32)> = registry
                .snapshot()
                .iter()
                .map(|p| {
                    let pos = p.pos();
                    (
                        pos.x.floor() as i32 >> 4,
                        pos.z.floor() as i32 >> 4,
                        p.view_radius() + 1,
                    )
                })
                .collect();
            let keep = |cx: i32, cz: i32| {
                views
                    .iter()
                    .any(|&(px, pz, r)| (cx - px).abs() <= r && (cz - pz).abs() <= r)
            };
            let s = world.unload(&keep, budget, IDLE_SWEEPS);
            let gone = s.unneeded + s.over_budget;
            UNLOADED.fetch_add(gone as u64, Ordering::Relaxed);
            if gone >= TRIM_AFTER {
                release_freed_memory();
            }
        })
        .expect("failed to start the residency sweep");
}

/// Columns a sweep must free before the allocator is asked to hand memory
/// back: a few MiB at least, so the trim is worth its pass over the heap.
const TRIM_AFTER: usize = 256;

/// Give freed heap back to the operating system.
///
/// glibc keeps what a program frees for reuse, so after a few thousand
/// columns leave memory the resident set stays where it peaked — reused for
/// the next columns, but invisible to `MemoryMax` and to anyone reading `top`.
/// A trim returns it.
fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only walks the allocator's own free lists.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// This process's resident set size in bytes, where the platform says.
pub fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: `sysconf` only reads a configuration value.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some(pages * u64::try_from(page).ok()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What one resident column of vanilla terrain costs: the status line's
/// resident-bytes figure over its column count, measured over 5 000 columns
/// at radius 64 (about 22 KiB). Noise terrain is a fraction of it.
const BYTES_PER_COLUMN: u64 = 24 * 1024;

/// The startup line saying what `view_radius` costs in memory.
pub fn radius_estimate(cfg: &crate::config::ServerConfig) -> String {
    let r = cfg.view_radius() as u64;
    let columns = (2 * r + 1).pow(2);
    let budget = match cfg.max_resident_columns {
        0 => "no column limit".to_string(),
        n => format!(
            "at most {n} column(s), ~{} MiB",
            mib(n as u64 * BYTES_PER_COLUMN)
        ),
    };
    format!(
        "view radius: {r} — up to {columns} column(s), ~{} MiB, for each player standing \
         apart from the others; {budget}",
        mib(columns * BYTES_PER_COLUMN)
    )
}

/// `n` bytes in MiB, for the log.
fn mib(n: u64) -> u64 {
    n / (1024 * 1024)
}

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
pub fn spawn_monitor(
    registry: SharedRegistry,
    world: Arc<DemoWorld>,
    watchdog_secs: u64,
    status_secs: u64,
) {
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
                    let rss = rss_bytes()
                        .map(|b| format!("{} MiB", mib(b)))
                        .unwrap_or_else(|| "?".into());
                    crate::log::info(&format!(
                        "status: {} player(s), {tps:.1} TPS, slowest tick {:.1} ms, \
                         {} KiB waiting to be sent; memory {rss}, {} column(s) / {} section(s) \
                         resident ({} MiB), {} unloaded so far",
                        players.len(),
                        slowest as f64 / 1000.0,
                        backlog / 1024,
                        world.resident_columns(),
                        world.resident_sections(),
                        mib(world.resident_bytes() as u64),
                        UNLOADED.load(Ordering::Relaxed),
                    ));
                    status_from = (Instant::now(), age);
                }
            }
        })
        .expect("failed to start the monitor");
}
