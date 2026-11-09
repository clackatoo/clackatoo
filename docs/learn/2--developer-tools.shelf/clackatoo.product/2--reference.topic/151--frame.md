---
title: The Frame and Modes
type: reference
description: Opening the window, how windows share one process, its frame and top bar, keyboard help, settings, insert and normal mode, the cursor, drawing, and learning clackatoo with :Tutor.
related:
  - clackatoo/reference/window
---

The window has a top bar of workspaces and tabs, and two modes: insert mode
types into the program, and normal mode is clackatoo's.

## Opening it

```sh
clackatoo
```

With no command, `clackatoo` opens a window on your
[workspaces](~clackatoo/reference/workspaces#workspaces-tabs-and-panes): the
one last used, at the tab and pane last used. If there are none, it makes one,
showing your newest running terminal, such as one started with
`clackatoo new`, or else a new terminal running your `$SHELL`. If no daemon is
running, it starts one in the background first. Until the daemon answers, the
window says it's connecting, and <kbd>Esc</kbd> quits. That daemon logs to
`daemon.log` in the [runtime directory](~clackatoo/reference/cli), and keeps
running after the window closes.

### Windows share one process

Every window opens in one process, the **process of windows**, which sets up
the GPU once, for its first window: that's most of the time a window takes to
open, so every window after it opens at once, in tens of milliseconds rather
than a few hundred. What's left is mostly your desktop making the window.

`clackatoo` hands its window to the process of windows over a socket only you
can reach, `window.sock` in the
[runtime directory](~clackatoo/reference/cli#daemon), and returns as soon as
the window is open, rather than staying until it closes. When none is running,
it forks one first, apart from itself: in a session of its own, with no
terminal, so closing the terminal you ran `clackatoo` in,
<kbd>Ctrl</kbd>+<kbd>C</kbd> or <kbd>Ctrl</kbd>+<kbd>Z</kbd> there, touch no
window. `ps` shows it as `clackatoo-win`, with the command line of the
`clackatoo` it was forked from. The launcher's token goes with the window, so
it takes focus. A window opened from another, as the
[machine picker](~clackatoo/reference/connecting#picking-a-machine) opens one,
opens in the same process too.

Each window is its own: its own view, mode and focus, its own connection to
its daemon, on this machine or another, and its own run of `init.lua`. What a
window starts, the daemon, ssh for a machine, a link's opener,
`clackatoo update`, is started with the environment and directory of the
`clackatoo` that opened the window, so a window opened from a shell with its
own `PATH`, virtualenv or `SSH_AUTH_SOCK` uses them. If one window fails, it
closes and the others stay open. If the GPU is lost, as when its driver
resets, each window opens again in a process of windows started afresh; if
it's lost a third time within a minute, the windows close instead. The process
ends with its last window.

A window gets a process of its own, which takes no others but those it opens,
when it couldn't be the window it would be in the process of windows, whose
own environment it shares: it's shown on another display (`WAYLAND_DISPLAY`,
`DISPLAY`); has another `HOME`, config, state, or cache directory
(`XDG_CACHE_HOME`), or desktop bus (`DBUS_SESSION_BUS_ADDRESS`); would draw
with another [renderer](#drawing), GPU, or driver (`WGPU_*`, `DRI_PRIME`,
`__NV_PRIME_RENDER_OFFLOAD`, `__GLX_VENDOR_LIBRARY_NAME`, `VK_ICD_FILENAMES`,
`VK_DRIVER_FILES`, `MESA_*`) or libraries (`LD_LIBRARY_PATH`, `LD_PRELOAD`);
has another locale (`LANG`, `LANGUAGE`, `LC_ALL`, `LC_CTYPE`, `LC_MESSAGES`,
`LC_COLLATE`, `LC_NUMERIC`, `LC_TIME`, `LC_MONETARY`, `LC_IDENTIFICATION`),
cursor theme (`XCURSOR_*`) or fontconfig (`FONTCONFIG_*`); or is from an older
build of clackatoo than the process. Windows like it share a process of
windows of their own, which listens on a socket named after what they share.
The build is the program's file, so a build of your own counts as new even at
the same version: the first window of a newer build, as after
`clackatoo update` or a new build put in place of the old, starts a process of
windows for itself and the windows after it, and the old process ends with its
last window. `:update` reopens only the window it's typed in, in the new
build.

The window opens in the `clackatoo` you ran itself only if no process of
windows takes it within a few seconds, if its environment is too big to hand
over (more than a megabyte), or with `CLACKATOO_WINDOW_HOST=off`, which also
keeps the windows it opens out of it.

Every window logs to `window.log` in clackatoo's state directory (see
[logs](~clackatoo/reference/connections#logs)), each line with its process's
ID, which windows in one process share. A font installed while the process of
windows runs is used once it has ended and started again, after its last
window closes.

The window sizes itself so that its one pane holds 100×30 cells. Resizing the
window resizes each pane's terminal to fit, and its program is told the new
size. When several windows show one terminal, the terminal takes the size of
whichever was last opened on it, resized or focused.

With `--remote <HOST>`, the window shows the workspaces on another machine,
through ssh. See [another machine](~clackatoo/reference/remote).

From a file manager, "Open in clackatoo" in its menu, or "Open With"
clackatoo, opens each folder in a new terminal tab and each file in a new
editor tab, in the window you used last, which switches to them and comes to
the front; with none open, a window opens on them.
[`clackatoo desktop install`](~clackatoo/reference/cli#file-managers) sets
this up, and [`clackatoo open`](~clackatoo/reference/cli#open) is what runs.

| Option | Environment variable | [Setting](#settings) | |
|---|---|---|---|
| `--font <FAMILY>` | `CLACKATOO_FONT` | `font` | The font family. Default: your system's monospace font. |
| `--font-size <POINTS>` | `CLACKATOO_FONT_SIZE` | `font-size` | The text size. Default: 12. |
| `--renderer <API>` | `CLACKATOO_RENDERER` | `renderer` | The graphics API to draw with: `vulkan` (the default) or `opengl`. See [drawing](#drawing). |
| `--remote <HOST>` | | | Use the daemon on another machine. |
| `--remote-command <COMMAND>` | `CLACKATOO_REMOTE_COMMAND` | | The command that runs clackatoo there. Default: `clackatoo`. |

An option on the command line wins over its environment variable, which wins
over the `font` and `fontsize` that `init.lua` sets as it runs, which win over
the setting. startup.toml's font is what the window opens in before config has
run; config nearly always runs first, so the window opens in config's font.

## The frame

The window draws its own frame, in place of the desktop's:

- **The top bar** holds your
  [workspaces](~clackatoo/reference/workspaces#workspaces-tabs-and-panes),
  each with a colored dot and how many tabs it has (unless its name is only a
  number, as unnamed workspaces' are), the one shown picked out, and a `+`
  after them that makes a new one, asking which machine once the daemon is
  connected to another. A workspace from another machine (see
  [`:connect`](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar))
  has a ring in place of its dot: in its color while connected, in the warning
  color while its connection is down, and filled amber while it needs a
  password. Hold the mouse over it to see which machine it's on, and how its
  connection is. The window's buttons sit at whichever end your desktop puts
  them, in the same order. Drag the bar to move the window, double-click it to
  maximize or restore it, and right-click it for the desktop's window menu. A
  workspace's chats mark it after its name, as tabs do (see
  [Workspaces, tabs, and panes](~clackatoo/reference/workspaces#workspaces-tabs-and-panes)).
  When there are more workspaces than fit, the bar shows those around the one
  shown, with a quiet count at either end of how many are hidden that way,
  such as `‹3` or `5›`; select a count to scroll that way. While a chat has
  [access to the window](~clackatoo/reference/agent-tools#native-window-access),
  a teal badge before the buttons names the chat and the access, as
  `Agent Control · chat 3`: select it to go to the chat, or its gold `×` to
  revoke the access. Only the top bar shows it, never the window's title,
  which a program could set to look the same.
- **The bottom bar** starts with the window's [mode](#modes), then the tabs of
  the workspace shown, with a `+` after them that opens a new one. When there
  are more tabs than fit, the tab shown stays in view, and a quiet count at
  either end, such as `‹3` or `5›`, says how many are hidden that way; select
  it to scroll. On the bar's right are how far the view is
  [scrolled](~clackatoo/reference/typing#scrolling) or a message, how many
  panes the tab has, and a reminder of the key to press next. These take the
  room the tabs leave; when there are more tabs than fit, a quarter of the bar
  is kept for them. A message too long for its room is cut short with `…`:
  hold the mouse over it to read it all. A tab's chats mark it after its name.
- **The panes** fill the space between the bars, side by side with a thin line
  between them. When a tab has more than one, a gold line runs round the
  focused pane. Gold is kept for focus and for what to act on: the cursor in
  normal mode, the focused pane, the button to press and a chat waiting for
  you. Links and matched letters are blue, and headings bold.
- **The palette** opens in the middle of the window for anything typed to
  clackatoo itself:
  [commands](~clackatoo/reference/workspaces#the-command-line),
  [searches](~clackatoo/reference/normal-mode#searching), and new names.

Drag the window's edges or corners to resize it. Interface text, such as the
names in the bars, is in your desktop's interface font. On GNOME, both the
font and where the buttons go come from its settings. If the desktop names a
font that isn't installed, it's your system's sans-serif font, as fontconfig
picks it, or else the first installed of Noto Sans, Cantarell, Inter, DejaVu
Sans, Liberation Sans and Roboto. The bottom bar is drawn in the terminal's
font, as Vim's status line is.

With the mouse:

- Select a workspace or tab to show it, and a pane to focus it.
- Select a `+` to make a workspace, or open a tab.
- Right-click a workspace or tab for **Rename** or **Close**. Rename opens the
  palette with the old name selected: typing replaces it, and an arrow key,
  <kbd>Home</kbd> or <kbd>End</kbd> keeps it to edit. Clearing a tab's name
  names it after its focused pane's program again. Close closes every pane in
  it, hanging up their programs.

## Keyboard help

Press **Ctrl+?** in the native window to see shortcuts for the current pane,
mode, prompt or popup. On a keyboard where `?` is
<kbd>Shift</kbd>+<kbd>/</kbd>, press
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>/</kbd>. Plain
<kbd>Ctrl</kbd>+<kbd>/</kbd> still goes to the terminal program.

Type to filter by key or description. <kbd>Up</kbd>/<kbd>Down</kbd> or
<kbd>Ctrl</kbd>+<kbd>P</kbd>/<kbd>Ctrl</kbd>+<kbd>N</kbd> scroll through
matches, <kbd>PageUp</kbd>/<kbd>PageDown</kbd> move a page, and the mouse
wheel scrolls too. <kbd>Esc</kbd>, <kbd>Ctrl</kbd>+<kbd>;</kbd>,
<kbd>Ctrl</kbd>+<kbd>?</kbd> again, or selecting outside it closes help. The
draft, selection, open popup, and unfinished command underneath are kept:
after <kbd>Ctrl</kbd>+<kbd>B</kbd>, for example, help lists bindings starting
with that prefix, and closing help lets you finish it.

The list uses the current Lua-configured mappings and their `desc` labels, so
replacements, unbindings and mode restrictions appear immediately. Built-in
entries are generated from the same key definitions that dispatch commands,
and Vim continuations come from its parser. They follow the current pane and
temporary state, including completion, pending operators and file-operation
confirmations; there is no separate built-in shortcut catalog to keep in sync.
Keys handled inside terminal programs, and arbitrary Lua `keypress` callback
logic, cannot be enumerated; terminal help identifies which keys pass through
to the program. <kbd>Ctrl</kbd>+<kbd>?</kbd> is reserved for this window-level
help.

## Settings

Settings you rarely change, or change to tune how the window starts, live in
`startup.toml`: `$XDG_CONFIG_HOME/clackatoo/client/startup.toml`, or
`~/.config/clackatoo/client/startup.toml`. The window reads it before anything
else starts, which takes a few microseconds.

```toml
font = "JetBrains Mono"
font-size = 12
renderer = "opengl"
priority = "normal"
```

`priority = "high"`, as it starts, has the window's thread, which draws it and
takes your keys, ask to run ahead of your other programs, as
[the daemon's does](~clackatoo/reference/cli#staying-quick-on-a-busy-machine);
`"normal"` leaves it as other programs are. `CLACKATOO_PRIORITY=off` in the
environment does the same.

`clackatoo config` shows and changes them, keeping the rest of the file,
comments included, as it was:

```sh
clackatoo config renderer opengl   # set one
clackatoo config renderer          # show one
clackatoo config --unset renderer  # back to its default
clackatoo config                   # all of them, and where the file is
```

In the window, [`:setpersist`](~clackatoo/reference/config#saving-options-settings-lua)
saves them too, as `:setp fontsize=13` or `:setp renderer=opengl`, the same
way. `renderer` and `priority` take effect in windows opened after.

A mistake in the file never stops the window opening. Each setting with a
mistake, such as an unknown name or a value of the wrong kind, is reported on
stderr and left at its default, and the rest still apply.

Keys, and what the window does, are configured in Lua instead, in `init.lua`
beside `startup.toml`. See [configuration](~clackatoo/reference/config).

The settings are on this machine. With
[`--remote`](~clackatoo/reference/remote), the window's settings come from the
machine the window opens on.

A font family is named as fontconfig knows it, as `fc-list : family` prints.
Characters the font doesn't have, such as CJK text or emoji, come from
fallback fonts, found the same way.

### A daemon from another build

Each build of clackatoo speaks one version of the protocol between windows and
the daemon, and refuses a daemon speaking another. When the window finds a
daemon from another build, it says so, and what <kbd>Enter</kbd> does depends
on which is older:

- **An older daemon, from protocol 11 on:** <kbd>Enter</kbd> upgrades it. The
  daemon becomes this build without stopping, so its terminals and the
  programs in them carry on, and the window opens on them.
- **An older daemon, from before protocol 11:** it's too old to upgrade, so
  <kbd>Enter</kbd> stops it and starts a new one. This closes every terminal
  in the old daemon, and hangs up their programs.
- **A daemon newer than the window:** the window says to open a window from
  the newer build instead. It never offers to replace a newer daemon.

<kbd>Esc</kbd> quits, leaving the daemon running.
[`clackatoo upgrade`](~clackatoo/reference/cli#upgrade) and `clackatoo stop`
do the same from the command line.

### Losing the daemon

If the connection to the daemon is lost, because the daemon stopped or,
[on another machine](~clackatoo/reference/remote), the network went, the
window says so and why, in place of the terminal:

- <kbd>Enter</kbd> reconnects, starting a daemon if none is running. If the
  workspace shown is still there, the window returns to it, with each pane as
  it was, in the same mode and scrolled to the same place.
- <kbd>Esc</kbd> quits.

When the daemon [upgrades](~clackatoo/reference/cli#upgrade), every connection
to it drops for a moment. A window whose daemon on this machine is still there
afterward reconnects by itself, without asking.

## Modes

The window is in one of two modes, like Vim:

- **Insert mode**, where it starts. Keys go to the program.
- **Normal mode**. Keys drive clackatoo. The bottom bar starts `NORMAL`, on
  gold, in place of `INSERT`, on lilac. A visual selection shows `VISUAL`,
  `V-LINE` or `V-BLOCK`, on blue.

The mode is the window's, not each pane's: every pane, tab, and workspace
shares it. Panes with no insert mode, such as the explorer, a board, a review
or a lesson, show `NORMAL` and leave the window's mode as it was. But whatever
you open or close from one, with a key, the mouse, or a command, leaves the
window in normal mode.

| Key | In | Does |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>;</kbd> | any | Switches to normal mode: from insert mode, and out of a visual selection, a search or the command line. |
| <kbd>Shift</kbd>+<kbd>Esc</kbd> or <kbd>Ctrl</kbd>+<kbd>&#92;</kbd> | any | The same. |
| `i` | normal mode | Switches to insert mode, back at the live screen. |
| <kbd>Ctrl</kbd>+<kbd>&#92;</kbd>, twice | insert mode | Sends a real <kbd>Ctrl</kbd>+<kbd>&#92;</kbd> to the program. |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | either | Interrupts the program. |

Plain <kbd>Esc</kbd> always goes to the program, since programs such as Vim
and less use it. <kbd>Ctrl</kbd>+<kbd>;</kbd> costs programs nothing: a
terminal has no code to send them for it, so none can be listening for it.
It's found by where the key is, so on layouts where `;` takes
<kbd>Shift</kbd>, such as German, it's <kbd>Ctrl</kbd> and the key to the
right of L.

## The cursor

In insert mode, the cursor has the shape the program asks for: a block, an
underline or a bar, which Vim and shells in vi mode switch between. If the
program hasn't asked, it's a bar. In normal mode it's always clackatoo's
yellow block. In panes without the focus, and while the window isn't focused,
either cursor is drawn as an outline.

## Drawing

The window draws with the GPU, and only when something changed: a quiet
terminal costs no CPU. It redraws at most once per frame, so a flood of output
costs one frame's drawing per frame.

On a machine with two GPUs, the window uses the one the system lists first,
which is the one driving the screen: on a desktop, usually the graphics card
the monitors are plugged into, and on a laptop, the integrated GPU. Frames
drawn on the other GPU might never reach the screen. To choose differently,
set `WGPU_POWER_PREF` to `high` for the faster GPU, or `low` for the one that
uses less power.

The window draws with Vulkan, or with OpenGL where Vulkan can't draw. On some
laptops with two GPUs, OpenGL opens a window much sooner: to choose between
them, Vulkan asks both about themselves, and that can take more than a second,
while OpenGL can use the screen's GPU without asking the other. When setting
up the GPU takes over half a second, the window says so on stderr, and
suggests trying OpenGL:

```sh
clackatoo config renderer opengl
```

If the API chosen can't draw, the window uses the other.

Box-drawing characters (`─` `│` `┌` `╭` `═`…) and block elements (`█` `▄`
`░`…) are drawn by clackatoo rather than taken from the font, so that borders
in htop, tmux or Vim join up across lines at any size. The diagonal lines come
from the font.

A character with others joined to it, such as a letter with accents, an emoji
sequence joined with zero-width joiners (👩&#x200D;💻), or a keycap (#️⃣), is
shaped: the font's own rules decide which glyphs draw it and where they go. An
emoji presentation selector (U+FE0F, as in ❤️) draws the character from an
emoji font.

A flag (🇬🇧) or an emoji with a skin tone (👍🏽) is drawn as one emoji, if the
emoji font has it. Each keeps the columns programs count for its parts, so a
skin-toned emoji is followed by two blank columns.

## Not supported

- Moving panes to another place in the layout.
- History lines are shown at the width they were printed at. A line that
  wrapped in a narrower window doesn't rewrap when the window gets wider.

## Learning clackatoo

`:Tutor` teaches clackatoo a lesson at a time, as vimtutor teaches Vim. It
opens the finder on the lessons, each with how far you've got;
<kbd>Enter</kbd> opens the one picked, and `:Tutor NAME` opens one by name,
such as `:Tutor text-objects`. The lessons are basics, text-objects, words,
registers, mouse, blocks, substitute, wrap, markdown, font, indenting,
snippets, explorer, shell, finder, agents, remote, and background.

A lesson opens in a tab of its own: the lesson on the left, and on the right a
practice pane, an editor on a file made for it or a terminal running your
shell. Do what the step says in the practice pane: once it's done, its heading
lights up, then stays green with a ✓ beside it, and the next step is lit and
scrolled to. A step with nothing to do is to read. Lessons draw code as chats
do, on a background of its own and never split between rows, with step titles
in bold, and a lesson's explorer is headed with the lesson's name.

| Keys | Does |
|---|---|
| `]s` | Skips to the next step, in normal mode in a lesson's tab. |
| `[s` | Goes back to the step before. |
| `:q`, in either of a lesson's panes | Ends the lesson, closing both. |

The lesson can't be changed: a change to it is put back. Lessons work in a
folder of their own, `clackatoo/tutor` in `$XDG_CACHE_HOME` (or `~/.cache`),
which goes when the lesson ends; a lesson's explorer can't leave it. A file of
yours opened in the practice pane is left as it is, unsaved changes and all,
when the lesson ends. Where each lesson was left is kept in `clackatoo/tutor`
in `$XDG_STATE_HOME` (or `~/.local/state`), so the next window picks up there.
With `--remote`, the tutor says it needs the daemon on this machine.

### What's new

The first time a window of a new build opens, if the build has something new,
the bottom bar says `clackatoo updated · :Tutor new` until the next key; a
first install says `Welcome to clackatoo · :Tutor to start`. `:Tutor new`
lists what's been added, newest first, those not listed before first and
marked `new`; <kbd>Enter</kbd> opens the lesson that teaches it.
