# Commands and links in terminals

What clackatoo knows of the commands you run: going between them with `[[` and `]]`, copying a command's output with `yib`, and opening the links and file names programs print with `gx`.

clackatoo knows where each command's prompt and output are when the shell marks them: bash and zsh do when clackatoo starts them as your shell, and fish 4 does by itself. In other shells these steps can't tick off; `]s` goes on.

```setup
terminal
run "printf 'one\\ntwo\\nthree\\n'"
run "ls"
```

```practice notes.txt
first line
second line
the third line
fourth line
```

## Copy a command's output

The terminal has run two commands. In normal mode, `[[` goes to the previous command's prompt, and `]]` to the next. `yib` copies the output of the command the cursor is in, from its first line to the line before the next prompt; if the command printed nothing, the bar says so.

Go to normal mode with Ctrl+;, press `[[` until the cursor is on the `printf` command's prompt, then `j` into its output, and type `yib`.

```check
copied "one\ntwo\nthree"
```

## Copy the command too

`yab` copies the command with its prompt and its output, ready to paste into a bug report. `vib` and `vab` select them instead.

With the cursor in the same command, type `yab`.

```check
copied "printf"
copied "three"
```

## Commands that failed

A command that fails, exiting with a status other than 0, ends its prompt's line with a dim red `✗` and the status, as `✗ 1`, in history too, so you can find it scrolling back. The mark isn't part of the text: nothing copies it. The web app on your phone marks them too.

In the terminal, run `false`, which only fails.

```check
exited 1
```

## Marking every command

`:set blockmarks=all` marks the commands that worked too, with a faint `✓`; `none` marks none, and `failed`, as it starts, marks only those that failed. The tab's name is the shell's folder, while the program sets no title.

Type `:set blockmarks=all` in normal mode.

```check
option blockmarks=all
```

## Open a file name

Programs print file names and places in them, as `grep -n` and compilers do: `src/main.rs:12:5`. Ctrl+click opens one, as does `gx` on it in normal mode: a URL in your browser, and a file in an editor at that line. A relative name is from the directory the shell is in.

```setup
run "grep -Hn third notes.txt"
```

In normal mode, put the cursor on `notes.txt:3` in grep's output, and press `gx`.

```check
file "notes.txt"
line 3
```

## Links

Links are also what programs print as links, such as `ls --hyperlink`, gcc and git do, and URLs: `http://`, `https://`, `file://` and `mailto:`. Hold Ctrl and the link under the pointer is underlined. Ctrl+O comes back from a file a link opened.

When a program takes the mouse, as htop does, Ctrl+click is the program's, and Ctrl+Shift+click opens the link.

## After a restart

clackatoo keeps your workspaces, tabs and panes as they change, so they come back after a restart, a crash or a power cut: each terminal as a new shell in the folder it was in, each editor on its file with its cursor where it was, explorers, boards, reviews, and chats, which go on with their agent's session once you look at one or send it a message. Scrollback and running programs don't come back.

A terminal that was running a command, such as `cargo watch`, says so at its bottom right. In normal mode, `R` puts it at the new shell's prompt, to read, and Enter runs it; Esc dismisses it, and a click on either does the same. Nothing runs by itself. Typing into the terminal dismisses it too. This works in bash, zsh and fish, started as your shell. A command typed with a space before it is never kept, as shells keep those out of their history, nor is one bash left out of its history some other way, nor one a program only printed: clackatoo's shell integration marks each command with a secret only it and clackatoo know, and only a command marked so is offered.

To start once with no workspaces, run `clackatoo stop`, then `clackatoo daemon --fresh`.
