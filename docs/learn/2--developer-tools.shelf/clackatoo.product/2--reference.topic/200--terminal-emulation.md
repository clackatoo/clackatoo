---
title: Terminal Emulation
type: reference
description: The terminal clackatoo emulates. What programs find in their environment, which escape sequences and modes it supports, which queries it answers, and what it ignores so far.
related:
  - clackatoo/reference/cli
  - clackatoo/getting-started/what-is-clackatoo
---

clackatoo emulates enough of xterm for shells, ncurses programs such as htop,
and vim, under `TERM=xterm-256color`, with the extensions modern terminals
such as kitty, foot, WezTerm and Ghostty share: the kitty keyboard protocol,
synchronized output, styled and colored underlines, a program's progress
shown on its tab, and the notifications programs ask for. Sequences it doesn't support are ignored, as xterm ignores
ones it doesn't know.

## The environment

A program started in a clackatoo terminal gets the daemon's environment, with
these changes:

| Variable | Value |
|---|---|
| `TERM` | `xterm-256color` |
| `COLORTERM` | `truecolor` |
| `TERM_PROGRAM` | `clackatoo` |
| `TERM_PROGRAM_VERSION` | The clackatoo version. |

Variables describing whatever terminal the daemon was started in are removed,
so programs don't think they're inside it. These include `TMUX`, `TMUX_PANE`,
`STY`, `VTE_VERSION`, `KITTY_WINDOW_ID`, `WEZTERM_PANE`,
`ALACRITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `WT_SESSION`, `COLUMNS`, and
`LINES`.

The program runs in its own session, with the terminal as its controlling
terminal, so job control and Ctrl-C work as usual.

### The display

The variables that say which desktop a program opens its windows on follow the
desktop's logon, not the daemon's environment. A daemon started over ssh,
before anyone signed in to the machine, has no `WAYLAND_DISPLAY`, but a
terminal opened after you sign in there still gets one. The variables are
`WAYLAND_DISPLAY`, `DISPLAY`, `XAUTHORITY`, `XDG_SESSION_TYPE`,
`XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`, `DESKTOP_SESSION`, `SWAYSOCK`,
`I3SOCK`, `HYPRLAND_INSTANCE_SIGNATURE`, and `NIRI_SOCKET`. They come from the
first of these that has a display whose socket is still there
(`WAYLAND_DISPLAY`'s in `$XDG_RUNTIME_DIR`, or where it names, and
`/tmp/.X11-unix/XN` for a `DISPLAY` of `:N`):

1. **The systemd user manager's environment**, as
   `systemctl --user show-environment` prints it. GNOME, KDE, and sway or
   Hyprland with their usual config put their display there as you sign in;
   GNOME takes it out as you sign out, while sway and Hyprland leave it, which
   is why its socket must still be there. All of the variables are then
   systemd's, and one systemd hasn't got is removed, but for
   `XDG_SESSION_TYPE`. With neither of its displays' sockets there, none of
   systemd's variables are taken.
2. **The daemon's own environment.** A desktop that tells systemd nothing, as
   sway started by hand, keeps working this way for a daemon started inside
   it, while it lasts.
3. **None**: all of the variables are removed, but `XDG_SESSION_TYPE`.

A display whose socket has gone is removed. A display on another host, as
ssh's X forwarding gives (`localhost:10.0`), can't be checked, so one the
daemon was started with is kept for as long as it runs: only a desktop on this
machine that has gone is dropped.

`XDG_SESSION_TYPE` says what kind of session a program runs in, and toolkits
read it to choose between Wayland and X: Qt, Chromium, and Electron choose
Wayland only when it's `wayland`. A daemon started over ssh has `tty`, which
beside a `WAYLAND_DISPLAY` would send them to X. GNOME tells systemd `wayland`
as you sign in, so it comes with the display. Beside a display, a session type
that's missing or `tty`, as sway and Hyprland leave it, becomes `wayland`, or
`x11` with only an X display. With no display, the daemon's own `tty` is still
true, so it stays.

The daemon reads systemd's environment as it starts, waiting up to 300 ms for
it so the terminals it makes again after a restart have the display. It reads
it again whenever a Wayland socket (`wayland-N` in `$XDG_RUNTIME_DIR`) or an X
socket (in `/tmp/.X11-unix`) appears or goes, and 0.5, 2 and 5 seconds after
that, as a desktop tells systemd a moment after making its socket; and as each
terminal is made, as systemd's can change with no socket coming or going,
which the terminal's shell takes at its first prompt. Nothing waits for these
reads, and only one runs at a time. It never checks on a timer. Where it can't
watch for sockets, it says so in its log, and tries again a while later,
waiting twice as long each time up to a minute, as when `$XDG_RUNTIME_DIR` is
removed as your last session ends and made again as you sign in. Each change
is logged, as
`display: from systemd's user manager: WAYLAND_DISPLAY=wayland-0 …`.

