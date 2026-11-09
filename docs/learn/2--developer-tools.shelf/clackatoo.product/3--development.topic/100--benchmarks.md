---
title: Benchmarks
type: how-to
description: Run clackatoo's benchmarks, read the results, and compare them across changes. Emulator throughput is measured next to alacritty_terminal, and an end-to-end harness measures the real binary's startup, latency, throughput, memory and idle wakeups, and its window.
related:
  - clackatoo/reference/terminal-emulation
  - clackatoo/reference/recordings
---

clackatoo's goal is to be very fast and very light, so speed is measured, not
assumed. There are two suites:

- **Emulator throughput** feeds terminal output straight into
  `clackatoo-term`, the crate that turns output into a grid of cells, and into
  `alacritty_terminal`, the emulator inside Alacritty, so the two can be
  compared on the same machine.
- **End to end** starts the real release binary and drives it the way a user
  would: it starts daemons, opens terminals and windows, types into them and
  floods them with output.

Both compare each run with the previous one, so a regression shows up as a
number.

## Run everything

```sh
cargo bench
```

That builds in release mode and runs both suites, which takes a few minutes.
To run one suite on its own:

```sh
cargo bench -p clackatoo-bench          # emulator throughput
cargo bench -p clackatoo --bench e2e    # end to end
```

Close anything busy first, such as builds, browsers playing video, or other
benchmarks. A desktop's timings wander by a few percent anyway, with CPU
frequency scaling and background load, and both suites treat changes under 5%
as noise.

## Workloads

Both suites use the same workloads. They are generated in code from a fixed
seed, so every run sees identical bytes and nothing large is committed. Each
one leans on a different part of the emulator:

| Workload | Stands for |
|---|---|
| `ascii` | Plain text scrolling, like `cat` of a log. Lines vary in length, and some wrap. |
| `highlight` | Words in 24-bit colors, some bold, like syntax-highlighted code. |
| `dense` | A different style in every cell, redrawing the whole screen. The worst case for style handling. |
| `region` | Scrolling inside a scroll region, the way pagers and editors scroll part of the screen. |
| `unicode` | Wide CJK characters and emoji, combining accents, and emoji joined with ZWJ (zero width joiner). |
| `cursor` | Short writes at random places, sometimes recolored, like a full-screen program redrawing. |
| `vim` | A recorded vim session (open, edit, quit), repeated. Emulator suite only. |

Generated workloads play in a 120x40 terminal, a typical window. The vim
recording plays at the size it was recorded at.

## Emulator throughput

Each benchmark feeds a workload to one emulator in 64 KiB pieces, the size the
daemon reads from a PTY at a time. The emulator is reused from one iteration
to the next, so its 10,000-line scrollback is full and every new line pushes
an old one out. That's the steady state of a terminal that has been open a
while. Neither emulator does any I/O or drawing.

For each workload there are three results: `clackatoo`, `clackatoo-eager`, and
`alacritty`. A flood never pauses, so by default clackatoo never compacts its
scrollback during the benchmark, much as alacritty never does.
`clackatoo-eager` compacts every line as it scrolls off, so the cost of
compacting stays measured.

```text
ascii/clackatoo         time:   [23.467 ms 23.522 ms 23.601 ms]
                        thrpt:  [84.745 MiB/s 85.029 MiB/s 85.228 MiB/s]
ascii/alacritty         time:   [15.675 ms 15.724 ms 15.780 ms]
                        thrpt:  [126.74 MiB/s 127.20 MiB/s 127.60 MiB/s]
```

