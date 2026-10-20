# Background tabs

Keeping something running, as a web server, out of the tab bar: Ctrl+B b t sends a tab to the background, an icon after the tabs counts those running, and Ctrl+B b l lists them, with a look at each one's screen.

```setup
terminal
```

## Send a tab to the background

A background tab keeps running, splits and all, but leaves the tab bar, so a server you only glance at now and then doesn't take a tab's place. Each workspace has its own, every window and the web app on your phone see them, and they come back after a restart, as tabs do.

Open a new tab with Ctrl+B c, start something that keeps running, as `sleep 600`, then press Ctrl+B, then `b`, then `t`.

```check
background 1
```

## The icon

After the tabs, `⚙ 1` says how many of the workspace's background tabs are running a command, as their shells' marks say: one at its prompt again isn't counted, and one whose shell doesn't mark its commands counts while it's alive. A command in one that fails turns the icon red, with its exit status, as `⚙ 1 ✗ 2`, until you look at the list.

## The list

Ctrl+B b l, or `:Background`, or a click on the icon, lists the background tabs: this workspace's first, then the others', each with how it's getting on, and beside them the last screenful of the one picked, as the finder previews a file.

Press Ctrl+B, then `b`, then `l`.

```check
finder background
```

## Bring it back

In the list, Enter brings the one picked back as a tab, after the others. Ctrl+D dismisses it, closing its terminals; one still running asks first, and a second Ctrl+D ends it. A background tab whose shell has exited stays, its screen readable, until it's dismissed or brought back; brought back, its shell starts again in its folder, offering `R` to put what it ran at the prompt.

Press Enter.

```check
background 0
```

## Start one in the background

`:bg CMD` starts a command straight in a new background tab, in the focused pane's folder, typed at a new shell's prompt as you'd type it, without showing it. `clackatoo bg -- CMD` does the same from a shell, and agents start dev servers so with their `run_in_background` tool.

In normal mode, type `:bg python3 -m http.server 8000`.

```check
background 1
```

## Ports

The list says the ports each one's programs listen on, as `· :8000`, looked at again every two seconds while it's open. In a window on another machine than the daemon, Ctrl+F in the list forwards them here, as `:PortForward 8000` does.

Press Ctrl+B, then `b`, then `l`, and look for `:8000` beside it.

```check
finder background
```

## A pane alone

Ctrl+B b p sends only the focused pane to the background, in a tab of its own; a tab's last pane takes the tab with it. `:bg` and `:bgpane` do what the keys do. The workspace's only tab can go too: a shell takes its place.

## Keep it running

After a restart, nothing runs by itself: a terminal that was running a command offers `R` to put it at its new shell's prompt. `:keep` marks a terminal to run its command again by itself instead, the command its shell runs or ran last, as your shell said it: never what a program printed, nor what you run later. `:keep always` runs it again as it ends too, a moment later, waiting longer each time it ends soon after starting, and giving up after five such ends in a row. Its tab says ` ↻`, or ` ↻✗` once given up on; typing into it while it runs, as Ctrl+C, pauses it, ` ↻‖`, and it offers `R` instead, until you run it there again. `:keep off` takes the mark away. You set the mark, here or with `clackatoo keep`; agents' tools don't.

In the terminal beside this lesson, run `echo kept`, then in normal mode type `:keep`.

```check
kept 1
```
