---
title: Background work
type: reference
description: See ongoing daemon work, inspect progress, cancel a job or dismiss an outcome.
---

The bottom bar shows **Working · N** while the daemon has work running or
queued. Click it, or run `:Work`, to open the background-work list. The list
stays up to date as jobs start, report progress and finish.

Type to filter by operation, state or project. Each row shows the operation,
its state, any progress count and its project. Where the window has room,
the preview shows the current phase or the failure explanation. A job with
an unknown amount of work shows a count so far rather than a percentage.

Press `Ctrl+D` on an active job to request cancellation. Some work is shared
by several chats: cancelling it cancels that shared operation. Cancellation
is cooperative; **Cancelling** remains until the worker returns. A cancelled
skill request can be retried by opening the relevant page or starting the
request again.

Press `Ctrl+D` on a finished job to dismiss its outcome. Successful and
cancelled jobs stop contributing to the bar's indicator immediately. Failed
jobs keep an error indicator until dismissed or replaced by newer failures
in the bounded history. The list retains at most 16 recent outcomes,
preserving failures ahead of successful outcomes. `Esc` closes the list.

The list holds work worth watching: reading the skill and specialist
catalogues, and indexing a project's outline. What the daemon does to
answer something you or an agent just asked isn't listed: a search in the
context browser, a preview, capturing a message's attachments, an outline
query. If one of those fails, whoever asked is told why, in place; it
leaves nothing in the list and never raises the bar's error indicator.

All of it shares one scheduler. It bounds queued and running work, runs
what an interaction is waiting for first, and runs at most one job for a
project at a time; jobs that belong to no project aren't held back by
each other. See [Project Code Outlines](~clackatoo/reference/code-outlines)
for indexing coverage and the tools agents can query.

Background work is scoped to the daemon the window is connected to. It is
shown in native windows; the phone and connected workspace lenders do not
receive the list. Jobs and their recent outcomes are not restored after a
daemon restart or upgrade. A reconnect to the same daemon gets its current
list.
