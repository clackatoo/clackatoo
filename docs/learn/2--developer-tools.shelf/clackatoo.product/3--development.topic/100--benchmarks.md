---
title: Benchmarks
type: how-to
description: Run clackatoo's benchmarks, read the results, and compare them across changes. Emulator throughput is measured next to alacritty_terminal, and an end-to-end harness measures the real binary's startup, latency, throughput, memory and idle wakeups, and its window.
related:
  - clackatoo/reference/terminal-emulation
  - clackatoo/reference/recordings
---

clackatoo's goal is to be very fast and very light, so speed is measured, not assumed. There are two suites:

- **Emulator throughput** feeds terminal output straight into `clackatoo-term`, the crate that turns output into a grid of cells, and into `alacritty_terminal`, the emulator inside Alacritty, so the two can be compared on the same machine.
- **End to end** starts the real release binary and drives it the way a user would: it starts daemons, opens terminals and windows, types into them and floods them with output.

Both compare each run with the previous one, so a regression shows up as a number.

## Run everything

```sh
cargo bench
```

That builds in release mode and runs both suites, which takes a few minutes. To run one suite on its own:

```sh
cargo bench -p clackatoo-bench          # emulator throughput
cargo bench -p clackatoo --bench e2e    # end to end
```

Close anything busy first, such as builds, browsers playing video, or other benchmarks. A desktop's timings wander by a few percent anyway, with CPU frequency scaling and background load, and both suites treat changes under 5% as noise.

## Workloads

Both suites use the same workloads. They are generated in code from a fixed seed, so every run sees identical bytes and nothing large is committed. Each one leans on a different part of the emulator:

| Workload | Stands for |
|---|---|
| `ascii` | Plain text scrolling, like `cat` of a log. Lines vary in length, and some wrap. |
| `highlight` | Words in 24-bit colours, some bold, like syntax-highlighted code. |
| `dense` | A different style in every cell, redrawing the whole screen. The worst case for style handling. |
| `region` | Scrolling inside a scroll region, the way pagers and editors scroll part of the screen. |
| `unicode` | Wide CJK characters and emoji, combining accents, and emoji joined with ZWJ (zero width joiner). |
| `cursor` | Short writes at random places, sometimes recoloured, like a full-screen program redrawing. |
| `vim` | A recorded vim session (open, edit, quit), repeated. Emulator suite only. |

Generated workloads play in a 120x40 terminal, a typical window. The vim recording plays at the size it was recorded at.

## Emulator throughput

Each benchmark feeds a workload to one emulator in 64 KiB pieces, the size the daemon reads from a PTY at a time. The emulator is reused from one iteration to the next, so its 10,000-line scrollback is full and every new line pushes an old one out. That's the steady state of a terminal that has been open a while. Neither emulator does any I/O or drawing.

For each workload there are three results: `clackatoo`, `clackatoo-eager` and `alacritty`. A flood never pauses, so by default clackatoo never compacts its scrollback during the benchmark, much as alacritty never does. `clackatoo-eager` compacts every line as it scrolls off, so the cost of compacting stays measured.

```text
ascii/clackatoo         time:   [23.467 ms 23.522 ms 23.601 ms]
                        thrpt:  [84.745 MiB/s 85.029 MiB/s 85.228 MiB/s]
ascii/alacritty         time:   [15.675 ms 15.724 ms 15.780 ms]
                        thrpt:  [126.74 MiB/s 127.20 MiB/s 127.60 MiB/s]
```

