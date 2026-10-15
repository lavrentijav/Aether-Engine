//! The `aether-server` binary. Everything lives in the [`aether_server`]
//! library so the visibility/block-write hot path can be benchmarked without
//! starting a server.

fn main() -> std::process::ExitCode {
    aether_server::run()
}
