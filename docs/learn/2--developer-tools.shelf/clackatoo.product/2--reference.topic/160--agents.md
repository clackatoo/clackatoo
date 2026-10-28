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

Long conversations open on recent turns. Scroll toward the top to fetch earlier turns a page at a time, keeping the message you're reading in place. This works in the window and web app, including chats on connected machines. Reconnecting retains the range you've loaded. Live output and file actions keep their original turn identities; to undo a specific older turn by number, load that turn first. Older daemons continue to send the full conversation.

## Opening a chat

| | Does |
|---|---|
| `:Agent codex`, `:\|Agent antigravity`… | Opens a chat with that agent at once: in a new tab, or beside the focused pane, as `:|Terminal` opens a terminal. |
| `:Agent`, `:\|Agent`, `:-Agent`…, Ctrl+B `a` | Opens the start page, for a chat in a new tab or beside the focused pane. |

Until its first message, a chat says which agent it has and the folder it works in, as `Claude Code in ~/project`, and how to start: in normal mode, "Press i to write a message · Shift+Enter for a new line".

### The start page

Its first row is a new chat, with its agent, model and folder. They start as the last chat started from the page had them; before that, the agent is the `agent` option's, the model the agent's own choice, and the folder the focused pane's. Under it are chats to go on with instead: this workspace's chats in no pane, such as those agents started in the background, then the sessions the agent keeps for the folder, its own and clackatoo's, newest first. For a pane on [another machine](~clackatoo/reference/remote#another-machine-s-workspaces-in-your-bar), or a page opened with `@host`, they're the sessions that machine's agent keeps in its folder, and going on with one makes the chat there; this needs a daemon here of protocol 107 or later, and says so otherwise.

