---
title: Configuration
type: reference
description: The window is configured in Lua, with an init.lua that sets options, binds keys, listens for key presses, and splits, focuses and closes panes. Where the files go, how a key travels through handlers and bindings, the options, the API so far, and what happens when the config goes wrong.
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
  client/settings.lua       options :setpersist saved, run before init.lua
  client/init.lua           the window's config
  client/lua/               modules for it to require
  client/startup.toml       the window's settings, needed before it starts
  daemon/agents.toml        agents chats can talk to, besides those clackatoo knows, and their limits
  daemon/machine.toml       the name this machine's daemon goes by on other machines, and its priority
  daemon/web.toml           whether phones and browsers can reach it, and how
  lua/                      modules shared with the daemon's config, once it has one
```

`require("theme")` loads `theme.lua` from `client/lua/`, or else `lua/`, or
`theme/init.lua` from either. A dot in a name is a directory:
`require("keys.tmux")` is `keys/tmux.lua`. Each module runs once, and later
requires get what it returned.

The config runs once for the client, the process that opens windows, so its
bindings apply in every window. It runs on a thread of its own, alongside the
window starting up, and a typical config takes under a millisecond. The window
never waits for it to show itself: keys pressed before it has run do what they
do without it.

`:reload` runs the config again, from the start: `settings.lua`, then
`init.lua`. Everything the old config bound or listened for goes, the window's
own keys come back first, and options go back to how they start, including
those changed with `:set`.


### The daemon's name

`daemon/machine.toml` names this machine's daemon, as other machines show it:

```toml
name = "jack-desktop"
```

Without it, the daemon takes the machine's hostname. It's read when the daemon
starts or upgrades.

`priority = false` beside it has the daemon run as other programs do, rather
than
[ahead of them](~clackatoo/reference/cli#staying-quick-on-a-busy-machine).

Each daemon also has an ID of its own, chosen the first time it starts and
kept in `machine` in clackatoo's state directory (`~/.local/state/clackatoo`),
or in `$CLACKATOO_MACHINE`. Deleting that file makes the daemon a new machine
to every other; copying it to another machine makes the two refuse each other
until one of them deletes it. `CLACKATOO_MACHINE=off` chooses a new ID at
every start, as tests do.

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

## Keys

`client.keys.map(modes, keys, function)` binds keys in one or more modes,
replacing what they were bound to. `client.keys.unmap(modes, keys)` takes a
binding away, the window's own included.

A binding can say what it does, as Neovim's `vim.keymap.set` takes it:
`client.keys.map("n", "<Space>ft", function() client.cmd("Files") end, {desc = "find todos"})`.
The panel of keys shows it in place of "from your config". Neovim's `silent`,
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
`client.forward(8080)` or `client.forward("3000:8080")`, each time a window
opens on another machine. A window on this machine passes it over silently; a
spec it can't read is an error.

`client.connect(host)` has the daemon bring another machine's workspaces in,
as
[`:connect HOST`](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar)
does, asking again whenever the window connects to the daemon and saying
nothing unless it fails. `client.disconnect(host)` ends that connection.
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
| `agent` | `ag` | The agent `:Agent` and <kbd>Ctrl</kbd>+<kbd>B</kbd> `a` open a chat with: `claude`, `codex`, `gemini`, or one in the daemon's [`agents.toml`](~clackatoo/reference/agent-settings#which-agent). Empty leaves it to the daemon. | `claude` |
| `agenttools` | `agt` | What a new chat's agent can do with [clackatoo's tools](~clackatoo/reference/agent-tools#what-the-agent-can-show-you): `ask` before running commands or typing, `allow` commands in its own terminals, or `off`. | `ask` |
| `claudetask` | `ct` | A new Claude Code chat keeps Claude Code's own Task tool, which runs helpers inside one turn, folded under the Task's [card](~clackatoo/reference/chat-view#claude-code-s-own-subagents). Off, Claude hands work on only as [subagents](~clackatoo/reference/chats-together#subagents), which you can watch. Other agents have no such tool. | off |
| `chatbrowser` | `cbr` | A new chat starts with its [Browser](~clackatoo/reference/agent-tools#the-browser) setting on, so its agent can drive a headless browser with a profile of its own. Off, a chat has none until you turn it on in its settings. | off |
| `chatcost` | `chc` | Under each chat turn, what it spent, its tokens and, where the agent says, its cost; and what the session cost, in the chat's usage detail (see [what else a turn says](~clackatoo/reference/chat-view#what-else-a-turn-says)). | on |
| `carryover` | `co` | About the most of a chat's conversation, in tokens, that an agent the chat moves to hears, from 1000 to 50000, but never more than a quarter of its context. | 20000 |
| `notify` | `ntf` | Which chats you can't see get a [desktop notification](~clackatoo/reference/chat-view#desktop-notifications): `all` (those that finish, fail, or ask something), `questions`, or `off`. | `all` |
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
| `hintpattern` | `hpat` | A regular expression whose matches [hints](~clackatoo/reference/workspaces#hints) label too, as `CLACKA-\d+`. One that can't be read is said in the bar as hints start. | none |

Setting an option that doesn't exist, or to a value it can't hold, such as
`client.opt.number = 1`, is a mistake. `renderer` and `priority` are read only
as a window opens, so `client.opt` reads them but can't set them, and neither
can `:set`.

### Saving options: settings.lua

`:setpersist`, or `:setp`, is `:set` that also saves what it sets, so it's set
again in every window that opens after, and after a `:reload`. It takes what
`:set` takes, and Tab completes the same:

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

## Events and handles

`client.on("keypress", function)` listens for keys anywhere. Windows, tabs and
panes have `:on` too, to listen only there. Each returns a subscription, whose
`:off()` stops it. A pane's or tab's handlers go when it closes.

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

A binding that lets its keys go on passes the count on with them, so `5x`
bound to a function that calls `ev:propagate()` still deletes five.

Windows, workspaces, tabs, and panes are handles: values that stand for one of
them, whose fields are read as they are at the time. Reading a pane that has
closed is a mistake. Two handles to the same pane are equal.

| | Fields | Methods |
|---|---|---|
| `client` | `windows`, `focused` (the focused pane), `opt` | `on`, `keys.map`, `keys.unmap`, `keys.group`, `cmd`, `forward`, `connect`, `disconnect` |
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
handles are for handlers.

## When it goes wrong

A mistake, such as a key that doesn't exist, a mode that isn't one, or a pane
that has closed, is shown in the bottom bar, with the file and line, until the
next key, and written in full to the window's log, `window.log` in clackatoo's
state directory (and to stderr, for a window opened in a process of its own).
A mistake in `init.lua` itself stops it there, as an error in any Lua program
does: what it bound before that line stays bound.

`init.lua` can run for 2 seconds before it's stopped where it is. A handler or
binding that runs for more than a quarter of a second at a time, as a loop
that never ends would, is stopped there, and its key is decided as if it had
done nothing more.

`print` writes to the window's log, `window.log` in clackatoo's state
directory, which keeps what it prints unless `RUST_LOG` says otherwise (and to
stderr, for a window opened in a process of its own).

## Known limits

- The daemon's only config files are `agents.toml`, `machine.toml` and
  `web.toml`.
- `init.lua` and `settings.lua` aren't run again when they change: use
  `:reload`. Windows already open don't see what another saved until then.
- Bindings made in Lua don't show in <kbd>Ctrl</kbd>+<kbd>B</kbd>'s list in
  the bottom bar.
- Keypresses are the only event.
- There are no type definitions for editors to complete the API with.
