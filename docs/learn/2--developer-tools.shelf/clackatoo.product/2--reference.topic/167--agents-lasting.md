---
title: What Lasts
type: reference
description: What survives closing the window, an upgrade, and a daemon restart for chats and their agents, and the known limits of chats.
related:
  - clackatoo/reference/agents
---

The daemon keeps chats as it keeps terminals, and their agents go on through
an upgrade.

## What lasts

The daemon keeps chats, as it keeps terminals. Closing the window closes
nothing: a chat keeps answering, and the next window shows it.

[Upgrading](~clackatoo/reference/cli#upgrade) the daemon keeps each chat's
conversation, and its agent goes on
[through the upgrade](#through-an-upgrade), with all it runs: an answer under
way goes on as if nothing had happened. Where the upgrade stops the agent
instead, an answer under way automatically continues in the same provider
session and visible turn after the upgrade, without another message or an open
window, and the agent is told to check interrupted tool results before
repeating actions. Messages still waiting to be sent, including images, remain
intact; queued messages follow the continued turn. Unconfirmed steering stays
paused for review. Idle, completed and manually stopped chats stay idle, as do
chats restored after a crash or ordinary daemon restart. The first upgrade
from a build without continuation records still needs another message to
restart interrupted work. A chat from a build before chats were kept as turns
comes through as one block of its text. Tools from a build before cards come
through as lines, without their output or changes. Changes from a build before
line numbers come through without them.

If the agent stops on its own, the chat says so, and the next message starts
it again, carrying on the same conversation.

A chat keeps which agent it talks to. Chats from before there were other
agents talk to Claude Code.

What each turn changed in files lasts through an upgrade, so turns can still
be undone, up to about 16 MB per chat. Past that, the oldest turns are too
long ago to undo.

A chat that moved between agents keeps the sessions it had with each, and goes
back to them after an upgrade too.

When the daemon stops, a chat in a pane comes back with its workspace the next
time it starts, on the same session: no agent starts until you look at the
chat or send it a message, and then the agent tells the conversation so far,
as going on with a
[past session](~clackatoo/reference/chatting#the-start-page) does, and nothing
it tells acts again. A turn you stopped shows as stopped, as Claude Code keeps
it; Codex keeps no note of a stop, so a stopped Codex turn shows as one that
ended. Claude Code keeps a stop as a message reading
`[Request interrupted by user]`, so a message of exactly that, typed yourself,
comes back as a stop too. The chat keeps its agent, folder, model, Tools,
Browser, and Subagents settings (the last no more than `working`) and whether
it can use Claude Code's own Task tool, and starts in the agent's own starting
mode. Chats in no pane don't come back, but the start page lists their
sessions.

A [subagent](~clackatoo/reference/chats-together#subagents) in a pane comes
back a subagent of its chat, if that chat is in a pane of the same workspace
and so comes back with it: with its band, on its chat's card, under the turn
that started it, and with its profile and that profile's limits. `:q` sends it
back to the card, its chat can message it and wait for it, and one you'd sent
back and opened again is still yours to close. It never comes back able to do
more than it could: it keeps its limits (ones this build doesn't know count as
read-only), never fewer than its chat's, it can start no deeper chats than it
could, and it asks its agent for the mode it had chosen only if that's no
higher than its chat's: otherwise for the safest, Manual or Read-only, and
never the mode your own new chats start in. An agent whose modes clackatoo
doesn't know is asked for none. A subagent whose chat didn't come back,
because it was in no pane, or is in another workspace, comes back on its own,
with its profile and limits, in a mode no higher than its agent's safest, and
says so. Its Subagents setting is never more than its chat's. Some things
start again: how many chats agents have started under each of yours (up to
128), how many turns notices have started (100), messages from other agents
still waiting their turn, and waits under way, which end. What its chat had
already heard from it isn't told again: the conversation it tells again counts
as no answer to anyone. One kept by a build from before then comes back as a
chat of its own.

`clackatoo capture` prints a chat as text: each message after a `>`, and what
the agent said.

What you've typed but not sent in a chat's box, and the
[messages sent](~clackatoo/reference/chatting#messages-sent-and-drafts)
<kbd>Up</kbd> recalls, last through an upgrade.

A line where the agent compacted its context comes through an upgrade as words
saying so; a command's exit code, what turns spent and the agent's own words
for a failure don't.

The agent's [plan](~clackatoo/reference/chat-view#the-agent-s-plan) lasts
through an upgrade. After the daemon stops, a chat going on with its session
shows the plan again once the agent has told the conversation so far, if the
agent tells its plans as it does: Claude Code does.

An upgrade stops the browser; the next page starts it again. The daemon
stopping ends what agents run in the background, as it ends the agents.

A pane showing
[a command's output](~clackatoo/reference/chat-view#a-command-s-output-in-a-terminal)
keeps what it shows through an upgrade, but nothing more reaches it; a restart
closes it.

### Through an upgrade

An agent's process goes on through an upgrade, rather than being stopped: the
new build takes over its pipes as it takes over terminals, and the agent never
knows. Everything it runs goes on with it, the same processes: shells it left
in the background, a dev server, a build, and a command it's waiting for in
its turn, such as a deploy that upgrades the daemon itself. A turn under way
goes on, its words coming into the same turn, which ends as it would have; no
message is sent to carry it on. Its background tasks stay running. A question
asking leave is still asked, and your answer reaches it. A
[form](~clackatoo/reference/chat-view#questions-forms-and-plans) can't be
carried: it's canceled, the chat says so, and the agent is told you didn't
cancel it, so it can ask again. Calls to clackatoo's tools go on too, as the
connections they're served on are taken over: a `wait_for_chat` or
`wait_for_terminal` under way is answered as it would have been, and a
question a tool asked you still waits for your answer. Only calls waiting on
clackatoo's browser, which an upgrade stops, or on a window, fail, so the
agent calls them again.

An agent is stopped instead, as the daemon upgrades, with all it runs, if:

- the build it upgrades to can't take agents over, as a build from before they
  could go on;
- that build pins another version of the agent's adapter, as when an upgrade
  moves Claude Code's adapter on, or would declare other capabilities to it
  than it was told as it started, as when a build adds one;
- it's set to: `CLACKATOO_LIVE_AGENTS=off` in the daemon's environment, or
  `daemon.upgrade.keep_agents = false` in the
  [daemon's config](~clackatoo/reference/agent-settings#which-agent);
- the agent is still starting, has stopped listening, has left a request
  unanswered for two minutes, has no chat, or what it runs from has gone;
- one of its chats is opening its session, connecting to clackatoo's tools,
  telling a past session's conversation again, or moving to another agent;
- something it sent is still being parsed, two seconds after the upgrade asks.

Then what it runs ends with it: a deploy an agent waits for is still killed by
an upgrade that moves its adapter's pin or what's declared to it, so detach
one that must outlive such an upgrade. Its chats go on as they did before
agents could go on, and so do those of an agent that couldn't be taken back
after the upgrade. An answer under way continues in the same session after the
upgrade, as above, and the chat shows what the agent ran in the background as
lost, "ended by the daemon's upgrade". The agent is told which: in the turn
that goes on, if one was under way, or else in a turn of its own, a notice
from clackatoo starting `[daemon restarted]`, once, so it can start again what
it still needs. That turn counts toward the 100 turns notices can start under
one of your chats between your messages, as agents can upgrade the daemon
themselves; past them it waits for you. Nothing is told of a turn you had
asked to stop. A turn an agent took by itself that the upgrade cut off isn't
carried on; the agent is told the same way where it was cut off.

If starting the new build fails, the daemon goes on as it was, agents and all.

## Known limits

- The phone's chats don't color code. Sideways swipes on a trackpad don't
  scroll tables: use <kbd>Shift</kbd> and the wheel.
- Moving a file in the explorer forgets which of its lines agents changed.
  Rejecting every change of a file an agent made leaves it empty, rather than
  in the trash. Reviews of everything since the base stop past about 8 MB of
  changes: narrow them with `:Review turn` or `chat`.
- A file the agent changed with a command, or that clackatoo couldn't read
  before the agent wrote it, can't be undone. The chat says which.
- A file changed both by the agent and by something else at once isn't taken
  in, and the bar says so.
- Agents don't see what you've selected in an editor until you send it with
  `<Space>a` or `:Ask`. Commands Claude Code runs with its own tool, rather
  than `run_in_terminal`, show their output once they're done; Codex's show it
  as it comes.
- Gemini CLI and custom adapters have no read-only subscription probes.
- A board's link to a chat's turn names the turn by its number, so after a
  rewind or a switch it goes to whatever turn has that number on the branch
  shown. Switching branches from the phone, and a summary of a branch from
  where it parts, are still to come.
- <kbd>Ctrl</kbd>+<kbd>?</kbd> doesn't list a form's keys. What an input
  method is composing doesn't show in a form's box until it's committed.
- In a past Claude Code session opened again, Read, Grep and Glob rows show
  what they read or looked for, but not what they found, while clackatoo asks
  Claude Code what it runs in the background.
- Codex's commands in the background (its `/ps`) aren't listed: its adapter
  lists them only to a client that asks as JetBrains' AIR does, which leaves
  every command's output out of its row. <kbd>Shift</kbd>+<kbd>Tab</kbd> steps
  through Codex's modes, not its Plan mode.
- You can't use the mouse or type in a browser pane yourself.
- An upgrade forgets the cards of subagents that had already closed, so their
  changes drop out of the chat's and turn's reviews. They stay in
  `:Review base` and in the gutter, as `chat N`.
