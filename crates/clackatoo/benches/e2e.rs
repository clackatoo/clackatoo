//! End-to-end benchmarks of the `clackatoo` binary; see
//! [`clackatoo_bench::e2e`]. They live in this package because only here
//! does cargo build the binary first and say where it is, and only here is
//! the window's crate, which builds frames for the keystroke gate.

use std::path::Path;
use std::process::ExitCode;

use clackatoo_protocol::Size;

fn main() -> ExitCode {
    clackatoo_bench::e2e::main(&clackatoo_bench::e2e::Paths {
        binary: Path::new(env!("CARGO_BIN_EXE_clackatoo")),
        results: &Path::new(env!("CARGO_TARGET_TMPDIR")).with_file_name("clackatoo-bench"),
        repo: Path::new(env!("CARGO_MANIFEST_DIR")),
        gate: Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/gate.tsv")),
        frames,
    })
}

/// Frames of a terminal of `size` that `output` was played into, as the
/// window builds them, if there's a GPU.
fn frames(size: Size, output: &[u8]) -> Option<Box<dyn FnMut()>> {
    let mut bench = clackatoo_gui::FrameBench::new(size.cols(), size.rows(), output)?;
    Some(Box::new(move || bench.frame()))
}