| Key | Does |
|---|---|
| Enter | Starts the new chat, shows the chat picked, or goes on with the session picked in a new chat. |
| `n` | Starts the new chat, whatever is picked. |
| `j`, `k`, Down, Up, Ctrl+N, Ctrl+P | The next or previous row. |
| Page Down, Page Up | A page further or back. |
| Home, End | The new chat, or the last row. |
| Tab, Shift+Tab | The new chat's next or previous picker: agent, model, folder. |
| `l`, `h`, Right, Left | The picker's next or previous choice. |
| `/`, then typing | Filters the chats and sessions, as the finder does: Backspace, Ctrl+U and Ctrl+W delete as [the finder's](~clackatoo/reference/window#the-finder) do. |
| Esc | Clears what's typed, or closes the page. |

`j`, `k`, `n`, `h` and `l` are keys only while nothing's typed: any other letter starts the filter, and `/` types one of them. A paste starts the filter too. Ctrl+? lists the page's keys.

Going on with a session shows its conversation so far as the chat's turns, where the agent can tell it, as Claude Code and Codex can. Nothing in those turns changes your files again, asks you anything, or runs anything: they're only shown. An agent that can't goes on from where it was, and the chat says the turns before aren't shown. A message sent while the turns are coming waits until they've all come. A session another chat has open goes to that chat; one open in another workspace's chat is refused, saying where it is.

A chat works in the directory a new terminal would start in: the one the pane it splits is in, or for a new tab, the one the workspace's focused pane is in. That's where the agent reads and changes files, and runs its commands.

Like any new pane, a chat opens in the window's mode, so after `:Agent` it's in normal mode: `i` starts typing into it. A chat that's asking something is always arrived at in normal mode. The first chat starts the agent, which takes a second or two. Until then the chat says it's starting, and a message sent meanwhile waits and goes when it can.

An agent runs in a process group of its own, so stopping it, or choosing Retry, also ends whatever it started, such as the node behind `npx`, and any background job it started without detaching. An upgrade or `clackatoo stop` ends them too: running agents get half a second to finish by themselves, then are asked to stop, and the daemon waits up to a second and a half for them before killing what's left. Before Linux 6.9, only the agent's own process is reached.

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

## Queuing and steering

While an agent works, Enter queues your draft; Shift+Enter still inserts a newline. Pending messages share a compact Queued panel above the composer; its heading shows the count, or Queue paused when delivery is paused. They run in order after a successful turn. Each row has an ↑ control to send it now and × to remove it. Sending now during a turn steers that work; while idle it resumes delivery. Ctrl+Enter steers the current draft directly. While you type during a turn, the composer shows a compact Steer or Stop & continue button and a square Stop control.

For text sent to the installed Claude or Codex adapter, **Steer** adds the message to the running turn without cancelling it. Other adapters, image messages, and pending provider changes use **Stop & continue**: cancel the current turn, wait for it to finish, then send the new message. The label tells you which behavior will be used. If the turn finishes while guidance is being delivered, that message runs once as the next turn instead. Codex uses the installed adapter with Clackatoo's steering backport; unpatched custom or PATH adapters retain Stop & continue. An uncertain steering acknowledgment leaves the message paused so you can check the conversation before retrying.

Stop or a failed turn pauses the queue. Use ↑ to resume, or × to discard a pending message. Queued text, images and provider choices belong to the daemon, so other windows and remote connections see the same queue. An upgrade preserves it: messages follow automatically continued work, while already paused queues and unconfirmed steering stay paused. A full daemon restart does not preserve it. The queue holds up to eight messages or 32 MiB, with text limited to 64 KiB per message and the usual image limits.


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

Hover over the gauge for the conversation's token counts and any account limits reported during that chat. Click it, or press `K` in normal mode, to open **Usage & limits**. The modal separates conversation context from account allowances and lists the installed providers, including those whose adapters cannot report limits. Escape, Ctrl+;, the close button or a click outside dismisses it without changing the draft. Scroll or use the arrow and page keys for more windows.

Claude Code, Codex and Antigravity can read their subscription windows without sending a model prompt: short and weekly limits, plus model-specific or extra-spend allowances when reported. Each allowance has a bordered card with a prominent percentage left and a provider-coloured bar. The hatched part is the allowance already used, and the badge inside the bar counts down to reset. Wide windows place the figure beside the bar; narrow panes stack them. Update times beside provider names use the window's local timezone. API-key accounts and adapters without this interface say when subscription limits are unavailable. Custom launch commands are not probed through another installation's sign-in.

The daemon collects these readings, so windows connected remotely see the accounts on that daemon's machine. It caches readings after the first completed message, then refreshes every three completed messages across chats (or on the next completion after five minutes). Opening the modal shows cached values immediately and requests a refresh, as does **Refresh**. All refreshes have a one-minute minimum interval and concurrent windows share the work; nothing polls while chats are idle. New chats receive the same cached readings. Failed reads retain the last successful values and their update time; expired windows say they need a refresh rather than guessing that usage is zero. Readings are collected again after a daemon upgrade.

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

Undoing turns, reviewing changes, boards, clackatoo's tools and the `claudetask` choice stay the chat's, whichever agent answered. The mode and effort carry over where the new agent has one by the same name; where it hasn't, it uses its own, and a chat an agent started gets the new agent's safest mode; the line across the chat says which. The new agent doesn't see the old one's thinking or the whole output of its tools, only what the chat shows, and its first answer costs more, as nothing of the conversation is cached for it yet. You can stage a handoff while the agent is answering, but must wait until it finishes or stop it before sending. Pending choices stay in this window with the draft; closing the window discards them. Turn numbers, as in `:agentundo 3`, skip the lines where the chat moved.
## Normal mode

In normal mode, a solid cursor moves through the conversation's text, and the view follows it. Coming into normal mode puts it on the last line in view, unless it's in view already. Keys are read as a terminal's and an editor's are, so a count goes before a move (`3j`, `2w`), a command half typed shows at the start of the bar, and Ctrl+? lists them:

| Key | Does |
|---|---|
| `h` `j` `k` `l`, arrows, `+` `-` | A character, or a line. |
| `w` `b` `e`, `W` `B` `E` | By words, or by anything between blanks. |
| `f` `t` `F` `T`, `;` `,` | To a character on the line, or just before it, back with `F` and `T`; `;` again, `,` the other way. |
| `0` `^` `$` | The start of the line, its first word, its end. |
| `{` `}` | The block before or after. |
| `gg` `G` | The start or end of the chat; with a count, the start of that turn, as `3G`. |
| Ctrl+D Ctrl+U, Ctrl+E Ctrl+Y, Ctrl+F, Page Down Page Up | Scroll half a view, a line, or a page of the view less two lines, as far as in any pane, with a count. The cursor stays in view. |
| `v` `V` | Select characters, or lines. `o` goes to the other end, Esc stops. |
| `y` and a motion or object, `yy` `Y` | Yanks what the motion moves over, the object, or whole lines, into the registers (see [Yanking](#yanking)). In visual mode, `y` yanks what's selected and `Y` its lines. |
| `"a` before `y` | Yanks into register `a`, or onto its end with `"A`. |
| `/` `?`, `n` `N` | Searches the chat's text forward or back, and again (see [Searching](#searching)). |
| `za` `zo` `zc`, Enter on a "Worked for" line | Opens or folds the turn's work, or on a subagents' line, their card. `zR` and `zM` open or fold every turn and every card. |
| `j` `k` in a table | The cell below or above. |
| `zl` `zh`, `zL` `zH` | Scroll the table the cursor's in sideways: a few characters, or half the column. |
| `u` | Undoes what the turn under the cursor changed in files, if it's in view, or else the last turn's, keeping your edits since. The bar says which. |
| Ctrl+R | Makes it again. |
| `K` | Opens Usage & limits. Escape or Ctrl+; closes it. |
| `i`, `a`, `o`, Enter | Back to insert mode. |

The mouse wheel scrolls in either mode. While you're at the end, the view follows the answer as it grows. Scrolled up, it stays put, and the bottom bar says so.

### Yanking

`y` is Vim's yank operator. It takes any motion the chat moves by, with Vim's rules for what each takes: `yw` up to the next word, `ye` and `y$` to the word's or the line's end, `yfx` up to `x`, `yj`, `yk`, `ygg` and `yG` whole lines. `yy` and `Y` take the line, and a count takes more, as `3yy` or `y2w`. A chat's lines are each block's own lines, as it breaks them, not the rows they wrap onto: the cursor moves by rows, but `y$`, `y0`, `yfx` and `yj` go by lines, and `yw` on a line's last word stops at its end, as Vim's. Text objects are Vim's, found in the block of text the cursor's in: `iw` `aw`, `iW` `aW`, `is` `as`, `ip` `ap`, `i"` `a"`, `i(` `a(` (and `ib` `ab`, as in Vim) and their kin, `iB` `aB` for braces, and `it` `at`. Two are a chat's own:

| Object | Is |
|---|---|
| `ix` | The block of code the cursor's in. |
| `ax` | The same with its fence and language, as Markdown writes it. |
| `im` | The message, answer or card the cursor's in, as it came: an answer as its Markdown. |
| `am` | Its whole turn: the message, and all the agent said and did for it. |

All four are whole lines. `ic` and `ac` are a class, as in an editor, which a chat hasn't got, so they do nothing. The cursor goes to the start of what's yanked.

What's yanked goes into the window's registers as an editor's yank does: into the register named before it, with `"ayy`, or onto its end, with `"Ayy`; `"_` keeps nothing. With no register named, it goes into the unnamed register and `"0`, and onto the clipboard if the `clipboard` option says. `p` in an editor puts it, lines as lines. Keys typed after a yank or a search, by a macro, a binding or quickly by hand, wait for it, so `nn` goes twice and `/x<CR>yiw` yanks the word found.

### Searching

`/` types a search in the bottom bar, and Enter goes to the first match after the cursor; `?` searches back. Enter with nothing typed searches for the last search again. `n` searches again the same way and `N` the other way, round the chat's ends, and the bar says when it goes round; a count goes that many matches, before `n` or before `/`, as `3/x`. The search is edited as the command line is: Up and Down bring back searches typed before, and Ctrl+R and a register's name puts in what the register holds. The pattern is Vim's, as an editor's search reads it, and its case follows the `ignorecase` and `smartcase` options.

A search looks through the text the chat shows, block by block: messages, the agent's words and thinking without their Markdown, and the lines of cards. A match is never across two blocks. It looks in folded work too, as Vim's search opens folds: a match in a turn's folded work opens the work, and one in a tool's folded card opens the card. Only the turns the chat has loaded are searched; scroll up to load older ones. The cursor goes to the match, and every match in view is lit up, as Vim's `hlsearch` lights them, the match the search, `n` or `N` went to brighter than the rest, wherever the cursor goes after, until Esc (but one leaving visual mode) or `:nohlsearch` (`:noh`), and again at the next search, `n` or `N`. Only the text drawn is looked through for them, with a few kilobytes around it, at most 512 in a block, and only again when what's drawn or the search changes. With the [option](~clackatoo/reference/config#options) `hlsearch` off, only the match found is lit, until Esc or `:noh`.

While you type the search, the cursor goes to the match it would find from where you started, opening the work or card it's in, and its matches are lit, as Neovim's `incsearch` shows them, in an editor too (see [Shown as you type](~clackatoo/reference/editor#shown-as-you-type)). It's worked out once after each key, as one step of a search, within the same bounds. Esc puts the cursor, the view and anything it opened back; Enter keeps them, and is the search. With the [option](~clackatoo/reference/config#options) `incsearch` off, nothing moves until Enter.

A card shows at most 1,000 lines of output: a match only in lines it leaves out is said to be there, rather than not found.

A long chat isn't laid out to be searched: each message, answer and card is first read as the chat holds it, and only one that may hold the next match is opened and laid out. Each step to the next match reads at most 64 MiB of text and lays out at most 32 turns or 4 MiB of what they hold; if it would need more, it stops and says so.

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

While the mouse is over a turn the agent has finished, `Copy` shows under its answer and copies the answer to the clipboard as the Markdown it came as, as `yim` on it yanks it in normal mode. It says `Copied` until the mouse leaves it. When the turn changed files, it shares the line with how many and the buttons that undo and redo them.

### What it runs in the background

Claude Code can leave a command running in the background, such as a dev server or a long build, and carry on with the turn. A shelf above the box lists what it runs: `In the background · 1 running`, then a line for each with its command and what it's for, how it's going, and for how long, as `● npm run dev · Start the dev server — running 2m 4s`. The seconds count while one runs. A watcher it started says `Watching:`. When one ends, Claude Code may [go on by itself](#turns-the-agent-takes-by-itself) to look at it.

The square at the end of a running line stops it, through the agent. From the keyboard, `:stoptask` stops the newest running, and `:stoptask 2` the second line. A watcher can't be stopped from here; ask the agent. The shelf lists at most three, newest first, with how many more there are.

A line that has ended says how: `✓ done`, `✗ failed`, or `■ stopped`, with how long it took and what Claude Code said of it. It stays until your next message. Nothing is left showing as running once it can't be: if the agent stops, what it ran stops with it, and its line says `ended with its agent`.

A subagent's card says how many things its chats run in the background, and when open, has a line under each chat for what it runs. A chat with something running in the background is never let go while it's idle in the background, as that would end it.

Only Claude Code says what it runs in the background. The phone lists the same lines, without the buttons that stop them.

Claude Code's adapter says so only to a client that asks as JetBrains' AIR does, and to such a client it reports some things differently:

- It sends a read or a search of files without what it found. clackatoo asks it for the messages it hands the model too, and shows what Claude Code's Read, Grep and Glob found from those, as before, while they run: in a past session opened again, those rows don't show it. (Claude Code 2.1.310 has no Grep or Glob of its own here: it searches with commands, whose output shows anyway.)
- It keeps the words and thinking of Claude Code's own subagents (its Task tool) to itself unless asked for them: clackatoo asks, so they show as before, and a past session opened again tells them too, which it didn't.
- It doesn't send the same task list twice in a row, which changes nothing shown.
- Its notices, such as a fast mode turned off, come as notices above the box rather than as words in the turn.
- A command moved to the background doesn't go on showing its output in its row once it has moved: the shelf says how it's going, and Claude Code reads what it printed when it ends.
- A question it asks you with its own AskUserQuestion tool is titled `Asking for your input`, not by the question; the question shows under it.
- A failed call of one of its tools for other agents, such as SendMessage, shows its error as plain text, not as code.

Commands' output, file changes and images are as before. To have none of this and no shelf, set `tasks = false` for Claude Code in [`agents.toml`](#which-agent).

### The agent's plan

On a long task, an agent keeps a to-do list: Claude Code's TodoWrite, Codex's plan. It shows on a shelf above the box, over what the agent runs in the background, folded to one line: how many steps are done, and the one being worked on, or else the next to do, as `Plan · 2/5 done · ◐ Write the tests`.

Click that line, or type `:plan`, to open it: a line a step, `☐` to do, `◐` being worked on, `☑` done and dimmed. Click or `:plan` again to fold it. Each time the agent changes its list it sends the whole list again, and the shelf follows. A long list shows the steps around the first one not done, as many as fit in two fifths of the pane, at least three and at most eight, and its heading says which, as `steps 5–12`. The daemon keeps 64 steps at most, each cut to 500 characters on one line.

Once every step is done, the list stays, as `Plan · all 5 done`, until your next message. An agent's plan is shown, not acted on: when a chat [moves to another agent](#moving-a-chat-to-another-agent), the new agent is told the old one's unfinished plan once, and then it's no longer shown. The phone shows the same list, every step, without the click.

### Turns the agent takes by itself

An agent can go on with no message from you: Claude Code does when something it ran in the background ends, to look at what it printed, and may commit and write you a summary. That shows as a turn of its own, with no message, under a line such as `Claude Code went on by itself · Background command "npm test" completed`. The chat is at work while it lasts, and a message you send waits for it. It ends when Claude Code says so. Ctrl+C stops it: Claude Code is asked to stop, and the turn ends once it says it has, after anything still on its way, or after 5 seconds at most. Something Claude Code says with no turn of its own, such as `Task stopped by user` after you stop a task, shows as a notice above the box, not as a turn; only the latest shows. An agent that doesn't say when it starts or ends such a turn opens one with whatever it says while the chat is idle, except what's shaped as its notices, such as Codex's `Warning: …`, which shows as a notice too. Its turn is taken to have finished once it has been quiet for 60 seconds with no tool running; Ctrl+C ends it at once. A daemon upgrade in the middle of such a turn ends it there, rather than carrying it on. A chat that ends one while you aren't looking is marked unseen, a subagent's card says it's working again, and an agent waiting for the subagent with `wait_for_chat` gets what it said.

### When the agent asks

In Manual mode, before it does anything that changes something, such as writing a file or running a command that isn't only reading, the agent asks. Other modes ask about less, and Auto leaves it to Claude. Its question takes the box's place, with an answer on each button: the first way to say yes in the accent colour, and the ways to say no quieter.

Click an answer to give it. In insert mode, press an answer's number to give it, or Esc to say no. In normal mode, a number picks its answer out with a ring, and the bar says what it is: the same number again, or Enter, gives it, from a third of a second after to five seconds after, so a count such as `22j` typed in one go can't answer by chance; any other key, a click, the wheel or leaving the window drops it, and Esc leaves the question be. A number pressed within half a second of the question coming, of you arriving at its chat, or of the last key you typed, doesn't answer it; the bar says to press it again. Ctrl+C says no and stops the answer. Other keys go nowhere while it asks, and the bar says which numbers answer it.

clackatoo's own questions about its tools say Reject, as Claude Code's do, and show what the agent wrote as code, so nothing in it can pass for clackatoo's words.

What it asks about follows the agent's own settings. Claude Code reads its usual settings files, so commands you've allowed in Claude Code are allowed here too.

### Questions, forms and plans

An agent can ask you something rather than ask leave: Claude Code's multiple-choice questions (its AskUserQuestion tool), Codex's questions, an MCP server's form, a link to sign in to an MCP server, or whether to try again with another model when one turns a request down. ACP calls these elicitations. The question takes the box's place as a form: the agent's message, then each field's title and what it asks, then a row for each option, a yes-or-no, or a box to type in, and at the end Submit and Decline.

- A choice of one shows a dot by each option (`◉` picked, `○` not); a choice of several a box to tick (`☑`, `☐`). Options are numbered, and an option's meaning shows under it.
- `j` and `k`, the arrows, Tab and Shift+Tab, or Ctrl+N and Ctrl+P move between rows, in either mode: in normal mode they move through the form, not the conversation. The row picked is lit.
- `1` to `9` pick that option of the field you're in. Space or `x` picks, ticks or flips the row picked. Enter does the same, moves on from a box, or presses Submit, Decline or Open the link.
- In a box, in insert mode, keys type, Backspace, Ctrl+W and Ctrl+U delete, and Tab, Down, Up or Enter move on. In normal mode, `i`, `a` or Enter go into it. A secret shows as dots; a number's box takes only what a number holds. A paste goes into the box picked. Click a row to pick it, or a box to type in it.
- Claude Code puts a box called Other after each of its questions: type your own answer there instead of picking, or a note beside what you picked.
- Submit sends only answers that fit what the agent asked; otherwise the bar says what's missing or wrong. A required field shows `*`. clackatoo checks them again before the agent hears them.
- Esc never declines a form: it goes to normal mode, as it does from the message box, so what you've typed stays. Decline says no, and the agent goes on without. Ctrl+C stops the agent, which takes the form away.
- As with other questions, Submit, Decline and Open don't answer within half a second of the form coming, or of you arriving at its chat; the bar says to press again.

A link to open, such as an MCP server's sign-in, shows whole, with where it goes on a line of its own, as plain text apart from the agent's message, then Open the link, which hands it to your desktop's browser and tells the agent it's open, and Decline. Only a plain web link is shown: one that isn't `http` or `https`, isn't plain ASCII, names a user (`user@host`), holds a space, a control character or `%` in its host, names an address as a number other than a plain dotted quad (such as `0x7f.1`), or hides anything is declined, and each window checks it again before it opens it. If the agent says it's done with a link you never opened, it goes by itself. The browser opens on the window's machine; a sign-in whose page sends you back to `localhost` needs the window on the daemon's machine.

When the agent asks to leave Plan mode, as Claude Code's ExitPlanMode or Codex's plan review do, the plan shows above the question as Markdown, in a frame at most half the pane high that scrolls with the wheel over it, or `j` and `k` in normal mode. The question says "Go ahead with this plan?", and the answers are the agent's own, such as "Yes, and use auto mode" or "No, keep planning".

A form waits until it's answered, from this window or any other, the phone included, and is still there if the window reconnects or you open the chat elsewhere. It goes, answered as cancelled, if you stop the chat (Ctrl+C or Stop), even between turns, or move it to another agent; it goes if the agent stops waiting for it, or if the agent or its session stops. One that comes between turns, as a sign-in can, waits past the end of the turn. One that comes while a past session is being loaded, as an MCP sign-in can, is declined: no one is there to answer it. While a form waits, its agent waits too: a chat an agent started, that no one is watching, stays stuck on it, as on a question asking leave, until you answer it. Its text is shown with nothing hidden in it, as other agents' words are, and a form asking more than 256 KiB of text all told is declined, with a notice in the chat, said once until you next write. A window older than these forms shows only a question naming what the agent asks, with one answer, Decline.

ACP has no way to send words back with a "no". Claude Code takes a refusal of its plan as "keep planning", and stops the turn: send your feedback as the next message. For its questions, the Other box is where your own words go.

Saying it can show forms changes what the agents do, beyond asking:

- Claude Code keeps its AskUserQuestion tool, which its adapter takes away from a client that can't show forms, and passes on MCP servers' forms and sign-in links, and the consent asked before retrying with another model, which it otherwise declines.
- Codex asks its questions (`request_user_input`), which it otherwise answers with nothing, and an MCP server's forms and links come as forms rather than as a question asking leave. It also offers signing in with a device code. A form about no chat, such as one asked while Codex is being logged in, is declined, as there's nowhere to show it.


### Selecting and copying

Drag across the conversation to select its text, or double-click a word or triple-click a line. Letting go copies it to the primary selection, which a middle click pastes. Drag past the top or bottom of the conversation and it scrolls, faster the further past you go, selecting as it goes. While it's selected, the bar says VISUAL, and `y` in normal mode yanks it into the registers, as visual mode's does, after a count or a register too; any other key forgets it. Ctrl+Shift+C, or Ctrl+C while the agent isn't answering, copies it to the clipboard, where `"+` and the unnamed register have it too. What's copied is the text as shown, without its Markdown. A turn's folded work isn't copied, only its message and its answer. Over a block of code, a Copy button copies the code.
## The files it changes

When the agent changes a file you have open, the change lands in the editor as the agent's own edit, in every window showing it, beside anything you've typed and not saved. `u` takes back your edits, never the agent's, and each of the agent's tool calls is one step of its own. A change to a line you've changed and not saved isn't made: the bar and the chat say so, the file keeps the agent's version, and `:e!` takes it or `:w!` keeps yours.

The file on disk gets the agent's change underneath yours: a file you'd saved stays saved, and one you hadn't stays changed by just your edits, still unsaved. So the commands the agent runs, such as `cargo build`, see its change, and your work in progress reaches the disk only when you write it.

The agent's commands change files too, such as `cargo fmt`, `sed -i` or a code generator, without saying so. clackatoo watches the files you have open, and a change made while one of the agent's tools runs, with only that chat answering in the project, is taken in as that agent's, under the same rule about lines you've changed; undoing its turn takes it out too. A change made while it's thinking or asking permission, or while two chats are answering in the project, is treated as from outside. Files indented with tabs are taken in like any other.

Claude Code and Codex read files from disk, so they don't see what you haven't saved. An agent that reads through clackatoo sees the editor's text, unsaved changes and all, and can only read and write files in its chat's directory, or the git repository that's in.

### Undoing a turn

Under each turn that changed files, a compact summary shows how many, with Undo or Redo beside the count. Hover over the answer to show Copy alongside it. Undo takes the turn's changes out of every file it changed, and keeps what you or later turns changed since. If something since changed the same lines, nothing is undone, and the chat says which lines: `:agentundo! 3` undoes the turn's other files anyway. A file the turn made goes to the trash, and a file that changes while the undo is being written is left as it is, and the chat says so. Once a turn is undone, Redo puts it back. `:agentundo` and `:agentredo` take a turn's number, from 1, or else take the last turn there is to undo or the first to redo. In an open file, an undo is one edit of the agent's, beside your unsaved changes. Undoing waits for the agent to finish answering.

### Reviewing its changes

Lines an agent changed that you haven't looked at yet get a bar of their own beside the line numbers, right of git's signs, in a colour for the chat (`agentsigns` turns it off). `:Review` opens a review beside the pane: every change of the chat's you haven't reviewed, by file, each with three lines around it, what its lines were on red and what they are on green. `:Review turn` shows the last turn's, `:Review turn 3` the third's, and `:Review base` everything changed since the branch left `main` (or `master`, or its upstream), yours included, with the agents' marked. `<Space>gr` opens one, goes to it, or closes it.

| Key | Does |
|---|---|
| `j` `k`, `gg` `G`, `H` `M` `L`, Ctrl+D Ctrl+U Ctrl+F, `zt` | Moves, with a count, as every [list](~clackatoo/reference/window#lists) does |
| `]h` `[h`, `]f` `[f` | The next change or the one before, the next file or the one before; a count goes that many |
| `/` `?` `n` `N` | Searches the lines, the files' names and the comments, shown as you type and lit as every [list](~clackatoo/reference/window#lists)'s search is |
| Enter | Goes to the line in the editor |
| `a`, `A` | Keeps the change, or every change in its file: its colour goes |
| `x`, `X` | Rejects the change, or every change in its file |
| `gc` | Comments on the line, for the agent |
| `S` | Sends the comments, as one message |
| `R` | Works the list out again |
| `q` | Closes the review |

Rejecting takes the change out as undoing a turn does, keeping what you've changed since, including anything you typed inside the agent's change. It's refused if you've changed the same lines, and says which. `:reviewkeep` and `:reviewreject` do the change at the cursor, or `file`, `turn` (`turn 3`) or `all`; `:reviewreject!` rejects what it can. Comments wait until `:reviewsend` or `S` sends them, as "Review comments:" and a line for each, with its file and line, and whether it's in the agent's change; they move with the text as you and the agent edit. Undoing a turn clears its colour; redoing it brings it back. What you've reviewed, the comments waiting and open reviews last through an upgrade.

A review of a turn or of the chat takes in its subagents too, open or closed: a turn's review lists the ones that turn started, and theirs. Under the heading is a line for each, with what it was asked, and each change says whose it is, as `· explore (chat 7), turn 1`, the turn being the subagent's own. Keeping and rejecting work as for any change. A rejected change of a subagent's is that subagent's step, never the chat's you're reviewing and never yours; rejecting a file holding several chats' changes does all of it or none, a step for each chat. Changes of two chats too close together to tell apart are refused: reject each chat's alone. Undoing a turn in the chat that started them leaves their changes, as the review says: reject them in the review instead. `:reviewkeep turn` and `:reviewreject turn` take in the subagents the turn started.

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

Before running a command or typing into a terminal, the agent asks: Allow, Always allow, or Reject. The question shows exactly what would be typed, with keys such as Enter shown as `⏎` and nothing cut short; long text goes in the conversation, in full. Always allow lets the agent run commands in terminals it started without asking; typing into a terminal you opened always asks, and so does showing a file or folder outside the chat's project. The tools that only show or read inside the project, and those that start and message chats, never ask.

The chat's box has a Tools setting, Ask, Allow or Off, that you can change at any time. The `agenttools` option sets it for new chats, `ask` to begin with, and `CLACKATOO_AGENT_TOOLS=off` for the daemon gives no agent any tools.

Waiting for a command relies on the shell marking where each one ends, as bash, zsh and fish do in clackatoo; such a shell gets up to 30 seconds to show its first prompt before the agent is told it didn't. In other shells the agent gets the terminal's last lines, and is told it can't know when a command finishes.

A command is typed only while the shell has the terminal to itself, as for a terminal [kept running](~clackatoo/reference/window#keeping-a-terminal-running): a `$SHELL` that starts another shell rather than becoming it, as `sh -c fish` does, never has, so the agent is told its command wasn't typed, and why; set the daemon's `$SHELL` to the real shell.

A command typed while the shell is still drawing its next prompt is waited for from that prompt on, and a command that clears the screen and scrollback, as `clear` does, doesn't end the wait.

The tools are an MCP server (the [Model Context Protocol](https://modelcontextprotocol.io)), which the agent starts as `clackatoo mcp`, for Claude Code and Codex alike. It talks to the daemon for the one chat it was started for.

## The browser

A chat's agent can drive a web page once you turn Browser on in the permissions menu beside the model, or for every new chat with `:set chatbrowser`. It's off unless you do. Enabling it works during an existing conversation without starting a new chat; disabling it closes the page and blocks further browser calls immediately.

When the daemon supports browsing, the tools are advertised from the start, even while Browser is off. Agents that cache their tool lists can therefore use them once you enable Browser. Every call checks the current setting; seeing the tools does not grant access.

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
| `list_chats` | Lists the chats in its workspace and family: each one's number, agent, title, folder and workspace, and whether it's idle, working or waiting for you. |
| `send_to_chat` | Sends a chat a message. |
| `read_chat` | Reads a chat's last turns: 3, unless it asks for up to 20. |
| `start_chat` | Starts a subagent: a chat with the agent, profile and folder it picks, sent a first message. It runs in the background, as a card in this chat, unless it asks for a pane. |
| `start_chats` | Starts several at once. |
| `close_chat` | Closes a chat it started, for good, as `:bd` does. Not one you have open in a pane, opened and sent back to the background, or whose changes or comments wait for your review: those are yours to close. |
| `wait_for_chat` | Waits until a chat has answered everything sent to it, for 5 minutes unless it asks for up to 30, and gives every answer to its messages it hasn't had yet, oldest first, or else the last. If the chat still runs something in the background, it says so: the chat may go on by itself when that ends. |
| `wait_for_chats` | Waits for several chats, all of them or the first to finish, and gives one summary: each one's status, final answer and files changed. |

A chat that started another and isn't waiting for it hears when it finishes: a notice from clackatoo, starting `[chat 7 finished]`, comes to it as a message would, as a turn of its own if it's idle, shown as from Clackatoo. It's framed as clackatoo's, and each chat's answer in it sits between lines of its own that the answer can't end, so no agent can pass one off. There's one notice at most waiting in a chat: what it says is worked out as it begins, every answer the chat hasn't had by then, so several finishing while the chat is busy come together, and an answer the chat has had meanwhile, through `wait_for_chat` or `read_chat`, isn't told again. A turn you had with that chat yourself isn't told. What a chat goes on to do after a notice, as reporting what its own subagent found, answers the message that started its work, so the chat above hears it too.

Agents are told to use `start_chat` or `start_chats`, with a profile, for all subagent work. Claude Code's own Task tool, whose helpers work out of sight, is off unless you `:set claudetask`. A chat keeps its choice through an upgrade, and the chats it starts take it too.

### Subagents

A chat an agent starts runs in the background. Your chat shows it as one line under the turn that started it, such as "3 subagents working… 12s ›". Click it, or press Enter on it in normal mode, to see a line for each: its profile, agent and model, what it was asked, and how it's going. Click a line, or press Enter on it, to open that chat as a pane beside yours. A subagent is a chat like any other: undo its turns, review its changes (your chat's review shows them too), message it, and answer its questions; the card says when one is waiting for you. Its tools reach what its parent's do.

#### A subagent's pane

A subagent in a pane, one its agent asked to show or one you opened from its card, has a teal band across the top that says what it is: `↳ subagent of chat 12 · explore`, the chat that started it and the profile it was started as. Click the band, or press Ctrl+B `u`, to go to that chat, whichever tab or workspace it's in.

| Command | Does |
|---|---|
| `:q`, `:q!`, `:close`, Ctrl+B `x` | In a subagent's pane, closes only the pane: the subagent goes on in the background, a card in its chat again, as if it had never had a pane, and the card opens it once more. Closing its tab or workspace does the same. |
| `:bd` (`:bdelete`) | Closes the focused chat for good, pane and all, a subagent's too. `clackatoo close N` does the same from a shell, and the card then says it's closed. |
| Ctrl+B `u` | Goes from a subagent's pane to the chat that started it. |

The daemon decides, so every window and client, and another machine's daemon relaying for its windows, all close a subagent's pane the same way. A subagent whose chat has closed has no card to go back to: its band says so, and closing its pane closes it, as for any chat. A daemon older than protocol 109 closes the chat with its pane, and its windows' bands don't name the profile; so does a window older than 109, to which closing a pane meant closing its chat, as it still does.

Once back in the background it is like any other there: it closes with the chat that started it, lets its agent's process go once finished, and lasts through an upgrade but not a restart, as [below](#what-lasts).

#### Closing

Subagents in the background close with the chat that started them, unless they're still working or have changes you haven't reviewed: then they pass to the chat above, if there is one. Otherwise a subagent stays open, finished or not, until you close it, with `:bd`, or the agent that started it does, with `close_chat`, so an agent can send its builder fixes for as long as the work takes. None is ever closed to make room: with 32 open under one of your chats, starting another is refused, and the agent is told to close those it's done with. One you had in a pane and sent back is yours: only you close it. A finished one in the background lets Claude Code's process go after 2 minutes, and its next message brings it back in about half a second.

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

Starting and messaging chats never asks you first. Instead, what an agent can't do through another chat is refused, and the agent is told to ask you, so it can never gain, through another chat, what its own chat isn't allowed:

- An agent lists, messages, reads and waits for only chats in its own workspace or family: the chat that started it, the chats it started and their siblings. Chats in other workspaces are out of its sight, a workspace lent to another machine included.
- A chat outside its family must work inside the sender's project, the git repository its folder is in, or else that folder, to be messaged.
- A message to a chat that may do more is refused: one whose Tools setting is looser than the sender's, whose [limits](#profiles) are fewer, counting what each one's mode rules out, or whose mode comes later among those its agent offers, the first, such as Manual or Read-only, being the safest. Modes of different agents can't be compared, so a chat with another agent is messaged only in its first mode, and not at all if that agent offers no modes. A chat whose agent hasn't started yet is judged by the mode it will start in, where that's known, and otherwise the sender is told to wait for it.
- A message that waits its turn is checked again as its turn comes. If it may no longer go, it's dropped, and the sender is told as it waits for that chat.
- A chat is started only inside the project of the chat starting it, a folder linked from inside it to outside included.
- A chat an agent starts gets its parent's Tools setting and limits (see [Profiles](#profiles)). With the same agent, it gets its parent's mode; with another, that agent's safest. An agent that offers no modes as session settings can't be held to its safest, so a chat started with one fails to start, and says so, whenever its session opens. Modes an agent offers only through ACP's older `modes` field aren't read.
- What one agent sends another shows its control characters, and those that reorder the text around them, as `^G` or `<U+202E>`, so nothing in it hides from you. So do tag characters, which can spell out words unseen, unless they make a flag such as Scotland's, and variation selectors that follow nothing they could change. Joiners and other characters emoji and scripts such as Persian need are left as they are.
- Agents send each other as many messages as the work needs. A chat of yours can have at most 32 chats started under it open at once, and 64 started under it in all, 4 of them working at once, a message waking one counting too, 2 deep. Two chats messaging each other back and forth stop after 6 messages, each sent in the turn the one before started. An agent can't wait for a chat that's waiting for it.
- A notice from clackatoo, such as that a subagent finished or an [activity](#managed-activities-and-recovery) ended, always reaches the chat it's for, and counts against none of these. A turn that a notice that subagents finished starts begins a new chain of messages, as yours does, so an agent going round with its builder isn't stopped after 6. Instead, notices that subagents finished start at most 100 turns under one of your chats between two of your messages there: past that, the next waits until you write again, there or in the chat it waits in, and that chat says so. Nothing is lost, and anything else waiting in that chat goes on meanwhile. An agent waiting for a chat holding such a notice isn't kept waiting: it's told why. An [activity](#managed-activities-and-recovery)'s notice, which nothing counts, leaves the chain of messages as it was.

Which chat started which, messages waiting their turn, which answers each chat has had from each other, and the counts above, last through an upgrade. A chat waiting for others with `wait_for_chat(s)` as the upgrade ends its agent hears when they settle, as a notice, whoever they were answering: each of them, or, waiting for the first, once. Boards, shared between agents and you, have tools of their own: see [Boards](~clackatoo/reference/boards).

## Which agent

A chat talks to Claude Code unless you name another: `claude`, `codex` (OpenAI's Codex), `antigravity` (Google Antigravity), or `gemini` (Gemini CLI for existing API-key setups). The chat says which in its box: "Message Codex".

clackatoo speaks ACP, the [Agent Client Protocol](https://agentclientprotocol.com). Claude Code and Codex don't speak it themselves, so each runs through an ACP adapter, at a version tried with clackatoo. The daemon looks for an agent's adapter where clackatoo installs it, `~/.local/share/clackatoo/agents`, then on your `PATH`, then runs it with `npx`, which needs Node.js. While it runs from `PATH` or `npx`, the chat offers once to install it, which makes it start faster and work offline: Claude Code's takes about 620 MB and Codex's about 450 MB, each with its own copy of the agent, installed from a lock file so nothing it depends on can change. With none of the three, the chat asks to install it before it can start. A new build that moves to a newer adapter asks again, and removes the old one once nothing uses it.

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
tasks = false                # don't ask what it runs in the background
```

The daemon reads it whenever a chat starts. `CLACKATOO_AGENT` gives Claude Code another command, for the daemon it's set for.

Each agent runs one process for all its chats, and stops when its last chat closes. Claude Code's adapter also runs a Claude Code process per chat, of about 250 MB.

## What lasts

The daemon keeps chats, as it keeps terminals. Closing the window closes nothing: a chat keeps answering, and the next window shows it.

[Upgrading](~clackatoo/reference/cli#upgrade) the daemon keeps each chat's conversation. An answer under way automatically continues in the same provider session and visible turn after the upgrade, without another message or an open window. The agent is told to check interrupted tool results before repeating actions. Messages still waiting to be sent, including images, remain intact; queued messages follow the continued turn. Unconfirmed steering stays paused for review. Idle, completed and manually stopped chats stay idle, as do chats restored after a crash or ordinary daemon restart. The first upgrade from a build without continuation records still needs another message to restart interrupted work. A chat from a build before chats were kept as turns comes through as one block of its text. Tools from a build before cards come through as lines, without their output or changes. Changes from a build before line numbers come through without them.

If the agent stops on its own, the chat says so, and the next message starts it again, carrying on the same conversation.

A chat keeps which agent it talks to. Chats from before there were other agents talk to Claude Code.

What each turn changed in files lasts through an upgrade, so turns can still be undone, up to about 16 MB per chat. Past that, the oldest turns are too long ago to undo.

A chat that moved between agents keeps the sessions it had with each, and goes back to them after an upgrade too.

When the daemon stops, a chat in a pane comes back with its workspace the next time it starts, on the same session: no agent starts until you look at the chat or send it a message, and then the agent tells the conversation so far, as going on with a [past session](#the-start-page) does, and nothing it tells acts again. A turn you stopped shows as stopped, as Claude Code keeps it; Codex keeps no note of a stop, so a stopped Codex turn shows as one that ended. Claude Code keeps a stop as a message reading `[Request interrupted by user]`, so a message of exactly that, typed yourself, comes back as a stop too. The chat keeps its agent, model, tools and Browser settings; it starts in the agent's own starting mode, and as a chat of its own, not another's subagent. Chats in no pane don't come back, but the start page lists their sessions.

`clackatoo capture` prints a chat as text: each message after a `>`, and what the agent said.

The agent's [plan](#the-agent-s-plan) lasts through an upgrade. After the daemon stops, a chat going on with its session shows the plan again once the agent has told the conversation so far, if the agent tells its plans as it does: Claude Code does.

An upgrade stops the browser; the next page starts it again. An upgrade, or the daemon stopping, ends what agents run in the background, as it ends the agents. After an upgrade the chat shows each lost, "ended by the daemon's upgrade", and the agent is told which: in the turn that goes on, if one was under way, or else in a turn of its own, a notice from clackatoo starting `[daemon restarted]`, once, so it can start again what it still needs. That turn counts towards the 100 notices may start under one of your chats between your messages, as agents can upgrade the daemon themselves; past them it waits for you. Nothing is told of a turn you had asked to stop. A turn an agent took by itself that the upgrade cut off isn't carried on; the agent is told the same way where it was cut off.

## Not yet

- The phone's chats don't colour code. Sideways swipes on a trackpad don't scroll tables: use Shift and the wheel.
- Moving a file in the explorer forgets which of its lines agents changed. Rejecting every change of a file an agent made leaves it empty, rather than in the trash. Reviews of everything since the base stop past about 8 MB of changes: narrow them with `:Review turn` or `chat`.
- A file the agent changed with a command, or that clackatoo couldn't read before the agent wrote it, can't be undone. The chat says which.
- There's no menu of agents to pick from: name one after `:Agent`. Codex's Plan mode can't be changed from the box.
- A file changed both by the agent and by something else at once isn't taken in, and the bar says so.
- Agents can't yet see what you've selected in an editor. Commands run with the agent's own tool, rather than `run_in_terminal`, show their output once they're done.
- The account-usage modal is native-only. Gemini CLI and custom adapters do not yet have read-only subscription probes; the phone keeps its existing per-chat usage detail.
- `/` commands aren't listed or completed as you type them.
- Ctrl+? doesn't list a form's keys yet. What an input method is composing doesn't show in a form's box until it's committed.
- In a past Claude Code session opened again, Read, Grep and Glob rows show what they read or looked for, but not what they found, while clackatoo asks Claude Code what it runs in the background.
- Codex's commands in the background aren't listed: asking for them would change how Codex reports every command.
- You can't click or type into a browser pane yourself.
- An upgrade forgets the cards of subagents that had already closed, so their changes drop out of the chat's and turn's reviews. They stay in `:Review base` and in the gutter, as `chat N`.

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
On the web, open the model selector and choose its **Tools** tab for the same report. Agents can call
`harness_capabilities` themselves. A requested tool catalogue is reported
separately from model acknowledgment: the harness cannot prove a model saw it.
