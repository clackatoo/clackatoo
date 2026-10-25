# Basics

How clackatoo is laid out, and how to get around it: modes, the command line, panes, tabs and workspaces, and a terminal's normal mode.

```setup
terminal
```

The pane on the right is a terminal, running your shell in a scratch directory. Like Vim, the window has a mode: insert mode, where keys go to the program, and normal mode, where they drive clackatoo. The bottom bar starts with it: `INSERT` on lilac, or `NORMAL` on gold.

## Type into the terminal

The mode is the window's, not each pane's, so wherever you go, you arrive in it. You typed `:Tutor` in normal mode, so the pane it just opened is in normal mode too. Press `i` to go to insert mode, where what you type goes to the shell.

```check
focused practice
mode insert
```

## Back to normal mode

Ctrl+; goes to normal mode from anywhere. Plain Esc always goes to the program, since programs such as Vim and less use it; Shift+Esc and Ctrl+\ do what Ctrl+; does.

```check
focused practice
mode normal
```

## Run a command

Press `i`, type `echo hello`, and press Enter.

This step ticks off once the command has finished, which clackatoo knows when your shell marks its commands, as bash, zsh and fish 4 do when clackatoo starts them. If yours doesn't, press `]s` to go on.

```check
exited 0
```

## Copy a line in normal mode

Go to normal mode with Ctrl+;. The yellow block is normal mode's own cursor, which moves through the terminal's history and screen as one document, with Vim's keys: `h` `j` `k` `l`, `w` `b` `W` `B`, `f` `t` `F` `T` and `;` `,` to find a character on the line, `+` `-` `_` to lines' first non-blanks, `g_` `|` `ge`, `gg` `G`, `H` `M` `L`, `/` to search. Back in history, a pill at the pane's bottom right says how far, as `↓ 12 lines · G to return`, and a search that goes round the end says `search wrapped to top`. The wheel scrolls history too. A full-screen program's screen has none, so over one the wheel moves this cursor instead, and the program gets arrow keys from it only in insert mode.

A count before a motion repeats it, as in Vim: `5k` goes up five lines, `3w` three words, and `10G` to line 10; `3yy` copies three lines, and `yH` up to the top of the view; `zt` puts the cursor's line at the top of the view, and `5zt` line 5; Ctrl+F pages down, keeping two lines in view where they fit. A command half typed shows at the start of the bar, as `y3`, until it's done. `v` selects by characters and `V` by lines, as in Vim: a selection by characters lights each line to a cell past its text, which is what `y` copies, and one by lines the pane's whole width.

Move it onto the line that says `hello` with `k`, and copy the line with `yy`. What you copy lights up for a moment, and goes on the clipboard.

```check
copied "hello"
```

## Split the pane

Type `:|` and press Enter. `:` opens the command line, in the middle of the window, and `|` splits the focused pane, with a new terminal to its right. `:-` would put it below, and `:|<` and `:-^` to the left and above. While the command line is open, the bar keeps the mode and the tabs, and the pane's cursor is hollow, as what you type goes to the line, not the pane.

```check
panes 3
```

## Move between panes

In normal mode, Ctrl+H, Ctrl+J, Ctrl+K and Ctrl+L move to the pane to the left, below, above or right. Ctrl+B and then `h`, `j`, `k` or `l` does the same in either mode, as tmux's prefix does. While Ctrl+B waits, the bar marks it `^B` by the mode, and a panel above the bar lists every key it takes and what each does: Esc puts it away. With two panes or more, each shows its number, for Ctrl+B `p` and the number to go to it.

Keys you bind in your `init.lua` are listed too, saying what their `desc` says, as in Neovim, and `client.keys.group` names keys that start several, as which-key.nvim's groups do:

```lua
client.keys.map("n", "<Space>ft", function() client.cmd("Files") end, {desc = "find todos"})
client.keys.group("n", "<Space>f", "my files")
```

