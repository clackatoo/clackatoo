---
title: Compatibility between Builds
type: reference
description: How a window and a daemon of different builds talk, and what the window offers for a daemon from another build.
related:
  - clackatoo/reference/remote
---

A window and a daemon from different builds of clackatoo still talk, at the
older of their two protocols.

## Protocol compatibility

A window and a daemon from different releases still work together, as long as
they're no more than a release or so apart: your laptop's window carries on
when your desktop's daemon is updated first, and the other way round.

When a window connects, each side says which version of the protocol it
speaks, and the connection runs at the older of the two. The newer side sends
only what the older one can read: what's newer is left out, or turned into
something the older one has.

- **A pane of a kind the window doesn't have** shows "This pane needs a newer
  clackatoo", naming the kind. The rest of the window works as usual.
- **Something the window offers that the daemon doesn't have** is refused,
  saying the daemon needs a newer clackatoo, which release it is, and how to
  update it: `clackatoo update` on the other machine, or `clackatoo upgrade`
  for this machine's daemon.
- **A message the window can't read** is passed over. If it changed a
  terminal's screen or a file being edited, the window asks the daemon for
  them afresh, and an editor's unsaved changes are kept and sent again, as
  after a lost connection. If that keeps happening, it stops asking after five
  times a minute and says to update.

Each release sets how far apart they can be, in two parts: the oldest window
its daemon talks to, and the oldest daemon its window talks to. Further apart
than that, the window says the daemon is from another build, as
[below](#a-daemon-from-another-build).

The first release that does this, 2026.10.11 (protocol 73), takes windows from
the release before it, 2026.10.10, but needs a daemon of at least its own: a
daemon older than that can't say it agrees to an older version, so a window
treats it as from another build.

## A daemon from another build

If the daemon on the other machine speaks another version of the protocol, the
window says so, as it does
[on this machine](~clackatoo/reference/frame#a-daemon-from-another-build).

- **An older daemon there:** <kbd>Enter</kbd> replaces it using the clackatoo
  installed there: it upgrades it if it can, keeping its terminals, or else
  stops it, closing them. If that clackatoo is itself from another build, the
  window says to update it.
- **A newer daemon there:** <kbd>Enter</kbd> updates this machine's clackatoo
  to the latest release, as `clackatoo update` does, and the window opens
  again as the new build, connected to the same machine. This machine's own
  daemon moves onto the new build too, keeping its terminals, and its other
  windows ask to open again. If the daemon there is from a build newer than
  any release, there's nothing to update to, and the window says so: use a
  window from the daemon's build, or move that daemon onto a release.
  <kbd>Esc</kbd> quits, or goes back to the picker if you picked the machine
  there.

The `clackatoo` command doesn't replace anything. On the other machine,
`clackatoo upgrade` upgrades its daemon, and `clackatoo stop` stops it.

If the daemon speaks the same protocol but is another release of clackatoo,
the window connects as usual and says in the bar which side is older. When
it's the other machine, `:update HOST` opens a tab there running
`clackatoo update`, which installs the latest release and moves that daemon
onto it; the window reconnects as it does after any upgrade. When it's this
machine, `:update` updates it, and the window needs opening again.
