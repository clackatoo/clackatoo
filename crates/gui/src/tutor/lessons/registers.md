# Registers, macros and marks

Vim's registers, to keep text in and put it back from; macros, which record keys and type them again, through the command line and other panes too; and marks, to come back to a place.

```practice fruit.txt
apple
banana
cherry
damson
elder
```

## Keep a line in a register

`"` and a register's name before a command keeps what it yanks or deletes there. `"a` to `"z` are yours.

With the cursor on `apple`, type `"ayy`.

```check
register a "apple\n"
```

## Add to it

A capital adds to a register rather than replacing what's in it: `"A` adds to `"a`.

Go down to `banana`, and type `"Ayy`.

```check
register a "apple\nbanana\n"
```

## Put it back

`"ap` puts what `"a` holds after the cursor, and lines on the line below. Go to the last line with `G`, and type `"ap`.

```expect
apple
banana
cherry
damson
elder
apple
banana
```

## The black hole

What goes to `"_` is thrown away, and the other registers keep theirs: `"_dd` deletes a line without losing what you yanked. `""` is what was last yanked or deleted, which `p` puts; `"0` the last yank; `"1` to `"9` the last deletes of lines, newest first; and `"+` the clipboard. `:registers` lists them.

```practice fruit.txt
apple
banana
cherry
damson
elder
```

Delete `cherry` with `"_dd`.

```expect
apple
banana
damson
elder
```

## Record a macro

`q` and a register records the keys you press, until `q` again; the bar says `recording @a` meanwhile.

```practice fruit.txt
apple
banana
cherry
damson
elder
```

```setup
cursor 1
```

On `apple`, type `qa`, then `A;`, press Esc, then `j`, and `q` to stop.

```check
register a "A;<Esc>j"
```

## Play it back

`@a` types the macro's keys again, as if you pressed them; `@@` types the last one again, and a count types it that many times. Type `4@a`.

A macro stops at a key that fails, as Vim's does, so one ending in `j` stops at the file's end.

```expect
apple;
banana;
cherry;
damson;
elder;
```

## A macro through the command line

A macro keeps every key, whatever has it: the editor, the command line, the finder, a terminal, or a binding. So it can run `:s`, `:w`, or move to another pane with Ctrl+L and go on there.

```practice fruit.txt
let a = 1
let b = 2
let c = 3
```

```setup
cursor 1
```

Type `qb`, then `:s/let/const/` and Enter, then `j`, and `q`. Then `2@b` for the other lines.

```expect
const a = 1
const b = 2
const c = 3
```

## Macros are text

A macro is kept as text, written as bindings are, so `"bp` puts its keys in the file to read or change, and `"by$` keeps a line of them in `"b` again.

Go to the end with `G`, and type `"bp`.

```check
has ":s/let/const/<CR>j"
```

## Mark a place

`m` and a letter marks the cursor's place: `a` to `z` in each file, `A` to `Z` anywhere. `'a` goes back to its line, and `` `a `` to the very place; both are jumps.

```practice fruit.txt
apple
banana
cherry
damson
elder
```

```setup
cursor 2
```

On `banana`, type `ma`. Go to the end with `G`, then come back with `'a`.

```check
line 2
```

## Delete to a mark

An operator takes a mark, as it takes any motion: `d'a` deletes the lines from the cursor's to the mark's.

Go to the end with `G`, and type `d'a`.

```expect
apple
```

## More about marks

`` `. `` goes to the last change. `:marks` lists the marks the file can go to in the finder. `'<` and `'>` are the last selection's start and end. Marks move with the text as anyone edits it, an agent included, and lines `:m` moves take theirs with them, the last selection's too: so `gv` after `:'<,'>m '>+1` selects the lines where they went, and the binding `client.keys.map("v", "J", ":m '>+1<CR>gv=gv")`, the keys of a well-known Neovim mapping, moves the lines selected down a line at a time.
