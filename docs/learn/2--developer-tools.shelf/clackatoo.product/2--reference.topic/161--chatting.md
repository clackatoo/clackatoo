---
title: Chatting with an Agent
type: reference
description: How to open a chat, the start page, typing and sending messages, slash commands, mentioning files, drafts, queuing and steering, and images.
related:
  - clackatoo/reference/agents
---

You open a chat in a pane, type your message in the box along the bottom, and
send it; while the agent works, what you send waits in a queue or steers the
turn under way.

## Opening a chat

| | Does |
|---|---|
| `:Agent codex`, `:\|Agent antigravity`… | Opens a chat with that agent at once: in a new tab, or beside the focused pane, as `:|Terminal` opens a terminal. |
| `:Agent`, `:\|Agent`, `:-Agent`…, <kbd>Ctrl</kbd>+<kbd>B</kbd> `a` | Opens the start page, for a chat in a new tab or beside the focused pane. |

Until its first message, a chat says which agent it has and the folder it
works in, as `Claude Code in ~/project`, and how to start: in normal mode,
"Press i to write a message · <kbd>Shift</kbd>+<kbd>Enter</kbd> for a new
line".

### The start page

Its first row is a new chat, with its agent, model and folder. They start as
the last chat started from the page had them; before that, the agent is the
`agent` option's, the model the agent's own choice, and the folder the focused
pane's. Under it are chats to go on with instead: this workspace's chats in no
pane, such as those agents started in the background, then the sessions the
agent keeps for the folder, its own and clackatoo's, newest first. For a pane
on
[another machine](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar),
or a page opened with `@host`, they're the sessions that machine's agent keeps
in its folder, and going on with one makes the chat there; this needs a daemon
here of protocol 107 or later, and says so otherwise. `:Agent@desk` from a
pane of another machine's, such as your own beside desk's, offers desk's
agents and their models, in desk's home folder, and its sessions there; a
daemon here older than protocol 140 offers the pane's machine's instead.

