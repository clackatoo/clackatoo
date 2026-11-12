---
title: An Agent's Settings
type: reference
description: The model, effort and permissions controls, usage and limits, moving a chat to another agent, Gemini through Antigravity, and choosing and configuring agents in agents.toml.
related:
  - clackatoo/reference/agents
---

Each chat has its own model, reasoning effort, and approval mode, picked from
the controls in its message box, and talks to one agent at a time, which you
can change.

## The agent's settings

The rounded message box has its model and reasoning effort at the lower left,
followed by one permissions control showing the current approval mode. At the
right, a small context gauge sits beside Send, or Stop while the agent
answers. Paste images with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>.

The permissions menu groups three separate settings: the agent's approval
mode, access to clackatoo's tools, and browser access. Changing one leaves the
others as they were. For Claude Code, Manual asks before every change, Accept
edits makes edits without asking, Plan plans before it acts, and Auto lets
Claude decide what needs asking about. A new Claude chat starts in Auto.

The circular gauge shows context used as a percentage in its center. Its ring
fills clockwise: quiet while there is room, amber past 80%, red past 95%. A
Claude account limit above those thresholds also raises its warning color; the
details identify which limit needs attention. When the context capacity is
unknown, the gauge shows a dash.

Hover over the gauge for the conversation's token counts and any account
limits reported during that chat. Select it, or press `K` in normal mode, to
open **Usage & limits**. The modal separates conversation context from account
allowances and lists the installed providers, including those whose adapters
cannot report limits. <kbd>Escape</kbd>, <kbd>Ctrl</kbd>+<kbd>;</kbd>, the
close button, or selecting outside it dismisses it without changing the draft.
Scroll or use the arrow and page keys for more windows.

Claude Code, Codex, and Antigravity can read their subscription windows
without sending a model prompt: short and weekly limits, plus model-specific
or extra-spend allowances when reported. Each allowance has a bordered card
with a prominent percentage left and a provider-colored bar. The hatched part
is the allowance already used, and the badge inside the bar counts down to
reset. Wide windows place the figure beside the bar; narrow panes stack them.
Update times beside provider names use the window's local timezone. API-key
accounts and adapters without this interface say when subscription limits are
unavailable. Custom launch commands are not probed through another
installation's sign-in.

The daemon collects these readings, so windows connected remotely see the
accounts on that daemon's machine. It caches readings after the first
completed message, then refreshes every three completed messages across chats
(or on the next completion after five minutes). Opening the modal shows cached
values immediately and requests a refresh, as does **Refresh**. All refreshes
have a one-minute minimum interval and concurrent windows share the work;
nothing polls while chats are idle. New chats receive the same cached
readings. Failed reads retain the last successful values and their update
time; expired windows say they need a refresh rather than guessing that usage
is zero. Readings are collected again after a daemon upgrade.

Select a setting to open its menu, immediately above that button.
<kbd>Up</kbd> and <kbd>Down</kbd> move through it, <kbd>Enter</kbd> or the
mouse picks, and <kbd>Esc</kbd> or selecting elsewhere closes it.
<kbd>Shift</kbd>+<kbd>Tab</kbd> steps through the modes without opening the
menu.

The model and effort share a searchable menu. Type to filter by model,
provider or description, including pinned versions hidden inside families.
<kbd>Up</kbd> and <kbd>Down</kbd> move through results, <kbd>Enter</kbd>
picks, and <kbd>Esc</kbd> closes it without changing your draft. The mouse
wheel scrolls the picker independently of the conversation.

The picker opens on favorites if you have any, otherwise the current provider.
The left rail switches between favorites and individual providers. Search
always covers every provider, placing matching favorites first; clearing the
search returns to the previous page. The popup keeps a fixed height, with the
provider rail and model list scrolling independently. Select a star beside a
model to add or remove a favorite; favorites are saved on this machine and
shared across its windows. Each row shows the provider below the model and a
tick on the current model.

The menu includes:

- **Models by family:** Opus, Fable, Sonnet and Haiku, each with the version
  it picks. Picking a family picks its newest version, and keeps up as new
  ones come out.
- **A family's versions:** the `▸` button at the end of its row, or
  <kbd>Right</kbd>, shows its other versions, to pin the chat to one.
  <kbd>Left</kbd> hides them. A chat on a pinned version opens the menu with
  that version shown and ticked.
