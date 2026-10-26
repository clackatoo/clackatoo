# Agents

A pane can hold a chat with an agent, such as Claude Code or Codex, which works on the files in its directory, shows you things in the window, and edits files open in your editor beside you.

Most of this lesson is to read: a chat uses your agent's own login, and with a subscription counts towards its limits. The last two steps try a real chat, only if you choose to; `]s` skips them.

```practice notes.txt
Notes for the agents lesson.
```

## Loading older messages

Long chats open on recent turns in the window and web app. Scroll near the top to load earlier messages without losing your place. In the web app, **Load earlier messages** also fetches a page. Each device keeps its own range, including through reconnects. The daemon retains the whole conversation. Load an older turn before asking to undo it by number.

## Opening a chat

`:Agent codex` or `:Agent antigravity` opens a chat with that agent in a new tab, at once; `:|Agent codex` opens it beside the focused pane.

A chat works in the directory a new terminal would start in, and until its first message it says which agent and folder it has, as `Claude Code in ~/project`. Like any pane, it opens in the window's mode, so after `:Agent` it's in normal mode, and says so: `i` starts typing in its box, Enter sends, and Shift+Enter starts a new line.

If the agent hasn't opened the chat's session after 30 seconds, the chat says so, and what it may be waiting on: most often an MCP server in the agent's config that needs a login. It goes on waiting, and asks whether to keep waiting, which Esc picks, Retry, which starts the agent again, or Stop. Ctrl+C stops it too.

## Queuing and steering

While an agent works, Enter queues your draft; Shift+Enter still inserts a newline. Pending messages share a compact Queued panel above the composer; its heading shows the count, or Queue paused when delivery is paused. They run in order after a successful turn. Each row has an ↑ control to send it now and × to remove it. Sending now during a turn steers that work; while idle it resumes delivery. Ctrl+Enter steers the current draft directly. While you type during a turn, the composer shows a compact Steer or Stop & continue button and a square Stop control.

For text sent to the installed Claude or Codex adapter, **Steer** adds the message to the running turn without cancelling it. Other adapters, image messages, and pending provider changes use **Stop & continue**: cancel the current turn, wait for it to finish, then send the new message. The label tells you which behavior will be used. If the turn finishes while guidance is being delivered, that message runs once as the next turn instead. Codex uses the installed adapter with Clackatoo's steering backport; unpatched custom or PATH adapters retain Stop & continue. An uncertain steering acknowledgment leaves the message paused so you can check the conversation before retrying.

Stop or a failed turn pauses the queue. Use ↑ to resume, or × to discard a pending message. Queued text, images and provider choices belong to the daemon, so other windows and remote connections see the same queue. An upgrade preserves it: messages follow automatically continued work, while already paused queues and unconfirmed steering stay paused. A full daemon restart does not preserve it. The queue holds up to eight messages or 32 MiB, with text limited to 64 KiB per message and the usual image limits.


## Viewing images

In the web app, tap **Attach photos** beside Send to choose photos or screenshots. Up to four thumbnails appear above the message box; × removes one. Send an image on its own or add text, and Queue works while the agent is busy. Photos stay with that chat while navigating and reconnecting, and clear only after delivery is accepted. Unsent photos are kept in this page's memory, so send them before reloading or closing the page. Large photos are reduced to a maximum side of 4,096 pixels; unsupported formats show an error so you can choose a PNG or JPEG export.

Click an image in the message box, a sent message, or a tool result to open the same image viewer over the chat. Escape, Ctrl+; (the normal-mode shortcut), or the × closes it and returns you to your draft and previous mode. While it is open, typing, pasting, scrolling and clicks on the chat behind it do nothing. The × on a draft thumbnail still removes that attachment.

The viewer loads the original image when opened and shows its pixel dimensions when ready. Drafts use the local attachment; sent messages and image tools fetch the original through the daemon connection, including remote and federated chats. Chat updates still carry only small thumbnails. Each chat retains up to 32 MiB / 128 recent encoded originals across window reconnects and daemon upgrades. Older images whose original was never retained or has expired explicitly show “Original unavailable · thumbnail only”. Originals are not saved across a full daemon restart. Decoding runs in the background, with a 16-million-pixel and 8,192-pixel-per-side limit.

