---
title: What an Agent Can Use
type: reference
description: clackatoo's tools for agents, for showing you terminals and files, the browser, native window access, managed activities, and inspecting a chat's capabilities.
related:
  - clackatoo/reference/agents
---

clackatoo offers agents tools of its own: to show you things in the window, to
browse, to see and work the window when you grant it, and to run long jobs
that survive an upgrade.

## What the agent can show you

Each chat's agent is given tools of clackatoo's own, so it can show you things
in the window instead of only telling you. An agent asked to run your tests,
say, can run them where you watch.

| Tool | Does |
|---|---|
| `open_file` | Shows a file in an editor beside the chat, at a line if it gives one. The next file it opens takes that pane's place. Your typing stays where it is. In a folder you haven't [trusted](~clackatoo/reference/languages#trusted-folders), the file has no language server, and the agent is told so; only you can trust the folder. |
| `run_in_terminal` | Opens a terminal beside the chat, running your own shell, and types a command at its first prompt. You can watch it, and type into it yourself. |
| `run_in_background` | Does the same in a new [background tab](~clackatoo/reference/workspaces#background-tabs) of the chat's workspace, out of your tab bar, for what runs on by itself, as a dev server. Nothing moves your focus; it shows in your list of background tabs. |
| `read_terminal` | Reads a terminal: its last command, whether it's still running or how it exited, and the end of its output: the last 50 lines, unless it asks for up to 500. |
| `wait_for_terminal` | Waits until the command in a terminal finishes, for a minute unless it asks for longer, up to ten. |
| `send_to_terminal` | Types into a terminal, as you would: an answer to a prompt, or another command. |
| `explore` | Opens a file explorer beside the chat, at a folder if it gives one. The next it opens shows its folder in that explorer instead. |
| `list_panes`, `show_pane` | List the panes in the chat's workspace, its background tabs' too, and point one out: its border flashes, or its tab is marked `●`. A background tab's pane isn't pointed out. Nothing an agent does moves your typing to another pane or tab. |
| `get_diagnostics` | What the language servers say is wrong in the chat's project, as the editor shows it. |

The first pane an agent opens goes to its chat's right, and the next ones
stack below that. An agent's tools reach only its chat's workspace.

Agent-created terminals have the same compact teal band as subagent panes,
aligned when they sit beside each other. It names the chat that started the
terminal and says whether an agent is actively waiting for it. Select the
band, or press <kbd>Ctrl</kbd>+<kbd>B</kbd> `u`, to return to that chat.

A terminal that has **never been selected** closes automatically 60 seconds
after its command finishes. Its band counts down the remaining seconds, such
as `closes in 59s`, using the daemon's closing deadline. Selecting it once
keeps it open permanently, even after you move elsewhere; marking it to keep
running does too. This applies to agent-created background terminals as well.
A new command resets the timer. If the shell cannot report completion, the
terminal stays open. Selection and remaining time survive a daemon upgrade;
terminals carried over from older builds stay open because their selection
history is unknown.

The marks that say a command finished aren't signed, so a program can print
them in what it outputs, as a log line or a page it fetched could. A finish is
taken, for `wait_for_terminal`, an activity's result and the closing timer
alike, only once the shell has the terminal to itself again, as when
[keeping a terminal running](~clackatoo/reference/workspaces#keeping-a-terminal-running):
its process group in front, and nothing running in it. So a command still
running is never closed under it, nor waited for as done, and `read_terminal`
says it's still running, not how the marks say it exited. A command that
resets the terminal (`reset`, or `ESC c`) as it runs takes every mark with it;
it's still waited for, until the shell has the terminal back. A finish is seen
within a moment of the shell being back, even after output that faked one. The
exit status is still the one the marks gave: output that marked its command
finished with status 0 can make a later real failure read as success once it
truly ends.

The same holds for a shell inside another, at the far end of `ssh` or
`docker exec`: its commands are marked, but the terminal's own shell has it
back only once ssh ends. So `wait_for_terminal` on a command run in that inner
shell ends when its time is up, not when the command does; `read_terminal`
shows how it's getting on meanwhile.

No tool marks a terminal to
[keep running](~clackatoo/reference/workspaces#keeping-a-terminal-running),
and `run_in_terminal` and `run_in_background` refuse a command line that runs
`clackatoo keep` or `clackatoo bg`. That's a guard against mistakes, not a
boundary: whatever an agent types into a terminal, or a script it runs there,
follows the rules below, and could do what you could at that shell.

Before running a command or typing into a terminal, the agent asks: Allow,
Always allow commands, or Reject. The question shows exactly what would be
typed, with keys such as <kbd>Enter</kbd> shown as `⏎` and nothing cut short;
long text goes in the conversation, in full. Where the command would run shows
as code too, as the folder it is, and a folder that isn't there is refused
without asking. Always allow commands sets the chat's Tools to Allow, below:
the agent runs commands, and types into terminals it started, without asking.
Typing into a terminal you opened always asks, and so does showing a file or
folder outside the chat's project; as Tools has no say in those, their
questions offer only Allow, for this once, and Reject. The tools that only
show or read inside the project, and those that start and message chats, never
ask.

The chat's box has a Tools setting, Ask, Allow or Off, that you can change at
any time. Off refuses the agent's calls at once; as the agent is given no
tools at all while it's Off, turning them back on reaches it only when its
session next opens. The `agenttools` option sets it for new chats, `ask` to
begin with, and `CLACKATOO_AGENT_TOOLS=off` for the daemon gives no agent any
tools. While the agent's mode changes nothing, as Claude Code's Plan or
Codex's Read-only, the tools that run or type commands refuse, whatever Tools
says, as they do for a chat with
[limits](~clackatoo/reference/chats-together#profiles): the mode's promise
holds for clackatoo's tools too. Turning Tools off, or picking such a mode,
takes back the questions about clackatoo's tools still waiting that it now
rules out, refusing those calls, and an Allow is checked against the chat's
settings again as you give it.

Waiting for a command relies on the shell marking where each one ends, as
bash, zsh and fish do in clackatoo; such a shell gets up to 30 seconds to show
its first prompt before the agent is told it didn't. In other shells the agent
gets the terminal's last lines, and is told it can't know when a command
finishes.

A command is typed only while the shell has the terminal to itself, as for a
terminal
[kept running](~clackatoo/reference/workspaces#keeping-a-terminal-running): a
`$SHELL` that starts another shell rather than becoming it, as `sh -c fish`
does, never has, so the agent is told its command wasn't typed, and why; set
the daemon's `$SHELL` to the real shell.

A command typed while the shell is still drawing its next prompt is waited for
from that prompt on, and a command that clears the screen and scrollback, as
`clear` does, doesn't end the wait.

The tools are an MCP server (the
[Model Context Protocol](https://modelcontextprotocol.io)), which the agent
starts as `clackatoo mcp`, for Claude Code and Codex alike. It talks to the
daemon for the one chat it was started for. Each time the chat's session
opens, the daemon gives that server a new secret, in its environment rather
than its arguments, and serves the chat's tools only to a server showing one:
`clackatoo mcp` run by hand, or by anything the agent wasn't told to start, is
refused. A secret is the session's, so only that session's server counts as
clackatoo's. Secrets last through an
[upgrade](~clackatoo/reference/agents-lasting#through-an-upgrade), and the
daemon remembers which session each went with, but a fresh start of the daemon
makes every one given before it no good. Two costs come once, after upgrading
from a release before secrets: an agent carried through the upgrade that
starts its tools' server again, which most never do, is refused until its
chat's next session opens; and a server it connected before the upgrade, which
has no secret, counts as clackatoo's for every session of its chat until it
closes. The same server has the tools for [the browser](#the-browser),
[chats working together](~clackatoo/reference/chats-together#chats-working-together),
[native window access](#native-window-access),
[managed activities](#managed-activities-and-recovery),
[boards](~clackatoo/reference/boards) and `harness_capabilities`
([below](#inspecting-capabilities)).

## The browser

A chat's agent can drive a web page once you turn Browser on in the
permissions menu beside the model, or for every new chat with
`:set chatbrowser`. It's off unless you do. Enabling it works during an
existing conversation without starting a new chat; disabling it closes the
page and blocks further browser calls immediately.

When the daemon supports browsing, the tools are advertised from the start,
even while Browser is off. Agents that cache their tool lists can therefore
use them once you enable Browser. Every call checks the current setting;
seeing the tools does not grant access.

| Tool | Does |
|---|---|
| `browser_navigate`, `browser_back` | Open a page, or go back. http, https, and `data:` addresses; never files. |
| `browser_snapshot` | Reads the page as an outline of headings, links, buttons and text boxes, each with a ref. |
| `browser_click`, `browser_type`, `browser_press_key` | Click or type into a thing by its ref, or press a key. |
| `browser_screenshot`, `browser_read` | A picture of the page, or its text. |

Before its first page, the chat asks: Allow for this chat, or Reject. The
browser is clackatoo's own headless Chromium, with a profile of its own, never
yours, so nothing is signed in, and each chat's page is kept apart from the
others'. Downloads are turned off. A chat whose
[limits](~clackatoo/reference/chats-together#profiles) are read-only or no
commands, as `explore` and `plan` are, or whose agent's mode is Plan or
Read-only, reads pages but clicks, types and presses keys in none, and opens
no `data:` page. That's not a sandbox: any page runs its own scripts, so an
agent that can open a page whose content it controls, as its own server's, a
service that renders what it's sent, or a dev server on `localhost` serving
files from the project, can run scripts of its choosing there, and those
scripts, or the page's address alone, can reach services on your machine and
network the browser can. Leave Browser off for a chat that must not do that.
What the agent reads of a page reaches it marked as the page's, not as
anything you asked.

Turning Browser or Tools off, stopping the chat, or closing it stops what its
agent asked the browser to do: what's waiting is never done, the click, typing
or load under way stops at its next step, and the agent is told why at once. A
chat can have 4 browser calls under way at a time; one browser serves every
chat, so more would hold up other chats' pages.

A page's alerts, confirms, and prompts are turned down as they open. The agent
hears of the first 8 since it last used the browser, and how many more there
were. A page's address and title are cut to 2 KiB wherever they're kept, shown
or sent, as a page can make its address megabytes long, many times a second.

A snap of Chromium can only use a profile in its own folder under `~/snap`,
and anywhere else it would use yours: clackatoo puts its profile there, checks
once the browser has started that it's using it, and stops it at once, and
never starts it again, if it isn't.

The first page opens a pane beside the chat showing the page as it goes, with
its address and what the agent did last. `:Browser` opens the focused chat's
page in a tab, `:|Browser` beside it, and `q` closes the pane. Pictures come
only while a window shows the pane, a few a second.

`CLACKATOO_BROWSER` for the daemon names the browser, or `off` for none;
`CLACKATOO_BROWSER_PROFILE` names its profile folder.

## Native window access

You can let an agent see the window its chat is in, and work it as you do.
Native access is **Off** until you grant it. Focus the chat in the window you
want to share, then type `:agentaccess inspect` to let the agent take
screenshots and see the window's controls, or `:agentaccess control` to let it
also use the mouse, type, and press keys anywhere in that window. That's a lot
to let it do: it sees all the window can show, and can do all the window can.

While a chat holds access, a teal badge at the right of the top bar says which
chat and how much, as `Agent Control · chat 3`. clackatoo draws it where no
program can draw, so a program's title can't pass for it; the window's own
title says nothing of the grant. Select the badge to go to the chat, and its
gold `×` to end the access, or type `:agentaccess off` in any pane. Closing
the window or the chat, disconnecting, or an upgrade ends it too.

**Control lets the agent act as you.** It can edit and save any file the
window can, its config included: your shell's startup files, Claude Code's or
Codex's settings and hooks, `agents.toml`, and `init.lua`. What those run
later runs as you, unasked. Grant it only to an agent you'd let use your
keyboard, and end it when the agent is done. `:agentaccess control` says so as
it asks.

The window has speed bumps on top, not a boundary. While it handles the
agent's input, it refuses what only you can do: answering a question or a
form; changing any chat's settings, agent, or provider, or forking one;
granting window access or using its badge; typing into a terminal; starting a
terminal with a command, a background command, or a chat; sending, drafting,
stopping, undoing, or rewinding in another chat; keeping, rejecting, or
commenting on agents' changes in a review, or sending its comments; posting on
a board; closing panes, tabs, workspaces, terminals, or background tabs;
pairing or revoking devices, unlocking pairing, `:keep`,
[trusting a folder](~clackatoo/reference/languages#trusted-folders),
connecting to or forgetting other machines, forwarding ports, installing,
upgrading, `:update`, and `:reload`. While a chat has control, the config's
Lua runs no commands, makes or closes no panes, and connects to nothing,
whether it acts by itself or for your keys. So `:reload`, run by you while a
chat has control, doesn't run the config's startup splits or commands either;
reload again once the access has ended. Keys the agent's input left to go on
later, as the rest of a macro waiting for the daemon to answer, are dropped as
each input ends. `:set` changes only this window, as it always does, so
control can change the window's options. None of this is an operating-system
sandbox, and an agent that runs commands without asking can reach the daemon
itself, outside the window.

A window is granted to one chat at a time, for inspect and control alike, and
a chat holds a grant on one window: granting it another moves the grant, and
the old window says so. A chat mirrored from another machine (see
[Another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar))
can't be granted one: open a window on that machine's daemon with `--remote`
instead. The web app can't grant native access. The agent's tools are
`native_snapshot`, and with control, `native_click`, `native_type`, and
`native_key`; a chat whose limits are read-only or no commands, or whose mode
is Plan or Read-only, gets `native_snapshot` alone. Each input uses up the
screenshot it was aimed by, so the agent takes a new `native_snapshot` before
every input. An input is refused too if, since the screenshot, you used the
window, it was resized or focus moved, something else in it changed, or the
control under a click moved. The tools never move the desktop's focus.

## Managed activities and recovery

For a long build, a test run, or a deploy, a job that ends by itself (not a
short command, nor a service that restarts itself), ask the agent to run it as
a managed activity. It gets a card in the chat, in the window and the web app
alike, that stays: `Starting`, `Running`, `Stopping`, and then how it ended,
as clackatoo saw it. Open the card for its command and the end of its output.
Stop sends it <kbd>Ctrl</kbd>+<kbd>C</kbd>, and the card says it's ended only
once the shell's integration says so. Its terminal is a
[background tab](~clackatoo/reference/workspaces#background-tabs), so
`:Background` lists it too.

clackatoo tells agents when to use its tools `start_activity`,
`read_activities`, and `cancel_activity`. Starting one asks leave as
`run_in_background` does, unless the chat's Tools setting is Allow, and is
refused when Tools is Off or the chat's limits are read-only. Each activity
has an ID that doesn't change, kept before its command runs: asked to start
the same ID, command, and folder again, the daemon answers with the one it
has, and the same ID with another command or folder is refused. When an
activity ends, its chat is told; a chat you stopped holds the notice until you
write there, then hears it after your message.

Activities, and what's kept of them, last through an upgrade. After a full
restart, one that hadn't ended says `Outcome unknown`, and never runs again by
itself; so does one cut off as it started. An agent must look at what's kept
of it, and at how things are now, before deciding what to do. This covers
managed activities only, not every agent's or other tool's calls.

Activities need the daemon to save its workspaces, and the shell's
integration. A chat can have 8 running at once, and the daemon 64; past those,
starting another is refused. Of those that ended, each chat keeps its newest
16, and the daemon 64 in all: an older one is let go as a new one starts,
oldest first, but never before its agent has been told how it ended. Its ID is
remembered, the last 1024 of each chat, so a job never starts twice: starting
it again is refused, and the agent is told to use a new ID. Those IDs aren't
kept through an upgrade or a restart, though the newest jobs are. A chat's
activities, and their IDs, go when the chat closes; what they ran goes on in
its background tab. What's kept of the result is the end of the terminal's
last 50 lines, up to 4 KiB, with how the command ended; the terminal keeps its
scrollback as any does.

## Inspecting capabilities

`:Capabilities` shows what the focused chat can use: its Tools setting, the
limits on its commands, whether the browser is set up and the chat can use it,
the window it's granted, its managed activities, its connection to clackatoo's
tools, and whether that connection asked for the list of them. In the web app,
the model menu's **Tools** tab shows the same. An agent can ask for it too,
with `harness_capabilities`. That the connection asked for the list of tools
says nothing of whether the model saw it, which clackatoo can't tell, so the
two are shown apart.
