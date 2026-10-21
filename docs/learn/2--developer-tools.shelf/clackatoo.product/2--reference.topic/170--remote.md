---
title: Another machine
type: reference
description: With --remote, the window and the clackatoo command use the daemon on another machine, reached with ssh. How it connects, how it moves onto UDP to survive network changes, what the other machine needs, and what happens when the connection drops.
related:
  - clackatoo/reference/phone
  - clackatoo/reference/window
  - clackatoo/reference/cli
---

The window and the `clackatoo` command can use the daemon on another machine instead of this one: your desktop from your laptop, or a server. The terminals run there, and keep running when you disconnect.

```sh
clackatoo --remote desktop                  # a window on desktop's workspaces
clackatoo list --remote desktop             # its terminals
clackatoo new --remote jack@server -- htop  # start one there
```

The host is anything `ssh` accepts: a hostname, `user@host`, `host:port`, or a name from your `~/.ssh/config`, where keys and jump hosts belong.

## Picking a machine

`clackatoo --connect-picker` opens the window as a picker of the machine to connect to, instead of connecting anywhere first. It lists:

- the machines you've connected to recently, newest first;
- each `Host` in `~/.ssh/config`, and the files it includes, that names one machine: patterns with `*`, `?` or `!`, and everything under `Match`, are left out;
- Tailscale's peers that are online, if Tailscale is installed, by their MagicDNS name (or address, without MagicDNS). Phones are left out.

Type a few letters to narrow them, or a machine that isn't listed, such as `jack@server:2222`, and Enter connects, as `--remote` would. If it can't, the picker opens again saying why, so you can pick another. Esc closes the window.

