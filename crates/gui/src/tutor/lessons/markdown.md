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
```

## Render it

The practice pane is a Markdown file, as text. `<Space>tm`, or `:Markdown`, shows it rendered: headings, lists, task lists, quotes, tables, and code coloured as an editor colours it, in a column of prose.

```check
focused practice
rendered
```

## Read it, then write in it

The rendered view is for reading. `j` and `k` scroll it a line, Ctrl+D and Ctrl+U half a view, `gg` and `G` to the top and the end; and so does the wheel. A click on a link opens it: a web page with your browser, a file in the editor, and `#` and a heading, such as `the top`, scrolls to that heading.

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
