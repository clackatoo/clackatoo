---
title: How a Connection Works
type: reference
description: How a connection to another machine is made and moves onto UDP, what the other machine needs, its desktop, pasting an image from this machine's clipboard, the options, what happens when it drops or slows, and the logs.
related:
  - clackatoo/reference/remote
---

A connection starts over ssh and moves onto UDP, so it survives a change of
network, and the window says what it's doing when the other machine stops
answering.

## How it connects

`--remote host` runs `ssh host clackatoo bridge`. On the other machine, the
bridge starts the daemon if it isn't running, then passes everything between
ssh and the daemon.

Through it, the window or command first checks that the daemon speaks its
version of the protocol, as it does on this one. Then the two talk over QUIC,
a network protocol that runs several independent streams over one connection,
carried inside the ssh connection.

ssh does the signing in, and it's how each end knows the other is genuine. The
daemon only takes QUIC connections that come through ssh, and the window only
trusts the certificate the daemon sent it through ssh. Anyone who can use the
daemon could already sign in to that machine as you.

Each daemon also has an identity key, kept beside its machine's ID in
`machine` in clackatoo's state directory, and signs its QUIC certificate with
it, so a machine reached through another (see
[machines passed on](~clackatoo/reference/connecting#machines-passed-on)) can
be checked. The keys of the machines it has met are kept in `known_machines`
beside it: like ssh's `known_hosts`, it's readable only by you, and starting
afresh or keeping no workspaces never loses it. A machine met straight with
ssh is known by the key it showed; one only passed on, by the key the machine
passing it on vouches for, the first time. Through another machine, a
connection must show a certificate signed by the key kept, and one you asked
for must have met that machine straight first. A key that differs from the one
kept is refused, saying so in the bar: through another machine, only that way
is ruled out, and ssh straight there is still tried; straight over ssh, the
connection stops until
[`clackatoo forget HOST`](~clackatoo/reference/cli#forget), which only this
machine's own windows and commands can run. Only a machine's own key puts a
connection in that machine's place. A daemon reached straight must sign its
certificate as the machine it says it is, and stay that machine: one that
later says it's another, or doesn't sign though its clackatoo does, is
stopped, and what it said is never taken. A connection reaches the machine it
reached before: if HOST comes back as another machine, as after its state
directory was wiped, it's stopped until you `:connect HOST` again, or
`clackatoo forget HOST`, either of which takes whatever is there now. One that
says it's a machine passed on to yours, which yours doesn't know by its key
yet, must sign with the key the machines passing it on vouch for. A daemon too
old to sign, before protocol 95 (2026.10.60), is taken as the machine it says
only while no machine of that ID is known here by its key, passed on, expected
by a connection you asked for, or connected to your daemon itself by another
name than HOST (two such daemons connected both ways take each other, by the
same name, whichever connects first; by another name, update clackatoo on
HOST); and a connection you asked for that proves it's that machine, by a key
it met straight or as you `:connect` it, takes its place later. So a machine
known by its key that goes back to a clackatoo that old is refused until it's
updated, or until `clackatoo forget HOST`, with the name you connected to it
by. A daemon passing machines on can have yours learn only so many new keys an
hour, and the keys of machines connected to, passed on or kept are never
forgotten to make room for others.

Each terminal's screen updates come on a QUIC stream of their own, apart from
everything else, so a lost packet holds up only the terminal it belonged to.

## Moving onto UDP

Once connected, the window or command tries reaching the daemon straight over
UDP, at the address ssh reached that machine at. If that works, the connection
moves there, out of ssh:

- **It survives your network changing,** such as moving from home Wi-Fi to a
  phone's hotspot, or a short sleep. ssh's connection dies when that happens;
  the QUIC connection carries on from the new network, and the window doesn't
  notice.
- **Keys come back a little faster,** with ssh out of the way.

It finds out with a few probes, small datagrams the daemon sends straight
back, while the connection carries on inside ssh, so nothing waits on the
attempt. If none comes back within two seconds, as when a firewall blocks UDP,
the connection stays inside ssh, and tries UDP again 15 seconds later. If UDP
stops getting through later, the connection goes back into ssh after about 20
seconds, if ssh's connection is still up, longer than ssh takes to give up on
a network that's gone, and tries UDP again 15 seconds after.

[`clackatoo ping --remote host`](~clackatoo/reference/cli#ping) says which way
each round went: `(over UDP)` or `(through ssh)`.

The daemon takes UDP on the first free port from 60000 to 61000, the same
range as mosh, when the first connection through ssh arrives. It never takes a
new connection over UDP, only packets for connections already set up through
ssh. Probes carry a key the daemon only gives out through ssh, and it answers
nothing else, so the port gives nothing away to a scan.

### Firewalls

A firewall on the other machine has to let UDP in on those ports. If it
already does for mosh, there's nothing to do. Otherwise, with `ufw`:

```sh
sudo ufw allow 60000:61000/udp
```

or with `firewalld`:

```sh
sudo firewall-cmd --permanent --add-port=60000-61000/udp && sudo firewall-cmd --reload
```

To use other ports, or none, set `CLACKATOO_UDP_PORTS` for the daemon on the
other machine: a port, a range such as `60100-60200`, or `off` to keep every
connection inside ssh. The daemon reads it when it starts, and one started by
`--remote` gets it from the remote command:

```sh
clackatoo --remote server --remote-command 'env CLACKATOO_UDP_PORTS=60100-60200 clackatoo'
```

Through a jump host (`ProxyJump`), ssh reached the other machine at an address
on its own network, which you usually can't reach directly, so the connection
stays inside ssh.

## What the other machine needs

- **clackatoo of the same release, or one a release or so apart** (see
  [Protocol compatibility](~clackatoo/reference/compatibility#protocol-compatibility)).
  A daemon further apart speaks another protocol, which the window
  [offers to replace](~clackatoo/reference/compatibility#a-daemon-from-another-build).
- **clackatoo on the PATH ssh's commands get.** That is often shorter than an
  interactive shell's, and might leave out `~/.cargo/bin` or `~/.local/bin`.
  If it isn't there, the error is the remote shell's "command not found".
  `--remote-command` names it instead:

  ```sh
  clackatoo --remote desktop --remote-command ~/.cargo/bin/clackatoo
  ```

  The command is run by the remote shell, so it can be any command line that
  ends by running clackatoo, such as `env XDG_RUNTIME_DIR=/tmp/ck clackatoo`.
  Setting `CLACKATOO_REMOTE_COMMAND` saves typing it each time.

- **A way to sign in.** A key loaded into `ssh-agent` signs in without asking
  anything. Otherwise, the window asks for ssh's password, or the key's
  passphrase, itself, in place of the terminal, and asks again on every
  reconnect. It also asks whether to trust a host ssh hasn't seen before. What
  you type is hidden, but for that question: what a server asks as you sign in
  is in its own words, so it's hidden whatever it says. The `clackatoo`
  command leaves ssh to ask on the terminal it runs in.

- **On a server nobody is signed in to:** systemd removes your runtime
  directory (`/run/user/<uid>`) when your last session ends, taking the
  daemon's socket with it. The daemon keeps running, but the next connection
  can't find it and starts another. Run this once on the server so your
  runtime directory stays:

  ```sh
  loginctl enable-linger
  ```

## The other machine's desktop

A daemon `--remote` starts on your desktop starts before you've signed in to
the desktop itself, over ssh, with no `WAYLAND_DISPLAY` of its own. Programs
it starts still open their windows on the desktop once you sign in there: the
daemon follows the display your desktop tells systemd as you sign in, and
gives it to the terminals, agents and language servers it starts from then on,
and shells already open take it before the next command they run (bash at its
next prompt). Once you sign out and the desktop's socket has gone, what starts
after that doesn't get its display. `ssh desk clackatoo display` says what
programs on desk get now. See
[the desktop's display](~clackatoo/reference/terminal-emulation#the-display).

## An image from this machine's clipboard

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> in a terminal on
another machine pastes the image on this machine's clipboard there: it goes to
that machine's daemon, through any machine between, which keeps it in its own
`~/.cache/clackatoo/pastes/` and pastes its path, there, for a program such as
Claude Code to read. So a screenshot taken on your laptop goes into Claude
Code running on your desktop. With `pasteimage=clipboard`, it's put on the
desktop's own clipboard instead, which needs you signed in there (see
[the other machine's desktop](#the-other-machine-s-desktop)). Each machine on
the way needs a clackatoo that knows how; the bottom bar says which to update
if one doesn't. See
[pasting an image](~clackatoo/reference/typing#pasting-an-image).

## Options

| Option | Environment variable | |
|---|---|---|
| `--remote <HOST>` | | Use the daemon on `HOST`: `host`, `user@host`, `host:port`, or a name from your ssh config. |
| `--connect-picker` | | Pick the machine in the window first (see [Picking a machine](~clackatoo/reference/connecting#picking-a-machine)). |
| `--remote-command <COMMAND>` | `CLACKATOO_REMOTE_COMMAND` | The command that runs clackatoo on the other machine. Default: `clackatoo`. |
| | `CLACKATOO_SSH` | The program run as ssh. Default: `ssh`. It is given ssh's options, then `--`, the host and the command. |

clackatoo passes ssh a few options of its own:

- `-T`: no terminal on the other end; the bridge doesn't want one.
- `-a`, `-x`, and `ClearAllForwardings=yes`: nothing forwarded, whatever your
  ssh config says. A daemon the bridge starts on the other machine would
  otherwise hand your ssh agent, or your X display, to every terminal, agent
  and language server it runs there, for as long as the connection lasts. What
  clackatoo forwards goes over its own connection.
- `ConnectTimeout=10`: an unreachable host fails after 10 seconds.
- `ServerAliveInterval=5` and `ServerAliveCountMax=3`: a connection that has
  died, such as when the laptop sleeps or changes network, is noticed within
  15 seconds. A connection that has moved onto UDP carries on without it.

## When the other machine stops answering

If what the window sends goes unanswered for about five seconds, a red notice
at the right of the top bar says so: `No contact with desktop for 12 s`,
counting from when it was sent. The window stays as it was. Editors carry on:
what you type in them is kept and goes when the other machine answers again,
landing exactly as it would have. Keys for terminals and everything else
aren't sent late: once what was sent has gone unanswered for three seconds,
the bar says `not connected: keys aren't sent`, and the pane flashes. The
notice goes as soon as the other machine answers again.

An idle window notices within about 15 seconds, at its next keep-alive.

## When ssh's connection closes

The window opens a new ssh connection in the background, and the same
connection carries on through it: nothing is redrawn and nothing is lost. It
tries again after 1, 2, 4… up to 30 seconds. Over UDP, typing carries on
meanwhile.

## When the connection drops

A connection ends only when the daemon stops, is upgraded or restarts, which
tells the window at once, or after two minutes without hearing from the other
end. An idle window sends a few bytes every 10 seconds, which keeps it, and
your router's memory of it, alive.

The window then connects again by itself. The panes stay as they were, the top
bar says
`Reconnecting to desktop (12 s): the daemon is upgrading to a new build`, and
it tries again after 1, 2, 4… up to 30 seconds; a connection that drops again
within 10 seconds counts as a failed try. <kbd>Enter</kbd> and <kbd>Esc</kbd>
are only offered when the first connection fails.

What you typed in an editor and the daemon hadn't yet taken is kept, and goes
again when the window reconnects, catching up with what others changed
meanwhile as if it had all happened live. A file renamed meanwhile keeps its
new name. If the daemon restarted rather than upgraded, and the file is as it
was, your changes go again as they were, and the bar says
`reconnected; your unsaved changes to X were kept`. If the file changed
meanwhile, the editor keeps your text and `:w` refuses: `:w!` writes your
window's text over the file, and `:e!` takes the daemon's.

Nothing on the other machine stops when the connection drops: the terminals
carry on, and output that arrives meanwhile is there when you reconnect.

## Bad networks

Every packet is 1200 bytes at most, which fits tunnels such as Tailscale's
(1280). On a network losing packets, small ones (keys, their echoes,
acknowledgements) are sent twice. After an outage, the window sends from a new
UDP port, so the other machine picks up at once rather than waiting out QUIC's
growing timers. A window that falls more than 8 MB behind is dropped and
reconnects.

## Typing over a slow connection

Every key goes to the other machine and back before the program's echo shows.
So, as mosh does, the window guesses what most keys will show and draws it at
once, underlined, until the daemon's screen shows the echo. It guesses
printable characters, <kbd>Backspace</kbd>, and <kbd>Left</kbd> and
<kbd>Right</kbd> within what you've typed on the line.

It only draws a key you typed at a shell prompt with nothing still running, as
shell integration marks it: never for full-screen programs, programs using the
mouse, a hidden cursor, or in normal mode, and never for keys typed while a
command runs, so a password typed ahead for `sudo`, or at any prompt a program
shows, is never drawn. A shell that doesn't mark its commands, such as bash
before 4.4, gets no guesses. A wrong guess vanishes, and guessing waits for
the program to echo again. The `predictecho` option turns this on or off.

[`clackatoo ping --remote host`](~clackatoo/reference/cli#ping) measures it:
how long the daemon takes to answer, and how long a key takes to come back
from a program. clackatoo's own share is small. To the same machine, where the
network costs next to nothing, a key comes back in about a fifth of a
millisecond over UDP, and a third through ssh, so nearly all of anything above
that is the network.

## Two windows on one terminal

A terminal has one size, so when windows of different sizes show the same
terminal, such as your laptop's and your desktop's, they take turns: the
terminal takes the size of whichever window last opened on it, was resized or
was focused.

## Logs

Lines carry UTC timestamps, and say why each connection closed and which way
it was going. The daemon logs to `daemon.log` in its runtime directory;
windows log to `~/.local/state/clackatoo/window.log`, moved aside past 8 MB.
`clackatoo ping --remote host --soak` keeps a connection open, logging each
echo and every drop with its reason.
