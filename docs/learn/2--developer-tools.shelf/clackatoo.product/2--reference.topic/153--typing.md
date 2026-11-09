---
title: Typing, Scrolling, and the Mouse
type: reference
description: What reaches a terminal's program as you type and paste, pasting an image, scrolling, and what the mouse does.
related:
  - clackatoo/reference/window
---

In insert mode, what you type goes to the focused terminal's program, and the
wheel follows the mode of the pane under the pointer.

## Typing

In insert mode, keys reach the program the way xterm sends them, which nearly
every program understands:

- Text types itself. <kbd>Ctrl</kbd> with a letter sends its control character
  (<kbd>Ctrl</kbd>+<kbd>C</kbd>, <kbd>Ctrl</kbd>+<kbd>D</kbd>…).
  <kbd>Alt</kbd> sends <kbd>Escape</kbd> before the key.
- <kbd>Enter</kbd>, <kbd>Backspace</kbd>, <kbd>Tab</kbd>,
  <kbd>Shift</kbd>+<kbd>Tab</kbd>, and <kbd>Escape</kbd> send their usual
  codes. <kbd>Ctrl</kbd>+<kbd>Backspace</kbd> deletes a word in most shells.
- Arrows, <kbd>Home</kbd>, <kbd>End</kbd>, <kbd>Insert</kbd>,
  <kbd>Delete</kbd>, <kbd>Page Up</kbd>, <kbd>Page Down</kbd> and
  <kbd>F1</kbd> to <kbd>F12</kbd> send escape sequences, with any modifiers
  held.
