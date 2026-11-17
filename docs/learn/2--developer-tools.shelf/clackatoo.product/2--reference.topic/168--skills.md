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
in a working directory, scoped by its selected specialist. Only the names and
the start of each description go to the agent initially; the full
instructions are loaded when needed.

The catalogue goes with a session's first ordinary request, and again with the
next one after a completed compaction. It's kept short, as every chat pays for
it, and every chat an agent starts:

- **Each skill is one line**: its name and about the first 200 characters of
  its description, cut at a word and ending in `…`.
- **The whole is at most 4 KiB.** With more skills than fit, it ends by saying
  that `list_skills` has the rest.
- **It isn't sent again when it changes.** An agent that needs the current
  list calls `list_skills`.
- **The built-in `explore`, `research` and `plan` specialists get none**, as
  they choose no skills.
- **A daemon upgrade sends it once more** to each chat.

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
directory; `list_skills` reads the catalogue again and lists it: the skills
the chat is offered and those it can't use, each with the start of its
description, in at most 32 KiB. It names nothing from a folder that isn't
trusted, only how many things weren't loaded. Neither tool executes a
script, changes the agent's model, or grants permissions. The agent uses its
usual tools to follow the procedure.

## Browsing skills

Open `:Settings`, then **Skills** under the **Agents** category in Easy mode.
The page lists the skills for the project of the pane that was focused when
Settings opened, and says which under its title. Each entry shows its
description, where it's from, who starts it (you with its slash command,
agents when it fits, or both) and its file's path.
Skills from a skillset include the bundle's name in their source. Search for
that name to see the bundle's skills.
**Open its file**, or `o` on a row, opens its `SKILL.md` in an editor on the
workspace's machine. The Settings search also finds skills by
name, description, source and path. With no skills, the page says where to
put one.

