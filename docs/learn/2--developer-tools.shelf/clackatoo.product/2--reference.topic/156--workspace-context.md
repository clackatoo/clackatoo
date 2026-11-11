---
title: Workspace Context
type: reference
description: Attach files, symbols, diagnostics, commands, changes, conversation excerpts, board entries and folder outlines as immutable message context.
related:
  - clackatoo/reference/chatting
  - clackatoo/reference/agent-tools
---

The **+ Context** control above a chat's message box opens a workspace browser.
Choose a category, search, select an object, inspect its preview, and attach it.
The phone uses the same daemon API and captures the same content as the window.
Images remain beside the message box and count towards the attachment limit.

## Choosing a source

| Source | What's captured |
|---|---|
| File | The current editor buffer, including unsaved text, or the file on disk. A line range includes only those lines. |
| Symbol | A named definition, validated against its current syntax outline. |
| Diagnostics | The latest language-server results for a file or the project. The preview warns that analysis may lag source changes. |
| Command | A retained shell block with its command, working directory, exit status and output. Running commands are labelled as still running. |
| Terminal selection | The selected retained document lines. Private regions are excluded. |
| Change | One already-loaded review hunk, with its before and after text and comparison scope. |
| Chat | Up to twenty explicitly selected conversation turns. Answers are included; requests are optional. Tools and hidden reasoning are excluded. |
| Board | One entry visible in the destination chat's workspace. |
| Folder | A directory outline, bounded to three levels and 1,000 entries, without file contents. |

The window also accepts `@file:`, `@symbol:`, `@diag:`, `@command:`,
`@change:`, `@chat:`, `@board:`, `@project:` and `@context:` to open the
browser in that category. Tab changes category; arrows choose a result;
Enter previews it, then Enter attaches the preview. Esc closes the browser.
Page Up and Page Down scroll a preview or move between result pages.
The mouse wheel scrolls the preview. **Copy snapshot** copies the captured text.
**Narrow lines** accepts an inclusive range such as `12-40` for a file,
symbol or terminal excerpt. **Contents / ref** chooses between supplying text
and supplying an explicitly labelled reference.

`:Ask` and `<Space>a` attach an editor selection. In a terminal, review,
board or chat, they attach that pane's selected object to a recently used
chat in the same tab. Dragging a file from the explorer onto a chat attaches it.

## Capturing and sending

A preview shows its source, size, approximate token count and any limitations.
Remove an attachment with its close control. Earlier and Later attachment
controls make every attachment accessible when there are many.

Whole files and live objects are captured again at send time. Exact selections,
symbols and excerpts require the preview's source revision to match. If one
changed, refresh the preview or remove it before sending. A selected source
that cannot be captured holds the message and leaves the draft intact.
An ordinary unknown `@word` remains text.

A message can contain at most 32 objects including images, 64 KiB of text per
object and 256 KiB of attachment text in total. Choose a smaller range or
Reference only for a larger file. Searches and folder outlines are bounded;
symbol search coverage depends on the shared project index.

Once accepted, each attachment is an immutable snapshot. Queuing, steering,
taking a queued message back for editing, retrying and branching preserve that
captured content. Editing the source later does not change an accepted message.
A transcript's **Context … inspect** control opens the original snapshot.
Reattaching that snapshot requires a daemon-issued grant scoped to its project.

The daemon sends the captured text as a labelled quotation with its source,
revision and snapshot identifier. Agents can call `read_context` to read that
same snapshot; the tool only reads attachments in its own conversation.
Reference-only attachments contain no captured source body.

## Ownership and retention

Source reads happen on the owning daemon. File reads stay inside the destination
project, validate the opened file handle, reject devices and pipes, and detect
concurrent disk changes. Search and capture use the shared bounded background
scheduler. Stop cancels pending message capture; a timeout never accepts a
partially captured selected attachment.

Remote picker results carry their source daemon's identity. Terminal and chat
identifiers remain in that daemon's namespace. Attaching an object to a chat on
a different source machine is refused; no local file is substituted for a
remote path.

Captured bodies and replay metadata live in a private SQLite store under the
daemon's state directory, namespaced by its socket. Repeated captures reuse
identical stored snapshots. The database has a 512 MiB page limit and fails a
new capture explicitly if it cannot retain it. It does not silently expire
snapshots that an older provider conversation may still reference. Provider
replay is matched against the exact captured message before attachment metadata
is restored. Removing the store loses that recovery data.

Protocol 159 is required. Older windows retain their existing file-mention
behavior; new attachment submissions are refused by older daemons instead of
being downgraded to plain text.
