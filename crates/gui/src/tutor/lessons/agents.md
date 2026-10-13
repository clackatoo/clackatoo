# Agents

A pane can hold a chat with an agent, such as Claude Code or Codex, which works on the files in its directory, shows you things in the window, and edits files open in your editor beside you.

Most of this lesson is to read: a chat uses your agent's own login, and with a subscription counts towards its limits. The last two steps try a real chat, only if you choose to; `]s` skips them.

```practice notes.txt
Notes for the agents lesson.
```

## Opening a chat

Ctrl+B and then `a` opens a chat to the right of the focused pane; `:Agent` opens one in a new tab, and `:|Agent` beside the pane. `:Agent codex` or `:Agent gemini` names another agent, and the `agent` option says which `:Agent` opens without a name.

A chat works in the directory a new terminal would start in. Like any pane, it opens in normal mode: `i` starts typing in its box, Enter sends, and Shift+Enter starts a new line.

## What it shows

Below each message, what the agent did with it, in order: its words, its thinking, each tool it uses with `○` while it runs and `✓` or `✗` after, and each command it runs and change it makes, on a card of its own. Click a card to open it. While it works the turn ends with `Working…`, and Ctrl+C asks it to stop.

Along the box's bottom are its settings: its mode on the left, how much it does without asking; its model and effort on the right. Shift+Tab steps through the modes.

## What it can show you

Each agent is given clackatoo's own tools, so it can show you things rather than only tell you: open a file in an editor beside the chat, run a command in a terminal you can watch, read a terminal, wait for its command to finish, open a file explorer, point out a pane, and say what the language servers say is wrong. Before it runs a command or types into a terminal, it asks: Allow, Always allow, or Reject.

## Its edits land in your editor

When an agent changes a file you have open, the change lands in the editor as the agent's own edit, beside anything you've typed and not saved. `u` takes back your edits, never the agent's. The file on disk gets the agent's change underneath yours, so the commands it runs see its change, and your work in progress reaches the disk only when you write it.

## Undoing a turn

Under each turn that changed files, the chat says how many, with Undo. In the chat's normal mode, `u` undoes what the last turn changed in files, keeping what you or later turns changed since, and Ctrl+R makes it again. `:AgentUndo 3` and `:AgentRedo 3` name a turn.

## Subagents

An agent hands work on with clackatoo's `start_chat`, or `start_chats` for several at once. Each starts a chat of its own, a subagent, in the background: your chat shows them as one line under the turn that started them, such as `3 subagents working… 12s ›`. Click it, or press Enter on it in normal mode, to see a line for each: what it is, what it was asked, and how it's going. Click one, or press Enter on its line, to open it as a pane.

A subagent is a chat like any other: you can steer it, review and undo what it changes, and answer its questions. The card says when one is waiting for you. Subagents in the background close with the chat that started them.

## Profiles

Each subagent starts as a profile, which gives it a role, limits, and an agent and model:

- `explore` researches quickly on the cheapest model, changes nothing, and answers briefly;
- `plan` reads and thinks on a strong model, changes nothing, and returns a plan;
- `general` works as your chat does, on its agent and model.

Add your own, or change these, in the daemon's `agents.toml`, under `[profiles.NAME]`. Claude Code's own Task tool, whose helpers you don't see, is off unless you `:set claudetask`.

## Try a chat

Only if you'd like to, and are happy for it to use your agent: press Ctrl+B and then `a` to open a chat beside the practice file. The first chat starts the agent, which takes a second or two, and may first ask to install the agent's adapter. Otherwise, press `]s`.

```check
pane chat
```

## Watch an edit land

In the chat, press `i`, ask it to add the line `tutor: hello` to notes.txt, and press Enter. Its change lands in the practice file beside it, as its own edit. Then, in the chat's normal mode, `u` undoes the turn.

```check
has "tutor: hello"
```
