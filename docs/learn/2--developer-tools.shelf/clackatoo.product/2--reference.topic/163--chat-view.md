---
title: What a Chat Shows
type: reference
description: How a chat lays out an agent's answers, its tool and command cards, a command's output in a terminal, background commands, plans, failures, questions and forms, notifications, and the normal-mode keys, yanking, and search that read it.
related:
  - clackatoo/reference/agents
---

Your messages are bubbles on the right. The agent's answers run the width of a
column down the middle of the pane, with Markdown laid out as it's meant to
look: headings in bold, lists, quotes, links in blue, code in the terminal's
font on a background of its own, colored as an editor colors it when its
language is installed, and tables as a grid. Code, quotes, and cards all start
at the column's left edge.

Long conversations open on recent turns. Scroll toward the top to fetch
earlier turns a page at a time, keeping the message you're reading in place.
This works in the window and web app, including chats on connected machines.
Reconnecting retains the range you've loaded. Live output and file actions
keep their original turn identities; to undo a specific older turn by number,
load that turn first. A daemon older than protocol 104 sends the full
conversation.

## Normal mode

In normal mode, a solid cursor moves through the conversation's text, and the
view follows it. Coming into normal mode puts it on the last line in view,
unless it's in view already. Keys are read as a terminal's and an editor's
are, so a count goes before a move (`3j`, `2w`), a command half typed shows at
the start of the bar, and <kbd>Ctrl</kbd>+<kbd>?</kbd> lists them:

