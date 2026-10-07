//! End-to-end benchmarks of the `clackatoo` binary; see
//! [`clackatoo_bench::e2e`]. They live in this package because only here
//! does cargo build the binary first and say where it is.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    clackatoo_bench::e2e::main(&clackatoo_bench::e2e::Paths {
        binary: Path::new(env!("CARGO_BIN_EXE_clackatoo")),
        results: &Path::new(env!("CARGO_TARGET_TMPDIR")).with_file_name("clackatoo-bench"),
        repo: Path::new(env!("CARGO_MANIFEST_DIR")),
    })
}
