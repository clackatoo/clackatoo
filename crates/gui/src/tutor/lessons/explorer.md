# The file explorer

A project's files as a tree, beside its editors: opening files from it, and making, renaming and deleting them.

The explorer on the left shows the lesson's scratch directory, made for it, so nothing here touches your own files.

```setup
explorer
```

```practice notes.txt
Notes for the explorer lesson.
```

```practice todo.txt
Things to do.
```

## Go to the explorer

`<Space>e` opens an explorer down the tab's left, rooted at the project the focused pane is in, or goes to the tab's explorer if there's one; pressed in the explorer, it closes it. `:Explore` does the same, and `:Explore DIR` roots it at DIR.

Press `<Space>e` to go to the explorer.

```check
focused explorer
```

## Open a file

`j` and `k` move, as do `gg`, `G`, Ctrl+D and Ctrl+U. `l` or Enter opens a file in the editor of the tab you were last in, in place of its file, as `:e` would; Ctrl+V and Ctrl+S open it in a split, and Ctrl+T in a new tab. On a folder, `l` opens it, `h` closes it, and `-` goes up a folder, though a lesson's explorer stays in the lesson's folder.

Open `todo.txt`.

```check
file "todo.txt"
```

## Make a file

Back in the explorer, `a` makes a file in the folder at the cursor: type its name in the bottom bar and press Enter. A name ending in `/` makes a folder, and `a/b/c.txt` makes the folders too.

Go back to the explorer with `<Space>e`, press `a`, and make `ideas.txt`.

```check
listed "ideas.txt"
```

## Rename it

`r` renames what's at the cursor; a name with `/` in it moves it. A file open in an editor goes with it, changes and all.

Put the cursor on `ideas.txt`, press `r`, and rename it `plans.txt`.

```check
listed "plans.txt"
not listed "ideas.txt"
```

## Delete it

`d` deletes what's at the cursor, after asking, to your desktop's trash, so your file manager can put it back. Nothing is ever written over unless you say so: the bar asks first, saying what's there.

Delete `plans.txt` with `d`, and say yes.

```check
not listed "plans.txt"
```

## Edit the folder as text

`e` shows the folder at the cursor as text, in the explorer's pane: a line for each file or folder, folders ending `/`. Edit it as you would any file. Change a line to rename what it names, or give it a path, as `sub/x`, to move it; delete a line to delete it; add a line to make a file, or a folder if it ends `/`; copy a line and change the copy's name to copy it. Enter on a folder's line shows its listing, and `-` the folder above's, so a line cut in one folder and put in another moves it.

`:w` lists every change and asks before making any: `y` makes them, one at a time, `n` goes back to the text. Nothing is written over, and deletes go to the trash. `:q!` leaves without making any.

Press `e`, rename `todo.txt` to `done.txt`, add a line `drafts/`, then `:w` and `y`.

```check
listed "done.txt"
listed "drafts"
not listed "todo.txt"
```

## More

`x` or `y` cuts or copies what's at the cursor, and `p` moves or copies it into the folder at the cursor. `.` shows hidden files, `/` searches the names shown, `R` lists everything again, and `q` closes the explorer.

Beside each file is what git says of it: `M` changed, `A` added, `?` untracked, `D` deleted, `!` in conflict. The daemon watches the folders open in the tree, so a file a terminal or an agent makes shows at once.
