//! Repeatable microbenchmark for streaming frame selection, including queue reuse.
//!
//! Run with: cargo run -p mycode --release --example stream_frames --locked
//! This measures framing work only, not GPUI rendering or provider latency.

use std::collections::VecDeque;
use std::hint::black_box;
use std::time::Instant;

use mycode::app::BridgeEvent;
use mycode::frontend::stream::split_stream_frame;

fn measure(label: &str, fragments: usize, text: &str, iterations: usize) {
    let mut queued: VecDeque<_> = (0..fragments)
        .map(|_| BridgeEvent::ChatText {
            session_id: "benchmark".into(),
            delta: text.to_owned(),
        })
        .collect();
    let start = Instant::now();
    let mut selected = 0;
    for _ in 0..iterations {
        let (slice, rest) = split_stream_frame(black_box(queued));
        selected += black_box(slice.len());
        queued = slice.into_iter().chain(rest).collect();
    }
    println!(
        "{label}: {:.0} ns/iteration; selected={selected}",
        start.elapsed().as_nanos() as f64 / iterations as f64
    );
    black_box(queued);
}

fn main() {
    // Warm up allocation and code paths before reporting the same fixed inputs.
    measure("warmup", 32, "token", 10_000);
    for round in 1..=5 {
        println!("round={round}");
        measure("32 small deltas", 32, "token", 50_000);
        measure("64 KiB ASCII delta", 1, &"x".repeat(64 * 1024), 10_000);
        measure("1 MiB UTF-8 delta", 1, &"界".repeat(1024 * 1024 / 3), 5_000);
    }
}