## Images returned by tools

When a tool returns an image, a thumbnail appears beneath its entry in the conversation, even when the turn's work is folded. Open the entry to give the preview more room. This works with Codex's View Image and other agents, including Claude, when their adapter returns inline image data or a link to a local image file.

PNG, JPEG and BMP results are supported, up to four per result, with files limited to 8 MiB and images to 16 million pixels. The daemon reads local files on its own machine and sends small previews to the window, so remote chats work too. Web URLs are not fetched. Previews share the chat's 4 MiB history allowance with pasted images; older results keep their labels. Newly received previews survive reconnects and daemon upgrades, but history replayed after a full restart shows labels only. The web client shows retained thumbnails for message attachments and tool images.

## Pasting images

Copy a screenshot or image to the clipboard, then press Ctrl+Shift+V in a chat. A thumbnail appears above the text in the message box, and above the sent message in the conversation. Add text if you like, then press Enter to send both; an image can go on its own too.

Click the × on a thumbnail to remove it, or press Backspace while the message box is empty to remove the last one. You can prepare attachments while the agent answers; they stay in the draft until you send. Text on the clipboard still pastes as text.

A message holds up to four images, at most 16 million pixels each and 8 MiB of encoded image data in total. An agent that does not accept images reports that clearly. The full image is sent to the agent, whose session keeps it. Clackatoo keeps small previews (up to 256 pixels on a side) across window reconnects and daemon upgrades, including on remote connections. Each chat keeps up to 4 MiB of recent previews; older messages retain their image labels. Images sent before previews were supported, or history loaded anew from an agent session after a full daemon restart, show labels only. Switching to a different agent carries the labels with the conversation, so paste the image again if that agent needs to see it.

## Gemini through Antigravity

Choose **Antigravity** in the provider rail, or open `:Agent antigravity`. Clackatoo installs Google's official ACP runtime from a pinned, SHA-256-checked release for x86_64 or ARM64 Linux. It needs no Node.js. Allow about 1.1 GB for the install and another 1 GB for runtime scratch on disk.

Sign in from a terminal on the machine that hosts the chat:

```sh
clackatoo agent login antigravity
```

Open the Google link it prints. On the same machine, the browser returns to the runtime and sign-in completes. From a laptop connected through federation, run the command in a terminal belonging to the desktop workspace. Open the link on your laptop; if the browser ends at an unreachable `127.0.0.1` page, copy the complete URL from its address bar, paste it into that terminal and press Enter. Input is hidden. Ctrl+C or `cancel` followed by Enter cancels sign-in. The return URL is accepted only for that login and only once; it never goes into chat history.

This login is separate from the Antigravity IDE. Its credentials stay on the hosting machine, in the private `antigravity-profile` directory beside the installed agents. Runtime scratch lives there too, rather than in `/tmp`. Clackatoo does not import an IDE login or silently use an API key from the environment.

After signing in, send a message or reopen the model picker. Models and settings come from your Google account, rather than a fixed list. Images, queued messages, provider handoffs and session restoration use the same paths as other ACP providers. Antigravity's approval modes are Default, Auto edit and YOLO; the mode names offered by the runtime appear in the permissions menu. Its running turns use Stop & continue for steering.

Click the context gauge to see Antigravity's subscription allowances alongside your other providers: Gemini, Gemini 4 Argon and shared Claude/GPT windows, when Google reports them. These use the same cached background refreshes and reset countdowns. Quota checks do not send model prompts. An untouched allowance has no countdown until Google reports a measured reset.

Antigravity uses its native file tools, including image viewing. Save editor changes before asking it to read them. File diffs, tool approvals, command output and image previews appear in the same chat history as other providers. Images open in the shared full-size viewer.

The separate **Gemini CLI** provider remains available for existing API-key setups.