| Key | Does |
|---|---|
| `h` `j` `k` `l`, arrows, `+` `-` | A character, or a line. |
| `w` `b` `e` `ge`, `W` `B` `E` `gE` | By words, or by anything between blanks; `ge` and `gE` go to the previous word's end. |
| `f` `t` `F` `T`, `;` `,` | To a character on the line, or right before it, back with `F` and `T`; `;` again, `,` the other way. |
| `0` `^` `$`, `g0` `g$` | The visual row's start, first non-blank, or end. |
| `_` `g_`, `N\|` | First or last non-blank on the row; column N, counting from 1. A count before `_` or `g_` takes that many rows, including this one. |
| `%`, `(` `)` | Matching bracket or sentence, within the block. A count before `%` goes to that percentage of the block's lines. |
| `H` `M` `L` | Top, middle or bottom of the visible text, respecting `scrolloff`; a count before H or L moves inward from that edge. |
| `zt` `zz` `zb` | Put the cursor's row at the top, middle or bottom of the view. |
| `{` `}` | The block before or after. |
| `gg` `G` | The start or end of the chat; with a count, the start of that turn, as `3G`. |
| `gf` `gx` | Opens the file a mention under the cursor names, or the link or path there (see [Mentioning files](~clackatoo/reference/chatting#mentioning-files)). |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> <kbd>Ctrl</kbd>+<kbd>U</kbd>, <kbd>Ctrl</kbd>+<kbd>E</kbd> <kbd>Ctrl</kbd>+<kbd>Y</kbd>, <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>Page Down</kbd> <kbd>Page Up</kbd> | Scroll half a view, a line, or a page of the view less two lines, as far as in any pane, with a count. The cursor stays in view. |
| `v` `V` | Select characters, or lines. `o` goes to the other end, <kbd>Esc</kbd> stops; `gv` restores the last selection, or swaps it with the current one. |
| `y` and a motion or object, `yy` `Y` | Yanks what the motion moves over, the object, whole lines, or to the line's end, into the registers (see [Yanking](#yanking)), `Y` as `y$`, as Neovim's. In visual mode, `y` yanks what's selected and `Y` its lines. |
| `"a` before `y` | Yanks into register `a`, or onto its end with `"A`. |
| `/` `?`, `n` `N` | Searches the chat's text forward or back, and again (see [Searching](#searching)). |
| `za` `zo` `zc`, <kbd>Enter</kbd> on a "Worked for" line | Opens or folds the turn's work, or on a subagents' line, their card. `zR` and `zM` open or fold every turn and every card. |
| `go` | Shows what the command whose card is under the cursor printed in a terminal below the chat (see [A command's output in a terminal](#a-command-s-output-in-a-terminal)). |
| `j` `k` in a table | The cell below or above. |
| `zl` `zh`, `zL` `zH` | Scroll the table the cursor's in sideways: a few characters, or half the column. |
| `u` | Undoes what the turn under the cursor changed in files, if it's in view, or else the last turn's, keeping your edits since. The bar says which. |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> | Makes it again. |
| `dG` | Rewinds the conversation to before the turn under the cursor, or the last (see [Rewinding and branches](~clackatoo/reference/agent-changes#rewinding-and-branches)). Other `d` and `c` commands say what the chat takes instead. |
| `cc` `S` | Edits your message under the cursor: rewinds to before it, and puts it in the box to change and send. |
| `[b` `]b` | The branch before or after where the conversation's branches part, at the turn under the cursor or the nearest above; with a count, that many. |
| `K` | Opens Usage & limits. <kbd>Escape</kbd> or <kbd>Ctrl</kbd>+<kbd>;</kbd> closes it. |
| `R` | Sends the last message again, if its turn failed (see [When a turn fails](#when-a-turn-fails)). |
| `gl` | Signs in to the agent again in a new tab, if the last turn failed for want of it (see [When a turn fails](#when-a-turn-fails)). |
| `gC` | Starts a new chat with the same agent in the same folder, in a new tab, if the last turn hit the model's limit. |
| `q` `@`, `m` `'` and `` ` ``, Ctrl+O Ctrl+I | Macros, marks and jumps, as in every pane (see [Macros and marks across panes](~clackatoo/reference/window#macros-and-marks-across-panes)). |
| `i`, `a`, `I`, `A`, `o`, <kbd>Enter</kbd> | Back to insert mode. |

The mouse wheel scrolls in either mode. While you're at the end, the view
follows the answer as it grows. Scrolled up, it stays put, and the bottom bar
says so.

### Yanking

`y` is Vim's yank operator. It takes any motion the chat moves by, with Vim's
rules for what each takes: `yw` up to the next word, `ye` and `y$` to the
word's or the line's end, `yfx` up to `x`, `yj`, `yk`, `ygg` and `yG` whole
lines. `yy` takes the line, and `Y` the rest of it, as `y$` does, as Neovim's;
a count takes more, as `3yy` or `y2w`. A chat's lines are each block's own
lines, as it breaks them, not the rows they wrap onto: the cursor moves by
rows, but `y$`, `y0`, `yfx` and `yj` go by lines, and `yw` on a line's last
word stops at its end, as Vim's. Text objects are Vim's, found in the block of
text the cursor's in: `iw` `aw`, `iW` `aW`, `is` `as`, `ip` `ap`, `i"` `a"`,
`i(` `a(` (and `ib` `ab`, as in Vim) and their kin, `iB` `aB` for braces, and
`it` `at`. Four are a chat's own:

| Object | Is |
|---|---|
| `ix` | The block of code the cursor's in. |
| `ax` | The same with its fence and language, as Markdown writes it. |
| `im` | The message, answer or card the cursor's in, as it came: an answer as its Markdown. |
| `am` | Its whole turn: the message, and all the agent said and did for it. |

All four are whole lines. `ic` and `ac` are a class, as in an editor, which a
chat hasn't got, so they do nothing. The cursor goes to the start of what's
yanked.

In visual mode, `iw`, `aw`, brackets, quotes, sentences, paragraphs and tags
use the same selection rules as the editor; repeated objects extend the
selection where Vim does. `ix` selects the visible code block, `im` the
visible message or card, and `am` the visible turn. Visual selections contain
the displayed text; use `yax` or `yim` directly to copy Markdown fences or
source Markdown.

Word motions and objects distinguish CJK scripts, Latin text and punctuation
as Vim does. Code blocks use the daemon's language word characters, including
custom packs, once their metadata arrives with highlighting; a daemon older
than protocol 125 uses plain Vim word characters. Mouse double-clicks in prose
follow Unicode word boundaries; terminal double-clicks retain whole paths and
URLs.

What's yanked goes into the window's registers as an editor's yank does: into
the register named before it, with `"ayy`, or onto its end, with `"Ayy`; `"_`
keeps nothing. With no register named, it goes into the unnamed register and
`"0`, and onto the clipboard if the `clipboard` option says. `p` in an editor
puts it, lines as lines. Keys typed after a yank or a search, by a macro, a
binding or quickly by hand, wait for it, so `nn` goes twice and `/x<CR>yiw`
yanks the word found.

### Searching

`/` types a search in the bottom bar, and <kbd>Enter</kbd> goes to the first
match after the cursor; `?` searches back. <kbd>Enter</kbd> with nothing typed
searches for the last search again. `n` searches again the same way and `N`
the other way, round the chat's ends, and the bar says when it goes round; a
count goes that many matches, before `n` or before `/`, as `3/x`. The search
is edited as the command line is: <kbd>Up</kbd> and <kbd>Down</kbd> bring back
searches typed before, and <kbd>Ctrl</kbd>+<kbd>R</kbd> and a register's name
puts in what the register holds. The pattern is Vim's, as an editor's search
reads it, and its case follows the `ignorecase` and `smartcase` options.

A search looks through the text the chat shows, block by block: messages, the
agent's words and thinking without their Markdown, and the lines of cards. A
match is never across two blocks. It looks in folded work too, as Vim's search
opens folds: a match in a turn's folded work opens the work, and one in a
tool's folded card opens the card. Only the turns the chat has loaded are
searched; scroll up to load older ones. The cursor goes to the match, and
every match in view is lit up, as Vim's `hlsearch` lights them, the match the
search, `n` or `N` went to brighter than the rest, wherever the cursor goes
after, until <kbd>Esc</kbd> (but one leaving visual mode) or `:nohlsearch`
(`:noh`), and again at the next search, `n` or `N`. Only the text drawn is
looked through for them, with 4 KiB around it, and at most 512 are lit in a
block, and only again when what's drawn or the search changes. With the
[option](~clackatoo/reference/config#options) `hlsearch` off, only the match
found is lit, until <kbd>Esc</kbd> or `:noh`.

While you type the search, the cursor goes to the match it would find from
where you started, opening the work or card it's in, and its matches are lit,
as Neovim's `incsearch` shows them, in an editor too (see
[Shown as you type](~clackatoo/reference/editor#shown-as-you-type)). It's
worked out once after each key, as one step of a search, within the same
bounds. <kbd>Esc</kbd> puts the cursor, the view, and anything it opened back;
<kbd>Enter</kbd> keeps them, and is the search. With the
[option](~clackatoo/reference/config#options) `incsearch` off, nothing moves
until <kbd>Enter</kbd>.

A card shows at most 1,000 lines of output: a match only in lines it leaves
out is said to be there, rather than not found.

A long chat isn't laid out to be searched: each message, answer and card is
first read as the chat holds it, and only one that might hold the next match
is opened and laid out. Each step to the next match reads at most 64 MiB of
text and lays out at most 32 turns or 4 MiB of what they hold; if it would
need more, it stops and says so.

## What the chat shows

Below each message you send, what the agent did with it, in order:

- its words
- its thinking, if it shows any, quieter and slanted
- each tool it uses, on a line of its own: `○` while it runs, then `✓` once it
  has finished, or `✗` if it failed
- each command it runs and each change it makes to a file, in a compact
  expandable row

Tool rows start collapsed, with a single-line summary and an expand arrow.
Long titles end in an ellipsis, and file tools can show only the basename.
Opening a row reveals the complete title. Commands start with `$` and show
their output beneath; a failed command keeps its last 8 output lines visible
even when collapsed.

A change's card has what the agent did at its head, such as `Edit Cargo.toml`,
and how many lines it adds and removes, such as `+3 −1`. Under that come the
lines, with those added on green after `+` and those removed on red after `−`.
A new file is all added. A card shows the part of the file changed, not the
whole file. Once the change is made, Claude Code also gives 3 lines either
side of it, and where in the file it is.

Lines are numbered by where they are in the file: added and unchanged lines by
where they are now, and removed lines by where they were. Before the change is
made, as while the agent asks to make it, an edit's lines have no numbers yet.
A new file is numbered from 1.

Select a row's header to open or close it. In normal mode, <kbd>Enter</kbd> or
`za` on a tool toggles it, `zo` opens it and `zc` closes it. Open output and
diffs wrap, with up to 1,000 lines shown. Long open cards also have a Collapse
button at the bottom. Whether a card is open belongs to the window. Copying
the whole tool keeps the complete command and retained output.

The daemon keeps the last 32 KB of a command's output, and the first 256 KB of
each side of a change.

Once the agent has answered, everything before its answer folds under one
line: `Worked for 42s ›`. Select it, or press <kbd>Enter</kbd> on it, to open
it, and again to fold it. While the agent works, the turn is open and the line
says `Working`, or `Waiting for you` while it asks you something. When it's
done, it folds itself, unless you've opened or folded it yourself. A turn
that's only words has no line. Whether a turn is open belongs to the window. A
turn the agent didn't finish says why: `Stopped.`, or what went wrong. The tab
is named after the title the agent gives the conversation.

A table is a grid: its head in bold over a line, each column as wide as its
widest cell and lined up as the table says. One wider than the column scrolls
sideways, with `‹` or `›` at a cut edge: <kbd>Shift</kbd> and the wheel over
it, `zl` and `zh` in normal mode, or the cursor going to a cell out of view.
Copied, its cells are a tab apart and its rows a line.

Code in a fenced block that names its language is colored by the daemon's
grammars, as `K`'s is, once the block's closing fence has come; until then,
and in a language that isn't installed, it's plain. Code in the agent's
thinking stays plain.

While the agent works, a line right above the box says so, wherever the chat
is scrolled to: three dots pulsing in clackatoo's color, what the agent is
doing — `Starting Claude Code`, `Loading the session`, `Thinking`,
`Running: cargo test`, `Running a command`, `Reading Cargo.toml`, `Writing`,
or `Working` between tools — and how long the turn has been going, as `· 12s`,
from the column's left. A notice goes above it, not in its place, and in a
narrow pane what it's doing is cut short before the seconds are. The dots move
only while the chat has the keyboard in a focused window and the agent has
been heard from in the last few minutes; otherwise they step once a second.
Nothing moves once the agent stops, while it asks you something, or while the
daemon is out of reach. Its tab, and its workspace in the top bar, show `⋯`
(see [the window](~clackatoo/reference/window#workspaces-tabs-and-panes)).

When you send a message more than half an hour after the turn before it ended,
a line across the chat says when: `14:32` today, `Yesterday 09:10`, the day of
the week for the five days before that, such as `Monday 18:05`, and the date
before that, such as `6 Oct 14:32`, with the year if it was another year. The
time is your machine's, on a 24-hour clock.

While the mouse is over a turn the agent has finished, `Copy` shows under its
answer and copies the answer to the clipboard as the Markdown it came as, as
`yim` on it yanks it in normal mode. It says `Copied` until the mouse leaves
it. When the turn changed files, it shares the line with how many and the
buttons that undo and redo them.

### A command's output in a terminal

A command's card shows the end of what it printed as plain text. To read all
of it as a terminal shows it, select the `↗` at the right of the card's head,
or press `go` with the cursor on the card in normal mode. A terminal opens
below the chat, on the chat's machine, with what the command has printed so
far, and goes on showing what it prints as the agent sends it. When the
command ends, a last line says how, as `[exited 0]`, or `[ended]` if the agent
didn't say. Colors, progress bars and anything else a program draws in a
terminal show as they would in a shell, and it has scrollback, `/` search and
yanks as any terminal has. Its tab is named after the command, and nothing the
command prints renames it, or the window, or says which folder its paths are
in; links it prints open as any terminal's do. Before the last line, whatever
the command left on that only a program running there would want is put back:
the mouse selects again, the alternate screen is left, and pastes and keys are
plain. `:q` closes it; the command goes on. Each time you select it, another
opens.

Once nothing more will come, the pane's terminal has ended, as `clackatoo ls`
says: `exited` with the command's code, or `killed (signal 0)` when it was cut
off, as when its chat closed, its turn was rewound away, or the daemon
upgraded.

No program runs in it: the daemon writes what the agent says the command
printed, as it says it, so it can't be typed into, and a key or a paste in it
only says so in the bar. Agents run their commands themselves, in their own
processes, and send their output to clackatoo over ACP as they go, which is
all this shows: Codex sends it as it comes, and Claude Code all at once, as
the command ends. Nothing opens by itself.

The daemon keeps the last 64 KB of what each command printed as it came,
escapes and all, and 512 KB for each chat, letting the command that printed
longest ago go first. A pane opened on one whose start wasn't kept says so on
its first line. Through an upgrade, an open pane keeps what it shows, but
nothing more reaches it: one whose command still runs says so on its last
line, and the card goes on showing the rest. What was kept isn't handed over
either, so a pane opened after an upgrade starts with the card's plain text. A
restart doesn't keep these panes. A daemon older than protocol 136 can't show
one, and the bar says so.

### What it runs in the background

Claude Code can leave a command running in the background, such as a dev
server or a long build, and carry on with the turn. A shelf above the box
lists what it runs: `In the background · 1 running`, then a line for each with
its command and what it's for, how it's going, and for how long, as
`● npm run dev · Start the dev server — running 2m 4s`. The seconds count
while one runs. A watcher it started says `Watching:`. When one ends, Claude
Code might [go on by itself](#turns-the-agent-takes-by-itself) to look at it.

The square at the end of a running line stops it, through the agent. From the
keyboard, `:stoptask` stops the newest running, and `:stoptask 2` the second
line. A watcher can't be stopped from here; ask the agent. The shelf lists at
most three, those running first, then those that ended, newest first in each,
and its heading says how many more there are, as `· 2 more`, or `all ended`
once none runs. A chat keeps 16 at most, letting the oldest that ended go
first.

#### What a background command printed

Select a line on the shelf to read what its command printed, or type
`:taskoutput`, for the newest that can show it, or `:taskoutput 2` for the
second line. A terminal opens below the chat, on the chat's machine, showing
the last 256 KiB of the output and following it as it grows. It's an ordinary
terminal running `tail -F` on the output, so colors and progress lines show as
they would in a shell, and it has scrollback, `/` search and yanks. Closing
it, with `:q`, leaves the command running. A line that can be opened lights up
under the mouse. Selecting it opens the task it's on, below its own chat,
whichever pane has the focus. Like any terminal's program, what the command
prints can set the terminal's title, and copy to your clipboard as the
[`osc52`](~clackatoo/reference/config) option allows (`copy` by default).

This is as much as Claude Code shares over ACP. Its adapter never sends a
background command's output: it says only where Claude Code writes it, a
`tasks/ID.output` file under `$TMPDIR/claude-UID/` on the daemon's machine,
which Claude Code's own TaskOutput tool reads. The agent runs where the daemon
does, so the terminal can follow that file, whichever machine the window is
on. clackatoo takes the path only in that shape, for that task, in Claude
Code's own temp area (`claude-UID` in `$CLAUDE_CODE_TMPDIR`, `$CLAUDE_TMPDIR`,
`$TMPDIR` or `/tmp`, as the daemon has them), and only if what's there is a
plain file, not a link, a pipe or a device; anything else the agent names is
ignored. `tail` is run with no shell, as `/usr/bin/tail` or `/bin/tail` when
the daemon is on the window's machine, or else as `tail` on the daemon's
`PATH`. So there's nothing to follow:

- for a task whose agent didn't say where it prints, such as a turn it took by
  itself, or another agent's;
- for an agent whose command gives it another temp directory than the
  daemon's;
- for a task from before an upgrade, as the path isn't handed over;
- with a daemon older than protocol 120, which the bar says.

The bar says so, and the agent can still be asked to show the output itself.
The phone lists the lines without opening them.

A line that has ended says how: `✓ done`, `✗ failed`, or `■ stopped`, with how
long it took and what Claude Code said of it. It stays until your next
message. Nothing is left showing as running once it can't be: if the agent
stops, what it ran stops with it, and its line says `ended with its agent`.

A subagent's card says how many things its chats run in the background, and
when open, has a line under each chat for what it runs. A chat with something
running in the background is never let go while it's idle in the background,
as that would end it.

Codex's adapter lists what Codex runs in the background (its `/ps` in a
terminal) only to a client that asks as AIR does, and then leaves every
command's output out of its row, so clackatoo doesn't ask. It shows anyway: a
command Codex leaves running as its turn ends keeps its row open until the
command ends, so a command that ran in a terminal of its own, still running as
a turn ends well, is listed on the shelf too, with its command and for how
long, as `● npm run dev — running 2m 4s`. Its row in the turn stays `○` rather
than failing with the turn, and goes on showing what the command prints,
wherever the conversation has got to; when it ends, the row says how, with its
exit code, and so does its line, as `✓ npm run dev — done in 5m: exit code 0`.
Such a line has no stop button, as Codex's adapter takes no stop from a client
that doesn't ask as AIR does, and nothing for `:taskoutput` to follow, as
there's no file: the row is where its output is. Ask Codex to stop it. A
command Codex ends without saying so stays on the shelf until the agent stops;
one of a turn you stopped, or that failed, is taken to have ended with it.
Leaving the branch it ran in, or the agent, lets it go, as
`ended with its agent`. The phone lists the same lines, without the buttons
that stop them.

Claude Code's adapter says so only to a client that asks as JetBrains' AIR
does, and to such a client it reports some things differently:

- It sends a read or a search of files without what it found. clackatoo asks
  it for the messages it hands the model too, and shows what Claude Code's
  Read, Grep and Glob found from those while they run: in a past session
  opened again, those rows don't show it. (Claude Code 2.1.310 has no Grep or
  Glob of its own here: it searches with commands, whose output shows anyway.)
- It keeps the words and thinking of Claude Code's own subagents (its Task
  tool) to itself unless asked for them: clackatoo asks, and shows them
  [under the Task's card](#claude-code-s-own-subagents).
- It doesn't send the same task list twice in a row, which changes nothing
  shown.
- A command moved to the background doesn't go on showing its output in its
  row once it has moved: the shelf says how it's going, its line
  [follows what it prints](#what-a-background-command-printed), and Claude
  Code reads it when it ends.
- A question it asks you with its own AskUserQuestion tool is titled
  `Asking for your input`, not by the question; the question shows under it.
- A failed call of one of its tools for other agents, such as SendMessage,
  shows its error as plain text, not as code.
- clackatoo also asks for AIR's session failures, so Claude Code says when it
  [tries a request again](#when-a-turn-fails), and why a turn failed by its
  kind. A turn that fails then ends as if well, with the failure beside, which
  clackatoo reads; and Claude Code's error for a failure leaves out the
  provider's words, which come with the failure instead.

Commands' output, file changes, and images show as usual. To have none of this
and no shelf, set `tasks = false` for Claude Code, beside its `command`, in
[`agents.toml`](~clackatoo/reference/agent-settings#which-agent).

### Claude Code's own subagents

With `:set claudetask`, Claude Code can hand work to a helper of its own with
its Task tool, which it calls Agent. What the helper says and the tools it
uses go under the Task's card, not into the turn as Claude Code's own. The
card starts folded, with a line under its title saying what the helper is
doing now and how many tools it has used, as
`↳ Reading src/parse.rs · 3 tool uses`, and once it's done,
`↳ Done · 12 tool uses`. Every file it set out to change, command it started
and other tool that might change something, as an MCP server's, is counted
there too, however it went and its own helpers' included, as
`· edited 2 files · ran 1 command · used 1 other tool`: nothing that might
have changed something is out of sight. When Claude Code asks you about one of
the helper's tools, as before an edit in Manual mode, the cards it's under
open, here and on the phone, so you see what you're asked about. Open it as
any card, by selecting its head, or <kbd>Enter</kbd>, `za` or `zo` on it in
normal mode, and the helper's words and tool rows show under it, indented, in
the order they came. `za` or `zc` in the helper's words folds the card again.
Searching finds what's folded in a card and opens it; yanking and motions go
through what shows. The line above the box says what the helper is doing while
it works.

Each Task keeps the last 200 of its helpers' steps, its helpers' own helpers
counted with them, and 1 MB of them; past either, the oldest go, each with all
that's under it, and a line at the top says how many, as
`12 earlier steps not shown`. What comes later for a step that went is dropped
and counted in that line. Words or tools marked as a helper's that's not in
the turn show on their own, never run on into Claude Code's answer.
clackatoo's own rows, such as what an agent asks to type, are never folded
under a card. The phone folds the helper's work under the card the same way. A
window older than protocol 117 shows the helper's work as Claude Code's own.

The work survives an upgrade. Claude Code 2.1.310 runs a Task in the
background and keeps its helper's transcript apart, so a past session opened
again shows the Task's card without what its helper did. What a helper does
after the turn that started it has ended shows in the later turn, on its own.

### The agent's plan

On a long task, an agent keeps a to-do list: Claude Code's TodoWrite, Codex's
plan. It shows on a shelf above the box, over what the agent runs in the
background, folded to one line: how many steps are done, and the one being
worked on, or else the next to do, as `Plan · 2/5 done · ◐ Write the tests`.

Select that line, or type `:plan`, to open it: a line a step, `☐` to do, `◐`
being worked on, `☑` done and dimmed. Select it, or type `:plan` again, to
fold it. Each time the agent changes its list it sends the whole list again,
and the shelf follows. A long list shows the steps around the first one not
done, as many as fit in two fifths of the pane, at least three and at most
eight, and its heading says which, as `steps 5–12`. The daemon keeps 64 steps
at most, each cut to 500 characters on one line.

Once every step is done, the list stays, as `Plan · all 5 done`, until your
next message. An agent's plan is shown, not acted on: when a chat
[moves to another agent](~clackatoo/reference/agent-settings#moving-a-chat-to-another-agent),
the new agent is told the old one's unfinished plan once, and then it stops
being shown. The phone shows the same list, every step, always open.

### Turns the agent takes by itself

An agent can go on with no message from you: Claude Code does when something
it ran in the background ends, to look at what it printed, and might commit
and write you a summary. That shows as a turn of its own, with no message,
under a line such as
`Claude Code went on by itself · Background command "npm test" completed`. The
chat is at work while it lasts, and a message you send waits for it. It ends
when Claude Code says so. <kbd>Ctrl</kbd>+<kbd>C</kbd> stops it: Claude Code
is asked to stop, and the turn ends once it says it has, after anything still
on its way, or after 5 seconds at most. Something Claude Code says with no
turn of its own, such as `Task stopped by user` after you stop a task, shows
as a notice above the box, not as a turn; only the latest shows. Thinking, or
a tool it starts, while the chat is idle opens such a turn for any agent. An
agent that doesn't say when it starts or ends such a turn opens one with its
words too, except what's shaped as its own adapter's notices to a client that
can't show them, such as Codex's `Warning: …`, which shows as a notice too.
Its turn is taken to have finished once it has been quiet for 60 seconds with
no tool running; <kbd>Ctrl</kbd>+<kbd>C</kbd> ends it at once. A daemon
upgrade in the middle of such a turn doesn't end it: the agent goes on through
the upgrade, and so does its turn. Only an upgrade that stops the agent, as
[Through an upgrade](~clackatoo/reference/agents-lasting#through-an-upgrade)
describes, ends it there. A chat that ends one while you aren't looking is
marked unseen, a subagent's card says it's working again, and an agent waiting
for the subagent with `wait_for_chat` gets what it said.

### When a turn fails

When the model is overloaded, rate limited or the connection drops, the agent
tries its request again by itself. While it waits, the line above the box says
what it says of that in place of what it's doing: Claude Code's
`Retrying Claude, attempt 2 of 10.`, Codex's `Reconnecting... 2/5`. It goes as
soon as the agent says anything more of the turn. Claude Code says this only
to a client that asks for it as JetBrains AIR does, as clackatoo does unless
`tasks = false` (see
[Which agent](~clackatoo/reference/agent-settings#which-agent)).

A turn that fails anyway says why in plain words, with what to do, rather than
the agent's error:

| What went wrong | What the turn says |
|---|---|
| The sign-in ran out, also mid-turn | that the agent needs you to sign in again, with its sign-in command to run in a terminal, such as `claude /login` |
| Rate limited | to wait a moment, then send the message again |
| The account's usage limit or credit is used up | that the usage limit is reached, with the agent's words for when it resets, as Codex's `Try again at 3:05 PM.` |
| The model is overloaded | to send the message again in a moment |
| The model's provider failed | to send the message again |
| The agent can't use the account or its credentials | only that |
| The agent refused the request | only that |
| The connection to the model dropped mid-turn | to send the message again |
| The conversation, or the answer, hit the model's limit | to start a new chat |
| A Codex session open in another Codex client: the app, the CLI, or an editor | to close it there, then send the message again |

Where the agent's words say more, they follow clackatoo's, as `Codex said:`
and the words, in the terminal's font, as code: nothing the agent says can
pass for clackatoo's advice, nor hide what it says, and each thing it says is
cut to about 2 KB. What it said in full follows underneath,
`The agent said: …`, unless that's said already; `clackatoo capture` and
copying the turn keep it. A failure clackatoo doesn't know says
`the agent failed:` and the agent's words, quoted the same way. A Codex
session open in another Codex client isn't taken for one that's gone: the chat
says so and waits, rather than starting a new session in its place.

A failure the agent gives by itself, rather than with a turn's answer, isn't
lost either: if it took a turn by itself, as Claude Code does when something
it ran in the background ends, that turn ends failed, saying why; otherwise a
notice above the box says so, as when its connection was lost.

The last turn, if it failed and was a message of yours, has a **Send again**
button under it, and `R` in normal mode does the same: the same message, with
its images, as a new turn below; and with the role a chat was started with, if
its first message failed before the agent did anything with it. If the turn
already started commands, changes to files or other tools, such as an MCP
server's, however they went, which might have done something that would be
done again, the bar says how many first, and it's sent only if you ask again
within 5 seconds, with no turn since. A failure the agent also said by itself
as it answered shows once, under the turn, not again as a notice. Images can
be sent again only until the daemon is upgraded; after that, its words go
alone and the chat says so. Nothing is sent again while the chat waits for a
session to load.

Where clackatoo can do what the turn says to, the button beside **Send again**
asks the daemon running the chat to do it, in a new tab of the chat's
workspace, on the chat's machine, which the window then shows:

- **→ Sign in**, and `gl`, when no one's signed in to the agent and clackatoo
  knows its sign-in command: the new tab runs that command in the chat's
  folder, as `codex login`. The command is clackatoo's own, from the agents it
  knows, never anything the agent said, and the daemon finds it again as it
  runs it; it's run as it is, with no shell to read it, with the path to the
  agent's own program where clackatoo installed it, as for Codex. What the
  turn says to run is the same command, each word quoted as a shell would need
  it. Sign in there, close the tab, and send the message again. An agent of
  your own from `agents.toml`, under a name clackatoo doesn't know, has no
  login command it knows, so no button. A failure the agent gives by itself,
  on a turn it took by itself, offers it too.
- **+ New chat**, and `gC`, when the conversation, or the answer, hit the
  model's limit: a new chat with the same agent, in the same folder, with the
  chat's own tools, Task tool and browser settings.

Only the last turn has them, while the agent waits for a message: the daemon
refuses any other, and a busy chat. They need a daemon that says what it can
do (protocol 142); a failed turn kept through an upgrade says what to do but
has no button. The [web app](~clackatoo/reference/phone) shows the same
buttons, **Sign in** and **New chat**, under the turn.

### What else a turn says

Where the agent compacted its context to make room, dropping the conversation
so far for a summary of it, a line across the turn says `Context compacted`,
with how it was started and the tokens before and after, as
`automatic · 182k → 31k tokens`, where the agent says: Claude Code does, Codex
says only that it did. The line never folds away with the turn's work.

A command that failed says its exit code at the end of its card's head, as
`$ cargo test · exit 101`, where the agent says it: both do for a command they
run as a terminal, and Claude Code says 1 for any command that failed.

Under each finished turn, on the right, is what it spent, where the agent
says: its tokens, and its cost, as `12k tokens · $0.04`. Claude Code gives
both, and a turn's cost is what the session's grew by during it; Codex gives
tokens only. `K` shows what the session has cost so far beside its context and
limits. `:set nochatcost` hides both.

An image the agent shows in its words, rather than from a tool, shows in a row
of its own, `Image`, with the same caps as a tool's images; one that can't be
shown, as too big, shows nothing.

### When the agent asks

In Manual mode, before it does anything that changes something, such as
writing a file or running a command that isn't only reading, the agent asks.
Other modes ask about less, and Auto leaves it to Claude. Its question takes
the box's place, with an answer on each button: the first way to say yes in
the accent color, and the ways to say no quieter.

Select an answer to give it. In insert mode, press an answer's number to give
it, or <kbd>Esc</kbd> to say no. In normal mode, a number picks its answer out
with a ring, and the bar says what it is: the same number again, or
<kbd>Enter</kbd>, gives it, from 0.3 seconds after to five seconds after, so a
count such as `22j` typed in one go can't answer by chance; any other key, a
mouse button, the wheel or leaving the window drops it, and <kbd>Esc</kbd>
leaves the question be. A number pressed within half a second of the question
coming, of you arriving at its chat, or of the last key you typed, doesn't
answer it; the bar says to press it again. <kbd>Ctrl</kbd>+<kbd>C</kbd> says
no and stops the answer. Other keys go nowhere while it asks, and the bar says
which numbers answer it.

clackatoo's own questions about its tools say Reject, as Claude Code's do, and
show what the agent wrote as code, so nothing in it can pass for clackatoo's
words.

Nothing in a question can hide what it says. A character that reorders the
text around it, as the right-to-left override does, or that shows nothing
itself, as a tag character or a zero-width space, shows as its code, such as
`<U+202E>`, wherever the agent's text is: the question, its answers, and the
titles of the tools in the conversation. So the command you read is the one
that runs, in the window and on the phone.

An answer of a kind clackatoo doesn't know, which an agent might offer, shows
as the agent names it, plainly: it's never taken for yes or for no, never the
one in the accent color, and <kbd>Esc</kbd> never picks it. A window from
before this is shown the other answers only, or, if every answer is of a kind
not known, a Decline that says no.

Questions wait their turn, 16 at most of each kind, the agent's own,
clackatoo's tools' and forms, so many of one kind never hold up another: one
more is turned down at once, and the chat says so. A question shows at most 64
answers, an answer's name at most 256 characters, and a title past 4096
characters, as a long command, says how many more there are.

What it asks about follows the agent's own settings. Claude Code reads its
usual settings files, so commands you've allowed in Claude Code are allowed
here too.

### Questions, forms, and plans

An agent can ask you something rather than ask leave: Claude Code's
multiple-choice questions (its AskUserQuestion tool), Codex's questions, an
MCP server's form, a link to sign in to an MCP server, or whether to try again
with another model when one turns a request down. ACP calls these
elicitations. The question takes the box's place as a form: the agent's
message, then each field's title and what it asks, then a row for each option,
a yes-or-no, or a box to type in, and at the end Submit and Decline.

- A choice of one shows a dot by each option (`◉` picked, `○` not); a choice
  of several a box to tick (`☑`, `☐`). Options are numbered, and an option's
  meaning shows under it.
- `j` and `k`, the arrows, <kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd>,
  or <kbd>Ctrl</kbd>+<kbd>N</kbd> and <kbd>Ctrl</kbd>+<kbd>P</kbd> move
  between rows, in either mode: in normal mode they move through the form, not
  the conversation. The row picked is lit.
- `1` to `9` pick that option of the field you're in. <kbd>Space</kbd> or `x`
  picks, ticks, or flips the row picked. <kbd>Enter</kbd> does the same, moves
  on from a box, or presses Submit, Decline or Open the link.
- In a box, in insert mode, keys type, <kbd>Backspace</kbd>,
  <kbd>Ctrl</kbd>+<kbd>W</kbd> and <kbd>Ctrl</kbd>+<kbd>U</kbd> delete, and
  <kbd>Tab</kbd>, <kbd>Down</kbd>, <kbd>Up</kbd> or <kbd>Enter</kbd> move on.
  In normal mode, `i`, `a` or <kbd>Enter</kbd> go into it. A secret shows as
  dots; a number's box takes only what a number holds. A paste goes into the
  box picked. Select a row to pick it, or a box to type in it.
- Claude Code puts a box called Other after each of its questions: type your
  own answer there instead of picking, or a note beside what you picked.
- Submit sends only answers that fit what the agent asked; otherwise the bar
  says what's missing or wrong. A required field shows `*`. clackatoo checks
  them again before the agent hears them.
- <kbd>Esc</kbd> never declines a form: it goes to normal mode, as it does
  from the message box, so what you've typed stays. Decline says no, and the
  agent goes on without. <kbd>Ctrl</kbd>+<kbd>C</kbd> stops the agent, which
  takes the form away.
- As with other questions, Submit, Decline and Open don't answer within half a
  second of the form coming, or of you arriving at its chat; the bar says to
  press again.

A link to open, such as an MCP server's sign-in, shows whole, with where it
goes on a line of its own, as plain text apart from the agent's message, then
Open the link, which hands it to your desktop's browser and tells the agent
it's open, and Decline. Only a plain web link is shown: one that isn't `http`
or `https`, isn't plain ASCII, names a user (`user@host`), holds a space, a
control character or `%` in its host, names an address as a number other than
a plain dotted quad (such as `0x7F.1`), or hides anything is declined, and
each window checks it again before it opens it. If the agent says it's done
with a link you never opened, it goes by itself. The browser opens on the
window's machine; a sign-in whose page sends you back to `localhost` needs the
window on the daemon's machine.

When the agent asks to leave Plan mode, as Claude Code's ExitPlanMode or
Codex's plan review do, the plan shows above the question as Markdown, in a
frame at most half the pane high that scrolls with the wheel over it, or `j`
and `k` in normal mode. The question says "Go ahead with this plan?", and the
answers are the agent's own, such as "Yes, and use auto mode" or "No, keep
planning".

A form waits until it's answered, from this window or any other, the phone
included, and is still there if the window reconnects or you open the chat
elsewhere. It goes, answered as canceled, if you stop the chat
(<kbd>Ctrl</kbd>+<kbd>C</kbd> or Stop), even between turns, or move it to
another agent; it goes if the agent stops waiting for it, or if the agent or
its session stops. One that comes between turns, as a sign-in can, waits past
the end of the turn. One that comes while a past session is being loaded, as
an MCP sign-in can, is declined: no one is there to answer it. While a form
waits, its agent waits too: a chat an agent started, that no one is watching,
stays stuck on it, as on a question asking leave, until you answer it. Its
text is shown with nothing hidden in it, as other agents' words are, and a
form asking more than 256 KiB of text all told is declined, with a notice in
the chat, said once until you next write. A window older than these forms
shows only a question naming what the agent asks, with one answer, Decline.

ACP has no way to send words back with a "no". Claude Code takes a refusal of
its plan as "keep planning", and stops the turn: send your feedback as the
next message. For its questions, the Other box is where your own words go.

Saying it can show forms changes what the agents do, beyond asking:

- Claude Code keeps its AskUserQuestion tool, which its adapter takes away
  from a client that can't show forms, and passes on MCP servers' forms and
  sign-in links, and the consent asked before retrying with another model,
  which it otherwise declines.
- Codex asks its questions (`request_user_input`), which it otherwise answers
  with nothing, and an MCP server's forms and links come as forms rather than
  as a question asking leave. It also offers signing in with a device code. A
  form about no chat, such as one asked while you're signing in to Codex, is
  declined, as there's nowhere to show it.


### Selecting and copying

Drag across the conversation to select its text, or double-click a word or
triple-click a line. Letting go copies it to the primary selection, which a
middle click pastes. Drag past the top or bottom of the conversation and it
scrolls, faster the further past you go, selecting as it goes. While it's
selected, the bar says VISUAL, and `y` in normal mode yanks it into the
registers, as visual mode's does, after a count or a register too; any other
key forgets it. <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd>, or
<kbd>Ctrl</kbd>+<kbd>C</kbd> while the agent isn't answering, copies it to the
clipboard, where `"+` and the unnamed register have it too. What's copied is
the text as shown, without its Markdown. A turn's folded work isn't copied,
only its message and its answer. Over a block of code, a Copy button copies
the code.

### Desktop notifications

The window shows a desktop notification when a chat you can't see finishes a
turn, fails (its turn went wrong, or its agent couldn't start) or asks you
something: a permission, or a form. A chat you can't see is one that isn't in
the tab shown, or any chat while the window doesn't have the focus. These are
the chats the tabs mark `•` and `?` (see
[Workspaces, tabs, and panes](~clackatoo/reference/window#workspaces-tabs-and-panes)):
the notification and the mark go by the same rules.

- **One a chat.** Each chat has one notification at most, replaced as it
  changes, so a chat that asks and then finishes says so in one.
- **Taken back when you look.** Showing the chat with the window focused
  removes its notification, as does its question being answered elsewhere, the
  agent going again, or the chat closing.
- **What it says.** The chat's title and a line such as `Claude Code finished`
  or `Claude Code stopped: something went wrong`, never what the agent wrote,
  as a lock screen can show it. A daemon older than the window doesn't say
  whether a turn went wrong, so its chats say `stopped: it needs a look`.
- **Subagents.** A subagent finishing tells the chat that started it, not you,
  so it shows no notification. A subagent's question does, since its chat
  waits on you. A chat in no workspace at all isn't one you watch, and shows
  none.
- **Only what the window saw happen.** Chats that finished or asked before the
  window opened, or while it was out of reach of the daemon (a laptop asleep,
  say), are marked, not notified, so waking up isn't a burst of notifications.
- **Seen elsewhere.** A window without the focus says nothing of a turn that
  ended while you were looking at the chat somewhere else, in another window
  or on your [phone](~clackatoo/reference/phone).
- **Selecting it** goes to the chat: its workspace, its tab and its pane, or
  for a subagent in the background, the pane of the chat that started it. On
  X11 the window takes the focus. On Wayland a window can't raise itself
  without an activation token; GNOME raises it anyway for clackatoo's desktop
  file (`clackatoo desktop install`). Other Wayland desktops send a token when
  it's selected, which the window can't use, so there it goes to the chat
  without coming to the front.

The [`notify` option](~clackatoo/reference/config#options) says which are
shown: `all`, the default; `questions` only; or `off`. The notifications are
the window's own, sent over D-Bus to the desktop's notification server
(`org.freedesktop.Notifications`), so a window connected to a daemon on
another machine notifies on the machine it runs on, of the chats it shows. The
daemon sends none. Two windows open on the same chats each show their own. A
browser on the same computer with
[phone notifications](~clackatoo/reference/phone#notifications) turned on is
told as well: turn one off.
