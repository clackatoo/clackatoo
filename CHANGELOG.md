# Changelog

## 2026.10.68 (2026-10-21)

### Fixes

- **gui:** a count before gg, or G as an operator's target, goes to that line (CLACKA-127) (96244b5)

## 2026.10.67 (2026-10-21)

### Fixes

- **daemon:** an editor brought back keeps the directory its file was opened from (CLACKA-126) (b24a7c5)
- **daemon:** a relative kept directory falls back to the file's own, and the docs say so (CLACKA-126) (c140bec)

## 2026.10.66 (2026-10-21)

### Fixes

- **gui:** H, L, Ctrl+D and their kin move a terminal's normal-mode cursor as many lines as they say (CLACKA-125) (f969be0)
- the wheel never types into a program in normal mode, and moves its cursor on the alternate screen (CLACKA-125) (c1314a4)
- the wheel leaves a pane without the focus in its own mode (CLACKA-125) (055b6d9)

## 2026.10.65 (2026-10-21)

### Fixes

- **editor:** :move carries marks with the lines it moves, so gv reselects a moved block (CLACKA-124) (31ce7a0)
- **editor:** only whole lines with text in them are taken for a move, and a move across a selection's end crosses it over (CLACKA-124) (de2bb03)

## 2026.10.64 (2026-10-20)

### Features

- background tabs, kept running out of the tab bar (CLACKA-121) (2b61f17)
- start commands in the background, and see the ports they listen on (CLACKA-121) (a8fbaa2)
- keep a terminal running, by a mark only the user sets (CLACKA-121) (715f4c5)

### Fixes

- **daemon:** the boards' lock waits a moment for a copy a starting process holds, as the workspaces' does (a2c94d5)
- one look for background tabs' ports at a time; the shell's marks and rebinding Ctrl+B b in the docs (6050928)
- kept commands only ever typed into their own shell, and the review's other findings (CLACKA-121) (7bea120)
- **docs:** what keeping a terminal running guards against, and what it doesn't (CLACKA-121) (5ccd770)
- a shell has its terminal to itself before anything's typed, and a pause outlasts an upgrade (CLACKA-121) (660bce8)

## 2026.10.63 (2026-10-20)

### Fixes

- snap found through a link of another name, and the alias test kept from the system's zshrc (bfd6b4f)

## 2026.10.62 (2026-10-20)

### Features

- **daemon:** the display follows the desktop's login (50449cc)

### Fixes

- **daemon:** the display followed is one that's still there, and read more often (3515f4c)

## 2026.10.61 (2026-10-19)

### Features

- **gui:** rendered Markdown in the editor, toggled with its text (9f034ed)

### Fixes

- **gui:** rendered Markdown as a pager, and keys typed into it kept as the editor's (d6376d7)

## 2026.10.60 (2026-10-19)

### Features

- **daemon,protocol,gui:** machines passed on through the daemons connected to them (ad8283b)

## 2026.10.59 (2026-10-19)

### Features

- **daemon,protocol,gui:** a pane of this machine's in a workspace of a machine that can't reach it (9001ed9)

## 2026.10.58 (2026-10-18)

### Features

- **daemon,protocol,gui:** a pane on another machine in your own workspace, and new workspaces there (b92b62c)

## 2026.10.57 (2026-10-18)

### Features

- **daemon,protocol,gui:** everything in another machine's workspaces works through the daemon, editors to port forwards (68e2db4)

## 2026.10.56 (2026-10-18)

### Features

- **daemon,protocol,gui:** :connect brings another machine's workspaces in, its terminals and chats working through the daemon (7411cd0)

## 2026.10.55 (2026-10-18)

### Features

- **daemon,protocol,gui:** daemons know which machine they are, and connect to each other (12f301a)

## 2026.10.54 (2026-10-16)

### Features

- **daemon,term:** offer a restored command only from the shell integration's own mark (39670b1)

## 2026.10.53 (2026-10-16)

### Features

- **daemon,gui:** workspaces, tabs and panes come back after a restart (4b37b19)

## 2026.10.52 (2026-10-16)

### Features

- **daemon,gui:** changes not written outlive a crash or a stop, and :recover shows them (a1bbebd)

## 2026.10.51 (2026-10-15)