`thrpt` is throughput: how much output the emulator gets through per second. The middle number is the best estimate, and the outer two bound it. The benchmarks use [criterion](https://github.com/criterion-rs/criterion.rs), which also prints how each result changed since the last run.

Before benchmarking a workload, the suite plays it through both emulators and checks they end up showing the same screen. Otherwise the two wouldn't be doing the same work. One difference is expected: alacritty draws a ZWJ emoji sequence such as 👩‍💻 as two separate emoji, while clackatoo joins it into one, as kitty, WezTerm and Ghostty do. So on `unicode`, alacritty's lines wrap sooner, and the suite prints a note saying so. Any other difference prints a warning with both screens.

alacritty also records which lines changed as it goes, for its renderer. clackatoo doesn't do that yet, so expect the gap to narrow a little once it does.

## End to end

The harness runs as a bench target of the `clackatoo` package. That way, cargo always builds a fresh release binary before measuring it. Every daemon it starts gets a temporary runtime directory of its own, so it never touches a daemon you are using. It prints one table:

```text
clackatoo end-to-end benchmarks
binary: /home/you/clackatoo/target/release/clackatoo
compared with baseline "base": commit d8c3b98, saved 2 hours ago

startup
  daemon start to accepting clients            677.2 µs  ±10.1%  -2.0% (noise)
```

Each line shows the median of several runs, then the spread: half the distance between the 10th and 90th percentiles, as a percentage of the median. The last column compares the result with the baseline.

What each group measures:

| Group | Measurement | How |
|---|---|---|
| `size` | Release binary | The size of `target/release/clackatoo`. |
| `startup` | Daemon start to accepting clients | From launching `clackatoo daemon` until a client can connect and say hello. |
| | CLI command | `clackatoo list`, from launch to exit, against a running daemon. |
| | New terminal | From asking for a terminal until it's running. |
| | New terminal to first text | Until your `$SHELL` shows any text, usually its prompt. Mostly the shell's own startup time. |
| `latency` | Keypress to echo in the grid | A program that echoes keys runs in a terminal. One key is sent, and the time is measured until a screen capture shows the echoed key. |
| | Screen capture round trip | One screen capture request and response. |
| | Keypress to view update | The same, following the terminal's view as the window does: timed until the update showing the echo arrives. This is the part of keypress-to-screen latency the daemon owns. |
| | Keypress to view update, while another floods | The same while a second terminal prints without end, which must not hold up the one being typed in. |
| | Keypress to view update, through the bridge | The same through `clackatoo bridge`, as [`--remote`](~clackatoo/reference/remote) connects, over QUIC, but with no ssh or network in between: what the bridge and QUIC add. |
| | Keypress to view update, through the bridge, while a followed terminal floods | The same while a second terminal the client follows prints without end, so its updates share the connection with the echoes. The harness acknowledges each of the flood's updates at once, so this median also carries one flood update per key; a window acknowledges once a frame. |
| `throughput` | Each workload | A script prints 16 MiB of the workload into a terminal. Timed from starting it until the daemon has processed everything. |
| | Bare PTY | The same script, read straight from a PTY and thrown away. This is the most any terminal could manage on this machine. |
| `cpu` | Daemon CPU per MiB | For each throughput workload, the CPU time the daemon spent per MiB of output, read to the nanosecond from `/proc`. Throughput stops at the PTY's limit, so this is where the daemon's own speed shows. |
| | Bare PTY reader per MiB | The CPU time the bare PTY reader spends per MiB: the floor, nearly all of it in the kernel's TTY layer. |
| `memory` | Daemon at rest | The daemon's resident memory with no terminals. |
| | Per terminal | The growth from opening 20 empty terminals, divided by 20. The programs running in them aren't counted. |
| | Per scrollback line | The growth from filling an 80-column terminal's scrollback with full lines, the worst case, divided by the number of lines. Measured once the daemon has compacted the scrollback, 300 ms after the output ends. |
| | Per scrollback line, before compaction | The same, measured as soon as the output ends. |
| | Per scrollback line, typical | The same as the first, with lines of varying length, averaging half the width. |
| `idle` | Wakeups | How often the daemon wakes up per second, with a client connected and three quiet terminals, one of which printed some history before going quiet. This should be zero. It is counted from the context switches in `/proc`. |
| `window` | Start to first frame | From starting `clackatoo` to the window's first frame, with the daemon already running. Most of it is the GPU driver starting up. |
| | Frame drawn, during a flood | How long the window takes over each frame, from starting on it to handing it to the compositor, while its terminal prints for two seconds without stopping. |
| | Wakeups of the window's own threads | How often the window wakes up per second with nothing to draw. This should be zero. |
| | Wakeups, the GPU driver's threads too | The same, counting threads the GPU driver started inside the window. Some drivers wake up on their own: NVIDIA's Vulkan driver about 110 times a second, even with no window at all. |

Most groups learn that a terminal has caught up by polling it. Throughput runs watch the terminal's title, which the script sets when it finishes, every 200 µs. The first keypress test captures the screen back to back, so its result includes up to one capture round trip on top of the true latency; the view-update tests don't poll.

The `window` group opens real windows, so it needs a display. Without one (no `WAYLAND_DISPLAY` or `DISPLAY`), it says so and is skipped. The windows open on the harness's own daemons, never on yours. The window logs its first frame, and how long each frame took when started with `RUST_LOG=clackatoo_gui::frame=trace`; the harness reads those from its output.

To run some groups only, name them:

```sh
cargo bench -p clackatoo --bench e2e -- startup memory
```

To measure a different build, such as another branch's, point the harness at its binary:

```sh
cargo bench -p clackatoo --bench e2e -- --binary ../other/target/release/clackatoo
```

## Compare changes

Both suites work like criterion's baselines:

- By default, each run is compared with the previous run and replaces it.
- `--save-baseline NAME` compares with the baseline called `NAME` and replaces it.
- `--baseline NAME` compares with `NAME` without saving anything.

To measure a change, save a baseline before making it, then compare against it afterwards:

```sh
cargo bench -- --save-baseline before
# … make the change …
cargo bench -- --baseline before
```

A change counts when it is bigger than 5% and outside both runs' spreads. Otherwise it's marked `(noise)`. Measurements that don't vary, like the binary's size, report any change.

Criterion keeps its baselines in `target/criterion/`. The end-to-end harness keeps its own in `target/clackatoo-bench/` as tab-separated files, along with the commit they were measured on.

## Profile a workload

To see where the time goes, replay one workload through the emulator in a loop and point a profiler at it:

```sh
cargo run --release -p clackatoo-bench --example replay -- dense 20
```

That plays `dense` for 20 seconds and prints its throughput. Release builds use link-time optimisation, so build with `CARGO_PROFILE_RELEASE_DEBUG=true` to get the function names back in a profiler.
