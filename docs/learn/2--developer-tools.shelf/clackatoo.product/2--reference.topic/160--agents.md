---
title: Agents
type: reference
description: A pane can hold a chat with an agent such as Claude Code or Codex instead of a shell. How to open one, how typing and the agent's questions work, what the chat shows, how its changes to files land in the editor beside yours, what it can show you in the window, which agent it talks to and what that costs, and what survives closing the window or upgrading.
related:
  - clackatoo/reference/window
  - clackatoo/reference/cli
---

A pane can hold a chat with an agent, such as Claude Code, instead of a shell. You type a message in the box along the bottom, the agent answers above it, and it works on the files in the pane's directory as it would in its own program.

Your messages are bubbles on the right. The agent's answers run the width of a column down the middle of the pane, with Markdown laid out as it's meant to look: headings in bold, lists, quotes, links in blue, code in the terminal's font on a background of its own, coloured as an editor colours it when its language is installed, and tables as a grid. Code, quotes and cards all start at the column's left edge.

## Opening a chat

| | Does |
|---|---|
| `:Agent codex`, `:\|Agent gemini`… | Opens a chat with that agent at once: in a new tab, or beside the focused pane, as `:|Terminal` opens a terminal. |
| `:Agent`, `:\|Agent`, `:-Agent`…, Ctrl+B `a` | Opens the start page, for a chat in a new tab or beside the focused pane. |

Until its first message, a chat says which agent it has and the folder it works in, as `Claude Code in ~/project`, and how to start: in normal mode, "Press i to write a message · Shift+Enter for a new line".

### The start page

Its first row is a new chat, with its agent, model and folder. They start as the last chat started from the page had them; before that, the agent is the `agent` option's, the model the agent's own choice, and the folder the focused pane's. Under it are chats to go on with instead: this workspace's chats in no pane, such as those agents started in the background, then the sessions the agent keeps for the folder, its own and clackatoo's, newest first.

| Key | Does |
|---|---|
| Enter | Starts the new chat, shows the chat picked, or goes on with the session picked in a new chat. |
| `n` | Starts the new chat, whatever is picked. |
| `j`, `k`, Down, Up | The next or previous row. |
| Tab, Shift+Tab | The new chat's next or previous picker: agent, model, folder. |
| `l`, `h`, Right, Left | The picker's next or previous choice. |
| `/`, then typing | Filters the chats and sessions, as the finder does. |
| Esc | Clears what's typed, or closes the page. |

`j`, `k`, `n`, `h` and `l` are keys only while nothing's typed: any other letter starts the filter, and `/` types one of them.

Going on with a session shows its conversation so far as the chat's turns, where the agent can tell it, as Claude Code and Codex can. Nothing in those turns changes your files again, asks you anything, or runs anything: they're only shown. An agent that can't goes on from where it was, and the chat says the turns before aren't shown. A message sent while the turns are coming waits until they've all come. A session another chat has open goes to that chat; one open in another workspace's chat is refused, saying where it is.

A chat works in the directory a new terminal would start in: the one the pane it splits is in, or for a new tab, the one the workspace's focused pane is in. That's where the agent reads and changes files, and runs its commands.

Like any new pane, a chat opens in the window's mode, so after `:Agent` it's in normal mode: `i` starts typing into it. A chat that's asking something is always arrived at in normal mode. The first chat starts the agent, which takes a second or two. Until then the chat says it's starting, and a message sent meanwhile waits and goes when it can.

An agent runs in a process group of its own, so stopping it, or choosing Retry, also ends whatever it started, such as the node behind `npx`, and any background job it started without detaching. An upgrade or `clackatoo stop` ends them too: running agents get half a second to finish by themselves, then are asked to stop, and the daemon waits up to a second and a half for them before killing what's left. Before Linux 6.9, only the agent's own process is reached.

## Queuing and steering

While an agent works, Enter queues your draft; Shift+Enter still inserts a newline. Pending messages share a compact Queued panel above the composer; its heading shows the count, or Queue paused when delivery is paused. They run in order after a successful turn. Each row has an ↑ control to send it now and × to remove it. Sending now during a turn steers that work; while idle it resumes delivery. Ctrl+Enter steers the current draft directly.

For text sent to the installed Claude adapter, **Steer** adds the message to the running turn without cancelling it. Other adapters, image messages, and pending provider changes use **Stop & continue**: cancel the current turn, wait for it to finish, then send the new message. The label tells you which behavior will be used. An uncertain steering acknowledgment leaves the message paused so you can check the conversation before retrying.

Stop, a failed turn, or a daemon upgrade pauses the queue. Use ↑ to resume, or × to discard a pending message. Queued text, images and provider choices belong to the daemon, so other windows and remote connections see the same queue. An upgrade preserves it, paused; a full daemon restart does not. The queue holds up to eight messages or 32 MiB, with text limited to 64 KiB per message and the usual image limits.


## Viewing images

Click an image in the message box, a sent message, or a tool result to open the same image viewer over the chat. Escape, Ctrl+; (the normal-mode shortcut), or the × closes it and returns you to your draft and previous mode. While it is open, typing, pasting, scrolling and clicks on the chat behind it do nothing. The × on a draft thumbnail still removes that attachment.

