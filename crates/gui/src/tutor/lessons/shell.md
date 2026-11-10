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

## Paste an image

Ctrl+Shift+Alt+V pastes the image on your clipboard, as a screenshot, into a terminal: it's kept in a file only you can read, in `~/.cache/clackatoo/pastes/` on the terminal's machine, and its path is pasted, quoted for the shell if it needs to be. Claude Code, Codex and other programs read a pasted image path as the image, so a screenshot taken on your laptop goes into Claude Code running in a terminal on your desktop. `:set pasteimage=clipboard` puts it on that machine's clipboard instead, and types Ctrl+V, for programs that read the clipboard. Ctrl+Shift+V pastes text alone, and says which keys paste an image when that's all the clipboard holds. In a chat, either attaches the image.

Copy a screenshot or any image. In the terminal, in insert mode, type `ls -l ` (with the space), press Ctrl+Shift+Alt+V, then Enter.

```check
exited 0
```

## A program's progress

A program can say how far a long task has got, as systemd and winget do, and the tab it's in says so at the end of its name: ` 42%` under way, ` …` busy with no telling how far, ` ‖` paused and ` ✗` failed. It goes once the program says it's done, or once your shell's next prompt shows the program has ended.

Run this, which says it failed 42% of the way, as a program might, and watch the tab's name while it sleeps: `printf '\e]9;4;2;42\e\\'; sleep 3; (exit 2)`

```check
exited 2
```

## Notifications from programs

A program can ask for a desktop notification, as a long build does when it ends: `OSC 9`, `OSC 777` and kitty's `OSC 99`. clackatoo shows it while you aren't looking at the program's pane: it's in another tab, or the window is in the background. It goes once you look. What it says is cleaned, as tab names are, and a program asking over and over shows a few, then one every ten seconds. `:set notify=off` turns them off with chats'.

A command that ran for 10 seconds or more tells of its end the same way, with how it ended and how long it took; `daemon.terminals.notify_after` in the daemon's config, `daemon/init.lua`, changes the 10 seconds.

`:notify phone` marks the terminal so its notifications reach your phone too, through the web app, when no one is looking at it anywhere; `:notify nophone` takes the mark off. The tab's name ends with ` 📱` while one of its terminals is marked.

In the terminal, type `:notify phone`.

```check
phone 1
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

## Pick by label

Ctrl+B and then `f`, or `:hints`, labels every link, file name, mail and IP address and git hash on screen with a letter or two, as kitty's hints and tmux-fingers do, in terminals, editors and chats alike. Type a label to open what it labels, as Ctrl+click would, asking first for a link that isn't a plain web one; what can't be opened, as a hash, is copied. With Shift on its last letter, it's copied, as Ctrl+Shift+C copies; with Alt, it's typed at the prompt. Esc puts the labels away. The `hintpattern` option adds a regular expression of your own, as `CLACKA-\d+`.

```setup
run "echo commit 7e84c2e9 is at https://example.com"
```

Press Ctrl+B and then `f`, then type the hash's label, holding Shift.

```check
copied "7e84c2e9"
```

## After a restart

clackatoo keeps your workspaces, tabs and panes as they change, so they come back after a restart, a shutdown, a crash or a power cut: each terminal as a new shell in the folder it was in, each editor on its file with its cursor where it was, explorers, boards, reviews, and chats, which go on with their agent's session once you look at one or send it a message. Running programs don't come back.

What each terminal printed comes back too, as history above its new shell's prompt: its last 10,000 lines, colours and all, dimmed, under a line saying when the restart was. It's only there to read and copy; nothing in it runs. It's written every half minute or so, so a crash keeps all but the last moments.

A terminal that was running a command, such as `cargo watch`, says so at its bottom right. In normal mode, `R` puts it at the new shell's prompt, to read, and Enter runs it; Esc dismisses it, and a click on either does the same. Nothing runs by itself. Typing into the terminal dismisses it too. This works in bash, zsh and fish, started as your shell. A command typed with a space before it is never kept, as shells keep those out of their history, nor is one bash left out of its history some other way, nor one a program only printed: clackatoo's shell integration marks each command with a secret only it and clackatoo know, and only a command marked so is offered.

To start once with no workspaces, run `clackatoo stop`, then `clackatoo daemon --fresh`.

## Keep or forget what a terminal printed

`:forget` drops what's kept of the focused terminal's history at once, and what it shows now: only what comes after is kept. `:keep history off` keeps none of it from now on, and the mark stays with the pane after a restart; `:keep history on` keeps it again.

In a terminal marked off, you can still keep one command: in normal mode, put the cursor on its prompt or output and press `gs`. That command and what it printed are kept, and nothing else.

Agents' own terminals, and the panes showing what an agent's command printed, are never kept.

## Secrets programs print

A program can mark what it prints as secret, with an escape sequence of clackatoo's own, OSC 3008: it's masked as ●●●● wherever it's shown, the phone too, never written to disk, and agents read `[secret]` in its place. `clackatoo secret -- COMMAND` marks a command's output so. A line you type a password on, where it doesn't show, is kept from agents and the disk by itself.

What you copy or search of a masked line is the mask too. To see it, put the cursor on it in normal mode and press `zv`, or Ctrl+click it: it's shown as it is, in this window, for 30 seconds, and copies see it. `zv` again masks it at once.

```setup
run "printf 'token:\\n\\033]3008;region;start;t1;secret\\007hunter2\\n\\033]3008;region;end;t1\\007'"
```

Press `zv` on the masked line, then `yy` to copy it.

```check
copied "hunter2"
```

## On a busy machine

When builds take every core, your keys still echo at once. clackatoo's daemon, which runs your terminals, asks RealtimeKit, the service desktops run so sound keeps up, to run ahead of your other programs, and so does the window. What they start never does: your shells, and everything you run in them, start as any program would.

Under systemd, the daemon also takes a group of its own, `app-clackatoo-PID.scope`, which shares the machine out: the daemon first, your terminals as usual, and agents, language servers and the terminals agents start after them. It takes the group only while you linger (`loginctl enable-linger`, which keeps your systemd running when you're logged out): without that, the group would end with your last login, and your terminals with it, so the daemon keeps out of it, and a window says so once. `clackatoo priority` says how it went, any time.

`daemon.machine.priority = false` in the daemon's config, `daemon/init.lua`, turns it off for the daemon, and `priority = "normal"` in `startup.toml` for the window.