- **Fast mode:** the lightning bolt in the rail beside reasoning effort
  toggles fast mode. It appears only when the current model advertises
  support, and fills in while enabled. The adapter controls its availability
  and account restrictions.
- **Named effort choices**, from Low to Max: select a choice, or with the
  effort row selected, press <kbd>Left</kbd> and <kbd>Right</kbd>. The menu
  stays open as you change it. Default leaves effort to Claude Code, which
  picks it for each model.

Models from another agent are grouped only when one choice in a family always
means its newest version, as Claude Code's `opus` does. Otherwise they're
listed one per row.

A chat keeps the settings you pick. If the agent stops, or the daemon is
[upgraded](~clackatoo/reference/cli#upgrade), the chat asks for them again as
its session reopens.


### Moving a chat to another agent

The model menu lists every agent's models, with its provider below each model,
after the chat's own agent's. Picking another of the same agent's models
changes it in the chat. Picking another provider stages it with your draft. A
ruled `Context handoff pending` row shows the source and destination with
their provider icons, and the model button shows your pending choice. Nothing
switches until you send a message or image. Choose a different destination to
replace it, or select × on the row or pick the current provider to cancel it.
Sending switches providers and submits the message together; a refused handoff
restores your draft instead of sending to the old provider. The completed
handoff stays in the history as a matching divider. An agent clackatoo hasn't
installed says "install to use", and sending its pending handoff offers to
install it. An agent whose models aren't known yet is asked for them as the
menu opens, which takes a moment.

The new agent hears the conversation with your next message, before it, fenced
off as context carried over rather than as something to do: the newest turns
whole, older ones a line each (what you asked, what it ran and changed, how it
ended), the files changed, and the previous agent's plan if it has steps left,
to ask you about rather than carry on with. Messages other agents sent the
chat are marked as theirs, not yours. The `carryover` option sets about the
most it takes, in tokens: 20000 unless set, never more than 50000 nor a
quarter of the new agent's context, and never less than 1000. Moving back to
an agent the chat had before opens its own session again, and it hears only
what happened since.

Undoing turns, reviewing changes, boards, clackatoo's tools and the
`claudetask` choice stay the chat's, whichever agent answered. The mode and
effort carry over where the new agent has one by the same name; where it
hasn't, it uses its own, and a chat an agent started gets the new agent's
safest mode; the line across the chat says which. The new agent doesn't see
the old one's thinking or the whole output of its tools, only what the chat
shows, and its first answer costs more, as nothing of the conversation is
cached for it yet. You can stage a handoff while the agent is answering, but
must wait until it finishes or stop it before sending. Pending choices stay in
this window with the draft; closing the window discards them. Turn numbers, as
in `:agentundo 3`, skip the lines where the chat moved.

## Gemini through Antigravity

Select **Antigravity** in the model menu's provider rail, or open
`:Agent antigravity`. clackatoo installs Google's official runtime for the
Agent Client Protocol (ACP), from a pinned release checked against its
SHA-256, for x86_64 or ARM64 Linux. It needs no Node.js. Allow about 1.1 GB of
disk for it, and another 1 GB for the scratch files it makes as it runs.

To sign in, run this in a terminal on the chat's machine:

```sh
clackatoo agent login antigravity
```

It prints a Google link to open. On the same machine, the browser hands back
to the runtime once you've signed in, and that's all. From a laptop showing
the desktop's workspaces (see
[Another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar)),
run the command in a terminal in one of the desktop's workspaces, and open the
link on the laptop. If the browser ends on a `127.0.0.1` page it can't reach,
copy the whole address from its address bar, paste it into that terminal, and
press <kbd>Enter</kbd>. What you paste isn't shown.
<kbd>Ctrl</kbd>+<kbd>C</kbd>, <kbd>Ctrl</kbd>+<kbd>D</kbd>, or `cancel` and
<kbd>Enter</kbd> stops signing in, and it gives up by itself after 10 minutes.
One sign-in runs at a time: a second says another is running. The address the
browser ends on is taken once, and only for that sign-in, and it never goes
into a chat's history.

This sign-in is apart from the Antigravity IDE's. Its credentials stay on the
chat's machine, in the private `antigravity-specialist` directory beside the
installed agents, and the runtime's scratch files go there too, not in `/tmp`.
clackatoo doesn't take a sign-in from the IDE, nor silently use an API key
from the environment.

