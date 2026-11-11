---
title: Shared Skills
type: reference
description: Share SKILL.md procedures and skillset bundles across agents, invoke them with slash commands, and control native provider plugins.
related:
  - clackatoo/reference/chatting
  - clackatoo/reference/config
  - clackatoo/reference/agent-tools
---

A skill is a procedure in a `SKILL.md` file, with optional supporting files
and scripts beside it. clackatoo supplies the same catalogue to every agent
in a working directory, scoped by its selected specialist. Only the names and descriptions go to the agent
initially; the full instructions are loaded when needed.
After a completed compaction, the catalogue accompanies the next ordinary
request again.

## Using a skill

Type `/` in a chat and select a skill, or type its name directly:

```text
/release-check v2.0
```

clackatoo reads the skill's latest instructions and gives them to the agent
with your arguments. The conversation keeps the command you typed. A
specialist's role and any conversation being carried from another agent still
accompany the request.

With the chat's **Tools** on, an agent can also select a skill whose
description matches the task. `load_skill` returns its instructions and
directory; `list_skills` refreshes the catalogue. Neither tool executes a
script, changes the agent's model, or grants permissions. The agent uses its
usual tools to follow the procedure.

## Browsing skills

Open `:Settings`, then **Skills** under the **Agents** category in Easy mode.
Each entry shows its description, source, invocation rules and file path.
Skills from a skillset include the bundle's name in their source. Search for
that name to see the bundle's skills.
**Open file**, or <kbd>Enter</kbd> on a row, opens its `SKILL.md` in an editor
on the workspace's machine. The Settings search also finds skills by name,
description, source and path.

Reopen Settings to refresh the list. New chats, `list_skills` and
`load_skill` also discover skills again. A slash invocation reads the current
file, so edits to an existing procedure apply on its next invocation; a
removed or newly invalid skill is refused rather than run from a stale copy.

## Where skills live

Put a project's skills in `.agents/skills/NAME/SKILL.md`. Put personal skills
in `~/.agents/skills/NAME/SKILL.md`, or `skills/NAME/SKILL.md` in clackatoo's
config directory. Discovery also reads `.claude/skills` in the project,
`~/.claude/skills`, and `~/.codex/skills` as compatibility folders.

Project folders are searched from the chat's working directory up to its
repository root. The closest project copy wins; within a directory,
`.agents/skills` wins over `.claude/skills`. Personal clackatoo skills come
next, then personal `.agents`, Claude and Codex folders in that order. A
broken higher-priority definition does not enable a lower-priority copy.
Only the chosen copy is offered as a command. Skill folders may be nested
up to four levels, and symlinked folders are followed without walking cycles.

Native plugin packages and their installation registries are not imported.
To share an instruction-only skill from one, place or symlink its skill
folder in a shared skill directory. This supplies its files; it does not
install the plugin's tools or other dependencies.

## Skillsets

A skillset bundles ordinary skills under one manifest. Put a project bundle
in `.agents/skillsets/NAME/`, or a personal bundle in
`~/.agents/skillsets/NAME/` or `skillsets/NAME/` in clackatoo's config directory:

```text
.agents/skillsets/release/
  skillset.toml
  skills/
    release-check/
      SKILL.md
      references/release-checklist.md
    changelog/
      SKILL.md
```

The `skillset.toml` manifest is:

```toml
format = 1
name = "release"
description = "Prepare and check releases."
version = "0.1.0"
```

`format`, `name` and `description` are required. `version` is an optional
nonempty string for identifying the bundle's revision. Names use 1–64
lowercase letters, digits, hyphens or underscores. The bundle contributes
every skill under `skills/`, including each skill's supporting files.
Invoke `/release-check` or `/changelog` as usual; bundles add no command
prefix and no separate execution step.

At each project directory, standalone `.agents/skills` and `.claude/skills`
take precedence over skillsets. A closer project still wins over its parent.
Personal clackatoo skills and skillsets come next, then personal `.agents`
skills and skillsets, then the Claude and Codex compatibility skill folders.
The first skillset with a given name replaces the whole lower-priority
bundle, so two revisions never contribute a mixture of skills. An individual
standalone skill can override a bundled skill with the same command name.

