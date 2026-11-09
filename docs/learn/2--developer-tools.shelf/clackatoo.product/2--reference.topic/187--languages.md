---
title: Languages and Language Servers
type: reference
description: How files are colored by language, installing language packs, language servers and what they find, trusted folders, and completion.
related:
  - clackatoo/reference/editor
---

A file is colored by its language's grammar, and checked by its language
server, both of which clackatoo offers to install when you first open such a
file.

## Languages

A file in a language that's installed is colored as the language's grammar
reads it: keywords, strings, comments, functions, types, and so on.

Opening a file in a language that isn't installed, but that clackatoo knows
where to get, asks in the bottom bar:

```text
Install toml's grammar and taplo? [Y/n]
```

`y` or <kbd>Enter</kbd> installs the language, and its
[language server](#language-servers) if that isn't installed either. The bar
says when it's done, and the file is colored and checked straight away. `n`
means it's never offered again. Any other key leaves the question unanswered,
and does what it would have done. Each window asks once about each language,
so a window opened later asks again. The question waits while a terminal has
the focus, or the command line is open.

The [option](~clackatoo/reference/config#options) `languageinstall` decides
what happens instead: `ask`, `always` (install without asking) or `never`.

[`clackatoo language`](~clackatoo/reference/cli#language) installs languages
from the command line too:

```text
clackatoo language list            what can be installed, and what is
clackatoo language install rust    install one, or several, with their servers
clackatoo language install --all   install every one
```

clackatoo knows where to get Bash, C, CSS, Go, HTML, JavaScript, JSON, Lua,
Python, Rust, TOML, TypeScript (and TSX) and YAML. A file's language is found
from its name: its extension, such as `.rs`, or the whole name, such as
`.bashrc`.

How it works:

- **Grammars are [tree-sitter](https://tree-sitter.github.io)'s**, compiled to
  WebAssembly by their authors, and run in the daemon by
  [wasmtime](https://wasmtime.dev). A grammar can't reach anything outside
  itself, so one that goes wrong can't take the daemon, and every terminal it
  keeps, with it. Nor does a new language need a new clackatoo.
- **The daemon parses each file once, whichever windows show it**, on a thread
  of its own, so typing never waits for a parse. After an edit, only what it
  changed is read again: about a tenth of a millisecond in a big source file.
  The colors of text you've typed follow within a frame or so; until then, the
  colors already there move with the text.
- **Opening a file reads it all**, which takes about 50 ms for an 80 KB file
  in a language not used since the daemon started, most of it getting the
  grammar ready, and less after.

### Language packs

Each language is a directory in `~/.local/share/clackatoo/languages` (or
`$XDG_DATA_HOME/clackatoo/languages`), which `clackatoo language install`
writes, and you can write yourself for a language clackatoo doesn't know:

| File | |
|---|---|
| `language.toml` | `grammar`: the grammar's name, which the WebAssembly is built with (`tree_sitter_NAME`). `extensions`: the endings of the names of files it's for, without the dot. `filenames`: whole names of files it's for. `server` and `roots`, if given: its [language server](#language-servers). `word`, if given: the characters its words are made of besides letters, digits and `_`, as Vim's `iskeyword` adds them, as a string (`"-"`) or a list (`["-", "?"]`) of ASCII punctuation; with `word = "-"`, `w`, `e`, `b`, `iw`, `*` and completion take `margin-top` as one word. A pack that doesn't say takes what clackatoo's own pack of its name says: CSS's is `-`, and the rest have none, as in Neovim. |
| `grammar.wasm` | The grammar, as `tree-sitter build --wasm` makes it. Many grammars' releases have one. |
| `highlights.scm` | The [query](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/index.html) naming what each piece of text is. Where two patterns name the same text, the first wins, as grammars' own queries are written: nvim-treesitter's queries expect the last to win, so the more particular patterns come first here. A query that never names the same text twice reads the same either way. |
| `textobjects.scm` | Optional. Where its functions, classes and arguments are, written as nvim-treesitter-textobjects writes them: `@function.outer` and `@function.inner`, `@class.outer` and `@class.inner`, `@parameter.outer` and `@parameter.inner`. `clackatoo language install` gets it; install a language again to get it for a pack installed before. Without it, they're found by the kinds of the grammar's nodes. |
| `indents.scm` | Optional. How its lines are indented, written as nvim-treesitter writes it: `@indent.begin`, `@indent.end`, `@indent.branch`, `@indent.dedent`, `@indent.align`, `@indent.zero`, `@indent.ignore`, and `@indent.auto`. `clackatoo language install` gets it; install a language again to get it for a pack installed before. Without it, a new line keeps the indent of the line before, and `=` says so. |

```toml
grammar = "rust"
extensions = ["rs"]
filenames = []
```

A query's names are colored by the first part: `@function.method.call` as a
function, `@string.escape` as an escape. A pack the daemon can't load is
passed over, and its log says why.

## Language servers

A language server is a program that knows a language well enough to say what's
wrong with a file as you write it, such as
[rust-analyzer](https://rust-analyzer.github.io) for Rust. It speaks
[LSP](https://microsoft.github.io/language-server-protocol/), the Language
Server Protocol, which most editors share. When you open a file in an
installed language whose server is installed, its mistakes are underlined with
a wavy line: red for errors, yellow for warnings, blue for information, and
cyan for hints. With the cursor on a line with one, the bottom bar says what
it is, after the cursor's place:

```text
main.rs  12:9  error: mismatched types [E0308 rustc]
```

That's the one under the cursor, or else the one on the line that matters
most.

The worst on each line is also written dimly after the line's end, in its
color, as far as the pane has room, unless the
[option](~clackatoo/reference/config#options) `virtualtext` is off; and the
line's number takes its color. `]d` goes to the next mistake and `[d` to the
one before, round the file's ends.

`K` asks the server what's under the cursor and shows it in a panel under the
cursor, or over it where there's more room, as wide as its text up to 80
columns, scrolling past 20 lines. Headings and strong text are bold, and code
is on a faint band. Code in a fenced block is colored in the language its tag
names, such as `rust` or `py`, or in the file's own language if it has no tag,
once the daemon has read it; code in a language that isn't installed stays
plain. The name the server says it's about is underlined while the panel is
shown, until the panel closes or the text there changes. What's wrong there
comes first, in full. <kbd>Ctrl</kbd>+<kbd>W</kbd> `d` shows only what's wrong
there, in full.

`grn` renames what's under the cursor everywhere the server knows of it,
asking in the bottom bar for the new name. The new name starts selected, so
what you type replaces it; an arrow key, <kbd>Home</kbd> or <kbd>End</kbd>
keeps it to edit, and <kbd>Backspace</kbd>, Delete or
<kbd>Ctrl</kbd>+<kbd>W</kbd> clears it. A server that can tell is asked first
whether there's anything there to rename: if there isn't, the bar says so at
once, in the server's words if it gives any; if there is, the new name starts
as the server suggests, or as the name it would rename. With any other server
it starts as the word under the cursor. Files that are open are changed as if
you'd typed it, one undo step each, so every window sees it. Files that aren't
open are written: all of them, or none, if one can't be, or has changed
meanwhile. A rename that would make, rename or delete files, or write outside
the project, is refused, as is one touching over 200 files that aren't open.

`gra` asks the server what it could do to the text under the cursor, or to
what's selected in visual mode, such as fix what's wrong there or rewrite it,
and shows it in a menu by the cursor: quick fixes first, and those the server
would pick before the rest. `j` and `k`, the arrows, <kbd>Tab</kbd>,
<kbd>Ctrl</kbd>+<kbd>N</kbd>, and <kbd>Ctrl</kbd>+<kbd>P</kbd> move through
it; <kbd>Enter</kbd> does the one picked, and 1 to 9 the one so numbered;
<kbd>Esc</kbd> or `q` closes it. Its changes are made as `grn`'s are, as if
you'd typed them, one undo step in each file, and refused the same way. An
action that runs one of the server's commands runs it, one at a time, and the
changes the server makes while it runs are made the same way; changes a server
makes unasked are refused. The bar says how it went.

`<Space>bs` lists the file's symbols in
[the finder](~clackatoo/reference/workspaces#the-finder), as the server knows
them, in the order of the file's outline. `<Space>ps` finds symbols in the
whole project. <kbd>Enter</kbd> goes to the one picked.

Once the cursor rests on a name in normal mode, the other places it's used in
the file are lit, as the server says: where it's given a value a little
brighter than where it's read. They go once the cursor leaves the name, the
text changes, or the mode does. The
[option](~clackatoo/reference/config#options) `highlightuses` turns this off.

A server that can also writes notes between the code, dim and italic: the type
of a name bound without one written, as `: u32` after `let total`, and the
name of the parameter an argument is for, as `count:` before it. They're never
part of the text: the cursor steps over them, motions, searches and yanks
don't see them, a click on one lands on the character after it, a long line
wraps with them, each staying with its character, and `colorcolumn` runs
straight past them. While you type, the cursor's line shows none; they come
back once typing pauses. They're asked for the part of the file in view and a
screenful either side. `<Space>th`, or the
[option](~clackatoo/reference/config#options) `inlayhints`, turns them off.

```text
 12 let total = count + "1";  ■ cannot add `&str` to `u32`
```

### Trusted folders

A language server can run the code of the project it reads. rust-analyzer
builds and runs a crate's build scripts and procedural macros;
typescript-language-server loads the plugins `tsconfig.json` names from
`node_modules`; gopls runs the `go` command, which can fetch and run another
toolchain. So a server starts only for a folder you've trusted. The first time
a file's server would start for a folder you haven't said anything about, the
bottom bar asks:

```text
Trust ~/src/cloned-thing? Its language servers may run its code (y/n)
```

- **The folder is the project the server would start for** (see below), named
  as the daemon's machine names it, with any character that could hide or turn
  round what's around it shown by its number, as `<U+202E>`.
- **It's asked while an editor showing a file of that folder has the focus**,
  in normal mode, with nothing half typed, and neither the command line nor
  the finder open; until then it waits. So a file an agent opens beside its
  chat asks only once you go to it. A file moved or saved under a new name
  into a folder not trusted asks too. A file on another machine names it, as
  in `Trust ~/src/thing on desk?`, and is trusted there.
- **A window older than the daemon's trusted folders isn't asked**, as it
  can't answer: its files get no servers until you run
  [`clackatoo trust`](~clackatoo/reference/cli#trust) for the folder, or
  `:trust` in a newer window.
- **`y` trusts the folder**, and its files open get their servers at once.
  **`n` marks it not trusted**, and it isn't asked about again. Any other key
  leaves it unanswered, and does what it would have done; the bar says
  `:trust` runs its servers. Each window is asked once about each folder while
  it's connected.
- **Only a key you press then answers it.** Not one a
  [macro](~clackatoo/reference/registers-macros-marks#macros) or a key binding
  types, nor one held while the window waited for something, nor one pressed
  in the first half second the question shows, which says to press it again:
  so a `y` meant for a yank, typed right as the question came, doesn't trust
  anything. Agents never answer it: no tool of theirs trusts a folder, and a
  window under an agent's [native control](~clackatoo/reference/agents)
  refuses to send a trust. A device on the
  [web app](~clackatoo/reference/remote) can't trust a folder either, and
  isn't asked.
- **A folder is trusted with everything inside it**, but for a folder inside
  it marked not trusted: the nearest folder marked, the project's own or one
  it's in, decides. So `clackatoo trust ~/src` trusts every project under it,
  and `clackatoo untrust ~/src/cloned-thing` still keeps that one out.
- **What's marked is the folder a server starts for**, the project's root (see
  below), so only a mark on that folder or one it's in counts. A Rust
  project's root is the outermost folder with a `Cargo.toml`: in a workspace
  of many crates, marking one crate's folder does nothing, as rust-analyzer
  starts for the workspace's folder. `:trusted` lists what's marked, and the
  question names the folder that counts.
- **A folder not trusted keeps its colors**, which clackatoo's own parser
  gives, and every editor key that doesn't need a server; it gets no mistakes,
  `K`, `gd`, completion, renames, or formatting.

| Command | What it does |
|---|---|
| `:trust` | Trusts the focused file's project, the folder its server starts for, and starts its servers. |
| `:trust DIR` | Trusts that folder, from the focused file's directory, or else from home; `~` works. |
| `:untrust`, `:untrust DIR` | Marks the folder not trusted: its servers stop, and it isn't asked about again. |
| `:trusted` | Lists the folders marked, trusted or not, in the bar (`:Messages` shows it again). |

Only a `:trust` you type trusts a folder: one a macro, a key binding, or your
config runs is refused. [`clackatoo trust`](~clackatoo/reference/cli#trust)
does the same from a shell. The folders marked are kept in
`~/.local/state/clackatoo/trusted` (or under `$XDG_STATE_HOME`), readable by
you alone, so they're remembered across restarts and upgrades. A folder whose
name isn't UTF-8 is never asked about; `clackatoo trust` trusts it for the
daemon's run.

When a daemon from before trusted folders upgrades to one that has them, the
projects of the files open then, whose servers are installed, are trusted, as
their servers were running anyway. The daemon's log names each, and the first
window you use says how many:
`trusted 3 folders whose language servers were running: :trusted lists them`.
That's only ever said by an upgrade from such a daemon: deleting the file of
folders, or starting the daemon afresh, trusts nothing, and every other folder
is asked about.

How it works:

- **The daemon runs the server, not the window**, one for each project, and
  keeps each file open in it as it's edited, by anyone. Every window, and
  every agent, sees the same.
- **A project is found from the file's place:** the nearest directory above it
  with one of the server's root files, such as `go.mod`, or else the nearest
  with a `.git`, or else the file's own directory. A file right in your home
  directory has no project, and no server, rather than make the whole of home
  one. A Rust project is the outermost directory with a `Cargo.toml`, so a
  workspace of many crates has one rust-analyzer, not one each.
- **Only what changes is sent** after each edit, for servers that can take
  that, which most can.
- **A question asked again cancels the last.** A newer `K`, completion or call
  help from the same window about the same file tells the server to stop
  working on the one before, and an answer that comes anyway is ignored. So do
  a newer search of the project's symbols, the file's symbols asked again, and
  a rename's check. Inlay hints asked again cancel only those asked for a part
  of the file the new question covers, so two panes on one file never cancel
  each other's. Another window's questions are never canceled.
- **A server starts when a file of its is first opened, if its folder is
  [trusted](#trusted-folders), and stops five minutes after the last closes,**
  so going to a library's file and back, or closing a project's files and
  opening another soon after, doesn't make it read the whole project again. A
  file gone to from an answer, such as a library's definition, is opened in
  the server that answered, not one of its own. One that stops by itself is
  started again at once, for the files of its on screen; if it stops by itself
  again, it's left stopped until its project's files are all closed, and the
  daemon's log says why. Servers don't come through an
  [upgrade](~clackatoo/reference/cli#upgrade): they start again as their files
  are next shown. A server stopped is asked to shut down, then told to exit,
  as LSP asks, and killed if it hasn't gone a second later. The daemon stops
  its servers the same way when it stops or upgrades, waiting at most a second
  and a half. What a server writes to stderr reaches the daemon's log, its
  last lines too. A server that stops listening is still read for a moment, so
  its last answers arrive, but it isn't asked anything more: questions, and
  commands from code actions, fail at once with "its language server has hung
  up". A server runs in a process group of its own, so stopping it also ends
  whatever it started (before Linux 6.9, only the server's own process).
- **A server's big answers don't hold up typing.** A message of 256 KiB or
  more is read, and later freed, on a thread of the daemon's own, so a 60 MB
  list of a project's symbols doesn't stop a key's echo in another pane. What
  the server sends after it waits behind it, so its messages are still taken
  in the order it sent them, and an answer sent right before a server stops
  still arrives. A message longer than 64 MiB stops the server, and the
  daemon's log says how long it was.
- **Servers hear of files changed on disk**, by git, an agent or another
  editor, if they ask, as rust-analyzer does: a file made, written or deleted
  is told them a tenth of a second later, a few at a time. Only folders inside
  the server's project are watched, never `.git`, what `.gitignore` leaves
  out, or `target`, `node_modules`, `.venv` and `__pycache__`, and the servers
  share up to 3072 of them. A file open in a buffer isn't told of, as the
  server has it from the buffer. A server that asks for nothing still hears of
  changes to files it has said are wrong.

clackatoo knows each installed language's usual server, so nothing needs
setting up. A server that isn't installed is [offered](#languages) along with
its language, or on its own when the language is installed but the server
isn't. The daemon looks for a server's program on `PATH` first, only in its
absolute directories (an empty or relative one, such as `.`, would find a
program in the project, which anyone could have filled), and runs it by the
path found, giving it that `PATH`; then in `~/.local/share/clackatoo/servers`,
where clackatoo installs servers, each in its own directory:

- **A server with a release built for Linux is downloaded**, from its own
  GitHub releases, at the version clackatoo was tried with, and checked to be
  exactly that file: rust-analyzer, taplo, lua-language-server, and clangd.
  clangd has no build for ARM.
- **A server published to npm is installed with npm**, at a pinned version:
  pyright, typescript-language-server (with TypeScript 6, as 7 has no
  `tsserver`), the CSS, HTML and JSON servers, yaml-language-server and
  bash-language-server. Without npm, these aren't offered.
- **gopls is installed yourself**, with
  `go install golang.org/x/tools/gopls@latest`.

| Language | Server |
|---|---|
| Rust | `rust-analyzer` |
| Go | `gopls` |
| Python | `pyright-langserver --stdio` |
| JavaScript, TypeScript, TSX | `typescript-language-server --stdio` |
| C | `clangd` |
| Lua | `lua-language-server` |
| Bash | `bash-language-server start` |
| CSS, HTML, JSON | `vscode-css-language-server`, `vscode-html-language-server`, `vscode-json-language-server`, each with `--stdio` |
| TOML | `taplo lsp stdio` |
| YAML | `yaml-language-server --stdio` |

A pack's `language.toml` can name another, or none:

```toml
# The command that starts it, speaking LSP on its stdin and stdout.
server = ["pylsp"]
# The files that mark a project's root.
roots = ["pyproject.toml", "setup.py"]
```

`server = []` turns a language's server off.

Lines past the end of the file show a faint `~`.

## Completion

As you type a word in insert mode, the [language server](#language-servers) is
asked what could finish it, once typing pauses; a character it names, such as
`.`, asks at once. <kbd>Ctrl</kbd>+<kbd>Space</kbd>,
<kbd>Ctrl</kbd>+<kbd>N</kbd>, and <kbd>Ctrl</kbd>+<kbd>P</kbd> ask too. The
menu under the cursor narrows as you type, best first. Each item's label
starts under the word you're typing, with its letters that match in blue, then
its detail, dimmed, and its kind, dimmed, at the right. When the menu is too
narrow, the detail is cut short first, then the label, and in a very narrow
pane the kind goes. The picked item's documentation shows in a box of its own
beside the menu: what it is, dimmed, then under a line, what it does.

The word completed is the one before the cursor as the file's language reads
it. Where the server doesn't say where the word starts, an item whose text
starts with what's right before it, such as `libcap-tools` after `libcap-t`,
takes that in too, rather than being put in twice.

| Key | Does |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>N</kbd> <kbd>Ctrl</kbd>+<kbd>P</kbd>, <kbd>Down</kbd> <kbd>Up</kbd> | The next item, the one before |
| <kbd>Ctrl</kbd>+<kbd>Y</kbd> | Accepts the picked item |
| <kbd>Enter</kbd> | Accepts the item you moved to; with none moved to, a new line, as the option `completeaccept` says |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | Closes the menu |
| <kbd>Esc</kbd> | Closes it and leaves insert mode |

Accepting puts in the item, and any import it needs elsewhere in the file, as
one undo step with what you typed, which `.` repeats. A snippet, such as a
function with its arguments, goes in with places to type. Its lines after the
first take your line's indent, and its tabs are what <kbd>Tab</kbd> would put
in. Its first place's text is selected: typing replaces it, and
<kbd>Backspace</kbd> deletes it. What you type in a place is copied to the
places that mirror it as you type, and a place with choices shows them in the
menu. <kbd>Tab</kbd> goes to the next place, and from the last to the
snippet's end; <kbd>Shift</kbd>+<kbd>Tab</kbd> goes to the place before.
<kbd>Tab</kbd> is <kbd>Tab</kbd> again once you reach the snippet's end, press
<kbd>Esc</kbd>, or move out of it. Filling it in is one undo step with the
rest of what you typed, and `.` types what you left, indented as the line it
types on.

While you type a call's arguments, its signature shows over the cursor, with
the argument you're on in bold.
