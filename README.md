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
checksums signed with [minisign](https://jedisct1.github.io/minisign/) by
the release key, whose public half is:

```
RWTx1sQF7KMKtpn+w/3hupcC6cdxdWFs4eggGaTWFEXYfB2dFKNXx4/7
```

With `minisign` and `curl` installed, this downloads the latest release,
checks the signature and then the checksum, installs it in
`~/.local/bin`, and puts it in your desktop's app launcher. It stops at
the first step that fails:

```sh
(
  set -euo pipefail
  arch=$(uname -m)   # x86_64 or aarch64
  base=https://github.com/clackatoo/clackatoo/releases/latest/download
  cd "$(mktemp -d)"
  curl -fLO "$base/clackatoo-$arch-linux.gz" -fLO "$base/SHA256SUMS" -fLO "$base/SHA256SUMS.minisig"
  minisign -Vm SHA256SUMS -P RWTx1sQF7KMKtpn+w/3hupcC6cdxdWFs4eggGaTWFEXYfB2dFKNXx4/7
  sha256sum -c --ignore-missing SHA256SUMS
  gunzip "clackatoo-$arch-linux.gz"
  install -D -m 755 "clackatoo-$arch-linux" ~/.local/bin/clackatoo
  ~/.local/bin/clackatoo desktop install ||
    echo "clackatoo is installed in ~/.local/bin; it isn't in the app launcher (see above)"
)
```

The checksums only mean something once the signature on them is good,
so the signature is checked first.

`clackatoo desktop install` writes `clackatoo.desktop`, which starts the
program it was run as, to `~/.local/share/applications`, and the icon to
`~/.local/share/icons/hicolor`, marked as clackatoo's. It never replaces
a launcher entry you wrote yourself: it says what it would write instead,
and `--force` replaces it. `clackatoo desktop remove` takes them out
again. Right-clicking clackatoo in the launcher offers a new window,
"Connect to…", which picks a machine to open a window on, and the last
three machines you connected to.

Run `clackatoo` to open a window. It starts the daemon if it isn't
running.

## Update

```sh
clackatoo update
```

installs the latest release, checking its signature the same way, and
moves the running daemon onto it, keeping its terminals. If
`clackatoo desktop install` put clackatoo in the app launcher, the new
build brings its entry and icon up to date too.
`clackatoo update --rollback` goes back.

The window looks for a new release once a day, a while after it opens,
and says in its bar when one is out. It never installs one by itself:
`:update` or `clackatoo update` does. Looking is one HTTPS request to
`https://api.github.com/repos/clackatoo/clackatoo/releases/latest`, which
says nothing about you but a User-Agent of `clackatoo/<version>`; GitHub
sees your address, as it would for any request. To turn it off, put this
in `~/.config/clackatoo/client/init.lua`:

```lua
client.opt.updatecheck = false
```

or type `:set noupdatecheck` in a window, for that window.

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
