---
title: Terminal emulation
type: reference
description: The terminal clackatoo emulates. What programs find in their environment, which escape sequences and modes it supports, which queries it answers, and what it ignores so far.
related:
  - clackatoo/reference/cli
  - clackatoo/getting-started/what-is-clackatoo
---

clackatoo emulates enough of xterm for shells, ncurses programs such as htop, and vim, under `TERM=xterm-256color`. Sequences it doesn't support are ignored, as xterm ignores ones it doesn't know.

## The environment

A program started in a clackatoo terminal gets the daemon's environment, with these changes:

| Variable | Value |
|---|---|
| `TERM` | `xterm-256color` |
| `COLORTERM` | `truecolor` |
| `TERM_PROGRAM` | `clackatoo` |
| `TERM_PROGRAM_VERSION` | The clackatoo version. |

Variables describing whatever terminal the daemon was started in are removed, so programs don't think they're inside it. These include `TMUX`, `TMUX_PANE`, `STY`, `VTE_VERSION`, `KITTY_WINDOW_ID`, `WEZTERM_PANE`, `ALACRITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `WT_SESSION`, `COLUMNS` and `LINES`.

The program runs in its own session, with the terminal as its controlling terminal, so job control and Ctrl-C work as usual.

### The display

The variables that say which desktop a program opens its windows on follow the desktop's login, not the daemon's environment. A daemon started over ssh, before anyone logged in to the machine, has no `WAYLAND_DISPLAY`, but a terminal opened after you log in there still gets one. The variables are `WAYLAND_DISPLAY`, `DISPLAY`, `XAUTHORITY`, `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`, `DESKTOP_SESSION`, `SWAYSOCK`, `I3SOCK`, `HYPRLAND_INSTANCE_SIGNATURE` and `NIRI_SOCKET`. They come from the first of these that has a display whose socket is still there (`WAYLAND_DISPLAY`'s in `$XDG_RUNTIME_DIR`, or where it names, and `/tmp/.X11-unix/XN` for a `DISPLAY` of `:N`):

1. **The systemd user manager's environment**, as `systemctl --user show-environment` prints it. GNOME, KDE, and sway or Hyprland with their usual config put their display there as you log in; GNOME takes it out as you log out, while sway and Hyprland leave it, which is why its socket must still be there. All of the variables are then systemd's, and one systemd hasn't got is removed, but for `XDG_SESSION_TYPE`. With neither of its displays' sockets there, none of systemd's variables are taken.
2. **The daemon's own environment.** A desktop that tells systemd nothing, as sway started by hand, keeps working this way for a daemon started inside it, while it lasts.
3. **None**: all of the variables are removed, but `XDG_SESSION_TYPE`.

A display whose socket has gone is removed. A display on another host, as ssh's X forwarding gives (`localhost:10.0`), can't be checked, so one the daemon was started with is kept for as long as it runs: only a desktop on this machine that has gone is dropped.

`XDG_SESSION_TYPE` says what kind of session a program runs in, and toolkits read it to choose between Wayland and X: Qt, Chromium and Electron choose Wayland only when it's `wayland`. A daemon started over ssh has `tty`, which beside a `WAYLAND_DISPLAY` would send them to X. GNOME tells systemd `wayland` as you log in, so it comes with the display. Beside a display, a session type that's missing or `tty`, as sway and Hyprland leave it, becomes `wayland`, or `x11` with only an X display. With no display, the daemon's own `tty` is still true, so it stays.

The daemon reads systemd's environment as it starts, waiting up to 300 ms for it so the terminals it makes again after a restart have the display. It reads it again whenever a Wayland socket (`wayland-N` in `$XDG_RUNTIME_DIR`) or an X socket (in `/tmp/.X11-unix`) appears or goes, and 0.5, 2 and 5 seconds after that, as a desktop tells systemd a moment after making its socket; and as each terminal is made, as systemd's can change with no socket coming or going, which the terminal's shell takes at its first prompt. Nothing waits for these reads, and only one runs at a time. It never checks on a timer. Where it can't watch for sockets, it says so in its log. Each change is logged, as `display: from systemd's user manager: WAYLAND_DISPLAY=wayland-0 …`.

