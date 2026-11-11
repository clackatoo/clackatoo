---
title: Workspaces, Tabs, and Panes
type: reference
description: Workspaces, tabs, panes and splits, zooming a pane, opening again what was closed, what comes back after a restart, keeping a terminal running, background tabs, the command line, moving between panes, Ctrl+B, and the finder.
related:
  - clackatoo/reference/window
---

A workspace holds tabs, and a tab holds panes, laid out as splits; the command
line and <kbd>Ctrl</kbd>+<kbd>B</kbd> make them and move between them.

## Workspaces, tabs, and panes

The window shows your work at three levels:

- **Workspaces**, one for each piece of distinct work, such as a project.
  Their names run along the [top bar](~clackatoo/reference/frame#the-frame).
  Often there's only one.
- **Tabs**, inside a workspace. They run along the bottom bar, numbered, and
  named after the title of their focused pane's program, or, for a terminal
  whose program set none, the folder its shell is in (`~` for your home, or
  for a terminal on
  [another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar),
  yours there), unless you've named them. A program's title, or a folder's
  name, shows without control characters or anything that shows nothing
  itself, as a mark that turns text round, so it can't hide or move what's
  beside it, in a tab's name, the window's title or a notification. Only the
  zero-width joiners and non-joiners that emoji and scripts such as Persian
  need stay, between two characters past ASCII that show, three in a row at
  most, so 👩&#x200D;👩&#x200D;👧 shows as one; agents' words to each other keep
  them by the same rule.
- **Panes**, which a tab is split into, side by side or stacked. Each shows a
  terminal, a [chat with an agent](~clackatoo/reference/agents), a
  [file being edited](~clackatoo/reference/editor), or one of the other kinds:
  an [explorer](~clackatoo/reference/explorer), a
  [board](~clackatoo/reference/boards), a
  [review](~clackatoo/reference/agent-changes#reviewing-its-changes), or a
  chat's browser page. A split shares its space equally until you
  [resize it](#resizing-panes).

Workspace names are unique on each machine, so one from another machine can
share a name with one of yours. It then shows its machine after its name, as
`notes@desk`; [`showhost`](~clackatoo/reference/config#options) changes when.
`:workspace notes@desk` switches to it by its machine, and `:workspace notes`
alone to your own.

A tab says what its chats are doing after its name: `⋯` while one's agent is
at work, `?` while one asks you something, and `•` when one finished while
this window wasn't showing it. A workspace in the top bar says the same of all
its chats, so a chat in another workspace that wants you shows there. Where
several would say different things, the one that needs you most shows: `?`,
then `⋯`, then `•`. A tab whose program says how far a long task has got ends
with that last: ` 42%`, ` …` busy, ` ‖` paused, or ` ✗` failed (see
[progress](~clackatoo/reference/terminal-emulation#progress)).

What's unseen is each window's own: a chat counts as seen once its tab is
shown in a focused window, and a chat finished while no window or phone was
looking is unseen to a window that comes to it later. A chat that starts again
loses its `•`. While the daemon is out of reach, no chat is said to work or
ask. A window also shows a
[desktop notification](~clackatoo/reference/chat-view#desktop-notifications)
for a chat it marks, as it finishes or asks.

The daemon keeps all of them, as it keeps terminals. Closing the window closes
nothing, and the next window opens on the workspace, tab and pane last used.
Each window shows one workspace, so two windows can show two workspaces, or
two tabs of one.

### Zooming a pane

<kbd>Ctrl</kbd>+<kbd>B</kbd> `z`, or `:zoom`, zooms the focused pane: it
fills its tab, and the tab's other panes are hidden, still running. The tab's
name ends with ` Z` while it's zoomed. The same key, or `:zoom` again, puts
the splits back as they were.

Going to another of the tab's panes, as <kbd>Ctrl</kbd>+<kbd>B</kbd> `h` or
<kbd>Ctrl</kbd>+<kbd>L</kbd> does, or splitting a pane in it, puts the splits
back first, as tmux does. So does the zoomed pane closing. Looking at another
of its terminals on [your phone](~clackatoo/reference/phone) doesn't, on this machine or through another, as the
phone shows one terminal at a time anyway.

The zoom belongs to the tab, which the daemon keeps, as it keeps the layout:
every window showing the tab shows it zoomed, and goes to the zoomed pane. It
stays zoomed through an upgrade and a restart.

### Opening again what was closed

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>T</kbd>, or `:reopen`, opens again the
pane or tab you closed last, as a browser does. Each time opens the one before
it, back through the last 20. What comes back is what a
[restart](#after-a-restart) brings back:

- a terminal, as a new shell in the folder its shell was in, offering the
  command it was running, which <kbd>R</kbd> puts at the prompt;
- an editor, on its file, with its cursor and view where they were;
- an explorer or a folder, on its root;
- a chat, on its agent's session, which it goes on with.

A pane goes back where it was: beside the panes it was beside, on the same
side, as big as it was. If those have gone, it goes beside the focused pane,
or into a tab of its own if the tab has gone too. A tab goes back to its place
among its workspace's tabs. Either gets the focus.

Only what you close is kept: a pane or tab closed with
<kbd>Ctrl</kbd>+<kbd>B</kbd> `x`, `:close`, `:q` and the like, in any window
or with the `clackatoo` command. A terminal whose program ends isn't, nor is a
[subagent](~clackatoo/reference/chats-together#subagents) sent back to its
parent, which is still there. One that closes with its parent's tab, as
its parent closed first, is kept with the tab, and comes back under its
parent. A terminal you [kept running](#keeping-a-terminal-running)
comes back offering its command, not running it. The list is the daemon's:
any window can open again what another closed. It's kept through an upgrade,
but not through a restart.

### After a restart

The daemon keeps your workspaces, tabs and panes, so they come back after a
restart, a crash or a power cut: with their names, order, splits and sizes,
and the tab and pane you were on. Each terminal comes back as a new shell in
the folder it was in, or your home if that's gone. Each editor comes back on
its file with its cursor and view where they were, still in the directory its
file was opened from (or its file's own folder if that's gone), and with its
changes not saved if it had any, as `[No Name]` files do; explorers and
folders keep their root; boards and reviews come back too, and chats on their
sessions, a [subagent](~clackatoo/reference/agents-lasting#what-lasts) still
its chat's. Running programs don't, but what each terminal printed does (see
[Scrollback after a restart](#scrollback-after-a-restart)). An editor whose
file has been deleted is left out.

A shutdown or reboot keeps them too, even though it ends your shells along
with the daemon. Before the machine shuts down, systemd's login manager,
logind, tells the daemon and waits for it. The daemon writes the workspaces as
they are, lets the shutdown go on, and then writes nothing more, so shells
that end with everything else don't take their panes or workspaces with them.
If the shutdown is called off, logind goes away, or the daemon is still
running a minute later, it writes them again at once and carries on keeping
them.
It also writes them before the machine sleeps. If logind can't be asked, as on
a system without systemd, the daemon's log says why in one line;
`CLACKATOO_LOGIND=off` in the daemon's environment stops it asking, as
`CLACKATOO_DISPLAY=off` does for tests. A terminal
whose shell a signal ended (SIGTERM, SIGHUP or SIGKILL, which a shutdown or
`systemctl stop` sends) keeps its pane for 2 seconds. If the daemon is asked
to stop in that time, the pane is kept and comes back like any other, in the
folder its shell was in. Otherwise it closes. A shell that exits by itself,
with `exit` or <kbd>Ctrl</kbd>+<kbd>D</kbd>, still closes its pane at once,
even as the daemon stops, and so does a terminal an agent started.
Signing out stops nothing while you
[linger](~clackatoo/reference/cli#lingering), and a daemon that isn't in a
scope of its own outlives your sign-outs too.

A terminal whose shell was running a command shows "was running: cargo watch"
at its bottom right. In normal mode, `R` puts the command at the new shell's
prompt, where you read it and press <kbd>Enter</kbd> to run it; <kbd>Esc</kbd>
dismisses it, and selecting either does the same. Nothing runs by itself.
Typing into the terminal dismisses it too, in every window. This works in
bash, zsh, and fish 4 when clackatoo starts them as your shell: their
integration marks each command with a secret only it and clackatoo know, and
only a command marked so is offered, never one a program printed. A command
the shell keeps out of its history is never offered: one typed with a space
before it, one bash leaves out (`HISTCONTROL`, `HISTIGNORE`), one your
`zshaddhistory` or `fish_should_add_to_history` turns away, and everything in
`fish --private`. bash is cautious here: with `ignoredups` (part of
`ignoreboth`) or `erasedups`, running the same command twice in a row gives no
offer for the second. A terminal started with a command of its own, or a shell
started any other way, comes back without an offer. An offer not yet taken or
dismissed stays through an upgrade, which makes no new ones, and shows in a
workspace on
[another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar)
too. An upgrade from a release before this one, which can't hand its offers
over, takes those still open from the workspaces it kept, once, unless what's
kept differs from the panes it hands over, as when its last write failed.

To start once with none, run `clackatoo stop`, then
[`clackatoo daemon --fresh`](~clackatoo/reference/cli#daemon).

### Scrollback after a restart

Each terminal's history comes back with it, above the new shell's prompt: its
last 10,000 lines, with their colours, weights and underlines, and the lines
on its screen. A window draws them dimmed, under a line across the pane that
says when the restart was: `── restarted 9 Nov 14:02 ───`. That line is drawn
by the window, not printed into the terminal, so no program can write over
it; a window or phone from before this release shows its text as a plain
line. It's history only: nothing in it runs, or reaches the new shell.
Restarted again, a terminal keeps the lines from before each restart, each
restart's line among them.

**When it's written:** the daemon notes which terminals printed something, and
writes theirs every 30 seconds, as it stops or upgrades, and before the
machine sleeps or shuts down (logind waits for it, as it does for the
workspaces). A crash, a power cut or `kill -9` keeps what was written last.
Writing happens on a thread of its own: the daemon only takes a copy of each
terminal's newest lines, which it can do in a moment, and leaves turning them
into a file to the thread. A terminal printing without a pause is put off a
few rounds, until its output rests. A daemon started with
`--scrollback-compaction off` keeps fewer lines: the newest that fit in 2 MiB
of cells.

**Where:** a file for each terminal in `~/.local/state/clackatoo/history`, a
folder only you may open (0700), each file only yours to read (0600), written
beside its old copy, synced and renamed over it, so a crash leaves the old one
or the new, never half of one. Each holds at most 10,000 lines and 8 MiB of
text, compressed. A terminal you close takes its file with it, and a restart
removes the files of terminals that didn't come back. Only the daemon that
keeps the workspaces keeps terminals' history.

**What's never kept:**

- an agent's own terminals, and the panes that show what an agent's command
  printed;
- a terminal you marked off (below), but for the commands you kept;
- what a program marked `nosave` or `secret` (see [secret
  regions](~clackatoo/reference/secret-regions)), and a line you typed a
  password on unseen: a run of them comes back as `[secret output not
  kept]`. A program can ask for what's kept to be forgotten too, as
  `:forget` does.

**Your say over it:**

| Command | What it does |
|---|---|
| `:forget` | Drops what's kept of the focused terminal at once, and what it shows now: only what comes after is kept. |
| `:keep history off` | Keeps none of its history from now on. The mark is kept with the layout, so it lasts across restarts. |
| `:keep history on` | Keeps it again. |
| `gs` in normal mode | In a terminal marked off, keeps the command the cursor is on, with what it printed, all the same. |

These are yours alone: no agent's tool asks them, and a
[device on the web app](~clackatoo/reference/phone) can't.

**Settings,** in the daemon's environment:

| Variable | Default | What it does |
|---|---|---|
| `CLACKATOO_HISTORY` | `~/.local/state/clackatoo/history` | The folder it's kept in, or `off` to keep none. |
| `CLACKATOO_HISTORY_LINES` | `10000` | The most lines kept of each terminal. |
| `CLACKATOO_HISTORY_EVERY` | `30` | How often, in seconds, what changed is written. |

Images programs show aren't kept: what comes back is the text around them.

### Keeping a terminal running

`:keep` marks the focused terminal to run its command again by itself as the
daemon starts afresh, in its folder: the one exception to nothing running by
itself, and only because you asked. Its command is the one its shell runs, or
else ran last, as the shell's integration said it with its secret, taken as
you mark it and kept: never what a program printed, nor whatever runs later. A
shell that hasn't said, as one clackatoo didn't start as your shell, can't be
marked. A terminal marked so offers nothing after a restart: it runs. The
secret is one of the shell's variables, so any code running in the shell, a
command typed at it, yours or an agent's, a function or a script it sources,
can read it. What the shell said a command is stays, whatever that command
prints with the secret while it runs; but once it has printed the marks that
say it ended (`D;0`), it can say another started, with the secret, and that's
the command an offer or `:keep` would then take. The secret tells a program
from the shell, not code you let into the shell from the shell itself, which
could as well edit your `~/.bashrc`. Read what `:keep` says it keeps.

| Command | Does |
|---|---|
| `:keep` | Runs its command again as the daemon starts afresh. |
| `:keep always` | That, and runs it again as it ends: a second after its shell's prompt comes back after that very command, never after one you ran since, and in a new shell in its pane if the shell itself ends. Each time it ends again within ten seconds of starting it waits twice as long, up to a minute, and after five such ends in a row it gives up until you mark it again. |
| `:keep off` | Takes the mark away. |

It's only ever typed at the shell's prompt, while the shell has the terminal
to itself. The marks that say a command ended and a prompt began aren't
signed, so a program can print them; so a command's end is taken for one, and
anything typed, only while the terminal's foreground process group is the
shell's, the shell is still the program it started as, not one it `exec`'d,
and it runs nothing in its own group, as a shell without job control does; and
typing waits a tenth of a second to see that still holds. That leaves a
little: a program that has left the shell's group yet reads the terminal, or a
command line whose programs follow one another faster than that, could still
be typed into. So could the shell's own `read`, `select` or zsh's `vared`,
which the shell runs itself, so nothing tells them from its prompt: a kept
command that's a function or script printing text it fetched, then asking with
`read`, could have that text mark an end and a prompt, and the command would
be typed as the answer. A run the daemon typed is the command's even if your
shell doesn't sign it, as bash doesn't sign a command it leaves out of its
history with `ignoredups`.

A `$SHELL` that starts another shell rather than becoming it, as `sh -c fish`,
a distrobox or toolbox wrapper, or a `nix-shell` does, never has its terminal
to itself: nothing is typed into its terminals, by this or by agents'
`run_in_terminal`, and what's said names the cause. Set the daemon's `$SHELL`
to the real shell.

Typing into a terminal kept running always, as <kbd>Ctrl</kbd>+<kbd>C</kbd> to
stop it, or a command typed ahead while it runs, pauses running it again: the
user has taken over. Its tab says ` ↻‖`, and as the command ends it offers `R`
to run it again, as after a restart. Marking it again, `:keep always`, ends
the pause. So does running the command there again yourself, but only when
your shell signs it: bash with `ignoredups` or `ignoreboth`, as Ubuntu sets
it, doesn't sign the same command twice in a row, so `R` and <kbd>Enter</kbd>
leave it paused, and `:keep always` is the way out. The mark stays for the
next fresh start meanwhile, and the pause, and the waits between runs, outlast
an upgrade and the tab going to the background and back.

A tab holding a terminal kept running says ` ↻` after its name, ` ↻‖` while
paused, and ` ↻✗` once it has given up; so does a
[background tab](#background-tabs) in the list. Only you set the mark, from a
window or with [`clackatoo keep`](~clackatoo/reference/cli#keep), and a device
on the [web app](~clackatoo/reference/phone) can't.
[Agents' tools](~clackatoo/reference/agent-tools#what-the-agent-can-show-you)
don't set it, and refuse a command line that runs `clackatoo keep` or
`clackatoo bg`; but that's a guard against mistakes, not a boundary: a command
an agent types into a terminal follows the
[ask-before-running rules](~clackatoo/reference/agent-tools#what-the-agent-can-show-you),
and a script it runs could do anything you could. `clackatoo keep` run in the
terminal itself keeps the command before it, never itself. The mark is kept
with the workspaces, and handed over as the daemon upgrades, which keeps
terminals running anyway. A terminal on another machine can't be marked from
here.

A pane closes when its program exits, as in other terminals. A tab closes with
its last pane, and a workspace with its last tab, unless it has a
[background tab](#background-tabs) still running, which comes back in its
place. When the last workspace closes, so does the window.

### Background tabs

A background tab is a tab kept running out of the tab bar, as a web server or
a `trail serve` you only glance at now and then, so it doesn't take a tab's
place. It keeps its splits, and holds terminals alone. Each workspace has its
own; the daemon keeps them, so every window and the
[web app](~clackatoo/reference/phone) sees them, they're handed over as the
daemon upgrades, and they come back after a restart as tabs do, each terminal
a new shell offering `R` to put what it ran at its prompt. A window from
before protocol 96 doesn't show them, and leaves them be.

| Keys | Command | Does |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>B</kbd> `b` `t` | `:bg` | Sends the tab shown to the background. The workspace's only tab can go too: a shell in its folder takes its place. |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> `b` `p` | `:bgpane` | Sends the focused pane to the background, in a tab of its own. A tab's last pane takes the tab with it. |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> `b` `l` | `:Background` | Lists the background tabs. Selecting the icon after the tabs does too. |
| | `:bg CMD` | Starts `CMD` in a new background tab of the workspace shown, typed at a new shell's prompt in the focused pane's folder, as you would type it, so its shell's marks say when it runs and how it ends. Nothing is shown. [`clackatoo bg`](~clackatoo/reference/cli#bg) does the same from a shell, and an agent can too, with [`run_in_background`](~clackatoo/reference/agent-tools#what-the-agent-can-show-you). |

After the tabs, an icon says how many of the workspace's background tabs are
running: `⚙ 2`. A tab counts as running while a command runs in one of its
terminals, as their shells' marks say, which bash, zsh and fish 4 make when
clackatoo starts them as your shell; one whose shell is at its prompt again is
idle, and a terminal whose shell doesn't mark its commands counts as running
while it's alive. With none running, the icon goes. A command in one that
fails, or a shell in one that exits with a failure, turns the icon red with
its exit status, as a failed command's mark is, `⚙ 1 ✗ 2`, until a window
lists them.

The list is the [finder's](#the-finder), with this workspace's background tabs
first, then the other workspaces': each with its name (its program's title,
else the command its shell runs or ran last, else its folder), how it's
getting on (`running`, `idle`, `exited 2`, with any failure since in red), the
TCP ports its programs listen on (`· :8080`), and the workspace of one from
another. Beside them is the last screenful of the one picked. Both are looked
at again every two seconds while the list is open, and never while it's
closed: the daemon finds the ports apart from its other work, from the sockets
the processes of each terminal's shell's session hold, so a server that leaves
its shell's session, as a daemon does, isn't seen. Typing narrows them.

| Key, in the list | Does |
|---|---|
| <kbd>Enter</kbd> | Brings it back as a tab of its workspace, after the others, going there. A terminal of it whose shell has exited starts again as a new shell in its folder, offering `R` to put what it ran at the prompt. |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> | Dismisses it, closing its terminals. One still running asks first, in the list's top right: a second <kbd>Ctrl</kbd>+<kbd>D</kbd> within three seconds ends it. |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> | In a window on [another machine](~clackatoo/reference/connecting#forwarding-ports) than the daemon, or for a background tab of another machine's workspace, forwards the ports it listens on here, each to the same port, as `:portforward 8080` does. |
| <kbd>Esc</kbd> | Closes the list. |

A background tab whose terminals have ended stays, its last screen readable,
until it's dismissed or brought back: nothing vanishes by itself. Closing a
workspace closes its background tabs with it; a window from before protocol
96, which doesn't know they're there, can't. A workspace keeps 64 background
tabs at most, and chats don't go to the background, only terminals.

A workspace on
[another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar)
shows its background tabs here as your own do: in its icon, in the list, with
the ports they listen on and their ` ↻` marks, and everything above works on
them, on that machine: `:bg`, `:bgpane`, `:bg CMD` (in its focused pane's
folder there), <kbd>Enter</kbd>, <kbd>Ctrl</kbd>+<kbd>D</kbd> and `:keep`.
`:bg CMD` beside a pane of one of your own workspaces borrowed from another
machine runs the command on that machine, in that pane's folder: a new shell
it lends you, in a background tab of your workspace, which shows how it's
getting on as that machine says. It's kept, once its program ends, until you
dismiss it, which closes it there (once that machine is reached again, if it
can't be now), or bring it back, as a tab of your workspace, a new shell there
if it had ended. It comes back after either machine restarts, and through
either upgrading. That machine needs this release of clackatoo or later. A
borrowed pane itself can't go to the background, nor can a tab holding one.

### The command line

In normal mode, `:` opens a command line, in the palette in the middle of the
window. <kbd>Enter</kbd> runs the command. <kbd>Esc</kbd>, or
<kbd>Backspace</kbd> past the colon, closes it.

While it's open, the bottom bar keeps the mode and the tabs, and the focused
pane's cursor is drawn hollow, since what you type goes to the line. The same
holds for a search, a new name, the finder and the machine picker. Suggestions
appear as you type without changing your text, with short descriptions beneath
their names. Commands, options, and argument choices explain their purpose;
file, workspace, and machine suggestions explain what selecting them does.
<kbd>Tab</kbd> completes the word before the caret: a command's name, a kind
of pane after a split's mark (`:|Te`), an option after `:set`, with `no` or
`inv` before a switch and `name=` before a choice's values, a file or folder
after `:e`, `:w`, `:Editor` and `:Explore` (spaces in names are written `\ `),
a workspace after `:ws`, a machine after a kind of pane's `@` and after
`:ws new@`, a lesson after `:Tutor`, and the words `:gitsigns`, `:devices` and
`:Review` take. One choice goes in when you press <kbd>Tab</kbd>. With more,
<kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd>, or
<kbd>Ctrl</kbd>+<kbd>N</kbd> and <kbd>Ctrl</kbd>+<kbd>P</kbd>, cycle through
the matches; once cycling, <kbd>Down</kbd> and <kbd>Up</kbd> do too, and past
either end back to what you typed. <kbd>Enter</kbd> runs only the text in the
prompt, and <kbd>Escape</kbd> closes it. Files are looked up in the background
and complete only when the focused pane's files are on this machine; a slow
folder says `listing…` while it's read. Connected host names are suggested as
you type; explicit <kbd>Tab</kbd> also includes SSH config and recent
connections.

<kbd>Up</kbd> and <kbd>Down</kbd> go back through the commands you've typed,
in this window and others, keeping to those that start with what's typed.
They're kept in `clackatoo/history` under `$XDG_STATE_HOME`, the last 100,
readable only by you; a line you start with a space isn't kept, nor is a
device's pairing code, and a line of either kind is left out when the file is
read, if an older build kept it. <kbd>Left</kbd>, <kbd>Right</kbd>,
<kbd>Home</kbd> and <kbd>End</kbd> move the caret, and <kbd>Ctrl</kbd> or
<kbd>Shift</kbd> with <kbd>Left</kbd> or <kbd>Right</kbd> move it a WORD,
anything between blanks, as Vim's command line does. The command line, every
pane's search (a terminal's, an editor's, a chat's, an explorer's, a board's,
and a review's) and an editor's new name all take keys this way; the names and
entries typed into an explorer or a board, and the finder, don't.
<kbd>Ctrl</kbd>+<kbd>U</kbd> deletes everything before the caret, and
<kbd>Ctrl</kbd>+<kbd>W</kbd> or <kbd>Ctrl</kbd>+<kbd>Backspace</kbd> the word
before it. <kbd>Alt</kbd> and a key types the key's character.
<kbd>Ctrl</kbd>+<kbd>R</kbd> and a register's name puts in what the register
holds: <kbd>Ctrl</kbd>+<kbd>R</kbd> <kbd>Ctrl</kbd>+<kbd>W</kbd> the word
under an editor's cursor, <kbd>Ctrl</kbd>+<kbd>R</kbd> `:` the last command,
and <kbd>Ctrl</kbd>+<kbd>R</kbd> `/` the last search. A count before `:` in an
editor starts the line with that many lines, `5:` giving `:.,.+4`; in any
other pane, `5:` opens the command line as `:` does. From a selection it's
`'<,'>`, count or not.

| Command | Does |
|---|---|
| `:portforward 8080`, `:portforward 3000:host:8080`, `:portforward`, `:portforward! 8080` | In a window on another machine, forwards a port here to one there, lists the forwards, or stops one: see [Forwarding ports](~clackatoo/reference/connecting#forwarding-ports). |
| `:Terminal` | Opens a terminal in a new tab. |
| `:\|Terminal` | Splits the focused pane, with a new terminal to its right. |
| `:\|<Terminal` | The same, to its left. |
| `:-Terminal` | The same, below it. |
| `:-^Terminal` | The same, above it. |
| `:Terminal@desk`, `:\|Terminal@desk`… | The same, on the machine named after `@`: one the daemon is [connected to](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar), by its connection's name, or `local` for the daemon's own. Any kind of pane takes one: `:Editor@desk PATH`, `:Explore@desk`, `:Agent@desk codex`. |
| `:Agent`, `:\|Agent`… | Opens a [chat with an agent](~clackatoo/reference/agents), the one named after it (`:Agent codex`) or the `agent` option's, in a new tab or beside the focused pane. |
| `:Board NAME`, `:Boards` | Opens a [board](~clackatoo/reference/boards) shared with agents, making it if there's none; `:Boards` picks one in the finder. |
| `:update`, `:update HOST` | Installs the latest release here, as [`clackatoo update`](~clackatoo/reference/cli#update) does, and says in the bar how it went; with `HOST`, on the remote daemon's machine, in a tab of its own. Once a day the window looks for a new release and says so quietly in the bar; it never installs one by itself. |
| `:Tutor`, `:Tutor NAME`, `:Tutor new` | Opens [the tutor](~clackatoo/reference/frame#learning-clackatoo): its lessons in the finder, the lesson called NAME, or what's new. |
| `:Editor PATH`, `:\|Editor PATH`…, `:e PATH`, `:e` | [Edits the file](~clackatoo/reference/editor), in a new tab or beside the focused pane; `:e` in place of the file an editor shows, and alone, an explorer beside an empty file. |
| `:Files`, `:Grep PATTERN`, `:Buffers`, `:Diagnostics`, `:Symbols`, `:ProjectSymbols NAME` | Opens [the finder](#the-finder), with what follows typed. |
| `:Registers`, `:reg` | Lists what an editor's [registers](~clackatoo/reference/registers-macros-marks#registers) hold, by its cursor. |
| `:nohlsearch`, `:noh` | Stops lighting up what the last search matched in the focused pane, an editor's, a terminal's, a chat's or a list's, until the next search, `n` or `N`. <kbd>Esc</kbd> in normal mode does too. |
| `:Messages`, `:mes` | Shows what the bottom bar has said lately, above it, until the next key. |
| `:Marks` | Opens [the finder](#the-finder) on the [marks](~clackatoo/reference/registers-macros-marks#marks) the focused file can go to. |
| `:1`, `:2`… | In an editor, goes to that line, as Vim does, as do `:$`, `:+3` and `:'a`. Elsewhere, moves to that tab of the workspace shown. |
| `:s/old/new/`, `:%s/old/new/g`… | Replaces text in the focused editor, with [Vim's ranges and flags](~clackatoo/reference/substituting#substituting). |
| `:g/pattern/d`, `:v/…`, `:d`, `:y`, `:m`, `:t`, `:normal` | Vim's [commands for lines](~clackatoo/reference/substituting#lines), in the focused editor. |
| `:workspace NAME`, `:ws NAME` | Switches to the workspace called NAME, making it if there's none. Without a name, makes a new workspace, named with the lowest number no workspace has. `:ws new@desk NAME` makes one on desk, as does `:ws NAME@desk` if desk has none of that name. |
| `:rename NAME` | Renames the workspace shown. |
| `:bg`, `:bgpane`, `:Background` | Sends the tab shown, or the focused pane, to the [background](#background-tabs), or lists the background tabs. |
| `:close` | Closes the focused pane, hanging up its program. A [subagent's](~clackatoo/reference/chats-together#subagents) pane only closes: the chat goes on in the background, a card in the chat that started it. |
| `:reopen` | [Opens again](#opening-again-what-was-closed) the pane or tab closed last. |
| `:zoom` | [Zooms the focused pane](#zooming-a-pane) to fill its tab, or puts the splits back. |
| `:hints` | Labels what's worth picking in the focused pane, as [hints](#hints) say. |
| `:q` | The same, unless the pane is the last showing a file with changes that aren't written. `:q!` closes it anyway, throwing them away. |
| `:bd`, `:bdelete` | Closes the focused chat for good, pane and all: a subagent's too, which `:q` and `:close` only send back to the background. |
| `:w`, `:wq`, `:e!`… | Write the focused pane's file, and the rest of Vim's commands for files: see [Editing files](~clackatoo/reference/editor-files#writing). |
| `:reload` | Runs the [config](~clackatoo/reference/config) again, from the start. |
| `:set nonu`, `:set rnu!`… | Changes [options](~clackatoo/reference/config#options), or shows them, as Vim's `:set` does. |

The first character of a split says which way the divider runs, `|` upright or
`-` flat, and `<` or `^` after it puts the new pane first. A mark alone opens
a terminal, so `:|` is `:|Terminal`. Capitalised words name kinds of pane:
`Terminal`, `Agent`, `Editor`, which takes the file's path after it, and
`Explore` (or `Ex`), which takes a folder, and `Review`; `:|Explore` and
`:-Review` open a new explorer or review beside the focused pane.

Canonical names use capitals for panes, pickers and overlays (`Terminal`,
`Files`, `Registers`, `Marks`, `Messages`), and lowercase for actions
(`write`, `format`, `agentundo`, `reviewkeep`). Command names and their
aliases ignore case: `:Terminal`, `:terminal`, and `:TERMINAL` do the same
thing. Autocomplete always shows the canonical spelling and matches regardless
of case. Existing spellings still work. Arguments keep their case, including
filenames, hosts, patterns, option values and Normal-mode keys. Mixed-purpose
commands such as `set` and `connect` are named for their primary action;
`edit` operates in the current editor, while `Editor` opens a pane.

A number goes to that line in an editor, or else that tab. A command with a
range of lines before it, or one of Vim's for lines, goes to the focused
editor; from visual mode, `:` starts with `'<,'>`.

A new pane goes on the machine of the pane it's made from: the one split, or
the focused one for a new tab or explorer, so `:Terminal`, `:e`, `:Explore`
and the finder in a pane on desk are desk's. `@host` after a kind of pane
picks the machine instead (see
[a pane on another machine](~clackatoo/reference/connecting#a-pane-on-another-machine)).

A new terminal starts in the directory that the pane it splits is in. A new
tab's terminal starts in the directory of the workspace's focused pane. That's
the directory of whatever is in front in that pane, such as a nested shell or
an editor that changed directory, or else the shell's own.

Workspace names are unique, and can't contain `@`, which names workspaces on
other machines. If a command fails, such as renaming a workspace to a name
another has, the bottom bar says why until the next key, in red, and if that's
too long for the bar, whole above it for a few seconds. `:Messages` shows what
the bar has said lately. A command, an option or a lesson that isn't one says
only so, as `no command ‘nonsens’`, and offers the nearest name when a letter
or two is off: `:Fils` asks `did you mean ‘:Files’?`, and `:set nonumbr` asks
about `‘:set nonumber’`. It never offers `:q` or `:wq` for a slip. Suggestions
list the matching names as you type.

### Resizing panes

Drag the gap between two panes to move it. It moves a whole column or row at a
time, and the programs on either side are told their new size as it goes. A
double click on the gap shares that split's space equally again.

In normal mode, <kbd>Alt</kbd>+<kbd>H</kbd>, <kbd>Alt</kbd>+<kbd>J</kbd>,
<kbd>Alt</kbd>+<kbd>K</kbd> and <kbd>Alt</kbd>+<kbd>L</kbd> move the focused
pane's edge on that side by a column or row, so <kbd>Alt</kbd>+<kbd>L</kbd>
widens a pane with a pane to its right. A pane with no edge on that side moves
its other edge instead, so <kbd>Alt</kbd>+<kbd>L</kbd> narrows the rightmost
pane. No pane gets smaller than one cell.

A new pane in a split gets the split's average share, so a split you never
resized stays equal. Sizes are kept by the daemon, like the rest of the
layout, and other windows show the change.

### Moving around

In normal mode, <kbd>Ctrl</kbd>+<kbd>H</kbd>, <kbd>Ctrl</kbd>+<kbd>J</kbd>,
<kbd>Ctrl</kbd>+<kbd>K</kbd> and <kbd>Ctrl</kbd>+<kbd>L</kbd> move to the pane
to the left, below, above or right. Tabs are `:1`, `:2` and so on, and
workspaces `:ws NAME`. [<kbd>Ctrl</kbd>+<kbd>B</kbd>](#ctrl-b) does all of
this in either mode.

Wherever you go, you arrive in the window's mode. That holds when you move,
use the mouse, switch tabs or workspaces, jump, or open a pane, and when the
pane you were in closes because you closed it, or its program ends right after
you typed into it, as after `exit` or <kbd>Ctrl</kbd>+<kbd>D</kbd>. Arriving
in insert mode, a terminal goes back to its live screen and a chat to its
message box. An editor types at its cursor once its file's text has come;
until then, what you type waits for it, and any other key, such as
<kbd>Esc</kbd> or <kbd>Ctrl</kbd>+<kbd>B</kbd>, is had at once in normal mode.
A chat that's asking something is arrived at in normal mode, so a key can't
answer it by chance. If anything else moves the focus, such as another window
or an agent closing the pane or opening a file in it, you arrive in normal
mode and the window goes to normal mode, so nothing you type lands where you
didn't choose. Leaving a pane ends a selection, a half-typed command, a search
or the command line. The pane keeps its place in history, or its cursor.

The window remembers where you've been, as Vim's jump list does: the panes you
moved between, and the places you jumped from in files, folders, terminals,
chats and lists. Long moves such as `gg` and `G`, and accepted searches
including `n` and `N`, are jumps. Search previews do not add places.

| Keys | In | Does |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>O</kbd> <kbd>Ctrl</kbd>+<kbd>I</kbd> | normal mode | Back and forward through the places in the tab shown. A count goes that many places: `3` <kbd>Ctrl</kbd>+<kbd>O</kbd> goes back three, or nowhere if there aren't three. |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>I</kbd> | either mode | Back and forward through everywhere: panes, tabs, and workspaces. |

Going back first remembers where you are, so going forward comes back to it.
Going back and then jumping somewhere new forgets the way forward. Panes that
have closed are skipped. Each window has its own list.

### Hints

<kbd>Ctrl</kbd>+<kbd>B</kbd> `f`, or `:hints`, labels everything worth picking
in the focused pane with a letter or two, as kitty's hints and tmux-fingers
do: URLs, file paths with their `:line:col`, mail addresses, IP addresses, and
git hashes (7 to 64 hex digits, holding a digit and a letter). It reads what
the pane draws, so it works the same in a terminal, an editor, an explorer, a
board, a review or a chat. A link wrapped over two rows of a terminal is one
hint. Those nearest the bottom get the home row's letters first.

| Type | Does |
|---|---|
| A label | Opens it, as <kbd>Ctrl</kbd>+click does: a URL with your browser, asking first for one that isn't a plain web link or a mail address; a path in an editor at its line, from the pane's folder; a mail address in your mail program. What can't be opened, such as a hash, is copied. |
| A label, <kbd>Shift</kbd> on its last letter | Copies it, as <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies. |
| A label, <kbd>Alt</kbd> on its last letter | Types it into the pane, as a paste: at the prompt, at an editor's cursor, or in a chat's box. |
| <kbd>Backspace</kbd> | Takes back the last letter typed. |
| <kbd>Esc</kbd>, or any other key | Puts the labels away. |

The [`hintpattern`](~clackatoo/reference/config#options) option adds a
regular expression of your own, as Rust's `regex` crate reads it:
`client.opt.hintpattern = [[CLACKA-\d+]]` labels issue numbers too, and
several join with `|`. One that can't be read is said in the bar, and left
out. A Markdown file shown rendered has nothing labelled yet.

### Macros and marks across panes

In normal mode, `q` and a register start recording, `q` stops, and `@a`, `3@a`
and `@@` replay, in editors, terminals, chats, explorers, boards and reviews.
The keys can move between panes and continue there. Explorer, board, and
review panes close with `Q` so `q` is available for recording. See
[macros](~clackatoo/reference/registers-macros-marks#macros) for registers,
waiting, and stopping on a failed command.

`ma` sets a mark in the current file or pane; `mA` sets a window-wide mark.
Backtick and the letter return to its position, and apostrophe and the letter
to its line. Capital marks can take you between pane kinds. `''` returns to
the place before the last jump in this file or pane, and again comes back.

Saved terminal positions follow line numbers while history grows. A new
snapshot or history renumbering invalidates them. Chat positions use absolute
turns, so loading older messages preserves them; replacing a branch
invalidates them. Folded chat content opens when returning to its mark. Marks
on live activity cards are not kept until their content is part of a turn.
Boards and reviews restore entries by identity, and an explorer remembers its
path. Known missing items and closed panes are skipped in jump history; a
missing named mark reports that its position is unavailable. Marks and jump
history are local to this window and are not kept on restart.

### <kbd>Ctrl</kbd>+<kbd>B</kbd>

<kbd>Ctrl</kbd>+<kbd>B</kbd> and one more key work in either mode, as tmux's
prefix does. While <kbd>Ctrl</kbd>+<kbd>B</kbd> waits, the bottom bar marks it
`^B` by the mode, and a panel above the bar lists every key it takes, with
what each does; a key that starts others says `+` and what follows. With two
panes or more, each shows its number. Other keys that start others, as
<kbd>Space</kbd> does in normal mode, are marked at once and list theirs after
`whichkeydelay`, so keys typed quickly never flash the panel. The panel is
made from the bindings: a key your config binds says what its `desc` says, or
"from your config" without one, and a key that starts others says `+` and its
group's name, as <kbd>Space</kbd> `p` says `+project`, or else what they
share, or how many keys can follow. <kbd>Esc</kbd>, or any other key, cancels.
These keys, and the others on this page that the window takes rather than the
program, are bindings like any you make, so
[the config](~clackatoo/reference/config) can change them or take them away.

| <kbd>Ctrl</kbd>+<kbd>B</kbd>, then | Does |
|---|---|
| `1`–`9` | Moves to that tab. |
| `c` | Opens a new tab. |
| `]` `[` | Moves to the next tab, or the one before, round from the last to the first. |
| `,` | Renames the tab. |
| `p`, then `1`–`9` | Moves to that pane. Each pane shows its number, counting left to right and top to bottom. |
| `h` `j` `k` `l` | Moves to the pane to the left, below, above or right. |
| `\|` `-` | Splits the pane, with the new one to the right or below. |
| `a` | Opens a [chat with an agent](~clackatoo/reference/agents) to the right. |
| `x` | Closes the pane; a subagent's goes back to the background. <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>T</kbd> [opens it again](#opening-again-what-was-closed). |
| `z` | [Zooms the pane](#zooming-a-pane) to fill its tab, or puts the splits back. |
| `f` | Labels what's worth picking in the pane, as [hints](#hints) say. |
| `u` | From a [subagent's](~clackatoo/reference/chats-together#subagents) pane, goes to the chat that started it. |
| `w` | Opens the workspace picker. |
| `r` | Opens the [machine picker](~clackatoo/reference/connecting#picking-a-machine), to open a window on another machine, as `:connect` does. |
| `o` `i` | Back and forward through the tab's places, as <kbd>Ctrl</kbd>+<kbd>O</kbd> and <kbd>Ctrl</kbd>+<kbd>I</kbd> do in normal mode. |
| `b`, then `t` `p` `l` | Sends the tab or the pane to the [background](#background-tabs), or lists the background tabs. |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> | Sends <kbd>Ctrl</kbd>+<kbd>B</kbd> to the program. |

The workspace picker lists the workspaces whose names hold what you type,
those starting with it first. <kbd>Up</kbd> and <kbd>Down</kbd>, or
<kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd>, pick one, and
<kbd>Enter</kbd> moves to it. If nothing matches, <kbd>Enter</kbd> makes a
workspace by the name typed.

### The finder

The finder picks a file from the project the focused pane is in: the files git
doesn't ignore, leaving out hidden ones; or with `:Grep`, the lines in them a
pattern matches; or the files open. It looks from the project's root: the one
the file's language server was started for, or else its repository's, or for a
terminal, the repository its program is in. Outside your home directory, only
a project is searched, never a bare directory.

| Keys, in normal mode | Opens |
|---|---|
| `<Space>pf` | The project's files |
| `<Space>bs` | The symbols in the focused editor's file: its functions, types and their fields, in the order of the file's outline, each indented inside what it's in, with what it is; past 5000, the count ends `+` |
| `<Space>ps` | The shared index's project symbols, matched by identifier and path word prefixes as typing pauses, with previews and partial-index status |
| `<Space>bo`, `<Space>bO` | Toggle the editor's outline panel, or focus it without closing it |
| `<Space>e` | Opens the tab's [file explorer](~clackatoo/reference/explorer), goes to it, or closes it |
| `<Space>a` | Mentions the focused editor's selection, or its file, in the chat beside it, as `:Ask` does (see [Mentioning files](~clackatoo/reference/chatting#mentioning-files)) |
| `<Space>pgf` | A grep of them, as you type |
| `<Space>?` | The files open, `[+]` after those with changes |
| `<Space>ds` | What's wrong in the project's files, as their language servers say, the worst first: the files open, and those not open that a server has checked, as rust-analyzer checks a whole crate, each `error`, `warning`, `info` or `hint` in the editor's color for it |

What you type matches as fzf's does: letters in order, words each on their
own, `'word` exactly, `^word` at the start, `word$` at the end, `!word` leaves
out. A grep's pattern is a regular expression, or plain text if it isn't one.
The letters it matched are picked out, in every list.

The [project symbol finder](~clackatoo/reference/code-outlines#project-symbol-finder)
uses the shared code index and installed grammars. It does not need a language
server, and shows partial results while indexing. An older daemon keeps its
language-server symbol search. `<Space>bs` remains the language-server file
picker. The [outline panel](~clackatoo/reference/code-outlines#editor-outline)
uses the shared index and follows the editor's file and cursor.

When nothing matches what you type, the list says so once it has finished
looking.

| Key | Does |
|---|---|
| <kbd>Down</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd>; <kbd>Up</kbd>, <kbd>Ctrl</kbd>+<kbd>P</kbd> | The next or previous one |
| <kbd>Page Down</kbd>, <kbd>Page Up</kbd> | A page further or back |
| <kbd>Home</kbd>, <kbd>End</kbd> | The first or the last |
| <kbd>Enter</kbd> | Opens it in place of the focused pane's file, as `:e` does |
| <kbd>Ctrl</kbd>+<kbd>V</kbd>, <kbd>Ctrl</kbd>+<kbd>X</kbd> | Opens it beside the focused pane, or below |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | Opens it in a new tab |
| <kbd>Ctrl</kbd>+<kbd>U</kbd>, <kbd>Ctrl</kbd>+<kbd>W</kbd> | Deletes what's typed, or its last word: its last letters and digits, and anything after them, as in a list's prompts |
| <kbd>Esc</kbd> | Closes the finder |

A click picks one, a double click opens it, and the wheel moves through them.
A paste is typed on one line, its line breaks as spaces. The
[machine picker](~clackatoo/reference/connecting#picking-a-machine) and the
[start page](~clackatoo/reference/chatting#the-start-page) take the same keys
for what's typed, for moving, and for the mouse. Beside the list, if the
window is wide enough, is the file round the line picked, colored as an editor
colors it, with a grep's match picked out in its line. A file that's open is
colored as its buffer is; one that isn't is colored by the language its name
ends in. A name with no ending such as `Makefile`, a language that isn't
installed, or a daemon older than protocol 84 leaves it plain, and a preview
from the middle of a file can be colored wrongly inside a long comment. An
empty file says so. Opening one is a
[jump](~clackatoo/reference/editor-keys#jumps), so
<kbd>Ctrl</kbd>+<kbd>O</kbd> comes back. <kbd>Space</kbd> starts these keys in
normal mode, so <kbd>Space</kbd> alone doesn't move the cursor, unless a
command is half typed, as in `d<Space>`.


## Daemon background work

The bottom bar's **Working · N** indicator opens the daemon's live work list.
`:Work` opens it too. See [Background work](~clackatoo/reference/background-work)
for progress, cancellation and recent outcomes.
