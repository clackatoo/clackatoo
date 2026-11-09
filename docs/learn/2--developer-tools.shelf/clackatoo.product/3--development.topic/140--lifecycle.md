---
title: Lifecycle tests
type: how-to
description: Run the tests that start the real daemon under the user's systemd, end it as a shutdown, a crash or an upgrade would, and check every workspace comes back.
related:
  - clackatoo/reference/workspaces
  - clackatoo/development/benchmarks
---

Most of clackatoo's tests start a daemon as a plain child process and stop it
with a signal. That isn't how it ends on a real machine. There, the daemon
runs in a scope of its own: a cgroup, the kernel's group of processes, that
the user's systemd made for it, divided into leaves for the daemon, its
terminals and its agents (see [`clackatoo priority`](~clackatoo/reference/cli)).
At a shutdown, systemd signals everything in the scope at once, the daemon's
leaf last, so the programs in its terminals end a moment before the daemon
hears it's to stop. A daemon that took their ends for ordinary exits would
close their panes, and their workspaces, before keeping them. That once lost
workspaces at a real shutdown, and no test that signals the daemon alone
could have caught it.

The lifecycle tests run the real `clackatoo daemon` in a scope made the way
the daemon makes its own, fill it with workspaces of terminals, chats and
editors, end it, start it again, and check that every workspace, tab, pane
and folder [came back](~clackatoo/reference/workspaces#after-a-restart).
They end it four ways:

| Test | How the daemon ends |
|---|---|
| `…after_systemd_stops_the_daemons_scope` | `systemctl --user stop` on its scope, as a shutdown or a logout without lingering does. |
| `…after_a_shutdown_logind_said_was_coming` | A fake logind, systemd's login manager, says a shutdown is coming (`PrepareForShutdown`); the daemon keeps its workspaces and lets go of its lock, then the scope is stopped. |
| `…outlives_an_upgrade_in_its_scope_and_a_stop_after` | `clackatoo upgrade`: the same programs run on, the next build stays in the daemon's leaf and starts new programs in theirs; then the scope is stopped. |
| `…after_the_daemon_is_killed` | `SIGKILL` to the daemon alone, as a crash. |

Each test also checks the scope is divided as the daemon divides its own:
the daemon in its leaf, each terminal's program in `terminals`, and the
agent in `agents`.

## Run them

They need a systemd user manager, so they're left out of `cargo test`
unless asked for, with the `lifecycle` feature. Build the daemon crate's
examples first, for the fake agent the chats talk to:

```sh
cargo build -p clackatoo-daemon --examples
cargo test -p clackatoo --features lifecycle --test lifecycle
```

Without a user manager to make scopes in (`systemd-run --user --scope true`
fails), every test fails at once, saying so.

The tests never touch a daemon you're using. Each daemon has a runtime,
state, config and data directory of its own, in a temporary directory, and
runs in a scope named as a daemon names its own, `app-clackatoo-N.scope`,
but with more digits than any process ID has, so it can never be a real
daemon's. Only scopes a test made are ever stopped.

## In CI

GitHub's runners have no user manager running for the job's user, so the
`lifecycle` job in `.github/workflows/ci.yml` asks logind to keep one
(`loginctl enable-linger`), points the tests at it, and then runs them.