## The start page

`:Agent` with no agent named, `:|Agent` beside the pane, or Ctrl+B and then `a`, opens the start page. Its first row is a new chat, with its agent, model and folder: Enter, or `n`, starts it, so a new chat is still one key away. Tab moves between those three, and `h` and `l` change the one picked. They start as the last new chat had them; the `agent` option says which agent to offer before that.

Under it are chats to go on with: this workspace's chats in no pane, such as those agents started in the background, then the sessions the agent keeps for the folder, its own and clackatoo's, newest first. `j` and `k` move, and Enter shows the chat, or goes on with the session in a new chat, with its conversation so far shown as its turns where the agent can tell it. Type to filter them, as in the finder; `/` first types a letter that's a key. Esc closes it.

Open the start page with `:|Agent`, look around, then press Esc.

```check
finder agent
```

## What it shows

Below each message, the agent’s words and thinking appear in order. Tools are compact rows with `○` while running and `✓` or `✗` after. Click a row to see its full title, command output or file changes; click its header again to collapse it. In normal mode, Enter or `za` on a tool opens or closes it; `zo` opens and `zc` closes. Failed commands keep the last few lines of output visible.

Its words are Markdown: headings, lists, quotes, links, code, and tables. Code in a language you've installed is coloured as an editor colours it, once its block is whole. A table is a grid, its head in bold over a line; one too wide for the chat scrolls sideways with Shift and the wheel over it, or `zl` and `zh` in the chat's normal mode (`zL` and `zH` go half the width), and moving the cursor to a cell out of view brings it into view.

While it works, a line just above the box says so, wherever the chat is scrolled to: three dots pulsing in clackatoo's colour, what the agent is doing, such as `Thinking`, `Running: cargo test` or `Writing`, and how long the turn has been going, as `· 12s`. The dots move only while the chat has the keyboard; elsewhere they step once a second, and once the agent stops, nothing moves. Ctrl+C asks it to stop.

Along the box's bottom, click the model and effort at the left to open the model picker. It opens on favorites if you have any, otherwise the current provider. The rail switches between favorites and providers. Search always covers all providers, with matching favorites first; clearing it returns to your previous page. Click a star to save a favorite on this machine. Up and Down move through the results, Enter picks, and Esc closes it; your draft stays intact. The picker keeps its height; the wheel scrolls the model list or provider rail under the pointer. Named reasoning-effort choices sit below the results. A lightning bolt beside them toggles fast mode, appearing only when the current model supports it and filling in while enabled. Menus open immediately above their buttons.

Choosing another provider stages a handoff with your draft. The “Context handoff pending” divider shows the source and destination; switching happens only when you send the next message or image. Pick a different provider to replace it, or pick the current provider or click × on the divider to cancel. A refused handoff restores your draft. The completed transition stays in history. You can stage a choice while the current agent is working, then send after it finishes or you stop it. Pending choices belong to this window and disappear when it closes.

Beside the model is one permissions control, showing the approval mode. Its menu groups approval mode, clackatoo tools and browser access; each remains a separate setting. Shift+Tab still steps through the modes.

Beside Send is a small circular context gauge, with the percentage in its centre. The ring fills clockwise, turning amber past 80% and red past 95%; a Claude account limit near full also raises its warning colour. Hover over the gauge for context details. Click it, or press `K` in normal mode, to open Usage & limits: conversation context followed by the account allowances of installed providers. Claude Code, Codex and Antigravity show their reported windows in allowance cards, with a large percentage left and a reset countdown inside each bar; other adapters say when limits are unavailable. Scroll for more providers, and close with Escape or Ctrl+;. Refresh checks again without sending a model prompt; requests are paced to protect provider limits. Readings are cached after the first completed message and refreshed every three messages, at most once a minute. Opening the modal shows the cache immediately and requests a refresh. A failed refresh keeps the last successful reading with its update time.

## Which chats need you

