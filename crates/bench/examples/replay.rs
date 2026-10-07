//! Replays one workload through `clackatoo-term` in a loop, for profiling.
//!
//! ```sh
//! cargo run --release -p clackatoo-bench --example replay -- dense 20
//! ```
//!
//! plays `dense` for 20 seconds. Point a profiler at it. Where `perf` is
//! locked down, sampling backtraces with gdb works; see the
//! `profiling-without-perf` memory in Acta.

use std::time::{Duration, Instant};

use clackatoo_bench::workloads::{self, CHUNK_LEN};
use clackatoo_term::{Config, Terminal};

fn main() {
    let mut args = std::env::args().skip(1);
    let all = workloads::generated(4 << 20)
        .into_iter()
        .chain(workloads::recorded(4 << 20));
    let names: Vec<_> = all.clone().map(|workload| workload.name).collect();
    let Some(workload) = args
        .next()
        .and_then(|name| all.into_iter().find(|workload| workload.name == name))
    else {
        eprintln!("usage: replay WORKLOAD [SECONDS], where WORKLOAD is one of {names:?}");
        std::process::exit(2);
    };
    let seconds = args.next().map_or(Ok(10), |s| s.parse()).unwrap_or(10);

    let mut terminal = Terminal::new(workload.size, &Config::default());
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut bytes = 0;
    let started = Instant::now();
    while Instant::now() < end {
        for chunk in workload.bytes.chunks(CHUNK_LEN) {
            terminal.advance(chunk);
            drop(terminal.take_replies());
        }
        bytes += workload.bytes.len();
    }
    #[allow(clippy::cast_precision_loss)] // Far below 2^52 bytes.
    let rate = bytes as f64 / started.elapsed().as_secs_f64() / f64::from(1 << 20);
    println!("{}: {rate:.0} MiB/s", workload.name);
}
