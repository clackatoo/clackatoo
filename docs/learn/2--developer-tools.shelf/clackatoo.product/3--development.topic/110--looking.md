---
title: Looking at the window
type: how-to
description: Drive the real window from a script without a desktop, and get screenshots and how long each step took to show.
related:
  - clackatoo/development/benchmarks
---

The `look` example runs the real window, drawn into a picture instead of onto the screen, against a daemon of its own in a temporary home, so it never touches yours.

    cargo build -p clackatoo-daemon --example fake_agent --example fake_language_server
    cargo run -p clackatoo-gui --example look -- crates/gui/looks/*.look target/looks

A script has a command a line: `size 1400x860`, `scale 2`, `file PATH`, `keys :Explore<CR>`, `type`, `shell`, `click X Y`, `drag`, `scroll`, `wait-for TEXT`, `sleep MS`, `shot NAME`. Chats talk to a fake agent, and `.fk` files have a fake language server. The full language is at the top of `crates/gui/examples/look.rs`, and `crates/gui/looks/` has a script for each of the window's main parts.

Each `shot` waits until nothing changes and no timer is due, then writes `NAME.png`. `timings.txt` says, for each key or click, how long the window took to show its first change and its last. Those are a debug build's drawing plus reading the picture back, so they're for comparing one step with another, not the window's real latency; `--profile quick` comes nearer a release build.

Everything but the surface is the window's own: its layout, fonts and renderer. The interface font follows this machine's desktop settings, so shots match from run to run on one machine, not across machines.
