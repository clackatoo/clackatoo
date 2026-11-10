---
title: Connecting to Another Machine
type: reference
description: Picking a machine, another machine's workspaces in your bar, upgrading its daemon, machines passed on, panes on another machine, forwarding ports, and reaching it from your phone.
related:
  - clackatoo/reference/remote
---

You reach another machine's daemon with `--remote` or `:connect`, and can then
show its workspaces, open panes on it, and forward its ports.

## Picking a machine

`clackatoo --connect-picker` opens the window as a picker of the machine to
connect to, instead of connecting anywhere first. It lists:

- the last machines you've connected to, newest first;
- each `Host` in `~/.ssh/config`, and the files it includes, that names one
  machine: patterns with `*`, `?` or `!`, and everything under `Match`, are
  left out;
- Tailscale's peers that are online, if Tailscale is installed, by their
  MagicDNS name (or address, without MagicDNS). Phones are left out.

Type a few letters to narrow them, or a machine that isn't listed, such as
`jack@server:2222`, and <kbd>Enter</kbd> connects, as `--remote` would. If it
can't, the picker opens again saying why, so you can pick another. What's
typed is edited, and the machines moved through, with
[the finder's keys](~clackatoo/reference/workspaces#the-finder).
<kbd>Esc</kbd>, <kbd>Ctrl</kbd>+<kbd>C</kbd>, <kbd>Ctrl</kbd>+<kbd>;</kbd>,
<kbd>Shift</kbd>+<kbd>Esc</kbd> or <kbd>Ctrl</kbd>+<kbd>&#92;</kbd> closes the
window.

Your app launcher offers this as "Connect to…" when you right-click clackatoo,
along with the last three machines you connected to (see
[desktop](~clackatoo/reference/cli#desktop)).

In a window, `:connect`, or <kbd>Ctrl</kbd>+<kbd>B</kbd> r, opens the same
picker over your panes. Picking a machine opens a new window on it and leaves
this one as it is, and the keys that close the window above close only the
picker.

Each window remembers the machines it connects to, in `clackatoo/remotes`
under `$XDG_STATE_HOME` (`~/.local/state`), a line each, up to 20.

## Another machine's workspaces in your bar

`:connect HOST` has your daemon connect to the daemon on HOST, as `--remote`
does, and show HOST's workspaces in your bar beside your own. Windows, your
phone and scripts keep talking to your daemon, which passes what they ask on
to HOST and HOST's answers back. Terminals and chats there work as yours do:
keys, screens, history, copying, and searching. A new tab, a split, a rename
or a close in one of HOST's workspaces happens on HOST, as does a new pane
made from one of HOST's panes anywhere, which keeps them, so they carry on
while your laptop sleeps. Everything else in them works as your own does, on
HOST: editors, with undo, `:w`, `:w NAME` and `:recover`, their colors, git's
signs, what agents changed, and what HOST's language servers say; explorers
and folders' listings, whose `:w` moves and deletes HOST's files; reviews,
boards, and a chat's page. The finder searches the machine the focused pane is
on, so in one of HOST's workspaces it finds and greps HOST's files. HOST keeps
each file and makes every edit, so its own windows and agents edit a file
alongside you, and what you type while the connection is down is made once
it's back. A file cut in one machine's explorer can't be pasted in another's.
HOST needs this release of clackatoo or later for all of this; an older one's
editors and explorers say so in their panes, and its terminals and chats still
work.

After HOST restarts, as after a power cut, its terminals offer `R` to put what
they were running at the new shell's prompt, as they do on HOST (see
[after a restart](~clackatoo/reference/workspaces#after-a-restart)), and its
editors come back where they were. <kbd>Esc</kbd> dismisses an offer on HOST
and in every window, as typing into the terminal does. One dismissed while
HOST isn't connected never reaches HOST, so it comes back. A terminal HOST
keeps running says ` ↻` after its tab's name, and `:keep` marks one of HOST's
from here, on HOST, as it would there; a device on the web app can't, as it
can't mark your daemon's own. HOST's
[background tabs](~clackatoo/reference/workspaces#background-tabs) are shown
in its workspaces, in the icon and the list, and sending a tab there, bringing
one back, dismissing one and `:bg CMD` all happen on HOST. Not shown here: the
offer of a pane HOST lends one of your own workspaces. Chats in HOST's
workspaces come back after HOST restarts, and go on with an interrupted turn
after HOST upgrades, as they do there; in one of HOST's panes, the
[start page](~clackatoo/reference/chatting#the-start-page) lists the sessions
HOST's agent kept, to go on with one on HOST.

Your daemon keeps the connection. It makes it again whenever it drops, and
again after your daemon restarts. `:disconnect HOST` ends it, as does
`:disconnect` alone in one of its workspaces. While HOST isn't connected, its
workspaces stay as they last were, each pane says so over its last screen, and
keys for its terminals are turned down rather than sent late; so are they, and
anything else asked of it, when HOST hasn't answered for a few seconds ("desk
isn't answering"). If ssh needs a password and no window is open to ask, the
connection waits until one is. In your config, `client.connect("desk")`
connects whenever a window opens.

`:portforward 8080` in one of HOST's workspaces forwards this machine's port
8080 to HOST's, through your daemon's connection to it, which keeps listening
while the connection drops and is made again. `:portforward` there lists the
forwards and `:portforward! 8080` stops one, as for a window on another
machine. A window that's itself on another machine than its daemon, such as
a `--remote` window, forwards a port of its own machine instead: it listens
there, as for its daemon's ports, and your daemon passes each connection on
to HOST through its connection to it, where HOST's daemon connects. While
your daemon isn't connected to HOST, each such connection is refused, saying
so, rather than wait; the port stays open. Your daemon needs this release or
later: an older one is never asked, as it would connect on its own machine.

## Upgrading your daemon

[Upgrading](~clackatoo/reference/cli#upgrade) your daemon keeps HOST's
workspaces as they were, panes and layouts and all, those of machines passed
on through HOST too, while its connections are made again. What you typed in
HOST's editors that HOST had already taken isn't sent twice. Keys you typed
into HOST's terminals that HOST hasn't answered yet hold the upgrade back; if
HOST doesn't answer, the upgrade is refused, saying so, rather than send those
keys again after it: keys for a terminal are never sent twice.

Only a daemon already on a release that keeps all this can hand it over, so
the first upgrade onto one doesn't. Ports
[forwarded](#forwarding-ports) to HOST go on listening through the upgrade:
connections open through them are reset, so a browser knows a page was cut
short and asks again, and connections made while it upgrades go through once
HOST is reached again. File operations still waiting for HOST are cut off.

## Machines passed on

Your daemon passes on the machines it's connected to. If your laptop is
connected to desk, and desk to a server, the laptop's bar has the server's
workspaces too, as `notes@server`: named after the machine they live on, never
the way there. Everything in them works as for a machine you connected to
yourself, through a connection of your laptop's own to the server, which desk
carries on.

Desk could read or change what it carries, so your laptop checks the server by
its identity key: the one it met over ssh, or, for a server it has never
reached itself, the one desk vouches for the first time, kept from then on
(see [who's genuine](~clackatoo/reference/connections#how-it-connects)). A
connection you asked for yourself goes through another machine only once it
has met the server over ssh.

A machine reached several ways is shown once, by the shortest.
`:connect server` on the laptop as well has it go straight there, through desk
only while ssh can't get there, and back once it can, without its panes
noticing. Trying ssh again by itself never asks for a password, so a server
that wants one stays reached through desk, and the mouse over its workspace
says `:connect server` goes straight there: typed while it goes through desk,
that tries ssh straight there at once, asking your window ssh's questions, a
password included. Given it, the connection goes straight there without its
panes noticing, and the bar says so; turned down, or wrong, it carries on
through desk and the bar says why. Once a try you asked for has found that ssh
asks, your daemon stops trying ssh straight there by itself, which would only
fail a login there every half minute, until you ask again. Your daemon keeps
the connection as one you
asked for, so after it restarts it tries ssh straight there first, asking
again. `clackatoo connect server` does the same, waiting to say how it went.
`:disconnect server`
then leaves it reached through desk; one only desk passes on goes when desk
stops passing it on, or with `:disconnect desk`. Machines connected to each
other, or in a ring, each show the others once, and never themselves. The
mouse over such a workspace says the way, as `on server, through desk`.

`:connect server --no-share` keeps a machine, such as a server others share,
from the daemons connected to yours. It keeps the server's whole subtree too:
every machine the server passes on to your daemon, every machine your daemon
reaches only by a way through the server, and so on at any depth. A machine
you connected to yourself is its own, even if the server passes it on too, or
your daemon's way to it goes through the server. `--no-share` keeps the
server from the daemons connected to yours. It doesn't keep their traffic off
the server's wire, so such a machine is passed on as any other, without naming
the server in its way. A machine passed on both by the server and by a machine
you share is kept, unless you connected to it yourself. For the daemons
connected to yours, all these machines are treated the same way:

- Your daemon doesn't pass them on, refuses them a way through, and cuts any
  way already open.
- Your daemon tells them nothing of these machines: not their workspaces,
  chats, home folders, notices, ports, names or machine IDs. A pane of yours
  borrowed from one shows in their windows and phones as "Not shared — this
  pane is on a machine that isn't shared with you", on no machine and not
  saying what kind of pane it is. A clackatoo from before protocol 148 shows
  it as a pane that needs a newer clackatoo instead. A pane of yours lent to
  one isn't shown to them at all. They can't watch or
  end your daemon's connections, or list what it lends.
- Nothing they ask goes through your daemon to these machines: keys, a
  terminal's screen, files, the finder, the editor, forwarded ports, agents'
  sessions and chats, or new panes and workspaces there. This holds wherever
  the thing they name is shown: in the machine's own layout, in a pane of
  yours borrowed from it, in a pane of yours lent to it, or in another
  machine's layout.
- They can't close, restart or dismiss anything there either, even by way of
  your own layout. They can't close a pane borrowed from the server, or a tab
  or workspace holding one, and can't bring back or dismiss a background tab
  the server lends you.

Each refusal comes before anything else is done. It says only "that machine
isn't shared with other machines", naming no machine. That something is
refused rather than unknown does still say that something is there. Said of a
machine already connected, or as what's kept changes, it also drops whatever
those daemons were following there, and what they were waiting for from it.

Your own windows, scripts and phone still reach these machines as before. So
does the server's own daemon, if it connects to yours: what you lend it, and
what it passes on, are its own. That only holds once your daemon's own
connection to the server has proved, by the server's key, that it is that
machine. A daemon that merely says it's the server, or says it passes the
server on, gets nothing for it. A daemon counts as one connected to yours only
once it says it relays for
windows of its own, as every clackatoo since protocol 92 does before it's told
anything. A daemon older than that is treated as a window.
`:connect server --share` passes it on again, and the bar says when one goes.
Your daemon keeps this with its connections. Each machine needs this
release of clackatoo or later to pass machines on or be passed on.

## A pane on another machine

`@host` after a kind of pane opens it on that machine, wherever the workspace
is: `:Terminal@desk` opens a terminal on desk in a new tab of the workspace
you're in, `:|Terminal@desk` splits one in beside the focused pane, and
`:Editor@desk notes.md`, `:Explore@desk` and the rest work the same. `@local`,
or this machine's own name, is your daemon's machine. Without `@`, a new pane
goes on the machine of the pane it's made from, so splitting a pane on desk,
or opening a file or the finder from it, stays on desk.

In one of your own workspaces, desk lends your daemon the pane: desk keeps it,
as one of its own, in a workspace no window sees, and your workspace shows it.
Desk's own windows, its phone and its scripts can't see it or act on it,
unless desk connects to your laptop too; a third machine connected to both
shows it, and can type into it, but not close it, and reaches it only while
your laptop is connected to desk too. What you type runs there, and its files,
explorer and finder are desk's. A chat in it, and any chat its agent starts,
marks your workspace in the bar as your own chats do. Its tab ends `@desk`,
unless `showhost` is `never`. Your daemon keeps it with your workspaces, and
desk keeps what it shows, so it comes back after either restarts: a terminal
as a new shell in its folder on desk, as desk's own do. While desk can't be
reached, the pane says so over its last screen. Closing it closes it on desk,
at once or once desk is back; its program exiting on desk closes it here. Desk
needs this release of clackatoo or later; an older one says it's too old to
lend panes. `:bg CMD` beside such a pane starts the command on desk, in a
shell desk lends you as a background tab of your workspace (see
[background tabs](~clackatoo/reference/workspaces#background-tabs)).

Desk never throws away what it lent on so little: a pane your daemon has
stopped showing, or a `[No Name]` with changes you closed, comes back as one
of desk's own workspaces, `from-laptop`, unless it's a shell running nothing.
On desk, [`clackatoo lent`](~clackatoo/reference/cli#lent) lists what it lent,
and `clackatoo lent reclaim N` takes one back; after 30 days without your
laptop, desk takes them all back. If desk restarts without a pane it lent, the
bar says so. An older clackatoo reading the workspaces kept shows a workspace
lent to another machine as one of its own, with no name, and a pane borrowed
from another machine as a new shell here.

It works the other way round too, even though desk can't reach your laptop:
`:|Terminal@local` in one of desk's workspaces splits in a terminal on your
laptop, and any pane made beside it is your laptop's as well. Desk's layout
keeps it; the terminal stays on your laptop, and what you type goes straight
to it, never to desk and back. Its tab ends `@local`. Desk's own windows, and
any machine that can't reach your laptop, show "a terminal on laptop, not
reachable from here" in its place; a machine connected to both shows it, and
so does desk once it's connected to your laptop.

While desk is away, such panes are parked in a workspace of your laptop's
named after it, `from-desk`, a tab each, so you can go on using them. Once
desk is back they're back where they were and that workspace goes; nothing is
moved or lost. `:rename NAME` there names it: it's called NAME whenever desk is
away from then on, after either daemon restarts too, and panes ever given back
from desk come back under that name. Closing `from-desk` closes them all; a new
pane beside one waits until it's back in desk's workspace. Closing one from your laptop closes
it on both. Closed on desk, a shell running nothing closes, and anything else
comes back as one of your laptop's own workspaces, `from-desk`; so does one
desk restarted without. Desk needs this release of clackatoo or later: an
older one says it's too old to show a pane from your machine.

An agent in a chat borrowed from desk opens what it shows you beside the
chat, as one of your own does: a file, a terminal it runs a command in, or a
subagent's chat from its card. Desk keeps each, lent to your laptop as the
chat is, and your laptop shows it beside the chat; while your laptop's daemon
isn't connected to desk, or runs a release before this one, the agent is told
why it can't.

`:workspace new@desk NAME` makes a workspace on desk, as does
`:workspace NAME@desk` if desk has none of that name, and the `+` after your
workspaces asks which machine. It lives on desk, so it carries on while your
laptop sleeps, and desk's own windows have it too.

## Forwarding ports

A program on the other machine that listens on `localhost`, such as a dev
server, can't be reached from this machine's browser. `:portforward` passes a
port here to it, as `ssh -L` does, through the window's own connection, so it
rides out network changes as the window does.

| Command | Does |
|---|---|
| `:portforward 8080` | `localhost:8080` here reaches `localhost:8080` there. |
| `:portforward 3000:8080` | Port 3000 here reaches 8080 there. |
| `:portforward 3000:box:8080` | Port 3000 here reaches `box:8080`, as the other machine finds `box`; an IPv6 address goes in brackets, `3000:[::1]:8080`. |
| `:portforward box:8080`, `:portforward [::1]:8080` | The same port here, to a machine the other one reaches. Brackets hold only IPv6 addresses. |
| `:portforward 0:8080` | Any free port here, which the bar names. |
| `:portforward` | Lists the forwards, and how many connections each has open. |
| `:portforward! 8080` | Stops forwarding port 8080 here, cutting off its connections; `:portforward!` alone stops them all. |

- **Only this machine can connect:** the window listens on 127.0.0.1 and ::1,
  never the network.
- **Websites in your browser could reach a forwarded port**, as with `ssh -L`:
  a page you visit can make the browser connect to `localhost:PORT`, and with
  DNS rebinding, under a name of its own. The forwarded server's own Host
  check is the defense, as dev servers such as Vite's have; one that answers
  any Host shouldn't be forwarded while you browse untrusted sites.
- **`localhost` there** is 127.0.0.1, then ::1, as dev servers listen on
  either. Other names are looked up on the other machine, a few at a time, and
  one that doesn't answer gives up after 10 seconds without holding up others.
- **A forward passes on 32 connections at once**, and a window 64 in all; more
  wait until one ends.
- **What goes wrong is said once**, not for every try, until a connection
  works again: a port in use here, suggesting a free one; nothing listening
  there; a name the other machine can't find; nothing answering within 10
  seconds.
- **Forwards last as long as the window.** When the connection drops, or the
  daemon there is upgraded, connections through them are reset, so a browser
  knows a page was cut short; the ports here stay open, and connections made
  meanwhile go through once the window is back.
- **A forwarded download sends after terminals' updates and keys**, and only
  as fast as the browser reads. On a slow link it also holds itself back so
  the network's queue stays short: keys typed while it runs, even the first
  after a pause, wait about 15 ms more, and the download still takes most of
  the link. Uploads do the same, and a download and an upload at once each
  keep to their own direction's speed, though keys might then wait about twice
  as long. On a link that slows down whenever it's busy, as bad Wi-Fi can, a
  forward goes more slowly than the link could carry. `:set noforwardpace`
  lets forwards take the whole link instead, at the cost of keys waiting
  behind them.
- **`client.forward(8080)`** in the [config](~clackatoo/reference/config)
  forwards a port each time a window opens on another machine; a window on
  this machine passes it over without a word.
- **A background tab's ports** show in the
  [list of background tabs](~clackatoo/reference/workspaces#background-tabs),
  and <kbd>Ctrl</kbd>+<kbd>F</kbd> there forwards them all, each to the same
  port here.
- A daemon older than protocol 82 doesn't forward ports, and the bar says how
  to update it. A window on this machine has nothing to forward.

## From your phone

From a phone, use the web app the daemon serves instead: see
[Your phone](~clackatoo/reference/phone). It shows which machine each workspace,
chat and terminal from another is on, with the same ring as the bar, though
never what ssh said; it can't connect, disconnect or give ssh a password. The daemon would take an image
pasted into a terminal from a phone as its path, within the limits of the
photos it sends chats, but never onto the desktop's clipboard, which isn't the
phone's; the web app doesn't offer it.