### Features

- **gui,daemon:** :e alone opens the explorer beside a blank file, which :w NAME names (c20d5d4)

## 2026.10.50 (2026-10-15)

### Fixes

- **daemon:** gone servers and agents refused at once, and waits that survive clear (1452afe)

## 2026.10.49 (2026-10-15)

### Features

- **gui,web:** counts for config bindings and jumps, desc and groups in the panel of keys, failed marks on the phone (a78a5c5)

## 2026.10.48 (2026-10-15)

### Features

- **gui:** block visual mode, Ctrl+V, in editors and terminals (3e5858a)

## 2026.10.47 (2026-10-15)

### Features

- **gui:** failed commands marked, a scrollback pill, and tabs named after their folders (d10749a)

## 2026.10.46 (2026-10-15)

### Features

- **gui:** completion under the word, coloured previews of unopened files, and a first chat without a pause (df93ef9)

## 2026.10.45 (2026-10-15)

### Features

- **gui:** a panel of the keys a prefix takes, prompts that leave one thing focused, and the :w plan asking in place (f1c804c)

## 2026.10.44 (2026-10-15)

### Features

- **gui:** amber kept for focus and action, a faint end of file, an empty chat that says where, and Copy on hover (2c3506a)

## 2026.10.43 (2026-10-15)

### Features

- **gui:** errors that say "did you mean", and a finder that colours what it shows (f1f049d)

## 2026.10.42 (2026-10-15)

### Features

- **gui:** chat tables, coloured code, one left edge, a solid cursor, and faster typing (e587466)

## 2026.10.41 (2026-10-15)

### Features

- **gui:** lessons that read like the chat, explorer keys, and selections that end at the text (ac43cd8)

## 2026.10.40 (2026-10-15)

### Features

- **gui:** a real command line, counts in terminal normal mode, and a word for Ctrl+V (698d112)

## 2026.10.39 (2026-10-15)

### Fixes

- **gui:** answer a chat's question from normal mode, and polish what's hard to see (72667a1)

## 2026.10.38 (2026-10-15)

### Speed

- **remote:** pace forwarded ports as background traffic, so a download never fills a slow link's queue ahead of keys (f61fe7a)

## 2026.10.37 (2026-10-15)

### Features

- **gui:** crowded bars scroll, and messages keep off the tabs (7d3f0e9)

## 2026.10.36 (2026-10-15)

### Features

- **gui:** look at the real window headlessly, from a script, with screenshots and timings (210d4cf)

### Fixes

- **daemon:** wait for a gone peer's last messages by the parser's progress (7079e56)
- open every tutor lesson, and say a chat waits for you while it asks (0b53366)

## 2026.10.35 (2026-10-15)

### Fixes

- **daemon:** read a language server or agent whose input broke until it exits (b9cdb2d)

## 2026.10.34 (2026-10-15)

### Speed

- **daemon:** let what agents and language servers flood in give way to keys (5d9d7c4)

## 2026.10.33 (2026-10-15)

### Features

- **gui:** take the launcher's startup token, so the window takes focus and the spinner ends (73b945c)

## 2026.10.32 (2026-10-15)

### Speed

- **daemon:** pick the best completion items without reading or sorting them all (b892de8)

## 2026.10.31 (2026-10-15)

### Speed

- **daemon:** parse and free big agent and language-server messages off the event loop (d9edded)

## 2026.10.30 (2026-10-15)

### Features

- **remote:** :PortForward forwards a port from the window's machine to the daemon's, over the window's QUIC connection (c953783)

## 2026.10.29 (2026-10-15)

### Fixes

- **editor:** > and < are refused in a folder's listing, and a double click from visual mode keeps the last selection for gv (5ca4bbe)

## 2026.10.28 (2026-10-14)

### Features

- **editor:** . repeats changes made in visual mode, and gv selects the last selection again (b08a32b)

## 2026.10.27 (2026-10-14)

### Features

- **explorer:** a .. line tops every folder listing, going up on Enter and naming nothing (df3d046)

## 2026.10.26 (2026-10-14)

### Features

- **editor:** drag the mouse to select, by words after a double click and lines after a triple (11261e9)

## 2026.10.25 (2026-10-14)

### Speed