Each skill has a switch. <kbd>Enter</kbd>, `l` or `h`, or a click on it,
switches the skill off, or on again, for every agent, by adding its name to
[`skills.disabled`](#switching-skills-off) or taking it out; `o`, or **Open
its file**, opens its file. A skill switched off stays in the list, saying so.

Under **Not loaded** are the skills that can't be used, each with the reason:
its file is broken, it asks for something shared skills don't do, or its
project isn't trusted, which `:trust` puts right. Specialists and skillsets
that couldn't be loaded are listed there too, by name, with what's wrong with
them.

The list is read each time the page is shown. A new chat and `list_skills`
also read the folders again; a catalogue read in the last two seconds is used
as it is, so the chats an agent starts together share one read. What you
change in your own folders reaches the chats in every project the next time
any of them reads.

`load_skill` and a slash invocation read only the skill's own file, each time.
So edits to an existing procedure apply on its next use, and a removed or
newly invalid skill is refused rather than run from a stale copy.

## Where skills live

Put a project's skills in `.agents/skills/NAME/SKILL.md`. Put personal skills
in `~/.agents/skills/NAME/SKILL.md`, or `skills/NAME/SKILL.md` in clackatoo's
config directory. A skill's folder is its name: `name` in its `SKILL.md` must
say the same.

Three kinds of folder are read, each trusted differently:

| Folders | How they're read |
|---|---|
| Your own: `~/.agents/skills`, and `skills` in clackatoo's config directory | Skills may be grouped in folders, up to four deep. Symlinked folders and files are followed, without walking cycles. |
| A project's: `.agents/skills` | As your own, but only once you [trust the project](#project-trust), and never through a symlink. |
| Other products': `~/.claude/skills`, `~/.codex/skills`, and a project's `.claude/skills` | One folder deep only, `NAME/SKILL.md`, as those products document them. A project's needs trust too. |

So what another product keeps in its folder for itself isn't offered. The
skills the Claude desktop app syncs into `~/.claude/skills/synced` are deeper
than one folder. Those Codex bundles are in `~/.codex/skills/.system`, and
folders whose names start with a dot are skipped everywhere.

A project's folders get half the room a catalogue has, 128 skills, so a
project with a great many never leaves your own unread.

Project folders are searched from the chat's working directory up to its
project root: the repository it's in, or else the working directory itself.
Nothing above the project is read, and neither is a project folder that
belongs to another user, such as a shared scratch directory. Your home
directory is never read as a project.

The closest project copy wins; within a directory, `.agents/skills` wins over
`.claude/skills`. Personal clackatoo skills come next, then personal
`.agents`, Claude and Codex folders in that order. Only the chosen copy is
offered as a command. A broken higher-priority copy does not enable a
lower-priority one: its folder's name is taken either way.

Native plugin packages and their installation registries are not imported.
To share an instruction-only skill from one, place or symlink its skill
folder in one of your own skill directories. This supplies its files; it does
not install the plugin's tools or other dependencies.

### Project trust

A cloned repository can hold anything, so a project's skills, skillsets and
[specialists](~clackatoo/reference/specialists) load only from a folder you've
trusted. It's the same trust that lets a project's
[language servers](~clackatoo/reference/languages#trusted-folders) run:
`clackatoo trust` in the folder, or `:trust FOLDER`, and `:trusted` lists
what's marked.

- **Until then no skill, skillset or specialist in the project is read.**
  What its folders hold is listed in Settings by folder name, as not loaded,
  and a new chat there says so once. Only names that could be a skill's are
  listed, and agents aren't given them.
- **A project's `.agents/mcp.json` is the exception**: it's read whatever the
  folder's trust, from folders that are yours. Its
  [servers](~clackatoo/reference/mcp-servers) are only listed until you've
  reviewed each one's exact definition, which is the stricter check.
- **Your own skill of the same name is still offered.** A project that isn't
  trusted changes nothing.
- **Untrusting a folder takes its skills from open chats at once**, before
  anything is read again. Trusting one reads its chats' catalogues again, and
  they gain its skills when that read ends; a read that fails is tried again.
- **Trust never lets a project replace a built-in specialist** or loosen one
  of your own: see [Specialists](~clackatoo/reference/specialists#defining-a-specialist).

### Switching skills off

`skills.disabled` is a daemon option listing the skills never offered to any
agent, by name, with spaces or commas between them:

```lua
daemon.skills.disabled = "docx pptx imagegen"
```

Each skill's switch on the Skills page edits this option, so switching one off
needs no typing. A skill switched off stays listed in Settings, saying so. It has
no slash command and no line in the catalogue, and `load_skill` refuses it.
Open chats lose it as the option is saved. A specialist that names it in
`skills` can't start, and says the skill is switched off.
Skillsets' skills are switched off the same way. The option takes effect as
the config is saved. There's no switch for one agent alone: every agent
sharing the same skills is what they're for.

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

Unknown fields and future format numbers are refused. A bundle that's refused
is listed under **Settings → Agents → Skills** with the reason, and the reason
is in the daemon log too. Hooks are reserved for later capability support; a
bundle's MCP servers are described under
[Bundled MCP servers](#bundled-mcp-servers). Bundles run no installation
commands, fetch no dependencies and start no services. Install one by placing
or symlinking its folder into a skillset directory; the Skills page lists it
the next time it's shown.

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
lowercase letters, digits, hyphens, underscores or colons, and match the
skill's folder: one that doesn't is listed as unavailable, saying to rename
the folder or change `name`. The file may start with a byte-order mark, use Windows line
ends, and end at its closing `---`. Names reserved by
built-in commands, such as `compact`, `init` and `review`, cannot be used.
Descriptions should say when to use the skill. Supporting paths are relative
to the skill's directory, which is supplied with the instructions.
`$ARGUMENTS` is replaced by the words after the command. A skill that doesn't
use it gets them after its instructions instead, as `User arguments:`.
`${CLAUDE_SKILL_DIR}` is also accepted as a reference to the skill's directory.

`disable-model-invocation: true` makes a skill user-only: it stays in the
slash menu, but is omitted from the automatic catalogue and refused by the
agent's loading tool. `policy.allow_implicit_invocation: false` in a skill's
`agents/openai.yaml` has the same effect. `user-invocable: false` hides the
slash command and leaves automatic selection available. Both controls can
be set to leave neither kind of invocation available.

Runtime-specific execution settings, including `context`, `agent`, `model`,
`effort`, `hooks`, `allowed-tools`, `disallowed-tools`, `background`, `shell`
and named `arguments`, are not executed by shared skills. Such a skill is
shown as unavailable with its reason. Dynamic shell expansion, a line that
starts with `` !` `` outside a code block, and positional `$ARGUMENTS[N]`
expansion are also unsupported. Files larger than 64 KiB or
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
alongside shared skills. A command the agent offers itself stays the agent's:
a shared skill of the same name isn't offered as a slash command, though an
agent can still load it. This
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

## Bundled MCP servers

Skillsets and specialists can contain `mcp.json` beside their manifest. The
same bundle composition supplies their server definitions; each project or
bundled definition still needs explicit local review. Manage connections,
credentials and provider imports in [MCP Servers](~clackatoo/reference/mcp-servers).
