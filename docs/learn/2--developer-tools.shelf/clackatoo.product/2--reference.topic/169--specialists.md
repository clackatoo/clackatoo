---
title: Specialists
type: reference
description: Configure reusable agent roles, compose them with skillsets, choose them in chats and restrict delegation and execution.
related:
  - clackatoo/reference/chats-together
  - clackatoo/reference/skills
  - clackatoo/reference/agent-settings
  - clackatoo/reference/config
---

A specialist is a skillset with a runnable role: instructions, a name and
description, optional model and effort defaults, skills, invocation controls
and limits. It can be used in your own chat or by an agent delegating work.
`explore`, `research`, `plan` and `general` are built in; the
[working-together reference](~clackatoo/reference/chats-together#specialists)
describes their defaults.

Use **Explore** for quick, narrow code lookups: locating a definition or
answering a focused question with a brief file reference. Use **Research**
for deeper codebase and architecture investigations: tracing behaviour across
files and available repositories, explaining the evidence, citing inspected
code and distinguishing confirmed findings from inference and open questions.
Both are read-only. Explore chooses a lightweight model; Research inherits
the selected model and effort when delegated to the same provider, and sets
no model or effort override in your own chat. Neither provides access to
repositories outside the chat's available tools and scope.

Built-ins use the same `SPECIALIST.md` format as custom roles and are
embedded in clackatoo. Inspect their instructions or make a custom copy in
Settings; personal and project files can also override a built-in by name.

## Creating and editing specialists

Open **Settings → Agents → Specialists**, or click the cog beside
**Specialists** in the model selector. Choose **Create specialist** or
**Edit** beside an existing role. The form includes instructions,
description, when-to-use guidance, optional provider, model and effort
defaults, skills and skillsets, delegation targets, invocation controls,
managed MCP server selections, and access, time, turn and cost limits.

Instructions accept multiline paste. While editing, <kbd>Ctrl+J</kbd>
inserts a line, <kbd>Enter</kbd> finishes the field, and <kbd>Esc</kbd>
undoes that field edit. **Save** validates the entire draft and saves it;
**Cancel** discards it. <kbd>Ctrl+S</kbd> saves from any field.
Blank optional fields inherit defaults; `[]` in a
list selects none. Model and effort defaults accept a single name or a
JSON object mapping provider names to defaults.

The editor writes `SPECIALIST.md` files on the connected daemon's machine.
Choose **Personal** or **Project** for a new role. Existing files keep their
location. Built-in roles offer **Make a custom copy**, retaining their
instructions and defaults in a new file. Names stay fixed when editing an
existing file. Conflicting edits are refused and leave your draft open.
Running chats keep their existing role; future selections use the saved file.

## Choosing a specialist

Before a new chat's first message, open the model selector. Choose a role in
the **Specialists** column to the right of the model list. The specialist
and model lists scroll independently; <kbd>Tab</kbd> moves between them.
Model search and provider filters leave the specialist list visible.
Once the conversation starts, the column shows the current role with
**New chats only**, and its choices are read-only.
Its instructions accompany the request and its defaults apply when the
session opens. Selecting another specialist after a conversation has started
is refused; open a new chat instead. The selected definition is stored with
the chat. Later edits affect new selections, and do not silently change an
existing chat's role. Its role survives daemon upgrades, saved-workspace
restoration, compaction and moves between agents.

With Tools on, an agent sees routing descriptions and can call `start_chat`
or `start_chats` with `specialist = "reviewer"`. It can select a different
agent or model explicitly; specialist defaults are optional. Your own chat
keeps the agent you chose when selecting a specialist.

## Defining a specialist

A specialist is a folder containing `SPECIALIST.md`, with YAML frontmatter
for metadata and a Markdown body for instructions:

```text
.agents/specialists/reviewer/
  SPECIALIST.md
  skills/check-change/SKILL.md
  references/review-guide.md
```

```markdown
---
name: reviewer
description: Review a change for bugs
when_to_use: Review completed code before merging
model:
  claude: haiku
  codex: gpt-6-luna
effort: low
includes: [shared-checks]
skills: []
delegates_to: [explore]
user_invocable: true
agent_invocable: true
limits:
  access: read-only
  seconds: 120
---

Review changes for concrete bugs. Give file references and explain impact.
```

The name must match its folder and use 1–64 lowercase letters, numbers,
hyphens or underscores. `description` is shown in the picker;
`when_to_use` provides routing guidance. Optional `agent` selects a default
provider for delegation. Model and effort accept a name or mapping by
provider; `model: inherit` keeps the parent's model.

Project files are discovered in `.agents/specialists`, from the chat's
working directory up to its repository root. Personal files are discovered
in the clackatoo config's `specialists` folder and `~/.agents/specialists`.
The nearest project definition wins, followed by clackatoo personal files,
then home personal files and built-ins. Settings creates project files at
the repository root, or in the working directory when there is no repository.
Reopen Settings or start a new chat to pick up external file edits.

Specialists and plain skillsets use the same dependency resolver. A
`SPECIALIST.md` can also live inside a discovered skillset folder, and
relative includes can point to nested specialist folders. Existing
`skillset.toml` bundles with a `[specialist]` table remain readable; when
both entry points exist, `SPECIALIST.md` takes precedence. Lua specialist
and profile definitions are unsupported.

The included `shared-checks` skillset contributes its procedures. If an included bundle is a specialist, it registers another role and keeps its
instructions, defaults, limits and procedures separate. The same composition
works for specialist-in-specialist, specialist-in-skillset,
skillset-in-specialist and skillset-in-skillset. Dependency cycles and
conflicting identities in one dependency graph are errors; repeated shared
dependencies are deduplicated.

### Skills and delegation

`skills` names individual skills from the shared catalogue. `skillsets` can
name additional discovered bundles. With both omitted, a specialist inherits
the shared catalogue. Setting either selects its capabilities explicitly;
empty lists select none from that source. A bundle always retains its own
skills and skills contributed by its plain `includes` dependencies. Missing
named dependencies refuse connection rather than starting an incomplete
role. File reads run on the skills worker, including slash-command loads.

A scoped-out skill is absent from the command menu and automatic catalogue,
and clackatoo refuses loading it through MCP or an explicit slash command.
This controls instruction availability; it is not a filesystem permission
boundary. An agent's ordinary file tools can still read accessible files.

`user_invocable` and `agent_invocable` default to true. Turn either off to
hide and refuse that invocation route. `delegates_to` is an optional list of
specialist names this role may start; an empty list disables delegation.
Other limits on subagent count, depth and scope still apply.

## Limits and budgets

`limits` accepts an access string (`none`, `no-commands`, `read-only`) or a
table with `access` and optional `seconds`, `turns`, `cost_usd`. Access
restrictions retain their existing
[agent-specific enforcement](~clackatoo/reference/chats-together#specialists).
A child inherits each parent ceiling, taking the lower value when it also
sets one. These are ceilings for each chat, not a shared family spending
pool.

`seconds` is a wall-clock limit on each request, including startup and time
waiting on tools or a question. clackatoo cancels the request when it expires
and marks its output as partial. Normal cancellation semantics apply; it is
not a process-kill sandbox. A new request gets a new clock. An upgrade carries
the active deadline rather than restarting it.

`turns` and `cost_usd` are initially supported by Claude Code's adapter only,
through its native maximum-turn and USD-budget options. Choosing them with
another agent fails with an unsupported-limit message. They use the
runtime's own accounting and stopping boundary; USD budgets can exceed the
ceiling by the final model operation. They are request/runtime budgets, not
a cumulative chat or workspace ledger, and are reapplied when a runtime
session opens. A provider change cannot silently drop an unsupported budget.

Custom tool and path permissions, memory, Action Prompts and execution
location defaults remain separate future features.

## Managed MCP servers

A specialist can bundle `mcp.json` and select configured servers with
`mcp_servers: [docs, search]`, or `[]` for none. Its source approval, the chat's
Tools setting and access limits still apply. See [MCP servers](~clackatoo/reference/mcp-servers)
for discovery, bundle composition, credentials and permissions.