- **daemon:** let a flood give way to keys and echoes in other terminals (d1dc1cf)

## 2026.10.24 (2026-10-14)

### Features

- **explorer:** show a .. line at the top of the tree that goes up to the parent folder (52568ef)

## 2026.10.23 (2026-10-14)

### Features

- **chat:** show how full a chat's context is and its account's limits, and mark chats that are working, asking or unseen across workspaces (a015b33)

## 2026.10.22 (2026-10-14)

### Features

- **editor:** draw language servers' inlay hints between the code, never as part of it (d06a81b)

## 2026.10.21 (2026-10-14)

### Fixes

- **chat:** say what the agent is doing above the box, always in view, with pulsing dots, and mark its tab (88dcb6b)

## 2026.10.20 (2026-10-14)

### Features

- **editor:** light the other uses of the name the cursor rests on, format a selection, and say why a format failed (7838192)

## 2026.10.19 (2026-10-14)

### Features

- **editor:** words as each language reads them, so names such as margin-top complete, move and search as one word (2a4e028)

## 2026.10.18 (2026-10-14)

### Features

- **editor:** list a file's symbols and find the project's in the finder, and check a rename first (68cf823)

## 2026.10.17 (2026-10-14)

### Fixes

- **daemon:** stop gracefully on SIGTERM, SIGINT and SIGHUP, and hold a signal that comes during an upgrade for the next build (ef7d2e3)

## 2026.10.16 (2026-10-14)

### Features

- **editor:** colour the code in K's panel, underline what it's about, and cancel questions asked again (7761e3e)

### Fixes

- **daemon:** run agents, language servers, the browser and git in process groups of their own, so stopping one ends what it started; read agents' stderr a budget at a time (90f917d)

## 2026.10.15 (2026-10-14)

### Features

- **web:** terminals on the phone, at their own size, typed into, and fitted on request (2739121)

## 2026.10.14 (2026-10-14)

### Fixes

- **lsp:** shut servers down before exit, stop them with the daemon, keep their last words, and watch files for them (ec16dbd)

## 2026.10.13 (2026-10-14)

### Features

- **editor:** re-indent a line once else or end is typed first on it, and find a text-object piece once per inside (3d0dd33)

## 2026.10.12 (2026-10-14)

### Features

- **window:** one mode for the whole window, shared by every pane, tab and workspace (bc6ca9d)

## 2026.10.11 (2026-10-14)

### Features

- **protocol:** windows and daemons a release or so apart still talk (084e278)

## 2026.10.10 (2026-10-13)

### Features

- **agents:** :Agent opens a start page to start a chat or go on with one (353cd31)

## 2026.10.9 (2026-10-13)

### Features

- **editor:** editors open folders, as oil.nvim's listings, to browse and edit (c322f23)

### Fixes

- **agents:** say when an agent is slow to open a chat's session, with Retry and Stop (802c6f6)

## 2026.10.8 (2026-10-13)

### Fixes

- **text:** refuse edits whose pieces add up past any text's length (ddf8c8d)
- **protocol:** refuse layouts nested too deeply to read (3bd8de7)
- **daemon:** numbers from a window at the end of their range no longer overflow (9f09174)
- **daemon:** refuse splits deeper than a window can read (fed79a7)
- **remote:** the loss log says packets found lost, and starts again on a new path (c11b852)

## 2026.10.7 (2026-10-13)

### Features

- **review:** a chat's review takes in its subagents' changes, open or closed (c3ebfbe)
- phones control clackatoo through a web app the daemon serves; protocol 69 (7e0efef)

### Fixes

- **review:** which chat holds a closed subagent's card is known again after an upgrade; protocol 68 (0edadfa)
- **web:** a connection can't stall or swell the daemon, and Tailscale is trusted only through a socket of the user's own (acceebe)
- **web:** password tries are limited daemon-wide and lock after fifty wrong, and the socket is made where only the user can reach it (b19bbcd)

## 2026.10.6 (2026-10-13)

### Features

- **editor:** motions and text objects work on lines of any length (7f923ce)
- **agents:** agents drive a browser of clackatoo's, and you watch the page in a pane (1c19807)
- **gui:** the font and its size as options, set in config and live with :set (0cf4523)

### Fixes

