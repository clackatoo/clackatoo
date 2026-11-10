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

`:set nowrap` turns wrapping off: a long line runs off the pane's edge, and the view scrolls sideways to follow the cursor. Options set with `:set` apply to every pane, in every window; set them for good in your `init.lua`, as `client.opt.wrap = false`.

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

## Where an option was set

`:set nolinebreak` breaks long lines at the last cell that fits, for now. `:verbose set linebreak?` says how it is, and what set it last: `:set` or a key, a line of your `init.lua`, `settings.lua`, or nothing, as it starts.

```check
said "nolinebreak · set with :set or a key"
```

## As it starts

`:set linebreak&` puts an option back as it starts. Ask `:verbose set linebreak?` again, to see it say so.

```check
said "linebreak · as it starts"
```

## Just this window

`:setlocal`, or `:setl`, sets an option for this window alone, over the value every window shares, as Vim's `:setlocal` does: `:setlocal nowrap`. Any other window you have open goes on wrapping. Only some options can be set so: the font and its size, and how editors show their lines, such as `number`, `colorcolumn` and `wrap`.

```check
option nowrap
```

## Every window's again

`:setlocal wrap<` takes this window's own value away, so it has every window's again, and wraps. `:set wrap` would too: `:set` in a window sets every window's value, and takes that window's own away.

```check
option wrap
```

## Keep it

`:set` lasts until clackatoo's windows have all closed. `:setpersist`, or `:setp`, sets an option and saves it as well, in `client/settings.lua` beside your `init.lua`, so every window opened after starts with it: `:setp nolinebreak`. `:setp linebreak&` takes it out again. This lesson doesn't save anything, as that changes your own config.

An option your `init.lua` sets as it loads isn't saved, as `init.lua` runs after `settings.lua` and would outdo it: `:setp` sets it for now, and the bar names your line that sets it. Font, font size, `renderer` and `priority` are saved in `startup.toml` instead, as the window needs them before any Lua runs.

## The settings menu

`:Settings`, or a click on the cog at the top right, opens a menu of your settings, in one of two modes. **Easy**, where it opens the first time, has pages of the settings most people change, each with a switch, buttons, a slider or the like, to use with the mouse or the keys. **Advanced** has every option and every key bound: what each does, its value, and where it was set. A change in either applies at once and is saved, as `:setp` saves it. `:Settings wrap` opens it searching for `wrap`: open it so.

```check
finder settings
```

## Easy and Advanced

The menu opens in the mode you used last. Its one search looks in both: Easy's rows first, then what only Advanced has, which Enter switches to. Press `m` to switch to Advanced, or click the switch at its top right.

```check
settings advanced
```

## Back to Easy

`m` switches back to Easy. There, `j` and `k` move between rows, Tab from control to control, `}` and `{` between pages, and `h` and `l` step a value. A row your `init.lua` sets is greyed, with the line that sets it, which Enter opens.

```check
settings easy
```

## Closing it

In Advanced, `j` and `k` move between rows, Tab between sections. In either, `/` searches again; on a row, Enter changes it, `r` puts it back as it starts, and `u` undoes. This lesson saves nothing, so press Esc twice: once to clear the search, and again to close the menu.

```check
not finder
```

## The daemon's own

The daemon, which runs your terminals and agents, has a config of its own: `daemon/init.lua`, beside `client/`. It sets the daemon's settings, as `daemon.machine.name = "desk"` or `daemon.history.lines = 20000`, adds agents, as `daemon.agents.opencode = { command = { "opencode", "acp" } }`, and sets up the web app, as `daemon.web.mode = "tailscale"`. The daemon runs it again as you save it, so a new chat can start with an agent you add at once, and every window's bar says what a change made outside clackatoo, as by an agent, did to agents, limits and the web app; what it reads only as it starts, such as the web app, waits for `clackatoo upgrade`, which keeps every terminal. A mistake in it never stops the daemon: it keeps what last ran, and the bar says what's wrong, with its line. Your window's own `init.lua` runs again as you save it too: its options apply at once, and `:reload` applies the rest, its keys and handlers. The old `agents.toml`, `machine.toml` and `web.toml` were moved into `daemon/settings.lua` the first time it started.

## The daemon's, in the menu

The settings menu changes the daemon's settings too: it asks the daemon, which saves them in its own `daemon/settings.lua` and runs its config again. Easy's Terminals and Agents pages have the usual ones, such as what terminals keep after a restart; Advanced has every one, after the keys. One the daemon reads only as it starts says it takes effect at its next upgrade or restart. `:Settings history` opens the menu searching for what terminals keep: open it so, and look, changing nothing.

```check
finder settings
```

## Done with it

Press Esc twice to close the menu again: once to clear the search, and once more to close it.

```check
not finder
```
