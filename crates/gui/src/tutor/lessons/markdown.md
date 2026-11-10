# Rendered Markdown

Reading a Markdown file rendered, as prose, in the editor, and going back to its text to change it.

```practice notes.md
# Notes

A file with **bold**, *italic* and `code` in it, and a [link](https://commonmark.org).

## A list

1. First
2. Second
   - nested
- [x] a task that's done
- [ ] one that isn't

> A quote.

~~~json
{ "rendered": true }
~~~

| Key | Does |
|---|---|
| `j` `k` | A line |
| Ctrl+D | Half a view |

Back to [the top](#notes).

![The plan, as a picture](plan.png)
```

```practice plan.png base64
iVBORw0KGgoAAAANSUhEUgAAABgAAAAQCAIAAACDRijCAAAAJUlEQVR42mN4sMEBKzJIWIAV4VLPMGrQiDaIVA241I8aNKINAgAYQEAfH2Ys+QAAAABJRU5ErkJggg==
```

## Render it

The practice pane is a Markdown file, as text. `<Space>tm`, or `:markdown`, shows it rendered: headings, lists, task lists, quotes, tables, and code coloured as an editor colours it, in a column of prose.

```check
focused practice
rendered
```

## Read it, then write in it

The rendered view is for reading. `j` and `k` scroll it a line, or as many as a count says, as `5j`, Ctrl+D and Ctrl+U half a view, `gg` and `G` to the top and the end; and so does the wheel. A click on a link opens it: a web page with your browser, a file in the editor, and `#` and a heading, such as `the top`, scrolls to that heading.

Any other key goes to the text. Press `i`: the text comes back, where the view was, in insert mode, to type in. It's the same file either way, and `:w` writes it from either.

```check
not rendered
mode insert
```

## Back to rendered

Go to normal mode with Ctrl+;, and render it again with `<Space>tm`. What anyone changes in the file, in another pane, another window or an agent's chat, shows in the rendered view as it's made.

`:set markdownview=rendered`, or `client.opt.markdownview = "rendered"` in your `init.lua`, opens every Markdown file rendered.

```check
rendered
```

## Open an image

At its end, the file names an image beside it, `plan.png`, which the rendered view shows below its description. Open the image itself in a pane of its own: `:e plan.png`, as for any file, or pick it in an explorer. It opens fitted to the pane. `+` and `-` zoom it, `0` shows it at its own size, `h` `j` `k` `l` move about in it, `=` fits it again, and `q` closes it. PNG, JPEG, GIF and WebP files open this way, on any machine.

```check
focused image
```

## Show an image in a terminal

Programs show images in a terminal too, with kitty's graphics protocol, sixel or iTerm2's: `kitten icat plan.png`, `img2sixel plan.png` or `imgcat plan.png`, whichever you have, and the previews of yazi and ranger. They go through the same layer as the image pane's, so they show on the phone too, scroll with the text, and copy as their size, as `[image 640×480]`, never their bytes.

Open a terminal beside the file with `:|Terminal`, and show the image in it, with one of those, or with this, which sends it as iTerm2's `imgcat` does:

`printf '\033]1337;File=inline=1;width=16:%s\a\n' "$(base64 -w0 plan.png)"`

```check
pictures
```