Every program the daemon starts from then on gets the display as it is: a terminal's program, an agent, a language server. The daemon never changes its own environment. A program already running keeps the environment it started with, as on any Unix, so an agent or language server started before you logged in has no display until it's started again.

**Shells already open** follow before the next command they run, or bash at its next prompt. bash, zsh and fish, started as your shell with clackatoo's integration, read `shell/display` in the daemon's [runtime directory](~clackatoo/reference/cli#daemon) at each prompt, and fish and zsh again just before each command you type runs, and set or unset the variables to match. So in fish or zsh, a command typed at a prompt drawn hours before you logged in still gets the display. The daemon writes the file whole, readable by you alone, whenever the display changes. A shell changes a variable only while it has the value the shell started with, or the value the shell last gave it, so one you set yourself, in your startup files or at the prompt, stays yours. The file is read with the shell's own builtins, starting nothing, and its few lines are only looked at when they've changed: about 20 to 30 µs a prompt or command.

fish reads it in its `fish_preexec` event, and zsh before your hooks in `preexec_functions` (a plain `preexec` function of yours runs before those). bash follows at its next prompt only, so in bash the command typed at a prompt drawn before you logged in runs with the old display, and the one after it has the new. What bash shows before running a command, `PS0`, runs its `$(…)` in a subshell, which can't change the shell's variables. What could run in the shell itself would get in the way of a `DEBUG` trap of your own: bash 5.3's `${| …; }` in `PS0` sets it off on clackatoo's call before your command, and a `DEBUG` trap of clackatoo's would have to share it. Either breaks tools built on it, as [bash-preexec](https://github.com/rcaloras/bash-preexec) is.

A program you started from a shell is no different from one the daemon started: once running, it keeps the environment it started with. A shell following the display changes what the commands it runs from then on get, never what's already running, so an agent you started before you logged in has no display until you start it again.

[`clackatoo display`](~clackatoo/reference/cli#display) shows the display programs get now, and where it came from. `CLACKATOO_DISPLAY=off` in the daemon's environment turns all of this off: programs get the daemon's own environment, and shells change nothing. Each machine's daemon follows its own desktop; nothing about it passes between machines.

## Screen behaviour

- **Scrollback**: the primary screen keeps the last 10,000 lines that scroll off its top. Lines only enter scrollback when the whole screen scrolls. Scrolling inside a region an app has narrowed (as vim does) discards them. Scrollback is stored as compact text, typically under 100 bytes a line, and remembers which lines were wrapped, so they can be rewrapped to another width. Compacting a line takes about as long as printing it did, so by default the daemon waits until a terminal's output has paused for 100 ms. Lines that scroll away during a flood (`cat` of a huge file) are never compacted at all. Until then, history keeps lines as they were on screen, about 8 bytes per column, capped at 16 MiB per terminal. The daemon's `--scrollback-compaction` option changes this: `eager` compacts each line as it scrolls off, which is the smallest but the slowest, and `off` never compacts, which is the fastest but the largest.
- **Floods**: a terminal printing without end never holds up another you're typing in. The daemon reads it last in each turn, after updates have gone out, and stops as soon as anything else is ready. Output a program prints at once, such as a full-screen redraw, is still shown whole, and a program's last output always reaches its pane before the pane closes.
- **Resizing**: lines on the primary screen are rewrapped to the new width, and the cursor stays on the same character. A line that started in scrollback is rewrapped whole, as if it had been printed at the new width, so its part on screen still lines up with its part above. Only the last few screens' worth of a very long line is rewrapped, and older scrollback is left as it is. When the screen gets shorter, blank lines below the cursor go first, then lines above it move into scrollback. Erasing the top line or scrolling it away ends a line continuing from scrollback, so a resize never joins what's printed next onto it. The alternate screen is cut off or extended instead, because the full-screen programs that use it redraw after a resize.
- **Alternate screen**: full-screen programs switch to a second screen with no scrollback. Leaving it restores the primary screen and cursor exactly.
- **Wrapping**: printing in the last column leaves the cursor there, and the line wraps only when the next character arrives.
- **Background colour erase**: clearing part of the screen fills it with the current background colour.
- **Wide characters** take two columns. A wide character that doesn't fit at the end of a line wraps to the next one.
- **Combining characters** (accents, variation selectors) join the character before them. So do characters after a zero-width joiner, so emoji sequences such as 👩‍💻 stay one character.

## Supported sequences

| Kind | Supported |
|---|---|
| Control characters | BS, HT, LF, VT, FF, CR, SO, SI. BEL is accepted and ignored. |
| `ESC` | `7` `8` (save/restore cursor), `D` (index), `E` (next line), `H` (set tab stop), `M` (reverse index), `c` (full reset), `#8` (screen alignment test), `(` and `)` with `B` (ASCII) or `0` (DEC line drawing) |
| `CSI` cursor | `A` `B` `C` `D` `E` `F` `G` `H` `I` `Z` `` ` `` `a` `d` `e` `f`, and `s` `u` without parameters |
| `CSI` editing | `@` `J` (including `3J` to clear scrollback) `K` `L` `M` `P` `S` `T` `X` `b` (repeat) |
| `CSI` other | `g` (clear tab stops), `r` (scroll region), `m` (SGR), `!p` (soft reset), ` q` (cursor shape: 0 the user's default, 1 and 2 block, 3 and 4 underline, 5 and 6 bar; blinking isn't drawn) |
| `ESC` keypad | `=` and `>` (application and numeric keypad) |
| ANSI modes (`CSI h`/`l`) | 4 (insert), 20 (line feed is newline) |
| DEC modes (`CSI ? h`/`l`) | 1 (application cursor keys), 6 (origin), 7 (autowrap), 25 (show cursor), 47, 1047 and 1049 (alternate screen), 66 (application keypad), 1048 (save cursor), 2004 (bracketed paste), 9, 1000, 1002 and 1003 (mouse tracking), 1005, 1006 and 1015 (mouse encodings), 1004 (focus events) |
| SGR | Bold, dim, italic, blink, inverse, hidden, strikethrough and their resets. Underlines: single, double (`21` or `4:2`), curly (`4:3`), dotted (`4:4`), dashed (`4:5`). 8, 16, 256 and 24-bit colours for text, background and underline (`58`), in both the `;` and `:` spellings. |
| `OSC` | 0 and 2 (window title), 7 (working directory, as a `file://` URL, which names a tab with no title), 8 (hyperlinks, with `id=`; kept in history and through upgrades), 52 (clipboard: text a program copies goes to the window's clipboard or primary selection, as the window's `osc52` option allows; reading it is refused unless allowed, with an empty answer), 133 (shell integration: `A` prompt, `B` where typing starts, `C` output, with an optional `cmdline_url=` and `clackatoo=`, the secret clackatoo's own shell integration adds, which is checked and dropped, and `D` exit status, which marks a failed command's prompt line; see [commands](~clackatoo/reference/window#commands); the marks also say whether a [background tab](~clackatoo/reference/window#background-tabs) is running, and which signed command a terminal [kept running](~clackatoo/reference/window#keeping-a-terminal-running) runs again) |

## Queries answered

| Query | Reply |
|---|---|
| `CSI c` (primary device attributes) | `CSI ? 62 ; 22 c`: a VT220 with colour |
| `CSI 5 n` (status) | `CSI 0 n` |
| `OSC 52 ; c ; ?` (clipboard) | `OSC 52 ; c ; <base64> ST`, empty if refused or unanswered within 2 s, ended with BEL if the query was |
| `CSI 6 n` and `CSI ? 6 n` (cursor position) | `CSI <line> ; <col> R`, with `?` for the second form. Relative to the scroll region in origin mode. |
| `CSI > q` (terminal version) | `DCS > \| clackatoo <version> ST` |

## Not supported yet

These are ignored:

- Secondary device attributes (`CSI > c`) and the kitty keyboard protocol.
- Left and right margins (`CSI s` with parameters).
- Variation selector 16 does not widen the character before it.
