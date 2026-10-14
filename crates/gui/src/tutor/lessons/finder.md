# The finder and diagnostics

Picking a file from the project, grepping its lines, the files open, what the language servers say is wrong, and the symbols they know of, all in the finder.

The finder looks from the project's root: the one its language server was started for, or its git repository, or else its folder. Here, that's the lesson's scratch directory.

```practice alpha.txt
This is alpha.txt, the practice file.
```

```practice beta.txt
This is beta.txt.
Somewhere in it is the word kiwi.
```

```practice gamma.txt
This is gamma.txt.
```

## Find a file

In normal mode, `<Space>pf` opens the finder on the project's files, those git doesn't ignore, leaving out hidden ones.

Press `<Space>pf`.

```check
finder files
```

## Pick one

What you type matches as fzf's does: letters in order, `'word` exactly, `^word` at the start, `word$` at the end, and `!word` leaves out. Down and Up, or Ctrl+N and Ctrl+P, pick. Enter opens it in place of the focused pane's file, Ctrl+V and Ctrl+X beside or below, and Ctrl+T in a new tab. Esc closes the finder.

Type `bet`, and press Enter.

```check
file "beta.txt"
```

## Come back

Opening a file from the finder is a jump, so Ctrl+O comes back, and Ctrl+I goes forward again.

Go back to `alpha.txt` with Ctrl+O.

```check
file "alpha.txt"
```

## Grep

`<Space>pgf` greps the project's files as you type: a regular expression, or plain text if it isn't one. Enter goes to the line picked.

Press `<Space>pgf`, type `kiwi`, and press Enter.

```check
file "beta.txt"
at "kiwi"
```

## The files open

`<Space>?` lists the files open, with `[+]` after those with changes that aren't written.

Press `<Space>?`.

```check
finder buffers
```

## What's wrong

`<Space>ds` lists what the language servers say is wrong in the project's files, the worst first: those open, and those a server has checked, as rust-analyzer checks a whole crate. In an editor, mistakes are underlined, the worst on each line is written dimly after it, and `]d` and `[d` go to the next and the one before. `K` shows what's under the cursor, with what's wrong there, and Ctrl+W `d` just what's wrong. The code in what `K` shows is coloured in its language once the daemon has read it, and what it's about is underlined while it's shown.

These text files have no language server, so here the list is empty. Press `<Space>ds` to see it, and Esc to close it.

```check
finder diagnostics
```

## Symbols

`<Space>bs` lists the symbols in the file, as its language server knows them: its functions, its types and their fields, each with what it is, indented inside what it's in. What you type narrows them down, and Enter goes to the one picked, as a jump, so Ctrl+O comes back. `<Space>ps` finds symbols in the whole project, asking the server again as you type, and opens the file of the one picked. A server that can tell is asked first by `grn` whether what's under the cursor can be renamed: if it can't, the bar says so at once, and if it can, the new name starts as the server suggests.

Once the cursor rests on a name in normal mode, the other places it's used in the file are lit, as its language server says: where it's written a little brighter than where it's read. `:set nohlu` turns that off. `gq` on a selection, or `:'<,'>Format`, has the server format just those lines, one undo step, as `:Format` does the whole file; a server that can't says why.

These text files have no language server, so here the list says so. Press `<Space>bs` to see it, and Esc to close it.

```check
finder symbols
```
