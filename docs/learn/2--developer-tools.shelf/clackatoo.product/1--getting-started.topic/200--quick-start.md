---
title: Quick Start
type: how-to
description: Install or build clackatoo, open a window on your shell, and drive terminals from the command line by starting one, typing into it, reading its screen and closing it.
related:
  - clackatoo/getting-started/what-is-clackatoo
  - clackatoo/reference/window
  - clackatoo/reference/remote
  - clackatoo/reference/cli
---

## Install it

Each release has a build for `x86_64` and `aarch64` Linux, signed with
clackatoo's release key. The
[README](https://github.com/clackatoo/clackatoo#install) shows how to check
and install the first one; after that,
[`clackatoo update`](~clackatoo/reference/cli#update) installs each new one.

The README's install also runs
[`clackatoo desktop install`](~clackatoo/reference/cli#desktop), which puts
clackatoo in your app launcher, under "Open With" in file managers for text
files, and "Open in clackatoo", for folders and text files, in the menus of
the file managers you have. Run it again after installing another file
manager.

## Or build it

clackatoo is a Cargo workspace. Build the `clackatoo` command in release mode:

```sh
cargo build --release -p clackatoo
```

The binary is `target/release/clackatoo`.

## Open a window

```sh
clackatoo --font "JetBrains Mono"
```

That opens a window on your shell, starting the daemon in the background if it
isn't running. Leave out `--font` for your system's monospace font, or choose
one for good with `clackatoo config font "JetBrains Mono"`. Close the window
and the shell keeps running; open another and you're back.

<kbd>Ctrl</kbd>+<kbd>;</kbd> switches to normal mode, where you scroll, select
and search history with Vim's keys, and `i` switches back. From normal mode,
`:|` <kbd>Enter</kbd> splits the pane with a new terminal on its right, `:-`
below it, and `:Terminal` opens one in a new tab.
<kbd>Ctrl</kbd>+<kbd>H</kbd>, <kbd>Ctrl</kbd>+<kbd>J</kbd>,
<kbd>Ctrl</kbd>+<kbd>K</kbd> and <kbd>Ctrl</kbd>+<kbd>L</kbd> move between
panes, and `:1`, `:2` and so on between tabs.
[The window](~clackatoo/reference/window) has all its keys.

## Start the daemon yourself

```sh
clackatoo daemon
```

To watch the daemon's log, run it in the foreground instead. It logs to
stderr; set `RUST_LOG=debug` for more detail. Only one daemon runs per user: a
second one refuses to start. Stopping the daemon (Ctrl-C, or `clackatoo stop`)
hangs up every terminal it owns, after shutting its language servers and
agents down properly.

## Drive a terminal

From another terminal, start one. `new` prints the new terminal's ID:

```sh
clackatoo new
```

The output is its ID:

```text
1
```

It runs your `$SHELL` in the current directory, at 80×24. Type a command into
it and press <kbd>Enter</kbd> with `run`:

```sh
clackatoo run 1 ls --color=always
```

See what it shows with `capture`. Add `--ansi` to keep the colors:

```sh
clackatoo capture 1 --ansi
```

`send` types text without pressing <kbd>Enter</kbd>, which is how you drive
full-screen programs:

```sh
clackatoo run 1 htop
clackatoo send 1 q
```

List terminals, and close one when you're done:

```sh
clackatoo list
clackatoo close 1
```

Closing a terminal hangs up its program, as closing a terminal window would. A
terminal whose program exits stays listed, with its exit status and last
screen, until you close it.

## From another machine

With clackatoo on both machines, and ssh able to sign in to the other one (the
window asks for a password or a key's passphrase itself, if ssh needs one):

```sh
clackatoo --remote desktop
```

That opens a window on `desktop`'s workspaces, starting the daemon there if it
isn't running. Every command takes `--remote` too, as in
`clackatoo list --remote desktop`. If ssh's commands on the other machine
can't find clackatoo, [another machine](~clackatoo/reference/remote) explains
how to point at it.