- **git:** never run a repository's own filter drivers or hooks (c5db812)
- **git:** never fetch, find a stray git directory, or run git that can't be made safe (08fc21c)
- **explorer:** the check that git honours its environment ignores an inherited GIT_CONFIG, as git() does (a34c492)
- **daemon:** an agent's last change is its own, a folder made again is watched again, and a stopping daemon's QUIC closes reach their clients (d4c5453)
- **daemon:** a stop waits only for closes still to be sent, a path check isn't taken for the close, and a folder gone before it's watched is listed again (8926311)
- **explorer:** a move between file systems deletes its source through the directories it copied, never by path (352130a)
- **daemon:** an upgrade waits for file operations under way, so no copy or move is cut short (1ca8fac)
- **explorer:** a move between file systems never empties a mount point, and tells a file given its old time back (b5dae13)
- **daemon:** a file operation that panics is still answered, and upgrades still go ahead (d7a61df)
- **editor:** a laid out line drops what a cell can't hold, as a terminal's row does (1f00db3)
- **browser:** no files, downloads off in every context, a browser on the wrong profile killed unasked, and no page can take the daemon down (bda42dd)
- **browser:** a snap starts only when $HOME is the home the system gives it, and the tests that start the installed Chromium are opt-in (88cbc32)

### Speed

- **motion:** a walk finds where a line's text ends once, not at every step (63b4dd7)

## 2026.10.5 (2026-10-13)

### Features

- **remote:** a window meeting a newer daemon updates clackatoo with Enter, and opens again connected where it was (6cefcbd)
- **chat:** the time between turns sent far apart, Copy under each answer, and drag-selecting past the edge scrolls (c06d0e7)

### Fixes

- **remote:** updating for a newer daemon reopens only once a build is put in place, ignores held keys, and runs one update at a time (3e8e7f4)
- **remote:** a window whose program an earlier update replaced opens again as it, rather than saying nothing is newer (ea7c529)
- **chat:** losing the focus ends a drag, the files line fits after Copy, the time zone is kept, and no time over a move to another agent (424dac8)

## 2026.10.4 (2026-10-13)

### Features

- subagents for every agent: background chats as a card, fan-out, profiles (19e87b6)
- **explorer:** edit folders as text, oil.nvim's way, and :w to make the changes (6ea0c39)
- **explorer:** linked worktrees' excludes, and git status for repositories inside another (933dcc8)

### Fixes

- **agents:** subagents never do more than the chat that started them, and nothing is closed that anyone still needs (e57a7bf)
- **explorer:** the editable listing's review: planning always ends, steps expect what was listed, git's fsmonitor and diff helpers are off (b5a6d2b)

## 2026.10.3 (2026-10-13)

### Features

- connect to a machine from a picker: the launcher's Connect to…, its recent hosts, and :connect (c4ee555)

### Fixes

- **editor:** a line is plain only if its last character is too, and plain again once it is (b06905c)
- **release:** scripts/deploy builds only the tag it waited for, never a HEAD moved meanwhile (4247fa3)

### Speed

- **editor:** keep laid-out lines across edits, laying out only from the change (1b21c1b)

## 2026.10.2 (2026-10-13)

### Features

- review agents' changes hunk by hunk: gutter colour, review panes, keep and reject, line comments, blame (9b24b75)
- one model menu across agents: a chat moves from Claude Code to Codex and back (4c8732d)

## 2026.10.1 (2026-10-13)

### Features

- agents message, start, read and wait for each other's chats, and share boards (3efd5a7)
- :Tutor lessons that teach clackatoo, and what's new after an upgrade (d1590ca)
- **cli:** desktop install puts clackatoo in the app launcher, and update keeps it current (15b1680)
- predictive local echo in terminals, as mosh does it, over slow links (a652a2c)
- claudetask option takes Claude Code's Task tool away, and agents are told when to use start_chat (040eab2)
- remote diagnostics: timestamped logs with every close's reason, ping --soak, and a network-impairment harness (ccd83ff)
- remote windows reconnect by themselves, keeping their panes (2c8d623)
- **tutor:** the remote lesson and what's new tell of reconnecting, and edits kept across drops (70f4806)

### Fixes