A chat is listed once, as the branch it shows: the sessions of its other
[branches](~clackatoo/reference/agent-changes#rewinding-and-branches) aren't
listed while it's open, and once it has gone, closed or not brought back after
a restart, the page still leaves them out. The count at the top says how many
it leaves out, as `12 / 12 · b lists 2 branches`, and `b` lists them too, each
saying `branch` after its title, or leaves them out again. Going on with one
makes it a conversation of its own from then on, listed as any other. The
daemon keeps which sessions are branches in `unlisted_sessions`, beside the
`machine` file in its state directory, the latest 1024; a session forked as an
upgrade cut its rewind off can't be told apart, as neither agent says what a
session was forked from, and is listed. A window older than protocol 135 is
sent none of these.

| Key | Does |
|---|---|
| <kbd>Enter</kbd> | Starts the new chat, shows the chat picked, or goes on with the session picked in a new chat. |
| `n` | Starts the new chat, whatever is picked. |
| `j`, `k`, <kbd>Down</kbd>, <kbd>Up</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd>, <kbd>Ctrl</kbd>+<kbd>P</kbd> | The next or previous row. |
| <kbd>Page Down</kbd>, <kbd>Page Up</kbd> | A page further or back. |
| <kbd>Home</kbd>, <kbd>End</kbd> | The new chat, or the last row. |
| <kbd>Tab</kbd>, <kbd>Shift</kbd>+<kbd>Tab</kbd> | The new chat's next or previous picker: agent, model, folder. |
| `l`, `h`, <kbd>Right</kbd>, <kbd>Left</kbd> | The picker's next or previous choice. |
| `b` | Lists the branches of chats that have gone too, or leaves them out again. |
| `/`, then typing | Filters the chats and sessions, as the finder does: <kbd>Backspace</kbd>, <kbd>Ctrl</kbd>+<kbd>U</kbd>, and <kbd>Ctrl</kbd>+<kbd>W</kbd> delete as [the finder's](~clackatoo/reference/workspaces#the-finder) do. |
| <kbd>Esc</kbd>, <kbd>Ctrl</kbd>+<kbd>C</kbd> | Clears what's typed, or closes the page. |

`j`, `k`, `n`, `h`, `l`, and `b` are keys only while nothing's typed: any
other letter starts the filter, and `/` types one of them. A paste starts the
filter too. <kbd>Ctrl</kbd>+<kbd>?</kbd> lists the page's keys.

Going on with a session shows its conversation so far as the chat's turns,
where the agent can tell it, as Claude Code and Codex can. Nothing in those
turns changes your files again, asks you anything, or runs anything: they're
only shown. An agent that can't goes on from where it was, and the chat says
the turns before aren't shown. A message sent while the turns are coming waits
until they've all come. A session another chat has open goes to that chat; one
open in another workspace's chat is refused, saying where it is.

A chat works in the directory a new terminal would start in: the one the pane
it splits is in, or for a new tab, the one the workspace's focused pane is in.
That's where the agent reads and changes files, and runs its commands.

Like any new pane, a chat opens in the window's mode, so after `:Agent` it's
in normal mode: `i` starts typing into it. A chat that's asking something is
always arrived at in normal mode. The first chat starts the agent, which takes
a second or two. Until then the chat says it's starting, and a message sent
meanwhile waits and goes when it can.

An agent runs in a process group of its own, so stopping it, or choosing
Retry, also ends whatever it started, such as the node behind `npx`, and any
background job it started without detaching. `clackatoo stop` ends them too:
running agents get half a second to finish by themselves, then are asked to
stop, and the daemon waits up to a second and a half for them before killing
what's left. An upgrade doesn't: agents
[go on through it](~clackatoo/reference/agents-lasting#through-an-upgrade),
with all they run, unless the upgrade has to stop them. Before Linux 6.9, only
the agent's own process is reached.

## Typing

In insert mode, what you type goes in the box at the bottom.

| Key | Does |
|---|---|
| <kbd>Enter</kbd> | Sends the message, or while the agent works, [queues it](#queuing-and-steering). |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | While the agent works, steers it with the message (see [Queuing and steering](#queuing-and-steering)). |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | Starts a new line in the message. |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | While the agent answers, asks it to stop, unless text in the message is selected, which it copies. When it's not answering, copies what's selected in the message or the conversation. |
| <kbd>Backspace</kbd>, <kbd>Delete</kbd> | Delete a character, or with <kbd>Ctrl</kbd>, a word. |
| <kbd>Left</kbd>, <kbd>Right</kbd> | Move by a character, or with <kbd>Ctrl</kbd>, a word. With <kbd>Shift</kbd>, they select. |
| <kbd>Up</kbd>, <kbd>Down</kbd> | Move by a line. On the box's first line, <kbd>Up</kbd> goes back through the [messages sent](#messages-sent-and-drafts), and on its last, <kbd>Down</kbd> comes forward again. In an empty box, <kbd>Up</kbd> takes the newest queued message back to edit. With <kbd>Shift</kbd>, they select. |
| <kbd>Home</kbd>, <kbd>End</kbd> | Move to the line's start or end. With <kbd>Shift</kbd>, they select. |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> | Opens the finder on the messages sent, to put one in. |
| <kbd>Ctrl</kbd>+<kbd>A</kbd> | Selects the whole message. |
| <kbd>Shift</kbd>+<kbd>Tab</kbd> | Moves the agent on to its next mode. |
| <kbd>Esc</kbd> | Normal mode. There's no program in a chat to need <kbd>Esc</kbd>. |

Clicking in the box puts the cursor there, dragging selects, and a double or
triple click selects a word or a line. A paste goes into the message whole,
lines and all, without sending it. Text can be typed through an input method:
what's being composed shows in the box, at the cursor, until it's committed.

### Slash commands

Agents take commands in a message that begins with `/`: Claude Code's
`/compact` and `/init`, Codex's `/review`, your skills, and prompts your Model
Context Protocol (MCP) servers offer. Type `/` at the start of a message and a
menu opens over the box: the commands the agent says it has, then clackatoo's
own (marked `clackatoo`). It narrows as you type the name, those starting with
what you typed first, each with what it does and, if it takes something after
it, a hint such as `‹branch name›`.

| Key | Does |
|---|---|
| <kbd>Down</kbd>, <kbd>Up</kbd>; <kbd>Ctrl</kbd>+<kbd>N</kbd>, <kbd>Ctrl</kbd>+<kbd>P</kbd> | The next or previous command, round the ends. |
| <kbd>Tab</kbd> | Puts the command picked in the box, with a space after it for what follows. |
| <kbd>Enter</kbd> | The same; or if you've typed the whole name already, sends it. |
| <kbd>Esc</kbd> | Closes the menu, staying in insert mode, until what you've typed changes. |

Selecting a command with the mouse puts it in, as <kbd>Tab</kbd> does. Once
there's a space after the name, the menu closes. The menu lists what the agent
last said it has; a chat that has moved to another agent lists none of the old
one's, and after a daemon upgrade or restart, the agent's own come back once
its session opens again with your next message.

Claude Code's and Codex's own terminal interfaces have commands their adapters
don't, so sent to the agent they'd reach the model as words. Typed as the
whole message, such as `/clear` alone, clackatoo does what each is for itself
instead of sending it, unless the agent says it has a command of that name (in
any case), which is then the agent's and is sent. With words after the name,
as `/help me find why this test fails`, the message is words for the agent and
goes as typed.

| Command | In clackatoo |
|---|---|
| `/clear`, `/new` | Opens the [start page](#the-start-page), to start a new chat. |
| `/resume` | Opens the start page, which lists the agent's past sessions under the new chat. |
| `/fork`, `/branch` | Copies the conversation into a new chat in this pane, as `:fork` does. |
| `/rewind` | Rewinds the conversation to before its last turn, as `:rewind` does, asking whether to undo what it changed in files (see [Rewinding and branches](~clackatoo/reference/agent-changes#rewinding-and-branches)). `u` still undoes only the files. |
| `/context` | Shows how full the agent's context is, as `K` does. |
| `/status`, `/cost`, `/usage` | Opens **Usage & limits**. |
| `/diff` | Opens [`:Review`](~clackatoo/reference/agent-changes#reviewing-its-changes) of what the chat's agents changed. |
| `/model` | Opens the model menu. `/model NAME` picks the agent's model of that name or value, such as `/model sonnet`; a name it hasn't is sent as words. |
| `/export` | Copies the conversation, as text, to the clipboard. `clackatoo capture` prints all of it, older turns not loaded too. |

Others, such as `/exit`, `/help`, `/config`, `/permissions` and `/login`,
aren't sent either: the bar says what to use instead, such as `:q`,
<kbd>Ctrl</kbd>+<kbd>?</kbd>, `:set` or the mode chip, and what you typed
stays in the box. Every other message beginning with `/` goes to the agent as
typed.

A command the agent says it has (or, before it has said, one its adapter is
known to do, such as `/compact`, `/init` or `/review`) goes to the agent on
its own. The first message after a chat moves to another agent, or a profile's
first message, usually goes after the conversation so far or the profile's
instructions; a command waits with those for your next message instead, as
Codex reads a command only when it comes first, Claude Code only when it comes
last, and some of Claude Code's only when they come alone. Any other message
starting with `/`, such as `/usr is full`, goes with them as usual, so a
profile's limits are never left behind.

The phone lists the agent's commands too, as you type `/` and a name, and
selecting one puts it in your message. It doesn't send the commands clackatoo
does itself, typed alone: use those in the window.

### Mentioning files

Type `@` at the start of a word and a menu of the chat's project's files opens
over the box: every file the finder's `<Space>pf` lists, without what the
project's ignore files leave out, walked by the daemon in the background. It
narrows fuzzily as you type the path.

| Key | Does |
|---|---|
| <kbd>Down</kbd>, <kbd>Up</kbd>; <kbd>Ctrl</kbd>+<kbd>N</kbd>, <kbd>Ctrl</kbd>+<kbd>P</kbd> | The next or previous file, round the ends. |
| <kbd>Tab</kbd>, <kbd>Enter</kbd> | Puts `@path` in the box in place of what you typed, from the chat's folder, with a space after it. |
| <kbd>Esc</kbd> | Closes the menu, staying in insert mode, until what you've typed changes. |

A path with a space in it is quoted, as Claude Code quotes one:
`@"docs/read me.md"`. A mention can name lines, `@src/parser.rs#L12-L40` (or
`#L12` for one), and you can type mentions by hand: a word starting with `@`
names a file relative to the chat's folder, or from the project's root, or an
absolute path or one starting `~/`. Punctuation ending a sentence after it
isn't part of it.

When the message is sent, the daemon finds each file it mentions and sends it
to the agent with the message, as Agent Client Protocol (ACP) context:

| What's mentioned | Claude Code | Codex, Gemini, and others |
|---|---|---|
| A saved file | A link to it, which it reads with its Read tool | Its text, if it's at most 64 KiB; a link if bigger |
| A file open in an editor with changes not saved | Its text as it is in the editor, at most 64 KiB; a link if bigger, which reads the saved file | The same |
| Lines of a file (`#L12-L40`) | Those lines' text, as they are in the editor if it's open, at most 64 KiB; a link if more, or if the file isn't open and is over 8 MiB | The same |
| A folder, or a file that isn't text | A link | A link |

The text sent with one message is at most 256 KiB in all; files past that go
as links. An agent that doesn't say it takes files' text gets links alone.
Claude Code gets links to saved files because it edits only files its own Read
tool has read, so text in the message would be read again before any edit;
Read also gives it line numbers. Both agents show what they were given as
`[@name](file:///…)` and a `<context>` block, and neither reads lines from
anywhere but the link, so the lines go in it: `file:///…/parser.rs#L12-L40`.

A mention of a file outside the chat's project (its repository, or else its
folder, with links followed) goes to the agent as words, not the file, and the
chat says so above the box; one that names nothing at all is only words. A
message that begins with one of the agent's slash commands is sent as typed,
its mentions as words, as the agents look for a command in its only or last
part; one that only begins with a `/`, as `/usr is full @a.rs` does, is words,
and its mentions go as files. Steering sends words alone, so a message steered
in that mentions a file is sent whole instead, stopping the turn: the button
says **Stop & send** for a message that seems to mention one (a path with a
`/` or a `.`), and the daemon decides by whether it finds the file.

The files are read in the background, never holding up the daemon: the message
waits for them, and so does what you ask of the chat after it, but Stop and
answers to the agent don't, and Stop drops the message. A read that takes more
than 2 seconds, as on a network file system that hangs, is given up on: the
message goes as words, and the chat says it couldn't read the files in time.
At most 8 messages' files are read at once; past that, a message goes as
words, and the chat says so. A file is read only if it's a plain file in the
project when it's opened, never a link, a pipe or a device swapped in after it
was checked, and never more of it than is sent.

In the conversation, mentions are chips in the link color. Select one, or put
the cursor on it in normal mode and type `gf` (or `gx`), to open the file, at
its first line if it names lines; `gf` on a link or path in the agent's words
opens it too. Going on with a past session shows its mentions as chips again,
not the text that was sent with them.

From an editor, `<Space>a` in visual mode, or `:'<,'>Ask`, puts the lines
selected at the end of the message in the chat beside the editor, or the one
used last in the tab, as `@src/parser.rs#L12-L40`, and moves you to its box in
insert mode, to say what to do with them. In normal mode, `<Space>a` (or
`:Ask`) mentions the whole file. The lines' text is read when you send the
message, so it's what the editor shows then. The key is a binding like any
other: unmap `<Space>a`, or map another key to `:Ask<CR>`, with
`client.keys.map` (see [Config](~clackatoo/reference/config)).

The phone shows mentions in sent messages as chips; typing `@path` by hand
there works the same, as the daemon finds the files, but it has no menu.

One message is answered at a time. While the agent answers, the button in the
box's corner is a stop button, and <kbd>Enter</kbd>
[queues](#queuing-and-steering) what you typed for when it's done (with a
daemon too old to queue, it stays in the box). Selecting the button with
nothing being answered sends the message, as <kbd>Enter</kbd> does.

### Messages sent and drafts

The box remembers what you've sent in the chat, as a shell remembers commands,
and what you've typed but not sent.

| Key | Does |
|---|---|
| <kbd>Up</kbd>, on the box's first line | The message sent before, of those that start with what's typed: in an empty box, every one, newest first. The cursor goes to its end. |
| <kbd>Down</kbd>, on its last line | The message sent after; past the newest, back to what you had typed. |
| <kbd>Up</kbd>, in an empty box with messages queued | Takes the newest queued message out of the queue and puts it in the box to edit, as Claude Code's <kbd>Up</kbd> does. |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> | Opens the finder on the messages sent, newest first: type to narrow them, and <kbd>Enter</kbd> puts the one picked in at the cursor. `:Prompts` opens it too. |

<kbd>Up</kbd> on a message's other lines moves the cursor up a line, so on a
message of several lines, <kbd>Up</kbd> goes to its first line before it goes
back to the one before. Typing anything forgets where <kbd>Up</kbd> had gone:
the next <kbd>Up</kbd> goes back from what's typed then. In normal mode, `k`
and `j` still move through the conversation.

The messages <kbd>Up</kbd> recalls are your own, in this chat: not another
chat's agent's, nor clackatoo's notices. They're worked out from the
conversation itself, so every window and the phone have the same ones, and
they last through upgrades, and after a restart once the chat has told its
conversation again. Up to the newest 200 are kept, each once, and at most 128
KiB of them, those over 16 KiB left out. A message with images recalls its
words. Messages from other chats aren't recalled.

The pencil on a queued message, or <kbd>Up</kbd> in an empty box, has the
daemon move it out of the queue and into the chat's draft in one step, and
puts it in your box before what you've typed there, so it's never both waiting
and being edited, and never only in a window: if the window closes first, the
next to open the chat starts with it. What's typed in other boxes stays in
them. If it was sent meanwhile, the bar says so. A queued message with images,
or one that moves the chat to another agent, has no pencil and isn't taken
back: send or remove it. Taking messages back needs a daemon that keeps
drafts.

What you've typed but not sent is kept by the daemon too, as the chat's draft:
a window, or the phone, tells it as you pause typing, as the box empties, and
as it closes, and the daemon keeps the last told. A box takes it only while
it's empty and you haven't typed in it there: as a window first shows the
chat, after closing the window, after an upgrade, on another machine, or as
the phone's page loads (over what the page kept itself, unless you've typed
since it loaded). Typing isn't mirrored from box to box: a box you've typed
in, or emptied, is never changed by what's typed elsewhere, nor by a
reconnect, and with two boxes typed in at once, the last told is what the next
window starts with. Sending a message, from any box, empties the draft, and
every other box holding only that message empties too; a box holding anything
else keeps it. Only the text is kept, up to 64 KiB: images attached stay in
the window. A full daemon restart doesn't keep it, and a chat on another
machine whose daemon is older keeps no draft: the box is the window's alone
there, and the phone keeps its own copy.

## Queuing and steering

While an agent works, <kbd>Enter</kbd> queues what you've typed, and
<kbd>Shift</kbd>+<kbd>Enter</kbd> starts a new line as usual. Messages waiting
to go share a small panel above the box, headed `Queued`, or `Queue paused`
while they're held, with how many there are. They go in order, each after a
turn that ends well. Each has a pencil to
[take it back into the box](#messages-sent-and-drafts) and edit it (but not
one with images, or one that moves the chat to another agent), ↑ to send it
now, and × to remove it. Sending one now while a turn is under way steers that
turn; with nothing under way, it sends the queue on again.
<kbd>Ctrl</kbd>+<kbd>Enter</kbd> steers with what's in the box. While you type
during a turn, the box shows a small **Steer** or **Stop & continue** button
and a square Stop button.

With the Claude Code or Codex adapter clackatoo installs, **Steer** adds a
message of words to the turn under way without stopping it. Codex's has
clackatoo's steering backported to it; a custom adapter, or one found on
`PATH`, without that patch stops and continues instead. So does a message with
images, and one that moves the chat to another agent. **Stop & continue**
stops the turn, waits for it to end, then sends the message. The button says
which it will do. A message that seems to mention a file says **Stop & send**,
as it can only go whole (see [Mentioning files](#mentioning-files)). If the
turn ends while a steer is on its way, the message goes once, as the next
turn. If it's unclear whether the agent took a steer, the message waits,
paused, so you can check the conversation before sending it again.

Stop, or a turn that fails, pauses the queue: ↑ sends it on again, and ×
throws a message away. The daemon keeps the queue, with its text, images, and
agents to move to, so every window, here or on another machine, sees the same
one. It lasts through an upgrade: messages go after the work the upgrade
[carries on](~clackatoo/reference/agents-lasting#what-lasts), and a queue that
was paused, or a steer not yet confirmed, stays paused. A full daemon restart
doesn't keep it. A queue holds up to 8 messages or 32 MiB, each message's text
up to 64 KiB, and its images within the [usual limits](#pasting-images).

## Pasting images

Copy a screenshot or image to the clipboard, then press
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> in a chat. A thumbnail appears
above the text in the message box, and above the sent message in the
conversation. Add text if you like, then press <kbd>Enter</kbd> to send both;
an image can go on its own too.

Select the × on a thumbnail to remove it, or press <kbd>Backspace</kbd> while
the message box is empty to remove the last one. You can prepare attachments
while the agent answers; they stay in the draft until you send. Text on the
clipboard still pastes as text.

A message holds up to four images, at most 16 million pixels each and 8 MiB of
encoded image data in total. An agent that does not accept images reports that
clearly. The full image is sent to the agent, whose session keeps it.
Clackatoo keeps small previews (up to 256 pixels on a side) across window
reconnects and daemon upgrades, including on remote connections. Each chat
keeps up to 4 MiB of recent previews; older messages retain their image
labels. Images sent before previews were supported, or history loaded anew
from an agent session after a full daemon restart, show labels only. Switching
to a different agent carries the labels with the conversation, so paste the
image again if that agent needs to see it.

## Viewing images

Select an image in the box, in a message sent, or in a tool's result, and it
opens in a viewer over the chat. <kbd>Esc</kbd>, a key that goes to normal
mode from anywhere (<kbd>Ctrl</kbd>+<kbd>;</kbd>,
<kbd>Shift</kbd>+<kbd>Esc</kbd>, or <kbd>Ctrl</kbd>+<kbd>&#92;</kbd>), or its
× closes it, in either mode, back to your draft in the mode you were in.
Selecting elsewhere doesn't close it. While it's open, typing, pasting,
scrolling, and the mouse's buttons on the chat behind it do nothing. The × on
a thumbnail in the box still removes that image.

The viewer fetches the original image as it opens, and says its size in pixels
once it has it. An image in the box is the window's own; one in a message or a
tool's result comes from the daemon, for a chat on another machine too. What
the chat sends the window as it changes still carries only small thumbnails.
Each chat keeps the originals of its newest images, up to 128 of them and 32
MiB encoded, through reconnects and upgrades. An older image whose original
was never kept, or has been let go, says
`Original unavailable · thumbnail only`. A full daemon restart keeps no
originals. Images are decoded in the background, at most 16 million pixels,
and 8192 on a side.

## Images returned by tools

When a tool returns an image, a thumbnail appears beneath its entry in the
conversation, even when the turn's work is folded. Open the entry to give the
preview more room. This works with Codex's View Image and other agents,
including Claude, when their adapter returns inline image data or a link to a
local image file.

PNG, JPEG and BMP results are supported, up to four per result, with files
limited to 8 MiB and images to 16 million pixels. The daemon reads local files
on its own machine and sends small previews to the window, so remote chats
work too. Web URLs are not fetched. Previews share the chat's 4 MiB history
allowance with pasted images; older results keep their labels. Newly received
previews survive reconnects and daemon upgrades, but history replayed after a
full restart shows labels only. The web client shows the previews too.
