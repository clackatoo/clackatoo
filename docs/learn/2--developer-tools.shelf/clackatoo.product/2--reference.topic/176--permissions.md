---
title: Permissions
type: reference
description: Choose whether the agent's own settings or clackatoo's rules answer its requests, edit literal Allow, Ask and Deny rules, and inspect recent decisions, what the rules cover and what they cannot stop.
related:
  - clackatoo/reference/agent-settings
  - clackatoo/reference/agent-tools
  - clackatoo/reference/mcp-servers
---

Open `:Settings`, then **Permissions** under **Agents**. The page says under
its title which machine's chats it's for. Its first control, **Who answers
agents' requests**, chooses for new chats:

- **The agent's own settings** uses the agent's own permission mode.
- **clackatoo's rules** reviews approval requests with your rules and asks you
  when none matches. There is no model reviewer.

Existing chats keep their selection. To change one, open its permissions
menu beside the model and choose **Who answers agents' requests**. Finish or
stop its
turn and answer pending permission questions first. The choice survives
reconnects, daemon upgrades and saved workspace restoration. A chat started
by another chat inherits its parent's engine.

## Provider modes and coverage

clackatoo's rules support Claude Code and Codex. Claude uses Manual mode;
Codex uses Workspace access, which keeps its own sandbox. Their more
restrictive modes, Claude's Plan and Codex's Read-only, remain available.
Clackatoo waits for the provider to confirm a compatible mode before sending
a pending message. Selecting the agent's own settings restores the mode the chat
used before the switch, where that mode is still offered.

Other providers use the agent's own settings. To move a chat using
clackatoo's rules to one of those providers, select the agent's own settings
first.

Rules apply to requests that reach Clackatoo:

| Request | Coverage |
| --- | --- |
| Provider tool approvals | Requests the provider sends over ACP, the Agent Client Protocol |
| Clackatoo's built-in tools | The calls that ask you: running a command, typing into a terminal, showing a file outside the project, and the browser's first page |
| Managed MCP servers | External tool calls and other operations; connection metadata does not ask |

Clackatoo's other tools, such as `list_panes`, `read_chat` and
`board_read`, never ask and are not checked against the rules. A rule naming
one of them has no effect. A chat working in the background is therefore
never left waiting on a question nobody sees.

Providers can still run operations without requesting approval. Claude's
read tools and existing provider allow rules are examples. Codex can run
commands and edits already allowed by its sandbox. Those operations do not
reach these rules. Provider-owned MCP connections also keep their provider's
permission behavior.

Tools Off, Browser Off, specialist limits, native window grants and managed
server trust remain in force. Rules cannot turn those capabilities on.
While clackatoo's rules are selected, they review commands regardless of
whether Tools was set to Ask or Allow; Tools Off still refuses them.

Some requests always need your direct approval, whichever rule would allow
them. A Deny still refuses them.

- Typing into a terminal, or into a command Codex left running.
- Showing a file outside the chat's project.
- The browser's first page in a chat. Its question keeps the **Allow for
  this chat** answer, which lets the agent open further pages.
- Provider plan approvals and the provider's own questions.
- Requests for more sandbox or network access: Claude Code's
  `SandboxNetworkAccess`, and Codex's requests for additional permissions
  or for network access with a command.
- A command whose request carries anything beyond the command itself, such
  as Claude Code's `dangerouslyDisableSandbox`.

## Add or edit a rule

The page lists each rule as a sentence, which says how far it reaches: "Allow
the command `git status`" is of those words and no more, and "Deny commands
starting `git push`" of whatever starts with them. Choose **New rule**, or `a`, or **Edit** beside an existing rule:
it opens as a form, titled `Permissions › New rule`. Choose what it does
**When it matches**, what **It matches** (a tool, a command, reading files or
changing files) and its target, then **Save** or
<kbd>Ctrl</kbd>+<kbd>S</kbd>. Typing on a field starts it, <kbd>Enter</kbd>
finishes it and goes to the next, <kbd>Tab</kbd> goes from field to field,
and <kbd>Esc</kbd> stops typing. A target that can't be right says why
underneath. With changes in the form, <kbd>Esc</kbd> and **Cancel** ask
before they're thrown away. **Delete this rule** asks for a second press to
confirm, which any other key or click takes back; <kbd>Enter</kbd> in the
field above it never goes on to it.

A rule that allows a whole tool which runs commands, such as `claude/Bash`,
reads "Allow any command through `claude/Bash`", and its form says so as it's
typed, since that is what it allows.

The daemon validates and saves the complete rule list. A failed save keeps
the form, and says why. A rule is saved into the list as the daemon has it
then, so a change made meanwhile in another window is kept; a rule that was
itself changed or deleted meanwhile is refused, for you to open again.

