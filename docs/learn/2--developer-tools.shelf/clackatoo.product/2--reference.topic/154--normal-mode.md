---
title: Normal Mode in a Terminal
type: reference
description: Moving through a terminal's history in normal mode, selecting and copying, text objects, commands, searching, and the keys of list panes.
related:
  - clackatoo/reference/window
---

In normal mode, a cursor of clackatoo's own moves through a terminal's screen
and history with Vim's keys.

## Normal mode

Normal mode has a cursor of its own, a yellow block, which moves through the
terminal's history and screen as one document. It starts where the program's
cursor is, and the program's cursor is hidden until you return to insert mode.

| Keys | Move |
|---|---|
| `h` `j` `k` `l`, arrows | <kbd>Left</kbd>, down, up, right. Moving up and down returns to the column you started from after passing shorter lines. |
| <kbd>Shift</kbd>+<kbd>Left</kbd> <kbd>Shift</kbd>+<kbd>Right</kbd>, <kbd>Ctrl</kbd>+<kbd>Left</kbd> <kbd>Ctrl</kbd>+<kbd>Right</kbd> | As `b` and `w`, and `B` and `W`, as in Vim. <kbd>Ctrl</kbd>+<kbd>Home</kbd> is `gg`, <kbd>Ctrl</kbd>+<kbd>End</kbd> goes to the last line's last character, and <kbd>Shift</kbd>+<kbd>Up</kbd> and <kbd>Shift</kbd>+<kbd>Down</kbd> page up and down. In insert mode they all go to the program. |
| `w` `b` `e` | To the next word's start, this or the previous word's start, this or the next word's end. Words are runs of letters, digits and `_`, or runs of other characters; both cross line ends. |
| `W` `B` `E` | The same, for anything between blanks. |
| `0` `^` `$`, <kbd>Home</kbd> <kbd>End</kbd> | The line's first column, its first character that isn't blank, its last character; `2$` the next line's last, a count that many lines down less one, as Vim's. |
| `+` `-` `_` | The next line's first character that isn't blank, the line before's, this line's; with a count, that many lines on, back, or down less one, as Vim's do. With `y`, they take whole lines. <kbd>Enter</kbd>, <kbd>Space</kbd> and <kbd>Backspace</kbd> aren't motions in a terminal, which is typed into with them, but `y` takes them as Vim's does: `y<CR>` copies this line and the next. |
| `g_` `\|` | The line's last character that isn't blank; the column a count says. |
| `ge` `gE` | The end of the word before, or of the run of anything but blanks before. |
| `gj` `gk` `g0` `g$` | As `j`, `k`, `0` and `$`: a terminal's lines don't wrap. |
| `gg` `G` | The oldest line of history, the screen's last line, or with a count, that line, keeping the column, as Neovim's do; at its first character that isn't blank with the [option](~clackatoo/reference/config#options) `startofline`. |
| `gx` | Opens the link under the cursor, as <kbd>Ctrl</kbd>+click does. |
| `f` `t` `F` `T`, `;` `,` | To the next of the character typed after them on the line, or right before it, or back to the one before, or right after it; `;` finds it again, and `,` the other way, as Vim's do. The window keeps one last find, as Vim does, so `;` finds again what `f` found in an editor, a chat or another terminal. With `y`, they take to it: `yt)`. |
| `%` `{` `}` | The bracket that pairs with the one under the cursor, or with a count, the line that far through, `50%` half way; the blank line before or after the paragraph. |
| `(` `)` | The start of this sentence or the one before, or of the next, as Vim's go: a sentence ends at `.`, `!` or `?` before a blank or a line's end, and at an empty line. |
| `H` `M` `L` | The top, middle and bottom lines in view, keeping the column as `gg` does. |
| `5k`, `3w`, `10G`, `2gg` | A count before a motion repeats it, as in Vim; before `G` or `gg`, it goes to that line, counting from the oldest line of history. `3H` and `3L` count from the view's top and bottom, no further than the other, but after `y` they go on past it, as far as history goes, as Vim's do: `y99L` takes up to the 99th line from the view's bottom. |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> <kbd>Ctrl</kbd>+<kbd>U</kbd> | Half the view down or up, rounded down, view and cursor together. A count sets how far they go from then on, a whole view at most, as Vim's does. |
| <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd> <kbd>Page Up</kbd> | The view down or up, less two lines, so two lines of what was in view stay where they fit, and the cursor to its top line, or its bottom line going up, as Vim's does. At the live screen, or history's start, nothing moves. A count goes that many pages. <kbd>Ctrl</kbd>+<kbd>B</kbd> is [the window's](~clackatoo/reference/workspaces#ctrl-b). |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> <kbd>Ctrl</kbd>+<kbd>Y</kbd> | The view a line down or up, or as many as a count says, with the cursor kept in it. |
| `zt` `zz` `zb` | The view scrolled to put the cursor's line at its top, middle or bottom, as far as history goes and no further than the live screen. With a count, that line, counted as `G` counts, the cursor going to it in the column it's in, as Vim's does. |

Moving past the top of the view scrolls into history. In normal mode the view
holds still while output arrives, so the cursor stays on its text. On the
alternate screen the cursor moves over the screen only.

The keys are read as an editor reads them, so a command half typed, as `y3` or
`"a`, shows at the start of the bar until it's done, as Vim's `showcmd` shows
it. A command a terminal doesn't take, as `dw`, `x`, `p` or `"ap`, does
nothing, all its keys spent, as in a file Vim won't change: so after `d`, the
next key is the command's too, `:` included. <kbd>Esc</kbd> after keys half
typed only forgets them, as in Vim: `vg` or `vi` and <kbd>Esc</kbd> is still
in visual mode, and a second <kbd>Esc</kbd> leaves it. <kbd>Esc</kbd> after
only a count or a register, as `v3` or `v"a`, does what <kbd>Esc</kbd> alone
does, and leaves visual mode.

### Selecting and copying

| Keys | Does |
|---|---|
| `v` | Selects characters, from the cursor to wherever it moves. |
| `V` | Selects whole lines. |
| <kbd>Ctrl</kbd>+<kbd>V</kbd> | Selects a block of columns. `$` takes every line to its end, and `O` goes to the other corner on the same line. |
| `o` | Goes to the other end of the selection, so that end moves instead. |
| `gv` | Selects the last selection again, as it was; in visual mode, swaps the two. |
| `y`, <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Yanks the selection into the registers, or with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> onto the clipboard and into `"+` as shown, by characters, and returns to normal mode. |
| `Y`, in visual mode | Yanks the lines selected, whole, or a block as it is. |
| `yy` | Copies the cursor's line, or with a count, as many lines from it: `3yy`. `y2w` and `2yj` take counts too. |
| `Y` | Copies to the line's end, as `y$` does, as Neovim's `Y` does; with a count, to the end of as many lines. For Vim's, the line, map it: `client.keys.map("n", "Y", "yy")`. |
| `y` and a motion | Copies what the motion moves over: `yw` to the next word, `y$` to the line's end, `yj` this line and the next, `ygg` up to the top, `yH` up to the view's top. |
| `y` and a [text object](#text-objects) | Copies the object around the cursor: `yiw` the word, `yi"` what's in the quotes, `yib` a command's output. |
| `"a` and any of these | Keeps what's copied in the register named, as in an editor: `"ayy`, `"Ayw` adds to `"a`, `"+yib` puts a command's output on the clipboard. A register before anything else does nothing. |
| <kbd>Esc</kbd> | Drops the selection. |

A selection by characters lights each line to a cell past its text, which is
what `y` copies; `V` lights whole lines, the pane's width. `y` on a block
copies a line for each row, without the blanks at their ends, and never joins
a wrapped line to the next. A copy stops at 100,000 lines or 4 MiB, and says
so. A daemon too old to copy blocks copies from corner to corner, and says so.

Any motion extends a selection, including `gg`, `G` and searches. What you
copy lights up for a moment, so you can see what went. Copied text is what the
program printed: a line that wrapped only because it reached the window's edge
comes out as one line, and blanks at the ends of lines are left out.

What `y` copies goes into the
[registers](~clackatoo/reference/registers-macros-marks#registers) exactly as
an editor's yank does: into the unnamed register, which `p` in an editor puts,
and `"0`, or into the register named before it; and, as the
[option](~clackatoo/reference/config#options) `clipboard` says, onto the
clipboard, as it is unless you change it, or the primary selection. With
`clipboard=none` it stays in the window's registers, and `"+y` or
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> puts it on the clipboard. It
keeps its kind, so `p` puts lines copied by lines as lines below the cursor's,
and a block as a block. Lines copied by lines end with a newline, as an
editor's do and as Vim's `"+yy` does. Pasted into a terminal, that never runs
the line: a shell that takes bracketed paste, as bash, zsh and fish do, takes
it as text, and to any other program the newlines that end a paste aren't sent
(see [Typing](~clackatoo/reference/typing#typing)), though lines before the
last still run.

### Text objects

After `y`, or in visual mode, `i` or `a` and one more key pick out a piece of
text around the cursor, as in Vim. `i` takes what's inside, `a` takes it all.
In visual mode, the object becomes the selection.

| Keys | Picks out |
|---|---|
| `iw` `aw` | A word, and with `a` the blanks after it (or before it, at the end of a line). A count takes that many, over lines too, a run of blanks counting as one with `iw`; in visual mode, with more than a character selected, it takes on another, as Vim's. |
| `iW` `aW` | The same, for anything between blanks. |
| `i"` `a"`, `i'` `a'`, `` i` `` `` a` `` | What's between the quotes on the cursor's line, as Vim's pairs them: around a quote the cursor's on, or else from the quote before it to the next, or the next pair; with `a`, the quotes too, and the blanks after or before. In visual mode `i"` again takes the quotes, and `a"` again the next string too. |
| `i(` `a(`, `i[` `a[`, `i{` `a{`, `i<` `a<` | What's between the brackets around the cursor, across lines, or with none around it the next pair after it, as Neovim's; with `a`, the brackets too. A count goes out that many pairs; in visual mode, typed again, they take the next pair out. The closing bracket works as well. |
| `ip` `ap` | A paragraph, or the blank lines the cursor is on, as whole lines; with `a`, the blank lines after it too, or before it if there are none after, as Vim's. A count takes that many. |
| `is` `as` | A sentence, as `(` and `)` find them; with `a`, the blanks after it too, or before it if there are none after, as Vim's. A count takes that many, a run of blanks counting as one with `is`; in visual mode, with more than a character selected, they take the selection on. |
| `it` `at` | What's between the HTML or XML tags around the cursor, or with the cursor on a tag, its own; with `a`, the tags too, as Vim's. A count goes out that many tags; in visual mode, typed again, they take the tags too, then the next pair out. A closing tag on a line of its own leaves its line break to `it`, as in Vim. |
| `ib` `ab` | A [command's](#commands) output; with `a`, its prompt and command line too. |

In Vim, `b` is another name for `(`. Here it's the command, the more useful
thing in a terminal, and `(` still means parentheses.

With the cursor past the end of a line's text, as it is after what you've
typed at a prompt, objects are found from the line's last character, as in
Neovim's terminal: `yiw` copies the word you typed last.

### Commands

When the shell marks its prompts and commands, clackatoo knows where each
command's prompt and output are, and how it exited.

| Keys | Does |
|---|---|
| `[[` `]]` | Moves to the previous or next command's prompt. |
| `yib` | Copies the output of the command the cursor is in. |
| `yab` | Copies the command with its prompt and output, ready to paste into a bug report. |
| `vib` `vab` | Selects them. |

A command's output runs from its first line to the line before the next
prompt, leaving out blank lines at the end. If the command printed nothing,
`yib` says `no output`. If the cursor isn't in a command, the bottom bar says
`no block here`.

Which shells mark their commands:

- **fish** 4 and later, by itself. clackatoo also runs a small script once
  your startup files have run (`fish -C`), which marks each command again with
  the terminal's secret, so it can be offered after a restart. As the script
  runs after your startup files, a program they start, such as a prompt's
  daemon, starts while the secret's file is still there, and could read it.
- **bash**, when it's your shell (`$SHELL`) and clackatoo starts it. clackatoo
  starts it with a startup script of its own, which reads your `~/.bashrc`
  first, then adds the marks to `PS1`, `PS0` and `PROMPT_COMMAND`. The command
  goes with its mark, from bash's history. bash before 4.4, or with
  `shopt -u promptvars` in your `~/.bashrc`, can't sign its commands, so it
  keeps no secret, and offers nothing after a restart; nor does one that stops
  signing later, at its next prompt, as when `promptvars` is turned off or
  `PS0` set afresh. bash started any other way, such as
  `clackatoo new -- bash`, is left as it is.
- **zsh**, when it's your shell and clackatoo starts it. clackatoo points
  ZDOTDIR at a `.zshenv` of its own, which puts ZDOTDIR back, reads your
  `.zshenv`, and adds its hooks once your `.zshrc` has run, so oh-my-zsh,
  powerlevel10k (instant prompt included) and a ZDOTDIR of your own all work.
  A shell started with `exec zsh`, or inside another, isn't marked.
- fish before 4 doesn't.

Commands are kept through an [upgrade](~clackatoo/reference/cli#upgrade).

A command that failed ends its prompt's line with a small red `✗` and its exit
status, as `✗ 2`, two cells after the line's text, on the screen and in
history. A line too long for it, as one a prompt on the right takes to the
edge, isn't marked; a prompt of two lines is marked on its first. The mark
isn't part of the text: copying, searching, and selecting leave it out. The
[`blockmarks` option](~clackatoo/reference/config#options) marks every
finished command, the ones that worked with a faint `✓` (`all`), or none
(`none`). Marks are kept through an upgrade, and need a daemon of protocol 86
or later.

### Searching

| Keys | Does |
|---|---|
| `/` | Searches forward from the cursor. Type the text, shown in the palette, then <kbd>Enter</kbd>; <kbd>Enter</kbd> with nothing typed searches for the last search again, this way. <kbd>Esc</kbd> cancels. It's edited as the command line is, and <kbd>Up</kbd> and <kbd>Down</kbd> go back through the searches typed before, in every pane alike. A count before it goes to that match, as `3/x` goes to the third. |
| `?` | Searches backward. |
| `n` `N` | Searches again, the same way or the other way. A count goes that many matches, never round more than once, in one answer from the daemon; an older daemon is asked a match at a time. |
| <kbd>Esc</kbd> | Puts out the last search's lit matches, until the next search, `n` or `N`; in visual mode, leaves it, keeping them. |

A search ignores case unless the text has a capital letter in it, as the
[`ignorecase` and `smartcase` options](~clackatoo/reference/config#options)
start; set them to change it. It covers all of history and wraps around at
either end, saying `search wrapped to top` (or `to bottom`) when it does. The
cursor moves to the match, which is highlighted, and every other match on the
screen or the history in view is lit more faintly, as Vim's `hlsearch` lights
them, one after another along each line, never overlapping, until
<kbd>Esc</kbd> or `:noh`, and again at the next search, `n` or `N`. Only the
lines in view are looked through for them, at most 512 on a line, and only in
normal mode: in insert mode the program has the screen, and only the match
found stays lit. The [`hlsearch` option](~clackatoo/reference/config#options)
off lights only the match found. If there's none, the bottom bar says
`not found`.

While you type the search, the cursor goes to the match it would find from
where you started, the view follows, and its matches are lit, as Neovim's
`incsearch` shows them, in an editor too (see
[Shown as you type](~clackatoo/reference/editor-keys#shown-as-you-type)).
<kbd>Esc</kbd> puts the cursor and the view back; <kbd>Enter</kbd> keeps them,
and is the search. The daemon is asked after each key, but only once it has
answered the last, so typing fast never queues searches, and the match shown
stays until the next answer moves it; nothing you type waits for these
answers. With no match, the cursor goes back where it started. A count before
`/` isn't shown until <kbd>Enter</kbd>. With the
[`incsearch` option](~clackatoo/reference/config#options) off, nothing moves
until <kbd>Enter</kbd>.

Copying and searching both happen in the daemon, which holds all of history,
so neither waits for the window to fetch the lines involved. Keys you type
after a search, `n`, `N`, `[[` or `]]` wait for its answer, so they act where
it leaves the cursor. <kbd>Esc</kbd> drops the keys typed after the search,
and stops a macro; the search still lands, and then <kbd>Esc</kbd> acts,
putting out what it lit, as Vim's `/foo` <kbd>Enter</kbd> <kbd>Esc</kbd> does.
<kbd>Ctrl</kbd>+<kbd>C</kbd> waits behind the search too. If the daemon takes
more than 5 seconds to answer, the keys waiting go on without it, so nothing
you typed for the program is lost, and the bar says so.

### Lists

An [explorer](~clackatoo/reference/explorer), a
[board](~clackatoo/reference/boards) and a
[review](~clackatoo/reference/agent-changes#reviewing-its-changes) read keys
as terminals and editors do, with the cursor on an item, a file, an entry or a
line, rather than a character. They share these, and add their own:

| Keys | Does |
|---|---|
| `j` `k`, <kbd>Down</kbd> <kbd>Up</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd> <kbd>Ctrl</kbd>+<kbd>P</kbd> | The next item or the one before; a count goes that many. |
| `gg` `G`, <kbd>Home</kbd> <kbd>End</kbd> | The first item or the last; with a count before `gg` or `G`, the item numbered so, counting from 1. |
| `H` `M` `L` | The top, middle, and bottom items in view; `3H` and `3L` count from the view's top and bottom. |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> <kbd>Ctrl</kbd>+<kbd>Y</kbd> | The view an item down or up, the cursor kept in it. |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> <kbd>Ctrl</kbd>+<kbd>U</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd> <kbd>Page Up</kbd> | Half the items in view, view and cursor together, or a page, as far as in a terminal or an editor. <kbd>Ctrl</kbd>+<kbd>B</kbd> is [the window's](~clackatoo/reference/workspaces#ctrl-b). |
| `zt` `zz` `zb` | The cursor's item at the view's top, middle or bottom. |
| `/` `?` `n` `N` | Searches forward or back, with the editor's patterns and case options, round from the end to the start: the names shown, a board's entries, a review's lines, files, and comments. A count before `/`, `?`, `n` or `N` goes that many matches, round no more than once. |

A search is typed as every pane's is (see [Searching](#searching)): edited as
the command line is, <kbd>Up</kbd> and <kbd>Down</kbd> going back through the
searches typed before, in every pane. While you type it, the cursor goes to
the item it would find from where you started, as
[`incsearch`](~clackatoo/reference/config#options) shows it, and
<kbd>Esc</kbd> puts the cursor and the view back. What it matches is lit, as
[`hlsearch`](~clackatoo/reference/config#options) lights it: in an explorer,
the names; in a board, the text of its entries; in a review, its lines, files'
names and comments; the first match in the item the search went to brighter,
until <kbd>Esc</kbd> in normal mode or `:noh`. A search that finds nothing
says so, and stops a macro.

A command half typed, as `3` or `g`, shows at the start of the bar, and
leaving the pane forgets it. Leaving also cancels an unfinished search and
restores its starting cursor and view. File-name prompts, file-operation
confirmations, and board-note drafts stay open when focus leaves. A count
before <kbd>Ctrl</kbd>+<kbd>D</kbd> or <kbd>Ctrl</kbd>+<kbd>U</kbd> sets the
distance for subsequent half-page moves in that pane, until its height
changes. A key they don't take, as `d` or `"`, waits for the rest of its
command, then does nothing. <kbd>Ctrl</kbd>+<kbd>?</kbd> lists the keys of the
pane you're in, read from how it reads them.