Once you've signed in, send a message or open the model menu again: its models
and settings are the ones your Google account has, not a fixed list. Images,
the queue, [moving a chat to another agent](#moving-a-chat-to-another-agent),
and reopening a session work as they do with other agents. The permissions
menu lists Antigravity's approval modes by the names its runtime gives them,
and a new chat starts in its default mode. A turn under way is steered with
**Stop & continue** (see
[Queuing and steering](~clackatoo/reference/chatting#queuing-and-steering)).

Select the context gauge, and **Usage & limits** shows Antigravity's
allowances beside your other agents': Gemini, Gemini 4 Argon, and
`Claude & GPT (shared)`, one allowance that Claude and GPT models both count
against, when Google reports them. They're kept and refreshed in the
background as the others are, with the same countdowns to reset, and reading
them sends the model no prompt. An allowance you haven't used has no countdown
until Google reports when it resets.

Antigravity uses its own file tools, the one that views images included, so
save what you've changed in an editor before asking it to read the file. Its
changes to files, the questions it asks before using a tool, what its commands
print, and its images show in the chat as other agents' do, and an image opens
in the same [viewer](~clackatoo/reference/chatting#viewing-images).

The separate **Gemini CLI** agent is for setups that already use an API key.

## Which agent

A chat talks to Claude Code unless you name another: `claude`, `codex`
(OpenAI's Codex), `antigravity` (Google Antigravity), or `gemini` (Gemini CLI
for existing API-key setups). The chat says which in its box: "Message Codex".

clackatoo speaks ACP, the
[Agent Client Protocol](https://agentclientprotocol.com). Claude Code and
Codex don't speak it themselves, so each runs through an ACP adapter, at a
version tried with clackatoo. The daemon looks for an agent's adapter where
clackatoo installs it, `~/.local/share/clackatoo/agents`, then on your `PATH`,
then runs it with `npx`, which needs Node.js. While it runs from `PATH` or
`npx`, the chat offers once to install it, which makes it start faster and
work offline: Claude Code's takes about 620 MB and Codex's about 450 MB, each
with its own copy of the agent, installed from a lock file so nothing it
depends on can change. With none of the three, the chat asks to install it
before it can start. A new build that moves to a newer adapter asks again, and
installing it removes the older ones nothing is running from. A build that
only patches Codex's adapter anew, as `2.2.2+clackatoo.2` does
`2.2.2+clackatoo.1`, asks nothing: the chat says it's installing while the
daemon prepares the new one from the one installed, fetching nothing, and
offers the usual install only if that one isn't the bundle pinned. With
provider-managed MCP servers off, as they are by default, Codex never runs from
`PATH` or `npx`, whose adapter can't keep them out: the chat offers to install
it instead. An agent, and
every command it runs, gets only the absolute directories of the daemon's
`PATH`: an empty or relative one, such as `.`, `bin` or `node_modules/.bin`,
or a `~/bin` written literally, would find a program in whatever folder it
runs in, which might be a project anyone could have filled. Write them out
whole (`$HOME/bin`, `/path/to/project/node_modules/.bin`), or call such a
program by its path. With no absolute directory at all, it's
`/usr/local/bin:/usr/bin:/bin`.

Each agent uses its own sign-in: run `claude`, `codex login` or `gemini` once
in a terminal, or for Antigravity, [its own](#gemini-through-antigravity). A
chat with an agent no one's signed in to says what to run. With a
subscription, chats count toward its limits.

If an agent hasn't opened a chat's session after 30 seconds, the chat says so,
and what it might be waiting on: most often a Model Context Protocol (MCP)
server in the agent's config that needs you to sign in. For Codex that's
`~/.codex/config.toml` (or `$CODEX_HOME`), and the chat names the servers
there reached over the network; for Claude Code, its settings in
`~/.claude.json` or the project's `.mcp.json`; for Gemini CLI,
`~/.gemini/settings.json`. `codex mcp list`, `claude mcp list`, or
`gemini mcp list` in a terminal shows which can't start. An agent that hasn't
answered at all is said not to have started; one run through `npx` is given
two minutes, as npx might be fetching it. The chat goes on waiting, and asks
whether to **Keep waiting**, which <kbd>Esc</kbd> picks, **Retry**, which
stops the agent and starts it again, or **Stop**; <kbd>Ctrl</kbd>+<kbd>C</kbd>
stops it too. A session that comes after you stop is closed unseen, and the
next message asks again. While another chat is in the middle of a turn with
the same agent, Retry only asks again rather than starting it again, which
would cut that turn off. The model menu says the same of an agent that doesn't
say what models it has within 20 seconds.

An agent that stops listening, say because its input broke, is still read for
a moment for the rest of what it says, but nothing more is sent to it: a
message to it ends at once, saying the agent stopped, and so does stopping the
chat or changing a setting. A chat that tries to start it meanwhile waits,
saying it's stopping, and starts it again once it has gone, sending any
message you typed.

A question clackatoo asks itself, such as this one or an offer to install,
shows as it's worded; anything an agent asks is always shown as the agent's,
"Allow the agent to …?", however it's worded.

Codex offers its own settings in the box: its mode in the permissions menu,
from Read-only, which asks before every change, through Workspace access and
Auto review to Full access, and its model and reasoning effort at the lower
left. A new Codex chat starts in the mode Codex's own config gives.

Codex's Plan mode is in the permissions menu too, under its mode, as Default
or Plan, and its `/plan` turns it on as in a terminal. While it's on, the
control under the box says so first, as `Plan · Auto review`, and Codex plans
rather than changes anything. When it has a plan, it asks to implement it,
with the plan
[above the question](~clackatoo/reference/chat-view#questions-forms-and-plans);
saying yes leaves Plan mode and Codex carries on with the plan in the same
turn, and a session opened again later stays out of Plan mode. Saying no keeps
it planning. The phone marks the box `Plan` while it's on, and lists it under
Permissions. Windows and phones from before this release don't show it at all;
nothing else changes for them.

A setting an agent changes by itself, such as its model or Codex's Plan mode,
is kept for its next session too. Its mode isn't, unless it can do less: an
agent that moves itself to a mode that can do more than the one you chose is
put back at once, and the line above the box says so, as
`Codex switched itself to Full access; put back to Read-only`. clackatoo knows
how much Claude Code's and Codex's modes let them do (Claude Code's Plan,
Manual, Accept edits, Auto, then Bypass permissions; Codex's Read-only,
Workspace access, Auto review, then Full access), whatever order they list
them in; any other agent is always put back to the mode you chose. Claude Code
leaving Plan mode is kept only for the mode you picked in its plan's question,
or with none picked, for one that can do no more than the mode you had before
Plan. A chat an agent started never leaves Plan mode this way, so it never
goes above the mode it started in; with no mode of its parent's to start in,
it starts in its agent's safest that can still act (Manual, Read-only), or for
any other agent, in that agent's own.

Every agent is asked for ACP's notices, so what Codex warns of, such as a
problem in its config, and that it moved to another model, shows as a notice
above the box rather than as words in the turn. Where Codex compacts its
context, the turn has the
[line](~clackatoo/reference/chat-view#what-else-a-turn-says) that says so.

To add an agent that speaks ACP, or run a known one another way, set it in the
[daemon's config](~clackatoo/reference/config#the-daemon-s-config),
`~/.config/clackatoo/daemon/init.lua`:

```lua
daemon.agents.opencode = {
  name = "OpenCode",             -- what the chat shows; the name, if left out
  command = { "opencode", "acp" },
  mode = "build",                -- the mode a new chat asks for, if offered
}

daemon.agents.claude = {
  command = { "claude-agent-acp" },
  tasks = false,                 -- don't ask what it runs in the background
}

daemon.upgrade.keep_agents = false -- stop agents as the daemon upgrades
daemon.subagents.working = 16      -- see Subagent limits
```

An agent has `name`, `command`, `mode` and `tasks`, and needs its `command`, a
list of strings. Anything else, or a field of the wrong kind, is a mistake,
which stops the config at the line that set the agent: the daemon then keeps
what it last ran, and says so in a window's bar. The daemon runs its config
again as you save it, so a new chat you start can start with a change at
once, while chats already going keep the agent they have; a change made
outside clackatoo is said in every window's bar (see
[Changes made outside clackatoo](~clackatoo/reference/config#changes-made-outside-clackatoo-are-said)).
A known
agent's name, such as `claude`, runs it with your command, keeping its title
and what clackatoo knows of it. `CLACKATOO_AGENT` gives Claude Code another
command, for the daemon it's set for.

Before, agents were set in `daemon/agents.toml`, which the daemon moved into
`daemon/settings.lua` the first time it started with the Luau config.

Each agent runs one process for all its chats, and stops when its last chat
closes. Claude Code's adapter also runs a Claude Code process per chat, of
about 250 MB.