A list this window can't read, as a newer daemon's may be, is never shown as
empty: the page says why under its title, lists no rules, and adds none, so
nothing is saved over it. A form open when the list became unreadable isn't
saved either.

Decisions have this precedence, regardless of list order:

1. **Deny** refuses the action.
2. **Ask** asks you once.
3. **Allow** approves this action once.

With no matching rule, Clackatoo asks you. An Allow never teaches the provider
to stop sending subsequent permission requests.

| Match | Example target | Meaning |
| --- | --- | --- |
| Tool | `clackatoo/run_in_terminal` | One of Clackatoo's tools that asks you |
| Tool | `managed_docs/search` | The managed server named `docs`, tool `search` |
| Tool | `managed_docs/resources/read` | Another MCP method of that server |
| Tool | `claude/Bash` | The provider's own tool, as the provider names it |
| Command | `git status` | A simple command, compared word by word |
| Read | `{project}` | Recognized file reads beneath this chat's project |
| Edit | `~/src/example` | Recognized file edits beneath this directory |

Clackatoo works out which tool a request uses from what it knows of the
call, never from the request's own input. A request that names no tool
matches no Tool rule. Codex names the tool for only some of its commands, as
`codex/exec_command`; others arrive without a name. Use Command rules for
Codex's commands, which match either way. Its approvals of MCP tool calls
always ask you. A managed tool whose name contains `/` could be mistaken for
an MCP method, so it matches no rule either.

A managed server's tool is ruled on once, as `managed_docs/search`, when
clackatoo's proxy is asked to make the call. Claude Code may ask about the
same call first, under its own name for it, such as
`claude/mcp__managed_docs__search`: that question is answered for you, and no
rule is read for it. So a rule on the agent's name for a managed tool never
applies. Write rules for managed tools on the `managed_…` name only. See
[MCP servers](~clackatoo/reference/mcp-servers).

Tool rules can also say **Only with these arguments**, a complete JSON
argument value. Left empty, it matches any arguments; `null` matches JSON null
specifically.
For managed tool calls, this is the tool’s argument value. Other MCP methods
match their complete request parameters, including resource URIs and prompt
names. Keep credentials out of rules.

### Command rules

A command is compared word by word, however its words are spaced. There are
no wildcards.

- **Allow** matches when every word is the same. `git status` allows
  `git  status`, but not `git status --short` or `/usr/bin/git status`.
- **Ask** and **Deny** match every command that starts with the rule's
  words, wherever its program is run from. `git push` matches
  `git push origin main` and `/usr/bin/git push`.

Only simple commands can be allowed. Shell operators, quotes, substitutions,
redirects and a variable set before the program require approval, even when
a broad tool Allow matches. So does a command that names a working directory
outside the chat's project.

An Ask or Deny on a command is a guard rail, not a sandbox. It matches the
words as written, from the first. A Deny on `git push` does not match any of
these:

- `env git push`, `nice git push` or `command git push`, which start with
  another program.
- `git -C . push` or `git --no-pager push`, which put an option first.
- A script or an alias that pushes.

Under a rule that allows the whole tool, those run. A command that is not
simple, such as `true; git push` or `"git" push`, is not denied either: it
asks you. Use a Deny to catch mistakes, not to contain an agent you do not
trust.

### Allowing a whole command tool

A Tool rule that allows `claude/Bash`, `clackatoo/run_in_terminal`,
`clackatoo/run_in_background` or `clackatoo/start_activity` without exact
arguments allows **any simple command**. That includes `sh script.sh`,
`make` and `python3 x.py`, which run code the rule never sees.

An agent allowed to run any command can do what you can. It can read your
files, reach the daemon's socket, and change these rules by writing the
daemon's config. Clackatoo does not try to list the commands that would do
so. Choose such a rule only for an agent you would let work unattended, and
prefer the provider's sandbox for limits that must hold.

### File rules

Read and Edit targets are directories written as absolute paths, `~/…`,
`{project}`, or `{project}/…`. `~` and `{project}` have that meaning in a
rule's target only. A path in a request must be absolute; one that is not,
including one starting with `~`, asks you. Links are resolved before
matching, including the existing parents of new files. Unresolvable paths
ask you.

`{project}` is the chat's git repository, or its working directory outside
one. It matches nothing when that is your home directory or a folder that
contains it, so a project rule never comes to cover everything you own.

These matches require recognizable file inputs, which are Claude Code's own
file tools:

