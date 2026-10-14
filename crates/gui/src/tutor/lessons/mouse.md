# The mouse in editors

Selecting with the mouse, as in Neovim: dragging selects in visual mode, a double click and drag by words, a triple click and drag by lines, ready for `d`, `y`, `c` or `>`.

```practice fruit.txt
apple banana cherry
damson elder fig
grape honeydew kiwi
lemon mango nectarine
```

## Drag across some text

Press the mouse button on the `b` of `banana`, and hold it down while you move to the `r` of `elder`. What's between lights up as you go, and the bar says `VISUAL`: it's visual mode's selection, as `v` makes.

```check
mode visual
```

## Act on it

Let go: the selection stays, for any of visual mode's commands. Press `d` to delete it. A click anywhere would have ended it instead, and moved the cursor there.

```check
not has "banana"
not has "elder"
has "apple"
has "fig"
```

## Whole words

Double-click `grape`, and on the second click keep the button down and drag to `kiwi`: whole words are selected, by the file's own word characters. Press `y` to copy them.

```check
register 0 "grape honeydew kiwi"
```

## Whole lines

Triple-click the line with `lemon` on it, and on the third click keep the button down and drag up a line: whole lines are selected, as `V` selects them, and the bar says `V-LINE`. Press `d`.

```check
not has "lemon"
not has "grape"
has "fig"
```

## From insert mode

Press `i` for insert mode, then drag across `fig`: the bar says `(insert) VISUAL`, a selection that goes back to insert mode once it ends, as Neovim's does. Press `y` to copy it, and you're back in insert mode. Esc would have gone back to it too, and `c` types over the selection.

```check
mode insert
register 0 "fig"
```

## More

Dragging past the pane's top or bottom scrolls it while you hold the button, faster the further past you go. While you drag, the view keeps still under the mouse; once you let go, `scrolloff` keeps rows around the cursor again.

Letting go also puts what's selected on the primary selection, which a middle click pastes elsewhere, as selecting in a terminal does; not with `clipboard=unnamed`, where `p` would then put the selection over itself. An editor showing a folder selects in the same way, but a double click there opens what's clicked, as Enter does. A long line wrapped over several rows is selected row by row, and inlay hints are never selected: the mouse on one takes the character it's drawn before.
