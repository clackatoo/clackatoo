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
**Specialists** in the model selector. The page lists the specialists for the
project of the pane that was focused when Settings opened, and says which
under its title: yours first, each with where it's kept, then those built in.
Choose **New specialist**, or `a`, or **Edit** beside one of yours. A built-in
role offers **Copy to change**, which keeps its instructions and defaults in
a new file; a copy that keeps the built-in's name is used in its place, and
the form says so.

A specialist opens as a form, titled `Specialists › name`:

| Section | Holds |
|---|---|
| (first) | Its name, where it's kept, its description, when to use it, and **Instructions** |
| Runs as | **Agent** and **Model**, each picked from a list of what the daemon knows, and **Effort** |
| Limits | **May do**: Everything, No commands or Only read; and time, turn and cost limits |
| May use | **Skills**, **Skillsets**, **MCP servers** and **Hands work to**: each as the chat that starts it, none, or only those named |
| Started by | Whether you can start it, and whether agents can hand work to it |

A specialist's instructions are the body of its `SPECIALIST.md`, and aren't
edited in the form: **Open its file** opens the file in an editor beside your
work and closes Settings, as a skill's **Open its file** does. With changes
in the form, or for a new specialist, the button reads **Save, then open its
file**, and does both.

Nothing is saved until **Save**, or <kbd>Ctrl</kbd>+<kbd>S</kbd>. Typing on a
field starts it, <kbd>Enter</kbd> finishes it and goes to the next,
<kbd>Tab</kbd> goes from field to field, and <kbd>Esc</kbd> puts back what
the field held. A field with something wrong in it, such as a time limit
that's no number, says so underneath as the keys leave it. With changes in
the form, <kbd>Esc</kbd>, **Cancel**, the cog and a click off the panel ask
before they're thrown away. What the form doesn't show of a file, such as a
model for each agent, is saved as it was.

The form writes `SPECIALIST.md` files on the connected daemon's machine.
Choose **Personal** or **This project** for a new role. Existing files keep
their location, and their name. Conflicting edits are refused and leave the
form open. Running chats keep their existing role; future selections use the
saved file.

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
The Specialists page reads its list each time it's shown; a new chat picks up
external file edits too.

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
