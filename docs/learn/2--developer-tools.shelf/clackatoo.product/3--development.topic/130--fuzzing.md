---
title: Fuzzing
type: how-to
description: Run the fuzz targets that feed clackatoo's parsers and readers what other programs, peers and earlier builds could send, and turn what they find into fixes and tests.
related:
  - clackatoo/development/benchmarks
---

A fuzzer runs a piece of code over and over with input it makes up, keeping the inputs that reach code it hadn't reached before, until something panics, takes too long or uses too much memory. clackatoo's fuzz targets are in `fuzz/` at the top of the repository. They run with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz), which drives LLVM's libFuzzer and needs a nightly Rust.

## What's fuzzed

| Target | What it reads | What it checks, beyond never panicking |
|---|---|---|
| `parser` | what a program writes to its terminal | the parser reports what `vte`, its oracle, reports, however the bytes are cut |
| `terminal` | the same, onto the whole emulator, with resizes, searches and scrollback | a snapshot, as an upgrade hands it over, makes the same terminal again |
| `frames` | protocol frames, at every version the compatibility window holds | a message read is encoded at that version to what reads back the same; what relays read without decoding agrees with decoding |
| `transform` | edits made at once to one text | each made after the other, transformed, gives one text; compose, invert and coarsen agree with making the edits |
| `history` | several authors editing, undoing and sending late edits | every edit the history gives fits the text; bringing an edit up to date keeps to its budget |
| `handover` | a handover file of any version, from any bytes | it's read or refused, and what's read is made into workspaces, screens, buffers and chats |
| `workspaces` | the saved workspaces file | what's read writes back the same |
| `agents_toml` | `agents.toml` | its limits on subagents stay in their ranges |
| `json_rpc` | what agents and language servers write, framed either way | read in pieces, or handed over midway, the messages are the same as read whole; the walkers keep to their caps |

Code a target can't reach otherwise is reached through a `fuzzing` feature of `clackatoo-term` and `clackatoo-daemon`. The feature is only for the fuzzer. `client/startup.toml` is read by the `clackatoo` program itself, which nothing can link to, so property tests in its `config.rs` check it instead.

## Running a target

Install the nightly toolchain and cargo-fuzz once:

    rustup toolchain install nightly --component rust-src
    cargo install cargo-fuzz

Then, in `fuzz/`, fill the corpus with seeds from the repository's fixtures, and run a target:

    ./seed
    cargo +nightly fuzz run frames corpus/frames -- -max_total_time=1800 -timeout=5 -rss_limit_mb=1024 -malloc_limit_mb=512 -max_len=65536

The first build takes a while: it builds the daemon and everything it uses, with AddressSanitizer. `--codegen-units 16` after `run` builds faster, and the targets run a little slower.

- `-timeout=5` treats an input that takes over 5 seconds as a failure. Most of these readers run on the daemon's loop, where that is a bug.
- `-malloc_limit_mb` treats a single allocation that big as a failure. That's how a reader that allocates far more than it was sent is caught.
- `-fork=2 -ignore_crashes=1` runs two processes and keeps going after a failure, saving each one. Leave a core or two free for the rest of the machine.

The fuzz targets are a workspace of their own, so builds, tests and CI of the rest never build them. Their build goes in `fuzz/target`. `corpus/` and `artifacts/` stay out of git.

## When it finds something

The input that failed is saved under `artifacts/TARGET/`. To run it again, with the panic's message:

    cargo +nightly fuzz run frames artifacts/frames/crash-…

Make it smaller with `cargo +nightly fuzz tmin frames artifacts/frames/crash-…`, then find the cause. Fix it in the product, and add an ordinary unit test with the smallest input, beside the code it tests, so the fix holds without the fuzzer. A failure in a target's own checks can be the check's fault: read what it asserts before changing the product.
