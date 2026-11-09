---
title: Files and Buffers
type: reference
description: Opening files and folders, buffers shared by every window, writing files, and recovering unsaved changes.
related:
  - clackatoo/reference/editor
---

An editor shows a buffer: the text of one file, kept by the daemon, which
every window showing the file edits together.

## Opening a file

| Command | Does |
|---|---|
| `:e PATH`, `:edit PATH` | Opens the file, or the folder's listing, in place of what the pane shows, as Vim's do; from a terminal, in a new tab. `:e .` shows the folder of the file shown. |
| `:e`, `:edit` alone | Opens [the explorer](~clackatoo/reference/explorer) at the folder you're in, the folder of an editor's file or a terminal's, beside an empty file with no name yet, where `:e PATH` would put a file: in place of an editor's file, or in a new tab. The explorer has the focus. |
| `:Editor PATH` | Opens the file, or the folder, in a new tab; with no path, the focused pane's folder. |
| `:\|Editor PATH`, `:\|<Editor PATH`, `:-Editor PATH`, `:-^Editor PATH` | Opens it beside the focused pane: right, left, below or above, as the [split marks](~clackatoo/reference/workspaces#the-command-line) do for terminals. |
| `clackatoo edit [PATH]` | From a shell: opens it, a folder too, in a new tab of the workspace last used; with no path, the current directory. See [the CLI](~clackatoo/reference/cli#edit). |

An image file opens in an [image
pane](~clackatoo/reference/images#opening-an-image-file), fitted, to zoom
and move about in, rather than as text.

A relative path is from the focused pane's directory: for a terminal, the
directory of the program in front, as for a new terminal; for an editor, the
directory its file was opened from. `~/` is your home directory.

A file a pane stops showing, as after `:e`, closes too, unless another pane
shows it, or it has changes that aren't written: then it's kept, unseen, the
bottom bar says so, and `:e` it again to go back.

A file that's already open is shown again, not read again: two panes on one
file are two views of one buffer, each with its own cursor. A file that
doesn't exist yet opens empty, and is made when it's first written. A file
that isn't UTF-8 text can't be opened, and the bottom bar says so. Nor can
anything but a regular file of up to 256 MiB, such as a pipe or a device, even
one put in a file's place while it's open: it's never waited on or read. A
save writes the new text to a new file beside the old one, made with a name no
one can guess, and puts it in the old one's place, so nothing put beside the
file can make a save write anywhere else.

A file with no name yet, as `:e` alone makes, is called `[No Name]`. Nothing
is written until `:w NAME` names it, from the folder it was opened in; `:w`
alone asks for the name on the command line. Nothing could open it again once
no pane shows it, so while it has changes the daemon refuses to close, or to
show anything else in, the last pane showing it, whoever asks: `:w NAME`
writes them, `:q!` closes it, `:e!` empties it.

## Folders

An editor shows a folder as its listing, as oil.nvim does: an entry on each
line, folders ending `/`. The first line is `..`, except at `/`:
<kbd>Enter</kbd> or a double click on it goes up, and deleting or moving it
changes nothing. It's the
[explorer's listing](~clackatoo/reference/explorer#editing-a-folder-as-text),
edited the same way: change a line to rename, delete one to delete (to the
trash), add one to make a file, or a folder if it ends `/`. `:w` lists every
change, and asks under them; `y` makes them, `n` goes back. Once they're made,
the folder is listed again. In a listing, `r` leaves each line's hidden mark
and the `..` line as they are; `J`, and deleting, changing or putting over
characters across lines, are refused, since they would join entries: act on
whole lines instead. `>` and `<` are refused too, since a name can start with
a space.

| Key | Does |
|---|---|
| <kbd>Enter</kbd> | On a file, opens it in the listing's place; on a folder, goes into it; on `..`, the first line, goes up |
| `-` | Goes up a folder; in a file, shows the folder it's in, with the cursor on the file |
| `g.` | Shows hidden files, or hides them, as `explorerhidden` starts them |

A listing with changes isn't left until they're made or thrown away: `:e!`
throws away every listing's changes, and `:q!` closes the pane. One with no
changes is listed afresh whenever its folder changes on disk, and a folder
you've left as it was stops being watched. The finder (`<Space>pf`) is rooted
at the folder shown, and a relative `:e` path starts there. Going into a
folder, up from one, and from a listing to a file are jumps:
<kbd>Ctrl</kbd>+<kbd>O</kbd> and <kbd>Ctrl</kbd>+<kbd>I</kbd> go back through
them, landing on the entry the cursor was on. If a folder with changes
disappears from disk, `y` makes none of them.

## Buffers

The daemon keeps each buffer, as it keeps terminals:

- **Every window edits the same text.** Edits made in one window, or by anyone
  else, show in the others, and move their cursors with the text around them.
- **Edits made at once meet.** If an agent edits a file while you type in it,
  both edits are kept, and every copy ends up with the same text. The daemon
  puts edits in order, and brings each one up to date past those made since,
  by operational transformation, as Google Docs does.
- **Undo undoes your edits, not anyone else's.** `u` takes back your last
  change, from whichever window you made it in, and leaves an agent's edits
  made since in place. See
  [the files an agent changes](~clackatoo/reference/agent-changes#the-files-it-changes).
- **Nothing is lost when the window closes, or the daemon
  [upgrades](~clackatoo/reference/cli#upgrade).** Buffers come through an
  upgrade with their undo history. Changes not written outlive a crash or a
  stop too: see [Recovery](#recovery).

Each command in normal mode is one undo step, as is each go of insert mode,
with the command that started it, such as `cw`.

Undo is the daemon's, one history per file that every window shares: each step
keeps where the cursor was and the file's last selection as it began, which
follow other windows' and agents' edits made since, so an undo puts them back
whoever edited meanwhile. Undo keeps your last 1000 steps in each file, or as
many as fit in 64 MB. Past that the oldest are let go, and `u` past the oldest
one kept says so in the bar. The daemon also keeps the last 32 MB of edits
made to each file, to bring up to date an edit a window made before it heard
of others'. A window that has fallen further behind than that, or so far that
bringing its edit up to date would hold up the daemon for more than a few
milliseconds, catches up itself and sends its edit again. You don't see this
happen, except with a window from before protocol 92, whose edit that far
behind isn't made: it keeps its own text, as when a change couldn't be made,
until `:w!` or `:e!`. An edit of more than 16,384 pieces, as `:%s` over a big
file can make, is sent with neighboring changes joined into one, so marks
between them move as if the text between had been typed again.

A file whose lines end with CRLF, as Windows ends them, is edited with plain
line ends, and written with CRLF again, as its first line says; a file that
mixes the two is written all one way. Its git signs compare it line by line,
but it's staged a change at a time in a terminal, not here.

A buffer closes with its last pane, unless it has changes that aren't written.
`:q` won't close that pane, and `:q!` throws the changes away. A pane closed
another way, such as with <kbd>Ctrl</kbd>+<kbd>B</kbd> `x` or by closing its
tab, keeps the buffer's changes, unseen: opening the file again shows them.

## Writing

| Command | Does |
|---|---|
| `:w`, `:write` | Writes the file, formatting it first if the option `formatonsave` is on. |
| `:format` | Formats the file, as its [language server](~clackatoo/reference/languages#language-servers) does, as one undo step. If the server won't format the file, the bar says why: "not formatted: …". |
| `:'<,'>Format`, `:{range}Format` | Formats only those lines, as the language server does, as one undo step. Any changes the server makes outside them are made too, as `:format`'s are. If the server can't format part of a file, the bar says so. |
| `:w!` | Writes it even if it's changed on disk since it was read or last written. |
| `:wq`, `:x` | Writes it, then closes the pane. `:wq!` writes it even if it's changed. |
| `:w NAME`, `:w! NAME`, `:wq NAME` | Names a file with no name yet and writes it there, from the folder it was opened in; not over a file that's there, unless with `!`. In a file that has a name, it's refused. |
| `:q`, `:quit` | Closes the pane, unless it's the last showing a file with changes not written. |
| `:q!` | Closes the pane, throwing away the file's changes not written. |
| `:e!` | Throws away the changes not written, reading the file again. That's an edit like any other: `u` brings the changes back. |

Writing writes a new file beside the old one, with its permissions, then
renames it over the old one, so the file is never half written. If something
else has changed the file since it was read or last written, `:w` refuses, so
as not to write over the change; `:w!` writes over it. The bottom bar shows
`[+]` after the file's name while it has changes that aren't written.

When something other than an agent changes the file on disk, such as a git
checkout or another editor, a file with nothing unwritten is read again, as
Vim's `autoread` does. That's an edit like any other, so `u` takes it back.
With changes not written, yours stay, `:w` refuses, and the bar says the file
changed: `:e!` takes it, `:w!` keeps yours. A file over 4 MB is never read
again by itself: the bar says it changed. Turn `autoread` off to be told in
every case.

## Recovery

Changes not written, a `[No Name]`'s too, are kept on disk as you make them,
so they outlive the daemon crashing, being killed or stopping, or the machine
going down, as Vim's swap files do. Each buffer with changes has a file in
`~/.local/state/clackatoo/recovery`, for you alone: the folder is 0700, each
file 0600. It's written a second after you stop typing, and every few seconds
while you don't (less often for a big file), by a thread of the daemon's own,
so a key never waits for the disk. It goes once the file is written, its
changes are thrown away with `:e!`, or they're undone. A buffer over 64 MB
isn't kept, and the bar says so. `CLACKATOO_RECOVERY` sets another folder for
the daemon, or `off` for none. Only one daemon at a time keeps the files in a
folder: a second one keeps none, and says so. An upgrade hands the folder to
the next build.

When the daemon next starts, each comes back unseen, as a file with changes
not written does after `:q`: opening the file shows it.

| What came back | What happens |
|---|---|
| A file unchanged on disk since | It opens with its changes, not written yet, and the bar says so. `:w` writes them, `:e!` drops them, and `u` puts the file's text back. |
| A file changed on disk since | It opens as it is on disk, and the bar says its changes were set aside. `:recover` takes them, as an edit `u` takes back, or, if you've changed the file since, opens them beside it as a `[No Name]`; `:recover!` drops them. Nothing is written until you write it. |
| A file already open, as after an upgrade | Its text is never replaced: what came back is set aside, as above. |
| A `[No Name]` | It comes back with no name. `:recover` shows it. |

| Command | Does |
|---|---|
| `:recover`, `:rec` | In a file whose changes were set aside, takes them. Otherwise shows the next file or `[No Name]` that came back that no pane shows: in place of an editor's file, or in a new tab. |
| `:recover!` | Drops a file's changes that were set aside. |

As the daemon starts, it takes back at most 64 files or 256 MB at once;
`:recover` takes back the rest, one at a time.