Your app launcher offers this as "Connect to…" when you right-click clackatoo, along with the last three machines you connected to (see [desktop](~clackatoo/reference/cli#desktop)).

In a window, `:connect`, or Ctrl+B r, opens the same picker over your panes. Picking a machine opens a new window on it and leaves this one as it is.

Each window remembers the machines it connects to, in `clackatoo/remotes` under `$XDG_STATE_HOME` (`~/.local/state`), a line each, up to 20.

## Another machine's workspaces in your bar

`:connect HOST` has your daemon connect to the daemon on HOST, as `--remote` does, and show HOST's workspaces in your bar beside your own. Windows, your phone and scripts keep talking to your daemon, which passes what they ask on to HOST and HOST's answers back. Terminals and chats there work as yours do: keys, screens, history, copying and searching. A new tab, a split, a rename or a close in one of HOST's workspaces happens on HOST, as does a new pane made from one of HOST's panes anywhere, which keeps them, so they carry on while your laptop sleeps. Everything else in them works as your own does, on HOST: editors, with undo, `:w`, `:w NAME` and `:recover`, their colours, git's signs, what agents changed, and what HOST's language servers say; explorers and folders' listings, whose `:w` moves and deletes HOST's files; reviews, boards and a chat's page. The finder searches the machine the focused pane is on, so in one of HOST's workspaces it finds and greps HOST's files. HOST keeps each file and makes every edit, so its own windows and agents edit a file alongside you, and what you type while the connection is down is made once it's back. A file cut in one machine's explorer can't be pasted in another's. HOST needs this release of clackatoo or later for all of this; an older one's editors and explorers say so in their panes, and its terminals and chats still work.

Your daemon keeps the connection. It makes it again whenever it drops, and again after your daemon restarts. `:disconnect HOST` ends it, as does `:disconnect` alone in one of its workspaces. While HOST isn't connected, its workspaces stay as they last were, each pane says so over its last screen, and keys for its terminals are turned down rather than sent late; so are they, and anything else asked of it, when HOST hasn't answered for a few seconds ("desk isn't answering"). If ssh needs a password and no window is open to ask, the connection waits until one is. In your config, `client.connect("desk")` connects whenever a window opens.

`:PortForward 8080` in one of HOST's workspaces forwards this machine's port 8080 to HOST's, through your daemon's connection to it, which keeps listening while the connection drops and is made again. `:PortForward` there lists the forwards and `:PortForward! 8080` stops one, as for a window on another machine. A window that's itself on another machine than its daemon can't forward to HOST yet.

## Machines passed on

Your daemon passes on the machines it's connected to. If your laptop is connected to desk, and desk to a server, the laptop's bar has the server's workspaces too, as `notes@server`: named after the machine they live on, never the way there. Everything in them works as for a machine you connected to yourself, through a connection of your laptop's own to the server, which desk carries on.

Desk could read or change what it carries, so your laptop checks the server by its identity key: the one it met over ssh, or, for a server it has never reached itself, the one desk vouches for the first time, kept from then on (see [who's genuine](#how-it-connects)). A connection you asked for yourself goes through another machine only once it has met the server over ssh.

A machine reached several ways is shown once, by the shortest. `:connect server` on the laptop as well has it go straight there, through desk only while ssh can't get there, and back once it can, without its panes noticing; trying ssh again never asks for a password. `:disconnect server` then leaves it reached through desk; one only desk passes on goes when desk stops passing it on, or with `:disconnect desk`. Machines connected to each other, or in a ring, each show the others once, and never themselves. The mouse over such a workspace says the way, as `on server, through desk`.

`:connect server --no-share` keeps a machine, such as a server others share, from the daemons connected to yours, refuses them a way through, and cuts any open; `:connect server --share` passes it on again, and the bar says when one goes. Your daemon keeps this with its connections. Each machine needs this release of clackatoo or later to pass machines on or be passed on.

## A pane on another machine

`@host` after a kind of pane opens it on that machine, wherever the workspace is: `:Terminal@desk` opens a terminal on desk in a new tab of the workspace you're in, `:|Terminal@desk` splits one in beside the focused pane, and `:Editor@desk notes.md`, `:Explore@desk` and the rest work the same. `@local`, or this machine's own name, is your daemon's machine. Without `@`, a new pane goes on the machine of the pane it's made from, so splitting a pane on desk, or opening a file or the finder from it, stays on desk.

In one of your own workspaces, desk lends your daemon the pane: desk keeps it, as one of its own, in a workspace no window sees, and your workspace shows it. Desk's own windows, its phone and its scripts can't see it or act on it, unless desk connects to your laptop too; a third machine connected to both shows it, and can type into it, but not close it, and reaches it only while your laptop is connected to desk too. What you type runs there, and its files, explorer and finder are desk's. Its tab ends `@desk`, unless `showhost` is `never`. Your daemon keeps it with your workspaces, and desk keeps what it shows, so it comes back after either restarts: a terminal as a new shell in its folder on desk, as desk's own do. While desk can't be reached, the pane says so over its last screen. Closing it closes it on desk, at once or once desk is back; its program exiting on desk closes it here. Desk needs this release of clackatoo or later; an older one says it's too old to lend panes.

Desk never throws away what it lent on so little: a pane your daemon no longer shows, or a `[No Name]` with changes you closed, comes back as one of desk's own workspaces, `from-laptop`, unless it's a shell running nothing. On desk, [`clackatoo lent`](~clackatoo/reference/cli#lent) lists what it lent, and `clackatoo lent reclaim N` takes one back; after 30 days without your laptop, desk takes them all back. If desk restarts without a pane it lent, the bar says so. An older clackatoo reading the workspaces kept shows a workspace lent to another machine as one of its own, with no name, and a pane borrowed from another machine as a new shell here.

It works the other way round too, even though desk can't reach your laptop: `:|Terminal@local` in one of desk's workspaces splits in a terminal on your laptop, and any pane made beside it is your laptop's as well. Desk's layout keeps it; the terminal stays on your laptop, and what you type goes straight to it, never to desk and back. Its tab ends `@local`. Desk's own windows, and any machine that can't reach your laptop, show "a terminal on laptop, not reachable from here" in its place; a machine connected to both shows it, and so does desk once it's connected to your laptop.

While desk is away, such panes are parked in a workspace of your laptop's named after it, `from-desk`, a tab each, so you can go on using them. Once desk is back they're back where they were and that workspace goes; nothing is moved or lost. Closing `from-desk` closes them all; a new pane beside one waits until it's back in desk's workspace. Closing one from your laptop closes it on both. Closed on desk, a shell running nothing closes, and anything else comes back as one of your laptop's own workspaces, `from-desk`; so does one desk restarted without. Desk needs this release of clackatoo or later: an older one says it's too old to show a pane from your machine.

`:workspace new@desk NAME` makes a workspace on desk, as does `:workspace NAME@desk` if desk has none of that name, and the `+` after your workspaces asks which machine. It lives on desk, so it carries on while your laptop sleeps, and desk's own windows have it too.

## How it connects

`--remote host` runs `ssh host clackatoo bridge`. On the other machine, the bridge starts the daemon if it isn't running, then passes everything between ssh and the daemon.

Through it, the window or command first checks that the daemon speaks its version of the protocol, as it does on this one. Then the two talk over QUIC, a network protocol that runs several independent streams over one connection, carried inside the ssh connection.

ssh does the logging in, and it's how each end knows the other is genuine. The daemon only takes QUIC connections that come through ssh, and the window only trusts the certificate the daemon sent it through ssh. Anyone who can use the daemon could already log in to that machine as you.

Each daemon also has an identity key, kept beside its machine's ID in `machine` in clackatoo's state directory, and signs its QUIC certificate with it, so a machine reached through another (see [machines passed on](#machines-passed-on)) can be checked. The keys of the machines it has met are kept in `known_machines` beside it: like ssh's `known_hosts`, it's readable only by you, and starting afresh or keeping no workspaces never loses it. A machine met straight with ssh is known by the key it showed; one only passed on, by the key the machine passing it on vouches for, the first time. Through another machine, a connection must show a certificate signed by the key kept, and one you asked for must have met that machine straight first. A key that differs from the one kept is refused, saying so in the bar: through another machine, only that way is ruled out, and ssh straight there is still tried; straight over ssh, the connection stops until [`clackatoo forget HOST`](~clackatoo/reference/cli#forget), which only this machine's own windows and commands can run. A daemon reached straight that claims another machine's ID is caught later, rather than taken silently: its key won't match the one that machine is known by.

Each terminal's screen updates come on a QUIC stream of their own, apart from everything else, so a lost packet holds up only the terminal it belonged to.

## Moving onto UDP

Once connected, the window or command tries reaching the daemon straight over UDP, at the address ssh reached that machine at. If that works, the connection moves there, out of ssh:

- **It survives your network changing,** such as moving from home Wi-Fi to a phone's hotspot, or a short sleep. ssh's connection dies when that happens; the QUIC connection carries on from the new network, and the window doesn't notice.
- **Keys come back a little faster,** with ssh out of the way.

It finds out with a few probes, small datagrams the daemon sends straight back, while the connection carries on inside ssh, so nothing waits on the attempt. If none comes back within two seconds, as when a firewall blocks UDP, the connection stays inside ssh. If UDP stops getting through later, the connection goes back into ssh after about ten seconds, if ssh's connection is still up, and tries UDP again a minute after.

[`clackatoo ping --remote host`](~clackatoo/reference/cli#ping) says which way each round went: `(over UDP)` or `(through ssh)`.

The daemon takes UDP on the first free port from 60000 to 61000, the same range as mosh, when the first connection through ssh arrives. It never takes a new connection over UDP, only packets for connections already set up through ssh. Probes carry a key the daemon only gives out through ssh, and it answers nothing else, so the port gives nothing away to a scan.

### Firewalls

A firewall on the other machine has to let UDP in on those ports. If it already does for mosh, there's nothing to do. Otherwise, with `ufw`:

```sh
sudo ufw allow 60000:61000/udp
```

or with `firewalld`:

```sh
sudo firewall-cmd --permanent --add-port=60000-61000/udp && sudo firewall-cmd --reload
```

To use other ports, or none, set `CLACKATOO_UDP_PORTS` for the daemon on the other machine: a port, a range such as `60100-60200`, or `off` to keep every connection inside ssh. The daemon reads it when it starts, and one started by `--remote` gets it from the remote command:

```sh
clackatoo --remote server --remote-command 'env CLACKATOO_UDP_PORTS=60100-60200 clackatoo'
```

Through a jump host (`ProxyJump`), ssh reached the other machine at an address on its own network, which you usually can't reach directly, so the connection stays inside ssh.

## What the other machine needs

- **clackatoo of the same release, or one a release or so apart** (see [Protocol compatibility](#protocol-compatibility)). A daemon further apart speaks another protocol, which the window [offers to replace](#a-daemon-from-another-build).
- **clackatoo on the PATH ssh's commands get.** That is often shorter than an interactive shell's, and may leave out `~/.cargo/bin` or `~/.local/bin`. If it isn't there, the error is the remote shell's "command not found". `--remote-command` names it instead:

  ```sh
  clackatoo --remote desktop --remote-command ~/.cargo/bin/clackatoo
  ```

  The command is run by the remote shell, so it can be any command line that ends by running clackatoo, such as `env XDG_RUNTIME_DIR=/tmp/ck clackatoo`. Setting `CLACKATOO_REMOTE_COMMAND` saves typing it each time.

- **A way to log in.** A key loaded into `ssh-agent` logs in without asking anything. Otherwise, the window asks for ssh's password, or the key's passphrase, itself, in place of the terminal, and asks again on every reconnect. It also asks whether to trust a host ssh hasn't seen before. The `clackatoo` command leaves ssh to ask on the terminal it runs in.

- **On a server nobody is logged in to:** systemd removes your runtime directory (`/run/user/<uid>`) when your last session ends, taking the daemon's socket with it. The daemon keeps running, but the next connection can't find it and starts another. Run this once on the server so your runtime directory stays:

  ```sh
  loginctl enable-linger
  ```

## The other machine's desktop

A daemon `--remote` starts on your desktop starts before you've logged in to the desktop itself, over ssh, with no `WAYLAND_DISPLAY` of its own. Programs it starts still open their windows on the desktop once you log in there: the daemon follows the display your desktop tells systemd as you log in, and gives it to the terminals, agents and language servers it starts from then on, and shells already open take it before the next command they run. Once you log out and the desktop's socket has gone, what starts no longer gets its display. `ssh desk clackatoo display` says what programs on desk get now. See [the desktop's display](~clackatoo/reference/terminal-emulation#the-display).

## Options

| Option | Environment variable | |
|---|---|---|
| `--remote <HOST>` | | Use the daemon on `HOST`: `host`, `user@host`, `host:port`, or a name from your ssh config. |
| `--connect-picker` | | Pick the machine in the window first (see [Picking a machine](#picking-a-machine)). |
| `--remote-command <COMMAND>` | `CLACKATOO_REMOTE_COMMAND` | The command that runs clackatoo on the other machine. Default: `clackatoo`. |
| | `CLACKATOO_SSH` | The program run as ssh. Default: `ssh`. It is given ssh's options, then `--`, the host and the command. |

clackatoo passes ssh a few options of its own:

- `-T`: no terminal on the other end; the bridge doesn't want one.
- `ConnectTimeout=10`: an unreachable host fails after 10 seconds.
- `ServerAliveInterval=5` and `ServerAliveCountMax=3`: a connection that has died, such as when the laptop sleeps or changes network, is noticed within 15 seconds. A connection that has moved onto UDP carries on without it.

## When the other machine stops answering

If what the window sends goes unanswered for about three seconds, a red notice at the right of the top bar says so: `No contact with desktop for 12 s`, counting up. The window stays as it was. Editors carry on: what you type in them is kept and goes when the other machine answers again, landing exactly as it would have. Keys for terminals and everything else aren't sent late: the bar says `not connected: keys aren't sent`, and the pane flashes. The notice goes as soon as the other machine answers again.

An idle window notices within about 15 seconds, at its next keep-alive.

## When ssh's connection closes

The window opens a new ssh connection in the background, and the same connection carries on through it: nothing is redrawn and nothing is lost. It tries again after 1, 2, 4… up to 30 seconds. Over UDP, typing carries on meanwhile.

## When the connection drops

A connection ends only when the daemon stops, is upgraded or restarts, which tells the window at once, or after two minutes without hearing from the other end. An idle window sends a few bytes every 10 seconds, which keeps it, and your router's memory of it, alive.

The window then connects again by itself. The panes stay as they were, the top bar says `Reconnecting to desktop (12 s): the daemon is upgrading to a new build`, and it tries again after 1, 2, 4… up to 30 seconds; a connection that drops again within 10 seconds counts as a failed try. Enter and Esc are only offered when the first connection fails.

What you typed in an editor and the daemon hadn't yet taken is kept, and goes again when the window reconnects, catching up with what others changed meanwhile as if it had all happened live. A file renamed meanwhile keeps its new name. If the daemon restarted rather than upgraded, and the file is as it was, your changes go again just the same, and the bar says `reconnected; your unsaved changes to X were kept`. If the file changed meanwhile, the editor keeps your text and `:w` refuses: `:w!` writes your window's text over the file, and `:e!` takes the daemon's.

Nothing on the other machine stops when the connection drops: the terminals carry on, and output that arrives meanwhile is there when you reconnect.

## Bad networks

Every packet is 1200 bytes at most, which fits tunnels such as Tailscale's (1280). On a network losing packets, small ones (keys, their echoes, acknowledgements) are sent twice. After an outage, the window sends from a new UDP port, so the other machine picks up at once rather than waiting out QUIC's growing timers. A window that falls more than 8 MB behind is dropped and reconnects.

## Logs

Lines carry UTC timestamps, and say why each connection closed and which way it was going. The daemon logs to `daemon.log` in its runtime directory; windows log to `~/.local/state/clackatoo/window.log`, moved aside past 8 MB. `clackatoo ping --remote host --soak` keeps a connection open, logging each echo and every drop with its reason.

## Protocol compatibility

A window and a daemon from different releases still work together, as long as they're no more than a release or so apart: your laptop's window carries on when your desktop's daemon is updated first, and the other way round.

When a window connects, each side says which version of the protocol it speaks, and the connection runs at the older of the two. The newer side sends only what the older one can read: what's newer is left out, or turned into something the older one has.

- **A pane of a kind the window doesn't have** shows "This pane needs a newer clackatoo", naming the kind. The rest of the window works as usual.
- **Something the window offers that the daemon doesn't have** is refused, saying the daemon needs a newer clackatoo, which release it is, and how to update it: `clackatoo update` on the other machine, or `clackatoo upgrade` for this machine's daemon.
- **A message the window can't read** is passed over. If it changed a terminal's screen or a file being edited, the window asks the daemon for them afresh, and an editor's unsaved changes are kept and sent again, as after a lost connection. If that keeps happening, it stops asking after five times a minute and says to update.

Each release sets how far apart they can be, in two parts: the oldest window its daemon talks to, and the oldest daemon its window talks to. Further apart than that, the window says the daemon is from another build, as [below](#a-daemon-from-another-build).

The first release that does this, 2026.10.11 (protocol 73), takes windows from the release before it, 2026.10.10, but needs a daemon of at least its own: a daemon older than that can't say it agrees to an older version, so a window treats it as from another build.

## A daemon from another build

If the daemon on the other machine speaks another version of the protocol, the window says so, as it does [on this machine](~clackatoo/reference/window#a-daemon-from-another-build).

- **An older daemon there:** Enter replaces it using the clackatoo installed there: it upgrades it if it can, keeping its terminals, or else stops it, closing them. If that clackatoo is itself from another build, the window says to update it.
- **A newer daemon there:** Enter updates this machine's clackatoo to the latest release, as `clackatoo update` does, and the window opens again as the new build, connected to the same machine. This machine's own daemon moves onto the new build too, keeping its terminals, and its other windows ask to open again. If the daemon there is from a build newer than any release, there's nothing to update to, and the window says so: use a window from the daemon's build, or move that daemon onto a release. Esc quits, or goes back to the picker if you picked the machine there.

The `clackatoo` command doesn't replace anything. On the other machine, `clackatoo upgrade` upgrades its daemon, and `clackatoo stop` stops it.

If the daemon speaks the same protocol but is another release of clackatoo, the window connects as usual and says in the bar which side is older. When it's the other machine, `:update HOST` opens a tab there running `clackatoo update`, which installs the latest release and moves that daemon onto it; the window reconnects as it does after any upgrade. When it's this machine, `:update` updates it, and the window needs opening again.

## Two windows on one terminal

A terminal has one size, so when windows of different sizes show the same terminal, such as your laptop's and your desktop's, they take turns: the terminal takes the size of whichever window last opened on it, was resized or was focused.

## Typing over a slow connection

Every key goes to the other machine and back before the program's echo shows. So, as mosh does, the window guesses what most keys will show and draws it at once, underlined, until the daemon's screen shows the echo. It guesses printable characters, Backspace, and Left and Right within what you've typed on the line.

It only draws a key you typed at a shell prompt with nothing still running, as shell integration marks it: never for full-screen programs, programs using the mouse, a hidden cursor, or in normal mode, and never for keys typed while a command runs, so a password typed ahead for `sudo`, or at any prompt a program shows, is never drawn. A shell that doesn't mark its commands, such as bash before 4.4, gets no guesses. A wrong guess vanishes, and guessing waits for the program to echo again. The `predictecho` option turns this on or off.

[`clackatoo ping --remote host`](~clackatoo/reference/cli#ping) measures it: how long the daemon takes to answer, and how long a key takes to come back from a program. clackatoo's own share is small. To the same machine, where the network costs next to nothing, a key comes back in about a fifth of a millisecond over UDP, and a third through ssh, so nearly all of anything above that is the network.

## Forwarding ports

A program on the other machine that listens on `localhost`, such as a dev server, can't be reached from this machine's browser. `:PortForward` passes a port here to it, as `ssh -L` does, through the window's own connection, so it rides out network changes as the window does.

| Command | Does |
|---|---|
| `:PortForward 8080` | `localhost:8080` here reaches `localhost:8080` there. |
| `:PortForward 3000:8080` | Port 3000 here reaches 8080 there. |
| `:PortForward 3000:box:8080` | Port 3000 here reaches `box:8080`, as the other machine finds `box`; an IPv6 address goes in brackets, `3000:[::1]:8080`. |
| `:PortForward box:8080`, `:PortForward [::1]:8080` | The same port here, to a machine the other one reaches. Brackets hold only IPv6 addresses. |
| `:PortForward 0:8080` | Any free port here, which the bar names. |
| `:PortForward` | Lists the forwards, and how many connections each has open. |
| `:PortForward! 8080` | Stops forwarding port 8080 here, cutting off its connections; `:PortForward!` alone stops them all. |

- **Only this machine can connect:** the window listens on 127.0.0.1 and ::1, never the network.
- **Websites in your browser could reach a forwarded port**, as with `ssh -L`: a page you visit can make the browser connect to `localhost:PORT`, and with DNS rebinding, under a name of its own. The forwarded server's own Host check is the defence, as dev servers such as Vite's have; one that answers any Host shouldn't be forwarded while you browse untrusted sites.
- **`localhost` there** is 127.0.0.1, then ::1, as dev servers listen on either. Other names are looked up on the other machine, a few at a time, and one that doesn't answer gives up after 10 seconds without holding up others.
- **A forward passes on 32 connections at once**, and a window 64 in all; more wait until one ends.
- **What goes wrong is said once**, not for every try, until a connection works again: a port in use here, suggesting a free one; nothing listening there; a name the other machine can't find; nothing answering within 10 seconds.
- **Forwards last as long as the window.** When the connection drops, or the daemon there is upgraded, connections through them are reset, so a browser knows a page was cut short; the ports here stay open, and connections made meanwhile go through once the window is back.
- **A forwarded download sends after terminals' updates and keys**, and only as fast as the browser reads. On a slow link it also holds itself back so the network's queue stays short: keys typed while it runs, even the first after a pause, wait about 15 ms more, and the download still takes most of the link. Uploads do the same, and a download and an upload at once each keep to their own direction's speed, though keys may then wait about twice as long. On a link that slows down whenever it's busy, as bad Wi-Fi can, a forward goes more slowly than the link could carry. `:set noforwardpace` lets forwards take the whole link instead, at the cost of keys waiting behind them.
- **`client.forward(8080)`** in the [config](~clackatoo/reference/config) forwards a port each time a window opens on another machine; a window on this machine passes it over without a word.
- **A background tab's ports** show in the [list of background tabs](~clackatoo/reference/window#background-tabs), and Ctrl+F there forwards them all, each to the same port here.
- A daemon older than protocol 82 doesn't forward ports, and the bar says how to update it. A window on this machine has nothing to forward.

## From your phone

From a phone, use the web app the daemon serves instead: see [Your phone](~clackatoo/reference/phone).