`thrpt` is throughput: how much output the emulator gets through per second.
The middle number is the best estimate, and the outer two bound it. The
benchmarks use [criterion](https://github.com/criterion-rs/criterion.rs),
which also prints how each result changed since the last run.

Before benchmarking a workload, the suite plays it through both emulators and
checks they end up showing the same screen. Otherwise the two wouldn't be
doing the same work. One difference is expected: alacritty draws a ZWJ emoji
sequence such as 👩&#x200D;💻 as two separate emoji, while clackatoo joins it
into one, as kitty, WezTerm and Ghostty do. So on `unicode`, alacritty's lines
wrap sooner, and the suite prints a note saying so. Any other difference
prints a warning with both screens.

alacritty also records which lines changed as it goes, for its renderer.
clackatoo doesn't do that, so the gap would narrow a little if it did.

## End to end

The harness runs as a bench target of the `clackatoo` package. That way, cargo
always builds a fresh release binary before measuring it. Every daemon it
starts gets a temporary runtime directory of its own, so it never touches a
daemon you are using. It prints one table:

```text
clackatoo end-to-end benchmarks
binary: /home/you/clackatoo/target/release/clackatoo
compared with baseline "base": commit d8c3b98, saved 2 hours ago

startup
  daemon start to accepting clients            677.2 µs  ±10.1%  -2.0% (noise)
```

Each line shows the median of several runs, then the spread: half the distance
between the 10th and 90th percentiles, as a percentage of the median. The last
column compares the result with the baseline.

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
| `cpu` | Daemon CPU per MiB | For each throughput workload, the CPU time the daemon spent per MiB of output, read to the nanosecond from `/proc`, in the same runs as `throughput`, which is the group to name for it. Throughput stops at the PTY's limit, so this is where the daemon's own speed shows. |
| | Bare PTY reader per MiB | The CPU time the bare PTY reader spends per MiB: the floor, nearly all of it in the kernel's TTY layer. |
| `memory` | Daemon at rest | The daemon's resident memory with no terminals. |
| | Per terminal | The growth from opening 20 empty terminals, divided by 20. The programs running in them aren't counted. |
| | Per scrollback line | The growth from filling an 80-column terminal's scrollback with full lines, the worst case, divided by the number of lines. Measured once the daemon has compacted the scrollback, 300 ms after the output ends. |
| | Per scrollback line, before compaction | The same, measured as soon as the output ends. |
| | Per scrollback line, typical | The same as the first, with lines of varying length, averaging half the width. |
| `idle` | Wakeups | How often the daemon wakes up per second, with a client connected and three quiet terminals, one of which printed some history before going quiet. This should be zero. It is counted from the context switches in `/proc`. |
| `gate` | Keypress to view update (cat), median and 99th percentile | The [keystroke gate](#the-keystroke-gate)'s first measurement: rounds of 1,000 keys typed into `cat`, each round on a new daemon, a pause apart. Each line gives the median of the rounds' medians, or of their 99th percentiles. |
| | A terminal's frame built, median and 99th percentile | Its second: how long the window takes to turn a 120x40 screen of coloured text into the rectangles and glyphs it draws, in rounds of 1,000 frames, typing changing the screen between them. Without a GPU, it's skipped. |
| `window` | Start to first frame | From starting `clackatoo` to the window's first frame, in a process of its own (`CLACKATOO_WINDOW_HOST=off`), with the daemon already running. Most of it is the GPU driver starting up. |
| | Start to first frame, first window of a process of windows | From starting `clackatoo` to the first frame of the first window, as by default: it forks the [process of windows](~clackatoo/reference/frame#windows-share-one-process), which sets up the GPU and opens it. The process is ended after each. |
| | Start to first frame, another window | From starting `clackatoo` to the first frame of a window it hands to the [process of windows](~clackatoo/reference/frame#windows-share-one-process) with a window open already, which opens it on the GPU it set up then, as the process's log says. |
| | Frame drawn, during a flood | How long the window takes over each frame, from starting on it to handing it to the compositor, while its terminal prints for two seconds without stopping. |
| | Wakeups of the window's own threads | How often the window wakes up per second with nothing to draw. This should be zero. |
| | Wakeups, the GPU driver's threads too | The same, counting threads the GPU driver started inside the window. Some drivers wake up on their own: NVIDIA's Vulkan driver about 110 times a second, even with no window at all. |

Most groups learn that a terminal has caught up by polling it. Throughput runs
watch the terminal's title, which the script sets when it finishes, every 200
µs. The first keypress test captures the screen back to back, so its result
includes up to one capture round trip on top of the true latency; the
view-update tests don't poll.

The `window` group opens real windows, so it needs a display. Without one (no
`WAYLAND_DISPLAY` or `DISPLAY`), it says so and is skipped. The windows open
on the harness's own daemons, never on yours. The window logs its first frame,
and how long each frame took when started with
`RUST_LOG=clackatoo_gui::frame=trace`; the harness reads those from its
output.

To run some groups only, name them:

```sh
cargo bench -p clackatoo --bench e2e -- startup memory
```

To measure a different build, such as another branch's, point the harness at
its binary:

```sh
cargo bench -p clackatoo --bench e2e -- --binary ../other/target/release/clackatoo
```

## Compare changes

Both suites work like criterion's baselines:

- By default, each run is compared with the previous run and replaces it.
- `--save-baseline NAME` compares with the baseline called `NAME` and replaces
  it.
- `--baseline NAME` compares with `NAME` without saving anything.

To measure a change, save a baseline before making it, then compare against it
afterward:

```sh
cargo bench -- --save-baseline before
# … make the change …
cargo bench -- --baseline before
```

A change counts when it is bigger than 5% and outside both runs' spreads.
Otherwise it's marked `(noise)`. Measurements that don't vary, like the
binary's size, report any change.

Criterion keeps its baselines in `target/criterion/`. The end-to-end harness
keeps its own in `target/clackatoo-bench/` as tab-separated files, along with
the commit they were measured on.

## The keystroke gate

Typing must never get slower. The keystroke gate measures the two things
between a key and the screen that clackatoo's own code decides, and checks
them against a baseline kept in the repository, `crates/bench/gate.tsv`:

- **Key to echo:** a key typed into `cat` in a terminal, through the daemon,
  until the view update showing it arrives, as a window follows it.
- **Frame built:** the window turning a screen of text into what it draws.

`scripts/release` runs it before making a release, and stops if it fails:

```sh
CARGO_INCREMENTAL=0 cargo bench --profile gate -p clackatoo --bench e2e -- --gate
```

It's built with the `gate` profile, into `target/gate/`: an optimised build
without whole-program optimisation, as `quick` is, so it's quicker to build
than a release, and it leaves the release build in `target/release/`, which
may be the one you're running, alone. Unlike `quick`, it's never built
incrementally. An incremental build inlines far less from one part of a crate
into another, and the same code built that way took up to 1.6 times as long
to build a frame, which would fail the gate with nothing changed. So the
baseline must be measured with the same profile, and `--gate` and
`--update-gate` refuse to run built with any other. Cargo's
`CARGO_INCREMENTAL` variable overrides any profile, so the commands here set
it to 0.

### How it keeps from crying wolf

A desktop running other work makes timings slower, never faster: another
program takes the CPU, or the other half of its core, and caches go cold. On
a desktop busy with builds, a few hundred milliseconds of measuring can come
out two to four times slower than the same a few seconds later, and a busy
spell can last minutes. So:

- Each measurement is taken in rounds, 15 to a batch, half a second apart,
  each with a daemon, and a renderer, of its own. The gate compares the least
  of the rounds, the moment the machine let it alone, for each round's median
  and its 99th percentile, the slowest key in a hundred.
- Each round also times a yardstick: hashing 16 KiB, work on the CPU that's
  none of clackatoo's. While even its least round takes over 10% longer than
  the baseline's, the machine is busier than when the baseline was made, and
  the gate judges nothing.
- If anything comes out slower than allowed, or the gate can't judge, it takes
  another batch, up to three, the least of all their rounds counting. A busy
  spell passes; a real slowdown doesn't.
- The least round's median may take up to 25% longer than the baseline's, and
  its 99th percentile up to 50%.

Those margins come from runs on a desktop with six cores (a Ryzen 5 7600X)
running other builds, at load averages of 6 to 42. While the yardstick held,
the least of a batch nearly always stayed within 10% of the baseline for
medians and 40% for 99th percentiles; one batch in about twenty came out
slower than allowed, and the next batch put it right. While the yardstick
didn't hold, the echo came out up to 2 times slower, and its 99th percentile
4 times. Of ten checks at load averages of 16 to 42, eight passed and two
couldn't judge; none said anything was slower. The task CLACKA-241 has the
numbers.

If it fails, it says either what took longer than allowed, or that the machine
stayed too busy to judge, with its load. When it's too busy, run it again once
the machine is quieter. When something took longer, the change made typing
slower.

### GitHub's runners

CI runs the `gate` group on each push to `main`, but only to record its
numbers: a runner's machine differs from one run to the next, so its numbers
can't be checked against a baseline measured on another machine. Read them in
the `keystrokes` job's log.

### A new baseline

When a change is meant to cost something, or the baseline is measured on a
new machine, make a new one, and commit it with the change and the reason.
Make it as releases are made: on the same machine, with what usually runs
beside a release running. The least of many rounds finds the machine's quiet
moments even while it's busy, but conditions count both ways: some batches
at the busiest times had keys come back faster (25 µs, against 31 µs at
quieter ones), so a check on a machine with nothing else running may come
out slower than a baseline made on a busy one.

```sh
CARGO_INCREMENTAL=0 cargo bench --profile gate -p clackatoo --bench e2e -- --update-gate
```

That takes all three batches, about half a minute, and writes them to
`crates/bench/gate.tsv`, with the commit it was measured on; the gate reads
the least round of each. The baseline is only meaningful on the machine
releases are made on.

## Profile a workload

To see where the time goes, replay one workload through the emulator in a loop
and point a profiler at it:

```sh
cargo run --release -p clackatoo-bench --example replay -- dense 20
```

That plays `dense` for 20 seconds and prints its throughput. Release builds
use link-time optimization, so build with `CARGO_PROFILE_RELEASE_DEBUG=true`
to get the function names back in a profiler.

## Pane status and focus

The manual `pane_status_and_focus_timing` test measures the desk's status
composition and focus changes for terminals, chats, editors, explorers,
boards, reviews and browsers, without a GPU or daemon:

```sh
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_INCREMENTAL=0 cargo test -p clackatoo-gui --lib pane_status_and_focus_timing --locked -- --ignored --nocapture
```

It reports the median of seven batches for each operation. Compare the same
harness and build profile on both revisions, with other builds finished. The
default test profile is unoptimized: these numbers help find changes in
dispatch cost, but do not measure release frame latency. There is no timing
assertion because a shared machine's load varies.

The ignored `pane_input_dispatch_timing` test measures text-target lookup,
pending-key lookup and one wheel event together for the same pane kinds:

```sh
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_INCREMENTAL=0 cargo test -p clackatoo-gui --lib pane_input_dispatch_timing --locked -- --ignored --nocapture
```

Each sample is a batch of 10,000 routing sequences; the report takes the
median of seven batches. The same build-profile and idle-machine rules apply.
This measures routing, not rendering or a populated pane's scrolling
throughput.

## List input

Explorer, Board and Review use `vim::list::ListPane` for key parsing, movement
and paging, search routing and leaving a pane. Their own actions and prompts
enter through hooks; Explorer also keeps its tree flattened before and after
each stroke. The trait is statically dispatched.

The three ignored `list_input_timing` tests exercise navigation and search
through each real pane, without drawing or daemon I/O:

```sh
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_INCREMENTAL=0 cargo test -p clackatoo-gui --lib list_input_timing --locked -- --ignored --nocapture --test-threads=1
```

Each reports nanoseconds per stroke, taking the median of seven batches:
10,000 navigation sequences or 100 search sequences, whose pattern compilation
is much more expensive in an unoptimized build. The search query matches
entries in the board and explorer; the review fixture exercises a missing
query. Compare identical harnesses and build profiles, with compilation
finished. These default unoptimized test timings compare dispatch cost, not
release input latency.
