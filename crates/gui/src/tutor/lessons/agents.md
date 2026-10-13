# Agents

A pane can hold a chat with an agent, such as Claude Code or Codex, which works on the files in its directory, shows you things in the window, and edits files open in your editor beside you.

Most of this lesson is to read: a chat uses your agent's own login, and with a subscription counts towards its limits. The last two steps try a real chat, only if you choose to; `]s` skips them.

```practice notes.txt
Notes for the agents lesson.
```

## Opening a chat

`:Agent codex` or `:Agent gemini` opens a chat with that agent in a new tab, at once; `:|Agent codex` opens it beside the focused pane.

A chat works in the directory a new terminal would start in. Like any pane, it opens in normal mode: `i` starts typing in its box, Enter sends, and Shift+Enter starts a new line.

If the agent hasn't opened the chat's session after 30 seconds, the chat says so, and what it may be waiting on: most often an MCP server in the agent's config that needs a login. It goes on waiting, and asks whether to keep waiting, which Esc picks, Retry, which starts the agent again, or Stop. Ctrl+C stops it too.

## The start page

`:Agent` with no agent named, `:|Agent` beside the pane, or Ctrl+B and then `a`, opens the start page. Its first row is a new chat, with its agent, model and folder: Enter, or `n`, starts it, so a new chat is still one key away. Tab moves between those three, and `h` and `l` change the one picked. They start as the last new chat had them; the `agent` option says which agent to offer before that.

Under it are chats to go on with: this workspace's chats in no pane, such as those agents started in the background, then the sessions the agent keeps for the folder, its own and clackatoo's, newest first. `j` and `k` move, and Enter shows the chat, or goes on with the session in a new chat, with its conversation so far shown as its turns where the agent can tell it. Type to filter them, as in the finder; `/` first types a letter that's a key. Esc closes it.

Open the start page with `:|Agent`, look around, then press Esc.

```check
finder agent
```

## What it shows

Below each message, what the agent did with it, in order: its words, its thinking, each tool it uses with `○` while it runs and `✓` or `✗` after, and each command it runs and change it makes, on a card of its own. Click a card to open it. While it works the turn ends with `Working…`, and Ctrl+C asks it to stop.

Along the box's bottom are its settings: its mode on the left, how much it does without asking; its model and effort on the right. Shift+Tab steps through the modes.

## Copying, and when it was said

Under each answer the agent has finished is `Copy`: click it to put the answer on the clipboard as the Markdown it came as, as `y` on the answer does in the chat's normal mode. A block of code shows its own Copy button while the mouse is over it.

Drag to select text and let go to copy it to the primary selection; drag past the chat's top or bottom and it scrolls, faster the further past you go, selecting as it goes.

When a message was sent more than half an hour after the turn before it ended, a line across the chat says when: `14:32` today, `Yesterday 09:10`, the day of the week for the last few days, and the date before that.

## What it can show you

Each agent is given clackatoo's own tools, so it can show you things rather than only tell you: open a file in an editor beside the chat, run a command in a terminal you can watch, read a terminal, wait for its command to finish, open a file explorer, point out a pane, and say what the language servers say is wrong. Before it runs a command or types into a terminal, it asks: Allow, Always allow, or Reject.

## Its edits land in your editor

When an agent changes a file you have open, the change lands in the editor as the agent's own edit, beside anything you've typed and not saved. `u` takes back your edits, never the agent's. The file on disk gets the agent's change underneath yours, so the commands it runs see its change, and your work in progress reaches the disk only when you write it.

## Undoing a turn

Under each turn that changed files, the chat says how many, with Undo. In the chat's normal mode, `u` undoes what the last turn changed in files, keeping what you or later turns changed since, and Ctrl+R makes it again. `:AgentUndo 3` and `:AgentRedo 3` name a turn.

## Subagents

An agent hands work on with clackatoo's `start_chat`, or `start_chats` for several at once. Each starts a chat of its own, a subagent, in the background: your chat shows them as one line under the turn that started them, such as `3 subagents working… 12s ›`. Click it, or press Enter on it in normal mode, to see a line for each: what it is, what it was asked, and how it's going. Click one, or press Enter on its line, to open it as a pane.

A subagent is a chat like any other: you can steer it, review and undo what it changes, and answer its questions. The card says when one is waiting for you. Subagents in the background close with the chat that started them.

## Reviewing what they changed

`:Review turn` lists what the chat's last turn changed in files, hunk by hunk, and `:Review chat` everything it changed that you haven't kept: `a` keeps a hunk, `x` rejects it, and `]h` goes to the next. Both take in the chat's subagents too, open or closed: under the heading is a line for each, with what it was asked, and each hunk says whose it is, as `· explore (chat 7)`. A turn's review lists the subagents that turn started, and theirs.

Rejecting a subagent's hunk is a change made as that subagent, as its edits were: never as you, so `u` in the editor leaves it, and never as the chat that started it. Undoing a turn in that chat leaves its subagents' changes, as the review says: reject them in the review instead.

## Profiles

Each subagent starts as a profile, which gives it a role, limits, and an agent and model:

- `explore` researches quickly on the cheapest model, changes nothing, and answers briefly;
- `plan` reads and thinks on a strong model, changes nothing, and returns a plan;
- `general` works as your chat does, on its agent and model.

Add your own, or change these, in the daemon's `agents.toml`, under `[profiles.NAME]`. Claude Code's own Task tool, whose helpers you don't see, is off unless you `:set claudetask`.

## The browser

A chat's agent can drive a web page once you turn on its **Browser** setting, from the chip along the box's bottom, or for every new chat with `:set chatbrowser`. It opens pages, reads them as an outline of headings, links, buttons and text boxes, clicks, types and takes pictures, each a tool call you see in the chat. Before its first page, it asks.

The browser is clackatoo's own, headless, with a profile of its own: never your browser, and never logged in as you. Each chat's page is kept apart from the others'. Its first page opens in a pane beside the chat, where you watch it as it goes; `:|Browser` opens the focused chat's page beside it, and `q` closes the pane.

## Try a chat

Only if you'd like to, and are happy for it to use your agent: press Ctrl+B, `a`, and then Enter to open a chat beside the practice file. The first chat starts the agent, which takes a second or two, and may first ask to install the agent's adapter. Otherwise, press `]s`.

```check
pane chat
```

## Watch an edit land

In the chat, press `i`, ask it to add the line `tutor: hello` to notes.txt, and press Enter. Its change lands in the practice file beside it, as its own edit. Then, in the chat's normal mode, `u` undoes the turn.

```check
has "tutor: hello"
```
