---
title: An Agent's Changes to Files
type: reference
description: How an agent's changes to files land in the editor beside yours, and how to undo a turn, rewind a conversation into branches, and review what changed.
related:
  - clackatoo/reference/agents
---

An agent's changes to files land in the editor as its own edits, in every
window showing the file, and each turn's changes can be undone, rewound, or
reviewed.

## The files it changes

When the agent changes a file you have open, the change lands in the editor as
the agent's own edit, in every window showing it, beside anything you've typed
and not saved. `u` takes back your edits, never the agent's, and each of the
agent's tool calls is one step of its own. A change to a line you've changed
and not saved isn't made: the bar and the chat say so, the file keeps the
agent's version, and `:e!` takes it or `:w!` keeps yours.

A change to a big file, such as an agent's rewrite of a few megabytes, is
merged into the editor on a thread of the daemon's own, so windows and
terminals carry on while it's worked out. It lands once it's ready: anything
you type in the file meanwhile is kept, and the change is merged again around
it. Saving the file, `:e!` or moving it waits for the change first, and the
bar says so afterward. The wait is bounded: only so much is merged at a time,
about a second's work at most, and a bigger change, or one you keep typing
over, is merged at once instead, which holds up the window as long.

The file on disk gets the agent's change underneath yours: a file you'd saved
stays saved, and one you hadn't stays changed by only your edits, still
unsaved. So the commands the agent runs, such as `cargo build`, see its
change, and your work in progress reaches the disk only when you write it.

The agent's commands change files too, such as `cargo fmt`, `sed -i` or a code
generator, without saying so. clackatoo watches the files you have open, and a
change made while one of the agent's tools runs, with only that chat answering
in the project, is taken in as that agent's, under the same rule about lines
you've changed; undoing its turn takes it out too. A change made while it's
thinking or asking permission, or while two chats are answering in the
project, is treated as from outside. Files indented with tabs are taken in
like any other.

Claude Code and Codex read files from disk, so they don't see what you haven't
saved. An agent that reads through clackatoo sees the editor's text, unsaved
changes and all, and can only read and write files in its chat's directory, or
the git repository that's in.

### Undoing a turn

Under each turn that changed files, a compact summary shows how many, with
Undo or Redo beside the count. Hover over the answer to show Copy alongside
it. Undo takes the turn's changes out of every file it changed, and keeps what
you or later turns changed since. If something since changed the same lines,
nothing is undone, and the chat says which lines: `:agentundo! 3` undoes the
turn's other files anyway. A file the turn made goes to the trash, and a file
that changes while the undo is being written is left as it is, and the chat
says so. Once a turn is undone, Redo puts it back. `:agentundo` and
`:agentredo` take a turn's number, from 1, or else take the last turn there is
to undo or the first to redo. In an open file, an undo is one edit of the
agent's, beside your unsaved changes. Nothing is undone or redone while the
agent is answering: the chat says to wait for it to finish.

### Rewinding and branches

Undoing a turn takes it out of the files, but the agent still remembers it.
Rewinding takes turns out of the conversation too, so the agent goes on as if
they'd never been, as Claude Code's `/rewind` and Codex's <kbd>Esc</kbd>
<kbd>Esc</kbd> do. What you rewind from isn't lost: it stays in the same chat
as another branch, and the chat never splits into a pile of chats.

| Key / command | Does |
|---|---|
| `dG` on a turn | Rewinds to before it. |
| `cc`, `S` on a message of yours | Rewinds to before it and puts it in the box, before anything you'd typed, to change and send again. |
| `:rewind N`, `:rewind` | Rewinds to before turn N, or the last turn. `/rewind`, typed as the whole message, is `:rewind`. |
| `:rewind! N` | The same, undoing the files without asking, leaving out any that can't be. |
| `[b` `]b`, selecting ‹ or › | Shows the branch before or after where branches part. |
| `:branch N` | Shows branch N where they part, at the turn under the cursor or the nearest above. |
| `:branches` | The finder over every branch: where it parts, its first message, its turns, when it was last shown. <kbd>Enter</kbd> shows it. |
| `:fork N`, `:fork` | Copies the branch shown, up to turn N or all of it, into a new chat in this pane; the chat copied goes to the start page's chats in no pane. `:\|fork` and `:-fork` put the copy beside instead. `/fork` and `/branch`, typed as the whole message, are `:fork`. |

A rewind always asks first, naming the turn and quoting its message, as in
`Rewind to before turn 7 ("fix the lint…")? (y/n)`: `dG` and `cc` are a key or
two, and fall back to the last turn when the turn under the cursor isn't in
view. Only `:rewind!` doesn't ask. If the turns taken out changed files, the
same question asks whether to undo them: `y` undoes them, newest first, as
undoing each turn does, `n` leaves them and rewinds all the same, and any
other key changes nothing. The `rewindfiles` option (`ask`, `undo`, or `keep`)
answers about the files for you. A turn whose files can't be undone, as when
you've changed the same lines since, stops the rewind before anything is
rewound; the turns after it that were undone stay undone, and the chat says
so. If the agent runs anything in the background, the bar says leaving the
branch stops it, and asks first. Nothing can be rewound while the agent is
answering or asking you something, and a chat another agent started can't be
rewound.

