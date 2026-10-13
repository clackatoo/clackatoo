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