A tab says what its chats are doing after its name: `⋯` while one is at work, `?` while one asks you something, and `•` when one finished while this window wasn't showing it with the focus, until you show it. The workspaces in the top bar say the same of theirs, so a chat in another workspace that wants you shows there. When chats in one tab or workspace would say different things, the one that needs you most is shown: `?`, then `⋯`, then `•`.

## Reading in normal mode

Ctrl+; puts the chat in normal mode, with a solid cursor on the last line in view, or where it was if that's still in view. `h`, `j`, `k`, `l`, `w`, `b`, `W`, `B`, `E`, `f` and `t` with `;` and `,`, `gg` and `G` move it as in an editor, with a count before them, as `3j`, and Ctrl+E, Ctrl+D and Ctrl+F scroll as far as in any pane; in a table, `j` and `k` go to the cell below and above. Ctrl+? lists every key. `v` and `V` select, and `y` copies. `za` opens or folds the turn's work under the cursor, and on the line of a turn's subagents, their card; `zR` opens all of them, and `zM` folds them.

## Copying, and when it was said

Under each answer the agent has finished, while the mouse is over its turn, is `Copy`: click it to put the answer on the clipboard as the Markdown it came as, as `y` on the answer does in the chat's normal mode. A block of code shows its own Copy button while the mouse is over it.

Drag to select text and let go to copy it to the primary selection; drag past the chat's top or bottom and it scrolls, faster the further past you go, selecting as it goes.

When a message was sent more than half an hour after the turn before it ended, a line across the chat says when: `14:32` today, `Yesterday 09:10`, the day of the week for the last few days, and the date before that.

## What it can show you

Each agent is given clackatoo's own tools, so it can show you things rather than only tell you: open a file in an editor beside the chat, run a command in a terminal you can watch, read a terminal, wait for its command to finish, open a file explorer, point out a pane, and say what the language servers say is wrong. Before it runs a command or types into a terminal, it asks: Allow, Always allow, or Reject. In insert mode, press an answer's number to give it, or Esc to turn it down. In normal mode, a number picks its answer out, and the bar says what it is: the same number again, or Enter, a moment later and within five seconds, gives it, so a count such as `2j` or `22j` can't answer by chance, and Esc, a click or the wheel drops it. A number pressed just as the question comes, or just as you arrive at its chat, or just after other keys typed while it asks, doesn't answer it, as it was likely meant for what was there before; the bar says to press it again.

## Its edits land in your editor

When an agent changes a file you have open, the change lands in the editor as the agent's own edit, beside anything you've typed and not saved. `u` takes back your edits, never the agent's. The file on disk gets the agent's change underneath yours, so the commands it runs see its change, and your work in progress reaches the disk only when you write it.

## Undoing a turn

Under each turn that changed files, a compact summary shows how many, with Undo or Redo beside the count. Hover over the answer to show Copy alongside it. In the chat's normal mode, `u` undoes what the last turn changed in files, keeping what you or later turns changed since, and Ctrl+R makes it again. `:agentundo 3` and `:agentredo 3` name a turn.

## Subagents

An agent hands work on with clackatoo's `start_chat`, or `start_chats` for several at once. Each starts a chat of its own, a subagent, in the background: your chat shows them as one line under the turn that started them, such as `3 subagents working · 12s ›`. Click it, or press Enter or `za` on it in normal mode, to see a line for each: what it is, what it was asked, and how it's going; `zR` opens every card in the chat. Click one, or press Enter on its line, to open it as a pane.

A subagent is a chat like any other: you can steer it, review and undo what it changes, and answer its questions. The card says when one is waiting for you. Subagents in the background close with the chat that started them.

## Reviewing what they changed

`:Review turn` lists what the chat's last turn changed in files, hunk by hunk, and `:Review chat` everything it changed that you haven't kept, to the right of the focused pane, or with a split's mark, as `:-Review`, on that side: `a` keeps a hunk, `x` rejects it, and `]h` goes to the next. Both take in the chat's subagents too, open or closed: under the heading is a line for each, with what it was asked, and each hunk says whose it is, as `· explore (chat 7)`. A turn's review lists the subagents that turn started, and theirs.

