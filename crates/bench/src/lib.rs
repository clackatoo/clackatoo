//! Benchmarks for clackatoo.
//!
//! - [`workloads`]: the output every benchmark feeds a terminal, generated in
//!   code.
//! - `benches/throughput.rs`: how fast `clackatoo-term` processes each
//!   workload, next to `alacritty_terminal`. Run it with
//!   `cargo bench -p clackatoo-bench`.
//! - [`e2e`]: the end-to-end harness, which measures the real binary. Run it
//!   with `cargo bench -p clackatoo --bench e2e`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod e2e;
pub mod workloads;
