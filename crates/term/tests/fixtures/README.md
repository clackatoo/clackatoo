# Recorded terminal output

Each `.rec` file is a [recording](../../src/recording.rs): a header line with
the terminal's size, then every byte a program wrote to it. The tests in
`../recordings.rs` replay them and compare the screen with a golden snapshot.
The same files are meant to serve as throughput benchmarks later.

They were recorded with `clackatoo new --record`, in a clean environment
(`env -i`), in a throwaway directory, so nothing about the recording machine
ends up in them. Check new recordings for paths, usernames and hostnames
before committing. Never record htop or anything else that shows other
processes' command lines.

| File | Size | Command |
|---|---|---|
| `ls-color.rec` | 40x6 | `ls --color=always -C` in a directory with a symlink, an executable and a subdirectory |
| `sgr-palette.rec` | 60x8 | `sgr-palette.sh` (attributes, colours, wide characters, graphemes, line drawing) |
| `vim-edit.rec` | 60x12 | `vim -u NONE -N -i NONE numbers.txt` (`seq 1 100`), then `50Gzt`, `ohello from clackatoo<Esc>`, `:set nu<CR>`, then the terminal closed |

To re-record one, start a daemon and run, for example:

```sh
clackatoo new --size 40x6 --cwd "$scratch" --record ls-color.rec -- \
    env -i TERM=xterm-256color PATH=/usr/bin:/bin LANG=C.UTF-8 ls --color=always -C
```

Then review the snapshot changes with `cargo insta review`.
