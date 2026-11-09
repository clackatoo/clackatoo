---
title: Editor Keys
type: reference
description: The editor's keys for motions, searches shown as you type, lit matches, jumps, changes, insert mode, and the mouse, and the editor's known limits.
related:
  - clackatoo/reference/editor
---

The editor's keys are Vim's.

## Keys

An editor in a window of its own starts in normal mode, as in Vim. One you go
to takes [the window's mode](~clackatoo/reference/window#modes): in insert
mode it types at its cursor once its text has come, holding what you type
until then. A file opened from the explorer or a review opens in normal mode.
The bottom bar shows the mode, the file's name, and the cursor's line and
column, and before them a command half typed, as `2d` or `"a`, as Vim's
`showcmd` shows it.

### Moving

| Keys | Moves to |
|---|---|
| `h` `j` `k` `l`, the arrows | The next character or line. A count moves that many. |
| <kbd>Space</kbd>, <kbd>Backspace</kbd> | The next character or the one before, as `l` and `h`, but on over a line's end to the next line's start, or back to the line before's last character, as Vim's do. With an operator, a line's end counts as a character, so `d` and <kbd>Space</kbd> on a line's last character deletes it, and `d` and <kbd>Backspace</kbd> at a line's start joins it to the line before. |
| `+` <kbd>Enter</kbd>, `-` | The next line's first character that isn't blank, or the line before's. A count goes that many lines. With an operator, they take whole lines. |
| `_` | This line's first character that isn't blank, or with a count, that many lines down less one, taking whole lines: `d2_` is `2dd`. |
| <kbd>Shift</kbd>+<kbd>Left</kbd> <kbd>Shift</kbd>+<kbd>Right</kbd>, <kbd>Ctrl</kbd>+<kbd>Left</kbd> <kbd>Ctrl</kbd>+<kbd>Right</kbd> | As `b` and `w`, and `B` and `W`, as in Vim; in insert mode, either moves a word back or on. <kbd>Ctrl</kbd>+<kbd>Home</kbd> is `gg` and <kbd>Ctrl</kbd>+<kbd>End</kbd> goes to the last line's last character, or in insert mode the text's start and end. <kbd>Shift</kbd>+<kbd>Up</kbd> and <kbd>Shift</kbd>+<kbd>Down</kbd> page up and down in normal mode, and are <kbd>Up</kbd> and <kbd>Down</kbd> in insert mode and in menus. |
| `w` `b` `e` | The start of the next word, the start of this one or the one before, the end of this one or the next. |
| `ge` `gE` | The end of the word before, or of the run of anything but blanks before, back over line ends, stopping at an empty line. With an operator, they take the character under the cursor too. |
| `W` `B` `E` | The same, for anything between blanks. |
| `0` `^` `$`, <kbd>Home</kbd>, <kbd>End</kbd> | The line's start, its first character that isn't blank (on a line of blanks, the last), its last character; `2$` the next line's last, a count that many lines down less one, as Vim's. <kbd>Home</kbd> is `0` and <kbd>End</kbd> is `$`. |
| `g_` | The line's last character that isn't blank, or with a count, that many lines down less one. |
| `\|` | The column a count says, counting from 1, or the line's last character if it's shorter, wanting that column on as `j` and `k` go. |
| `gg` `G` | The first line, the last, keeping the column, as Neovim's do, or at its first character that isn't blank with the [option](~clackatoo/reference/config#options) `startofline`, as `:` and a line number do. With a count, that line: `12G` and `12gg` are line 12. A count works the same after an operator, so `d3G` deletes from the cursor's line to line 3, and `y2gg` copies from line 2 to the cursor's. |
| `]f` `[f` | The start of the next function, or the one before, as the grammar finds them. A count goes that many. `]C` and `[C` go to classes, structs, impls, traits, enums and interfaces. |
| `H` `M` `L` | `H` and `L` the view's first and last lines shown whole, `M` the line half its rows reach; `3H` and `3L` count that many lines from the top or bottom. They keep `scrolloff` screen rows from the view's edges, unless it's at the file's start or end, but an operator takes to the edges: `dL`, `yH`, and with a count past the view's other edge as far as the text goes, as Vim's does: `d99L` to the text's start. They keep the column as `gg` does. |
| `%` | The bracket that pairs with the one under the cursor, or with the next on its line: `()`, `[]` or `{}`, across lines, looking no further than 10,000 lines away. With a count, the line that far through the file, `50%` half way, as Vim's: whole lines to an operator. |
| `{` `}` | The empty line before the paragraph, or after it; or the file's first line, or the end of its last. A count goes that many paragraphs, or nowhere if there aren't so many, as Vim's. With an operator, from a line's start, they take whole lines, so `d}` deletes the paragraph. |
| `(` `)` | The start of this sentence or the one before, or of the next, as Vim's go: a sentence ends at `.`, `!` or `?`, with any of `)` `]` `"` `'` after it, before a space, a tab or a line's end, and at an empty line. A count goes that many, or nowhere if there aren't so many. At the text's end, `)` goes to its last character, which an operator takes too. They're jumps. |
| `*` `#` | Searches for the word under the cursor, or the next on its line, as a whole word: forward, or back. `n` and `N` go on. Case is ignored as `ignorecase` says, whatever `smartcase` says, as in Vim. |
| `f` `t` `F` `T` and a character | The next one on the line, or the character before it with `t`; back with `F` and `T`. `;` does it again, and `,` back, after an operator too, as `d;`. The window keeps one last find, as Vim does, so `;` finds again what `f` found in another editor, a terminal's normal mode or a chat. A tab is one character to them: `f<Tab>` finds it. |
| `/` `?` | Searches forward or back for a pattern, written as Vim writes it (see [Substituting](~clackatoo/reference/substituting#substituting)). `n` and `N` find the next and previous match, going round the file's ends. A count before either goes to that match, as `3/x`, never going round more than once. Case is ignored unless the search has a capital letter, as `ignorecase` and `smartcase` start. Every match in view is lit up, the one found brighter, until <kbd>Esc</kbd> or `:noh` (see [Lit matches](#lit-matches)). While you type, the cursor goes to the match it would find (see [Shown as you type](#shown-as-you-type)). |
| `gj` `gk` | The next or previous screen row, where lines wrap; a count moves that many. With `nowrap`, as `j` and `k`. |
| `g0` `g$` | The screen row's first or last character; with `nowrap`, the first or last in view. |
| <kbd>Ctrl</kbd>+<kbd>D</kbd>, <kbd>Ctrl</kbd>+<kbd>U</kbd> | Half the view's screen rows down or up, rounded down, the view and the cursor together. As Vim's, the view goes no further down than shows the file's last line at its bottom, and starts at a line's start where lines wrap, while the cursor goes as far as the view would have. A count sets how far they go from then on, a whole view at most, until the view changes size. |
| <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd>, <kbd>Page Up</kbd> | A view down, or up with <kbd>Page Up</kbd>, to the start of a line, as Vim's does: its screen rows less two, keeping two lines of what was in view where they fit, or one, or none, or a whole view with the text's end in view, and no further down than puts the last line at the top. Then the cursor goes to the first line shown whole, or the last going up, as many rows of whole lines in as `scrolloff` asks, as far as they fit. If the view can't move, nothing does. A count goes that many pages. With `startofline`, these and <kbd>Ctrl</kbd>+<kbd>D</kbd> and <kbd>Ctrl</kbd>+<kbd>U</kbd> put the cursor at its line's first character that isn't blank, as Vim's do. |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> <kbd>Ctrl</kbd>+<kbd>Y</kbd> | The view a screen row down or up, or as many as a count says, keeping the cursor in view, `scrolloff` rows in, as the wheel does. While a panel by the cursor is shown, they scroll it instead. |
| `zt` `zz` `zb` | Scrolls the cursor's line to the top, middle or bottom, `zt` and `zb` keeping `scrolloff` lines above or below it, with the view starting at a line's start, as Vim's does: where lines wrap, one that doesn't fit is left out. With a count, that line, the cursor going to it in the column it's in, as a jump, as Vim's does: `5zt`. |
| `]d` `[d` | The next mistake a [language server](~clackatoo/reference/languages#language-servers) found, or the one before, going round the file's ends. A count goes that many. |
| `gd` `gD` `gy` `gri` | Where what's under the cursor is defined, declared, where its type is defined, or what implements it, as the [language server](~clackatoo/reference/languages#language-servers) says, as Neovim's keys go. A place in another file opens there in place of this one, as `:e` does. Several places open [the finder](~clackatoo/reference/window#the-finder) on them, each with its line. |
| `grr` | Everywhere what's under the cursor is used, its declaration included, as the language server says, in [the finder](~clackatoo/reference/window#the-finder), each with its line. |
| `K` | What's under the cursor, as the [language server](~clackatoo/reference/languages#language-servers) says, in a panel by the cursor, with what's wrong there in full above it, its code colored and the name it's about underlined. `K` again pages through a long one, <kbd>Ctrl</kbd>+<kbd>E</kbd> and <kbd>Ctrl</kbd>+<kbd>Y</kbd> scroll it, and <kbd>Esc</kbd>, moving the cursor or an edit closes it. |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> `d`, <kbd>Ctrl</kbd>+<kbd>W</kbd> <kbd>Ctrl</kbd>+<kbd>D</kbd> | What's wrong under the cursor, or else on its line, in full, in the same panel. |

A word is letters, digits and `_`, and in a language whose pack says so, more:
in CSS, `margin-top` and `--main-color` are each one word to `w`, `b`, `e`,
`iw`, `aw`, `cw`, `*`, `#` and <kbd>Ctrl</kbd>+<kbd>W</kbd> in insert mode,
and to `\<`, `\>` and `\k` in a search, which take words as the motions do,
past ASCII too: `*` on the `中文` of `x中文` searches for `中文`, a word of its own,
as Vim's does. A search with `\<` or `\>` in such a file that would take too
long to finish says it's too complex, and changes nothing. `W`, `B` and `E`
are the same in every language, and terminals keep plain words. Past ASCII,
characters make words as Vim sorts them: in Latin-1, accented letters and `µ`
are letters, but `ª`, `²` and `½` are punctuation and the no-break space is
blank; past it, CJK ideographs, kana, hangul and emoji are each a kind of word
of their own, apart from letters, so `w` goes from `中文` to the `x` after it. A
skin tone after an emoji, or after any other character, goes with it as part
of its word to the motions (`dw` on `👍🏽` takes it whole), though not after a
tab or at a line's start, and only up to four in a row; to `*`, `#`, `\<` and
`\>` a skin tone is punctuation, so `*` on `👍🏽` searches for `👍` with any tone
or none.

### Shown as you type

While you type a search after `/` or `?`, the cursor goes to the match it
would find from where you started, the view follows, and its matches are lit
up, the one it would go to brighter, as Neovim's `incsearch` shows them. Each
key shows it afresh from where you started, <kbd>Backspace</kbd> too.
<kbd>Esc</kbd> puts the cursor, the view and what was lit before back;
<kbd>Enter</kbd> keeps them, and is the search, so `n`, the search history and
macros have it as they would without it. A pattern you haven't finished, such
as `\(` without its `\)`, shows nothing yet, nor does a count before `/` until
<kbd>Enter</kbd>.

It's worked out once before each frame is drawn, not at each key, so typing
fast costs one search between frames, and a macro typing `/foo` and
<kbd>Enter</kbd> costs none. It looks no further than a megabyte from where
you started each way round the file's ends, and through no more than so many
matches going back. Where it can't be sure it would find what <kbd>Enter</kbd>
will, such as a match further away than that, matches along a very long line
found from part way along it, or a pattern that crosses line breaks in a big
file, it shows nothing until <kbd>Enter</kbd>, rather than a match
<kbd>Enter</kbd> wouldn't go to. Edits from elsewhere while you type move
where it started, and show at your next key. A pattern with `\<`, `\>` or `\k`
in it also reads the whole file once a key to check how its words are made, as
any search with them does. With the
[option](~clackatoo/reference/config#options) `incsearch` off, nothing moves
until <kbd>Enter</kbd>.

### Lit matches

After `/`, `?`, `*`, `#`, `n` or `N`, every match of the search in view is lit
up, as Neovim's `hlsearch` lights them, and the one the search went to
brighter than the rest. `:s`, `:g` and a `:/pattern/` address make their
pattern the last search, and light its matches, as in Vim. They stay lit as
you move, edit and scroll, until <kbd>Esc</kbd> in normal mode (leaving visual
mode keeps them) or `:nohlsearch` (`:noh`), and come back at the next search,
`n` or `N`. A terminal's and a chat's do the same. With the
[option](~clackatoo/reference/config#options) `hlsearch` off, only the match
found is lit.

Matches are found along each line from its start, or a few kilobytes before
what's shown of a very long line, one after another, as `n` finds them, so
they never overlap, and none is drawn over the one found. Only the lines in
view are looked through, with a few kilobytes around what's shown of each, for
a match that runs into view from before it, from an earlier line too, or on
past it, and at most 512 matches on a line; and only again when the text, the
view or the search changes, not at each `n`. So a huge file or a very long
line costs no more than a short one. A match starting at a line's break, as
`/\nfoo` finds, lights the cell after the line's end.

### Jumps

`gg`, `G`, `12G`, <kbd>Ctrl</kbd>+<kbd>Home</kbd>,
<kbd>Ctrl</kbd>+<kbd>End</kbd>, `H`, `M`, `L`, `%`, `{`, `}`, `(`, `)`, `]c`,
`[c`, `]f`, `[f`, `]C`, `[C`, `` `a ``, `'a`, `''`, a search found with Enter,
`*`, `#`, `n`, `N`, `]d`, `[d`, `gd` and its kin, `zt`, `zz` and `zb` with a
count, `:12`, `:$`, `:+3`, and `:e` are jumps: the window remembers where the
cursor was, and Ctrl+O goes back there, opening the file again in place of the
one shown if it has to, as `:e` does. Ctrl+I goes forward again. A count goes
that many places, as `3` Ctrl+O, or nowhere if there aren't that many. Moving
to another pane is a jump too (see
[the window](~clackatoo/reference/window#moving-around)).

A jump from the line the last one left replaces it, so going back never stops
twice on one line. A place moves with the text while a pane of the window
shows its file, whoever edits it. Once none does, it keeps its line and
column, as far as the file still goes.


`` ` `` and a mark go to it, and `'` to its line's first character that isn't
blank (see [Marks](~clackatoo/reference/registers-macros-marks#marks)). `''`
and ` `` ` go back to where the last jump left in this file, and again to come
back. `m'` makes where the cursor is a place to come back to.
### Changing

| Keys | Does |
|---|---|
| `i` `a` `I` `A` | Insert mode before or after the cursor, at the line's first character that isn't blank (after a line of blanks), at its end. `I` in a selection by characters or lines types at the start of the line its other end is on, or at the cursor if it's on a line before, as Vim's. |
| `o` `O` | Insert mode on a new line below or above, indented as the language says, or as the cursor's line is where it has no rules. |
| `d`, `c`, `y` and a motion | Deletes, changes, or copies what the motion moves over: `dw`, `c$`, `y2j`. |
| `d`, `c`, `y` and an object | The same, for a [text object](~clackatoo/reference/window#text-objects): `diw`, `ci"`, `ya(`. |
| `dd` `cc` `yy` | Whole lines: the cursor's, and with a count, those after it. |
| `>>` `<<` | Indents the cursor's line by `shiftwidth`, or takes as much off its indent. A count does that many lines. |
| `~` | Turns over the case of the character under the cursor, and as many after it as the count, moving on past them. With `tildeop`, `~` takes a motion as `g~` does. |
| `g~` `gu` `gU` `g?` and a motion or an object | Turns over, lowers or raises the case of what it moves over, or rot13s it: `gUiw`. Doubled, as `guu` or `gUgU`, the line. A character stays one character, as in Vim: `ß` goes up to `ẞ`. |
| `>` `<` and a motion or an object | Indents, or takes off, the lines it moves over: `>j`, `>i{`, `<G`. |
| `==` | Indents the cursor's line as its language says. A count does that many lines. |
| `=` and a motion or an object | Indents the lines it moves over as the language says, in one undo step: `=ip`, `gg=G`, `=af`. Blank lines are emptied, and lines the rules say to keep, such as those in a string, keep their indent. Keys typed while the daemon answers wait their turn, as for `af`. |
| `x` `X`, Del | Deletes the character under the cursor, or before it; a count deletes that many. Del is `x`; while a count is typed, it takes the count's last digit off, as Vim's. |
| `D` `C` `Y` | Deletes, changes, or copies to the line's end: `Y` is `y$`, as Neovim's is, where Vim's copies the line; `client.keys.map("n", "Y", "yy")` makes it Vim's. In visual mode, `Y` copies the lines selected, whole, wherever on them the selection starts and ends, or a block as it is, as Vim's does. |
| `s` `S` | Changes the character under the cursor, or the line. |
| `r` and a character | Replaces the character under the cursor with it, or with a count that many: `3rx`. If the line hasn't that many, it changes nothing and fails, stopping a macro, as Vim's. `r` and <kbd>Enter</kbd> breaks the line in their place, as Vim's: the blanks after them go, and the new line takes this one's indent. |
| `J` | Joins the next line onto the cursor's, its indent taken off, with a space between, but none onto an empty line, after a space or a tab, or before a `)`, as Vim's. A count joins that many lines: `3J`. |
| `p` `P` | Puts what was last deleted or copied after the cursor, or before. Lines go on the line below or above. |
| `d`, `c`, `y` and `af`, `if`, `ac`, `ic`, `aa`, or `ia` | A function and its body, a class (or struct, impl, trait, enum, interface) and its body, an argument with its comma and the argument, as the grammar finds them: `daf`, `cif`, `yaa`. As Vim's `d` does with any text object or motion by characters, `d` takes one that starts a line, or its indent, and ends one as whole lines. A last argument followed by a comma is one argument, with the comma after it. With none around the cursor, the next one; a count takes the one that many out. Typed again in visual mode, they grow the selection to the next one out. Keys typed while the daemon answers wait their turn; if it takes over a second, or <kbd>Esc</kbd> is pressed, nothing is done. |
| `u` <kbd>Ctrl</kbd>+<kbd>R</kbd> | Undoes your last change, or redoes the last undone. The cursor goes back to where it was as the change began, as Vim's does: where an operator began, such as the start of what `dk` took, if that's in or beside the lines the change made, or else the first of those lines. The file's last selection comes back as it was then, for `gv`. A command that changes nothing still makes a step where Vim's does, such as `3J` on the last line, `X` at a line's start, `x` on an empty line, or an operator on an empty line, and `u` undoes it on its own, putting the cursor back. With a daemon from before 2026.10.81, the cursor goes to where the text the undo changes starts. |
| `gra` | What the [language server](~clackatoo/reference/languages#language-servers) could do to the text under the cursor, or selected, such as fix what's wrong there, in a menu by the cursor to pick from with <kbd>Enter</kbd> or a number. The bar says how many files it changed. |
| `grn` | Renames what's under the cursor everywhere the [language server](~clackatoo/reference/languages#language-servers) knows of it, asking for the new name in the bottom bar; it starts selected, so typing replaces it. The bar says how many places in how many files. |
| `.` | Makes the last change again, with what was typed in insert mode after it. A count makes it that many times. A change made in visual mode is made again on as much text from the cursor: as many lines; on one line, as many columns, a tab counting for its width; over several lines, as many lines, to the same column of the last, or to its end if the selection went there with `$`. It keeps its own count, as in Vim, so `V3>` and `.` indent by three steps again. In visual mode, `.` makes a change made in visual mode again on what's selected, so `V>` and `.` indent the lines once more. Unlike Vim, as many columns take no line break past a shorter line's end, though to its end after `$` does, and after `p` over a selection it puts the same text again, where Vim's only deletes. After a change to a block, it makes it again on a block as big from the cursor, as many lines by as many columns, or to each line's end; a count changes nothing. |
| `v` `V` | Visual mode, by characters or whole lines. `d`, `x`, `c`, `s` and `y` act on what's selected, `D` and `X` delete the lines selected, whole, and `C`, `S` and `R` change them, as Vim's, `o` goes to its other end, and the cursor can go right past a line's end, as Vim's, with `l`, <kbd>Space</kbd>, <kbd>Backspace</kbd>, `$` or a move up or down after it, taking the line break, and `i` or `a` and an object selects the object. `>` and `<` indent the selected lines, by as many steps as the count, and keep them selected, so they can be pressed again. `~`, `u` and `U` turn over, lower or raise the selection's case, as do `g~`, `gu` and `gU`, and `g?` rot13s it, `J` joins its lines, `r` and a character replaces every character in it, `p` and `P` put in its place (see [Registers](~clackatoo/reference/registers-macros-marks#registers)), `I` types at its first line's start, or at the cursor if that's above the other end, as the `I` row says, and `A` after the selection's end, as Vim's: after the cursor's character, or at the start of the other end's line if the cursor is above it, but before the last character after `$`, which `.` types again, Del deletes it, and `:` starts a command for its lines with `'<,'>`, count or not. `=` indents the selected lines as the language says, and leaves visual mode. `gq` has the language server format what's selected, as one undo step. |
| <kbd>Ctrl</kbd>+<kbd>V</kbd> | Selects a block of columns, from the corner it starts at to the cursor; `v`, `V`, and <kbd>Ctrl</kbd>+<kbd>V</kbd> switch between the kinds of selection. Columns are as drawn: a tab or wide character at a corner is in whole, and one an edge cuts is split into spaces, as Vim does. `o` goes to the opposite corner, `O` to the other corner on the same line, and `$` takes every line to its end. `d`, `x`, `X`, `y`, `Y`, `c`, `r`, `~`, `u`, `U`, `>`, and `<` act on the block; `D` and `C` act from its left edge to each line's end, and `S` and `R` change its lines, whole, as Vim's. `I` and `A` type on the first line, and once insert mode ends, what was typed goes before the block on every line long enough to reach it, or after it, padding short lines, or after `$`, at each line's end; if someone else changed those lines meanwhile, it stays on the first and the bar says so. A line break typed goes on the first line only. `J` joins its lines and `:` starts with `'<,'>`. Each change is one undo step. In a folder's listing it changes names only, leaves `..` alone, and won't type starting on `..` or put past the end. |
| `gv` | Selects the file's last selection again, by characters, lines or a block as it was, with the cursor at the end it was at; `'<` and `'>` are its ends. Each file keeps its own, through going to another file and back. Its ends are kept as Vim keeps `'<` and `'>`, a line and a column each: lines put in or taken away above take them along, and lines `:m` moves; a line taken away leaves them at that line; `J` joining a line on takes their columns with its text; but text typed or taken away within a line, or a line broken in two with <kbd>Enter</kbd>, leaves them at their columns, unlike other marks, and an end beyond its line's end selects right past it. Each undo step keeps the file's last selection as it began, and undoing it puts that selection back, as Vim's does, keeping the one you had for a redo to put back; any other edit, even one that makes the text as it was, moves it as edits do. In visual mode, it swaps what's selected with the selection before. A selection `.` made isn't one. Terminals' normal mode has it too. |

`cw` changes to the end of the word, not up to the next word, as in Vim.

What `y` copies goes on the clipboard too. What `d` and `c` delete only goes
where `p` puts from, so deleting doesn't lose what you copied from somewhere
else.

### Insert mode

Keys type into the file. <kbd>Esc</kbd>, or <kbd>Ctrl</kbd>+<kbd>;</kbd> as in
a terminal, goes back to normal mode.

| Keys | Does |
|---|---|
| <kbd>Enter</kbd> | A new line, indented as the language says, or as the line before where it has no rules. |
| <kbd>Tab</kbd> | Indents with a tab, where the file is indented with tabs, or else with spaces to the next multiple of `shiftwidth` columns. |
| <kbd>Backspace</kbd>, <kbd>Ctrl</kbd>+<kbd>H</kbd> | Deletes the character before the cursor, joining the line to the one before at its start. |
| <kbd>Delete</kbd> | Deletes the character under the cursor. |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Deletes the word before the cursor. |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Deletes back to the end of the line's indent, or from there, to the line's start. |
| The arrows, <kbd>Home</kbd>, <kbd>End</kbd> | Move, and start a new undo step. |

A count before `i`, `a`, or `o` types what's typed that many times: `3ix`
<kbd>Esc</kbd> types `xxx`.

### The mouse

A click puts the cursor where it's clicked, or at the line's start if it's on
the line's number, and the wheel scrolls. A click beside a row a line goes on
over goes to that row's start. Moving up and down after a click aims for the
column clicked, as Neovim's cursor does, even past the end of a short line or
in the middle of a tab.

With <kbd>Alt</kbd> held, or <kbd>Ctrl</kbd> and <kbd>Alt</kbd>, dragging
selects a block of columns. Dragging selects, as in Neovim: from where the
button went down to where the mouse is, in visual mode, ready for `d`, `y`,
`c`, `>` and the rest once you let go. A double click selects a word, by the
file's own word characters, and dragging after it selects whole words; a
triple click selects the line, and dragging after it whole lines. A long line
wrapped over several rows is selected row by row, and an inlay hint is never
selected: the mouse on one takes the character it's drawn before. A click ends
the selection, and `gv` selects it again. An editor showing a folder selects
the same way, but a double click there opens what's clicked, as
<kbd>Enter</kbd> does.

Mouse-wheel scrolling is smooth across native panes, including editors,
terminal history, chats, rendered Markdown, explorers, boards and reviews.
Trackpads retain pixel precision, and keyboard navigation remains immediate.
Use `:set nosmoothscroll` to disable wheel animation, or save
`client.opt.smoothscroll = false` in `client/init.lua`. Programs that handle
their own mouse input receive wheel events directly.

Past the pane's top or bottom, the view scrolls while the button is held,
faster the further past the mouse goes. While you drag, the view keeps still
under the mouse; once you let go, `scrolloff` keeps rows around the cursor
again. If the text changes under a drag, say by an agent's edit, where it
started moves with the text.

Letting go puts what's selected on the primary selection, which a middle click
pastes, as selecting in a terminal does; not with `clipboard=unnamed`, where
`p` over the selection would put the selection itself.

A drag from insert mode selects in visual mode for a while, as Neovim's
"(insert) VISUAL" does, and the bar says `(insert) VISUAL`: once the selection
ends, by an operator, <kbd>Esc</kbd> or a click, the editor goes back to
insert mode at the cursor; `c` types over it. A binding or macro run on it
goes back to insert only once all its keys have run. `:` on it, `gra`, and
leaving the pane leave it in normal mode.

## Known limits

- A window isn't told of changes that came back until it opens their file, or
  `:recover`; a `[No Name]` that came back isn't in `:Buffers`.
- No `breakat` or `breakindentopt`. Typing near the start of a line of
  megabytes that isn't plain ASCII takes a moment, as the rest of it is laid
  out again.
- Lightbulbs or gutter hints for lines with code actions, and typing to narrow
  the menu of them.
- `:jumps`, to list them.
- A server installed some other way than through clackatoo, after a file of
  its was first opened, isn't looked for again until the daemon restarts.
- `indentkeys` and `indentexpr` as options. <kbd>Esc</kbd> on a line with
  nothing but its indent leaves the indent. `.` doesn't wait for a repeated
  new line's indent to be put right.
- In Python, `else:` typed under the body it ends stays where it's typed,
  where Neovim takes it back a step: the grammar clackatoo installs reads that
  line differently.
- Languages inside others, such as code in a Markdown file, or CSS in HTML:
  they're colored as the outer language only. Code in a Markdown file is
  colored only when it's
  [rendered](~clackatoo/reference/editor-view#rendered-markdown).
- Selecting, copying or searching rendered Markdown, images in it, scrolling a
  wide table in it sideways, and a link's heading in another file.
- Choosing the colors. They're the palette's named colors where there's one to
  suit, so they follow the terminal's.
- Options for one pane only: `:set` sets them for every pane.
- `clackatoo edit` opens the tab, but the window doesn't switch to it
  ([`clackatoo open`](~clackatoo/reference/cli#open) does).
- `\1` inside a pattern, `\@` lookaround, `\&`, and `\%V`; `\zs` and `\ze`
  inside a group; `:g` inside `:g`, or `:s` with `c` under `:g`; a `:g` that
  moves more than one line, as `:g/^/m0` does, leaves marks on them where they
  were, as its moves are made as one edit. There are no `'[` and `']` marks.
  Macros don't record pastes. A mark on deleted text stays where the text was,
  rather than going.
- Sideways scrolling while dragging with `nowrap`. In visual mode, `D`, `X`,
  `C`, `S` and `R` don't act on whole lines as Vim's do, `X`, `R` and `S`
  don't act on a block as Vim's do, and `.` doesn't make `gq` again.
- A change typed right as the connection to the daemon drops can be lost.
- Where `u` puts the cursor differs from Vim in a few cases: after `A` on a
  selection made from its end back to its start, after `d` with a search, and
  after `:d` or `:m` on lines selected in visual mode, which Vim starts from
  the selection's first line's start.
