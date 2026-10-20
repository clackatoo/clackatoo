---
title: The window
type: reference
description: clackatoo's window shows your workspaces, their tabs and their panes of terminals, and sends your keys to the focused one. Insert mode types into the program; normal mode moves a cursor of clackatoo's own through history, and runs the window's commands.
related:
  - clackatoo/getting-started/quick-start
  - clackatoo/reference/cli
  - clackatoo/reference/remote
  - clackatoo/reference/terminal-emulation
---

The window is a client of the daemon, like the `clackatoo` command. It shows a workspace's terminals, keeps up with them as their programs print, and sends your keys to the one with the focus. Closing the window closes nothing: the terminals keep running in the daemon, and the next window shows them again, laid out as they were.

## Opening it

```sh
clackatoo
```

With no command, `clackatoo` opens a window on your [workspaces](#workspaces-tabs-and-panes): the one last used, at the tab and pane last used. If there are none, it makes one, showing your newest running terminal, such as one started with `clackatoo new`, or else a new terminal running your `$SHELL`. If no daemon is running, it starts one in the background first. Until the daemon answers, the window says it's connecting, and Esc quits. That daemon logs to `daemon.log` in the [runtime directory](~clackatoo/reference/cli), and keeps running after the window closes.

The window sizes itself so that its one pane holds 100×30 cells. Resizing the window resizes each pane's terminal to fit, and its program is told the new size. When several windows show one terminal, the terminal takes the size of whichever was last opened on it, resized or focused.

With `--remote <HOST>`, the window shows the workspaces on another machine, through ssh. See [another machine](~clackatoo/reference/remote).

| Option | Environment variable | [Setting](#settings) | |
|---|---|---|---|
| `--font <FAMILY>` | `CLACKATOO_FONT` | `font` | The font family. Default: your system's monospace font. |
| `--font-size <POINTS>` | `CLACKATOO_FONT_SIZE` | `font-size` | The text size. Default: 12. |
| `--renderer <API>` | `CLACKATOO_RENDERER` | `renderer` | The graphics API to draw with: `vulkan` (the default) or `opengl`. See [drawing](#drawing). |
| `--remote <HOST>` | | | Use the daemon on another machine. |
| `--remote-command <COMMAND>` | `CLACKATOO_REMOTE_COMMAND` | | The command that runs clackatoo there. Default: `clackatoo`. |

An option on the command line wins over its environment variable, which wins over the `font` and `fontsize` that `init.lua` sets as it runs, which win over the setting. startup.toml's font is what the window opens in before config has run; config nearly always runs first, so the window opens in config's font.

## The frame

The window draws its own frame, in place of the desktop's:

- **The top bar** holds your [workspaces](#workspaces-tabs-and-panes), each with a coloured dot and how many tabs it has (unless its name is just a number, as unnamed workspaces' are), the one shown picked out, and a `+` after them that makes a new one, asking which machine once the daemon is connected to another. A workspace from another machine (see [`:connect`](~clackatoo/reference/remote#another-machine-s-workspaces-in-your-bar)) has a ring in place of its dot: in its colour while connected, in the warning colour while its connection is down, and filled amber while it needs a password. Hold the mouse over it to see which machine it's on, and how its connection is. The window's buttons sit at whichever end your desktop puts them, in the same order. Drag the bar to move the window, double-click it to maximise or restore it, and right-click it for the desktop's window menu. A workspace's chats mark it after its name, as tabs do (see below). When there are more workspaces than fit, the bar shows those around the one shown, with a quiet count at either end of how many are hidden that way, such as `‹3` or `5›`; click a count to scroll that way.
- **The bottom bar** starts with the window's [mode](#modes), then the tabs of the workspace shown, with a `+` after them that opens a new one. When there are more tabs than fit, the tab shown stays in view, and a quiet count at either end, such as `‹3` or `5›`, says how many are hidden that way; click it to scroll. On the bar's right are how far the view is [scrolled](#scrolling) or a message, how many panes the tab has, and a reminder of the key to press next. These take the room the tabs leave; when there are more tabs than fit, a quarter of the bar is kept for them. A message too long for its room is cut short with `…`: hold the mouse over it to read it all. A tab's chats mark it after its name.
- **The panes** fill the space between the bars, side by side with a thin line between them. When a tab has more than one, a gold line runs round the focused pane. Gold is kept for focus and for what to act on: the cursor in normal mode, the focused pane, the button to press and a chat waiting for you. Links and matched letters are blue, and headings bold.
- **The palette** opens in the middle of the window for anything typed to clackatoo itself: [commands](#the-command-line), [searches](#searching) and new names.

Drag the window's edges or corners to resize it. Interface text, such as the names in the bars, is in your desktop's interface font. On GNOME, both the font and where the buttons go come from its settings. If the desktop names a font that isn't installed, it's your system's sans-serif font, as fontconfig picks it, or else the first installed of Noto Sans, Cantarell, Inter, DejaVu Sans, Liberation Sans and Roboto. The bottom bar is drawn in the terminal's font, as Vim's status line is.

With the mouse:

- Click a workspace or tab to show it, and a pane to focus it.
- Click a `+` to make a workspace, or open a tab.
- Right-click a workspace or tab for **Rename** or **Close**. Rename opens the palette with the old name selected: typing replaces it, and an arrow key, Home or End keeps it to edit. Clearing a tab's name names it after its focused pane's program again. Close closes every pane in it, hanging up their programs.

## Settings

Settings you rarely change, or change to tune how the window starts, live in `startup.toml`: `$XDG_CONFIG_HOME/clackatoo/client/startup.toml`, or `~/.config/clackatoo/client/startup.toml`. The window reads it before anything else starts, which takes a few microseconds.

```toml
font = "JetBrains Mono"
font-size = 12
renderer = "opengl"
```

`clackatoo config` shows and changes them, keeping the rest of the file, comments included, as it was:

```sh
clackatoo config renderer opengl   # set one
clackatoo config renderer          # show one
clackatoo config --unset renderer  # back to its default
clackatoo config                   # all of them, and where the file is
```

A mistake in the file never stops the window opening. Each setting with a mistake, such as an unknown name or a value of the wrong kind, is reported on stderr and left at its default, and the rest still apply.

Keys, and what the window does, are configured in Lua instead, in `init.lua` beside `startup.toml`. See [configuration](~clackatoo/reference/config).

The settings are on this machine. With [`--remote`](~clackatoo/reference/remote), the window's settings come from the machine the window opens on.

A font family is named as fontconfig knows it, as `fc-list : family` prints. Characters the font doesn't have, such as CJK text or emoji, come from fallback fonts, found the same way.

### A daemon from another build

Each build of clackatoo speaks one version of the protocol between windows and the daemon, and refuses a daemon speaking another. When the window finds a daemon from another build, it says so, and what Enter does depends on which is older:

- **An older daemon, from protocol 11 on:** Enter upgrades it. The daemon becomes this build without stopping, so its terminals and the programs in them carry on, and the window opens on them.
- **An older daemon, from before protocol 11:** it's too old to upgrade, so Enter stops it and starts a new one. This closes every terminal in the old daemon, and hangs up their programs.
- **A daemon newer than the window:** the window says to open a window from the newer build instead. It never offers to replace a newer daemon.

Esc quits, leaving the daemon running. [`clackatoo upgrade`](~clackatoo/reference/cli#upgrade) and `clackatoo stop` do the same from the command line.

### Losing the daemon

If the connection to the daemon is lost, because the daemon stopped or, [on another machine](~clackatoo/reference/remote), the network went, the window says so and why, in place of the terminal:

- Enter reconnects, starting a daemon if none is running. If the workspace shown is still there, the window returns to it, with each pane as it was, in the same mode and scrolled to the same place.
- Esc quits.

When the daemon [upgrades](~clackatoo/reference/cli#upgrade), every connection to it drops for a moment. A window whose daemon on this machine is still there afterwards reconnects by itself, without asking.

## Modes

The window is in one of two modes, like Vim:

- **Insert mode**, where it starts. Keys go to the program.
- **Normal mode**. Keys drive clackatoo. The bottom bar starts `NORMAL`, on gold, in place of `INSERT`, on lilac. A visual selection shows `VISUAL`, `V-LINE` or `V-BLOCK`, on blue.

The mode is the window's, not each pane's: every pane, tab and workspace shares it. Panes with no insert mode, such as the explorer, a board, a review or a lesson, show `NORMAL` and leave the window's mode as it was. But whatever you open or close from one, with a key, a click or a command, leaves the window in normal mode.

| Key | In | Does |
|---|---|---|
| Ctrl+; | any | Switches to normal mode: from insert mode, and out of a visual selection, a search or the command line. |
| Shift+Esc or Ctrl+\\ | any | The same. |
| `i` | normal mode | Switches to insert mode, back at the live screen. |
| Ctrl+\\, twice | insert mode | Sends a real Ctrl+\\ to the program. |
| Ctrl+C | either | Interrupts the program. |

Plain Esc always goes to the program, since programs such as Vim and less use it. Ctrl+; costs programs nothing: a terminal has no code to send them for it, so none can be listening for it. It's found by where the key is, so on layouts where `;` takes Shift, such as German, it's Ctrl and the key to the right of L.

## Workspaces, tabs and panes

The window shows your work at three levels:

- **Workspaces**, one for each piece of distinct work, such as a project. Their names run along the [top bar](#the-frame). Often there's just one.
- **Tabs**, inside a workspace. They run along the bottom bar, numbered, and named after the title of their focused pane's program, or, for a terminal whose program set none, the folder its shell is in (`~` for your home), unless you've named them.
- **Panes**, which a tab is split into, side by side or stacked. Each shows a terminal, a [chat with an agent](~clackatoo/reference/agents), or a [file being edited](~clackatoo/reference/editor). A split shares its space equally until you [resize it](#resizing-panes).

Workspace names are unique on each machine, so one from another machine may share a name with one of yours. It then shows its machine after its name, as `notes@desk`; [`showhost`](~clackatoo/reference/config#options) changes when. `:workspace notes@desk` switches to it by its machine, and `:workspace notes` alone to your own.

A tab says what its chats are doing after its name: `⋯` while one's agent is at work, `?` while one asks you something, and `•` when one finished while this window wasn't showing it. A workspace in the top bar says the same of all its chats, so a chat in another workspace that wants you shows there. Where several would say different things, the one that needs you most shows: `?`, then `⋯`, then `•`.

What's unseen is each window's own: a chat counts as seen once its tab is shown in a focused window, and a chat finished while no window or phone was looking is unseen to a window that comes to it later. A chat that starts again loses its `•`. While the daemon is out of reach, no chat is said to work or ask.

The daemon keeps all of them, as it keeps terminals. Closing the window closes nothing, and the next window opens on the workspace, tab and pane last used. Each window shows one workspace, so two windows can show two workspaces, or two tabs of one.

### After a restart

The daemon keeps your workspaces, tabs and panes, so they come back after a restart, a crash or a power cut: with their names, order, splits and sizes, and the tab and pane you were on. Each terminal comes back as a new shell in the folder it was in, or your home if that's gone. Each editor comes back on its file with its cursor and view where they were, and with its changes not saved if it had any, as `[No Name]` files do; explorers and folders keep their root; boards and reviews come back too. Scrollback and running programs don't. An editor whose file has been deleted is left out.

A terminal whose shell was running a command shows "was running: cargo watch" at its bottom right. In normal mode, `R` puts the command at the new shell's prompt, where you read it and press Enter to run it; Esc dismisses it, and a click on either does the same. Nothing runs by itself. Typing into the terminal dismisses it too, in every window. This works in bash, zsh and fish 4 when clackatoo starts them as your shell: their integration marks each command with a secret only it and clackatoo know, and only a command marked so is offered, never one a program printed. A command the shell keeps out of its history is never offered: one typed with a space before it, one bash leaves out (`HISTCONTROL`, `HISTIGNORE`), one your `zshaddhistory` or `fish_should_add_to_history` turns away, and everything in `fish --private`. bash is cautious here: with `ignoredups` (part of `ignoreboth`) or `erasedups`, running the same command twice in a row gives no offer for the second. A terminal started with a command of its own, or a shell started any other way, comes back without an offer.

To start once with none, run `clackatoo stop`, then [`clackatoo daemon --fresh`](~clackatoo/reference/cli#daemon).

A pane closes when its program exits, as in other terminals. A tab closes with its last pane, and a workspace with its last tab. When the last workspace closes, so does the window.

### The command line

In normal mode, `:` opens a command line, in the palette in the middle of the window. Enter runs the command. Esc, or Backspace past the colon, closes it.

While it's open, the bottom bar keeps the mode and the tabs, and the focused pane's cursor is drawn hollow, since what you type goes to the line. The same holds for a search, a new name, the finder and the machine picker. Tab completes the word before the caret: a command's name, a kind of pane after a split's mark (`:|Te`), an option after `:set`, with `no` or `inv` before a switch and `name=` before a choice's values, a file or folder after `:e`, `:w`, `:Editor` and `:Explore` (spaces in names are written `\ `), a workspace after `:ws`, a machine after a kind of pane's `@` and after `:ws new@`, a lesson after `:Tutor`, and the words `:Gitsigns`, `:Devices` and `:Review` take. One choice goes in at once. With more, they're listed under the line, and Tab and Shift+Tab, or Down and Up, go through them, and past either end back to what you typed. Files complete only while the daemon is on this machine, and a slow folder says `listing…` while it's read.

Up and Down go back through the commands you've typed, in this window and others, keeping to those that start with what's typed. They're kept in `clackatoo/history` under `$XDG_STATE_HOME`, the last 100, readable only by you; a line you start with a space isn't kept, nor is a device's pairing code, and a line of either kind is left out when the file is read, if an older build kept it. Left, Right, Home and End move the caret, and Ctrl+Left and Ctrl+Right move it a word. Ctrl+U deletes everything before the caret, and Ctrl+W the word before it. Ctrl+R and a register's name puts in what the register holds: Ctrl+R Ctrl+W the word under an editor's cursor, Ctrl+R `:` the last command, and Ctrl+R `/` the last search. A count before `:` in an editor starts the line with that many lines, `5:` giving `:.,.+4`. From a selection it's `'<,'>`, count or not.

| Command | Does |
|---|---|
| `:PortForward 8080`, `:PortForward 3000:host:8080`, `:PortForward`, `:PortForward! 8080` | In a window on another machine, forwards a port here to one there, lists the forwards, or stops one: see [Forwarding ports](~clackatoo/reference/remote#forwarding-ports). |
| `:Terminal` | Opens a terminal in a new tab. |
| `:\|Terminal` | Splits the focused pane, with a new terminal to its right. |
| `:\|<Terminal` | The same, to its left. |
| `:-Terminal` | The same, below it. |
| `:-^Terminal` | The same, above it. |
| `:Terminal@desk`, `:\|Terminal@desk`… | The same, on the machine named after `@`: one the daemon is [connected to](~clackatoo/reference/remote#another-machine-s-workspaces-in-your-bar), by its connection's name, or `local` for the daemon's own. Any kind of pane takes one: `:Editor@desk PATH`, `:Explore@desk`, `:Agent@desk codex`. |
| `:Agent`, `:\|Agent`… | Opens a [chat with an agent](~clackatoo/reference/agents), the one named after it (`:Agent codex`) or the `agent` option's, in a new tab or beside the focused pane. |
| `:Board NAME`, `:Boards` | Opens a [board](~clackatoo/reference/boards) shared with agents, making it if there's none; `:Boards` picks one in the finder. |
| `:update`, `:update HOST` | Installs the latest release here, as [`clackatoo update`](~clackatoo/reference/cli#update) does, and says in the bar how it went; with `HOST`, on the remote daemon's machine, in a tab of its own. Once a day the window looks for a new release and says so quietly in the bar; it never installs one by itself. |
| `:Tutor`, `:Tutor NAME`, `:Tutor new` | Opens [the tutor](#learning-clackatoo): its lessons in the finder, the lesson called NAME, or what's new. |
| `:Editor PATH`, `:\|Editor PATH`…, `:e PATH`, `:e` | [Edits the file](~clackatoo/reference/editor), in a new tab or beside the focused pane; `:e` in place of the file an editor shows, and alone, an explorer beside an empty file. |
| `:Files`, `:Grep PATTERN`, `:Buffers`, `:Diagnostics`, `:Symbols`, `:ProjectSymbols NAME` | Opens [the finder](#the-finder), with what follows typed. |
| `:registers`, `:reg` | Lists what an editor's [registers](~clackatoo/reference/editor#registers) hold, by its cursor. |
| `:messages`, `:mes` | Shows what the bottom bar has said lately, above it, until the next key. |
| `:marks` | Opens [the finder](#the-finder) on the [marks](~clackatoo/reference/editor#marks) the focused file can go to. |
| `:1`, `:2`… | In an editor, goes to that line, as Vim does, as do `:$`, `:+3` and `:'a`. Elsewhere, moves to that tab of the workspace shown. |
| `:s/old/new/`, `:%s/old/new/g`… | Replaces text in the focused editor, with [Vim's ranges and flags](~clackatoo/reference/editor#substituting). |
| `:g/pattern/d`, `:v/…`, `:d`, `:y`, `:m`, `:t`, `:normal` | Vim's [commands for lines](~clackatoo/reference/editor#lines), in the focused editor. |
| `:workspace NAME`, `:ws NAME` | Switches to the workspace called NAME, making it if there's none. Without a name, makes a new workspace, named with the lowest number no workspace has. `:ws new@desk NAME` makes one on desk, as does `:ws NAME@desk` if desk has none of that name. |
| `:rename NAME` | Renames the workspace shown. |
| `:close` | Closes the focused pane, hanging up its program. |
| `:q` | The same, unless the pane is the last showing a file with changes that aren't written. `:q!` closes it anyway, throwing them away. |
| `:w`, `:wq`, `:e!`… | Write the focused pane's file, and the rest of Vim's commands for files: see [Editing files](~clackatoo/reference/editor#writing). |
| `:reload` | Runs the [config](~clackatoo/reference/config) again, from the start. |
| `:set nonu`, `:set rnu!`… | Changes [options](~clackatoo/reference/config#options), or shows them, as Vim's `:set` does. |

The first character of a split says which way the divider runs, `|` upright or `-` flat, and `<` or `^` after it puts the new pane first. A mark alone opens a terminal, so `:|` is `:|Terminal`. Capitalised words name kinds of pane: `Terminal`, `Agent`, `Editor`, which takes the file's path after it, and `Explore` (or `Ex`), which takes a folder, and `Review`; `:|Explore` and `:-Review` open a new explorer or review beside the focused pane. Lowercase words are the window's own commands. A number goes to that line in an editor, or else that tab. A command with a range of lines before it, or one of Vim's for lines, goes to the focused editor; from visual mode, `:` starts with `'<,'>`.

A new pane goes on the machine of the pane it's made from: the one split, or the focused one for a new tab or explorer, so `:Terminal`, `:e`, `:Explore` and the finder in a pane on desk are desk's. `@host` after a kind of pane picks the machine instead (see [a pane on another machine](~clackatoo/reference/remote#a-pane-on-another-machine)).

A new terminal starts in the directory that the pane it splits is in. A new tab's terminal starts in the directory of the workspace's focused pane. That's the directory of whatever is in front in that pane, such as a nested shell or an editor that changed directory, or else the shell's own.

Workspace names are unique, and can't contain `@`, which names workspaces on other machines. If a command fails, such as renaming a workspace to a name another has, the bottom bar says why until the next key, in red, and if that's too long for the bar, whole above it for a few seconds. `:messages` shows what the bar has said lately. A command, an option or a lesson that isn't one says only so, as "no command ‘nonsens’", and offers the nearest name when a letter or two is off: `:Fils` asks "did you mean ‘:Files’?", and `:set nonumbr` asks about ‘:set nonumber’. It never offers `:q` or `:wq` for a slip. Tab lists the names themselves.

### Resizing panes

Drag the gap between two panes to move it. It moves a whole column or row at a time, and the programs on either side are told their new size as it goes. A double click on the gap shares that split's space equally again.

In normal mode, Alt+H, Alt+J, Alt+K and Alt+L move the focused pane's edge on that side by a column or row, so Alt+L widens a pane with a pane to its right. A pane with no edge on that side moves its other edge instead, so Alt+L narrows the rightmost pane. No pane gets smaller than one cell.

A new pane in a split gets the split's average share, so a split you never resized stays equal. Sizes are kept by the daemon, like the rest of the layout, and other windows show the change.

### Moving around

In normal mode, Ctrl+H, Ctrl+J, Ctrl+K and Ctrl+L move to the pane to the left, below, above or right. Tabs are `:1`, `:2` and so on, and workspaces `:ws NAME`. [Ctrl+B](#ctrl-b) does all of this in either mode.

Wherever you go, you arrive in the window's mode. That holds when you move, click, switch tabs or workspaces, jump, or open a pane, and when the pane you were in closes because you closed it, or its program ends just after you typed into it, as after `exit` or Ctrl+D. Arriving in insert mode, a terminal goes back to its live screen and a chat to its message box. An editor types at its cursor once its file's text has come; until then, what you type waits for it, and any other key, such as Esc or Ctrl+B, is had at once in normal mode. A chat that's asking something is arrived at in normal mode, so a key can't answer it by chance. If anything else moves the focus, such as another window or an agent closing the pane or opening a file in it, you arrive in normal mode and the window goes to normal mode, so nothing you type lands where you didn't choose. Leaving a pane ends a selection, a half-typed command, a search or the command line. The pane keeps its place in history, or its cursor.

The window remembers where you've been, as Vim's jump list does: the panes you moved between, and in an [editor](~clackatoo/reference/editor#jumps), the places in files you jumped from.

| Keys | In | Does |
|---|---|---|
| Ctrl+O Ctrl+I | normal mode | Back and forward through the places in the tab shown. A count goes that many places: `3` Ctrl+O goes back three, or nowhere if there aren't three. |
| Ctrl+Shift+O Ctrl+Shift+I | either mode | Back and forward through everywhere: panes, tabs and workspaces. |

Going back first remembers where you are, so going forward comes back to it. Going back and then jumping somewhere new forgets the way forward. Panes that have closed are skipped. Each window has its own list.

### Ctrl+B

Ctrl+B and one more key work in either mode, as tmux's prefix does. While Ctrl+B waits, the bottom bar marks it `^B` by the mode, and a panel above the bar lists every key it takes, with what each does; a key that starts others says `+` and what follows. With two panes or more, each shows its number. Other keys that start others, as Space does in normal mode, are marked at once and list theirs after `whichkeydelay`, so keys typed quickly never flash the panel. The panel is made from the bindings: a key your config binds says what its `desc` says, or "from your config" without one, and a key that starts others says `+` and its group's name, as Space `p` says `+project`, or else what they share, or how many keys can follow. Esc, or any other key, cancels. These keys, and the others on this page that the window takes rather than the program, are bindings like any you make, so [the config](~clackatoo/reference/config) can change them or take them away.

| Ctrl+B, then | Does |
|---|---|
| `1`–`9` | Moves to that tab. |
| `c` | Opens a new tab. |
| `]` `[` | Moves to the next tab, or the one before, round from the last to the first. |
| `,` | Renames the tab. |
| `p`, then `1`–`9` | Moves to that pane. Each pane shows its number, counting left to right and top to bottom. |
| `h` `j` `k` `l` | Moves to the pane to the left, below, above or right. |
| `\|` `-` | Splits the pane, with the new one to the right or below. |
| `a` | Opens a [chat with an agent](~clackatoo/reference/agents) to the right. |
| `x` | Closes the pane. |
| `w` | Opens the workspace picker. |
| `o` `i` | Back and forward through the tab's places, as Ctrl+O and Ctrl+I do in normal mode. |
| Ctrl+B | Sends Ctrl+B to the program. |

The workspace picker lists the workspaces whose names hold what you type, those starting with it first. Up and Down, or Tab and Shift+Tab, pick one, and Enter moves to it. If nothing matches, Enter makes a workspace by the name typed.

### The finder

The finder picks a file from the project the focused pane is in: the files git doesn't ignore, leaving out hidden ones; or with `:Grep`, the lines in them a pattern matches; or the files open. It looks from the project's root: the one the file's language server was started for, or else its repository's, or for a terminal, the repository its program is in. Outside your home directory, only a project is searched, never a bare directory.

| Keys, in normal mode | Opens |
|---|---|
| `<Space>pf` | The project's files |
| `<Space>bs` | The symbols in the focused editor's file: its functions, types and their fields, in the order of the file's outline, each indented inside what it's in, with what it is; past 5000, the count ends `+` |
| `<Space>ps` | The symbols in the whole project whose names match what you type, as the file's language server finds them, asked again each time typing pauses, with the letters you typed picked out in their names |
| `<Space>e` | Opens the tab's [file explorer](~clackatoo/reference/explorer), goes to it, or closes it |
| `<Space>pgf` | A grep of them, as you type |
| `<Space>?` | The files open, `[+]` after those with changes |
| `<Space>ds` | What's wrong in the project's files, as their language servers say, the worst first: the files open, and those not open that a server has checked, as rust-analyzer checks a whole crate, each `error`, `warning`, `info` or `hint` in the editor's colour for it |

What you type matches as fzf's does: letters in order, words each on their own, `'word` exactly, `^word` at the start, `word$` at the end, `!word` leaves out. A grep's pattern is a regular expression, or plain text if it isn't one. The letters it matched are picked out, in every list.

When nothing matches what you type, the list says so once it has finished looking.

| Key | Does |
|---|---|
| Down, Ctrl+N; Up, Ctrl+P | The next or previous one |
| Enter | Opens it in place of the focused pane's file, as `:e` does |
| Ctrl+V, Ctrl+X | Opens it beside the focused pane, or below |
| Ctrl+T | Opens it in a new tab |
| Ctrl+U, Ctrl+W | Deletes what's typed, or its last word |
| Esc | Closes the finder |

A click picks one, a double click opens it, and the wheel moves through them. Beside the list, if the window is wide enough, is the file round the line picked, coloured as an editor colours it, with a grep's match picked out in its line. A file that's open is coloured as its buffer is; one that isn't is coloured by the language its name ends in. A name with no ending such as `Makefile`, a language that isn't installed, or a daemon older than protocol 84 leaves it plain, and a preview from the middle of a file can be coloured wrongly inside a long comment. An empty file says so. Opening one is a [jump](~clackatoo/reference/editor#jumps), so Ctrl+O comes back. Space starts these keys in normal mode, so Space alone no longer moves the cursor, unless a command is half typed, as in `d<Space>`.

## Typing

In insert mode, keys reach the program the way xterm sends them, which nearly every program understands:

- Text types itself. Ctrl with a letter sends its control character (Ctrl+C, Ctrl+D…). Alt sends Escape before the key.
- Enter, Backspace, Tab, Shift+Tab and Escape send their usual codes. Ctrl+Backspace deletes a word in most shells.
- Arrows, Home, End, Insert, Delete, Page Up, Page Down and F1 to F12 send escape sequences, with any modifiers held.
- When a program asks for them, arrows and the number pad send application codes, which Vim and less expect.

Ctrl+Shift+V pastes, and Ctrl+Shift+C copies text [selected with the mouse](#the-mouse); neither reaches the program. If the program has asked for bracketed paste, the text is marked as pasted, so editors don't treat it as typing and shells don't run it line by line. Otherwise newlines are sent as Enter.

Typing while scrolled up returns the view to the live screen.

Text can be typed through an input method, as Chinese, Japanese and Korean are, such as IBus on GNOME. What's being composed shows underlined where it will go, at the cursor or in the bottom bar, with the input method's choices beside it, and the text chosen is typed as if keyed. The input method is used in insert mode, the command line and searches; in normal mode, keys are clackatoo's commands, so it's left out, and it's never used for a password. `:set noime` turns it off.

## Scrolling

The mouse wheel scrolls history in either mode, in the pane under the pointer. On the alternate screen, which full-screen programs use and which has no history, the wheel sends arrow keys instead, so less, Vim and htop scroll themselves. A program that takes [the mouse](#the-mouse) gets the wheel itself.

While the view is scrolled up, a pill at the pane's bottom right says how far, and how to get back: `↓ 42 lines · G to return` in normal mode, `type to return` in insert mode, where typing goes back to the live screen. A pane without the focus says only how far. In a pane too small for the pill, the bottom bar's note says it instead, as `↑42`. New output doesn't move what you're reading.

## The mouse

Clicking a pane focuses it. What else the mouse does depends on the program in the pane.

**Selecting.** Usually the mouse selects text:

- Dragging selects characters. Dragging past the top or bottom of the pane scrolls it.
- A double click selects a word. Paths and URLs count as one word, so `~/src/main.rs` comes whole.
- A triple click selects whole lines.
- With Alt held, or Ctrl and Alt, dragging selects a block of columns. Over a program that takes the mouse, add Shift.

Letting go copies the selection to the primary selection, and a middle click pastes it, as elsewhere on Linux. Ctrl+Shift+C, or `y` in normal mode, copies it to the clipboard. The selection stays highlighted until the next key or click. In normal mode, a click also moves the cursor there.

**Programs that take the mouse.** Programs such as Neovim, htop and less can ask for clicks, drags and the wheel, and get them while their pane is in insert mode on the live screen. The pointer is then an arrow rather than a text cursor. Hold Shift to select text anyway. A drag stays with the pane it started in, even when the pointer leaves it.

Programs that ask are also told when their pane gains or loses the focus, which Neovim uses to reload files changed meanwhile.


**Links.** Hold Ctrl and the link under the pointer is underlined, and the pointer becomes a hand. Ctrl+click opens it. Links are what programs print as links (`ls --hyperlink`, gcc, systemd and git do), plus URLs (`http://`, `https://`, `file://`, `mailto:`) and file paths in the text, such as `src/main.rs:12:5` from rustc or `src/main.rs:12:` from grep. A URL opens in your browser, or whatever the desktop opens it with. A file opens in an editor pane at that line: in an editor already in the tab, else beside the terminal if it's wide enough, else in a tab of its own. A relative path is from the directory the shell is in. Ctrl+O comes back. A `file://` link naming another machine isn't opened, and the bar says why. When a program takes the mouse, Ctrl+click is the program's, and Ctrl+Shift+click opens the link.
## Normal mode

Normal mode has a cursor of its own, a yellow block, which moves through the terminal's history and screen as one document. It starts where the program's cursor is, and the program's cursor is hidden until you return to insert mode.

| Keys | Move |
|---|---|
| `h` `j` `k` `l`, arrows | Left, down, up, right. Moving up and down returns to the column you started from after passing shorter lines. |
| `w` `b` `e` | To the next word's start, this or the previous word's start, this or the next word's end. Words are runs of letters, digits and `_`, or runs of other characters; both cross line ends. |
| `0` `$`, Home End | The line's first column, its last character. |
| `gg` `G` | The oldest line of history, the screen's last line, or with a count, that line. |
| `gx` | Opens the link under the cursor, as Ctrl+click does. |
| `%` `{` `}` | The bracket that pairs with the one under the cursor; the blank line before or after the paragraph. |
| `H` `M` `L` | The top, middle and bottom lines in view. |
| `5k`, `3w`, `10G`, `2gg` | A count before a motion repeats it, as in Vim; before `G` or `gg`, it goes to that line, counting from the oldest line of history. `3H` and `3L` count from the view's top and bottom. A count waiting shows in the bar. |
| Ctrl+D Ctrl+U | Half a screen down or up, view and cursor together. |
| Ctrl+F, Page Down Page Up | A screen down or up. Ctrl+B is [the window's](#ctrl-b). |
| Ctrl+E Ctrl+Y | The view a line down or up, with the cursor kept in it. |

Moving past the top of the view scrolls into history. In normal mode the view holds still while output arrives, so the cursor stays on its text. On the alternate screen the cursor moves over the screen only.

### Selecting and copying

| Keys | Does |
|---|---|
| `v` | Selects characters, from the cursor to wherever it moves. |
| `V` | Selects whole lines. |
| Ctrl+V | Selects a block of columns. `$` takes every line to its end, and `O` goes to the other corner on the same line. |
| `o` | Moves the other end of the selection. |
| `y`, Ctrl+Shift+C | Copies the selection to the clipboard, and returns to normal mode. |
| `yy`, `Y` | Copies the cursor's line, or with a count, as many lines from it: `3yy`. `y2w` and `2yj` take counts too. |
| `y` and a motion | Copies what the motion moves over: `yw` to the next word, `y$` to the line's end, `yj` this line and the next, `ygg` up to the top. |
| `y` and a [text object](#text-objects) | Copies the object around the cursor: `yiw` the word, `yi"` what's in the quotes, `yib` a command's output. |
| Esc | Drops the selection. |

A selection by characters lights each line to a cell past its text, which is what `y` copies; `V` lights whole lines, the pane's width. `y` on a block copies a line for each row, without the blanks at their ends, and never joins a wrapped line to the next. A copy stops at 100,000 lines or 4 MiB, and says so. A daemon too old to copy blocks copies from corner to corner, and says so.

Any motion extends a selection, including `gg`, `G` and searches. What you copy to the clipboard lights up for a moment, so you can see what went. Copied text is what the program printed: a line that wrapped only because it reached the window's edge comes out as one line, blanks at the ends of lines are left out, and there is no newline after the last line, so pasting it into a shell never runs it.

### Text objects

After `y`, or in visual mode, `i` or `a` and one more key pick out a piece of text around the cursor, as in Vim. `i` takes what's inside, `a` takes it all. In visual mode, the object becomes the selection.

| Keys | Picks out |
|---|---|
| `iw` `aw` | A word, and with `a` the blanks after it (or before it, at the end of a line). |
| `iW` `aW` | The same, for anything between blanks. |
| `i"` `a"`, `i'` `a'`, `` i` `` `` a` `` | What's between the quotes around the cursor, or the next pair on its line; with `a`, the quotes too. |
| `i(` `a(`, `i[` `a[`, `i{` `a{`, `i<` `a<` | What's between the brackets around the cursor, across lines; with `a`, the brackets too. The closing bracket works as well. |
| `ib` `ab` | A [command's](#commands) output; with `a`, its prompt and command line too. |

In Vim, `b` is another name for `(`. Here it's the command, the more useful thing in a terminal, and `(` still means parentheses.

### Commands

When the shell marks its prompts and commands, clackatoo knows where each command's prompt and output are, and how it exited.

| Keys | Does |
|---|---|
| `[[` `]]` | Moves to the previous or next command's prompt. |
| `yib` | Copies the output of the command the cursor is in. |
| `yab` | Copies the command with its prompt and output, ready to paste into a bug report. |
| `vib` `vab` | Selects them. |

A command's output runs from its first line to the line before the next prompt, leaving out blank lines at the end. If the command printed nothing, `yib` says `no output`. If the cursor isn't in a command, the bottom bar says `no block here`.

Which shells mark their commands:

- **fish** 4 and later, by itself. clackatoo also runs a small script once your startup files have run (`fish -C`), which marks each command again with the terminal's secret, so it can be offered after a restart.
- **bash**, when it's your shell (`$SHELL`) and clackatoo starts it. clackatoo starts it with a startup script of its own, which reads your `~/.bashrc` first, then adds the marks to `PS1`, `PS0` and `PROMPT_COMMAND`. The command goes with its mark, from bash's history. bash started any other way, such as `clackatoo new -- bash`, is left as it is.
- **zsh**, when it's your shell and clackatoo starts it. clackatoo points ZDOTDIR at a `.zshenv` of its own, which puts ZDOTDIR back, reads your `.zshenv`, and adds its hooks once your `.zshrc` has run, so oh-my-zsh, powerlevel10k (instant prompt included) and a ZDOTDIR of your own all work. A shell started with `exec zsh`, or inside another, isn't marked.
- fish before 4 doesn't.

Commands are kept through an [upgrade](~clackatoo/reference/cli#upgrade).

A command that failed ends its prompt's line with a small red `✗` and its exit status, as `✗ 2`, two cells after the line's text, on the screen and in history. A line too long for it, as one a prompt on the right takes to the edge, isn't marked; a prompt of two lines is marked on its first. The mark isn't part of the text: copying, searching and selecting leave it out. The [`blockmarks` option](~clackatoo/reference/config#options) marks every finished command, the ones that worked with a faint `✓` (`all`), or none (`none`). Marks are kept through an upgrade, and need a daemon of protocol 86 or later.

### Searching

| Keys | Does |
|---|---|
| `/` | Searches forward from the cursor. Type the text, shown in the palette, then Enter. Esc cancels. It's edited as the command line is, and Up and Down go back through the searches typed before, in terminals and editors alike. |
| `?` | Searches backward. |
| `n` `N` | Searches again, the same way or the other way. |
| Esc | Clears the highlight on the last match. |

A search ignores case unless the text has a capital letter in it, as the [`ignorecase` and `smartcase` options](~clackatoo/reference/config#options) start; set them to change it. It covers all of history and wraps around at either end, saying `search wrapped to top` (or `to bottom`) when it does. The cursor moves to the match, which is highlighted. If there's none, the bottom bar says `not found`.

Copying and searching both happen in the daemon, which holds all of history, so neither waits for the window to fetch the lines involved.

## The cursor

In insert mode, the cursor has the shape the program asks for: a block, an underline or a bar, which Vim and shells in vi mode switch between. If the program hasn't asked, it's a bar. In normal mode it's always clackatoo's yellow block. In panes without the focus, and while the window isn't focused, either cursor is drawn as an outline.

## Drawing

The window draws with the GPU, and only when something changed: a quiet terminal costs no CPU. It redraws at most once per frame, so a flood of output costs one frame's drawing per frame.

On a machine with two GPUs, the window uses the one the system lists first, which is the one driving the screen: on a desktop, usually the graphics card the monitors are plugged into, and on a laptop, the integrated GPU. Frames drawn on the other GPU may never reach the screen. To choose differently, set `WGPU_POWER_PREF` to `high` for the faster GPU, or `low` for the one that uses less power.

The window draws with Vulkan, or with OpenGL where Vulkan can't draw. On some laptops with two GPUs, OpenGL opens a window much sooner: to choose between them, Vulkan asks both about themselves, and that can take more than a second, while OpenGL can use the screen's GPU without asking the other. When setting up the GPU takes over half a second, the window says so on stderr, and suggests trying OpenGL:

```sh
clackatoo config renderer opengl
```

If the API chosen can't draw, the window uses the other.

Box-drawing characters (`─` `│` `┌` `╭` `═`…) and block elements (`█` `▄` `░`…) are drawn by clackatoo rather than taken from the font, so that borders in htop, tmux or Vim join up across lines at any size. The diagonal lines come from the font.

A character with others joined to it, such as a letter with accents, an emoji sequence joined with zero-width joiners (👩‍💻), or a keycap (#️⃣), is shaped: the font's own rules decide which glyphs draw it and where they go. An emoji presentation selector (U+FE0F, as in ❤️) draws the character from an emoji font.

A flag (🇬🇧) or an emoji with a skin tone (👍🏽) is drawn as one emoji, if the emoji font has it. Each keeps the columns programs count for its parts, so a skin-toned emoji is followed by two blank columns.

## Not supported yet

- Moving panes to another place in the layout.
- History lines are shown at the width they were printed at. A line that wrapped in a narrower window doesn't rewrap when the window gets wider.

## Learning clackatoo

`:Tutor` teaches clackatoo a lesson at a time, as vimtutor teaches Vim. It opens the finder on the lessons, each with how far you've got; Enter opens the one picked, and `:Tutor NAME` opens one by name, such as `:Tutor text-objects`. The lessons are basics, text objects, registers, substitute, wrap, indenting, snippets, explorer, shell, finder, agents and remote.

A lesson opens in a tab of its own: the lesson on the left, and on the right a practice pane, an editor on a file made for it or a terminal running your shell. Do what the step says in the practice pane: once it's done, its heading lights up, then stays green with a ✓ beside it, and the next step is lit and scrolled to. A step with nothing to do is to read. Lessons draw code as chats do, on a background of its own and never split between rows, with step titles in bold, and a lesson's explorer is headed with the lesson's name.

| Keys | Does |
|---|---|
| `]s` | Skips to the next step, in normal mode in a lesson's tab. |
| `[s` | Goes back to the step before. |
| `:q`, in either of a lesson's panes | Ends the lesson, closing both. |

The lesson can't be changed: a change to it is put back. Lessons work in a folder of their own, `clackatoo/tutor` in `$XDG_CACHE_HOME` (or `~/.cache`), which goes when the lesson ends; a lesson's explorer can't leave it. A file of yours opened in the practice pane is left as it is, unsaved changes and all, when the lesson ends. Where each lesson was left is kept in `clackatoo/tutor` in `$XDG_STATE_HOME` (or `~/.local/state`), so the next window picks up there. With `--remote`, the tutor says it needs the daemon on this machine.

### What's new

The first time a window of a new build opens, if the build has something new, the bottom bar says `clackatoo updated · :Tutor new` until the next key; a first install says `Welcome to clackatoo · :Tutor to start`. `:Tutor new` lists what's been added, newest first, those not listed before first and marked `new`; Enter opens the lesson that teaches it.