Rejecting a subagent's hunk is a change made as that subagent, as its edits were: never as you, so `u` in the editor leaves it, and never as the chat that started it. Undoing a turn in that chat leaves its subagents' changes, as the review says: reject them in the review instead.

## Profiles

Each subagent starts as a profile, which gives it a role, limits, and an agent and model:

- `explore` researches quickly on the cheapest model, changes nothing, and answers briefly;
- `plan` reads and thinks on a strong model, changes nothing, and returns a plan;
- `general` works as your chat does, on its agent and model.

Add your own, or change these, in the daemon's `agents.toml`, under `[profiles.NAME]`. Claude Code's own Task tool, whose helpers you don't see, is off unless you `:set claudetask`.

## The browser

A chat's agent can drive a web page once you turn on **Browser** in the permissions menu beside the model, or for every new chat with `:set chatbrowser`. You can turn it on during an existing conversation; no new chat is needed. It opens pages, reads them as an outline of headings, links, buttons and text boxes, clicks, types and takes pictures, each a tool call you see in the chat. Before its first page, it asks. Turning Browser off closes its page and blocks further browser calls immediately.

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

## Continuing after upgrades

If a daemon upgrade interrupts an answer, the agent reopens the same provider session and continues the same turn automatically. It is told to check interrupted tool results before repeating actions. Unsent messages and images are preserved. Idle, completed and manually stopped chats stay idle. This applies to planned upgrades, not crashes or ordinary restarts. The first upgrade from a build without continuation records still needs another message.


## Native window access

Native access starts **Off**. Focus the chat in the window you want to share,
then run `:agentaccess inspect` for screenshots and visible controls, or
`:agentaccess control` to also allow clicks, text and keys throughout that
window. This is broad access to what the window can display and operate.
The title and top bar identify the grant. Click the top bar's **Revoke** badge
or use `:agentaccess off` from any pane. Closing the window, disconnecting or upgrading also clears it.
Automation cannot use window input to answer approval questions, change chat
permissions or grant itself access. Control is not an operating-system sandbox.

Only one chat can control a window, and one window can be granted to a chat.
Federated mirrored chats cannot receive a grant; open a window directly on the
remote daemon with `--remote` instead. The browser app cannot grant native access.
Each input consumes an observed screenshot revision; changed targets or human
input require another screenshot. These tools do not change desktop focus.

## Managed activities and recovery

Ask the agent to use a managed activity for a long build, test run, deployment
or bounded watcher. Its persistent card shows Starting, Running, Stopping and
the observed result in native and web chat history. Expand the card for its
command and bounded final output. Stop requests Ctrl+C; it does not claim the
process has exited until shell integration reports it. The background pane is
also available through `:Background`.

The harness tells agents when to use `start_activity`, `read_activities` and
`cancel_activity`. Existing command approvals apply. A stable operation ID is
saved before execution; asking to start the same ID and command returns the
existing record. Reusing it for a different command is refused. Completion
notifies the owning chat, without resuming a chat explicitly stopped by you.

Jobs and their records survive a planned daemon upgrade. After a full restart,
unfinished operations say **Outcome unknown** and never restart automatically.
A command whose startup was interrupted is also marked unknown. Agents must
inspect the record and current state before deciding what to do. This recovery
covers managed activities, not every provider or external tool call.

Activities require workspace storage and shell integration. Each chat retains
at most 64 jobs, with 8 active; the daemon keeps at most 128. Results retain the
last inspected terminal output up to 4 KiB; the terminal keeps its own scrollback.

## Inspecting capabilities

`:Capabilities` shows the focused chat's tools policy, command restrictions,
browser configuration and permission, native grant, and tool connection.
On the web, tap the model selector, then **Tools**, to open the same report.
The selector also has **Model** and **Permissions** tabs, grouping the web
composer controls in one sheet. Agents can call
`harness_capabilities` themselves. A requested tool catalogue is reported
separately from model acknowledgment: the harness cannot prove a model saw it.
