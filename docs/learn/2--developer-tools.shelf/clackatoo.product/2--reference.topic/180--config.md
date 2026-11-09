---
title: Configuration
type: reference
description: The windows are configured in Lua, with an init.lua that runs once for every window, sets options, binds keys, listens for key presses and chats' turns ending, and splits, focuses and closes panes; the daemon has its own, in a sandbox, for its agents, its web app and this machine. Where the files go, how a key travels through handlers and bindings, how handlers are run, the options and :setlocal, the :Settings menu, the API so far, the daemon's config and its options, and what happens when the config goes wrong.
related:
  - clackatoo/reference/window
  - clackatoo/reference/cli
---

The window's behavior is configured in code: an `init.lua`, as Neovim's is,
written in [Luau](https://luau.org), a fast dialect of Lua with optional types
and a sandbox. Without one, the window works as it always does. Settings
needed before the window starts, such as the graphics API, stay in
[`startup.toml`](~clackatoo/reference/frame#settings), beside it. The font is
both: startup.toml's is what the window starts in, and `init.lua`'s outdoes
it.

```lua
-- ~/.config/clackatoo/client/init.lua

-- tmux's split keys, as well as clackatoo's own.
client.keys.map("niv", '<C-b>"', function(ev) ev.pane:split("below") end)
client.keys.map("niv", "<C-b>%", function(ev) ev.pane:split("right") end)

-- Closing a pane takes :close, not a key.
client.keys.unmap("niv", "<C-b>x")

-- The list of background tabs on Ctrl+B Space, rather than Ctrl+B b l.
client.keys.unmap("niv", "<C-b>bl")
client.keys.map("niv", "<C-b><Space>", function() client.cmd("Background") end,
  {desc = "background tabs"})

-- x in normal mode closes a chat, and does what it always did elsewhere.
client.keys.map("n", "x", function(ev)
  if ev.pane.kind ~= "chat" then return ev:propagate() end
  ev.pane:close()
end)
```

## Where it goes

```text
~/.config/clackatoo/        or $XDG_CONFIG_HOME/clackatoo/
  client/settings.lua       what :setpersist, :mappersist and :Settings saved, run before init.lua
  client/init.lua           the window's config
  client/lua/               modules for it to require
  client/startup.toml       the window's settings, needed before it starts
  daemon/settings.lua       the daemon's settings clackatoo saved, run before its init.lua
  daemon/init.lua           the daemon's config: its agents, this machine, the web app
  daemon/lua/               modules for it to require
  lua/                      modules shared by the window's config and the daemon's
```

`require("theme")` loads `theme.lua` from `client/lua/`, or else `lua/`, or
`theme/init.lua` from either. A dot in a name is a directory:
`require("keys.tmux")` is `keys/tmux.lua`. A name is letters, digits, `_` and
`-`, never a path, so nothing outside those folders is required, and a module
is read only as text, up to 1 MiB, never Luau's bytecode. Each module runs
once, and later requires get what it returned.

## Windows

The config runs once for the client, the process that opens clackatoo's
windows, not once for each window. Its bindings, handlers and options apply in
every window, and a window opened later goes by them as they are, without the
config running again, so state a config or a module keeps, such as a count, is
kept once. It runs on a thread of its own, alongside the first window starting
up, and a typical config takes under a millisecond. The window never waits for
it to show itself: keys pressed before it has run do what they do without it.

Each window is an object in it. `client.windows` lists them, and an event says
which window it happened in, as `ev.window`. A pane, tab or workspace is always
one window's, the one it was found through: two windows can be on two
machines' daemons, whose panes are numbered apart. So `ev.pane:split("right")`
splits in the window whose key it was, and `client.cmd` runs its command
there; while the config loads, in no window's handler, it runs in the window
with the focus.

A window opened in a process of its own, as with `CLACKATOO_WINDOW_HOST=off`,
or one that couldn't share the others' process, as with another graphics API,
runs the config for itself.

`:reload` runs the config again, from the start, for every window:
`settings.lua`, then `init.lua`. Everything the old config bound or listened
for goes, the window's own keys come back first, and options go back to how
they start, including those changed with `:set` and `:setlocal`.


## How a key travels

Each key you press goes through, in order:

1. **Keypress handlers**, added with `on("keypress", …)`, in the order they
   were added. A handler on a pane only hears keys pressed in that pane.
2. **Bindings**, the window's own and those made with `client.keys.map`.
3. **The pane**: the program in a terminal, the box in a chat, or the pane's
   normal mode.

A handler lets the key go on unless it calls `ev:stop()`, which ends its
journey there. A binding stops its keys unless it calls `ev:propagate()`,
which lets them go on to the pane, as if they weren't bound. If the binding
took several keys, such as `<C-b>x`, they all go on, in order.

Whether a key goes on is decided when the handler finishes, or first waits for
something, as `pane:split` waits for the new pane. Calling `stop` or
`propagate` after that is a mistake. A key waits at most 300 milliseconds for
the config to decide. After that, it's as if the config had done nothing more:
a binding's keys stop there, and a key a handler heard goes on.

A binding made with `{propagate = true}` decides at once, without the config:
its keys go on to the pane as soon as they're pressed, as if it had called
`ev:propagate()`, and its function runs after, so a slow one never holds them
up. Calling `ev:stop()` in it is a mistake.

```lua
-- u undoes at once, as it always does, and the config notes it after.
client.keys.map("n", "u", function(ev)
  print(`undid something in a {ev.pane.kind}`)
end, {propagate = true})
```

### How handlers are run

One config runs for every window, on one thread, so its handlers take turns.
The handlers a key waits for, those of `on("keypress")` and a binding's
function, run first, each until it finishes or first waits. Everything else is
background work: a handler going on once what it waited for has come, a
handler of an event nothing waits on, such as a chat's turn ending, and a
binding with `propagate`. Background work runs a slice of a couple of
milliseconds at a time, in turn, and a key that comes meanwhile is decided
first, so a busy handler in one window never holds up keys in another: a key
waits a fraction of a millisecond for it. A binding with `propagate` pressed
again before its function has started runs it once, for the latest keys. At
most 64 handlers wait their turn; past that, a new one is left out, and the
bottom bar says so.

A handler that calls `coroutine.yield()` itself lets other work go first, and
goes on after. A coroutine of its own, made with `coroutine.create` or
`coroutine.wrap`, is never ended for a slice: it runs on until it yields to
the handler, which is then. Nor can Luau end one inside a metamethod, or a
function called from C, such as `table.sort`'s comparator; there it runs on.

## Keys

`client.keys.map(modes, keys, function)` binds keys in one or more modes,
replacing what they were bound to. `client.keys.unmap(modes, keys)` takes a
binding away, the window's own included.

A binding can say what it does, as Neovim's `vim.keymap.set` takes it:
`client.keys.map("n", "<Space>ft", function() client.cmd("Files") end, {desc = "find todos"})`.
The panel of keys shows it in place of "from your config". `{propagate = true}`
lets its keys go on at once and runs the function after (see [how a key
travels](~clackatoo/reference/config#how-a-key-travels)). Neovim's `silent`,
`noremap`, `nowait`, `unique`, and `remap = false` are taken and change
nothing; any other option is left out, with a warning in the bar, and the
binding is still made. `client.keys.group(modes, keys, name)` names the keys
that start several, as which-key.nvim's groups do:
`client.keys.group("n", "<Space>f", "my files")` makes <kbd>Space</kbd>'s
panel say `f +my files`. A group with no name stops naming them. Keys that
finish a binding of their own say what that does instead.

Keys can be bound to other keys too, given as a string, as Vim's `noremap`
maps them: `client.keys.map("n", "gi", "gri")` makes `gi` go to
implementations, and `client.keys.map("n", "<Space>w", ":w<CR>")` writes the
file. The keys are typed as if pressed, so the window's own keys do what they
do, but the config's bindings and keypress handlers don't see them, so a
binding can't set off itself or another of the config's. They're typed after
any keys typed before them: a mapping to `@qx` types q's keys and then `x`,
and a key among them that fails stops the rest, as in Vim.

**Modes** are written as letters, together: `"niv"` is all three of normal,
insert and visual. `c` is typing into a pane's prompt, such as a search.

**Keys** are written as in Vim. A character is itself, and `<…>` holds a key
with modifiers or a named key:

| Written | Is |
|---|---|
| `<C-b>`, `<A-h>` | <kbd>Ctrl</kbd>+<kbd>B</kbd>, <kbd>Alt</kbd>+<kbd>H</kbd>. `<M-h>` is `<A-h>`. |
| `<C-O>`, `<C-S-o>` | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd>. <kbd>Shift</kbd> is part of a letter: `<C-o>` is <kbd>Ctrl</kbd>+<kbd>O</kbd>. |
| `<S-Tab>` | <kbd>Shift</kbd> with a named key |
| `<Esc>`, `<CR>`, `<Tab>`, `<BS>`, `<Del>`, `<Insert>`, `<Space>` | Named keys. `<Enter>` and `<Return>` are `<CR>`, and `<Escape>` is `<Esc>`. |
| `<Up>`, `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`, `<PageDown>`, `<F1>`–`<F12>` | More named keys |
| `<lt>` | `<` |

Several keys in a row, such as `<C-b>"`, make a sequence. A key that starts
one waits for the rest as long as it takes, as tmux's prefix does. If the next
key goes on to no binding, the keys typed so far go to the pane as if nothing
were bound to them, as Vim's do, so binding `gi` leaves `gg` as it was, and
the next key is looked up on its own. Keys that began with <kbd>Ctrl</kbd> or
<kbd>Alt</kbd> held, as `<C-b>` does, are the window's, and are dropped
instead. A binding whose keys start a longer binding's, such as `<C-b>` beside
`<C-b>x`, can't be reached.

`client.cmd(line)` runs a command, as if typed after `:`, so a key can do
anything a command can. To find files with `<Space>ff` rather than
`<Space>pf`:

```lua
client.keys.unmap("nv", "<Space>pf")
client.keys.map("n", "<Space>ff", function() client.cmd("Files") end)
```

`client.cmd("reload")` is refused: the config can't run itself again.

`client.forward(spec)` forwards a port as
[`:portforward`](~clackatoo/reference/connecting#forwarding-ports) does,
`client.forward(8080)` or `client.forward("3000:8080")`, in every window on
another machine, those opened later included. A window on this machine passes
it over silently; a spec it can't read is an error.

`client.connect(host)` has the daemon bring another machine's workspaces in,
as
[`:connect HOST`](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar)
does. Every window asks its own daemon, those opened later included, and asks
again whenever it connects, saying nothing unless it fails; the daemon keeps
its connections, so windows on the same one asking again leave it as it is.
`client.disconnect(host)` ends that connection.
`client.connect("server", {share = false})` keeps that machine from the
daemons connected to this one, as `:connect HOST --no-share` does.

## Options

Options are settings, as Vim's are, read and set by name through `client.opt`,
as through Neovim's `vim.opt`:

```lua
client.opt.relativenumber = false   -- absolute line numbers only
client.opt.numberwidth = 5
client.opt.colorcolumn = { 80, 120 }
print(client.opt.number)            -- true
print(client.opt.cc[1])             -- 80
```

A list, such as `colorcolumn`, reads as a table of numbers, and is set with
one, with a string as `:set` writes it, such as `"80,120"`, or with a number
for a list of one. `""` or `{}` sets none. Text, such as `showbreak`, is a
string. A number can have a fraction where the option takes one,
`client.opt.fontsize = 10.5`, and a number can be set as a string, as `:set`
writes it.

An option has one value every window shares, as Vim's global options have:
`client.opt`, `:set` and
[`:setpersist`](~clackatoo/reference/config#saving-options-settings-lua) change
it for every window at once. A few can also be set for one window alone, with
[`:setlocal`](~clackatoo/reference/config#one-window-s-own-setlocal).

`:set` changes them as you go, as in Vim, and a config reads what it sets:

| Written | Does |
|---|---|
| `:set number`, `:set nonumber` | Turns an option on or off. |
| `:set number!`, `:set invnumber` | Turns it over. |
| `:set number?` | Shows it in the bottom bar. The name alone does too, for an option that isn't on or off. |
| `:set numberwidth=5` | Sets a number. |
| `:set colorcolumn=80,120` | Sets a list, with commas between its numbers. `:set cc=` sets none. |
| `:set showbreak=↪\ ` | Sets text; `\ ` is a space and `\\` a backslash. |
| `:set number&` | Puts it back as it starts. |
| `:set` | Shows the options changed from how they start. |
| `:verbose set number?` | Shows it, and [where it was set](~clackatoo/reference/config#where-an-option-was-set). |

Several can go after one `:set`, such as `:set nu rnu`. Each option has a
short name, as in Vim, which works everywhere the long one does.

### Font

| Option | Short | Is | Starts |
|---|---|---|---|
| `font` | `gfn` | The font family terminals and editors are drawn in, as `fc-list : family` names it, such as `JetBrains Mono`. Empty is your system's monospace font. A family that isn't installed is refused, and the font stays as it was. `--font` or `CLACKATOO_FONT` outdoes what `init.lua` sets as it runs, but not `:set` or what your keys set afterward. | your system's |
| `fontsize` | `fsz` | The size text is drawn at, in points, from 4 to 200, such as `12` or `10.5`. The window keeps its size, and the panes fit as many cells as now fit, as when it's resized. `--font-size` or `CLACKATOO_FONT_SIZE` outdoes what `init.lua` sets as it runs. | 12 |

### Editors

| Option | Short | Is | Starts |
|---|---|---|---|
| `number` | `nu` | Line numbers down an [editor](~clackatoo/reference/editor-view#line-numbers)'s left, the cursor's line showing its own. | on |
| `relativenumber` | `rnu` | The other lines show how far they are from the cursor's, for counts such as `5j`. Without `number`, the cursor's line shows 0. | on |
| `numberwidth` | `nuw` | The fewest columns the line numbers take, with the space after them, from 1 to 20. | 4 |
| `cursorline` | `cul` | The cursor's line is lit faintly across an editor, so the eye finds it. | on |
| `colorcolumn` | `cc` | Columns marked down an editor, such as `80`, to keep lines short by. A list, counting from 1, as the bottom bar's column does, up to 9999. | none |
| `virtualtext` | `vt` | What a [language server](~clackatoo/reference/languages#language-servers) says is wrong with a line, written dimly after its end: the worst, if there are several. | on |
| `gitsigns` | `gs` | The column of [git signs](~clackatoo/reference/editor-view#git-signs) left of the line numbers. While it's on, it's there for every file, so the text doesn't move when the first change comes. | on |
| `agentsigns` | `as` | A column right of git's signs marking the lines an [agent changed](~clackatoo/reference/editor-view#agents-changes) that you haven't reviewed, in the chat's color. It shows only while a file has such lines, or `:blame` is on. | on |
| `wrap` | `wrap` | A line too long for an editor's pane goes on over as many rows as it needs. Off, the view scrolls sideways to follow the cursor. | on |
| `linebreak` | `lbr` | Wrapped lines break between words, not in them. | on |
| `breakindent` | `bri` | Wrapped rows start as far in as their line is indented. | on |
| `showbreak` | `sbr` | Text drawn at the start of each wrapped row. | none |
| `scrolloff` | `so` | The fewest lines kept in view above and below an editor's cursor. Half the pane or more keeps the cursor's line in the middle. | 5 |
| `markdownview` | `mdv` | How a Markdown file opens in an editor: as its text, `raw`, or [rendered](~clackatoo/reference/editor-view#rendered-markdown), as prose, `rendered`. `:markdown` and `<Space>tm` turn each editor from one to the other. | `raw` |

### Editing

| Option | Short | Is | Starts |
|---|---|---|---|
| `tabstop` | `ts` | How wide a tab is drawn: to the next column that's a multiple of it, from 1 to 32. | 4 |
| `shiftwidth` | `sw` | The columns `>>` and `<<` [indent](~clackatoo/reference/editor-view#tabs-and-indenting) by, as does <kbd>Tab</kbd> where it indents with spaces, up to 32. 0 means `tabstop`'s, so the two can't drift apart. | 0 |
| `expandtab` | `et` | Indents with spaces, not tabs, where no line nearby is indented yet. A file keeps the kind of indent it has. | on |
| `smartindent` | `si` | A new line, and one where `}`, `]` or `)` is typed first or `:` ends `else` and the like, is [indented](~clackatoo/reference/editor-view#tabs-and-indenting) as its language's tree-sitter rules say, not Vim's rules for C, a moment after it's given the line before's indent. Off, it keeps the line before's. `=` indents by the rules either way. | on |
| `autoread` | `ar` | A file changed on disk by something other than an agent is read again if it has no changes not written, as an edit `u` undoes. Off, or with changes, the bottom bar says so. | on |
| `formatonsave` | `fos` | `:w` formats the file first, as its [language server](~clackatoo/reference/languages#language-servers) does, then writes it. If the server can't format, or takes over 3 seconds, the file is written as it is, and the bottom bar says why. | off |
| `tildeop` | `top` | `~` takes a motion, as `g~` does. | off |
| `startofline` | `sol` | These go to the line's first character that isn't blank, as Vim's do: `gg`, `G` and <kbd>Ctrl</kbd>+<kbd>Home</kbd>, `H`, `M` and `L`, <kbd>Ctrl</kbd>+<kbd>D</kbd>, <kbd>Ctrl</kbd>+<kbd>U</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd> and <kbd>Page Up</kbd>, in editors and terminals; and in an editor, `:` and a line number, and deleting, shifting or indenting whole lines (`dd`, `dj`, `dG`, `>>`, `<<`, `==`, and their kin). Off, as Neovim's are, they keep the column, as `j` and `k` do. Yanks and <kbd>Ctrl</kbd>+<kbd>E</kbd> and <kbd>Ctrl</kbd>+<kbd>Y</kbd> keep it either way, and `:d` goes to the first non-blank either way. | off |

### Searching

| Option | Short | Is | Starts |
|---|---|---|---|
| `ignorecase` | `ic` | Searches ignore case, in editors, in a terminal's normal mode, in a chat's and in lists'. An editor ignores the case of plain English letters only: `é` doesn't match `É`. | on |
| `smartcase` | `scs` | With `ignorecase`, a search with a capital letter in it matches case, in editors, terminals, chats and lists. Does nothing without `ignorecase`, as in Vim. | on |
| `hlsearch` | `hls` | Every match of the last search in view is lit up, in editors, in a terminal's normal mode, in chats and in lists, the one the search went to brighter than the rest, as Neovim's are: after `/`, `?`, `n` and `N`, and in editors `*`, `#`, `:s` and `:g` too, until <kbd>Esc</kbd> or `:nohlsearch`. Off, only the match the search went to is lit. | on |
| `incsearch` | `is` | While `/` or `?` is typed, in editors, a terminal's normal mode, chats and lists (explorers, boards, and reviews), the cursor and the view go to the match the search would go to from where it started, and its matches are lit, as Neovim's are. <kbd>Esc</kbd> puts them back; <kbd>Enter</kbd> keeps them, and is the search. Off, nothing moves until <kbd>Enter</kbd>. | on |
| `inccommand` | `icm` | Shows what `:s` would do while it's typed: `nosplit`, in place, or `none`. | `nosplit` |
| `hintpattern` | `hpat` | A regular expression whose matches [hints](~clackatoo/reference/workspaces#hints) label too, as `CLACKA-\d+`. One that can't be read is said in the bar as hints start. | none |

### Completion and language servers

| Option | Short | Is | Starts |
|---|---|---|---|
| `completion` | `cmp` | Asks the language server what could finish a word as you type it. Off, <kbd>Ctrl</kbd>+<kbd>Space</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd>, and <kbd>Ctrl</kbd>+<kbd>P</kbd> still ask. | on |
| `completedelay` | `cmpd` | How long typing pauses, up to 2000 milliseconds, before completion asks, so a burst of keys asks once. 0 asks at every key. | 40 |
| `completeaccept` | `cmpa` | What <kbd>Enter</kbd> does with the menu open: accept the item you moved to (`picked`), always accept (`enter`), or never (`ctrl-y`), leaving it to <kbd>Ctrl</kbd>+<kbd>Y</kbd>. | `picked` |
| `completetab` | `cmpt` | <kbd>Tab</kbd> accepts the picked item too. | off |
| `signaturehelp` | `sig` | Shows the signature of the call you're typing the arguments of. | on |
| `highlightuses` | `hlu` | Lights the other uses of the name the cursor rests on, as the [language server](~clackatoo/reference/languages#language-servers) says; where it's given a value a little brighter. | on |
| `inlayhints` | `ih` | Writes what the [language server](~clackatoo/reference/languages#language-servers) says between the code: names' types and arguments' parameter names, dim, never part of the text. `<Space>th` toggles it. | on |
| `languageinstall` | `li` | What happens when a file is opened in a language, or with a language server, that isn't installed but [can be](~clackatoo/reference/languages#languages): `ask` in the bottom bar, install it `always` without asking, or `never` offer. | `ask` |

### The clipboard

| Option | Short | Is | Starts |
|---|---|---|---|
| `clipboard` | `cb` | Which of the desktop's the unnamed register goes along with, for what's yanked or deleted in an editor and yanked in a terminal or a chat: the clipboard (`unnamedplus`), the primary selection (`unnamed`), or neither (`none`), keeping it the window's own but for `"+` and `"*`. | `unnamedplus` |
| `osc52` | `osc52` | What programs in a terminal can do with the clipboard through OSC 52: `copy`, `paste`, `copy-paste`, or `off`. What they copy goes to `"+` and `"*` too. Reading is off unless allowed, since whatever runs in the terminal would get the clipboard. Only the window that last typed into the terminal acts, and text is capped at 1 MiB. | `copy` |
| `pasteimage` | `pi` | How <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> [pastes an image](~clackatoo/reference/typing#pasting-an-image) into a terminal: `path`, kept in a file on the terminal's machine whose path is pasted, or `clipboard`, put on that machine's clipboard and <kbd>Ctrl</kbd>+<kbd>V</kbd> typed, which needs a desktop session there. A chat attaches the image either way. | `path` |

### Terminals

| Option | Short | Is | Starts |
|---|---|---|---|
| `blockmarks` | `bm` | Which finished commands a terminal marks at the end of their prompt's line, where the shell says how they ended: those that failed, with a red `✗` and their exit status (`failed`), every one, with a faint `✓` for those that worked (`all`), or none (`none`). | `failed` |
| `predictecho` | `pe` | Shows what's typed in a terminal at once, underlined, before its echo comes back: `auto` for a daemon on another machine whose round trip is over 30 ms or has stalled moments before, `always`, or `never`. | `auto` |

### Agents

| Option | Short | Is | Starts |
|---|---|---|---|
| `agent` | `ag` | The agent `:Agent` and <kbd>Ctrl</kbd>+<kbd>B</kbd> `a` open a chat with: `claude`, `codex`, `gemini`, or one the [daemon's config](~clackatoo/reference/agent-settings#which-agent) adds. Empty leaves it to the daemon. | `claude` |
| `agenttools` | `agt` | What a new chat's agent can do with [clackatoo's tools](~clackatoo/reference/agent-tools#what-the-agent-can-show-you): `ask` before running commands or typing, `allow` commands in its own terminals, or `off`. | `ask` |
| `claudetask` | `ct` | A new Claude Code chat keeps Claude Code's own Task tool, which runs helpers inside one turn, folded under the Task's [card](~clackatoo/reference/chat-view#claude-code-s-own-subagents). Off, Claude hands work on only as [subagents](~clackatoo/reference/chats-together#subagents), which you can watch. Other agents have no such tool. | off |
| `chatbrowser` | `cbr` | A new chat starts with its [Browser](~clackatoo/reference/agent-tools#the-browser) setting on, so its agent can drive a headless browser with a profile of its own. Off, a chat has none until you turn it on in its settings. | off |
| `chatcost` | `chc` | Under each chat turn, what it spent, its tokens and, where the agent says, its cost; and what the session cost, in the chat's usage detail (see [what else a turn says](~clackatoo/reference/chat-view#what-else-a-turn-says)). | on |
| `carryover` | `co` | About the most of a chat's conversation, in tokens, that an agent the chat moves to hears, from 1000 to 50000, but never more than a quarter of its context. | 20000 |
| `notify` | `ntf` | Which chats you can't see get a [desktop notification](~clackatoo/reference/chat-view#desktop-notifications): `all` (those that finish, fail, or ask something), `questions`, or `off`. A program's own [notifications](~clackatoo/reference/terminal-emulation#notifications) show unless it's `off`. | `all` |
| `rewindfiles` | `rwf` | What [rewinding a chat](~clackatoo/reference/agent-changes#rewinding-and-branches), or switching its branch, does to the files the turns it takes out changed: `ask` each time, `undo` them, or `keep` them. `:rewind!` undoes them without asking. | `ask` |

### The window

| Option | Short | Is | Starts |
|---|---|---|---|
| `smoothscroll` | `sms` | Ease mouse-wheel scrolling in native panes. Trackpad pixels stay direct; keyboard motions and jumps stay immediate. | on |
| `whichkeydelay` | `wkd` | How long, up to 5000 milliseconds, keys that start others, as <kbd>Space</kbd> does, wait before a panel lists what can come next, so keys typed quickly never flash it. Keys held with <kbd>Ctrl</kbd> or <kbd>Alt</kbd>, as <kbd>Ctrl</kbd>+<kbd>B</kbd>, show it at once; 0 shows it at once for every key. | 400 |
| `ime` | `ime` | Type through the desktop's [input method](~clackatoo/reference/typing#typing), as Chinese, Japanese and Korean are typed and as an emoji picker sends its emoji, while keys type text. Off, keys go straight to the pane, in case the input method gets in the way. | on |
| `explorerwidth` | `ew` | How wide a [file explorer](~clackatoo/reference/explorer) opens, in columns: 0 is 30, or a quarter of the tab if that's narrower; otherwise from 10 to 200. | 0 |
| `explorerhidden` | `eh` | File explorers show hidden files when they open. | off |
| `updatecheck` | `uc` | Look for a new release at most once a day, a while after the window opens, and say so in the bar. It sends one request to api.github.com, saying the version. | on |
| `renderer` | — | The graphics API windows draw with: `vulkan`, fastest where it works well, or `opengl`, which on some machines with two GPUs opens a window much sooner. It's read from [`startup.toml`](~clackatoo/reference/frame#settings) as the window process starts, so `:set` can't change it: `:setpersist renderer=opengl` saves it for windows opened after. | `vulkan` |
| `priority` | — | Whether windows [run ahead](~clackatoo/reference/cli#staying-quick-on-a-busy-machine) of your other programs (`high`) or as they do (`normal`). Read from `startup.toml` as the window process starts, so only `:setpersist` changes it, for windows opened after. | `high` |

### Other machines

| Option | Short | Is | Starts |
|---|---|---|---|
| `showhost` | `shh` | When a workspace from another machine shows its machine after its name, as `notes@desk`: only while another workspace has the same name (`ambiguous`), always (`always`), or never (`never`). | `ambiguous` |
| `forwardpace` | `fp` | [Forwarded ports](~clackatoo/reference/connecting#forwarding-ports) hold themselves back on a slow link so keys don't wait behind them. Off, they take the whole link. What the daemon sends changes for connections made after it's changed; what the window sends changes at once. | on |

Setting an option that doesn't exist, or to a value it can't hold, such as
`client.opt.number = 1`, is a mistake. `renderer` and `priority` are read only
as a window opens, so `client.opt` reads them but can't set them, and neither
can `:set`.

### One window's own: setlocal

`:setlocal`, or `:setl`, sets an option for this window alone, over the value
every window shares, as Vim's `:setlocal` does for its window-local options.
It takes what `:set` takes. These can be set so:

`font`, `fontsize`, `number`, `relativenumber`, `numberwidth`, `cursorline`,
`colorcolumn`, `wrap`, `linebreak`, `breakindent`, `showbreak`, `scrolloff`

| Written | Does |
|---|---|
| `:setlocal nowrap` | Turns wrapping off in this window, leaving the others as they are. |
| `:setlocal wrap<` | Takes this window's own value away, so it has every window's again. `:set wrap<` does too. |
| `:setlocal wrap&` | Sets this window's own to how it starts. |
| `:setlocal` | Shows this window's own values. |
| `:verbose setlocal wrap?` | Shows it, and says whether it's this window's own. |

`:set` in a window sets the value every window shares, and takes that window's
own value of it away, so it shows what was set; other windows keep theirs. So
does setting `client.opt` in a handler of a key pressed in the window, once
the config has loaded. While the config loads, and in a handler of a chat's
turn ending, which is every window's, it sets only the value every window
shares.
`:setlocal` values aren't saved: `:setpersist` saves the value every window
shares, and a reload takes them away.

The font a window is opened with, `--font` and `--font-size` or
`CLACKATOO_FONT` and `CLACKATOO_FONT_SIZE`, is its own in the same way: the
config setting the font as it loads doesn't change it, but `:set` in that window
does. A reload, and `:set font&`, put it back.

`client.opt` reads the value every window shares while the config loads, and
in a handler of a chat's turn ending; in a handler of a key, it reads the
window's the key was pressed in, its own if it has one, as Neovim's `vim.o`
reads the current window's.

### Saving options: settings.lua

`:setpersist`, or `:setp`, is `:set` that also saves what it sets, so it's set
again after a `:reload`, and whenever clackatoo's windows open again. Like
`:set`, it changes every window open at once. It takes what `:set` takes, and
Tab completes the same:

```text
:setp nowrap colorcolumn=80,120
```

Most options are saved in `client/settings.lua`, beside `init.lua`. The font,
its size, `renderer` and `priority` are saved in
[`startup.toml`](~clackatoo/reference/frame#settings) instead, as the window
needs them before any Lua runs, keeping the rest of that file, comments
included. `:setp number&` puts an option back as it starts and takes it out of
the file.

clackatoo writes `settings.lua`, and says so at its top. It holds a line for
each option saved, set as your config would set it:

```lua
-- Written by clackatoo, whose :setpersist and settings menu change it.
-- Your own config goes in init.lua, which runs after this and outdoes it.

client.opt.wrap = false
client.opt.colorcolumn = { 80, 120 }
```

It runs before `init.lua`, so anything `init.lua` sets outdoes it. An option
your `init.lua` sets as it loads, or a module it requires, isn't saved: `:setp`
sets it for now, and the bottom bar says which line of yours sets it, such as
`client/init.lua:12 sets wrap, so it's set for now and not saved`. Change that
line instead, or take it out.

Each save reads the file again and changes only the saved option's line, so
saving from one window never undoes what another saved, and everything else
stays as it was: blank lines, comments, a comment after a value
(`client.opt.wrap = false -- too narrow here`), and options this build doesn't
know, saved by a newer one. An option saved for the first time goes among the
options at the end of the file, in the order they're listed here. A block
comment, `--[[ … ]]`, is kept whole, so an option commented out in one stays
commented out, and saving it adds a line of its own. The file is replaced all
at once, never left half written, off the window's thread, keeping its
permissions; a new one is readable only by you. If `settings.lua` or
`startup.toml` is a link, as stow and home-manager make, the file it links to
is changed and the link kept; the bottom bar says if its folder can't be
written in.

If the file has a line clackatoo didn't write, such as code, or a block comment
it can't follow, as one that doesn't end, nothing is saved, and the bottom bar
names the line: move it to `init.lua`. A line `settings.lua` can't set, such as
an option a newer build saved, is said in the bottom bar, and the rest are set
all the same.

### Where an option was set

`:verbose set` shows what last set each option it shows, as Vim's does:

```text
:verbose set wrap? cc? nu?
nowrap · set in client/init.lua:4  colorcolumn=80,120 · saved in client/settings.lua:5  number · as it starts
```

An option can be as it starts; set as the window opened, by `startup.toml` or
the command line; saved in `settings.lua`; set in your config as it loaded,
with the line, and the line of a module's helper that really set it if that's
elsewhere, as `client/init.lua:3 (in lua/opts.lua:8)`; set by your config after
it loaded, by a handler or a binding; or set with `:set` or a key. A `:set`
your config runs, with `client.cmd`, is your config's, at its line. Your config
is loading until each file has run to its end, even one that waits partway, as
`pane:split` waits. An option set in a coroutine of your own, one made with
`coroutine.create` or `coroutine.wrap`, is said to be set at a line of that
coroutine's, as only its lines can be seen from there.

## The settings menu

`:Settings` opens a menu of every option and every key bound, over the panes,
as the finder is. `:Settings wrap` opens it searching for `wrap`. Down its
left are its sections: the options' groups, as they're listed here, then
**Keys**, then **Daemon**, where the daemon's own settings will go. Each row
is an option and its value, its name in bold if it isn't as it starts; beside
the list, the row picked is said in full: its value, its default, what it
takes, where its value comes from, as [`:verbose set`](~clackatoo/reference/config#where-an-option-was-set)
says it, and the file it's saved in.

| Key | Does |
|---|---|
| `j`, `k`; <kbd>Down</kbd>, <kbd>Up</kbd> | The next or previous row |
| `gg`, `G`; <kbd>Home</kbd>, <kbd>End</kbd> | The first row or the last |
| <kbd>Tab</kbd>, <kbd>Shift</kbd>+<kbd>Tab</kbd>; `}`, `{` | The next section, or the one before |
| `/` | Searches the names, short names and what each does, as the finder matches; <kbd>Enter</kbd> stops typing and keeps what's found |
| <kbd>Enter</kbd>, <kbd>Space</kbd> | Changes the row: turns it on or off, picks the next choice, types a value, picks a font, or records keys |
| `l`, `h`; <kbd>Right</kbd>, <kbd>Left</kbd> | The next or previous value: on or off, a choice, or a number up or down |
| `r` | Puts the option back as it starts, and out of its file |
| `u` | Undoes the row's last change, one at a time, even after the menu is closed and opened again |
| `x` | Takes away this window's own value, as `:setlocal nu<` does |
| `d` | Takes keys the menu saved out of `settings.lua` |
| `o` | Opens the line of config that set the row |
| <kbd>Esc</kbd>, `q` | Clears the search, or else closes the menu |

A change applies at once, in every window, as `:set` does, and is saved where
[`:setpersist`](~clackatoo/reference/config#saving-options-settings-lua) saves
it: in `settings.lua`, or for the font, its size, `renderer` and `priority`,
in [`startup.toml`](~clackatoo/reference/frame#settings), which takes effect
for windows opened after; the font and its size change in this one too.
`renderer` and `priority` are only saved, for windows opened after.

The font picker lists the monospace font families installed, those whose
font says so or whose letters are all as wide, with the system's monospace
font first, and the font in use whatever it is. <kbd>Tab</kbd> lists every
family, as `fc-list : family` does, or only the monospace ones again. A value
typed, as for
`colorcolumn` or `showbreak`, is written as `:set` takes it after `=`.

**Locked.** An option your `init.lua` sets as it loads is locked: it would
outdo anything the menu saved, so the menu says where, as `init.lua:12` at the
row's end, and changes nothing. <kbd>Enter</kbd> on it opens that line in an
editor beside the focused pane, by the daemon on this machine, where your
config is. If the window's daemon is on another machine, the bottom bar says
so instead, and nothing opens there.

**Configured and current.** An option changed since your config loaded, with
`:set` or a key, says both: "configured 4, currently 7", and `not saved` at
the row's end. Changing it in the menu changes what it's configured to as
well.

**This window's own.** An option this window has a value of its own for, from
[`:setlocal`](~clackatoo/reference/config#one-window-s-own-setlocal), says so
at the row's end, and `x` takes it away.

### Keys

The **Keys** section lists every key bound, in every mode, those bound alike
in several as one row, with what each does and what bound it: as the window
starts, `settings.lua`, or a line of your config. <kbd>Enter</kbd> on one
records more keys to do the same: press them, then <kbd>Enter</kbd>.
<kbd>Backspace</kbd> takes the last one back, and <kbd>Esc</kbd> gives up.
They're bound at once, and saved in `settings.lua` as keys bound to the
binding's keys, in the same modes:

```lua
client.keys.map("niv", "<A-t>", "<C-b>c", { desc = "new tab" })
```

Keys your config binds as it loads are locked, as options are, and so are
keys it binds to a function, which `settings.lua` can't hold: bind more keys
to those in `init.lua`. `d` takes keys the menu saved out of `settings.lua`,
in the row's modes, and puts back what the window binds them to as it starts,
if anything; the bottom bar says if `settings.lua` no longer binds them. A
row is keys bound alike in several modes by the same thing, so keys saved for
some modes and bound otherwise in others are rows of their own. `u` after
recording keys over ones saved before binds those again.

`:mappersist`, or `:mapp`, does the same from the command line: modes, keys,
and the keys to type, the rest of the line, as Vim's `:map` takes them:

```text
:mappersist niv <A-t> <C-b>c
```

It's refused for keys your `init.lua` binds as it loads, and the bottom bar
names the line.

Bindings in `settings.lua` go at the end of the file, after its options.
Saving one changes only the lines binding its keys, and those only in its
modes: a line binding them in the same modes is changed where it is, keeping
its comment; a line binding them in other modes too keeps those, as
`client.keys.map("niv", …)` taken out in `iv` is left `"n"`. So the last line
binding the keys in each mode, which is what counts, is always the one saved
last.

The menu is the window's own: options are the window process's, not the
daemon's, so the [web app](~clackatoo/reference/phone) doesn't list them.

## Events and handles

`client.on("keypress", function)` listens for keys anywhere. Windows, tabs and
panes have `:on` too, to listen only there. Each returns a subscription, whose
`:off()` stops it. A pane's or tab's handlers go when it closes, and a
window's when it closes.

`client.on("turnend", function)` hears a chat's turn end: its agent finished,
or failed. It's heard once, however many windows show that daemon's chats, so
a config can count turns or notify once. It can't be stopped, and only
`client.on` hears it, as a chat's turn ends for every window. Its handler gets
`ev.chat`, the chat's number, `ev.title`, `ev.agent` (as "Claude Code"),
`ev.failed` (`true` or `false`, or `nil` from an older daemon that doesn't say),
and `ev.window` and `ev.pane`, the window that heard it and the pane showing
the chat there, if one does.

```lua
client.on("turnend", function(ev)
  if ev.failed then print(`{ev.title} failed`) end
end)
```

A handler gets the event, `ev`:

| Field | Is |
|---|---|
| `ev.key` | The key, as a binding writes it, such as `<C-b>` or `q` |
| `ev.keys` | For a binding, all of its keys |
| `ev.text` | The text the key types, if any |
| `ev.mode` | The mode's letter |
| `ev.count` | For a binding, the count typed before its keys, 0 if none, as Vim's `v:count` |
| `ev.count1` | The same, but 1 if none, as `v:count1` |
| `ev.pane`, `ev.window` | Where it was pressed |

These are a key's. A chat's turn ending has its own, above.

A binding that lets its keys go on passes the count on with them, so `5x`
bound to a function that calls `ev:propagate()` still deletes five.

Windows, workspaces, tabs, and panes are handles: values that stand for one of
them, whose fields are read as they are at the time. Reading a pane that has
closed is a mistake. Two handles to the same pane, found through the same
window, are equal.

| | Fields | Methods |
|---|---|---|
| `client` | `windows`, `focused` (the focused pane of the window with the focus), `opt` | `on`, `keys.map`, `keys.unmap`, `keys.group`, `cmd`, `forward`, `connect`, `disconnect` |
| A window | `workspace`, `tab`, `pane` | `on` |
| A workspace | `name`, `tabs` | `on` |
| A tab | `name`, `panes`, `focused`, `workspace` | `on` |
| A pane | `kind` (`"terminal"`, `"chat"`, `"editor"`, `"explorer"`, `"folder"`, `"board"`, `"review"`, `"browser"`, or `"other"` for a kind this window doesn't know), `title`, `tab` | `on`, `split`, `close`, `focus` |

`pane:split(side)` puts a new pane on that side, `"right"`, `"left"`,
`"below"` or `"above"`, of the same kind, or another with `{ kind = "chat" }`
or `{ kind = "terminal" }`. Those are the only kinds the config can make, so
splitting any other pane, such as an [editor](~clackatoo/reference/editor),
needs one. It waits for the daemon to make it, then focuses it, as
<kbd>Ctrl</kbd>+<kbd>B</kbd> `|` does, and returns it:

```lua
client.keys.map("n", "<C-b>t", function(ev)
  local below = ev.pane:split("below")
  below:split("right")
end)
```

While it waits, the window carries on, and so does the key. Each handler and
binding runs in a coroutine, a function that can pause partway and carry on
later, so waiting holds up nothing else.

Nothing is shown while `init.lua` first runs, so `client.focused` is nil then:
handles are for handlers. The first window is in `client.windows` already, to
listen to with `client.windows[1]:on`.

## The daemon's config

The daemon, which runs terminals, agents and the web app for every window, has
a config of its own, also in Luau: `daemon/settings.lua`, which clackatoo
writes, then `daemon/init.lua`, your own, which runs after it and outdoes it.
Each sets settings through `daemon`, a table for each group of them:

```lua
-- ~/.config/clackatoo/daemon/init.lua

daemon.machine.name = "jack-desktop"
daemon.web.mode = "tailscale"
daemon.history.lines = 20000

-- An agent of your own, and Claude Code run another way.
daemon.agents.opencode = { name = "OpenCode", command = { "opencode", "acp" } }
daemon.agents.claude = { command = { "claude-agent-acp" }, tasks = false }

-- A profile for chats agents start.
daemon.profiles.review = {
  about = "Reviews a change for bugs",
  agent = "codex",
  model = { codex = "gpt-6-luna", claude = "haiku" },
  limits = "read-only",
}
```

A setting reads back what it is, the default until something sets it, so
`print(daemon.history.lines)` prints `10000`, and `nil` puts it back as it
starts. `daemon.agents` and `daemon.profiles` are tables by name, so
`daemon.agents.mine.tasks = false` changes one set above, `= nil` takes one
out, and `for name, agent in daemon.agents do … end` goes through them
(`pairs` finds none, as the table only stands for them). A name that isn't a
word is written `daemon.agents["my agent"]`. What an agent and a profile can
have is in
[Which agent](~clackatoo/reference/agent-settings#which-agent) and
[Profiles](~clackatoo/reference/chats-together#profiles).

`require` loads modules from `daemon/lua/`, or else the `lua/` shared with the
window's config, by a name of letters, digits, `_` and `-`, a dot being a
folder, as the window's does. Where an option was set, the file and line, is
kept, as for the window's.

### When it runs

The daemon runs its config as it starts, and each time it
[upgrades](~clackatoo/reference/cli#upgrade), on a thread of its
own, before it does anything else; a config of a few dozen lines takes about a
millisecond. A change takes effect when the daemon next starts: `clackatoo
upgrade`, which keeps every terminal and agent running, runs it again at once.

It runs in a sandbox. There's no `io` or `os`, nothing that loads code but
`require`, and `require` reaches only those two folders. Luau's bytecode is
never run, only text. A run may take half a second and 64 MB, or it's stopped
where it is, and stays stopped, so a `pcall` can't carry it on: a loop that
never ends can't hold up the daemon. As the daemon upgrades, its terminals
waiting, a run may take a fifth of a second. The build an upgrade is to is
asked first whether agents go on through it; it answers from what last ran,
never running your `init.lua` for it.

### When it goes wrong

A mistake never stops the daemon. A value a setting can't hold, a name that
isn't a setting, a Lua error, or a run stopped for taking too long, stops the
config there, and none of what it set is used. Instead the daemon keeps what
the config set the last time it ran without a mistake, kept in
`daemon-config.lua` in clackatoo's state directory (`~/.local/state/clackatoo`).
With none kept, as when no config has run yet, it goes by its defaults, but
with terminals' history and the web app off until one has. It writes why in
its log, and the first window you're at says so in its bar, with the file and
line of the mistake that stopped it:

```text
the daemon's config went wrong, so it kept its settings from the last time it ran: daemon/init.lua:4: daemon.history.lines is a whole number from 1 to 1000000
```

Some settings are checked together, once the config has run, at the line that
set the one that can't be: `daemon.subagents.default` more than
`daemon.subagents.working`, `daemon.images.terminal_mb` more than
`daemon.images.total_mb`, or a [web app](~clackatoo/reference/phone) set to
`public` without its certificate, key and origin.

### Moved from TOML

The daemon was set before in `daemon/agents.toml`, `machine.toml` and
`web.toml`. The first time a daemon with this config starts or upgrades, and
finds them there and no `settings.lua`, it moves every value they set into
`settings.lua`, a line each, with their comments, and keeps the old files
beside it as `agents.toml.bak` and so on. A value the config can't hold, or a
key that was never a setting, is written commented out, saying why, so the
rest still runs. A field an agent or profile never had is left out alone,
keeping the agent, as before. If any of `web.toml` is left out, so is its
`mode`: the web app stays off until it's put right, as it did when `web.toml`
went wrong. Every comment written is a line comment of its own, with nothing
in it that could end it or open a block comment.

It goes in order, each step only once the one before has worked: the web
app's password to the state directory (see
[the phone](~clackatoo/reference/phone)); then `settings.lua`, once it
compiles; then the old files to `.bak`, `web.toml`'s without its password, so
no password stays in the config folder. If a step fails, the old files stay
as they are and are read as they would have been, and the first window you're
at says why. When it works, that window says what was moved, and what was left
out.

That happens once. A TOML file there after it is passed over: the daemon's log
and the first window you're at say so, and it can be deleted.

### What outdoes it

As for the window, the environment outdoes the config, for tests and CI:
`CLACKATOO_HISTORY` (a folder, or `off`), `CLACKATOO_HISTORY_LINES`,
`CLACKATOO_HISTORY_EVERY` and `CLACKATOO_IMAGE_MEMORY` (as `64,256`) over the
settings below, and `CLACKATOO_AGENT` over the command of the agent a chat
gets when it asks for none. `CLACKATOO_CONFIG` names a folder to take the
config from in place of `~/.config/clackatoo`, keeping the web app's password
and the last settings that ran in `state` in it, never in your own state
directory; or it's `off` for none, as tests' daemons run.

### The daemon's name

`daemon.machine.name` names this machine's daemon, as other machines show it.
Without it, the daemon takes the machine's hostname.

`daemon.machine.priority = false` has the daemon run as other programs do,
rather than
[ahead of them](~clackatoo/reference/cli#staying-quick-on-a-busy-machine).

Each daemon also has an ID of its own, chosen the first time it starts and
kept in `machine` in clackatoo's state directory (`~/.local/state/clackatoo`),
or in `$CLACKATOO_MACHINE`. Deleting that file makes the daemon a new machine
to every other; copying it to another machine makes the two refuse each other
until one of them deletes it. `CLACKATOO_MACHINE=off` chooses a new ID at
every start, as tests do.

## The daemon's options

Each is set as `daemon.` and its name, such as `daemon.web.mode = "public"`,
and read as the daemon starts or upgrades.

### This machine

| Option | Is | Starts |
|---|---|---|
| `daemon.machine.name` | The name other machines show this one by, as [connections](~clackatoo/reference/connections) list them. | none |
| `daemon.machine.priority` | The daemon takes a higher priority and a place of its own, [ahead of other programs](~clackatoo/reference/cli#staying-quick-on-a-busy-machine). | on |

### Agents

| Option | Is | Starts |
|---|---|---|
| `daemon.upgrade.keep_agents` | Agents go on through an upgrade, with what they run, rather than being stopped and started again (see [agents through upgrades](~clackatoo/reference/agents-lasting)). | on |

### Chats agents start

How far [chats agents start](~clackatoo/reference/chats-together#subagent-limits)
may go.

| Option | Is | Starts |
|---|---|---|
| `daemon.subagents.working` | The most chats agents started that may work at once, in all, from 1 to 64. | 16 |
| `daemon.subagents.default` | A new chat's Subagents setting, from 0 (off) to `working`. Unless it's set, it's no more than `working`. | 6 |
| `daemon.subagents.open` | The most chats agents started open under one of yours, from 1 to 256. | 32 |
| `daemon.subagents.started` | The most chats agents start under one of yours between two of your messages there, from 1 to 10000. | 128 |
| `daemon.subagents.depth` | How deep chats agents start go, from 1 to 8. | 3 |
| `daemon.subagents.hops` | The longest chain of messages between agents, from 1 to 64. | 6 |
| `daemon.subagents.notice_turns` | The most turns notices of chats finishing start under one of yours between two of your messages there, from 1 to 10000. | 100 |

### The web app

How phones and browsers reach the daemon: see
[the phone](~clackatoo/reference/phone).

| Option | Is | Starts |
|---|---|---|
| `daemon.web.mode` | `off`, `tailscale` (through `tailscale serve`) or `public` (straight from the network, over TLS). | `off` |
| `daemon.web.socket` | With `tailscale`, the unix socket `tailscale serve` connects to. None is `web.sock` in the web app's data. | none |
| `daemon.web.login` | With `tailscale`, the only Tailscale login let in. None is the first device's, which only a window can let in. | none |
| `daemon.web.address` | With `public`, the IP address listened on. | `0.0.0.0` |
| `daemon.web.port` | With `public`, the port listened on. | 8765 |
| `daemon.web.certificate` | With `public`, the certificate chain's PEM file, read again when it changes. | none |
| `daemon.web.key` | With `public`, the certificate's private key's PEM file. | none |
| `daemon.web.origin` | The address the app is reached at, as `https://host:port`: requests for any other, or from a page anywhere else, are refused. Needed with `public`. | none |
| `daemon.web.expire_days` | Days a device can go unused before it's unpaired, from 1 to 36500. | 90 |
| `daemon.web.password_days` | Days before the password is asked for again, or 0 for never. | 0 |
| `daemon.web.ask` | A device may ask to be let in from a window. | on with `tailscale`, off with `public` |
| `daemon.web.lock_after` | Wrong passwords in a row before pairing with the password locks, from 1 to 10000. | 50 |

### What terminals keep

| Option | Is | Starts |
|---|---|---|
| `daemon.history.keep` | Terminals' scrollback is kept [across restarts](~clackatoo/reference/workspaces#after-a-restart). | on |
| `daemon.history.lines` | The most lines kept of each terminal, from 1 to 1000000. | 10000 |
| `daemon.history.every` | How often, in seconds, the scrollback of terminals that changed is written, from 1 to 86400. | 30 |
| `daemon.images.terminal_mb` | The most MiB of [images](~clackatoo/reference/images) each terminal, chat, picture pane or Markdown buffer may hold. | 64 |
| `daemon.images.total_mb` | The most MiB of images they may hold together. | 256 |

## When it goes wrong

A mistake, such as a key that doesn't exist, a mode that isn't one, or a pane
that has closed, is shown in the bottom bar, with the file and line, until the
next key, and written in full to the window's log, `window.log` in clackatoo's
state directory (and to stderr, for a window opened in a process of its own).
A mistake in `init.lua` itself stops it there, as an error in any Lua program
does: what it bound before that line stays bound.

`init.lua` can run for 2 seconds before it's stopped where it is. A handler or
binding that runs for more than a quarter of a second, as a loop that never
ends would, is stopped there, and its key is decided as if it had done nothing
more. The quarter second is counted from when it starts, or what it waited for
comes, until it finishes or waits again, across its slices.

`print` writes to the window's log, `window.log` in clackatoo's state
directory, which keeps what it prints unless `RUST_LOG` says otherwise (and to
stderr, for a window opened in a process of its own).

## Known limits

- `init.lua` and `settings.lua` aren't run again when they change: use
  `:reload`. The daemon's are run again only as it starts or upgrades:
  `clackatoo upgrade` runs them again, keeping every terminal.
- The daemon's config only sets settings: it has no events or handles yet.
- The settings menu has no colour options yet, as no option is a colour, and
  lists none of the daemon's settings yet: set them in `daemon/init.lua`.
- The settings menu can't record <kbd>Enter</kbd>, <kbd>Esc</kbd> or
  <kbd>Backspace</kbd> as keys, nor <kbd>Ctrl</kbd>+<kbd>;</kbd>, which closes
  it.
- Bindings made in Lua don't show in <kbd>Ctrl</kbd>+<kbd>B</kbd>'s list in
  the bottom bar.
- Keypresses and chats' turns ending are the only events.
- `client.opt` can't set a window's own value: `:setlocal` can, through
  `client.cmd`.
- There are no type definitions for editors to complete the API with.