- remote sessions ride out outages, lossy links and ssh dying, and hear when the daemon goes (8921708)
- what an editor types while a remote connection is down is made once it's back (2f18731)
- keys for terminals that can't reach the daemon in time are turned down, not sent late (4541db1)
- a remote connection that drops soon after it's made counts as a failed try (c2417e9)
- the daemon lets go of remote clients that vanish or fall behind, and sends twice only to checked addresses (8f63410)
- a window's log is moved aside as soon as it passes 8 MB, not only at startup (38ca4d4)
- edits kept across a reconnect come back exactly as live, or are kept, never garbled (6c03159)

## 2026.10.0 (2026-10-13)

### Features

- **term:** terminal emulation core with our own grid (6435df7)
- **protocol:** wire messages, screen snapshots and framing (745a05b)
- **daemon:** own terminals on PTYs and serve clients over a Unix socket (2884296)
- **cli:** the clackatoo command (5ea53a2)
- **bench:** emulator throughput and end-to-end benchmarks (70e92f5)
- **term:** our own escape-sequence parser, replacing vte (0be0ef6)
- **bench:** daemon CPU per MiB, and measuring another binary (78ab661)
- **bench:** CPU per MiB of a bare PTY reader, the floor (2b35a19)
- **bench:** scrollback memory for typical lines too (7e8e61f)
- **term:** resize, rewrapping the primary screen (f0b31cb)
- resize terminals over the protocol (a77548e)
- **daemon:** compact scrollback once a terminal's output pauses (892f61d)
- **term:** track which screen lines changed (3b9af58)
- **term:** rebuild rows from text, and record input modes (75632ae)
- push terminal views to attached clients (protocol v3) (56980b7)
- **gui:** a window that mirrors a terminal (de87b25)
- **gui:** the cockatoo icon, and an app ID for desktops to find it by (0b6035e)
- **gui:** type into the window (bc193e0)
- **term:** number history lines, and read any range of them (53414be)
- normal mode, scrollback, paste and drawn box characters (protocol v4) (93d7474)
- a normal-mode cursor with Vim motions, and program cursor shapes (protocol v5) (e75f92e)
- select, copy and search in normal mode (protocol v6) (5ee5b49)
- replace a daemon from an older build, and clackatoo stop (a59fd41)
- use the daemon on another machine through ssh (--remote) (7a3f1b9)
- clackatoo ping, to time requests and key echoes, locally or remotely (e071ae2)
- ask for ssh passwords in the window, and show it while connecting (b82b675)
- client.toml and clackatoo config, with a renderer setting for OpenGL (acfe295)
- workspaces, tabs and split panes, kept by the daemon, with a : command line (87b16dd)
- Ctrl+; switches to normal mode (450de6a)
- Ctrl+hjkl move between panes, :N between tabs, and normal mode carries over (862d228)
- the window draws its own frame: a top bar with the workspaces and window buttons, and a status line with the tabs, mode and command line (b503fb0)
- a modern frame: panes on rounded cards, the desktop's font and buttons, a palette in the middle, and rounded window corners (9e2071c)
- a gradient backdrop, glass names and buttons, and neutral grey cards (a292812)
- a quieter frame, after Tern: near-black navy, soft picked names, bare icons, faint edges and a glow on the focused card (15a3a8f)
- right-click menus to rename and close workspaces and tabs, named tabs, + buttons, and the backdrop bluest at the top right (4d78ad9)
- remote clients talk QUIC, carried inside the ssh connection (aa1d2ea)
- over QUIC, each terminal's view updates come on a stream of their own (1a8ece1)
- remote connections move onto UDP when it gets through, and outlive ssh (202daf7)
- the window says when a remote daemon is out of contact, and UDP is tried with probes instead of sending everything both ways (4072d89)
- **term:** snapshot a terminal's whole state, and restore it (c98d996)
- upgrade the daemon in place, keeping its terminals running (protocol 11) (e79f191)
- **term:** record the mouse reporting and focus modes programs ask for, and how to encode their events (protocol 12) (9d56be6)
- the mouse selects text, and goes to programs that ask for it (0ccff4f)
- resize panes by dragging the gaps between them, or with Alt and hjkl (protocol 13) (a80b5f5)
- **term:** shell blocks from OSC 133 marks, and the working directory from OSC 7 (83a279a)
- ask the daemon for the block at, before or after a line (protocol 14) (3768ed9)
- **gui:** y takes motions and text objects, ib and ab are shell blocks, [[ and ]] jump between them (605c663)
- **daemon:** upgrading in place keeps each terminal's shell blocks (handover version 2) (4a53803)
- **daemon:** bash started as the user's shell marks its commands too (a2e27fb)
- **gui:** text copied to the clipboard is highlighted for a moment (40ede4d)
- **gui:** a flatter look: panes side by side with lines between, the mode named at the bottom left (f1dc4fe)
- **gui:** workspaces named with a number don't show how many tabs they have (34dbe47)
- **gui:** Ctrl+B commands for tabs, panes and workspaces, and jumping back and forth with Ctrl+O and Ctrl+I (800bbda)
- **gui:** joined emoji, keycaps and accents are shaped, so the font's own glyph is drawn (528894a)
- **gui:** typing through an input method, with the text being composed drawn at the cursor (53b7bf7)
- **daemon:** chats with agents such as Claude Code, as terminals the daemon writes, over ACP (e5a0f00)
- **gui:** :Agent and Ctrl+B a open a chat with an agent (6c53a70)
- chats drawn as a conversation, with a box to type in, not written into a terminal (3c2029e)
- a chat's settings in its box, starting in Auto mode (5488b2b)
- **gui:** the pointer is a hand over what a chat's clicks do something on (e165d4c)
- **gui:** models picked by family, with their versions behind a button, and effort on a slider (1da8e5d)
- commands and changes to files on cards in a chat (33a2207)
- line numbers on a chat's change cards (212f6b7)
- the window's config in Luau, with key bindings, keypress handlers and panes (d5d21fe)
- **text:** edits, reconciling concurrent ones, and undo per author (bf32003)
- edit files in the window, with buffers kept by the daemon (b65ddee)
- colour files by tree-sitter grammars loaded as WebAssembly, and install languages (35b885d)
- line numbers in editors, and options set by config or :set (87a90b5)
- editor options for tabs, indenting, the view and search case (41e072a)
- language servers in the daemon, and their diagnostics in editors (bdc0ba4)
- diagnostics written after lines, numbered in colour, and ]d and [d (ef53285)
- :e shows a file in place of the one in the pane, as Vim's does (0723ac7)
- go to definition, declaration, type and implementation (0ec84c0)
- offer to install a file's language and its language server (612d624)
- **term:** resizing rewraps a line that started in scrollback whole (8e0e924)
- a jump list across files: Ctrl+O back from gd, Ctrl+I forward (1c01b77)
- **gui:** flags and skin tones the grid splits across cells are drawn as one emoji (05229c7)
- **gui:** an ime option, keys and commits of one keypress typed once, and composing in the bar (a3f1d7f)
- hover in a panel by the cursor, and rename (08e2030)
- the finder: a project's files, grep, buffers and places (8888298)
- grr lists where what's under the cursor is used (b67e079)
- <Space>ds and :Diagnostics list what's wrong in the file in the finder (52399f5)
- :Format, and formatonsave, formatting a file with its language server (b913047)
- %, { and }, and * and #, in editors and terminals' normal mode (12d6fb6)
- git signs beside an editor's lines, with hunks to go to, show, reset and stage (96abe6c)
- completion in insert mode, and a call's signature over the cursor (3d27c79)
- keys mapped to keys from the config, and a binding's first keys typed when the rest don't come (511a128)
- <Space>ds lists what's wrong in the whole project, files not open too (100925c)
- code actions, with gra, in a menu by the cursor (6cf2540)
- af, if, ac, ic, aa and ia, and ]f, [f, ]C and [C, from the syntax tree (dc74dbd)
- links in terminals, opened with Ctrl+click or gx (c7fff6e)
- Vim registers, macros and marks in editors (a89c48d)
- soft wrap in editors, with linebreak, breakindent and showbreak, and gj and gk by row (f39bfb7)
- macros record and replay through the desk (f798608)
- :s with ranges, flags and a live preview, :g and :v, :m :t :d :y :normal, ~ g~ gu gU g? case changes, and :N to a line (9316dcd)
- indenting by the syntax tree: Enter, o and O, closing brackets, and = (9257001)
- snippets in completion: Tab and Shift+Tab between places, typing over placeholders, mirrors, choices and variables (05c5563)
- OSC 52, so programs in terminals can use the clipboard (cf83385)
- shell integration for zsh (597c535)
- file explorer panes, with git marks, live updates and file operations (8b83bba)
- agent edits land in the daemon's buffers, as the agent's own, never mixed into unsaved edits (83003dd)
- agents by name: Codex and Gemini beside Claude Code, installed by clackatoo (667cb52)
- clackatoo as an MCP server for agents (c3e26e2)
- undo an agent's turn, keeping your edits, and redo it (bf086ba)
- buffers follow their files on disk: agents' changes as the agent's, the rest as Vim's autoread (bbdd9b7)
- chat folds each turn's work under "Worked for", and its text can be selected and copied (eaa80f1)
- versions, signed releases and clackatoo update, with a notice in the window (ea6afbd)
- **update:** builds trust the release key, B60AA3EC05C4D6F1 (8b35fa2)

### Fixes

- keep lazy scrollback under its cap after a resize, and tidy up on close (f9b2a9c)
- survive GPU and daemon trouble when opening a window (d7371e3)
- draw with the GPU that drives the screen, not the fastest one (0f1ef4b)
- draw the window in an 8-bit format, not a floating-point one (e8ce091)
- the OpenGL fallback draws on Wayland, and WGPU_BACKEND chooses the API (59a3c18)
- switching workspace keeps normal mode and each pane's state, with the window's workspace logic in a tested Desk (b495237)
- greet a remote daemon before asking it to carry QUIC, so an older one can still say its version (2bf587d)
- check the next build accepts the daemon's own arguments before upgrading, and restore history without a memory peak (6f24a0c)
- hand back the memory history held once a program clears it (b1bfc13)
- modifier keys on their own don't count as the next key, so Ctrl+Shift+C copies a mouse selection (6b3d1bb)
- **term:** copying keeps blanks that more text on the line follows (884a59a)
- **gui:** the bottom bar's corner follows a rounded window's, and an empty tab counts no panes (63ced21)
- **gui:** Ctrl+; and its kin go to normal mode from anywhere: a visual selection, a search or the command line (264042e)
- **gui:** a click cancels Ctrl+B, taking the pane numbers with it (ce97fff)
- **daemon:** new panes start where the program in front is, as a nested shell, not only the pane's own shell (e9fe4b8)
- **daemon:** a chat says when its agent stops while it's idle (a46c707)
- **gui:** an empty chat's hint is laid out once, not every frame (eea0ef2)
- **gui:** a chat falls back to the terminal's font and emoji for what the interface font hasn't got (7eb45a5)
- **gui:** a menu over a card lights up the choice under the mouse (3190e3d)
- settings in client/startup.toml, and nothing stale after a cancel or reload (5929a2a)
- **gui:** the input method's late text is dropped once it's off, and every keypress sent twice pairs up (048420a)
- **gui:** flags pair from the left whether or not they join, across wrapped rows too (321eb72)
- **syntax:** an inner capture keeps its colour when a highlight's range cuts it and the capture it's in to the same text (195960e)
- files with CRLF line ends are edited without ^M and written with CRLF again (4b2e991)
- **update:** a release's signature must name the release, or it isn't installed (624f755)

### Speed

- **term:** print text in runs, index big palettes, store graphemes inline (32cadff)
- build releases with fat LTO and one codegen unit (e6c02b0)
- **term:** pass ASCII runs as bytes, decode known-valid UTF-8 directly (b2d42f3)
- **term:** parse ordinary CSI sequences in one pass (1354c59)
- **term:** allocate the alternate screen on first use (ecbe4bc)
- **term:** keep grapheme entries plain data (b123ed1)
- **term:** look BMP character widths up in a 16 KiB table (3cfd645)
- **term:** validate UTF-8 text with simdutf8 (619f3df)
- **term:** decode text through str, cover emoji in the width table (45437d6)
- **term:** store scrollback as compact text records (7cbfb4f)
- **term:** compact scrollback lazily, once output pauses (3862180)
- measure the window, and open and draw it faster (5f7f629)
- **term:** screen rows allocate their cells on first write (64e99ce)
