---
title: Images
type: reference
description: Image files in a pane of their own, the images Markdown names, chats' images, and how clackatoo keeps them all, within limits.
related:
  - clackatoo/reference/editor-files
  - clackatoo/reference/editor-view
  - clackatoo/reference/chat-view
  - clackatoo/reference/explorer
---

clackatoo shows images in several places: an image file in a pane of its own,
the images a Markdown file names, the images in a chat, and those programs show
in a terminal. All of them go through one layer, kept once by the daemon and
drawn the same way everywhere.

## Opening an image file

`:e plan.png`, or picking an image file in [the
explorer](~clackatoo/reference/explorer#opening-files), opens it in an image
pane. The same goes for the rest of the [ways of opening a
file](~clackatoo/reference/editor-files#opening-a-file), such as
`:|Editor plan.png`. clackatoo shows PNG, JPEG, GIF (its first frame) and WebP
files, by their names' endings. An SVG file is text, so it opens in the
editor.

The image opens fitted to the pane, in proportion, and never bigger than its
own size. The line above it names the file and gives the image's size, its
format and how it's zoomed.

| Key | Does |
|---|---|
| `+` `-` | Zooms in or out, by a quarter |
| `=` | Fits it to the pane again |
| `0` | Shows it at its own size, a pixel for a pixel |
| `h` `j` `k` `l`, the arrows, the wheel | Move about in it, once it's zoomed |
| `g` `G` | Its top, or its bottom |
| `q` | Closes the pane |

<kbd>Ctrl</kbd>+<kbd>?</kbd> lists the keys too. How it's zoomed is your
window's own, as a pane's scroll is.

The file is read by the daemon of the machine it's on, so an image on another
machine opens as one there does. That daemon reads it only if it's a regular
file, not a pipe or a device, of no more than the limit for one owner (see
[Limits](#limits)). A file that isn't an image of a format clackatoo shows says
so, in the pane's top line. A file you're already editing as text stays in the
editor.

## Images in Markdown

The editor's [rendered Markdown](~clackatoo/reference/editor-view#rendered-markdown)
shows each image a file names, as `![the plan](img/plan.png)`, below the block
that names it, as wide as it is or as the column. Its description stays in its
place, after `▣`.

Only a relative path, from the folder the Markdown file is in, is shown. In a
folder you [trust](~clackatoo/reference/languages) it may climb out of that
folder with `..`, as far as the root of the repository it's in, but no
further; in a folder not trusted, only a path down from the file's own folder
is shown. The daemon never reads one through a link, at any part of its path,
and never reads a pipe or a device. A Markdown file shows at most 64 images,
each asked for as it comes near the view. An absolute path, a path starting
`~`, or an image from the web shows only its description: clackatoo doesn't
fetch anything from the web for Markdown.

## Images in chats

Images you paste into a chat, and those an agent's tools show, are kept in the
same layer. Each is shown from the image itself, at the size it's drawn, and
fetched once. Selecting one opens it whole, over the chat. A window older than
the image layer, or a daemon older than it, still shows the small previews it
always did, even of a chat on another machine that comes to it through newer
daemons.

## Images in terminals

Programs show images in a terminal with kitty's graphics protocol, its Unicode
placeholders, sixel, or iTerm2's inline images: `kitten icat plan.png`,
`img2sixel plan.png`, `imgcat plan.png`, or the previews of yazi and ranger.
They're kept in the same layer, as the terminal's own, and drawn wherever the
terminal is, the phone included. [Terminal
emulation](~clackatoo/reference/terminal-emulation#images) says what each
protocol does.

## On the phone

The [web app](~clackatoo/reference/phone) shows a chat's images too, and a
terminal's. It
fetches each one only as it scrolls into view, and as a copy only as big as
the screen shows it.

## Limits

The daemon keeps each image once, as it was sent, however many places show it:
a PNG stays a PNG. Pixels sent with no format are kept as a PNG.

Each terminal, chat, image pane or Markdown file may hold up to 64 MiB of
images, and all of them together 256 MiB. Past either limit, the image used
longest ago that isn't showing in an image pane is let go, and its place says
`[image dropped: over the 64 MB limit]`. An image bigger than the limit by
itself isn't kept at all. Each window keeps the images it has fetched for
itself, within limits of its own, and lets go of the oldest the same way.

A terminal's images that its screen shows are never the ones let go: those
that scrolled into history, or that kitty's commands keep for later, go first.
So a terminal whose screen shows more than its limit of images, or Unicode
placeholders of them, can't be given another until some leave the screen:
new ones are refused meanwhile.
A terminal lets go of an image once nothing it shows or keeps holds it.

[`daemon.images.terminal_mb` and
`daemon.images.total_mb`](~clackatoo/reference/config) in the daemon's config
set the limits, and `CLACKATOO_IMAGE_MEMORY`, in MiB, each owner's and then
all of them together, as `64,256`, overrides them. They're read when the
daemon starts or [upgrades](~clackatoo/reference/cli). The daemon keeps its
images through an upgrade, but not through a restart: a terminal's history
kept across one says each image's size where it was.

Agents never see an image's bytes in what they read of a pane: an image pane
reads as `image, ~/plan.png [image 640×480]`, and an image in a terminal as
`[image 640×480]` where it starts.

## Known limits

- An image pane on another machine, in a workspace from there, shows as a pane
  that needs a newer clackatoo. Open the file on that machine's own workspace,
  or with a `--remote` window.
- An animated GIF or WebP shows only its first frame.
- A JPEG of more than about 37 million pixels (8000 × 6000, say) isn't shown:
  decoding one would take more memory than clackatoo allows an image.
- SVG isn't shown as an image.
- kitty's animations, and its placements relative to others, aren't done (see
  [terminal emulation](~clackatoo/reference/terminal-emulation#not-done)).
