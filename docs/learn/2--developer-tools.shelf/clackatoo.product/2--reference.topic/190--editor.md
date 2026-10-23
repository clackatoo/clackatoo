---
title: Editing files
type: reference
description: An editor pane edits a file with Vim's keys. The daemon keeps the file's text and its undo history, so every window edits the same text, agents' edits and yours meet, and nothing is lost when the window closes or the daemon upgrades. How to open and write files, the languages files are coloured in, language servers and the mistakes they find, line numbers, tabs and indenting, the view, Markdown rendered, the keys, and what doesn't work yet.
related:
  - clackatoo/reference/window
  - clackatoo/reference/cli
  - clackatoo/reference/config
---

An editor pane edits one file, with Vim's keys. Its text lives in the daemon, in a buffer, as a terminal's screen does, and each window showing it keeps a copy that it changes as you type, so a key never waits for the daemon.

```
:e src/main.rs          edit a file in a new tab
:|Editor src/main.rs    the same, beside the focused pane
:w                      write it
:q                      close the pane
```

## Opening a file

| Command | Does |
|---|---|
| `:e PATH`, `:edit PATH` | Opens the file, or the folder's listing, in place of what the pane shows, as Vim's do; from a terminal, in a new tab. `:e .` shows the folder of the file shown. |
| `:e`, `:edit` alone | Opens [the explorer](~clackatoo/reference/explorer) at the folder you're in, the folder of an editor's file or a terminal's, beside an empty file with no name yet, where `:e PATH` would put a file: in place of an editor's file, or in a new tab. The explorer has the focus. |
| `:Editor PATH` | Opens the file, or the folder, in a new tab; with no path, the focused pane's folder. |
| `:\|Editor PATH`, `:\|<Editor PATH`, `:-Editor PATH`, `:-^Editor PATH` | Opens it beside the focused pane: right, left, below or above, as the [split marks](~clackatoo/reference/window#the-command-line) do for terminals. |
| `clackatoo edit [PATH]` | From a shell: opens it, a folder too, in a new tab of the workspace last used; with no path, the current directory. See [the CLI](~clackatoo/reference/cli#edit). |

A relative path is from the focused pane's directory: for a terminal, the directory of the program in front, as for a new terminal; for an editor, the directory its file was opened from. `~/` is your home directory.

A file a pane stops showing, as after `:e`, closes too, unless another pane shows it, or it has changes that aren't written: then it's kept, unseen, the bottom bar says so, and `:e` it again to go back.

A file that's already open is shown again, not read again: two panes on one file are two views of one buffer, each with its own cursor. A file that doesn't exist yet opens empty, and is made when it's first written. A file that isn't UTF-8 text can't be opened, and the bottom bar says so.

A file with no name yet, as `:e` alone makes, is called `[No Name]`. Nothing is written until `:w NAME` names it, from the folder it was opened in; `:w` alone asks for the name on the command line. Nothing could open it again once no pane shows it, so while it has changes the daemon refuses to close, or to show anything else in, the last pane showing it, whoever asks: `:w NAME` writes them, `:q!` closes it, `:e!` empties it.

## Folders

An editor shows a folder as its listing, as oil.nvim does: an entry on each line, folders ending `/`. The first line is `..`, except at `/`: Enter or a double click on it goes up, and deleting or moving it changes nothing. It's the [explorer's listing](~clackatoo/reference/explorer#editing-a-folder-as-text), edited the same way: change a line to rename, delete one to delete (to the trash), add one to make a file, or a folder if it ends `/`. `:w` lists every change, and asks under them; `y` makes them, `n` goes back. Once they're made, the folder is listed again. In a listing, `r` leaves each line's hidden mark and the `..` line as they are; `J`, and deleting, changing or putting over characters across lines, are refused, since they would join entries: act on whole lines instead. `>` and `<` are refused too, since a name may start with a space.

| Key | Does |
|---|---|
| Enter | On a file, opens it in the listing's place; on a folder, goes into it; on `..`, the first line, goes up |
| `-` | Goes up a folder; in a file, shows the folder it's in, with the cursor on the file |
| `g.` | Shows hidden files, or hides them, as `explorerhidden` starts them |

A listing with changes isn't left until they're made or thrown away: `:e!` throws away every listing's changes, and `:q!` closes the pane. One with no changes is listed afresh whenever its folder changes on disk, and a folder you've left as it was stops being watched. The finder (`<Space>pf`) is rooted at the folder shown, and a relative `:e` path starts there. Going into a folder, up from one, and from a listing to a file are jumps: Ctrl+O and Ctrl+I go back through them, landing on the entry the cursor was on. If a folder with changes disappears from disk, `y` makes none of them.

## Buffers

The daemon keeps each buffer, as it keeps terminals:

- **Every window edits the same text.** Edits made in one window, or by anyone else, show in the others, and move their cursors with the text around them.
- **Edits made at once meet.** If an agent edits a file while you type in it, both edits are kept, and every copy ends up with the same text. The daemon puts edits in order, and brings each one up to date past those made since, by operational transformation, as Google Docs does.
- **Undo undoes your edits, not anyone else's.** `u` takes back your last change, from whichever window you made it in, and leaves an agent's edits made since in place. See [the files an agent changes](~clackatoo/reference/agents#the-files-it-changes).
- **Nothing is lost when the window closes, or the daemon [upgrades](~clackatoo/reference/cli#upgrade).** Buffers come through an upgrade with their undo history. Changes not written outlive a crash or a stop too: see [Recovery](#recovery).

Each command in normal mode is one undo step, as is each go of insert mode, with the command that started it, such as `cw`.

A file whose lines end with CRLF, as Windows ends them, is edited with plain line ends, and written with CRLF again, as its first line says; a file that mixes the two is written all one way. Its git signs compare it line by line, but it's staged a change at a time in a terminal, not here.

A buffer closes with its last pane, unless it has changes that aren't written. `:q` won't close that pane, and `:q!` throws the changes away. A pane closed another way, such as with Ctrl+B `x` or by closing its tab, keeps the buffer's changes, unseen: opening the file again shows them.

## Languages

A file in a language that's installed is coloured as the language's grammar reads it: keywords, strings, comments, functions, types and so on.

Opening a file in a language that isn't installed, but that clackatoo knows where to get, asks in the bottom bar:

```
Install toml's grammar and taplo? [Y/n]
```

`y` or Enter installs the language, and its [language server](#language-servers) if that isn't installed either. The bar says when it's done, and the file is coloured and checked straight away. `n` means it's never offered again. Any other key leaves the question unanswered, and does what it would have done. Each window asks once about each language, so a window opened later asks again. The question waits while a terminal has the focus, or the command line is open.

The [option](~clackatoo/reference/config#options) `languageinstall` decides what happens instead: `ask`, `always` (install without asking) or `never`.

[`clackatoo language`](~clackatoo/reference/cli#language) installs languages from the command line too:

```
clackatoo language list            what can be installed, and what is
clackatoo language install rust    install one, or several, with their servers
clackatoo language install --all   install every one
```

clackatoo knows where to get Bash, C, CSS, Go, HTML, JavaScript, JSON, Lua, Python, Rust, TOML, TypeScript (and TSX) and YAML. A file's language is found from its name: its extension, such as `.rs`, or the whole name, such as `.bashrc`.

How it works:

- **Grammars are [tree-sitter](https://tree-sitter.github.io)'s**, compiled to WebAssembly by their authors, and run in the daemon by [wasmtime](https://wasmtime.dev). A grammar can't reach anything outside itself, so one that goes wrong can't take the daemon, and every terminal it keeps, with it. Nor does a new language need a new clackatoo.
- **The daemon parses each file once, whichever windows show it**, on a thread of its own, so typing never waits for a parse. After an edit, only what it changed is read again: about a tenth of a millisecond in a big source file. The colours of text you've just typed follow within a frame or so; until then, the colours already there move with the text.
- **Opening a file reads it all**, which takes about 50 ms for an 80 KB file in a language not used since the daemon started, most of it getting the grammar ready, and less after.

### Language packs

Each language is a directory in `~/.local/share/clackatoo/languages` (or `$XDG_DATA_HOME/clackatoo/languages`), which `clackatoo language install` writes, and you can write yourself for a language clackatoo doesn't know:

| File | |
|---|---|
| `language.toml` | `grammar`: the grammar's name, which the WebAssembly is built with (`tree_sitter_NAME`). `extensions`: the endings of the names of files it's for, without the dot. `filenames`: whole names of files it's for. `server` and `roots`, if given: its [language server](#language-servers). `word`, if given: the characters its words are made of besides letters, digits and `_`, as Vim's `iskeyword` adds them, as a string (`"-"`) or a list (`["-", "?"]`) of ASCII punctuation; with `word = "-"`, `w`, `e`, `b`, `iw`, `*` and completion take `margin-top` as one word. A pack that doesn't say takes what clackatoo's own pack of its name says: CSS's is `-`, and the rest have none, as in Neovim. |
| `grammar.wasm` | The grammar, as `tree-sitter build --wasm` makes it. Many grammars' releases have one. |
| `highlights.scm` | The [query](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/index.html) naming what each piece of text is. Where two patterns name the same text, the first wins, as grammars' own queries are written: nvim-treesitter's queries expect the last to win, so the more particular patterns come first here. A query that never names the same text twice reads the same either way. |
| `textobjects.scm` | Optional. Where its functions, classes and arguments are, written as nvim-treesitter-textobjects writes them: `@function.outer` and `@function.inner`, `@class.outer` and `@class.inner`, `@parameter.outer` and `@parameter.inner`. `clackatoo language install` gets it; install a language again to get it for a pack installed before. Without it, they're found by the kinds of the grammar's nodes. |
| `indents.scm` | Optional. How its lines are indented, written as nvim-treesitter writes it: `@indent.begin`, `@indent.end`, `@indent.branch`, `@indent.dedent`, `@indent.align`, `@indent.zero`, `@indent.ignore` and `@indent.auto`. `clackatoo language install` gets it; install a language again to get it for a pack installed before. Without it, a new line keeps the indent of the line before, and `=` says so. |

```toml
grammar = "rust"
extensions = ["rs"]
filenames = []
```

A query's names are coloured by the first part: `@function.method.call` as a function, `@string.escape` as an escape. A pack the daemon can't load is passed over, and its log says why.

## Language servers

A language server is a program that knows a language well enough to say what's wrong with a file as you write it, such as [rust-analyzer](https://rust-analyzer.github.io) for Rust. It speaks [LSP](https://microsoft.github.io/language-server-protocol/), the Language Server Protocol, which most editors share. When you open a file in an installed language whose server is installed, its mistakes are underlined with a wavy line: red for errors, yellow for warnings, blue for information and cyan for hints. With the cursor on a line with one, the bottom bar says what it is, after the cursor's place:

```
main.rs  12:9  error: mismatched types [E0308 rustc]
```

That's the one under the cursor, or else the one on the line that matters most.

The worst on each line is also written dimly after the line's end, in its colour, as far as the pane has room, unless the [option](~clackatoo/reference/config#options) `virtualtext` is off; and the line's number takes its colour. `]d` goes to the next mistake and `[d` to the one before, round the file's ends.

`K` asks the server what's under the cursor and shows it in a panel under the cursor, or over it where there's more room, as wide as its text up to 80 columns, scrolling past 20 lines. Headings and strong text are bold, and code is on a faint band. Code in a fenced block is coloured in the language its tag names, such as `rust` or `py`, or in the file's own language if it has no tag, once the daemon has read it; code in a language that isn't installed stays plain. The name the server says it's about is underlined while the panel is shown, until the panel closes or the text there changes. What's wrong there comes first, in full. Ctrl+W `d` shows only what's wrong there, in full.

`grn` renames what's under the cursor everywhere the server knows of it, asking in the bottom bar for the new name. The new name starts selected, so what you type replaces it; an arrow key, Home or End keeps it to edit, and Backspace, Delete or Ctrl+W clears it. A server that can tell is asked first whether there's anything there to rename: if there isn't, the bar says so at once, in the server's words if it gives any; if there is, the new name starts as the server suggests, or as the name it would rename. With any other server it starts as the word under the cursor. Files that are open are changed as if you'd typed it, one undo step each, so every window sees it. Files that aren't open are written: all of them, or none, if one can't be, or has changed meanwhile. A rename that would make, rename or delete files, or write outside the project, is refused, as is one touching over 200 files that aren't open.

`gra` asks the server what it could do to the text under the cursor, or to what's selected in visual mode, such as fix what's wrong there or rewrite it, and shows it in a menu by the cursor: quick fixes first, and those the server would pick before the rest. `j` and `k`, the arrows, Tab, Ctrl+N and Ctrl+P move through it; Enter does the one picked, and 1 to 9 the one so numbered; Esc or `q` closes it. Its changes are made as `grn`'s are, as if you'd typed them, one undo step in each file, and refused the same way. An action that runs one of the server's commands runs it, one at a time, and the changes the server makes while it runs are made the same way; changes a server makes unasked are refused. The bar says how it went.

`<Space>bs` lists the file's symbols in [the finder](~clackatoo/reference/window#the-finder), as the server knows them, in the order of the file's outline. `<Space>ps` finds symbols in the whole project. Enter goes to the one picked.

Once the cursor rests on a name in normal mode, the other places it's used in the file are lit, as the server says: where it's given a value a little brighter than where it's read. They go once the cursor leaves the name, the text changes, or the mode does. The [option](~clackatoo/reference/config#options) `highlightuses` turns this off.

A server that can also writes notes between the code, dim and italic: the type of a name bound without one written, as `: u32` after `let total`, and the name of the parameter an argument is for, as `count:` before it. They're never part of the text: the cursor steps over them, motions, searches and yanks don't see them, a click on one lands on the character after it, a long line wraps with them, each staying with its character, and `colorcolumn` runs straight past them. While you type, the cursor's line shows none; they come back once typing pauses. They're asked for the part of the file in view and a screenful either side. `<Space>th`, or the [option](~clackatoo/reference/config#options) `inlayhints`, turns them off.

```
 12 let total = count + "1";  ■ cannot add `&str` to `u32`
```

How it works:

- **The daemon runs the server, not the window**, one for each project, and keeps each file open in it as it's edited, by anyone. Every window, and every agent, sees the same.
- **A project is found from the file's place:** the nearest directory above it with one of the server's root files, such as `go.mod`, or else the nearest with a `.git`, or else the file's own directory. A file right in your home directory has no project, and no server, rather than make the whole of home one. A Rust project is the outermost directory with a `Cargo.toml`, so a workspace of many crates has one rust-analyzer, not one each.
- **Only what changes is sent** after each edit, for servers that can take that, which most can.
- **A question asked again cancels the last.** A newer `K`, completion or call help from the same window about the same file tells the server to stop working on the one before, and an answer that comes anyway is ignored. So do a newer search of the project's symbols, the file's symbols asked again, and a rename's check. Inlay hints asked again cancel only those asked for a part of the file the new question covers, so two panes on one file never cancel each other's. Another window's questions are never cancelled.
- **A server starts when a file of its is first opened, and stops five minutes after the last closes,** so going to a library's file and back, or closing a project's files and opening another soon after, doesn't make it read the whole project again. A file gone to from an answer, such as a library's definition, is opened in the server that answered, not one of its own. One that stops by itself is started again at once, for the files of its on screen; if it stops by itself again, it's left stopped until its project's files are all closed, and the daemon's log says why. Servers don't come through an [upgrade](~clackatoo/reference/cli#upgrade): they start again as their files are next shown. A server stopped is asked to shut down, then told to exit, as LSP asks, and killed if it hasn't gone a second later. The daemon stops its servers the same way when it stops or upgrades, waiting at most a second and a half. What a server writes to stderr reaches the daemon's log, its last lines too. A server that stops listening is still read for a moment, so its last answers arrive, but it isn't asked anything more: questions, and commands from code actions, fail at once with "its language server has hung up". A server runs in a process group of its own, so stopping it also ends whatever it started (before Linux 6.9, only the server's own process).
- **A server's big answers don't hold up typing.** A message of 256 KiB or more is read, and later freed, on a thread of the daemon's own, so a 60 MB list of a project's symbols doesn't stop a key's echo in another pane. What the server sends after it waits behind it, so its messages are still taken in the order it sent them, and an answer sent just before a server stops still arrives. A message longer than 64 MiB stops the server, and the daemon's log says how long it was.
- **Servers hear of files changed on disk**, by git, an agent or another editor, if they ask, as rust-analyzer does: a file made, written or deleted is told them a tenth of a second later, a few at a time. Only folders inside the server's project are watched, never `.git`, what `.gitignore` leaves out, or `target`, `node_modules`, `.venv` and `__pycache__`, and the servers share up to 3072 of them. A file open in a buffer isn't told of, as the server has it from the buffer. A server that asks for nothing still hears of changes to files it has said are wrong.

clackatoo knows each installed language's usual server, so nothing needs setting up. A server that isn't installed is [offered](#languages) along with its language, or on its own when the language is installed but the server isn't. The daemon looks for a server's program on `PATH` first, then in `~/.local/share/clackatoo/servers`, where clackatoo installs servers, each in its own directory:

- **A server with a release built for Linux is downloaded**, from its own GitHub releases, at the version clackatoo was tried with, and checked to be exactly that file: rust-analyzer, taplo, lua-language-server and clangd. clangd has no build for ARM.
- **A server published to npm is installed with npm**, at a pinned version: pyright, typescript-language-server (with TypeScript 6, as 7 has no `tsserver`), the CSS, HTML and JSON servers, yaml-language-server and bash-language-server. Without npm, these aren't offered.
- **gopls is installed yourself**, with `go install golang.org/x/tools/gopls@latest`.

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

As you type a word in insert mode, the [language server](#language-servers) is asked what could finish it, once typing pauses; a character it names, such as `.`, asks at once. Ctrl+Space, Ctrl+N and Ctrl+P ask too. The menu under the cursor narrows as you type, best first. Each item's label starts under the word you're typing, with its letters that match in blue, then its detail, dimmed, and its kind, dimmed, at the right. When the menu is too narrow, the detail is cut short first, then the label, and in a very narrow pane the kind goes. The picked item's documentation shows in a box of its own beside the menu: what it is, dimmed, then under a line, what it does.

The word completed is the one before the cursor as the file's language reads it. Where the server doesn't say where the word starts, an item whose text starts with what's just before it, such as `libcap-tools` after `libcap-t`, takes that in too, rather than being put in twice.

| Key | Does |
|---|---|
| Ctrl+N Ctrl+P, Down Up | The next item, the one before |
| Ctrl+Y | Accepts the picked item |
| Enter | Accepts the item you moved to; with none moved to, a new line, as the option `completeaccept` says |
| Ctrl+E | Closes the menu |
| Esc | Closes it and leaves insert mode |

Accepting puts in the item, and any import it needs elsewhere in the file, as one undo step with what you typed, which `.` repeats. A snippet, such as a function with its arguments, goes in with places to type. Its lines after the first take your line's indent, and its tabs are what Tab would put in. Its first place's text is selected: typing replaces it, and Backspace deletes it. What you type in a place is copied to the places that mirror it as you type, and a place with choices shows them in the menu. Tab goes to the next place, and from the last to the snippet's end; Shift+Tab goes to the place before. Tab is Tab again once you reach the snippet's end, press Esc, or move out of it. Filling it in is one undo step with the rest of what you typed, and `.` types what you left, indented as the line it types on.

While you type a call's arguments, its signature shows over the cursor, with the argument you're on in bold.

## Registers

What's yanked and deleted is kept in registers, as in Vim. `"` and a register's name before a command keeps text in it, or puts it from it: `"ayy`, `"ap`, `"_dd`. A count can go either side: `2"a3yy` copies six lines.

| Register | Holds |
|---|---|
| `""` | What was last yanked or deleted: what `p` puts when no register is named. |
| `"0` | The last yank with no register named. |
| `"1` to `"9` | Deletes of a line or more, newest first. `"1p` then `.` puts `"2`'s, and so on back. |
| `"-` | The last delete within a line, with no register named. |
| `"a` to `"z` | Whatever's kept in them. `"A` to `"Z` add to them, on a line of their own if either is lines. |
| `"_` | Nothing: what goes to it is thrown away, and the others keep theirs. |
| `"+` `"*` | The clipboard, and the primary selection that a middle click pastes. |

A block is kept as one, its rows a line each, and `p` and `P` put it back as one: a row on each line from the cursor's, padded with spaces where text follows, and on new lines past the end. `:registers` marks it `b`.

As the option `clipboard` says, the unnamed register goes along with the clipboard by default, as Neovim's `unnamedplus` does: what's yanked or deleted can be pasted anywhere, and `p` puts what was last copied anywhere. Deleting nothing, as `D` on an empty line, leaves them alone. The window reads the clipboard and the primary selection when it gets the focus, off to one side, so a program slow to answer for them never holds it up. In visual mode, `p` and `P` put in place of what's selected; `p` keeps what was selected, as a delete, and `P` doesn't. `:registers` lists them by the cursor.

## Macros

`q` and a register records the keys you press, until `q`; the bar says `recording @a` meanwhile. Every key is kept, whatever has it: the editor, a terminal, the command line, the finder, or a binding. So a macro can run `:w`, move to another pane with Ctrl+L, or open a file with `<Space>pf`. `@` and the register types the keys again, as if you pressed them now, through your bindings as they are now; `@@` types the last one again, and a count that many times: `100@a`. Each file a macro changes takes one `u` to undo. The keys are kept as text, written as bindings are (`A;<Esc>:w<CR><C-l>j`), so `"ap` puts them in the file to change, and `"ay$` keeps them again. An item picked from the completion menu is kept as the text it put in, and a snippet as the text you left in it, not the keys that filled it in.

A key that fails stops the macro, as in Vim: a motion that can't move, a search that finds nothing, an empty register, a command that isn't one or is refused, Enter in the finder with nothing picked, or a write the daemon refuses. So a macro ending `j@a` goes down the file and stops at its end. When a key starts something that takes a moment, such as `gd`, `:w`, opening a file or a search in the finder, the macro waits for it before its next key, and keys you press meanwhile wait behind it. After 5 seconds it gives up and says what it waited for; Esc stops it at once, and so does moving to another pane yourself while it waits. `q` stops recording in a terminal's normal mode too.

## Marks

`m` and a letter marks the cursor's place: `a` to `z` for each file on its own, `A` to `Z` anywhere. `` `a `` goes to it, and `'a` to its line; both are jumps, and an operator takes them: `d'a` deletes the lines from here to the mark's. A mark in another file opens it in place, as `:e` does. `` `. `` goes to your last change. Marks move with the text, whoever edits it, and lines `:m` moves take theirs with them; once no pane shows a file, its marks keep their lines. `:marks` lists them in the finder. `'<` and `'>` are the last selection's start and end, which `gv` selects again, kept as Vim keeps them (see `gv` below); by lines, `'<` is the first line's start and `'>` the last's end. They follow lines `:m` moves too, so the binding `client.keys.map("v", "J", ":m '>+1<CR>gv=gv")`, the keys of a well-known Neovim mapping, moves the lines selected down a line at a time, and keeps them selected.

## Substituting

`:s/pattern/replacement/flags` replaces what matches on the cursor's line, or in a range typed before it: `:%s/old/new/g` replaces every match in the file. Patterns are Vim's, as `/` reads them too: `\(` `\)`, `\|`, `\+`, `\{n,m}`, `\<` `\>`, `\s` `\d` `\w` and their kin, `\v` for fewer backslashes, `\zs` and `\ze`, `\c` and `\C`. Case is matched as `ignorecase` and `smartcase` say. An empty pattern is the last search's, and `:s` makes its pattern the last search, for `n`.

| Range | Lines |
|---|---|
| `12` `.` `$` | Line 12, the cursor's, the last. |
| `12,20` `.,$` | From one to the other. |
| `%` | Every line. |
| `'a` `'<,'>` | A mark's line; the last selection's lines, which `:` in visual mode starts with. |
| `/pat/` `?pat?` | The next line that matches, or the one before. |
| `.+3` `$-1` | Counted on from another. `;` between two counts the second from the first. |

| Flag | Does |
|---|---|
| `g` | Every match on a line, not just the first. |
| `c` | Asks about each in the bottom bar: `y` replaces it, `n` leaves it, `a` replaces the rest, `l` replaces it and stops, `q` or Esc stops, Ctrl+E and Ctrl+Y scroll. |
| `i` `I` | Ignores case, or matches it, whatever the options say. |
| `n` | Counts the matches, replacing nothing. |
| `e` | Says nothing if nothing matches. |
| `&` | The last `:s`'s flags, with these. |
| `r` | With no pattern, the last search's rather than the last `:s`'s. |

In the replacement, `&` and `\0` are the match, `\1` to `\9` its groups, `\r` and `\n` a line break, `\t` a tab, `~` the last replacement, and `\u` `\l` make the next character a capital or small, `\U` `\L` the rest, until `\E`.

`&` does the last `:s` again on the cursor's line, without its flags; `g&` on every line, with them and the last search's pattern. `:&&` keeps the flags, and `:~` uses the last search's pattern. A count after it does that many lines from the range's last: `:s/x/y/ 3`.

Each `:s` is one undo step, however many lines it changes. While you type it, what it would do shows in place, as Neovim's `inccommand` shows it, with the replacements lit; the file isn't touched until Enter, and Esc puts the view back. A `:g` being typed lights its pattern's matches. When nothing matches, the bar says so and a macro stops, as in Vim.

## Lines

| Command | Does |
|---|---|
| `:g/pattern/command` | Does the command on each line that matches: `:g/TODO/d`, `:g/x/normal Atext`, `:g/x/s//y/`, `:g/^/m0` turns the file upside down. `:v` and `:g!` on each line that doesn't. All of it is one undo step. Marks move as lines are deleted. |
| `:d` `:y` | Deletes or copies the lines, into a register if one's named: `:2,5d a`. |
| `:m 0` `:t $` `:co .` | Moves or copies the lines after the line given; 0 is the top. Lines moved take their marks, and the last selection, with them, and so does undoing it. |
| `:normal keys` | Types the keys on each line of the range, as in normal mode, then Esc. |

## Git signs

When a file is in a git repository, a column left of the line numbers shows how it differs from the file in git's index, as gitsigns.nvim does: a green bar beside lines added, a blue bar beside lines changed, and a red line under a line where lines after it were deleted. The signs follow your typing, and update a moment after you pause, or at once when something stages changes, in a terminal or anywhere else.

| Key | Does |
|---|---|
| `]c` `[c` | The next change, or the one before, round the file's ends. A jump, so Ctrl+O comes back. |
| `<Space>hp` | Shows the change at the cursor: its lines as they were, and as they are. |
| `<Space>hr` | Puts the change at the cursor back as git's index has it. `u` undoes it. |
| `<Space>hs` | Stages the change at the cursor, and only it, as `git add -p` would. |

`:Gitsigns preview_hunk`, `reset_hunk`, `stage_hunk`, `next_hunk` and `prev_hunk` do the same, so `client.cmd` can bind them to other keys. A file with conflicts is compared with your side. A file git converts, such as one whose line endings it changes, or that Git LFS keeps, shows its signs but is staged in a terminal, not a change at a time. Files that git doesn't track, or that are bigger than 8 MiB, have no signs.

## Agents' changes

Lines an agent changed and you haven't reviewed have a bar in a column right of git's signs, in the chat's colour, shown while a file has such lines (`agentsigns`). They follow your typing; typing a line back as it was clears it.

| Key | Does |
|---|---|
| `<Space>ga` | Keeps the agent's change at the cursor |
| `<Space>gx` | Rejects it: takes it out, unless you've changed its lines since |
| `<Space>c` | Comments on the line, for the agent |
| `<Space>gr` | Opens a review of the chat's changes (see [Agents](~clackatoo/reference/agents#reviewing-its-changes)) |

`:Blame` writes after each line an agent changed which chat and turn changed it, and `:Blame` again stops.

## Writing

| Command | Does |
|---|---|
| `:w`, `:write` | Writes the file, formatting it first if the option `formatonsave` is on. |
| `:Format` | Formats the file, as its [language server](#language-servers) does, as one undo step. If the server won't format the file, the bar says why: "not formatted: …". |
| `:'<,'>Format`, `:{range}Format` | Formats just those lines, as the language server does, as one undo step. Any changes the server makes outside them are made too, as `:Format`'s are. If the server can't format part of a file, the bar says so. |
| `:w!` | Writes it even if it's changed on disk since it was read or last written. |
| `:wq`, `:x` | Writes it, then closes the pane. `:wq!` writes it even if it's changed. |
| `:w NAME`, `:w! NAME`, `:wq NAME` | Names a file with no name yet and writes it there, from the folder it was opened in; not over a file that's there, unless with `!`. In a file that has a name, it's refused. |
| `:q`, `:quit` | Closes the pane, unless it's the last showing a file with changes not written. |
| `:q!` | Closes the pane, throwing away the file's changes not written. |
| `:e!` | Throws away the changes not written, reading the file again. That's an edit like any other: `u` brings the changes back. |

Writing writes a new file beside the old one, with its permissions, then renames it over the old one, so the file is never half written. If something else has changed the file since it was read or last written, `:w` refuses, so as not to write over the change; `:w!` writes over it. The bottom bar shows `[+]` after the file's name while it has changes that aren't written.

When something other than an agent changes the file on disk, such as a git checkout or another editor, a file with nothing unwritten is read again, as Vim's `autoread` does. That's an edit like any other, so `u` takes it back. With changes not written, yours stay, `:w` refuses, and the bar says the file changed: `:e!` takes it, `:w!` keeps yours. A file over 4 MB is never read again by itself: the bar says it changed. Turn `autoread` off to be told in every case.

## Recovery

Changes not written, a `[No Name]`'s too, are kept on disk as you make them, so they outlive the daemon crashing, being killed or stopping, or the machine going down, as Vim's swap files do. Each buffer with changes has a file in `~/.local/state/clackatoo/recovery`, for you alone: the folder is 0700, each file 0600. It's written a second after you stop typing, and every few seconds while you don't (less often for a big file), by a thread of the daemon's own, so a key never waits for the disk. It goes once the file is written, its changes are thrown away with `:e!`, or they're undone. A buffer over 64 MB isn't kept, and the bar says so. `CLACKATOO_RECOVERY` sets another folder for the daemon, or `off` for none. Only one daemon at a time keeps the files in a folder: a second one keeps none, and says so. An upgrade hands the folder to the next build.

When the daemon next starts, each comes back unseen, as a file with changes not written does after `:q`: opening the file shows it.

| What came back | What happens |
|---|---|
| A file unchanged on disk since | It opens with its changes, not written yet, and the bar says so. `:w` writes them, `:e!` drops them, and `u` puts the file's text back. |
| A file changed on disk since | It opens as it is on disk, and the bar says its changes were set aside. `:recover` takes them, as an edit `u` takes back, or, if you've changed the file since, opens them beside it as a `[No Name]`; `:recover!` drops them. Nothing is written until you write it. |
| A file already open, as after an upgrade | Its text is never replaced: what came back is set aside, as above. |
| A `[No Name]` | It comes back with no name. `:recover` shows it. |

| Command | Does |
|---|---|
| `:recover`, `:rec` | In a file whose changes were set aside, takes them. Otherwise shows the next file or `[No Name]` that came back that no pane shows: in place of an editor's file, or in a new tab. |
| `:recover!` | Drops a file's changes that were set aside. |

As the daemon starts, it takes back at most 64 files or 256 MB at once; `:recover` takes back the rest, one at a time.

## Line numbers

Line numbers run down the pane's left. To begin with, the cursor's line shows its own number, in clackatoo's yellow, and every other line shows how far it is from the cursor, so the count for a motion such as `5j` or `3dd` can be read off the screen. The numbers widen as the file grows, and a pane too narrow for them to leave the text 12 columns shows none.

Three [options](~clackatoo/reference/config#options) choose them, as in Vim. Set them in the config, or as you go with `:set`:

| Options | Show |
|---|---|
| `number` and `relativenumber`, as they start | The cursor's line's number, and the others' distances from it. |
| `number` alone (`:set nornu`) | Every line's own number. |
| `relativenumber` alone (`:set nonu`) | Distances, with 0 on the cursor's line. |
| Neither (`:set nonu nornu`) | No numbers. |

`numberwidth` is the fewest columns they take, with the space after them: 4 to begin with.

## Tabs and indenting

A tab is drawn to the next column that's a multiple of `tabstop`: 4 to begin with. `>>`, `<<` and Tab in insert mode indent by `shiftwidth` columns, which is `tabstop`'s while it's 0, as it starts.

A file keeps the kind of indent it has, whatever the options say. `>`, `<` and Tab indent with tabs or spaces as the first indented line among those they change does, or else the nearest indented line around them. `expandtab`, on to begin with, decides only where no line nearby is indented yet: spaces while it's on, tabs while it's off.

`>` and `<` write each line's indent afresh, as many tabs as fit and spaces for the rest, or all spaces. Empty lines stay empty. Changing `tabstop` with a file open redraws its tabs, and the cursor and selection stay on their characters.

A new line is indented as its language says, where its pack has the rules for it. It takes the indent of the line before at once, so typing never waits, and is put right a moment later, when the daemon has worked it out: usually within a millisecond or two. What you type meanwhile stays after the indent, but if you've typed in the indent itself, as a Tab or Backspace does, or made another change, it's left as you made it. A line where `}`, `]` or `)` is typed first is worked out again. So is a line that starts with `else`, `elif`, `elsif`, `elseif`, `except`, `finally`, `catch`, `rescue`, `ensure`, `case` or `default`, or with `end`, `fi`, `done` or `esac`, which end blocks, once you've typed the word: the key after it, such as a space, Tab, `{`, `:` or Enter, or Esc, finishes it. Only a word you type counts, not one that was there or was pasted, nor a longer word such as `elsewhere`. The rules decide the indent, so a word that isn't one of the language's gets what `=` would give the line. `:set nosi` turns this off, for new lines and these words, and a new line keeps the line before's indent. `=` indents by the rules either way.

```
:set ts=8 sw=4      tabs 8 wide, indenting by 4
:set noet           indent new lines with tabs
```

## The view

| Option | Does | Starts |
|---|---|---|
| `scrolloff` | The fewest lines kept in view above and below the cursor, so what's around it shows as it moves. 999 keeps the cursor's line in the middle. | 5 |
| `cursorline` | Lights the cursor's line faintly, across the pane. | on |
| `wrap` | A line too long for the pane goes on over as many rows as it needs, rather than off the pane's edge. `j` and `k` still move by line; `gj` and `gk` by row. | on |
| `linebreak` | Lines break between words, after a blank or one of `!@*-+;:,./?`. | on |
| `breakindent` | The rows a line goes on over start as far in as it's indented, keeping 20 cells for text. | on |
| `showbreak` | Drawn at the start of those rows, after the indent, such as `:set sbr=↪\ `. | none |
| `colorcolumn` | Marks columns down the file's lines, such as `:set cc=80,120`, to keep lines short by. They count from 1, as the bottom bar's column does. | none |

The marks are drawn behind the text, and the selection over it. See [the options](~clackatoo/reference/config#options) for setting them.

## Rendered Markdown

`<Space>tm`, or `:Markdown`, shows a Markdown file (`.md`, `.markdown`) rendered, in place of its text: headings, lists, task lists, quotes, tables, rules, links, and code coloured as the editor colours its language, in a column of prose down the pane. Again, it shows the text. Either way it keeps your place: the rendered view opens with the block holding the line at the top of the text's view at its top, and the text comes back with the top block's first line at the top. It's a view of the file: what anyone changes, in another pane or window or an agent's chat, shows as it's made.

| Key | Does |
|---|---|
| `j` `k`, `gj` `gk`, Ctrl+E Ctrl+Y | A line, or as many as a count says |
| Ctrl+D Ctrl+U | Half a view, rounded down, or as far as a count before one set, a view at most |
| Ctrl+F, Page Down, Page Up | A view less two lines, which stay in view; a count goes that many |
| `gg` `G` | The top, the end; `10G` or `10gg`, the block holding line 10 |

The wheel scrolls it too. Any other key is the text's: the text comes back where the view was, and the key does there what it does, so `i` types at the start of the top block, and `/` searches the text. `:w`, `:q` and the rest of the command line are the same either way.

A click on a link opens it: a web address with the desktop's opener, as a terminal's links open; a file in the editor, in place of this one, as `:e` would, written from the Markdown file's folder; and `#` and a heading scrolls to that heading, named as GitHub names them. An image shows its text, after `▣`.

The [option](~clackatoo/reference/config#options) `markdownview` says how a Markdown file opens: as its text, `raw`, to begin with, or `rendered`.

## Keys

An editor in a window of its own starts in normal mode, as in Vim. One you go to takes [the window's mode](~clackatoo/reference/window#modes): in insert mode it types at its cursor once its text has come, holding what you type until then. A file opened from the explorer or a review opens in normal mode. The bottom bar shows the mode, the file's name, and the cursor's line and column, and before them a command half typed, as `2d` or `"a`, as Vim's `showcmd` shows it.

### Moving

| Keys | Moves to |
|---|---|
| `h` `j` `k` `l`, the arrows | The next character or line. A count moves that many. |
| Space, Backspace | The next character or the one before, as `l` and `h`, but on over a line's end to the next line's start, or back to the line before's last character, as Vim's do. With an operator, a line's end counts as a character, so `d` and Space on a line's last character deletes it, and `d` and Backspace at a line's start joins it to the line before. |
| `+` Enter, `-` | The next line's first character that isn't blank, or the line before's. A count goes that many lines. With an operator, they take whole lines. |
| `_` | This line's first character that isn't blank, or with a count, that many lines down less one, taking whole lines: `d2_` is `2dd`. |
| Shift+Left Shift+Right, Ctrl+Left Ctrl+Right | As `b` and `w`, and `B` and `W`, as in Vim; in insert mode, either moves a word back or on. Ctrl+Home is `gg` and Ctrl+End goes to the last line's last character, or in insert mode the text's start and end. Shift+Up and Shift+Down page up and down in normal mode, and are Up and Down in insert mode and in menus. |
| `w` `b` `e` | The start of the next word, the start of this one or the one before, the end of this one or the next. |
| `ge` `gE` | The end of the word before, or of the run of anything but blanks before, back over line ends, stopping at an empty line. With an operator, they take the character under the cursor too. |
| `W` `B` `E` | The same, for anything between blanks. |
| `0` `^` `$` | The line's start, its first character that isn't blank (on a line of blanks, the last), its last character; `2$` the next line's last, a count that many lines down less one, as Vim's. |
| `g_` | The line's last character that isn't blank, or with a count, that many lines down less one. |
| `\|` | The column a count says, counting from 1, or the line's last character if it's shorter, wanting that column on as `j` and `k` go. |
| `gg` `G` | The first line, the last, keeping the column, as Neovim's do, or at its first character that isn't blank with the [option](~clackatoo/reference/config#options) `startofline`, as `:` and a line number do. With a count, that line: `12G` and `12gg` are line 12. A count works the same after an operator, so `d3G` deletes from the cursor's line to line 3, and `y2gg` copies from line 2 to the cursor's. |
| `]f` `[f` | The start of the next function, or the one before, as the grammar finds them. A count goes that many. `]C` and `[C` go to classes, structs, impls, traits, enums and interfaces. |
| `H` `M` `L` | `H` and `L` the view's first and last lines shown whole, `M` the line half its rows reach; `3H` and `3L` count that many lines from the top or bottom. They keep `scrolloff` screen rows from the view's edges, unless it's at the file's start or end, but an operator takes to the edges: `dL`, `yH`, and with a count past the view's other edge as far as the text goes, as Vim's does: `d99L` to the text's start. They keep the column as `gg` does. |
| `%` | The bracket that pairs with the one under the cursor, or with the next on its line: `()`, `[]` or `{}`, across lines. With a count, the line that far through the file, `50%` half way, as Vim's: whole lines to an operator. |
| `{` `}` | The empty line before the paragraph, or after it; or the file's first line, or the end of its last. A count goes that many paragraphs, or nowhere if there aren't so many, as Vim's. With an operator, from a line's start, they take whole lines, so `d}` deletes the paragraph. |
| `*` `#` | Searches for the word under the cursor, or the next on its line, as a whole word: forward, or back. `n` and `N` go on. Case is ignored as `ignorecase` says, whatever `smartcase` says, as in Vim. |
| `f` `t` `F` `T` and a character | The next one on the line, or the character before it with `t`; back with `F` and `T`. `;` does it again, and `,` back, after an operator too, as `d;`; each pane keeps its own last find, where Vim keeps one for every window. A tab is one character to them: `f<Tab>` finds it. |
| `/` `?` | Searches forward or back for a pattern, written as Vim writes it (see [Substituting](#substituting)). `n` and `N` find the next and previous match, going round the file's ends. Case is ignored unless the search has a capital letter, as `ignorecase` and `smartcase` start. |
| `gj` `gk` | The next or previous screen row, where lines wrap; a count moves that many. With `nowrap`, as `j` and `k`. |
| `g0` `g$` | The screen row's first or last character; with `nowrap`, the first or last in view. |
| Ctrl+D, Ctrl+U | Half the view's screen rows down or up, rounded down, the view and the cursor together. As Vim's, the view goes no further down than shows the file's last line at its bottom, and starts at a line's start where lines wrap, while the cursor goes as far as the view would have. A count sets how far they go from then on, a whole view at most. |
| Ctrl+F, Page Down, Page Up | A view down, or up with Page Up, to the start of a line, as Vim's does: its screen rows less two, keeping two lines of what was in view where they fit, or one, or none, or a whole view with the text's end in view, and no further down than puts the last line at the top. Then the cursor goes to the first line shown whole, or the last going up, as many rows of whole lines in as `scrolloff` asks, as far as they fit. If the view can't move, nothing does. A count goes that many pages. |
| Ctrl+E Ctrl+Y | The view a screen row down or up, or as many as a count says, keeping the cursor in view, `scrolloff` rows in, as the wheel does. While a panel by the cursor is shown, they scroll it instead. |
| `zt` `zz` `zb` | Scrolls the cursor's line to the top, middle or bottom, `zt` and `zb` keeping `scrolloff` lines above or below it, with the view starting at a line's start, as Vim's does: where lines wrap, one that doesn't fit is left out. With a count, that line, the cursor going to it in the column it's in, as a jump, as Vim's does: `5zt`. |
| `]d` `[d` | The next mistake a [language server](#language-servers) found, or the one before, going round the file's ends. A count goes that many. |
| `gd` `gD` `gy` `gri` | Where what's under the cursor is defined, declared, where its type is defined, or what implements it, as the [language server](#language-servers) says, as Neovim's keys go. A place in another file opens there in place of this one, as `:e` does. Several places open [the finder](~clackatoo/reference/window#the-finder) on them, each with its line. |
| `grr` | Everywhere what's under the cursor is used, its declaration included, as the language server says, in [the finder](~clackatoo/reference/window#the-finder), each with its line. |
| `K` | What's under the cursor, as the [language server](#language-servers) says, in a panel by the cursor, with what's wrong there in full above it, its code coloured and the name it's about underlined. `K` again pages through a long one, Ctrl+E and Ctrl+Y scroll it, and Esc, moving the cursor or an edit closes it. |
| Ctrl+W `d` | What's wrong under the cursor, or else on its line, in full, in the same panel. |

A word is letters, digits and `_`, and in a language whose pack says so, more: in CSS, `margin-top` and `--main-color` are each one word to `w`, `b`, `e`, `iw`, `aw`, `cw`, `*`, `#` and Ctrl+W in insert mode, and to `\<`, `\>` and `\k` in a search, which take words as the motions do, past ASCII too: `*` on the `中文` of `x中文` searches for `中文`, a word of its own, as Vim's does. A search with `\<` or `\>` in such a file that would take too long to finish says it's too complex, and changes nothing. `W`, `B` and `E` are the same in every language, and terminals keep plain words. Past ASCII, characters make words as Vim sorts them: in Latin-1, accented letters and `µ` are letters, but `ª`, `²` and `½` are punctuation and the no-break space is blank; past it, CJK ideographs, kana, hangul and emoji are each a kind of word of their own, apart from letters, so `w` goes from `中文` to the `x` after it.

### Jumps

`gg`, `G`, `12G`, `H`, `M`, `L`, `%`, `{`, `}`, `]f`, `[f`, `]C`, `[C`, `` `a ``, `'a`, `''`, a search found with Enter, `*`, `#`, `n`, `N`, `]d`, `[d`, `gd` and its kin, `:12`, `:$`, `:+3`, and `:e` are jumps: the window remembers where the cursor was, and Ctrl+O goes back there, opening the file again in place of the one shown if it has to, as `:e` does. Ctrl+I goes forward again. A count goes that many places, as `3` Ctrl+O, or nowhere if there aren't that many. Moving to another pane is a jump too (see [the window](~clackatoo/reference/window#moving-around)).

A jump from the line the last one left replaces it, so going back never stops twice on one line. A place moves with the text while a pane of the window shows its file, whoever edits it. Once none does, it keeps its line and column, as far as the file still goes.


`` ` `` and a mark go to it, and `'` to its line's first character that isn't blank (see [Marks](#marks)). `''` and ` `` ` go back to where the last jump left in this file, and again to come back. `m'` makes where the cursor is a place to come back to.
### Changing

| Keys | Does |
|---|---|
| `i` `a` `I` `A` | Insert mode before or after the cursor, at the line's first character that isn't blank (after a line of blanks), at its end. `I` in a selection by characters or lines types at the start of the line its other end is on, or at the cursor if it's on a line before, as Vim's. |
| `o` `O` | Insert mode on a new line below or above, indented as the language says, or as the cursor's line is where it has no rules. |
| `d`, `c`, `y` and a motion | Deletes, changes or copies what the motion moves over: `dw`, `c$`, `y2j`. |
| `d`, `c`, `y` and an object | The same, for a [text object](~clackatoo/reference/window#text-objects): `diw`, `ci"`, `ya(`. |
| `dd` `cc` `yy` | Whole lines: the cursor's, and with a count, those after it. |
| `>>` `<<` | Indents the cursor's line by `shiftwidth`, or takes as much off its indent. A count does that many lines. |
| `~` | Turns over the case of the character under the cursor, and as many after it as the count, moving on past them. With `tildeop`, `~` takes a motion as `g~` does. |
| `g~` `gu` `gU` `g?` and a motion or an object | Turns over, lowers or raises the case of what it moves over, or rot13s it: `gUiw`. Doubled, as `guu` or `gUgU`, the line. A character stays one character, as in Vim: `ß` goes up to `ẞ`. |
| `>` `<` and a motion or an object | Indents, or takes off, the lines it moves over: `>j`, `>i{`, `<G`. |
| `==` | Indents the cursor's line as its language says. A count does that many lines. |
| `=` and a motion or an object | Indents the lines it moves over as the language says, in one undo step: `=ip`, `gg=G`, `=af`. Blank lines are emptied, and lines the rules say to keep, such as those in a string, keep their indent. Keys typed while the daemon answers wait their turn, as for `af`. |
| `x` `X` | Deletes the character under the cursor, or before it. |
| `D` `C` `Y` | Deletes or changes to the line's end, or copies the line. |
| `s` `S` | Changes the character under the cursor, or the line. |
| `r` and a character | Replaces the character under the cursor with it. |
| `J` | Joins the next line onto the cursor's, with a space. |
| `p` `P` | Puts what was last deleted or copied after the cursor, or before. Lines go on the line below or above. |
| `d`, `c`, `y` and `af`, `if`, `ac`, `ic`, `aa` or `ia` | A function and its body, a class (or struct, impl, trait, enum, interface) and its body, an argument with its comma and the argument, as the grammar finds them: `daf`, `cif`, `yaa`. As Vim's `d` does with any text object or motion by characters, `d` takes one that starts a line, or its indent, and ends one as whole lines. A last argument followed by a comma is one argument, with the comma after it. With none around the cursor, the next one; a count takes the one that many out. Typed again in visual mode, they grow the selection to the next one out. Keys typed while the daemon answers wait their turn; if it takes over a second, or Esc is pressed, nothing is done. |
| `u` Ctrl+R | Undoes your last change, or redoes the last undone. The cursor goes back to where the change started, as Vim's: the start of what an operator or a selection took, or where the cursor was, if that's on the lines it changed or next to them, or else to the first of those lines. |
| `gra` | What the [language server](#language-servers) could do to the text under the cursor, or selected, such as fix what's wrong there, in a menu by the cursor to pick from with Enter or a number. The bar says how many files it changed. |
| `grn` | Renames what's under the cursor everywhere the [language server](#language-servers) knows of it, asking for the new name in the bottom bar; it starts selected, so typing replaces it. The bar says how many places in how many files. |
| `.` | Makes the last change again, with what was typed in insert mode after it. A count makes it that many times. A change made in visual mode is made again on as much text from the cursor: as many lines; on one line, as many columns, a tab counting for its width; over several lines, as many lines, to the same column of the last, or to its end if the selection went there with `$`. It keeps its own count, as in Vim, so `V3>` and `.` indent by three steps again. In visual mode, `.` makes a change made in visual mode again on what's selected, so `V>` and `.` indent the lines once more. Unlike Vim, as many columns take no line break past a shorter line's end, though to its end after `$` does, and after `p` over a selection it puts the same text again, where Vim's only deletes. After a change to a block, it makes it again on a block as big from the cursor, as many lines by as many columns, or to each line's end; a count changes nothing. |
| `v` `V` | Visual mode, by characters or whole lines. `d`, `x`, `c`, `s` and `y` act on what's selected, `o` goes to its other end, and the cursor can go just past a line's end, as Vim's, with `l`, Space, Backspace, `$` or a move up or down after it, taking the line break, and `i` or `a` and an object selects the object. `>` and `<` indent the selected lines, by as many steps as the count, and keep them selected, so they can be pressed again. `~`, `u` and `U` turn over, lower or raise the selection's case, `J` joins its lines, `r` and a character replaces every character in it, `I` types at its first line's start and `A` at its last line's end, which `.` types again, Del deletes it, and `:` starts a command for its lines with `'<,'>`, count or not. `=` indents the selected lines as the language says, and leaves visual mode. `gq` has the language server format what's selected, as one undo step. |
| Ctrl+V | Selects a block of columns, from the corner it starts at to the cursor; `v`, `V` and Ctrl+V switch between the kinds of selection. Columns are as drawn: a tab or wide character at a corner is in whole, and one an edge cuts is split into spaces, as Vim does. `o` goes to the opposite corner, `O` to the other corner on the same line, and `$` takes every line to its end. `d`, `x`, `y`, `c`, `r`, `~`, `u`, `U`, `>` and `<` act on the block; `D` and `C` act from its left edge to each line's end. `I` and `A` type on the first line, and once insert mode ends, what was typed goes before the block on every line long enough to reach it, or after it, padding short lines, or after `$`, at each line's end; if someone else changed those lines meanwhile, it stays on the first and the bar says so. A line break typed goes on the first line only. `J` joins its lines and `:` starts with `'<,'>`. Each change is one undo step. In a folder's listing it changes names only, leaves `..` alone, and won't type starting on `..` or put past the end. |
| `gv` | Selects the file's last selection again, by characters, lines or a block as it was, with the cursor at the end it was at; `'<` and `'>` are its ends. Each file keeps its own, through going to another file and back. Its ends are kept as Vim keeps `'<` and `'>`, a line and a column each: lines put in or taken away above take them along, and lines `:m` moves; a line taken away leaves them at that line; `J` joining a line on takes their columns with its text; but text typed or taken away within a line, or a line broken in two with Enter, leaves them at their columns, unlike other marks, and an end beyond its line's end selects just past it. An undo or a redo that makes the text as it was puts back the selection it had then; after another's edit, it moves the selection as any edit does. In visual mode, it swaps what's selected with the selection before. A selection `.` made isn't one. Terminals' normal mode has it too. |

`cw` changes to the end of the word, not up to the next word, as in Vim.

What `y` copies goes on the clipboard too. What `d` and `c` delete only goes where `p` puts from, so deleting doesn't lose what you copied from somewhere else.

### Insert mode

Keys type into the file. Esc, or Ctrl+; as in a terminal, goes back to normal mode.

| Keys | Does |
|---|---|
| Enter | A new line, indented as the language says, or as the line before where it has no rules. |
| Tab | Indents with a tab, where the file is indented with tabs, or else with spaces to the next multiple of `shiftwidth` columns. |
| Backspace, Ctrl+H | Deletes the character before the cursor, joining the line to the one before at its start. |
| Delete | Deletes the character under the cursor. |
| Ctrl+W | Deletes the word before the cursor. |
| Ctrl+U | Deletes back to the end of the line's indent, or from there, to the line's start. |
| The arrows, Home, End | Move, and start a new undo step. |

A count before `i`, `a` or `o` types what's typed that many times: `3ix` Esc types `xxx`.

### The mouse

A click puts the cursor where it's clicked, or at the line's start if it's on the line's number, and the wheel scrolls. A click beside a row a line goes on over goes to that row's start. Moving up and down after a click aims for the column clicked, as Neovim's cursor does, even past the end of a short line or in the middle of a tab.

With Alt held, or Ctrl and Alt, dragging selects a block of columns. Dragging selects, as in Neovim: from where the button went down to where the mouse is, in visual mode, ready for `d`, `y`, `c`, `>` and the rest once you let go. A double click selects a word, by the file's own word characters, and dragging after it selects whole words; a triple click selects the line, and dragging after it whole lines. A long line wrapped over several rows is selected row by row, and an inlay hint is never selected: the mouse on one takes the character it's drawn before. A click ends the selection, and `gv` selects it again. An editor showing a folder selects the same way, but a double click there opens what's clicked, as Enter does.

Past the pane's top or bottom, the view scrolls while the button is held, faster the further past the mouse goes. While you drag, the view keeps still under the mouse; once you let go, `scrolloff` keeps rows around the cursor again. If the text changes under a drag, say by an agent's edit, where it started moves with the text.

Letting go puts what's selected on the primary selection, which a middle click pastes, as selecting in a terminal does; not with `clipboard=unnamed`, where `p` over the selection would put the selection itself.

A drag from insert mode selects in visual mode for a while, as Neovim's "(insert) VISUAL" does, and the bar says `(insert) VISUAL`: once the selection ends, by an operator, Esc or a click, the editor goes back to insert mode at the cursor; `c` types over it. A binding or macro run on it goes back to insert only once all its keys have run. `:` on it, `gra` and leaving the pane leave it in normal mode.

## Not yet

- A window isn't told of changes that came back until it opens their file, or `:recover`; a `[No Name]` that came back isn't in `:Buffers`.
- No `breakat` or `breakindentopt`. Typing near the start of a line of megabytes that isn't plain ASCII takes a moment, as the rest of it is laid out again.
- Lightbulbs or gutter hints for lines with code actions, and typing to narrow the menu of them.
- `:jumps`, to list them, and `5%`, which goes to that part of the file in Vim.
- A server installed some other way than through clackatoo, after a file of its was first opened, isn't looked for again until the daemon restarts.
- `indentkeys` and `indentexpr` as options. Esc on a line with nothing but its indent leaves the indent. `.` doesn't wait for a repeated new line's indent to be put right.
- In Python, `else:` typed under the body it ends stays where it's typed, where Neovim takes it back a step: the grammar clackatoo installs reads that line differently.
- Languages inside others, such as code in a Markdown file, or CSS in HTML: they're coloured as the outer language only. Code in a Markdown file is coloured only when it's [rendered](#rendered-markdown).
- Selecting, copying or searching rendered Markdown, images in it, scrolling a wide table in it sideways, and a link's heading in another file.
- Choosing the colours. They're the palette's named colours where there's one to suit, so they follow the terminal's.
- Options for one pane only: `:set` sets them for every pane.
- `$` and `l` stop at a line's last character that isn't blank, as on a terminal's screen, so blanks at a line's end can be reached only with `A` and `D`.
- `clackatoo edit` opens the tab, but the window doesn't switch to it.
- `\1` inside a pattern, `\@` lookaround, `\&` and `\%V`; `\zs` and `\ze` inside a group; `:g` inside `:g`, or `:s` with `c` under `:g`; a `:g` that moves more than one line, as `:g/^/m0` does, leaves marks on them where they were, as its moves are made as one edit. There are no `'[` and `']` marks. Macros don't record pastes. A mark on deleted text stays where the text was, rather than going.
- Sideways scrolling while dragging with `nowrap`. In visual mode, `D`, `X`, `C`, `S`, `R` and `Y` don't act on whole lines as Vim's do, `X`, `Y`, `R` and `S` don't act on a block as Vim's do, and `.` doesn't make `gq` again.
- A change typed just as the connection to the daemon drops can be lost.