Add `includes = ["shared", "./helpers/reviewer"]` to compose bundles.
A name refers to another discovered skillset; a path starting with `./`
refers to a bundle inside this one. Each dependency has its own
`skillset.toml`. Absolute paths, `..`, paths leaving the containing bundle,
cycles and conflicting names in a dependency graph are refused. Shared
dependencies are included once. A bundle's own skills take precedence over
imported skills, then dependencies follow the order in `includes`.

A bundle becomes a [specialist](~clackatoo/reference/specialists) by adding a
`[specialist]` table with instructions. Either kind of bundle can include
either kind. Including a plain skillset contributes its skills; including a
specialist registers a callable role with its own skills, instructions,
defaults and limits. Its instructions and private skills do not merge into
the containing role.

Unknown fields and future format numbers are refused, with the reason in
the daemon log. MCP servers and hooks are reserved for later capability
support. Bundles run no installation commands, fetch no dependencies and
start no services. Install one by placing or symlinking its folder into a
skillset directory, then reopen Settings to refresh.

## Writing a skill

```markdown
---
name: release-check
description: Check a release before publishing it.
argument-hint: version
---

Check the release named in $ARGUMENTS:

1. Read references/release-checklist.md in this skill's directory.
2. Run the relevant tests.
3. Report any problems before publishing.
```

The YAML frontmatter needs a name and description. Names contain at most 64
lowercase letters, digits, hyphens, underscores or colons. Names reserved by
built-in commands, such as `compact`, `init` and `review`, cannot be used.
Descriptions should say when to use the skill. Supporting paths are relative
to the skill's directory, which is supplied with the instructions.
`$ARGUMENTS` is replaced by the words after the command. `${CLAUDE_SKILL_DIR}`
is also accepted as a reference to the skill's directory.

`disable-model-invocation: true` makes a skill user-only: it stays in the
slash menu, but is omitted from the automatic catalogue and refused by the
agent's loading tool. `policy.allow_implicit_invocation: false` in a skill's
`agents/openai.yaml` has the same effect. `user-invocable: false` hides the
slash command and leaves automatic selection available. Both controls can
be set to leave neither kind of invocation available.

Runtime-specific execution settings, including `context`, `agent`, `model`,
`effort`, `hooks`, `allowed-tools`, `disallowed-tools`, `background`, `shell`
and named `arguments`, are not executed by shared skills. Such a skill is
shown as unavailable with its reason. Dynamic shell expansion and positional
`$ARGUMENTS[N]` expansion are also unsupported. Files larger than 64 KiB or
with invalid metadata are shown with their problem.

## Hosted runtimes

Native plugins are off by default for Claude Code and Codex. In
**Settings → Agents → Native plugins**, opt in separately with **Allow
Claude Code plugins** or **Allow Codex plugins**. These daemon options are
`plugins.claude` and `plugins.codex` (`daemon.plugins.claude = true` in Lua).
Changes take effect after a daemon restart or upgrade; an upgrade restarts
an agent whose plugin policy changed.

With native plugins off, Claude Code receives session-local marketplace and
directory-plugin restrictions, plugin syncing is off for the session, the
SDK skill allowlist is empty, and the native `Skill` tool is removed. Codex
receives child-only overrides disabling plugins, the native skills instruction
block and bundled system skills. The user's configuration files are not
edited. Native skill command entries identified by the adapters are filtered
from the slash menu in favour of shared skills. Built-in commands remain.

Opting in lets the runtime apply its own plugin and skill configuration
alongside shared skills. Shared skills win command-name collisions. This
also restores the native skill mechanism so a plugin's skills can work with
its other components. The option permits native plugins; it does not install
or enable any particular plugin.

These controls target the pinned local adapters. Claude's administrator-managed
policy can take precedence over an embedding application's session policy;
built-in runtime components remain the runtime's responsibility. Standalone
provider hooks and MCP configurations are otherwise unchanged.

Other ACP agents receive the shared catalogue and tools too. Their native
skill discovery remains governed by their own runtime; ACP has no universal
switch for turning it off. Native instruction files such as `AGENTS.md` and
`CLAUDE.md` continue to belong to the hosted runtime.
