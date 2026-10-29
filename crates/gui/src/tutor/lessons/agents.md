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

While an agent works, Enter queues your draft; Shift+Enter still inserts a newline. Pending messages share a compact Queued panel above the composer; its heading shows the count, or Queue paused when delivery is paused. They run in order after a successful turn. Each row has a pencil to take it back into the box to edit, an ↑ control to send it now, and × to remove it. Sending now during a turn steers that work; while idle it resumes delivery. Ctrl+Enter steers the current draft directly. While you type during a turn, the composer shows a compact Steer or Stop & continue button and a square Stop control.

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

In a workspace of another machine you've `:connect`ed to, or with `:Agent@host`, the sessions are that machine's agent's, and going on with one starts the chat there.

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

Beside the model is one permissions control, showing the approval mode. Its menu groups approval mode, clackatoo tools and browser access; each remains a separate setting. Shift+Tab still steps through the modes. With Codex, its Plan mode is in the menu too, under its mode: while it's on, the control says so first, as `Plan · Auto review`, and once you approve its plan, Codex leaves Plan mode and gets on with it.

Beside Send is a small circular context gauge, with the percentage in its centre. The ring fills clockwise, turning amber past 80% and red past 95%; a Claude account limit near full also raises its warning colour. Hover over the gauge for context details. Click it, or press `K` in normal mode, to open Usage & limits: conversation context followed by the account allowances of installed providers. Claude Code, Codex and Antigravity show their reported windows in allowance cards, with a large percentage left and a reset countdown inside each bar; other adapters say when limits are unavailable. Scroll for more providers, and close with Escape or Ctrl+;. Refresh checks again without sending a model prompt; requests are paced to protect provider limits. Readings are cached after the first completed message and refreshed every three messages, at most once a minute. Opening the modal shows the cache immediately and requests a refresh. A failed refresh keeps the last successful reading with its update time.

## Which chats need you

A tab says what its chats are doing after its name: `⋯` while one is at work, `?` while one asks you something, and `•` when one finished while this window wasn't showing it with the focus, until you show it. The workspaces in the top bar say the same of theirs, so a chat in another workspace that wants you shows there. When chats in one tab or workspace would say different things, the one that needs you most is shown: `?`, then `⋯`, then `•`.

## Desktop notifications

When a chat you can't see finishes, fails or asks you something, the desktop shows a notification too: the chat's title and what happened, never what the agent wrote. A chat you can't see is one outside the tab shown, or any chat while the window doesn't have the focus. Each chat has one notification at most, updated as the chat changes, and it goes away once you look at the chat. Clicking it goes to the chat. A subagent's finishing tells the chat that started it, not you, but its questions are yours. The `notify` option says which are shown: `all`, `questions`, or `off`.

Type `:set notify=questions` in normal mode, so only questions are shown. `:set notify=all` puts it back.

```check
option notify=questions
```

## Reading in normal mode

Ctrl+; puts the chat in normal mode, with a solid cursor on the last line in view, or where it was if that's still in view. `h`, `j`, `k`, `l`, `w`, `b`, `W`, `B`, `E`, `f` and `t` with `;` and `,`, `gg` and `G` move it as in an editor, with a count before them, as `3j`, and Ctrl+E, Ctrl+D and Ctrl+F scroll as far as in any pane; in a table, `j` and `k` go to the cell below and above. Ctrl+? lists every key. `v` and `V` select, and `y` yanks what's selected. `za` opens or folds the turn's work under the cursor, and on the line of a turn's subagents, their card; `zR` opens all of them, and `zM` folds them.

## Yanking in a chat

`y` in a chat's normal mode is Vim's yank: `yw` takes a word, `y$` the rest of the line, `yy` the line, `yj` it and the next, with a count as `3yy`, and `yiw`, `yi(` and `yip` Vim's objects in the block of text the cursor's in. Two objects are a chat's own: `yix` yanks the block of code the cursor's in, and `yax` the block with its fence; `yim` yanks the message, answer or card, as it came, and `yam` its whole turn. A chat's lines are each block's own, not the rows they wrap onto.

What's yanked goes into the registers, as an editor's yank does: `"ayy` into `a`, `"Ayy` onto its end, and with no register named, the unnamed one and `"0`, and the clipboard as the `clipboard` option says, so `p` in an editor puts it, lines as lines. Try `yix` on a block of code in a chat, then `p` in an editor.

## Searching a chat

`/` in a chat's normal mode types a search in the bar, and Enter goes to it; `?` searches back. `n` searches again, and `N` back, round the chat's ends. The pattern is Vim's, as in an editor, and Up brings back searches typed before. It searches the text the chat shows, messages, the agent's words and cards' lines, and its folded work too: a match there opens the turn's work, or the tool's card, to show it. Every match in view is lit up until Esc or `:noh`. Only the turns loaded are searched; scroll up to load older ones.

## Copying, and when it was said

Under each answer the agent has finished, while the mouse is over its turn, is `Copy`: click it to put the answer on the clipboard as the Markdown it came as, as `yim` on the answer yanks it in the chat's normal mode. A block of code shows its own Copy button while the mouse is over it.

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