A function bound to keys hears the count typed before them as `ev.count`, 0 if none, as Vim's `v:count`, and `ev.count1`, 1 if none.

Go back to the practice terminal, the one you split.

```check
focused practice
```

## One mode everywhere

The mode goes with you: every pane you arrive at, in this tab or another tab or workspace, is in the window's mode. Arriving in insert mode, a terminal shows its live screen, ready to type into; an editor types at its cursor.

Press `i`, then Ctrl+B and `l` to go back to the new pane: it's in insert mode too. A pane with no insert mode, such as the file explorer, shows `NORMAL`, and leaves the mode as it was for the next pane; a file you open from it opens in normal mode. If another window or an agent moves the focus, you arrive in normal mode, so nothing you type goes where you didn't choose.

```check
not focused practice
focused terminal
mode insert
```

## Close the new pane

Close the new pane, where you are, with Ctrl+B and then `x`, or `:close` from normal mode. Closing a pane hangs up its program.

Close the new one, not the practice terminal: closing that ends the lesson.

```check
panes 2
```

## Complete with Tab

The command line suggests matches as you type: a command's name, an option after `:set`, a file or folder after `:e`, a lesson after `:Tutor`. Each suggestion includes a short description of what it does. Suggestions leave your text alone. Tab fills the word before the caret; with more than one match, Tab and Shift+Tab cycle through them, and then back to what you typed. Once cycling, Down and Up move through the matches too. Enter runs the text in the prompt; Escape closes it.

Files are looked up in the background, only when the pane's files are on this machine. Typing and pasting update the suggestions without waiting for disk.

Type `:set norel`, press Tab to make it `:set norelativenumber`, and press Enter: editors number their lines from the top now, not from the cursor.

```check
option norelativenumber
```

## Go back with Up

Up and Down go back through the commands you've typed, in this window and others, keeping to those that start with what's typed. Ctrl+W deletes the word before the caret, and Ctrl+U everything before it; Home, End and the arrows move about, and Ctrl+R and a register puts in what it holds.

Type `:set`, press Up for the command you just ran, press Ctrl+W to delete its option, then type `rnu` and press Enter, to put the numbers back as they were.

```check
option relativenumber
```

## Open a tab

Type `:Terminal` and press Enter, for a terminal in a new tab. Tabs run along the bottom bar, numbered, and named after their focused pane's program.

```check
shown elsewhere
```

## Come back

`:1`, `:2` and so on go to that tab, and Ctrl+B with `]` or `[` to the next tab or the one before. Come back to this lesson's tab. Leave the other open, or close it first with `:close`.

```check
shown lesson
```

## Make a workspace

A workspace holds the tabs of one piece of work, such as a project. Their names run along the top bar. `:ws tutor` goes to the workspace called tutor, making it if there's none.

```check
workspace "tutor"
```

## And back again

Ctrl+B and then `w` opens the workspace picker: type part of a name, and Enter goes to it. Go back to the workspace you started in; it opens on this tab.

The tutor workspace closes with its last pane: `:close` it before you go, if you like.

```check
shown lesson
```

## When there's more than fits

When a workspace has more tabs than the bottom bar has room for, it shows those around the tab shown, with a quiet count at either end, such as `‹3` or `5›`, of how many are hidden that way. Click a count to scroll that way. The workspaces in the top bar do the same.

A message keeps to the room the tabs leave, cut short with `…` if it must be: hold the mouse over it to read it all. Type `:messages` to see what the bar has said lately, until the next key.

```check
messages
```

## Where you've been

Ctrl+O and Ctrl+I go back and forward through the places you've been in the tab, panes and places in files alike, as Vim's jump list does; Ctrl+Shift+O and Ctrl+Shift+I through everywhere: panes, tabs and workspaces. A count goes that many places: `3` and Ctrl+O goes back three, or nowhere if there aren't three.

Everything here is kept by the daemon, so closing the window closes nothing: the next window opens where you left off. A lesson is the exception, and closes with the window.
