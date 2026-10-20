---
title: What is clackatoo
type: concept
description: clackatoo is a terminal workspace aiming to be very fast and very light. Its daemon owns every terminal, and windows, scripts and agents are all clients of it.
related:
  - clackatoo/getting-started/quick-start
  - clackatoo/reference/window
  - clackatoo/reference/cli
  - clackatoo/reference/terminal-emulation
---

**clackatoo** is a terminal workspace: a terminal emulator that grows into a place to work, with an agent harness built in. Its first priority is to be very fast and very light.

## The daemon owns the terminals

Most terminal emulators are one program: close the window and your shells die with it. In clackatoo, a background **daemon** owns every terminal. That means the pseudo-terminal each program runs on (its PTY), the program itself, and the grid of characters its output builds.

Everything else is a **client** of the daemon and talks to it over one protocol: the window, the `clackatoo` command, and later agents. Clients can be on [another machine](~clackatoo/reference/remote), reaching the daemon through ssh. Because the daemon owns the terminals:

- closing a window closes nothing, and reopening it brings everything back;
- a script can do anything a window can, because it uses the same requests;
- an agent is just another client. It can start terminals, type into them and read their screens.

The daemon is one thread waiting on every PTY and connection at once. A program's output goes straight from its PTY into the grid on that thread, with no locks and no handoff between threads.

## What exists today

The daemon; [a window](~clackatoo/reference/window) that shows workspaces of tabs and split panes, with Vim-like modes for scrolling, selecting, copying and searching, and a command line for opening panes; and a command-line client that can start terminals, type into them, show their screens and close them. Both clients work with a daemon on [another machine](~clackatoo/reference/remote). The [quick start](~clackatoo/getting-started/quick-start) walks through both.

clackatoo runs on Linux.