| Tool | Matched as | Path |
| --- | --- | --- |
| Read | Read | The file |
| Grep, Glob | Read | The folder searched, or the chat's working directory when none is given |
| Write, Edit, NotebookEdit | Edit | The file |

A request from one of these tools that names a second path, or a search
pattern that leaves its folder, asks you. An opaque or multi-file provider
approval asks you too. Codex approvals carrying additional consent content
also ask directly, including network-access consent. Clackatoo's own
file-opening tools ask only for a file outside the project, and that
question is always yours to answer; a Deny on a Read rule still refuses it.

### Files that always ask

Some files ask you even if an Allow matches, whether the Allow is a file
rule or a rule for the whole tool. A Deny still refuses them. Their names
are compared in any letter case, and a protected name that is a symbolic
link is still protected.

| Request | Always asks for |
| --- | --- |
| Read or Edit | `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, `~/.docker`, `~/.config/gh` and `~/.config/gcloud` |
| Read or Edit | `~/.claude/.credentials.json`, `~/.codex/auth.json`, `~/.cargo/credentials.toml` |
| Read or Edit | `.env`, `.env.*` and `*.env` files, `.envrc`, `.netrc`, `.git-credentials`, `.npmrc` and `.pypirc`, in any folder |
| Read or Edit | Clackatoo's config and state, and its `mcp-private` folder |
| Edit | Agent instructions and settings: `.clackatoo`, `.claude`, `.codex`, `.agents`, `.cursor` and `.gemini` folders, including skills and specialists; `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `GEMINI.md`, `.cursorrules`, `.mcp.json` and `.github/copilot-instructions.md` |
| Edit | Project files that run code: `.git` (hooks and config), `.vscode`, `.zed`, `.idea`, `.devcontainer`, `.husky`, `.github/workflows`, `.cargo/config.toml`, `.pre-commit-config.yaml`, `lefthook.yml`, `.gitmodules`, `mise.toml`, `.yarnrc.yml` |
| Edit | What runs when you log in or open a shell: `~/.bashrc`, `~/.bash_profile`, `~/.profile`, `~/.zshrc`, `~/.zshenv`, `~/.config/fish`, `~/.gitconfig`, `~/.config/git`, `~/.config/systemd/user`, `~/.config/autostart` and `~/.local/bin` |

Folder names such as `.claude` and `.git` count inside the chat's project
and inside your home directory, not in the path above the project. A chat
whose project is a worktree under `.claude/worktrees` can therefore use an
Edit rule on `{project}`.

This table applies to the file tools listed above. It does not apply to
other tools that reach files, such as an MCP filesystem server or a command:
a rule that allows `managed_fs/write_file` or `claude/Bash` allows writes to
these files. Allow such tools only when you accept that.

The list is a guard against accidents. It does not cover every file that
can run code, such as a build script.

## Recent decisions

The daemon keeps the latest 64 rule decisions in memory, and the page lists
them under **Recent decisions**, read again every two seconds while it's
shown. Each says what was decided of which tool (allowed, asked of you,
denied, or stopped being asked about, for a check that was cancelled), in
which chat and roughly how long ago, with the matching rule number, or why
approval was needed. It does not record action arguments,
command text or file contents. The history clears when the daemon restarts
or upgrades.

Checks that fail or take longer than three seconds ask you.

Changing the rules cancels nothing. A check still in progress is evaluated
again against the new rules. A question you have already been asked is
checked again too. It stays open unless the new rules now allow or deny
what it asks about; then they answer it.

A question stays open for you to answer when its check against the new rules
could not finish: the check timed out, or more than 64 questions were open
at once. A new Deny does not close such a question, so read it before you
allow it.

A check is **Cancelled** when nothing was decided:

- You stopped the turn. Nothing runs, and the agent is told you stopped it
  and not to try again. This holds across a daemon upgrade. It affects that
  turn only: a turn the agent starts later, such as when a background task
  ends, is checked as usual.
- The turn ended, or the chat moved to another session, before the check
  finished. Nothing runs, and the agent is told nobody refused, so it may
  ask again.

Pending checks become human questions before an upgrade; they are never
replayed.

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

Rules belong to the daemon a chat runs on. A chat on another machine follows
that machine's rules and default engine, whichever window shows it, and this
page edits the rules of the daemon the window is connected to. Rules are not
copied between machines.

These are approval rules, not an operating-system sandbox. An approved
program can run other code and make changes the rule does not inspect.
Provider-approved operations can bypass this approval boundary, and an
unsandboxed shell running as you can reach your config and daemon sockets.
The daemon applies changes to its config as they are saved, so a program
that can write `daemon/settings.lua` can change these rules.
Use the provider's sandbox to enforce filesystem and network boundaries.
