---
title: Chats Working Together
type: reference
description: How agents start subagents and message other chats, a subagent's pane, profiles, and the limits on what subagents can do.
related:
  - clackatoo/reference/agents
---

An agent can start other chats as its subagents, message them, and wait for
them, within limits you set.

## Chats working together

An agent can work with your other chats through more of clackatoo's tools.

| Tool | Does |
|---|---|
| `list_chats` | Lists the chats in its workspace and family: each one's number, agent, title, folder and workspace, and whether it's idle, working or waiting for you. Then how many subagents it can start (see [Subagent limits](#subagent-limits)). |
| `send_to_chat` | Sends a chat a message, of at most 16 KiB. |
| `read_chat` | Reads a chat's last turns: 3, unless it asks for up to 20. |
| `start_chat` | Starts a subagent: a chat with the agent, profile and folder it picks, sent a first message. It runs in the background, as a card in this chat, unless it asks for a pane. It answers at once, with the chat's number, without waiting for the agent to start: the message goes once it has, and if it can't, the chat fails, saying why, and a wait for it says so too. |
| `start_chats` | Starts several at once, up to 16. |
| `close_chat` | Closes a chat it started, for good, as `:bd` does. Not one you have open in a pane, opened and sent back to the background, or whose changes or comments wait for your review: those are yours to close. |
| `wait_for_chat` | Waits until a chat has answered everything sent to it, for 5 minutes unless it asks for up to 30, and gives every answer to its messages it hasn't had yet, oldest first, or else the last. If the chat still runs something in the background, it says so: the chat might go on by itself when that ends. |
| `wait_for_chats` | Waits for several chats, up to 16, all of them or the first to finish, and gives one summary: each one's status, final answer, and files changed. |

What another chat's agent said reaches an agent quoted, each line after `| `,
in what `read_chat`, `wait_for_chat` and `wait_for_chats` give, as the
messages in a turn are after `> `: nothing a chat says can pass for a turn of
yours, or for clackatoo's words around it.

A chat that started another and isn't waiting for it hears when it finishes: a
notice from clackatoo, starting `[chat 7 finished]`, comes to it as a message
would, as a turn of its own if it's idle. It's framed as clackatoo's, and each
chat's answer in it sits between lines of its own that the answer can't end,
so no agent can pass one off. There's one notice at most waiting in a chat:
what it says is worked out as it begins, every answer the chat hasn't had by
then, so several finishing while the chat is busy come together, and an answer
the chat has had meanwhile, through `wait_for_chat` or `read_chat`, isn't told
again. A turn you had with that chat yourself isn't told. What a chat goes on
to do after a notice, as reporting what its own subagent found, answers the
message that started its work, so the chat above hears it too.

Your chat doesn't show you that text, which is for the agent. It shows a card
in the subagents' teal, headed `↳ 2 subagents finished in 3m 4s`, or
`· 1 failed`, or `↳ 1 subagent failed`, with a line for each chat: its number,
its profile, or else its agent, how long it worked on the message it answered,
how that went if not well, and what it was asked, such as
`chat 7 · explore · 1m 20s — Find where turns are drawn`. Under each line is
the first line of its answer. Select the line, or press <kbd>Enter</kbd> or
`za` on it in normal mode, to read the whole answer under it, and again to
fold it; `zR` and `zM` open and fold them all with the turns' work. Select a
chat's number, or press `gx` on its line, to open that chat beside yours.
Searching the chat finds words in the answers, opening the one it finds them
in, and yanking takes what the card shows (`yim` on a line, its answer whole).
The agent's own answer to the notice shows under the card as any answer does.
Clackatoo's other notices, such as that an
[activity](~clackatoo/reference/agent-tools#managed-activities-and-recovery)
ended, show quoted down the left under `Clackatoo · Activity completion`. A
window or phone older than these cards shows the text the agent was told. The
cards go through a daemon upgrade with the notice. One handed over by a build
that didn't carry them gets its cards back from the text the agent was told,
which doesn't say how long each chat worked or what it was asked, so its lines
leave those out.

Agents are told to use `start_chat` or `start_chats`, with a profile, for all
subagent work. Claude Code's own Task tool, whose helpers work inside one
turn,
[folded under its card](~clackatoo/reference/chat-view#claude-code-s-own-subagents),
is off unless you `:set claudetask`. A chat keeps its choice through an
upgrade, and the chats it starts take it too.

### Subagents

A chat an agent starts runs in the background. Your chat shows it as one line
under the turn that started it, such as "3 subagents working · 12s ›", with "·
1 needs you" when one asks you something. Select it, or press <kbd>Enter</kbd>
on it in normal mode, to see a line for each: its profile, agent and model,
what it was asked, and how it's going. Select a line, or press
<kbd>Enter</kbd> on it, to open that chat as a pane beside yours. A subagent
is a chat like any other: undo its turns, review its changes (your chat's
review shows them too), message it, and answer its questions; the card says
when one is waiting for you. Its tools reach what its parent's do.

When the conversation continues with another message, subagents from earlier
turns that are still working also appear in a compact shelf above the message
box. Select the heading to expand it and a row to open that chat. Subagents
waiting for your answer and those with background tasks still running stay
there too. The shelf disappears once all that work ends; the original cards
remain in the conversation.

#### A subagent's pane

A subagent in a pane, one its agent asked to show or one you opened from its
card, has a teal band across the top that says what it is:
`↳ subagent of chat 12 · explore`, the chat that started it and the profile it
was started as. Select the band, or press <kbd>Ctrl</kbd>+<kbd>B</kbd> `u`, to
go to that chat, whichever tab or workspace it's in.

| Command | Does |
|---|---|
| `:q`, `:q!`, `:close`, <kbd>Ctrl</kbd>+<kbd>B</kbd> `x` | In a subagent's pane, closes only the pane: the subagent goes on in the background, a card in its chat again, as if it had never had a pane, and the card opens it once more. Closing its tab or workspace does the same. |
| `:bd` (`:bdelete`) | Closes the focused chat for good, pane and all, a subagent's too. `clackatoo close N` does the same from a shell, and the card then says it's closed. |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> `u` | Goes from a subagent's pane to the chat that started it. |

The daemon decides, so every window and client, and another machine's daemon
relaying for its windows, all close a subagent's pane the same way. A subagent
whose chat has closed has no card to go back to: its band says so, and closing
its pane closes it, as for any chat. A daemon older than protocol 109 closes
the chat with its pane, and its windows' bands don't name the profile; so does
a window older than 109, to which closing a pane meant closing its chat, as it
still does.

Once back in the background it is like any other there, but yours: it lets its
agent's process go once finished, and lasts through an upgrade but not a
restart, as [What lasts](~clackatoo/reference/agents-lasting#what-lasts) says.
It doesn't close with the chat that started it: it passes to the chat above,
if there is one, or else stays open, as one in a pane does. One in a pane
comes back after a restart as a subagent still, band, card and all.

#### Closing

Subagents in the background close with the chat that started them, unless
they're still working or have changes you haven't reviewed: then they pass to
the chat above, if there is one. Otherwise a subagent stays open, finished or
not, until you close it, with `:bd`, or the agent that started it does, with
`close_chat`, so an agent can send its builder fixes for as long as the work
takes. None is ever closed to make room: with 32 open under one of your chats
(`open` under [`[subagents]`](#subagent-limits)), starting another is refused,
and the agent is told to close those it's done with. One you had in a pane and
sent back is yours: `close_chat` won't close it, and it doesn't close with the
chat that started it, passing to the chat above as one in a pane does, or
staying open if there's none. A finished one in the background lets its
agent's process go after 2 minutes, and its next message brings it back in
about half a second.

### Profiles

Each subagent starts as a profile: a role sent before its first message,
limits, and a default agent and model.

| Profile | Does |
|---|---|
| `explore` | Quick research on the cheapest model (Haiku; Codex's 6 Luna, low effort). Read-only; answers briefly. |
| `plan` | Reads and thinks on a strong model (Opus; Codex at high effort). Read-only; returns a plan. |
| `general` | Your chat's agent and model, with no limits of its own. The default. |

Read-only gives Claude Code only its tools that read and search, and no Model
Context Protocol (MCP) servers but clackatoo's, and puts Codex in its
Read-only mode. `no-commands` gives Claude Code its reading and editing tools
only, and puts Codex in its Read-only mode too, as Codex runs commands in its
other modes without asking. For every agent, clackatoo refuses file writes
through it, refuses its own command tools, and turns down the agent's
questions about edits and commands.

A subagent never has fewer limits than the chat that started it. One started
with another agent, which can't share its parent's mode, starts in that
agent's safest mode, and a parent in a read-only or planning mode makes it
read-only. A model named in a profile is matched by its value, its name, or
the one choice that has it in; if none matches, or several do, the agent's own
choice stands, and the chat says so.

Add or change profiles in the
[daemon's config](~clackatoo/reference/config#the-daemon-s-config),
`daemon/init.lua`. A profile of a built-in one's name changes only what it
gives:

```lua
daemon.profiles.review = {
  about = "Reviews a change for bugs",
  agent = "codex",
  model = "gpt-6-luna",   -- or by agent: { claude = "haiku" }; "inherit" for the parent's
  effort = "low",
  limits = "read-only",   -- or "no-commands", or "none"
  prompt = "You review changes for bugs. Say what's wrong, briefly.",
}
daemon.profiles.explore = { model = { claude = "sonnet" } }
```

A message from another chat shows quoted down the left, under "From chat 3 ·
explore", in the subagents' teal, with the profile it was started as if your
chat's agent started it, or else its agent, never as a message of yours; it
reaches the agent framed as another agent's request, not yours: it's told to
ask you before anything destructive or outside its task. If the chat is busy,
the message waits until its answer ends; up to 8 can wait.

Stopping a chat holds it for you, as it pauses your own
[queue](~clackatoo/reference/chatting#queuing-and-steering): messages from
other agents waiting their turn, and any sent to it after, wait until you next
write there (a new message, or one you send from the queue), and so does the
notice that its subagents finished, so no other agent, and no notice from
clackatoo, starts it again. The agent's own background work still can, as when
Claude Code
[goes on by itself](~clackatoo/reference/chat-view#turns-the-agent-takes-by-itself)
after a command it ran ends; what waits for you goes on waiting through that
turn. Your message goes first, then what waited. The chat says how many wait,
and stopping it again, with no turn begun since you last did, drops them; a
**Stop & continue** that sends your message never does. An agent messaging it
is told the user stopped it, and one waiting for it with `wait_for_chat` isn't
kept waiting: it's told why. A chat held so stays held through an upgrade; if
the upgrade stopped what its agent ran, the notice that says so waits for you
too, then goes after your message.

Starting and messaging chats never asks you first. Instead, what an agent
can't do through another chat is refused, and the agent is told to ask you, so
it can never gain, through another chat, what its own chat isn't allowed:

- An agent lists, messages, reads, and waits for only chats in its own
  workspace or family: the chat that started it, the chats it started, and the
  others the chat that started it started. Chats in other workspaces are out
  of its sight, a workspace lent to another machine included.
- A chat outside its family must work inside the sender's project, the git
  repository its folder is in, or else that folder, to be messaged.
- A message to a chat that can do more is refused: one whose Tools setting is
  looser than the sender's, whose [limits](#profiles) are fewer, counting what
  each one's mode rules out, or whose mode comes later among those its agent
  offers, the first, such as Manual or Read-only, being the safest. Modes of
  different agents can't be compared, so a chat with another agent is messaged
  only in its first mode, and not at all if that agent offers no modes. A chat
  whose agent hasn't started yet is judged by the mode it will start in, where
  that's known, and otherwise the sender is told to wait for it.
- A message that waits its turn is checked again as its turn comes. If it
  can't go by then, it's dropped, and the sender is told as it waits for that
  chat.
- A chat is started only inside the project of the chat starting it, a folder
  linked from inside it to outside included.
- A chat an agent starts gets its parent's Tools and Browser settings and
  limits (see [Profiles](#profiles)). With the same agent, it gets its
  parent's mode; with another, that agent's safest. An agent that offers no
  modes as session settings can't be held to its safest, so a chat started
  with one fails to start, and says so, whenever its session opens. Modes an
  agent offers only through the older `modes` field of the Agent Client
  Protocol (ACP) aren't read.
- What one agent sends another shows its control characters, and those that
  reorder the text around them, as `^G` or `<U+202E>`, so nothing in it hides
  from you. So do tag characters, which can spell out words unseen, unless
  they make a flag such as Scotland's, and variation selectors that follow
  nothing they could change. Joiners and other characters emoji and scripts
  such as Persian need are left as they are.
- A chat whose [Subagents setting](#subagent-limits) lets more of its
  subagents work at once than the sender's can do more too, so a chat at 2
  can't fan out through one at 8.
- So can a chat with
  [the browser](~clackatoo/reference/agent-tools#the-browser) on, where the
  sender's is off, or one you've let open pages without asking, where the
  sender has yet to be; and a chat a window has given
  [access](~clackatoo/reference/agent-tools#native-window-access) to, as a
  window gives it to one chat only. A chat with the browser off can't have
  another browse for it, nor one without access click in your window. A chat
  and one it started itself are let message each other whatever of these each
  has, as what you let the one do, it does working for the other. The agents'
  own "allow always" rules, which they keep themselves, aren't known to
  clackatoo, so they aren't compared.
- Agents send each other as many messages as the work needs, within the
  [limits](#subagent-limits) below. Two chats messaging each other back and
  forth stop after 6 messages, each sent in the turn the one before started.
  An agent can't wait for a chat that's waiting for it.
- A notice from clackatoo, such as that a subagent finished or an
  [activity](~clackatoo/reference/agent-tools#managed-activities-and-recovery)
  ended, always reaches the chat it's for, and counts against none of these. A
  turn that a notice that subagents finished starts begins a new chain of
  messages, as yours does, so an agent going round with its builder isn't
  stopped after 6. Instead, notices that subagents finished start at most 100
  turns under one of your chats between two of your messages there: past that,
  the next waits until you write again, there or in the chat it waits in, and
  that chat says so. Nothing is lost, and anything else waiting in that chat
  goes on meanwhile. An agent waiting for a chat holding such a notice isn't
  kept waiting: it's told why. An
  [activity](~clackatoo/reference/agent-tools#managed-activities-and-recovery)'s
  notice, which nothing counts, leaves the chain of messages as it was.

### Subagent limits

The permissions menu beside the model, in a window or on the phone, has a
**Subagents** setting after Tools and Browser: how many subagents can work at
once under the chat. It offers Off, 2, 4, 6, 8, 12 and 16, none more than
`working` below, and `default` below if that's none of them, and starts at
`default`, 6 unless set. Off means none of the chat's subagents work:
`start_chat` and `start_chats` are refused, and so is a message that would set
one of its idle subagents working again, and the agent is told you turned it
off. As you can change it at any time, agents aren't told a number in their
instructions. Instead `start_chat`, `start_chats`, `list_chats` and the waits
for chats each end with how it stands, as
`Subagents: 2 working of the 6 the user lets work at once under this chat; 4 more may start now.`,
and why fewer can start when one of the limits below holds more. The count is
of every subagent working under your chat, however deep, and the fewest that
any chat on the way up allows holds: a subagent starts with its chat's value,
and you can lower it in the subagent's pane, but never raise it above its
chat's. Lowering a chat lowers the subagents under it that had more, and a
chat closing never lets those under it have more than they had. A message that
would set an idle subagent working again counts as starting one. Only you
change it: an agent never can.

Each of your own chats has its own budget, which you set. An agent can message
another of your chats whose setting is no higher than its own chat's, and what
that chat then starts counts against that chat's budget, not the sender's:
that's by design, and `working` below bounds all of them together.

The rest are set in the daemon's config, as `daemon.subagents`:

```lua
daemon.subagents.working = 16       -- subagents working at once in all, under all your chats (1–64)
daemon.subagents.default = 6        -- the Subagents setting a new chat starts at (0, off, to working)
daemon.subagents.open = 32          -- open under one of your chats (1–256)
daemon.subagents.started = 128      -- started under one of your chats since you last wrote in it (1–10000)
daemon.subagents.depth = 3          -- how deep subagents go: yours is 0, its subagents 1 (1–8)
daemon.subagents.hops = 6           -- the longest chain of messages, each in the turn the one before started (1–64)
daemon.subagents.notice_turns = 100 -- turns notices start under one of your chats since you last wrote (1–10000)
```

A value outside its range is a mistake in the config, never brought into
range: the daemon keeps what it last ran, and says so in a window's bar. So
is a name it doesn't know, or a `default` above `working`. With `working`
below 6 and no `default`, new chats start at `working`. The daemon reads them
as it starts and upgrades. Lowering `working` lowers any chat's Subagents
setting that's above it. At a depth of 3, your chat's PM can start a builder, which can start
a helper of its own.

`started` counts the subagents started under one of your chats since you last
wrote in that chat: writing there starts the count again, and so does
rewinding it or switching it to another branch, which count as writing again.

Which chat started which, messages waiting their turn, which answers each chat
has had from each other, the counts above, and each chat's Subagents setting,
last through an upgrade. A chat waiting for others with `wait_for_chat(s)`
goes on waiting through an upgrade, and its call is answered as it would have
been. If the upgrade stops its agent instead, it hears when they settle, as a
notice, whoever they were answering: each of them, or, waiting for the first,
once. Boards, shared between agents and you, have tools of their own: see
[Boards](~clackatoo/reference/boards).
