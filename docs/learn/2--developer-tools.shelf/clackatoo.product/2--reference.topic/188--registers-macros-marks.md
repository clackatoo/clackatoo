---
title: Registers, Macros, and Marks
type: reference
description: The editor's registers and the clipboard, recording and running macros, and marks.
related:
  - clackatoo/reference/editor
---

Registers hold what you yank and delete, macros replay keys you recorded, and
marks remember places, as in Vim.

## Registers

What's yanked and deleted is kept in registers, as in Vim. `"` and a
register's name before a command keeps text in it, or puts it from it: `"ayy`,
`"ap`, `"_dd`. A count can go either side: `2"a3yy` copies six lines.

| Register | Holds |
|---|---|
| `""` | What was last yanked or deleted: what `p` puts when no register is named. |
| `"0` | The last yank with no register named. |
| `"1` to `"9` | Deletes of a line or more, newest first. `"1p` then `.` puts `"2`'s, and so on back. |
| `"-` | The last delete within a line, with no register named. |
| `"a` to `"z` | Whatever's kept in them. `"A` to `"Z` add to them, on a line of their own if either is lines. |
| `"_` | Nothing: what goes to it is thrown away, and the others keep theirs. |
| `"+` `"*` | The clipboard, and the primary selection that a middle click pastes. |

A terminal's normal mode yanks into the same registers, `"a` before `y` too,
so `p` puts a line copied from a terminal as a line, and so does a chat's (see
[Yanking](~clackatoo/reference/chat-view#yanking)). Text selected with the
mouse there goes only to `"*`, as in Vim.

A block is kept as one, its rows a line each, and `p` and `P` put it back as
one: a row on each line from the cursor's, padded with spaces where text
follows, and on new lines past the end. `:Registers` marks it `b`.

As the option `clipboard` says, the unnamed register goes along with the
clipboard by default, as Neovim's `unnamedplus` does: what's yanked or deleted
can be pasted anywhere, and `p` puts what was last copied anywhere. Deleting
nothing, as `D` on an empty line, leaves them alone. The window reads the
clipboard and the primary selection when it gets the focus, off to one side,
so a program slow to answer for them never holds it up. In visual mode, `p`
and `P` put in place of what's selected; `p` keeps what was selected, as a
delete, and `P` doesn't. `:Registers` lists them by the cursor.

## Macros

`q` and a register records the keys you press, until `q`; the bar says
`recording @a` meanwhile, in every pane you move to and below a search or
command line you type. Every key is kept, whatever has it: the editor, a
terminal, the command line, the finder, or a binding. So a macro can run `:w`,
move to another pane with <kbd>Ctrl</kbd>+<kbd>L</kbd>, or open a file with
`<Space>pf`. `@` and the register types the keys again, as if you pressed them
now, through your bindings as they are now; `@@` types the last one again, and
a count that many times: `100@a`. Each file a macro changes takes one `u` to
undo. The keys are kept as text, written as bindings are
(`A;<Esc>:w<CR><C-l>j`), so `"ap` puts them in the file to change, and `"ay$`
keeps them again. An item picked from the completion menu is kept as the text
it put in, and a snippet as the text you left in it, not the keys that filled
it in.

A key that fails stops the macro, as in Vim: a motion that can't move, a
search that finds nothing, an empty register, a command that isn't one or is
refused, <kbd>Enter</kbd> in the finder with nothing picked, or a write the
daemon refuses. So a macro ending `j@a` goes down the file and stops at its
end. When a key starts something that takes a moment, such as `gd`, `:w`,
opening a file or a search in the finder, the macro waits for it before its
next key, and keys you press meanwhile wait behind it. After 5 seconds it
gives up and says what it waited for; <kbd>Esc</kbd> stops it at once, and so
does moving to another pane yourself while it waits. Recording and replay work
in every text and list pane in normal mode; see
[macros and marks across panes](~clackatoo/reference/window#macros-and-marks-across-panes).
A macro typing through a terminal's normal mode or a chat stops there the same
way, at a motion that goes nowhere, a search that finds nothing or a command
they refuse; keys after a terminal's search or `[[` wait for the daemon's
answer, as they wait for a chat's search.

## Marks

`m` and a letter marks the cursor's place: `a` to `z` for each file on its
own, `A` to `Z` anywhere. `` `a `` goes to it, and `'a` to its line; both are
jumps, and an operator takes them: `d'a` deletes the lines from here to the
mark's. A mark in another file opens it in place, as `:e` does. `` `. `` goes
to your last change. Marks move with the text, whoever edits it, and lines
`:m` moves take theirs with them; once no pane shows a file, its marks keep
their lines. `:Marks` lists them in the finder. `'<` and `'>` are the last
selection's start and end, which `gv` selects again, kept as Vim keeps them
(see `gv` below); by lines, `'<` is the first line's start and `'>` the last's
end. They follow lines `:m` moves too, so the binding
`client.keys.map("v", "J", ":m '>+1<CR>gv=gv")`, the keys of a well-known
Neovim mapping, moves the lines selected down a line at a time, and keeps them
selected.
