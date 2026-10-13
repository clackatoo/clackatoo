# clackatoo

A fast, lightweight terminal workspace for Linux.

A daemon owns every terminal, so closing a window closes nothing. Windows,
the `clackatoo` command and coding agents are all clients of the same
daemon, on this machine or on another one over ssh. It has workspaces of
tabs and split panes, Vim's modes, its own editor with tree-sitter and
language servers, a file explorer, and chats with agents such as Claude
Code, Codex and Gemini CLI.

> **clackatoo is alpha.** It's young and changes quickly. Expect rough
> edges, and options and behaviour that change between releases.

## Install

Each release has a build for `x86_64` and `aarch64` Linux, gzipped, and
checksums signed with [minisign](https://jedisct1.github.io/minisign/).
The key's public half is [`crates/update/release-key.pub`](crates/update/release-key.pub).

```sh
arch=x86_64   # or aarch64
base=https://github.com/clackatoo/clackatoo/releases/latest/download
curl -LO "$base/clackatoo-$arch-linux.gz" -LO "$base/SHA256SUMS" -LO "$base/SHA256SUMS.minisig"
curl -LO https://raw.githubusercontent.com/clackatoo/clackatoo/main/crates/update/release-key.pub

minisign -Vm SHA256SUMS -p release-key.pub
sha256sum -c --ignore-missing SHA256SUMS
gunzip "clackatoo-$arch-linux.gz"
install -m 755 "clackatoo-$arch-linux" ~/.local/bin/clackatoo
```

Check the signature first: the checksums only mean something once it's
good.

Run `clackatoo` to open a window. It starts the daemon if it isn't
running.

## Update

```sh
clackatoo update
```

installs the latest release, checking its signature the same way, and
moves the running daemon onto it, keeping its terminals. The window says
when a new release is out. `clackatoo update --rollback` goes back.

## Build

You need Rust 1.96 or newer, and a C and C++ compiler.

```sh
cargo build --release -p clackatoo
```

The program is `target/release/clackatoo`.

## Documentation

https://learn.peios.org/clackatoo/

## Licence

clackatoo is licensed under either of the [Apache License 2.0](LICENSE-APACHE)
or the [MIT licence](LICENSE-MIT), at your option. Some of it is derived
from other projects: see [NOTICE](NOTICE).
