---
title: Substituting and Lines
type: reference
description: The :s and :g commands, their patterns and flags, and commands on ranges of lines.
related:
  - clackatoo/reference/editor
---

`:s` replaces what a pattern matches, and `:g` runs a command on every line
that matches, as in Vim.

## Substituting

`:s/pattern/replacement/flags` replaces what matches on the cursor's line, or
in a range typed before it: `:%s/old/new/g` replaces every match in the file.
Patterns are Vim's, as `/` reads them too: `\(` `\)`, `\|`, `\+`, `\{n,m}`,
`\<` `\>`, `\s` `\d` `\w` and their kin, `\v` for fewer backslashes, `\zs` and
`\ze`, `\c` and `\C`. Case is matched as `ignorecase` and `smartcase` say. An
empty pattern is the last search's, and `:s` makes its pattern the last
search, for `n`.

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
| `g` | Every match on a line, not only the first. |
| `c` | Asks about each in the bottom bar: `y` replaces it, `n` leaves it, `a` replaces the rest, `l` replaces it and stops, `q` or <kbd>Esc</kbd> stops, <kbd>Ctrl</kbd>+<kbd>E</kbd> and <kbd>Ctrl</kbd>+<kbd>Y</kbd> scroll. |
| `i` `I` | Ignores case, or matches it, whatever the options say. |
| `n` | Counts the matches, replacing nothing. |
| `e` | Says nothing if nothing matches. |
| `&` | The last `:s`'s flags, with these. |
| `r` | With no pattern, the last search's rather than the last `:s`'s. |

In the replacement, `&` and `\0` are the match, `\1` to `\9` its groups, `\r`
and `\n` a line break, `\t` a tab, `~` the last replacement, and `\u` `\l`
make the next character a capital or small, `\U` `\L` the rest, until `\E`.

`&` does the last `:s` again on the cursor's line, without its flags; `g&` on
every line, with them and the last search's pattern. `:&&` keeps the flags,
and `:~` uses the last search's pattern. A count after it does that many lines
from the range's last: `:s/x/y/ 3`.

Each `:s` is one undo step, however many lines it changes. While you type it,
what it would do shows in place, as Neovim's `inccommand` shows it, with the
replacements lit; the file isn't touched until <kbd>Enter</kbd>, and
<kbd>Esc</kbd> puts the view back. A `:g` being typed lights its pattern's
matches. When nothing matches, the bar says so and a macro stops, as in Vim.

## Lines

| Command | Does |
|---|---|
| `:g/pattern/command` | Does the command on each line that matches: `:g/TODO/d`, `:g/x/normal Atext`, `:g/x/s//y/`, `:g/^/m0` turns the file upside down. `:v` and `:g!` on each line that doesn't. All of it is one undo step. Marks move as lines are deleted. |
| `:d` `:y` | Deletes or copies the lines, into a register if one's named: `:2,5d a`. |
| `:m 0` `:t $` `:co .` | Moves or copies the lines after the line given; 0 is the top. Lines moved take their marks, and the last selection, with them, and so does undoing it. |
| `:normal keys` | Types the keys on each line of the range, as in normal mode, then <kbd>Esc</kbd>. |
