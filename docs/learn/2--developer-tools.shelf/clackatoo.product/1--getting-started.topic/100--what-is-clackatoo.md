---
title: What Is clackatoo
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

Everything else is a **client** of the daemon and talks to it over one protocol: the window, the `clackatoo` command, the [web app on your phone](~clackatoo/reference/phone), other machines' daemons, and agents. Clients can be on [another machine](~clackatoo/reference/remote), reaching the daemon through ssh. Because the daemon owns the terminals:

- closing a window closes nothing, and reopening it brings everything back;
- a script can do anything a window can, because it uses the same requests;
- an agent is one more client. It can start terminals, type into them, and read their screens.

The daemon's terminals live on one thread, waiting on every PTY and connection at once. A program's output goes straight from its PTY into the grid on that thread, with no locks and no handoff between threads. Slower work, such as parsing files to color them, asking git, or reading a language server's big answers, goes to threads of its own, so it never holds up a key.

## What exists today

The daemon; [a window](~clackatoo/reference/window) that shows workspaces of tabs and split panes, with Vim's modes for scrolling, selecting, copying and searching, and a command line for opening panes; [an editor](~clackatoo/reference/editor) of its own, with tree-sitter's colors and language servers; [file explorers](~clackatoo/reference/explorer); [chats with agents](~clackatoo/reference/agents) such as Claude Code, Codex and Gemini, which can open panes to show you what they do, and [boards](~clackatoo/reference/boards) they share with you; a [web app](~clackatoo/reference/phone) for your phone; and a command-line client that can start terminals, type into them, show their screens and close them. Windows and commands work with a daemon on [another machine](~clackatoo/reference/remote), and your daemon can bring another machine's workspaces in beside your own. The [quick start](~clackatoo/getting-started/quick-start) walks through the window and the command line.

clackatoo runs on Linux.