- When a program asks for them, arrows and the number pad send application
  codes, which Vim and less expect.

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> pastes, and
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies text
[selected with the mouse](#the-mouse); neither reaches the program. If the
program has asked for bracketed paste, the text is marked as pasted, so
editors don't treat it as typing and shells don't run it line by line.
Otherwise newlines are sent as <kbd>Enter</kbd>, but for those at the very
end, which are dropped, so the last line pasted waits at the prompt rather
than running, even with blank lines copied after it, as `yj` over a command
and the blank line under it copies. Only the end is kept from running: every
line before the last still runs, as <kbd>Enter</kbd> would run it. Either way,
control characters other than tab and line breaks are left out, as xterm
leaves them out, so text a program
[copied](~clackatoo/reference/terminal-emulation) can't act as keys when you
paste it: an escape as an arrow, or <kbd>Ctrl</kbd>+<kbd>C</kbd>. Whether the
program asked for bracketed paste is as the window last saw it, so a paste
right after a command starts can reach a program, or a shell, that has turned
it off since, and be typed as keys. The phone pastes the same way, and asks
first when lines before the last would run.

Typing while scrolled up returns the view to the live screen.

### Pasting an image

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> pastes the image
on the clipboard, such as a screenshot, into the program, in insert, normal or
visual mode; in a [chat](~clackatoo/reference/agents) it attaches the image to
the message, as <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> does there. In a
terminal, the daemon whose terminal it is, on this machine or
[another](~clackatoo/reference/remote#an-image-from-this-machine-s-clipboard),
keeps the image in a file of its own, `~/.cache/clackatoo/pastes/` then the
time it was pasted, as `20261108-224501-1.png`, and pastes the file's path, as
dropping a file on the terminal does. Claude Code, Codex, and other programs
that take images read a pasted image path as the image. At a shell's prompt
(bash, zsh, or fish) the path is quoted if it needs to be and has a space
after it; any other program gets the path as it is. What you type while the
image is on its way, as <kbd>Enter</kbd>, follows the path, never goes before
it. A path with a control character in it, from a folder named so with
`CLACKATOO_PASTES`, is never typed: the bar says so. The folder is only yours,
as is each file, and images are deleted after three days, the oldest first
once together they're over 256 MiB. An image is at most 16 million pixels and
about 6 MiB as PNG, as in chats.

With `:set pasteimage=clipboard`, the daemon instead puts the image on its own
machine's clipboard, with `wl-copy` or `xclip`, in the desktop session it
[follows](~clackatoo/reference/terminal-emulation#the-display), then types
<kbd>Ctrl</kbd>+<kbd>V</kbd>, for a program that reads the clipboard itself,
as Claude Code does. `wl-copy` or `xclip` runs with the daemon's own
environment, with the desktop's display variables in place of the daemon's,
never a terminal's or an agent's. That needs you signed in to that machine's
desktop: if you aren't, or neither program is installed, it pastes the path
instead, and the bottom bar says why.

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> pastes text only. When the
clipboard holds an image and no text, the bottom bar says which keys paste it:
`PasteImage`'s, as you've bound them. `:Tutor` has a step on it.

Text can be typed through an input method, as Chinese, Japanese and Korean
are, such as IBus on GNOME. What's being composed shows underlined where it
will go, at the cursor or in the bottom bar, with the input method's choices
beside it, and the text chosen is typed as if keyed. The input method is used
in insert mode, the command line and searches; in normal mode, keys are
clackatoo's commands, so it's left out, and it's never used for a password.
`:set noime` turns it off.

## Scrolling

The mouse wheel scrolls the pane under the pointer. Over a window bar or gap,
it scrolls the focused pane instead; this also works for editors, explorers,
boards and reviews, including smooth scrolling and trackpad motion. In a
terminal, it scrolls history in either mode. On the alternate screen, which
full-screen programs use and which has no history, the wheel sends arrow keys
instead in insert mode, so less, Vim and htop scroll themselves. In normal
mode it moves normal mode's cursor up and down there, and never reaches the
program. The wheel follows the mode of the pane under the pointer, and the
focused pane's is the window's. A program that takes [the mouse](#the-mouse)
gets the wheel itself, in insert mode.

While the view is scrolled up, a pill at the pane's bottom right says how far,
and how to get back: `↓ 42 lines · G to return` in normal mode,
`type to return` in insert mode, where typing goes back to the live screen. A
pane without the focus says only how far. In a pane too small for the pill,
the bottom bar's note says it instead, as `↑42`. New output doesn't move what
you're reading.

## The mouse

Selecting a pane focuses it. What else the mouse does depends on the program
in the pane.

**Selecting.** Usually the mouse selects text:

- Dragging selects characters. Dragging past the top or bottom of the pane
  scrolls it.
- A double click selects a word. Paths and URLs count as one word, so
  `~/src/main.rs` comes whole.
- A triple click selects whole lines.
- With <kbd>Alt</kbd> held, or <kbd>Ctrl</kbd> and <kbd>Alt</kbd>, dragging
  selects a block of columns. Over a program that takes the mouse, add
  <kbd>Shift</kbd>.

Letting go copies the selection to the primary selection, `"*`, and a middle
click pastes it, as elsewhere on Linux. It goes only there, as Vim puts a
mouse selection in `"*` and leaves the unnamed register as it was, so `p` in
an editor still puts what you last yanked. A middle click also focuses and
pastes into editors and chats, and into searches or names already being typed
in explorers, boards and reviews.
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies it to the clipboard, and
`"+`, as shown: by characters, with no newline after a last line, even of
lines a triple click selected. And `y` in normal mode yanks it, as `y` yanks
anything, into the
[registers](~clackatoo/reference/normal-mode#selecting-and-copying); a count
or a register typed first, as `"a`, waits with the selection, as in Vim's
visual mode, so `"ay` keeps it in `"a`. Meanwhile the bar says VISUAL (or
V-LINE, or V-BLOCK). Any other key forgets it. The selection stays highlighted
until the next key or click. In normal mode, a click also moves the cursor
there.

**Programs that take the mouse.** Programs such as Neovim, htop and less can
ask for clicks, drags and the wheel, and get them while their pane is in
insert mode on the live screen. The pointer is then an arrow rather than a
text cursor. Hold <kbd>Shift</kbd> to select text anyway. A drag stays with
the pane it started in, even when the pointer leaves it.

Programs that ask are also told when their pane gains or loses the focus,
which Neovim uses to reload files changed meanwhile.


**Links.** Hold <kbd>Ctrl</kbd> and the link under the pointer is underlined,
and the pointer becomes a hand. <kbd>Ctrl</kbd>+click opens it. Links are what
programs print as links (`ls --hyperlink`, gcc, systemd, and git do), plus
URLs (`http://`, `https://`, `file://`, `mailto:`) and file paths in the text,
such as `src/main.rs:12:5` from rustc or `src/main.rs:12:` from grep. A URL
opens in your browser, or whatever the desktop opens it with. A link's text
needn't be where it goes, so only a plain `http://` or `https://` address, or
`mailto:` and an address alone, opens at once; for any other, such as `ssh://`
or `git://`, whose handler could take part of it for an option, the bar asks,
with where it goes shown whole under the question, every character but ASCII's
letters, digits and punctuation written as `<U+202E>`, so nothing in it can
hide or pass for something else: `y` opens it, and any other key, a paste, or
a key you didn't press for the question (one a macro or binding types, or one
pressed earlier and held while the window waited) doesn't. One too long to
show whole, over about 250 characters, isn't opened, and the bar says so; nor
is one the window was too small to show whole, until it's opened again in a
bigger one. A file opens in an editor pane at that line: in an editor already
in the tab, else beside the terminal if it's wide enough, else in a tab of its
own. A relative path is from the directory the shell is in.
<kbd>Ctrl</kbd>+<kbd>O</kbd> comes back. A `file://` link naming another
machine isn't opened, and the bar says why. When a program takes the mouse,
<kbd>Ctrl</kbd>+click is the program's, and
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+click opens the link.
