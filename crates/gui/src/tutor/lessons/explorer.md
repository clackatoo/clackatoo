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

`<Space>e` opens an explorer down the tab's left, rooted at the project the focused pane is in, or goes to the tab's explorer if there's one; pressed in the explorer, it closes it. `:Explore` does the same, and `:Explore DIR` roots it at DIR. With a split's mark before it, `:|Explore` or `:-Explore` opens a new one beside the focused pane, to its right or below. It opens 30 cells wide, or a quarter of the tab if that's narrower; `explorerwidth` sets its width in cells.

Press `<Space>e` to go to the explorer.

```check
focused explorer
```

## Move and search as in Vim

The explorer reads keys as an editor does, a line a file: `j` and `k` move, with a count, `5G` goes to the fifth line, `H`, `M` and `L` to the top, middle and bottom of the view, Ctrl+E and Ctrl+Y scroll a line, Ctrl+D, Ctrl+U and Ctrl+F half a view or a page, and `zt`, `zz` and `zb` put the cursor's line at the top, middle or bottom. `/` searches the names shown, `?` back up, and `n` and `N` go to the next or the one before; a review and a board read their keys the same way. Ctrl+? lists them all.

Type `/todo` and press Enter.

```check
at "todo.txt"
```

## Searches show as you type

A search here is typed as an editor's is: while you type it, the cursor goes to the name it would find and every name it matches lights up, the one it goes to brighter. Esc puts the cursor back; Enter keeps it. A count before it goes to that match, as `2/t` goes to the second name with a `t` in it, and `3n` three on. The bar edits as the command line does: Up and Down go back through the searches you've typed, Ctrl+R puts in a register. Esc, or `:noh`, puts out what's lit. A board and a review search the same way, lighting the text they match.

Type `?not` and watch the cursor go to `notes.txt` before you press Enter.

```check
at "notes.txt"
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

`:w` lists every change, under "Changes to" and the folder, and asks right under them before making any: `y` makes them, one at a time, `n` goes back to the text. Nothing is written over, and deletes go to the trash. `:q!` leaves without making any.

Press `e`, rename `todo.txt` to `done.txt`, add a line `drafts/`, then `:w` and `y`.

```check
listed "done.txt"
listed "drafts"
not listed "todo.txt"
```

## Root it at a folder

`:Explore DIR` roots the tab's explorer at DIR, from its root, so the tree shows only what's in it.

Back in the tree, type `:Explore drafts` to root it at the folder you just made.

```check
rooted "drafts"
```

## Go up with ..

The first line under the root's name is `..`, the way up. Enter on it, or a double click, does what `-` does: the folder above becomes the root, and the cursor lands on the folder you came from. `gg` goes to it, as to any first line. It names no file, so `r`, `d`, `x` and `y` leave it alone. There's none at `/`, nor at the lesson's folder, which its explorer stays in.

Press `gg`, then Enter, to go back up to the lesson's folder.

```check
rooted "."
```

## Browse a folder in an editor

An editor shows a folder too, as oil.nvim does: `:e .` or `:e DIR` shows a folder's listing in the editor's pane, and `:Editor` with no file opens a new one on the folder you're in. It's the listing `e` shows, edited the same way, with `:w` and `y` making the changes. From a file, `-` shows the folder it's in, with the cursor on the file.

Go to the editor showing `done.txt`, and press `-`.

```check
focused folder
```

## Go into a folder

Enter on a folder's line shows that folder's listing in its place.

Press Enter on `drafts/`.

```check
focused folder
rooted "drafts"
```

## Go up with .. in a listing

A listing starts with `..`, as the tree does: Enter or a double click on it goes up, as `-` does, with the cursor on the folder you came from. It names nothing, so deleting it, moving it or putting it in another listing changes nothing, and `:w` leaves it alone. There's none at `/`, nor at the lesson's folder, which this editor stays in.

`drafts` is empty, so `..` is its only line, and the listing says so under it. Press Enter on it.

```check
focused folder
not rooted "drafts"
at "drafts/"
```

## Open a file from the listing

Enter on a file opens it in the listing's place. `g.` shows hidden files, or hides them, as `explorerhidden` starts them. `<Space>pf` finds files in the folder shown, and Ctrl+O goes back through the folders and files you've been to.

Open `notes.txt` with Enter.

```check
file "notes.txt"
```

## Start a new file

`:e` with nothing after it opens the explorer at the folder of the pane you're in, a terminal's folder or the one an editor's file is in, as `<Space>e` finds or opens one, beside an empty file with no name yet. That goes where `:e PATH` would put a file: in place of an editor's file, or in a tab of its own from a terminal or anything else. The explorer has the focus, to pick a file from. A file left with changes not written is kept, unseen, as `:e PATH` keeps it.

Type `:e`, with nothing after it.

```check
focused explorer
pane blank
```

## Name it

A file with no name is called `[No Name]`. `:w` asks for a name on the command line, and `:w NAME` gives it one, from the folder the explorer showed, writing it there. A file that's there already is written over only with `:w!`. Until it has a name, `:e` and `:q` won't leave its changes behind: `:e!` and `:q!` throw them away.

Go to it with Ctrl+L, type something in it, and write it as `later.txt` with `:w later.txt`.

```check
file "later.txt"
listed "later.txt"
```

## More

`x` or `y` cuts or copies what's at the cursor, and `p` moves or copies it into the folder at the cursor. `.` shows hidden files, `/` searches the names shown, `R` lists everything again, and `q` closes the explorer. `g?` lists the explorer's keys at its foot, and again hides them; the bar says so while they're hidden.

Beside each file is what git says of it: `M` changed, `A` added, `?` untracked, `D` deleted, `!` in conflict. The daemon watches the folders open in the tree, so a file a terminal or an agent makes shows at once.

Changes not written, `[No Name]`'s too, are kept on disk a moment after you make them, so they outlive the daemon crashing, being killed or stopping, as Vim's swap files do. Next time, a file whose changes were kept comes back with them, not written yet, and says so: `:w` keeps them, `:e!` drops them, and `u` puts the file's text back. If the file changed on disk meanwhile, it opens as it is now, with its changes set aside: `:recover` takes them, or opens them beside it if you've changed it since, and `:recover!` drops them. A `[No Name]` that came back, or a file no pane shows, is shown by `:recover`.
