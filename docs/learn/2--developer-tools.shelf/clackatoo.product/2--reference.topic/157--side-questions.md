---
title: Side Questions
type: reference
description: Ask a temporary question about the current conversation while the main agent keeps working.
related:
  - clackatoo/reference/chatting
  - clackatoo/reference/workspace-context
  - clackatoo/reference/agent-changes
---

Type `/btw Why did we choose cookies?` in a chat to open its **Side question**
sheet. `/btw` alone reopens that chat's existing side conversation, or starts
an empty one. The command belongs to clackatoo, appears in slash completion,
and is never sent to the main agent. The **Side question** control beside
**+ Context** opens the same sheet without changing the main draft.

The window opens the sheet on the right, keeping the main conversation visible
in wide panes. A narrow pane gives the sheet its full width. The phone opens
an accessible sheet with the same actions. The header says whether the main
agent is working. The side conversation has its own composer, Markdown
answers, follow-up questions and Stop control.

## Captured context

The first opening captures the parent's visible conversation and plan into a
bounded transcript digest. Recent exchanges are included in full where they
fit; older ones are summarized. Attached text already present in the transcript
is included according to that same budget. Hidden reasoning, unreported
provider state and full tool-output history are not copied.

A separate provider session receives this snapshot. Opening it works while the
main agent is answering, without forking a live session or moving any file undo
records. Later changes in the parent are not added automatically. The side
session inherits the parent's model and effort where the adapter offers them.
It answers from the captured context and its own knowledge.

The side conversation is context-only. Clackatoo supplies no MCP tools, browser
or delegation, refuses host file reads and permission requests, and stops the
side answer if the adapter reports a tool call. Claude sessions also receive
an empty built-in tool list and strict MCP configuration. Other adapters do
not expose equivalent tool-removal controls: cancellation of a reported tool
call is best effort, and is not a guarantee that a provider-internal operation
has not started. The no-tools policy is separate from a security sandbox.

## Returning to the main conversation

- **Esc**, **×**, or the phone's **Done** hides the sheet. Its conversation and
  unsent follow-up remain available when reopened.
- **Copy** copies one answer as Markdown.
- **Send to main** captures one finished answer as an immutable attachment in
  the parent's draft. It closes the sheet and leaves sending up to you. It
  does not steer or interrupt the main agent.
- **Keep as chat** promotes a finished side conversation into a normal chat in
  a new workspace tab, retaining its captured background and exchanges. The
  new chat keeps the original parent's access limits. The parent's conversation
  and file undo records stay where they were.
- **Discard** clears the temporary conversation. Already attached snapshots
  remain available in their destination drafts and messages.

Temporary conversations stay out of ordinary chat lists. There is one per
parent, up to 32 per daemon, with at most 20 exchanges and 64 KiB per question.
Keep a longer conversation as a normal chat. Closing the parent permanently
also closes its temporary conversation. Live daemon upgrades retain the link;
a fresh daemon restart does not restore temporary sheets. Provider-side
conversation storage follows the provider's own retention behaviour.

Reconnects refresh the side conversation without automatically replaying a
question whose delivery was unconfirmed. Check its last exchange before
retrying. Protocol 160 is required; older clients and daemons cannot use the
side-question API.