## A subagent's own pane

An agent can give a subagent a pane of its own, and a card opens one as a pane. A teal band across the top of a subagent's pane says what it is, as `↳ subagent of chat 12 · explore`: whose it is, and the profile it was started as. Click the band, or press Ctrl+B `u`, to go to the chat that started it, whichever tab that's in.

`:q`, `:close` or Ctrl+B `x` in a subagent's pane closes only the pane: the subagent goes on in the background, a card in its chat again, as if it had never had a pane, and the card opens it once more. One you sent back is yours: the agent that started it can't close it. `:bd` closes the chat itself for good, pane and all, as `clackatoo close` does from a shell. A subagent whose chat has closed has no card to go back to: its band says so, and closing its pane closes it.

A subagent in a pane comes back after the daemon restarts, a subagent still, if its chat was in a pane too: band, card, `:q` and all, with its profile's limits and never a mode above its chat's.

## Reviewing what they changed

`:Review turn` lists what the chat's last turn changed in files, hunk by hunk, and `:Review chat` everything it changed that you haven't kept, to the right of the focused pane, or with a split's mark, as `:-Review`, on that side: `a` keeps a hunk, `x` rejects it, and `]h` goes to the next. Both take in the chat's subagents too, open or closed: under the heading is a line for each, with what it was asked, and each hunk says whose it is, as `· explore (chat 7)`. A turn's review lists the subagents that turn started, and theirs.

Rejecting a subagent's hunk is a change made as that subagent, as its edits were: never as you, so `u` in the editor leaves it, and never as the chat that started it. Undoing a turn in that chat leaves its subagents' changes, as the review says: reject them in the review instead.

## Profiles

Each subagent starts as a profile, which gives it a role, limits, and an agent and model:

- `explore` researches quickly on the cheapest model, changes nothing, and answers briefly;
- `plan` reads and thinks on a strong model, changes nothing, and returns a plan;
- `general` works as your chat does, on its agent and model.

Add your own, or change these, in the daemon's `agents.toml`, under `[profiles.NAME]`. Claude Code's own Task tool, whose helpers work inside one turn, folded under its card, is off unless you `:set claudetask`.

## Claude Code's own helpers

With `:set claudetask`, a new Claude Code chat can hand work to a helper with its Task tool. The helper's words and tools fold under the Task's card, which says what the helper is doing now, as `↳ Reading src/parse.rs · 3 tool uses`. Click the card, or press `za` on it in normal mode, to see what it did, indented under it; `za` in there folds it again. A search opens the card if it finds something inside.

If you have a chat open with `claudetask` on, ask it to use its Agent tool to read a file, then open the card with `za`.

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

## Shells in the background

Claude Code can leave a command running in the background, such as a dev server, and carry on. A shelf above the box lists what it runs, with its command, how it's going and for how long, counting while it runs. The square at the end of a running line stops it; `:stoptask` does the same from the keyboard, for the newest running, and `:stoptask 2` for the second line. Once one ends, its line says done, failed or stopped until your next message. `tasks = false` for Claude Code in the daemon's `agents.toml` turns the shelf off.

When one ends, Claude Code may go on by itself to look at it: that shows as a turn of its own, under a line saying it went on by itself, with no message of yours.

If you have a chat open, ask it to run `sleep 30` in the background, watch the shelf count, then stop it with `:stoptask`.

## What a background shell printed

Click a line on that shelf, or type `:taskoutput`, to read what its command printed: a terminal opens below the chat, showing the last 256 KiB of the output and following it as it grows, colours and all, with scrollback, `/` search and yanks as any terminal has. `:taskoutput 2` opens the second line's. `:q` in it closes it; the command goes on. Claude Code writes each background command's output to a file on the daemon's machine and its adapter says where, which is all the agent shares: it never sends the output itself. So an agent that doesn't say, or a task from before an upgrade, has nothing to follow, and the bar says so; ask the agent to show you the output instead.

If you have a chat open, ask it to run `for i in 1 2 3 4 5; do echo $i; sleep 2; done` in the background, then click its line on the shelf and watch the numbers come.

## Slash commands

Type `/` at the start of a message and a menu opens over the box: the agent's own commands, such as Claude Code's `/compact` or Codex's `/review`, your skills, then clackatoo's. Keep typing to narrow it, Down and Up to pick, Tab to put one in, Enter to put it in or, once the whole name is typed, to send it, and Esc to close the menu without leaving insert mode. Typed alone, commands only the agent's own terminal interface has don't go to the agent: `/clear` and `/resume` open the start page, `/model` the model menu, `/diff` `:Review`, `/status` your usage limits, `/rewind` undoes the last turn's files, and `/export` copies the conversation.

If you have a chat open, type `/` in its box, then `/model` and Enter to open the model menu.

## Messages sent, and drafts

The box remembers what you've sent, as a shell does. In insert mode, Up on the box's first line puts the message you sent before in the box, and again the one before that; Down on its last line comes forward, and past the newest, back to what you'd typed. With something typed, Up goes back only through messages that start with it. Ctrl+R opens the finder on every message sent: type to narrow them, and Enter puts the one picked in at the cursor. Normal mode's `k` still moves through the conversation.

