#[path = "common/mod.rs"]
mod support;

use criterion::{criterion_group, criterion_main, Criterion};

/// In-memory source → destination relay copy throughput.
fn bench_relay_copy(c: &mut Criterion) {
    support::bench_copy(c, "relay_copy", "memory_16mib", 16 * 1024 * 1024);
    support::bench_copy(c, "relay_copy", "memory_64mib", 64 * 1024 * 1024);
}

criterion_group!(benches, bench_relay_copy);
criterion_main!(benches);
