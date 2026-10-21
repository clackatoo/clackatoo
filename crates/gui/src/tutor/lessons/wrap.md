# Soft wrap

Long lines going on over as many rows as they need, between words, and moving by screen row with `gj` and `gk`.

```practice long.txt
This first line is one long line, as a paragraph in a Markdown file or a long comment often is, and the editor wraps it onto as many rows as it needs to show it whole, breaking between words rather than in them, so it reads as prose does. Somewhere near its end, after a good many more words than fit across one row of the pane, is the word mango, which is where this lesson's first step wants the cursor to go, one screen row at a time.
A short second line.
A third.
```

## Down a row with gj

`j` and `k` move by line, so `j` from the first line goes straight to the second, however many rows the first takes. `gj` and `gk` move by screen row instead, and a count moves that many.

```setup
cursor 1
```

Go down the first line with `gj` until the cursor is on the word `mango`.

```check
at "mango"
```

## Turn wrapping off

`:set nowrap` turns wrapping off: a long line runs off the pane's edge, and the view scrolls sideways to follow the cursor. Options set with `:set` apply to every pane; set them for good in your `init.lua`, as `client.opt.wrap = false`.

```check
option nowrap
```

## And on again

`:set wrap` turns it back on.

```check
option wrap
```

## How lines wrap

Three options shape it, all on to begin with but the last: `linebreak` breaks between words, after a blank or a character such as `-` or `,`; `breakindent` starts the rows a line goes on over as far in as the line is indented; and `showbreak` is drawn at the start of each, such as `:set sbr=↪\ ` for an arrow and a space.

`g0` and `g$` go to the screen row's first and last character, and Ctrl+D and Ctrl+U move by half a page of screen rows, Ctrl+E and Ctrl+Y the view by a row. `H`, `M` and `L` go to the first line in view, the middle and the last shown whole, keeping `scrolloff` rows from the edges. A click beside a row goes to that row's start.