Up in an empty box while messages are queued takes the newest back out of the queue to edit, as Claude Code does; so does the pencil on its row.

What you type and don't send is kept by the daemon, so closing the window, upgrading, or opening the chat on another machine or the phone finds it in the box again. Typing isn't mirrored live between windows; sending a message empties every other box holding just that message.

If you have a chat open, press `i`, then Up to bring back your last message, and Ctrl+R to search them all.

## Mentioning files

Type `@` at the start of a word in a chat's box and a menu of the project's files opens over it, those the finder lists, leaving out what's ignored. Keep typing to narrow it, as `@pars` for `src/parser.rs`; Down and Up pick, and Tab or Enter puts `@src/parser.rs` in, from the chat's folder, with quotes round a path with a space in it, as `@"docs/read me.md"`. Esc closes the menu.

When the message goes, the agent gets each file it mentions in the chat's project with it: Claude Code a link it reads itself, Codex the file's text if it's small. A file open in an editor with changes not saved goes as it is in the editor, so the agent sees what you see. A mention of something outside the project stays words, and the chat says so. In the conversation, mentions are chips: click one, or put the cursor on it and type `gf`, to open the file.

In an editor, select some lines with `V` and press `<Space>a`, or type `:'<,'>Ask`: they go to the chat beside it, or the one you used last in the tab, as `@notes.txt#L2-L4` at the end of what you're typing, and the chat takes you to its box to say what to do with them. The agent gets those lines' text as they are when you send it. Without a selection, `<Space>a` mentions the whole file. To bind another key, map it to `:Ask<CR>` in your config with `client.keys.map`.

If you have a chat open, type `@` in its box and pick a file, then send it with a question about that file.

## The agent's plan

On a long task, Claude Code keeps a to-do list, and Codex a plan. It shows on a shelf above the box, folded to one line: how many steps are done, and the one being worked on, as `Plan · 2/5 done · ◐ Write the tests`. Click it, or type `:plan`, to open it: a line a step, ☐ to do, ◐ being worked on, ☑ done and dimmed. Click or `:plan` again folds it. Each time the agent changes its list, the shelf follows. Once every step is done, the list stays until your next message. It survives an upgrade, and a chat going on with an old session shows the list as its agent last gave it. The phone shows it too.

If you have a chat open, ask it to plan a small change as a to-do list, then type `:plan` to open the list.

## Questions, and leaving Plan mode

An agent can ask you a question rather than ask leave: Claude Code's multiple-choice questions, Codex's, or a form from an MCP server. It takes the box's place, with a row for each option and a box to type in. `j` and `k` move between rows in either mode, `1` to `9` pick an option of the field you're on, Space ticks one, and Enter presses Submit or Decline. In a box, insert mode types; `i` goes into it from normal mode. Esc never declines a form: what you typed stays.

When the agent asks to leave Plan mode, its plan shows above the question, scrolled with the wheel or `j` and `k`. Saying no keeps it planning: send what you'd change as your next message.

If you have a chat open, ask it to ask you, with its question tool, which of two colours you'd like, and answer from the form.

## When a turn fails, and what it spent

When the model is overloaded or the connection drops, the agent tries again by itself: the line above the box says so while it waits, as `Retrying Claude, attempt 2 of 10.` If a turn fails anyway, it says why in plain words and what to do, such as signing in again with the command it names, or that the account's usage limit is reached; what the agent itself said follows underneath. The last turn, if it failed, has a **Send again** button, and `R` in normal mode does the same: the same message, images and all, as a new turn. If that turn already started commands, changes to files or other tools, the bar says so first: ask again within 5 seconds to send it anyway.

A line across a turn shows where the agent compacted its context to make room, with how many tokens before and after. A command that failed says its exit code on its card, as `$ cargo test · exit 101`. Under each turn, on the right, is what it spent, its tokens and, where the agent says, its cost; `K` shows the session's cost with its context and limits. `:set nochatcost` hides both.

If a turn in a chat of yours has failed, press `Esc`, then `R` to send it again.

## Going on through upgrades

A daemon upgrade doesn't stop agents: each agent's process goes on, and so does everything it runs, such as shells in the background, a dev server, or a deploy it's waiting for. A turn under way keeps streaming into the same turn, a question it asked is still asked, and its calls to clackatoo's tools are still answered. Nothing is sent to carry it on, as nothing was lost.

An upgrade does stop an agent if the new build can't take it over, pins another version of its adapter, or is told to with `keep_agents = false` under `[upgrade]` in the daemon's `agents.toml`. Then the agent reopens the same provider session and continues the same turn automatically, told to check interrupted tool results before repeating actions, and what it ran in the background shows as lost. Unsent messages and images are preserved either way. This applies to planned upgrades, not crashes or ordinary restarts.

If you have a chat open, ask it to run `sleep 300` in the background, run `clackatoo upgrade`, and watch the shelf go on counting.


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