The viewer loads the original image when opened and shows its pixel dimensions when ready. Drafts use the local attachment; sent messages and image tools fetch the original through the daemon connection, including remote and federated chats. Chat updates still carry only small thumbnails. Each chat retains up to 32 MiB / 128 recent encoded originals across window reconnects and daemon upgrades. Older images whose original was never retained or has expired explicitly show “Original unavailable · thumbnail only”. Originals are not saved across a full daemon restart. Decoding runs in the background, with a 16-million-pixel and 8,192-pixel-per-side limit.

## Images returned by tools

When a tool returns an image, a thumbnail appears beneath its entry in the conversation, even when the turn's work is folded. Open the entry to give the preview more room. This works with Codex's View Image and other agents, including Claude, when their adapter returns inline image data or a link to a local image file.

PNG, JPEG and BMP results are supported, up to four per result, with files limited to 8 MiB and images to 16 million pixels. The daemon reads local files on its own machine and sends small previews to the window, so remote chats work too. Web URLs are not fetched. Previews share the chat's 4 MiB history allowance with pasted images; older results keep their labels. Newly received previews survive reconnects and daemon upgrades, but history replayed after a full restart shows labels only. The web client shows a label for tool images.

## Pasting images

Copy a screenshot or image to the clipboard, then press Ctrl+Shift+V in a chat. A thumbnail appears above the text in the message box, and above the sent message in the conversation. Add text if you like, then press Enter to send both; an image can go on its own too.

Click the × on a thumbnail to remove it, or press Backspace while the message box is empty to remove the last one. You can prepare attachments while the agent answers; they stay in the draft until you send. Text on the clipboard still pastes as text.

A message holds up to four images, at most 16 million pixels each and 8 MiB of encoded image data in total. An agent that does not accept images reports that clearly. The full image is sent to the agent, whose session keeps it. Clackatoo keeps small previews (up to 256 pixels on a side) across window reconnects and daemon upgrades, including on remote connections. Each chat keeps up to 4 MiB of recent previews; older messages retain their image labels. Images sent before previews were supported, or history loaded anew from an agent session after a full daemon restart, show labels only. Switching to a different agent carries the labels with the conversation, so paste the image again if that agent needs to see it.

## Typing

In insert mode, what you type goes in the box at the bottom.

| Key | Does |
|---|---|
| Enter | Sends the message. |
| Shift+Enter | Starts a new line in the message. |
| Ctrl+C | While the agent answers, asks it to stop, unless text in the message is selected, which it copies. When it's not answering, copies what's selected in the message or the conversation. |
| Backspace, Delete | Delete a character, or with Ctrl, a word. |
| Left, Right | Move by a character, or with Ctrl, a word. With Shift, they select. |
| Up, Down, Home, End | Move by a line, or to its start or end. With Shift, they select. |
| Ctrl+A | Selects the whole message. |
| Shift+Tab | Moves the agent on to its next mode. |
| Esc | Normal mode. There's no program in a chat to need Esc. |

Clicking in the box puts the cursor there, dragging selects, and a double or triple click selects a word or a line. A paste goes into the message whole, lines and all, without sending it. Text can be typed through an input method: what's being composed shows in the box, at the cursor, until it's committed.

A message beginning with `/` goes to the agent as typed, which is how agents take their own commands, such as Claude Code's `/compact`.

One message is answered at a time. While the agent answers, the button in the box's corner is a stop button, and Enter keeps what you typed until it's done. Clicking the button with nothing being answered sends the message, as Enter does.

What you've typed but not sent belongs to the window: another window, or this one after it closes, starts with an empty box.

## The agent's settings

The rounded message box has its model and reasoning effort at the lower left, followed by one permissions control showing the current approval mode. At the right, a small context gauge sits beside Send, or Stop while the agent answers. Paste images with Ctrl+Shift+V.

The permissions menu groups three separate settings: the agent's approval mode, access to clackatoo's tools, and browser access. Changing one leaves the others as they were. For Claude Code, Manual asks before every change, Accept edits makes edits without asking, Plan plans before it acts, and Auto lets Claude decide what needs asking about. A new Claude chat starts in Auto.

The circular gauge shows context used as a percentage in its centre. Its ring fills clockwise: quiet while there is room, amber past 80%, red past 95%. A Claude account limit above those thresholds also raises its warning colour; the details identify which limit needs attention. When the context capacity is unknown, the gauge shows a dash.

Hover over it, click it, or press `K` in normal mode for the detail: the tokens used of the context, and each limit, how much of it is used and when it resets, as `at 15:40` or `Tuesday 09:00`. The limits are the account's, so every Claude chat shows the latest, from whichever chat heard it; one past its reset time isn't shown. Codex doesn't share its limits, so a Codex chat shows its context alone; its own `/status` says them.

Click a setting to open its menu, immediately above that button. Up and Down move through it, Enter or a click picks, and Esc or a click elsewhere closes it. Shift+Tab steps through the modes without opening the menu.

The model and effort share a searchable menu. Type to filter by model, provider or description, including pinned versions hidden inside families. Up and Down move through results, Enter picks, and Esc closes it without changing your draft. The mouse wheel scrolls the picker independently of the conversation.

