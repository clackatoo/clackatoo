# Block selection

Selecting a block of columns with Ctrl+V, as in Vim, and typing on every line of it at once.

```practice fruit.txt
apple
banana
cherry
damson
```

## Select a block

On the `a` of `apple`, press Ctrl+V, then `3j` and `l`: two columns of four lines light up, and the bar says `V-BLOCK`. `v` and `V` switch to the other kinds of selection, and Ctrl+V again ends it.

```check
mode visual-block
```

## Act on all of it

Press `U`: what's in the block goes to capitals, on every line. `r`, `~`, `u`, `d`, `c`, `y`, `>` and `<` all act on a block, as they do on any selection.

```check
has "APple\nBAnana\nCHerry\nDAmson"
```

## Type on every line with I

Press Ctrl+V, `3j`, then `I`, type `- ` and press Esc. You type on the first line, and once insert mode ends, what you typed goes before the block on every line.

```check
has "- APple\n- BAnana\n- CHerry\n- DAmson"
```

## To each line's end with $ and A

Press Ctrl+V and `3j`, then `$`: the block goes to the end of each line, however long. Press `A`, type `;` and Esc, and it goes after every line. Without `$`, `A` types just after the block, padding a line that's too short.

```check
has "- APple;\n- BAnana;\n- CHerry;\n- DAmson;"
```

## Delete a block

Press `0`, Ctrl+V, `3j`, `l` and `d`: the `- ` goes from every line, and the register keeps it as a block.

```check
has "APple;"
not has "- "
```

## Put it back with P

Press `P`: the block goes back as it was taken, a row on each line from the cursor's, padded where text follows so the columns stay in line.

```check
has "- APple;\n- BAnana;\n- CHerry;\n- DAmson;"
```

## More

`o` goes to the block's opposite corner, and `O` to the other corner on the same line. `D` and `C` take each line from the block's left edge to its end. `.` makes a block change again on a block as big from the cursor. A tab or a wide character the block's edge cuts is split into spaces, as Vim does.

With Alt held, or Ctrl and Alt, dragging the mouse selects a block, in an editor or a terminal. In a terminal's normal mode Ctrl+V selects a block too, and `y` copies it, a line for each row, without the blanks at their ends.
