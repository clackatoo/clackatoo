---
title: The file explorer
type: reference
description: A file explorer pane shows a project's files as a tree, beside its editors and terminals. It lists folders as you open them, shows what git says of each file, opens files in the tab's editor, and makes, renames, moves, copies and deletes files without ever writing over or destroying what you didn't ask it to. What the tree shows comes from the daemon, so files a terminal or an agent makes appear at once.
related:
  - clackatoo/reference/window
  - clackatoo/reference/editor
  - clackatoo/reference/config
---

A file explorer is a pane showing a folder, its root, as a tree. The daemon lists each folder when you open it in the tree, and watches it while it's open, so a file made in a terminal, or by an agent, shows straight away. Nothing is read until it's shown: a project with a hundred thousand files costs only what you open.

## Opening it

| Key or command | Does |
|---|---|
| `<Space>e` | Opens an explorer down the left of the tab, goes to it if there's one, or closes it if it has the focus. |
| `:Explore`, `:Ex` | Opens one, or goes to it. |
| `:Explore DIR` | The same, with `DIR` as its root. A relative `DIR` is from the root, and `~/` is your home directory. |
| `:e` | With nothing after it: opens one at the folder you're in, or roots the tab's there, beside an empty file with no name yet (see [the editor](~clackatoo/reference/editor#opening-a-file)). |

Its root is the project the focused pane is in: the git repository of an editor's file or of the folder a terminal is in, or else that folder. Opened from an editor, the tree opens down to the editor's file, with the cursor on it. `:e` alone roots it at the folder you're in, not the project.

It opens 30 columns wide, or a quarter of the tab if that's narrower (`explorerwidth`), and keeps that share of the tab as the window resizes.

## The tree

The top line is the root. Below it is `..`, the way up: Enter on it, or a double click, makes the folder above the root the root, with the cursor on the folder you came from, as `-` does. The cursor moves onto it as onto any line, and `gg` goes to it. There's none when the root is `/`. It's drawn as a folder is. A heading too long for the pane is cut from its start, as `…/tutor/explorer`. It names no file, and nothing is read of the folder above until you go up. A root you may not read shows `..` alone, so you can still go up; it isn't asked for again until `R`.

Folders have `▸` when closed and `▾` when open, and the guides down the left show what each line is in. Folders are blue, programs green, links cyan.

Beside each file is what git says of it: `M` changed, `A` added, `?` new and untracked, `D` deleted, `!` in conflict. A folder shows the mark of what matters most inside it. What `.gitignore` leaves out is dimmed, with everything inside it, and so is what a worktree's repository excludes. A repository inside another has its own status. No command a repository's own config names ever runs for it: not an fsmonitor, not its hooks, and not its filter drivers, which git would otherwise run to compare a changed file. The same goes for an editor's git signs and for reviews. Git LFS's driver is the exception, exactly as `git lfs install` writes it, though files Git LFS keeps get no git signs. To change this, set `CLACKATOO_GITFILTERS` in the daemon's environment: `lfs` (the default), `all` to let every driver run, or `none` to run none, your own included. It's read when the daemon first runs git.

clackatoo needs git 2.38 or newer for git status, signs and reviews; with an older git, or none, the explorer says so and shows no marks. A git directory found on its own, such as one committed inside a clone, isn't treated as a repository, and git never fetches anything: a partial clone's missing files get no signs. Files whose names start with `.` are hidden until you press `.`.

Which folders are open, where the cursor is and what's scrolled to belong to the window: two windows can show one explorer differently. When you close an explorer and open one on the same root again in the same window, it's as you left it.

## Keys

| Key | Does |
|---|---|
| `j` `k`, `gg` `G`, `H` `M` `L`, Ctrl+D Ctrl+U Ctrl+F Ctrl+E Ctrl+Y, `zt` `zz` `zb` | Moves, with a count, as every [list](~clackatoo/reference/window#lists) does |
| `l`, Enter | Opens a file; opens a folder (Enter closes an open one); on `..`, goes up |
| `h` | Closes a folder, or goes to the one it's in |
| `-` | Goes up: the folder above the root becomes the root, as Enter on `..` does |
| `g?` | Lists these keys at the explorer's foot, or hides them; the bar says `g? keys` until then |
| `zo` `zc` `za` | Opens, closes or toggles the folder at the cursor |
| `zR` `zM` | Opens every folder shown, one level a press; closes them all |
| `/` `?` `n` `N` | Searches the names shown, with the editor's patterns, shown as you type and lit as every [list](~clackatoo/reference/window#lists)'s search is |
| `.`, `g.` | Shows hidden files, or hides them |
| `R` | Lists everything shown again |
| `q` | Closes the explorer, or stops recording a macro |

A click puts the cursor on a line, a double click opens it, or goes up from `..`, and the wheel scrolls.

## Opening files

Enter or `l` opens a file in the editor of the tab you were last in, in place of its file, as `:e` would, and moves the focus there. With no editor in the tab, the file opens in a new one beside the explorer. Ctrl+V and Ctrl+S open it in a split beside or below that editor, and Ctrl+T in a new tab.

## Making and moving files

| Key | Does |
|---|---|
| `a` | Makes a file in the folder at the cursor, or the one its line is in. A name ending in `/` makes a folder; `a/b/c.txt` makes the folders too. |
| `r` | Renames what's at the cursor. A name with `/` moves it. |
| `d` | Deletes what's at the cursor, after asking, to the trash. |
| `x` `y`, then `p` | Cuts or copies what's at the cursor, then moves or copies it into the folder at the cursor. |

`..` names no file: `r`, `d`, `x`, `y`, Ctrl+V, Ctrl+S and Ctrl+T say so and do nothing, and `a`, `p` and `e` act on the root, never the folder above.

Nothing is written over unless you say so. If a file is already there, the bar names it, such as "a file of 3 KB", and asks; if it changes before you answer, you're asked again. A folder is never written over, and a file open with unsaved changes can't be. Nothing can be moved or copied into itself, or over a folder it's in.

Deleting moves to the desktop's trash, so your file manager can put it back. A file on a disk with no trash can only be deleted for good, and the bar asks again first, saying when a folder goes with everything in it. A mount point, or a folder with another disk mounted inside it, is never deleted.

A file you move or rename that's open in an editor goes with it: the editor shows its new name, and its changes and undo history stay. A move to another disk copies first, then deletes from the old place only what was copied and hasn't changed since, and cleans up after itself if it fails. What it deletes it finds through the folders it copied, never by name: a folder swapped for a link in the meantime is left where it is, and so is whatever the link leads to. A mount point, or a folder with another disk mounted inside it, can't be moved to another disk, and a folder holding pipes, sockets or devices can't be moved or copied.

The daemon doesn't upgrade in the middle of a copy, move or delete. It waits up to 3 seconds for it to finish; past that, `clackatoo upgrade` says a file operation is still running, and to try again when it's done. While it waits, new copies, moves and deletes are turned down, and the bar says why.

## Editing a folder as text

`e` on a folder, or `:Explore!`, shows that folder in an editor in the explorer's pane, as oil.nvim does: a line for each entry, folders ending in `/`. Edit it as any text, with every motion, `:s`, macros and undo:

- Change a line to rename what it names. Give it a path, such as `../old/notes.txt`, to move it.
- Delete a line to delete what it names.
- Add a line to make a file, or a folder if it ends in `/`. `a/b/c.txt` makes the folders too.
- Copy a line, and change the copy's name, to copy what it names.

Enter on a folder's line shows that folder's listing, and `-` the folder above's, so a line cut in one folder and put in another moves what it names there. Folders' lines are coloured as in the tree, and a folder with nothing in it shows `empty folder: o makes a file` under its `..`. Every listing but `/`'s starts with a `..` line: Enter or a double click on it goes up, as `-` does, with the cursor on the folder you came from. `..` names nothing, so deleting it, moving it or putting it in another listing changes nothing, and `:w` leaves any line that's `..` or `../` alone. A path through `..`, such as `../notes.txt`, still means the folder above, and a change that leads outside the folder shown is listed with its full path. A `:%s` that adds to the start of every line reaches `..` too and is refused at `:w`; use `:2,$s`. Your edits to each listing are kept until you write or throw them away.

`:w` works out what your edits come to and lists every step in the pane, under "Changes to" and the folder, then asks right under them, "Apply 3 changes?  y yes · n no", naming the keys only while the pane has the focus (otherwise "(focus to answer)"); the bottom bar asks only if the pane has no room. The steps: copies first, then renames and moves, each name freed before it's taken, swaps and rings through a name set aside, and deletes last. `y` does them, one at a time, through the same file operations as the keys above: nothing is written over, and deletes go to the trash. The first step that fails stops the rest, and the bar says what was and wasn't done, naming anything left set aside, such as `a.txt.moving`, to put back. `n`, `q` or Esc goes back to the text. `:q!` throws your edits away.

Each step checks the disk first: if a file has changed since it was listed, or something else is there now, that step is refused and the rest stop. A folder that changed while you edited is refused at `y`: `:q!`, then `e` again, shows it as it is.

Some edits can't be done in one write, and `:w` refuses them at their line, saying why: putting something where a folder is being moved or copied to, keeping a file in a folder you've deleted, or a line pasted from another listing session, which can't be known to name what it seems to. Do those in writes of their own, or type the name on a new line.

Each line starts with a hidden mark naming its entry, so what it named is known whatever you change of the rest. A name with a line break in it, or bytes that aren't UTF-8, can't be a line: it's left out of the listing, and left alone, and the bar says how many were.

## Options

| Option | Default | Does |
|---|---|---|
| `explorerwidth` (`ew`) | `0` | How wide an explorer opens, in columns: 0 is 30, or a quarter of the tab if that's narrower; otherwise 10 to 200. |
| `explorerhidden` (`eh`) | off | Explorers show hidden files when they open. |

## Not yet

- You can't drag files with the mouse.
- Only 4096 folders are watched at once; past that, `R` shows what's changed.
- Names that aren't UTF-8 are shown as near as they can be, and can't be renamed, moved or deleted here.
