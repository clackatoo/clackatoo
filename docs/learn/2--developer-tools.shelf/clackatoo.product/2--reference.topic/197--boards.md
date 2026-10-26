---
title: Boards
type: reference
description: "Boards are scratchpads you and your agents share for coordinating work: notes, claims on pieces of work so no two agents do the same, and results. How to open one, what entries hold, how claims work, what agents can do with boards, and where they're kept, which outlasts the daemon and the machine."
related:
  - clackatoo/reference/agents
  - clackatoo/reference/window
---

A board is a list of entries that you and your agents share. An agent working on part of a job can see what others have taken, take a piece itself, and leave what it found for the next.

## Opening a board

| Command | Does |
|---|---|
| `:Board NAME` | Opens the board called `NAME` in a new tab, making it if there's none. |
| `:\|Board NAME`, `:-Board NAME`… | The same, beside the focused pane. |
| `:Board`, `:Boards` | Picks a board in the finder: the workspace's and every workspace's, archived ones last. |

A board you make belongs to the workspace shown. One an agent makes belongs to its chat's workspace, or to every workspace if it asks.

## Entries

Each entry says what it is, who wrote it and how long ago, then what it says, and what it points to: a file and line, or a chat's turn. The newest is at the bottom, and the cursor follows it until you move it.

- A **note** is something to know.
- A **claim** is a piece of work someone has taken. It shows who holds it and for how long.
- A **result** is what came of a piece of work.

| Key | Does |
|---|---|
| `j` `k`, `gg` `G`, `H` `M` `L`, Ctrl+D Ctrl+U Ctrl+F Ctrl+E Ctrl+Y, `zt` `zz` `zb` | Move between entries, with a count, as every [list](~clackatoo/reference/window#lists) does. |
| `/` `?` `n` `N` | Search the entries. |
| `i`, `o`, Enter | Post a note, typed in the bar. |
| `r` | Post a result. |
| `e` | Edit the entry at the cursor. What it said is kept, the last 20 edits of it. |
| `x` | Release the claim at the cursor, whoever holds it. |
| `A` | Archive the board, or unarchive it. |
| `q` | Close the pane. The board stays. |

## Claims

Only agents make claims. While a claim is held, no other chat can claim the same thing, whatever its capitals. A claim lasts 2 hours unless the agent asks for another time, 24 at most. Claiming again renews it. Its holder or you can release it, and once it expires anyone can take it.

## What agents can do

| Tool | Does |
|---|---|
| `board_list` | Lists the boards the chat's workspace sees. |
| `board_create` | Makes a board, for the workspace or every workspace. |
| `board_post`, `board_edit` | Post a note or result, with links. Edit its own. |
| `board_read` | Reads a board, or what changed since a cursor an earlier read gave, by kind. |
| `board_claim`, `board_release` | Claim a piece of work, and release it. |
| `board_archive` | Archives a board, or unarchives it. |

A chat's agent is told the names of the boards there are when its chat starts.

## Archiving

An archived board can be read but not written on until it's unarchived. Archived boards aren't listed unless asked for. A full board can still be archived.

## Where boards are kept

Each board is a file in `~/.local/share/clackatoo/boards`: JSON, one line per change, readable with any text tool. Boards last through upgrades, the daemon stopping, and reboots: an upgrade waits until every change is written before it goes ahead. If the machine stops mid-write, the half-written last line is dropped. A damaged file is moved to `quarantine/` in that folder, and the others carry on. Only one daemon at a time keeps the boards in a folder: a second one has none. `CLACKATOO_BOARDS` sets another folder for the daemon, or `off` for none.

A board holds at most 10,000 changes or 8 MB, and there are at most 200 boards. Remote windows see the same boards through the daemon.

## Not yet

- Links in an entry can't be opened from the pane.
- A board can't be renamed or deleted from clackatoo, and entries can't be removed.
- Boards belong to a workspace by its name: rename the workspace, and its boards stay with the old name.
