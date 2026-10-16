//! Parse and suggest cost of the command tree.
//!
//! The tree is built once and read forever, so what matters is that a parse
//! stays in the microsecond range as the tree grows past two hundred nodes,
//! and that a suggestion is cheap enough to run on every keypress. The same
//! grammar is exercised both ways: a literal chain then a greedy argument, and
//! an ambiguous prefix that must gather every `t…` command.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use aether_server::commands::tree::{suggest, tree, SuggestCtx};
use aether_server::players::{PlayerHandle, PosLook, Registry};
use aether_server::proto::Conn;
use aether_server::protocol::codec_for;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn handle() -> Arc<PlayerHandle> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
    let _peer = listener.accept().unwrap();
    Registry::default()
        .join(
            1,
            1,
            "bencher".into(),
            None,
            Vec::new(),
            codec_for(763).unwrap(),
            &client,
            PosLook {
                x: 0.0,
                y: 64.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
        )
        .unwrap()
}

fn bench_command_tree(c: &mut Criterion) {
    let handle = handle();
    let registry = Arc::new(Registry::default());
    let ctx = SuggestCtx {
        registry: &registry,
        world: None,
    };

    // Build once, outside the timed loop.
    let _ = tree();

    c.bench_function("parse/lookup_greedy", |b| {
        b.iter(|| {
            tree()
                .parse(
                    black_box("/lookup player:steve time:30m radius:20"),
                    &handle,
                )
                .unwrap()
        })
    });

    c.bench_function("parse/ambiguous_prefix", |b| {
        b.iter(|| tree().parse(black_box("/t"), &handle).is_err())
    });

    c.bench_function("suggest/ambiguous_prefix", |b| {
        b.iter(|| suggest(black_box("/t"), &handle, &ctx))
    });

    c.bench_function("suggest/gamemode_argument", |b| {
        b.iter(|| suggest(black_box("/gamemode "), &handle, &ctx))
    });
}

criterion_group!(benches, bench_command_tree);
criterion_main!(benches);