The picker opens on favorites if you have any, otherwise the current provider. The left rail switches between favorites and individual providers. Search always covers every provider, placing matching favorites first; clearing the search returns to the previous page. The popup keeps a fixed height, with the provider rail and model list scrolling independently. Click a star beside a model to add or remove a favorite; favorites are saved on this machine and shared across its windows. Each row shows the provider below the model and a tick on the current model.

The menu includes:

- **Models by family:** Opus, Fable, Sonnet and Haiku, each with the version it picks. Picking a family picks its newest version, and keeps up as new ones come out.
- **A family's versions:** the `▸` button at the end of its row, or Right, shows its other versions, to pin the chat to one. Left hides them. A chat on a pinned version opens the menu with that version shown and ticked.
- **Fast mode:** the lightning bolt in the rail beside reasoning effort toggles fast mode. It appears only when the current model advertises support, and fills in while enabled. The adapter controls its availability and account restrictions.
- **Named effort choices**, from Low to Max: click a choice, or with the effort row selected, press Left and Right. The menu stays open as you change it. Default leaves effort to Claude Code, which picks it for each model.

Models from another agent are grouped only when one choice in a family always means its newest version, as Claude Code's `opus` does. Otherwise they're listed one per row.

A chat keeps the settings you pick. If the agent stops, or the daemon is [upgraded](~clackatoo/reference/cli#upgrade), the chat asks for them again as its session reopens.


### Moving a chat to another agent

The model menu lists every agent's models, with its provider below each model, after the chat's own agent's. Picking another of the same agent's models changes it in the chat, as before. Picking another provider stages it with your draft. A ruled “Context handoff pending” row shows the source and destination with their provider icons, and the model button shows your pending choice. Nothing switches until you send a message or image. Choose a different destination to replace it, or click × on the row or pick the current provider to cancel it. Sending switches providers and submits the message together; a refused handoff restores your draft instead of sending to the old provider. The completed handoff stays in the history as a matching divider. An agent clackatoo hasn't installed says "install to use", and sending its pending handoff offers to install it. An agent whose models aren't known yet is asked for them as the menu opens, which takes a moment.

The new agent hears the conversation with your next message, before it, fenced off as context carried over rather than as something to do: the newest turns whole, older ones a line each (what you asked, what it ran and changed, how it ended), the files changed, and the previous agent's plan if it has steps left, to ask you about rather than carry on with. Messages other agents sent the chat are marked as theirs, not yours. The `carryover` option sets about the most it takes, in tokens: 20000 unless set, and never more than a quarter of the new agent's context. Moving back to an agent the chat had before opens its own session again, and it hears only what happened since.

Undoing turns, reviewing changes, boards, clackatoo's tools and the `claudetask` choice stay the chat's, whichever agent answered. The mode and effort carry over where the new agent has one by the same name; where it hasn't, it uses its own, and a chat an agent started gets the new agent's safest mode; the line across the chat says which. The new agent doesn't see the old one's thinking or the whole output of its tools, only what the chat shows, and its first answer costs more, as nothing of the conversation is cached for it yet. You can stage a handoff while the agent is answering, but must wait until it finishes or stop it before sending. Pending choices stay in this window with the draft; closing the window discards them. Turn numbers, as in `:AgentUndo 3`, skip the lines where the chat moved.
## Normal mode

In normal mode, a solid cursor moves through the conversation's text, and the view follows it. Coming into normal mode puts it on the last line in view, unless it's in view already:

| Key | Does |
|---|---|
| `h` `j` `k` `l`, arrows | A character, or a line. |
| `w` `b` `e` | By words. |
| `0` `^` `$` | The start of the line, its first word, its end. |
| `{` `}` | The block before or after. |
| `gg` `G` | The start or end of the chat. |
| Ctrl+D Ctrl+U, Ctrl+E Ctrl+Y, Page Down Page Up | Scroll half a view, or a line. The cursor stays in view. |
| `v` `V` | Select characters, or lines. `o` goes to the other end, Esc stops. |
| `y` | Copies what's selected to the clipboard, or else what the cursor's in: a block of code, a message, an answer as its Markdown, a card, or on a "Worked for" line, the whole turn. |
| `za` `zo` `zc`, Enter on a "Worked for" line | Opens or folds the turn's work, or on a subagents' line, their card. `zR` and `zM` open or fold every turn and every card. |
| `j` `k` in a table | The cell below or above. |
| `zl` `zh`, `zL` `zH` | Scroll the table the cursor's in sideways: a few characters, or half the column. |
| `u` | Undoes what the turn under the cursor changed in files, if it's in view, or else the last turn's, keeping your edits since. The bar says which. |
| Ctrl+R | Makes it again. |
| `K` | Shows how much context the chat fills, and the account's limits, or hides it. Any other key hides it too. |
| `i`, `a`, `o`, Enter | Back to insert mode. |

The mouse wheel scrolls in either mode. While you're at the end, the view follows the answer as it grows. Scrolled up, it stays put, and the bottom bar says so.

## What the chat shows

Below each message you send, what the agent did with it, in order:

- its words
- its thinking, if it shows any, quieter and slanted
- each tool it uses, on a line of its own: `○` while it runs, then `✓` once it has finished, or `✗` if it failed
- each command it runs and each change it makes to a file, in a compact expandable row

Tool rows start collapsed, with a single-line summary and an expand arrow. Long titles end in an ellipsis, and file tools can show just the basename. Opening a row reveals the complete title. Commands start with `$` and show their output beneath; a failed command keeps its last 8 output lines visible even when collapsed.

A change's card has what the agent did at its head, such as `Edit Cargo.toml`, and how many lines it adds and removes, such as `+3 −1`. Under that come the lines, with those added on green after `+` and those removed on red after `−`. A new file is all added. A card shows the part of the file changed, not the whole file. Once the change is made, Claude Code also gives 3 lines either side of it, and where in the file it is.

Lines are numbered by where they are in the file: added and unchanged lines by where they are now, and removed lines by where they were. Before the change is made, as while the agent asks to make it, an edit's lines have no numbers yet. A new file is numbered from 1.

Click a row's header to open or close it. In normal mode, Enter or `za` on a tool toggles it, `zo` opens it and `zc` closes it. Open output and diffs wrap, with up to 1,000 lines shown. Long open cards also have a Collapse button at the bottom. Whether a card is open belongs to the window. Copying the whole tool keeps the complete command and retained output.

The daemon keeps the last 32 KB of a command's output, and the first 256 KB of each side of a change.

Once the agent has answered, everything before its answer folds under one line: `Worked for 42s ›`. Click it, or press Enter on it, to open it, and again to fold it. While the agent works, the turn is open and the line says `Working…`. When it's done, it folds itself, unless you've opened or folded it yourself. A turn that's only words has no line. Whether a turn is open belongs to the window. A turn the agent didn't finish says why: `Stopped.`, or what went wrong. The tab is named after the title the agent gives the conversation.

A table is a grid: its head in bold over a line, each column as wide as its widest cell and lined up as the table says. One wider than the column scrolls sideways, with `‹` or `›` at a cut edge: Shift and the wheel over it, `zl` and `zh` in normal mode, or the cursor going to a cell out of view. Copied, its cells are a tab apart and its rows a line.

Code in a fenced block that names its language is coloured by the daemon's grammars, as `K`'s is, once the block's closing fence has come; until then, and in a language that isn't installed, it's plain. Code in the agent's thinking stays plain.

While the agent works, a line just above the box says so, wherever the chat is scrolled to: three dots pulsing in clackatoo's colour, what the agent is doing — `Starting Claude Code`, `Loading the session`, `Thinking`, `Running: cargo test`, `Running a command`, `Reading Cargo.toml`, `Writing`, or `Working` between tools — and how long the turn has been going, as `· 12s`, from the column's left. A notice goes above it, not in its place, and in a narrow pane what it's doing is cut short before the seconds are. The dots move only while the chat has the keyboard in a focused window and the agent has been heard from in the last few minutes; otherwise they step once a second. Nothing moves once the agent stops, while it asks you something, or while the daemon is out of reach. Its tab, and its workspace in the top bar, show `⋯` (see [the window](~clackatoo/reference/window#workspaces-tabs-and-panes)).

When you send a message more than half an hour after the turn before it ended, a line across the chat says when: `14:32` today, `Yesterday 09:10`, the day of the week for the six days before, such as `Monday 18:05`, and the date before that, such as `6 Oct 14:32`, with the year if it was another year. The time is your machine's, on a 24-hour clock.

While the mouse is over a turn the agent has finished, `Copy` shows under its answer and copies the answer to the clipboard as the Markdown it came as, as `y` on it does in normal mode. It says `Copied` until the mouse leaves it. When the turn changed files, it shares the line with how many and the buttons that undo and redo them.

### When the agent asks

In Manual mode, before it does anything that changes something, such as writing a file or running a command that isn't only reading, the agent asks. Other modes ask about less, and Auto leaves it to Claude. Its question takes the box's place, with an answer on each button: the first way to say yes in the accent colour, and the ways to say no quieter.

Click an answer to give it. In insert mode, press an answer's number to give it, or Esc to say no. In normal mode, a number picks its answer out with a ring, and the bar says what it is: the same number again, or Enter, gives it, from a third of a second after to five seconds after, so a count such as `22j` typed in one go can't answer by chance; any other key, a click, the wheel or leaving the window drops it, and Esc leaves the question be. A number pressed within half a second of the question coming, of you arriving at its chat, or of the last key you typed, doesn't answer it; the bar says to press it again. Ctrl+C says no and stops the answer. Other keys go nowhere while it asks, and the bar says which numbers answer it.

clackatoo's own questions about its tools say Reject, as Claude Code's do, name a new chat's profile ("with the general profile"), write folders from your home as `~/…`, and show what the agent wrote as code, each message quoted on its own, so nothing in it can pass for clackatoo's words.

What it asks about follows the agent's own settings. Claude Code reads its usual settings files, so commands you've allowed in Claude Code are allowed here too.


### Selecting and copying

Drag across the conversation to select its text, or double-click a word or triple-click a line. Letting go copies it to the primary selection, which a middle click pastes. Drag past the top or bottom of the conversation and it scrolls, faster the further past you go, selecting as it goes. Ctrl+Shift+C, `y` in normal mode, or Ctrl+C while the agent isn't answering copies it to the clipboard, where `"+` has it too. What's copied is the text as shown, without its Markdown. A turn's folded work isn't copied, only its message and its answer. Over a block of code, a Copy button copies the code.
## The files it changes

When the agent changes a file you have open, the change lands in the editor as the agent's own edit, in every window showing it, beside anything you've typed and not saved. `u` takes back your edits, never the agent's, and each of the agent's tool calls is one step of its own. A change to a line you've changed and not saved isn't made: the bar and the chat say so, the file keeps the agent's version, and `:e!` takes it or `:w!` keeps yours.

The file on disk gets the agent's change underneath yours: a file you'd saved stays saved, and one you hadn't stays changed by just your edits, still unsaved. So the commands the agent runs, such as `cargo build`, see its change, and your work in progress reaches the disk only when you write it.

The agent's commands change files too, such as `cargo fmt`, `sed -i` or a code generator, without saying so. clackatoo watches the files you have open, and a change made while one of the agent's tools runs, with only that chat answering in the project, is taken in as that agent's, under the same rule about lines you've changed; undoing its turn takes it out too. A change made while it's thinking or asking permission, or while two chats are answering in the project, is treated as from outside. Files indented with tabs are taken in like any other.

Claude Code and Codex read files from disk, so they don't see what you haven't saved. An agent that reads through clackatoo sees the editor's text, unsaved changes and all, and can only read and write files in its chat's directory, or the git repository that's in.

### Undoing a turn

Under each turn that changed files, a compact summary shows how many, with Undo or Redo beside the count. Hover over the answer to show Copy alongside it. Undo takes the turn's changes out of every file it changed, and keeps what you or later turns changed since. If something since changed the same lines, nothing is undone, and the chat says which lines: `:AgentUndo! 3` undoes the turn's other files anyway. A file the turn made goes to the trash, and a file that changes while the undo is being written is left as it is, and the chat says so. Once a turn is undone, Redo puts it back. `:AgentUndo` and `:AgentRedo` take a turn's number, from 1, or else take the last turn there is to undo or the first to redo. In an open file, an undo is one edit of the agent's, beside your unsaved changes. Undoing waits for the agent to finish answering.

### Reviewing its changes

Lines an agent changed that you haven't looked at yet get a bar of their own beside the line numbers, right of git's signs, in a colour for the chat (`agentsigns` turns it off). `:Review` opens a review beside the pane: every change of the chat's you haven't reviewed, by file, each with three lines around it, what its lines were on red and what they are on green. `:Review turn` shows the last turn's, `:Review turn 3` the third's, and `:Review base` everything changed since the branch left `main` (or `master`, or its upstream), yours included, with the agents' marked. `<Space>gr` opens one, goes to it, or closes it.

| Key | Does |
|---|---|
| `]h` `[h`, `]f` `[f` | The next change or the one before, the next file or the one before |
| Enter | Goes to the line in the editor |
| `a`, `A` | Keeps the change, or every change in its file: its colour goes |
| `x`, `X` | Rejects the change, or every change in its file |
| `gc` | Comments on the line, for the agent |
| `S` | Sends the comments, as one message |
| `R` | Works the list out again |
| `q` | Closes the review |

Rejecting takes the change out as undoing a turn does, keeping what you've changed since, including anything you typed inside the agent's change. It's refused if you've changed the same lines, and says which. `:ReviewKeep` and `:ReviewReject` do the change at the cursor, or `file`, `turn` (`turn 3`) or `all`; `:ReviewReject!` rejects what it can. Comments wait until `:ReviewSend` or `S` sends them, as "Review comments:" and a line for each, with its file and line, and whether it's in the agent's change; they move with the text as you and the agent edit. Undoing a turn clears its colour; redoing it brings it back. What you've reviewed, the comments waiting and open reviews last through an upgrade.

A review of a turn or of the chat takes in its subagents too, open or closed: a turn's review lists the ones that turn started, and theirs. Under the heading is a line for each, with what it was asked, and each change says whose it is, as `· explore (chat 7), turn 1`, the turn being the subagent's own. Keeping and rejecting work as for any change. A rejected change of a subagent's is that subagent's step, never the chat's you're reviewing and never yours; rejecting a file holding several chats' changes does all of it or none, a step for each chat. Changes of two chats too close together to tell apart are refused: reject each chat's alone. Undoing a turn in the chat that started them leaves their changes, as the review says: reject them in the review instead. `:ReviewKeep turn` and `:ReviewReject turn` take in the subagents the turn started.

## What the agent can show you

Each chat's agent is given tools of clackatoo's own, so it can show you things in the window instead of only telling you. An agent asked to run your tests, say, can run them where you watch.

| Tool | Does |
|---|---|
| `open_file` | Shows a file in an editor beside the chat, at a line if it gives one. The next file it opens takes that pane's place. Your typing stays where it is. |
| `run_in_terminal` | Opens a terminal beside the chat, running your own shell, and types a command at its first prompt. You can watch it, and type into it yourself. |
| `run_in_background` | Does the same in a new [background tab](~clackatoo/reference/window#background-tabs) of the chat's workspace, out of your tab bar, for what runs on by itself, as a dev server. Nothing moves your focus; it shows in your list of background tabs. |
| `read_terminal` | Reads a terminal: its last command, whether it's still running or how it exited, and the end of its output. |
| `wait_for_terminal` | Waits until the command in a terminal finishes, for a minute unless it asks for longer, up to ten. |
| `send_to_terminal` | Types into a terminal, as you would: an answer to a prompt, or another command. |
| `explore` | Opens a file explorer beside the chat. |
| `list_panes`, `show_pane` | List the panes in the chat's workspace, its background tabs' too, and point one out: its border flashes, or its tab is marked `●`. A background tab's pane isn't pointed out. Nothing an agent does moves your typing to another pane or tab. |
| `get_diagnostics` | What the language servers say is wrong in the chat's project, as the editor shows it. |

The first pane an agent opens goes to its chat's right, and the next ones stack below that. An agent's tools reach only its chat's workspace.

No tool marks a terminal to [keep running](~clackatoo/reference/window#keeping-a-terminal-running), and `run_in_terminal` and `run_in_background` refuse a command line that runs `clackatoo keep` or `clackatoo bg`. That's a guard against mistakes, not a boundary: whatever an agent types into a terminal, or a script it runs there, follows the rules below, and could do what you could at that shell.

Before running a command or typing into a terminal, the agent asks: Allow, Always allow, or Reject. The question shows exactly what would be typed, with keys such as Enter shown as `⏎` and nothing cut short; long text goes in the conversation, in full. Always allow lets the agent run commands in terminals it started without asking; typing into a terminal you opened always asks, and so does showing a file or folder outside the chat's project. The tools that only show or read inside the project never ask.

The chat's box has a Tools setting, Ask, Allow or Off, that you can change at any time. The `agenttools` option sets it for new chats, `ask` to begin with, and `CLACKATOO_AGENT_TOOLS=off` for the daemon gives no agent any tools.

Waiting for a command relies on the shell marking where each one ends, as bash, zsh and fish do in clackatoo; such a shell gets up to 30 seconds to show its first prompt before the agent is told it didn't. In other shells the agent gets the terminal's last lines, and is told it can't know when a command finishes.

A command is typed only while the shell has the terminal to itself, as for a terminal [kept running](~clackatoo/reference/window#keeping-a-terminal-running): a `$SHELL` that starts another shell rather than becoming it, as `sh -c fish` does, never has, so the agent is told its command wasn't typed, and why; set the daemon's `$SHELL` to the real shell.

A command typed while the shell is still drawing its next prompt is waited for from that prompt on, and a command that clears the screen and scrollback, as `clear` does, doesn't end the wait.

The tools are an MCP server (the [Model Context Protocol](https://modelcontextprotocol.io)), which the agent starts as `clackatoo mcp`, for Claude Code and Codex alike. It talks to the daemon for the one chat it was started for.

## The browser

A chat's agent can drive a web page once you turn its Browser setting on, from the chip beside Tools in the box, or for every new chat with `:set chatbrowser`. It's off unless you do.

| Tool | Does |
|---|---|
| `browser_navigate`, `browser_back` | Open a page, or go back. http, https and `data:` addresses; never files. |
| `browser_snapshot` | Reads the page as an outline of headings, links, buttons and text boxes, each with a ref. |
| `browser_click`, `browser_type`, `browser_press_key` | Click or type into a thing by its ref, or press a key. |
| `browser_screenshot`, `browser_read` | A picture of the page, or its text. |

Before its first page, the chat asks: Allow for this chat, or Reject. The browser is clackatoo's own headless Chromium, with a profile of its own, never yours, so nothing is logged in, and each chat's page is kept apart from the others'. Downloads are turned off. A chat started as `explore`, or with no commands, reads pages but clicks and types in none, and opens no `data:` page. What the agent reads of a page reaches it marked as the page's, not as anything you asked.

A snap of Chromium can only use a profile in its own folder under `~/snap`, and anywhere else it would use yours: clackatoo puts its profile there, checks once the browser has started that it's using it, and stops it at once, and never starts it again, if it isn't.

The first page opens a pane beside the chat showing the page as it goes, with its address and what the agent did last. `:Browser` opens the focused chat's page in a tab, `:|Browser` beside it, and `q` closes the pane. Pictures come only while a window shows the pane, a few a second.

`CLACKATOO_BROWSER` for the daemon names the browser, or `off` for none; `CLACKATOO_BROWSER_PROFILE` names its profile folder.

## Chats working together

An agent can work with your other chats through more of clackatoo's tools.

| Tool | Does |
|---|---|
| `list_chats` | Lists the chats: each one's number, agent, title, folder and workspace, and whether it's idle, working or waiting for you. Chats in other workspaces show only their number and title. |
| `send_to_chat` | Sends a chat a message. |
| `read_chat` | Reads a chat's last turns: 3, unless it asks for up to 20. |
| `start_chat` | Starts a subagent: a chat with the agent, profile and folder it picks, sent a first message. It runs in the background, as a card in this chat, unless it asks for a pane. |
| `start_chats` | Starts several at once, asking you once for all of them. |
| `wait_for_chat` | Waits until a chat has answered everything sent to it, for 5 minutes unless it asks for up to 30, and gives its answer. |
| `wait_for_chats` | Waits for several chats, all of them or the first to finish, and gives one summary: each one's status, final answer and files changed. |

Agents are told to use `start_chat` or `start_chats`, with a profile, for all subagent work. Claude Code's own Task tool, whose helpers work out of sight, is off unless you `:set claudetask`. A chat keeps its choice through an upgrade, and the chats it starts take it too.

### Subagents

A chat an agent starts runs in the background. Your chat shows it as one line under the turn that started it, such as "3 subagents working… 12s ›". Click it, or press Enter on it in normal mode, to see a line for each: its profile, agent and model, what it was asked, and how it's going. Click a line, or press Enter on it, to open that chat as a pane beside yours. A subagent is a chat like any other: undo its turns, review its changes (your chat's review shows them too), message it, and answer its questions; the card says when one is waiting for you. Its tools reach what its parent's do.

Subagents in the background close with the chat that started them, unless they're still working or have changes you haven't reviewed: then they pass to the chat above, if there is one. Past 8 under one of your chats, the oldest close to make room, but only those that have finished, whose answers the agent that started them has read, with nothing waiting for your review and no subagents of their own. A finished one in the background lets Claude Code's process go after 2 minutes, and its next message brings it back in about half a second.

### Profiles

Each subagent starts as a profile: a role sent before its first message, limits, and a default agent and model.

| Profile | Does |
|---|---|
| `explore` | Quick research on the cheapest model (Haiku; Codex's 6 Luna, low effort). Read-only; answers briefly. |
| `plan` | Reads and thinks on a strong model (Opus; Codex at high effort). Read-only; returns a plan. |
| `general` | Your chat's agent and model, with no limits of its own. The default. |

Read-only gives Claude Code only its tools that read and search, and no MCP servers but clackatoo's, and puts Codex in its Read-only mode. `no-commands` gives Claude Code its reading and editing tools only, and puts Codex in its Read-only mode too, as Codex runs commands in its other modes without asking. For every agent, clackatoo refuses file writes through it, refuses its own command tools, and turns down the agent's questions about edits and commands.

A subagent never has fewer limits than the chat that started it. One started with another agent, which can't share its parent's mode, starts in that agent's safest mode, and a parent in a read-only or planning mode makes it read-only. A model named in a profile is matched by its value, its name, or the one choice that has it in; if none matches, or several do, the agent's own choice stands, and the chat says so.

Add or change profiles in `agents.toml`:

```toml
[profiles.review]
about = "Reviews a change for bugs"
agent = "codex"
model = "gpt-6-luna"        # or by agent: { claude = "haiku" }; "inherit" for the parent's
effort = "low"
limits = "read-only"        # or "no-commands", or "none"
prompt = "You review changes for bugs. Say what's wrong, briefly."
```

A message from another chat shows with "From chat 3 · Claude Code · …" over it, and reaches the agent framed as another agent's request, not yours: it's told to ask you before anything destructive or outside its task. If the chat is busy, the message waits until its answer ends; up to 8 can wait, and stopping the chat drops them, and says so.

An agent can never gain, through another chat, what its own chat isn't allowed:

- A message goes without asking only to a chat in its own family, the chat that started it, the chats it started and their siblings, and only if that chat's Tools setting isn't looser than its own. Otherwise you're asked: Allow, Always allow for that chat, or Deny.
- Starting a chat asks you first unless the chat's Tools setting is Allow, and always asks for a folder outside the project. Starting several asks once, listing each one's agent, profile and folder. The new chat gets its parent's Tools setting and mode, and its limits (see [Profiles](#profiles)).
- Reading or waiting on a chat needs the same workspace, the same family, or a pair you've allowed.
- One message of yours can lead to at most 16 turns of agents' work across all chats, and a chat of yours can have at most 8 chats started under it (finished ones in the background close to make room), 4 of them working at once, a message waking one counting too, 2 deep. An agent can't wait for a chat that's waiting for it.

Which chat started which, and messages waiting their turn, last through an upgrade. Boards, shared between agents and you, have tools of their own: see [Boards](~clackatoo/reference/boards).

## Which agent

A chat talks to Claude Code unless you name another: `claude`, `codex` (OpenAI's Codex) or `gemini` (Gemini CLI, not yet tried with clackatoo). The chat says which in its box: "Message Codex".

clackatoo speaks ACP, the [Agent Client Protocol](https://agentclientprotocol.com). Claude Code and Codex don't speak it themselves, so each runs through an ACP adapter, at a version tried with clackatoo. The daemon looks for an agent's adapter where clackatoo installs it, `~/.local/share/clackatoo/agents`, then on your `PATH`, then runs it with `npx`, which needs Node.js. While it runs from `PATH` or `npx`, the chat offers once to install it, which makes it start faster and work offline: Claude Code's takes about 540 MB and Codex's about 450 MB, each with its own copy of the agent, installed from a lock file so nothing it depends on can change. With none of the three, the chat asks to install it before it can start. A new build that moves to a newer adapter asks again, and removes the old one once nothing uses it.

Each agent uses its own login: run `claude`, or `codex login`, once in a terminal. A chat with an agent no one's logged in to says what to run. With a subscription, chats count towards its limits.

If an agent hasn't opened a chat's session after 30 seconds, the chat says so, and what it may be waiting on: most often an MCP server in the agent's config that needs a login. For Codex that's `~/.codex/config.toml` (or `$CODEX_HOME`), and the chat names the servers there reached over the network; for Claude Code, its settings in `~/.claude.json` or the project's `.mcp.json`. `codex mcp list` or `claude mcp list` in a terminal shows which can't start. An agent that hasn't answered at all is said not to have started; one run through `npx` is given two minutes, as npx may be fetching it. The chat goes on waiting, and asks whether to **Keep waiting**, which Esc picks, **Retry**, which stops the agent and starts it again, or **Stop**; Ctrl+C stops it too. A session that comes after you stop is closed unseen, and the next message asks again. While another chat is in the middle of a turn with the same agent, Retry only asks again rather than starting it again, which would cut that turn off. The model menu says the same of an agent that doesn't say what models it has within 20 seconds.

An agent that stops listening, say because its input broke, is still read for a moment for the rest of what it says, but nothing more is sent to it: a message to it ends at once, saying the agent stopped, and so does stopping the chat or changing a setting. A chat that tries to start it meanwhile waits, saying it's stopping, and starts it again once it has gone, sending any message you typed.

A question clackatoo asks itself, such as this one or an offer to install, shows as it's worded; anything an agent asks is always shown as the agent's, "Allow the agent to …?", however it's worded.

Codex offers its own settings in the box: its mode in the permissions menu, from Read-only, which asks before every change, through Workspace access and Auto review to Full access, and its model and reasoning effort at the lower left. A new Codex chat starts in the mode Codex's own config gives.

To add an agent that speaks ACP, or run a known one another way, list it in `~/.config/clackatoo/daemon/agents.toml`:

```toml
[agents.opencode]
name = "OpenCode"            # what the chat shows; the name, if left out
command = ["opencode", "acp"]
mode = "build"               # the mode a new chat asks for, if offered

[agents.claude]
command = ["claude-agent-acp"]
```

The daemon reads it whenever a chat starts. `CLACKATOO_AGENT` gives Claude Code another command, for the daemon it's set for.

Each agent runs one process for all its chats, and stops when its last chat closes. Claude Code's adapter also runs a Claude Code process per chat, of about 250 MB.

## What lasts

The daemon keeps chats, as it keeps terminals. Closing the window closes nothing: a chat keeps answering, and the next window shows it.

[Upgrading](~clackatoo/reference/cli#upgrade) the daemon keeps each chat's conversation, but stops the agent. An answer that was under way is cut off, and the turn says so. The next message starts the agent again and carries on the same conversation. A chat from a build before chats were kept as turns comes through as one block of its text. Tools from a build before cards come through as lines, without their output or changes. Changes from a build before line numbers come through without them.

If the agent stops on its own, the chat says so, and the next message starts it again, carrying on the same conversation.

A chat keeps which agent it talks to. Chats from before there were other agents talk to Claude Code.

What each turn changed in files lasts through an upgrade, so turns can still be undone, up to about 16 MB per chat. Past that, the oldest turns are too long ago to undo.

A chat that moved between agents keeps the sessions it had with each, and goes back to them after an upgrade too.

When the daemon stops, a chat in a pane comes back with its workspace the next time it starts, on the same session: no agent starts until you look at the chat or send it a message, and then the agent tells the conversation so far, as going on with a [past session](#the-start-page) does, and nothing it tells acts again. The chat keeps its agent, model, tools and Browser settings; it starts in the agent's own starting mode, and as a chat of its own, not another's subagent. Chats in no pane don't come back, but the start page lists their sessions.

`clackatoo capture` prints a chat as text: each message after a `>`, and what the agent said.

An upgrade stops the browser; the next page starts it again.

## Not yet

- The phone's chats don't colour code. Sideways swipes on a trackpad don't scroll tables: use Shift and the wheel.
- Moving a file in the explorer forgets which of its lines agents changed. Rejecting every change of a file an agent made leaves it empty, rather than in the trash. Reviews of everything since the base stop past about 8 MB of changes: narrow them with `:Review turn` or `chat`.
- A file the agent changed with a command, or that clackatoo couldn't read before the agent wrote it, can't be undone. The chat says which.
- There's no menu of agents to pick from: name one after `:Agent`. Codex's Plan mode can't be changed from the box.
- A file changed both by the agent and by something else at once isn't taken in, and the bar says so.
- Agents can't yet see what you've selected in an editor. Commands run with the agent's own tool, rather than `run_in_terminal`, show their output once they're done.
- Limits come only as Claude Code's adapter says them, when they change: after an upgrade they're blank until the next. Per-model weekly limits and extra usage aren't shown.
- `/` commands aren't listed or completed as you type them.
- You can't click or type into a browser pane yourself.
- An upgrade forgets the cards of subagents that had already closed, so their changes drop out of the chat's and turn's reviews. They stay in `:Review base` and in the gutter, as `chat N`.