Every program the daemon starts from then on gets the display as it is: a
terminal's program, an agent, a language server. The daemon never changes its
own environment. A program already running keeps the environment it started
with, as on any Unix, so an agent or language server started before you signed
in has no display until it's started again.

**Shells already open** follow before the next command they run, or bash at
its next prompt. bash, zsh and fish, started as your shell with clackatoo's
integration, read `shell/display` in the daemon's
[runtime directory](~clackatoo/reference/cli#daemon) at each prompt, and fish
and zsh again right before each command you type runs, and set or unset the
variables to match. So in fish or zsh, a command typed at a prompt drawn hours
before you signed in still gets the display. The daemon writes the file whole,
readable by you alone, whenever the display changes. A shell changes a
variable only while it has the value the shell started with, or the value the
shell last gave it, so one you set yourself, in your startup files or at the
prompt, stays yours. The file is read with the shell's own builtins, starting
nothing, and its few lines are only looked at when they've changed: about 20
to 30 µs a prompt or command.

fish reads it in its `fish_preexec` event, and zsh before your hooks in
`preexec_functions` (a plain `preexec` function of yours runs before those).
bash follows at its next prompt only, so in bash the command typed at a prompt
drawn before you signed in runs with the old display, and the one after it has
the new. What bash shows before running a command, `PS0`, runs its `$(…)` in a
subshell, which can't change the shell's variables. What could run in the
shell itself would get in the way of a `DEBUG` trap of your own: bash 5.3's
`${| …; }` in `PS0` sets it off on clackatoo's call before your command, and a
`DEBUG` trap of clackatoo's would have to share it. Either breaks tools built
on it, as [bash-preexec](https://github.com/rcaloras/bash-preexec) is.

A program you started from a shell is no different from one the daemon
started: once running, it keeps the environment it started with. A shell
following the display changes what the commands it runs from then on get,
never what's already running, so an agent you started before you signed in has
no display until you start it again.

[`clackatoo display`](~clackatoo/reference/cli#display) shows the display
programs get now, and where it came from. `CLACKATOO_DISPLAY=off` in the
daemon's environment turns all of this off: programs get the daemon's own
environment, and shells change nothing. Each machine's daemon follows its own
desktop; nothing about it passes between machines.

## Screen behavior

- **Scrollback**: the primary screen keeps the last 10,000 lines that scroll
  off its top. Lines only enter scrollback when the whole screen scrolls.
  Scrolling inside a region an app has narrowed (as vim does) discards them.
  Scrollback is stored as compact text, typically under 100 bytes a line, and
  remembers which lines were wrapped, so they can be rewrapped to another
  width. Compacting a line takes about as long as printing it did, so by
  default the daemon waits until a terminal's output has paused for 100 ms.
  Lines that scroll away during a flood (`cat` of a huge file) are never
  compacted at all. Until then, history keeps lines as they were on screen,
  about 8 bytes per column, capped at 16 MiB per terminal. The daemon's
  `--scrollback-compaction` option changes this: `eager` compacts each line as
  it scrolls off, which is the smallest but the slowest, and `off` never
  compacts, which is the fastest but the largest.
- **Floods**: a terminal printing without end never holds up another you're
  typing in. The daemon reads it last in each turn, after updates have gone
  out, and stops as soon as anything else is ready. Output a program prints at
  once, such as a full-screen redraw, is still shown whole, and a program's
  last output always reaches its pane before the pane closes.
- **Resizing**: lines on the primary screen are rewrapped to the new width,
  and the cursor stays on the same character. A line that started in
  scrollback is rewrapped whole, as if it had been printed at the new width,
  so its part on screen still lines up with its part above. Only the last few
  screens' worth of a very long line is rewrapped, and older scrollback is
  left as it is. When the screen gets shorter, blank lines below the cursor go
  first, then lines above it move into scrollback. Erasing the top line or
  scrolling it away ends a line continuing from scrollback, so a resize never
  joins what's printed next onto it. The alternate screen is cut off or
  extended instead, because the full-screen programs that use it redraw after
  a resize.
- **Alternate screen**: full-screen programs switch to a second screen with no
  scrollback. Leaving it restores the primary screen and cursor exactly.
- **Wrapping**: printing in the last column leaves the cursor there, and the
  line wraps only when the next character arrives.
- **Background color erase**: clearing part of the screen fills it with the
  current background color.
- **Wide characters** take two columns. A wide character that doesn't fit at
  the end of a line wraps to the next one.
- **Combining characters** (accents, variation selectors) join the character
  before them. So do characters after a zero-width joiner, so emoji sequences
  such as 👩&#x200D;💻 stay one character.

## Supported sequences

| Kind | Supported |
|---|---|
| Control characters | BS, HT, LF, VT, FF, CR, SO, SI. BEL is accepted and ignored. |
| `ESC` | `7` `8` (save/restore cursor), `D` (index), `E` (next line), `H` (set tab stop), `M` (reverse index), `c` (full reset), `#8` (screen alignment test), `(` and `)` with `B` (ASCII) or `0` (DEC line drawing) |
| `CSI` cursor | `A` `B` `C` `D` `E` `F` `G` `H` `I` `Z` `` ` `` `a` `d` `e` `f`, and `s` `u` without parameters |
| `CSI` editing | `@` `J` (including `3J` to clear scrollback) `K` `L` `M` `P` `S` `T` `X` `b` (repeat) |
| `CSI` other | `g` (clear tab stops), `r` (scroll region), `m` (SGR), `!p` (soft reset), ` q` (cursor shape: 0 the user's default, 1 and 2 block, 3 and 4 underline, 5 and 6 bar; blinking isn't drawn), `t` (window operations: `22` and `23` push and pop the title, with a second parameter of 0 or 2, ten titles deep, each kept to 4096 bytes; see [queries](#queries-answered) for `18`), `>u` `<u` `=u` (the [kitty keyboard protocol](#keyboard)) |
| `ESC` keypad | `=` and `>` (application and numeric keypad) |
| ANSI modes (`CSI h`/`l`) | 4 (insert), 20 (line feed is newline) |
| DEC modes (`CSI ? h`/`l`) | 1 (application cursor keys), 6 (origin), 7 (autowrap), 25 (show cursor), 47, 1047 and 1049 (alternate screen), 66 (application keypad), 1048 (save cursor), 2004 (bracketed paste), 9, 1000, 1002 and 1003 (mouse tracking), 1005, 1006 and 1015 (mouse encodings), 1004 (focus events), 2026 ([synchronized output](#synchronized-output)) |
| SGR | Bold, dim, italic, blink, inverse, hidden, strikethrough, and their resets. Underlines: single, double (`21` or `4:2`), curly (`4:3`), dotted (`4:4`), dashed (`4:5`). 8, 16, 256 and 24-bit colors for text, background and underline (`58`), in both the `;` and `:` spellings. |
| `OSC` | 0 and 2 (window title, which names the tab and the window without control characters or those that show nothing but change what's around them, as the marks that turn text round and the line separators do; in the window's title, only its first 256 characters, set apart from the machine's name and anything clackatoo adds), 7 (working directory, as a `file://` URL, which names a tab with no title; a path over 4096 bytes is ignored), 8 (hyperlinks, with `id=`; kept in history and through upgrades), 52 (clipboard: text a program copies goes to the window's clipboard or primary selection, as the window's `osc52` option allows; reading it is refused unless allowed, with an empty answer), 133 (shell integration: `A` prompt, `B` where typing starts, `C` output, with an optional `cmdline_url=` (a command over 1024 bytes isn't kept) and `clackatoo=`, the secret clackatoo's own shell integration adds, which is checked and dropped, and `D` exit status, which marks a failed command's prompt line; see [commands](~clackatoo/reference/normal-mode#commands); the marks also say whether a [background tab](~clackatoo/reference/workspaces#background-tabs) is running, and which signed command a terminal [kept running](~clackatoo/reference/workspaces#keeping-a-terminal-running) runs again), 9;4 ([progress](#progress)), 9, 777 and 99 ([notifications](#notifications)) |

## Queries answered

| Query | Reply |
|---|---|
| `CSI c` (primary device attributes) | `CSI ? 62 ; 22 c`: a VT220 with color |
| `CSI 5 n` (status) | `CSI 0 n` |
| `OSC 52 ; c ; ?` (clipboard) | `OSC 52 ; c ; <base64> ST`, empty if refused or unanswered within 2 s, ended with BEL if the query was |
| `CSI 6 n` and `CSI ? 6 n` (cursor position) | `CSI <line> ; <col> R`, with `?` for the second form. Relative to the scroll region in origin mode. |
| `CSI > q` (terminal version) | `DCS > \| clackatoo <version> ST` |
| `CSI > c` (secondary device attributes) | `CSI > 1 ; <version> ; 0 c`: a VT220, its version clackatoo's year and month, as `202611`, as foot answers with its own. vim reads a number this high as a modern terminal's, not an old xterm's. |
| `CSI ? Ps $ p` and `CSI Ps $ p` (DECRQM, is a mode set?) | `CSI ? Ps ; Pm $ y`, without the `?` for an ANSI mode: `Pm` is 1 for set, 2 for reset, and 0 for a mode clackatoo doesn't know. Every mode in [supported sequences](#supported-sequences) is known, 2026 included. 2027 (grapheme clustering) is answered 0: not every character's width is as Unicode's segmentation would have it. |
| `OSC 10 ; ?`, `OSC 11 ; ?`, `OSC 12 ; ?` (the text's, background's, and cursor's colors) | `OSC 11 ; rgb:1d1d/1f1f/2121 ST`, ended with BEL if the question was. Several at once, as `OSC 10 ; ? ; ?`, are answered in turn. Neovim asks for the background as it starts, to choose its light or dark colors, as vim, bat and delta do. |
| `OSC 4 ; <index> ; ?` (a palette entry, several pairs at once) | `OSC 4 ; <index> ; rgb:… ST` for each |
| `CSI ? u` (kitty keyboard flags) | `CSI ? <flags> u`, the [flags](#keyboard) in effect |
| `CSI 18 t` (text area size) | `CSI 8 ; <lines> ; <columns> t`. The sizes in pixels (`14t`, `16t`) aren't answered: the daemon knows the terminal's size in cells alone. Nor is the title read back (`21t`), which would let a program type what it set. |
| `DCS + q <names> ST` (XTGETTCAP) | For each name, in hex: `DCS 1 + r <name> = <value> ST`, the value in hex, or without `=` for a boolean; `DCS 0 + r <name> ST` for one clackatoo doesn't say, the name said back only if it's hex, and `DCS 0 + r ST` otherwise: no answer ever says back what a program printed, which could have it type anything into the shell. It says `xterm-256color`'s name (`TN`) and colors (`Co`), 24-bit color (`RGB`, `Tc`, `setrgbf`, `setrgbb`), styled and colored underlines (`Su`, `Smulx`, `Setulc`), the clipboard (`Ms`), the cursor's shape (`Ss`, `Se`), synchronized output (`Sync`), bracketed paste (`BE`, `BD`, `PS`, `PE`), focus events (`fe`, `fd`, `kxIN`, `kxOUT`) and `XT`. At most 64 names a request, and a request over 1024 bytes isn't answered. |
| `DCS $ q <setting> ST` (DECRQSS) | `DCS 1 $ r <sequence> ST`, the sequence that sets the setting as it is, for the pen (`m`, as `0;1;38:5:1m`), the cursor's shape (` q`, steady, or 0 for your own) and the scroll region (`r`); `DCS 0 $ r ST` for any other. |

The colors are those the window draws terminals in, which it tells the daemon
as it connects, so the daemon answers at once, as programs wait only a moment.
Every terminal answers with those of the window that told it last, and the
daemon tells those on [other machines](~clackatoo/reference/remote) it's
connected to. Until a window has told it, as with a window older than protocol
139, these questions aren't answered, and programs keep their defaults.

A program that asks faster than it reads the answers, as one at the far end of
an ssh connection that has stopped reading can, gets no more once 64 KiB of
answers and input wait for it. Answers are dropped whole, never cut, and what
you type still goes in. The clipboard's answers go the same way, and so do
answers to a program still reading a long paste: it gets none until it has
read most of it.

## Keyboard

Keys are sent as xterm sends them, which nearly every program understands,
until a program asks for the kitty keyboard protocol
(<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), as fish 4, Neovim,
Helix and Kakoune do. xterm's encoding can't tell some keys apart:
<kbd>Esc</kbd> from the start of a sequence, <kbd>Ctrl</kbd>+<kbd>I</kbd> from
<kbd>Tab</kbd>, <kbd>Shift</kbd>+<kbd>Enter</kbd> from <kbd>Enter</kbd>. The
protocol sends those as sequences of their own.

A program asks a flag at a time:

| Flag | What changes |
|---|---|
| 1 (disambiguate) | <kbd>Esc</kbd>, and keys with <kbd>Ctrl</kbd> or <kbd>Alt</kbd>, are sent as `CSI <key> ; <modifiers> u`. <kbd>Enter</kbd>, <kbd>Tab</kbd> and <kbd>Backspace</kbd> with no modifiers are sent as they are without the flag, so a shell a program left with the flag on can still be typed `reset` into. Text typed with or without <kbd>Shift</kbd> is text. |
| 4 (alternate keys) | Those sequences also say the key <kbd>Shift</kbd> made, and the key in a US layout, as `CSI 97:65;2u`. |
| 8 (all keys) | Every key is a sequence, text and all. |
| 16 (text) | With 8, each sequence also says the text its key types. |

Flag 2, key repeats and releases, isn't kept, so a program asking for it sees
from `CSI ? u` that it isn't there: windows send presses alone. Nor are
<kbd>Super</kbd>, Hyper, Meta, Caps Lock and Num Lock sent as modifiers, or
modifier keys pressed alone. The keypad's keys have numbers of their own, but
type their text without <kbd>Ctrl</kbd> or <kbd>Alt</kbd>.

A program pushes the flags it wants (`CSI > flags u`) and pops them as it ends
(`CSI < count u`), leaving what was there before; `CSI = flags ; mode u` sets
(1), adds (2) or takes away (3) flags in effect. Each screen keeps a stack of
its own, sixteen deep, so a full-screen program leaves the shell's alone, and
a full reset (`ESC c`) ends them all. A program switching to the alternate
screen starts with none, whatever one before it left there, and the shell's
next prompt (OSC 133 `A`) drops what a program left on the primary screen, so
a program that dies with the protocol on doesn't leave
<kbd>Ctrl</kbd>+<kbd>C</kbd> reaching the shell as `CSI 99;5u`: a shell that
wants it, as fish 4 does, asks again after its prompt. They, the titles
pushed, and a program's progress go on through an upgrade in place. The window
encodes keys this way; the web app on your phone sends xterm's encoding still.
A window older than protocol 139 does too, which programs that ask still
understand.

## Synchronized output

A program that draws its screen in pieces can say it's drawing a frame
(`CSI ? 2026 h`) and that it's done (`CSI ? 2026 l`), as Neovim, Helix, tmux
and programs built on crossterm do. Meanwhile, the daemon sends windows
nothing of that terminal, and what changed goes out whole once the frame is
drawn, so you never see half a redraw. A frame that takes longer than 150 ms
is shown as far as it got, and nothing more is held back until the program
starts another, so a program that never says it's done only looks slow.

## Progress

A program can say how far a long task has got with
`OSC 9 ; 4 ; <state> ; <percent> ST`, as ConEmu began it and Windows Terminal,
Ghostty, kitty and WezTerm show it: state 1 is under way at a percent, 2
failed, 3 busy with no telling how far, 4 paused, and 0 done. The tab it's in
ends its name with ` 42%`, ` ✗` (and the percent, if given) failed, ` …` busy,
or ` ‖` paused: the focused pane's, or else the first of the tab's panes to
say, in a tab shown or not. It goes once the program says it's done (state 0),
or once the shell's next prompt shows the program has ended (OSC 133 `A`). Any
other `OSC 9` is iTerm2's [notification](#notifications).

## Notifications

A program can ask for a desktop notification, as a build or a long command
does when it ends, in any of the three ways terminals read:

| Sequence | What it says |
|---|---|
| `OSC 9 ; <text> ST` | iTerm2's: a line, with no title. ConEmu's numbered commands, whose first part is a number, aren't notifications. |
| `OSC 777 ; notify ; <title> ; <body> ST` | urxvt's, as foot and Ghostty read it. |
| `OSC 99 ; <metadata> ; <payload> ST` | kitty's (<https://sw.kovidgoyal.net/kitty/desktop-notifications/>). The metadata is `key=value` pairs separated by `:`: `p=title` (as when it's left out) or `p=body` says which the payload is, `d=0` that more of it comes in the next, `e=1` that it's base64, `u=0`, `1` or `2` how urgent it is, and `i=<id>` names it, so that `p=close` with the same id takes it back. Buttons, icons, sounds and the rest are left out. |

A window shows it while you aren't looking at the program's pane: it's in a
tab not shown, or the window doesn't have the focus. It goes once you look, as
the program asks, or as the program ends, and clicking it goes to the pane. It
isn't sent at all while someone whose window has the focus, or whose phone
shows the terminal, is looking at it. A low urgency (`u=0`) is shown as such;
a critical one (`u=2`) as a normal one, since critical ones break through Do
Not Disturb. `:set notify=off` turns them off with chats', as the
[`notify` option](~clackatoo/reference/config#options) says.

`:notify phone` marks the focused terminal to have its notifications reach the
[phone](~clackatoo/reference/phone#notifications) too, and `:notify nophone`
takes the mark away. A tab with a terminal so marked ends its name with
` 📱`. The mark goes on through an upgrade in place and a restart, as `:keep`
does.

What a program says is cleaned as tab names are: control characters become
spaces, what shows nothing but changes what's around it goes, runs of spaces
become one, and a title is cut to 120 characters, a line to 300. A terminal
shows three at once, then one every ten seconds; one asking more often is
heard only as often. Nothing is ever said back to the program: kitty's
questions about what's supported (`p=?`), and its reports of a notification
clicked or closed, get no answer.

### Long commands

A command that ran for 10 seconds or more says so as it ends, in the same way
and to the same places as a program's notifications, while no one is looking
at its terminal: the command, how it ended, and how long it took, as
`cargo build` and `failed (exit 101) after 2m14s`. The command is named only
when clackatoo's own shell integration said it, signed; otherwise it's
`A command`. Its end is believed only once the shell has the terminal to
itself again, as with an agent's wait for a command, so a program printing
the marks of an end (OSC 133 `D`) while it runs on says nothing. Terminals an
agent started, and those showing what an agent's command printed, never say.
`notify_after` in `daemon/machine.toml`'s
[`[terminals]`](~clackatoo/reference/config#the-daemon-s-terminals) changes
the 10 seconds, or turns it off.

## Not supported

These are ignored:

- The kitty graphics protocol (`APC G`), sixels and other images.
- Setting colors: `OSC 4`, `10`, `11`, `12`, `104`, and the others. Asking
  about them is [answered](#queries-answered).
- Key repeats and releases (the kitty keyboard protocol's flag 2), and
  <kbd>Super</kbd>, Hyper and Meta as modifiers.
- Grapheme clustering (mode 2027), in-band resize reports (mode 2048) and
  mouse reports in pixels (mode 1016).
- Left and right margins (`CSI s` with parameters), and overlines (`SGR 53`).
- Variation selector 16 does not widen the character before it.