After a rewind, the chat ends at the turn before, with a row saying "New
branch · write to go on from here". Nothing is asked of the agent until you
send: the first message makes the branch, forking the agent's session at the
end of the turn before, so the agent knows the conversation up to there
exactly. Where the agent can't fork its session there, it's told the turns
before as a summary, and the switcher says "from a summary".

Where branches part, `‹ 2/3 ›` shows over the turn: which branch is shown, of
how many. Switching to another branch goes on with that branch's own agent
session, which remembers it exactly as it left it and knows nothing of the
others. With `rewindfiles` at `ask`, the bar asks whether to undo what this
branch changed in files after the turn where they part, and make again what
leaving the other branch undid; or, with nothing here to undo, only whether to
make that again. `y` does, and `n` switches leaving the files as they are.
Your files are shared by every branch, so with `n`, the branch you switch to
sees files from the one you left.

`:fork` is the deliberate way to a separate chat, when you want both
conversations side by side. The copy has the same agent, folder and settings,
and only the branch shown, with no other branches. A copy of a chat another
agent started keeps that chat's limits and profile, so it can never do more; a
browser the chat was let use must be allowed again in the copy. Its agent's
session is forked from the chat's where it can be, so it knows the
conversation exactly; otherwise it's told it as a summary with your first
message. Files aren't touched, but what the copied turns changed is undone
from the copy from then on, not from the chat copied, and both chats say so.
Closing the copy gives that back to the chat copied, for the turns it still
shows. A chat another agent started can be copied, and the copy is your own.

A chat keeps at most 16 branches besides the one shown: past that, the one
shown longest ago is let go whole. It keeps about 4 MB of the turns of
branches not shown, and all chats together about 64 MB; past that, those shown
longest ago keep only where they part and their first message, and their turns
are told again by the agent when you switch to one. A branch with no agent
session to tell them again is let go whole instead. A branch whose agent
didn't tell a turn again shows that turn as failed. The phone says where
branches part ("Branch 2 of 3"), but switching is the window's alone.

### Reviewing its changes

Lines an agent changed that you haven't looked at yet get a bar of their own
beside the line numbers, right of git's signs, in a color for the chat
(`agentsigns` turns it off). `:Review` (or `:Review chat`) opens a review
beside the pane: every change of the chat's you haven't reviewed, by file,
each with three lines around it, what its lines were on red and what they are
on green. `:Review turn` shows the last turn's, `:Review turn 3` the third's,
and `:Review base` everything changed since the branch left `main` (or
`master`, or its upstream), yours included, with the agents' marked.
`<Space>gr` opens one, goes to it, or closes it.

| Key | Does |
|---|---|
| `j` `k`, `gg` `G`, `H` `M` `L`, <kbd>Ctrl</kbd>+<kbd>D</kbd> <kbd>Ctrl</kbd>+<kbd>U</kbd> <kbd>Ctrl</kbd>+<kbd>F</kbd>, `zt` | Moves, with a count, as every [list](~clackatoo/reference/normal-mode#lists) does |
| `]h` `[h`, `]f` `[f` | The next change or the one before, the next file or the one before; a count goes that many |
| `/` `?` `n` `N` | Searches the lines, the files' names and the comments, shown as you type and lit as every [list](~clackatoo/reference/normal-mode#lists)'s search is |
| <kbd>Enter</kbd> | Goes to the line in the editor |
| `a`, `A` | Keeps the change, or every change in its file: its color goes |
| `x`, `X` | Rejects the change, or every change in its file; `x` on a comment takes it back |
| `gc` | Comments on the line, for the agent |
| `S` | Sends the comments, as one message |
| `R` | Works the list out again |
| `Q` | Closes the review |

Rejecting takes the change out as undoing a turn does, keeping what you've
changed since, including anything you typed inside the agent's change. It's
refused if you've changed the same lines, and says which. `:reviewkeep` and
`:reviewreject` do the change at the cursor, or `file`, `turn` (`turn 3`) or
`all`; `:reviewreject!` rejects what it can. Comments wait until `:reviewsend`
or `S` sends them, as "Review comments:" and a line for each, with its file
and line, and whether it's in the agent's change; they move with the text as
you and the agent edit. Undoing a turn clears its color; redoing it brings it
back. What you've reviewed, the comments waiting, and open reviews last
through an upgrade.

A review of a turn or of the chat takes in its subagents too, open or closed:
a turn's review lists the ones that turn started, and theirs. Under the
heading is a line for each, with what it was asked, and each change says whose
it is, as `· explore (chat 7), turn 1`, the turn being the subagent's own.
Keeping and rejecting work as for any change. A rejected change of a
subagent's is that subagent's step, never the chat's you're reviewing and
never yours; rejecting a file holding several chats' changes does all of it or
none, a step for each chat. Changes of two chats too close together to tell
apart are refused: reject each chat's alone. Undoing a turn in the chat that
started them leaves their changes, as the review says: reject them in the
review instead. `:reviewkeep turn` and `:reviewreject turn` take in the
subagents the turn started.
