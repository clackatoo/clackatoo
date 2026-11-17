---
title: Permissions
type: reference
description: Choose provider permissions or Clackatoo rules, edit literal Allow, Ask and Deny rules, and inspect recent decisions and the boundaries they cover.
related:
  - clackatoo/reference/agent-settings
  - clackatoo/reference/agent-tools
  - clackatoo/reference/mcp-servers
---

Open `:Settings`, then **Permissions** under **Agents**. The page says under
its title which machine's chats it's for. Its first control, **Permission
engine**, chooses the engine for new chats:

- **Provider permissions** uses the agent's own permission mode.
- **Clackatoo rules** reviews approval requests with your rules and asks you
  when none matches. There is no model reviewer.

Existing chats keep their selection. To change one, open its permissions
menu beside the model and choose **Permission engine**. Finish or stop its
turn and answer pending permission questions first. The choice survives
reconnects, daemon upgrades and saved workspace restoration. A chat started
by another chat inherits its parent's engine.

## Provider modes and coverage

Clackatoo rules supports Claude Code and Codex. Claude uses Manual mode;
Codex uses Workspace access, which keeps its own sandbox. Their more
restrictive modes, Claude's Plan and Codex's Read-only, remain available.
Clackatoo waits for the provider to confirm a compatible mode before sending
a pending message. Selecting Provider permissions restores the mode the chat
used before the switch, where that mode is still offered.

Other providers use Provider permissions. To move a chat using Clackatoo
rules to one of those providers, select Provider permissions first.

Rules apply to requests that reach Clackatoo:

| Request | Coverage |
| --- | --- |
| Provider tool approvals | Requests the provider sends over ACP, the Agent Client Protocol |
| Clackatoo's built-in tools | Calls through the chat's authenticated tool connection |
| Managed MCP servers | External tool calls and other operations; connection metadata does not ask |

Providers can still run operations without requesting approval. Claude's
read tools and existing provider allow rules are examples. Codex can run
commands and edits already allowed by its sandbox. Those operations do not
reach these rules. Provider-owned MCP connections also keep their provider's
permission behavior.

Tools Off, Browser Off, specialist limits, native window grants and managed
server trust remain in force. Rules cannot turn those capabilities on.
While Clackatoo rules is selected, its rules review tool calls regardless of
whether Tools was set to Ask or Allow; Tools Off still refuses them.
Typing into a terminal and granting the browser's initial consent still
require your direct approval. Provider plan approvals and changes to
sandbox permissions require it too.

## Add or edit a rule

The page lists each rule as a sentence, such as "Allow the command `git
status`". Choose **New rule**, or `a`, or **Edit** beside an existing rule:
it opens as a form, titled `Permissions › New rule`. Choose what it does
**When it matches**, what **It matches** (a tool, a command, reading files or
changing files) and its target, then **Save** or
<kbd>Ctrl</kbd>+<kbd>S</kbd>. Typing on a field starts it, <kbd>Enter</kbd>
finishes it and goes to the next, <kbd>Tab</kbd> goes from field to field,
and <kbd>Esc</kbd> puts back what the field held. A target that can't be
right says why underneath. With changes in the form, <kbd>Esc</kbd> and
**Cancel** ask before they're thrown away. **Delete this rule** asks for a
second press to confirm.

A rule that allows a whole tool which runs commands, such as `claude/Bash`,
reads "Allow any command through `claude/Bash`", and its form says so as it's
typed, since that is what it allows.

The daemon validates and saves the complete rule list. A failed save keeps
the form, and says why. A rule is saved into the list as the daemon has it
then, so a change made meanwhile in another window is kept; a rule that was
itself changed or deleted meanwhile is refused, for you to open again.

A list this window can't read, as a newer daemon's may be, is never shown as
empty: the page says why under its title, lists no rules, and adds none, so
nothing is saved over it.

Decisions have this precedence, regardless of list order:

1. **Deny** refuses the action.
2. **Ask** asks you once.
3. **Allow** approves this action once.

With no matching rule, Clackatoo asks you. An Allow never teaches the provider
to stop sending subsequent permission requests.

| Match | Example target | Meaning |
| --- | --- | --- |
| Tool | `clackatoo/list_panes` | Exact server and tool identity |
| Tool | `managed_docs/search` | The managed server named `docs`, tool `search` |
| Tool | `claude/Bash` | A provider-reported tool identity |
| Command | `git status` | This complete simple command |
| Read | `{project}` | Recognized file reads beneath this chat's project |
| Edit | `~/src/example` | Recognized file edits beneath this directory |

Tool rules can also say **Only with these arguments**, a complete JSON
argument value. Left empty, it matches any arguments; `null` matches JSON null
specifically.
For managed tool calls, this is the tool’s argument value. Other MCP methods
match their complete request parameters, including resource URIs and prompt
names. Keep credentials out of rules.

Command matches are literal. There are no prefixes, wildcards or inferred
command families. Shell operators, quotes, substitutions, redirects and
shell wrappers require approval, even when a broad tool Allow matches.
Commands explicitly naming a working directory outside the chat's project
also require approval.

Read and Edit targets are directories written as absolute paths, `~/…`,
`{project}`, or `{project}/…`. Links are resolved before matching, including
the existing parents of new files. Unresolvable paths ask you. These matches
require recognizable file inputs: Claude's Read, Write, Edit and NotebookEdit
tools, and Clackatoo's file-opening tools. An opaque or multi-file provider
approval asks you instead. Codex approvals carrying additional consent
content also ask directly, including network-access consent.

Direct edits to Clackatoo's config and state, or to `.clackatoo`, `.claude`,
`.codex`, `.agents` and `.git` paths, require your approval even if an Allow
matches, including when those protected folder names are symbolic links.
A Deny still refuses them.

## Recent decisions

The daemon keeps the latest 64 rule decisions in memory, and the page lists
them under **Recent decisions**, read again every two seconds while it's
shown. Each says what was decided of which tool, in which chat and roughly
how long ago, with the matching rule number, or why approval was needed. It does not record action arguments,
command text or file contents. The history clears when the daemon restarts
or upgrades.

Checks that fail or time out ask you. Changing the rules cancels pending
checks and permission questions so an old approval cannot bypass a new Deny.
A stopped or superseded turn cannot execute a delayed automatic approval. Pending checks
become human questions before an upgrade; they are never replayed.

## Configuration and enforcement

The page saves `permissions.engine` and `permissions.rules` in the daemon's
`daemon/settings.lua`. Your `daemon/init.lua` can set them instead; the page
then shows where, as `init.lua:12`, on the engine or on each rule, which a
click opens, and offers no way to add or edit a rule.

```lua
daemon.permissions.engine = "provider"
daemon.permissions.rules = [[
[
  {"decision":"allow","kind":"command","target":"git status"},
  {"decision":"ask","kind":"command","target":"git push"},
  {"decision":"deny","kind":"tool","target":"managed_upload/send"}
]
]]
```

The list is limited to 128 rules and 64 KiB of JSON; each target is at most
4,096 bytes. Unknown fields and invalid values are refused. Invalid config
keeps the last configuration that ran successfully.

These are approval rules, not an operating-system sandbox. An approved
program can run other code and make changes the rule does not inspect.
Provider-approved operations can bypass this approval boundary, and an
unsandboxed shell running as you can reach your config and daemon sockets.
Use the provider's sandbox to enforce filesystem and network boundaries.
