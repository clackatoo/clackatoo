---
title: The Editor's View
type: reference
description: Git signs, the marks for agents' changes, line numbers, tabs and indenting, wrapping and the view, and rendered Markdown.
related:
  - clackatoo/reference/editor
---

Beside its text, an editor shows git's signs, the lines agents changed, and
line numbers, and it can show Markdown rendered.

## Git signs

When a file is in a git repository, a column left of the line numbers shows
how it differs from the file in git's index, as gitsigns.nvim does: a green
bar beside lines added, a blue bar beside lines changed, and a red line under
a line where lines after it were deleted. The signs follow your typing, and
update a moment after you pause, or at once when something stages changes, in
a terminal or anywhere else.

| Key | Does |
|---|---|
| `]c` `[c` | The next change, or the one before, round the file's ends. A jump, so <kbd>Ctrl</kbd>+<kbd>O</kbd> comes back. |
| `<Space>hp` | Shows the change at the cursor: its lines as they were, and as they are. |
| `<Space>hr` | Puts the change at the cursor back as git's index has it. `u` undoes it. |
| `<Space>hs` | Stages the change at the cursor, and only it, as `git add -p` would. |

`:gitsigns preview_hunk`, `reset_hunk`, `stage_hunk`, `next_hunk` and
`prev_hunk` do the same, so `client.cmd` can bind them to other keys. A file
with conflicts is compared with your side. A file git converts, such as one
whose line endings it changes, or that Git LFS keeps, shows its signs but is
staged in a terminal, not a change at a time. Files that git doesn't track, or
that are bigger than 8 MiB, have no signs.

## Agents' changes

Lines an agent changed and you haven't reviewed have a bar in a column right
of git's signs, in the chat's color, shown while a file has such lines
(`agentsigns`). They follow your typing; typing a line back as it was clears
it.

| Key | Does |
|---|---|
| `<Space>ga` | Keeps the agent's change at the cursor |
| `<Space>gx` | Rejects it: takes it out, unless you've changed its lines since |
| `<Space>c` | Comments on the line, for the agent |
| `<Space>gr` | Opens a review of the chat's changes (see [Agents](~clackatoo/reference/agent-changes#reviewing-its-changes)) |

`:blame` writes after each line an agent changed which chat and turn changed
it, and `:blame` again stops.

## Line numbers

Line numbers run down the pane's left. To begin with, the cursor's line shows
its own number, in clackatoo's yellow, and every other line shows how far it
is from the cursor, so the count for a motion such as `5j` or `3dd` can be
read off the screen. The numbers widen as the file grows, and a pane too
narrow for them to leave the text 12 columns shows none.

Three [options](~clackatoo/reference/config#options) choose them, as in Vim.
Set them in the config, or as you go with `:set`:

| Options | Show |
|---|---|
| `number` and `relativenumber`, as they start | The cursor's line's number, and the others' distances from it. |
| `number` alone (`:set nornu`) | Every line's own number. |
| `relativenumber` alone (`:set nonu`) | Distances, with 0 on the cursor's line. |
| Neither (`:set nonu nornu`) | No numbers. |

`numberwidth` is the fewest columns they take, with the space after them: 4 to
begin with.

## Tabs and indenting

A tab is drawn to the next column that's a multiple of `tabstop`: 4 to begin
with. `>>`, `<<` and <kbd>Tab</kbd> in insert mode indent by `shiftwidth`
columns, which is `tabstop`'s while it's 0, as it starts.

A file keeps the kind of indent it has, whatever the options say. `>`, `<` and
<kbd>Tab</kbd> indent with tabs or spaces as the first indented line among
those they change does, or else the nearest indented line around them.
`expandtab`, on to begin with, decides only where no line nearby is indented
yet: spaces while it's on, tabs while it's off.

`>` and `<` write each line's indent afresh, as many tabs as fit and spaces
for the rest, or all spaces. Empty lines stay empty. Changing `tabstop` with a
file open redraws its tabs, and the cursor and selection stay on their
characters.

A new line is indented as its language says, where its pack has the rules for
it. It takes the indent of the line before at once, so typing never waits, and
is put right a moment later, when the daemon has worked it out: usually within
a millisecond or two. What you type meanwhile stays after the indent, but if
you've typed in the indent itself, as a <kbd>Tab</kbd> or <kbd>Backspace</kbd>
does, or made another change, it's left as you made it. A line where `}`, `]`,
or `)` is typed first is worked out again. So is a line that starts with
`else`, `elif`, `elsif`, `elseif`, `except`, `finally`, `catch`, `rescue`,
`ensure`, `case` or `default`, or with `end`, `fi`, `done` or `esac`, which
end blocks, once you've typed the word: the key after it, such as a space,
<kbd>Tab</kbd>, `{`, `:` or <kbd>Enter</kbd>, or <kbd>Esc</kbd>, finishes it.
Only a word you type counts, not one that was there or was pasted, nor a
longer word such as `elsewhere`. The rules decide the indent, so a word that
isn't one of the language's gets what `=` would give the line. `:set nosi`
turns this off, for new lines and these words, and a new line keeps the line
before's indent. `=` indents by the rules either way.

```text
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

The marks are drawn behind the text, and the selection over it. See
[the options](~clackatoo/reference/config#options) for setting them.

## Rendered Markdown

`<Space>tm`, or `:markdown`, shows a Markdown file (`.md`, `.markdown`)
rendered, in place of its text: headings, lists, task lists, quotes, tables,
rules, links, and code colored as the editor colors its language, in a column
of prose down the pane. Again, it shows the text. Either way it keeps your
place: the rendered view opens with the block holding the line at the top of
the text's view at its top, and the text comes back with the top block's first
line at the top. It's a view of the file: what anyone changes, in another pane
or window or an agent's chat, shows as it's made.

| Key | Does |
|---|---|
| `j` `k`, `gj` `gk`, <kbd>Ctrl</kbd>+<kbd>E</kbd> <kbd>Ctrl</kbd>+<kbd>Y</kbd> | A line, or as many as a count says |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> <kbd>Ctrl</kbd>+<kbd>U</kbd> | Half a view, rounded down, or as far as a count before one set, a view at most |
| <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd>, <kbd>Page Up</kbd> | A view less two lines, which stay in view; a count goes that many |
| `gg` `G` | The top, the end; `10G` or `10gg`, the block holding line 10 |

The keys are read as the text's are, so a count or `g` shows in the bar while
it's typed, and <kbd>Esc</kbd> forgets it. The wheel scrolls it too. Any other
key is the text's: the text comes back where the view was, and the key does
there what it does, so `i` types at the start of the top block, and `/`
searches the text. `:w`, `:q`, and the rest of the command line are the same
either way.

Selecting a link opens it: a web address with the desktop's opener, as a
terminal's links open, asking first, with where it goes shown whole, if it
isn't a plain `http://` or `https://` address or a mail address; a file in the
editor, in place of this one, as `:e` would, written from the Markdown file's
folder; and `#` and a heading scrolls to that heading, named as GitHub names
them. An image shows its text, after `▣`.

The [option](~clackatoo/reference/config#options) `markdownview` says how a
Markdown file opens: as its text, `raw`, to begin with, or `rendered`.
