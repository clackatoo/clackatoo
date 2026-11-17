---
title: Project Code Outlines
type: reference
description: The editor outline, project symbol finder and agent code tools share a background index, with unsaved source and coverage limits.
related:
  - clackatoo/reference/agent-tools
  - clackatoo/reference/languages
  - clackatoo/reference/background-work
---

Clackatoo builds a shared index of source definitions when an editor outline,
project symbol finder or agent first asks for it. Windows and chats in the
same project use the same index.
Indexing runs in the background and appears in the bottom bar's
[work indicator](~clackatoo/reference/background-work). The queries
themselves don't. Queries can return useful partial results while the rest
of the project is being indexed.

The home directory, and any folder above it, is never indexed: it's no
project, and far too much to walk. That's the home directory as it really
is, with links followed. A chat there with no project of its own gets no
symbol search or repository map, and says why. The outline of a single
file there still works, in the editor and through `file_outline`: only
that file is read.

## Editor outline

In a saved source file, `<Space>bo` or `:Outline` toggles an outline on the
editor's right. `<Space>bO` or `:OutlineFocus` focuses it without closing it.
It lists up to 1,024 definitions, indented inside their enclosing definitions,
and reports when there are more. An installed grammar is needed; a language
server is not.

Use `j`/`k` or the arrow keys to select a definition and `Enter` to jump to
it. `Home`/`End` and `Page Up`/`Page Down` move through a long outline. A click
on a definition jumps there; the wheel moves through the list. `Esc` or `h`
returns the keys to the editor, leaving the outline visible; `q` or the
heading's cross closes it. `r` refreshes it. Jumps enter the normal jump
history, so `Ctrl+O` comes back.

The outline follows the current file and highlights the enclosing definition
as the editor's cursor moves. Source edits clear outdated definitions and
refresh after a short pause, including unsaved edits. The panel is local to
this window and stays open when the same editor switches files. In panes
narrower than 64 columns it hides until there is room again.

## Project symbol finder

`<Space>ps` or `:ProjectSymbols` searches across the project using identifier
and path word prefixes, including camel-case components. It works from a
terminal or explorer too. With nothing typed, it shows a bounded initial
page. Up to 200 results are returned; narrow the query when there are more.

The finder shows indexing and partial-coverage status and refreshes partial
results as indexing advances, preserving the selected location. Its usual
preview, `Enter`, `Ctrl+V`/`Ctrl+X`, `Ctrl+T` and jump-history controls apply.
Unsaved editor text replaces cached disk results. Cancelling indexing in
`:Work` leaves partial results available; automatic finder refreshes do not
restart the cancelled job. Reopening the finder, typing a new query or
pressing `r` in the outline explicitly retries indexing.

An older daemon keeps its language-server project search. `<Space>bs` and
`:Symbols` remain the language-server file-symbol picker.

## Agent tools

| Tool | Reads |
|---|---|
| `file_outline` | Definitions, signatures, source lines and import declarations in a relative project file. It can index that file immediately, ahead of the background scan. `limit` defaults to 200, at most 200. |
| `search_symbols` | Definition names and path words matching the query's word prefixes, including camel-case components. `limit` defaults to 50, at most 200. |
| `repo_map` | A compact outline selected by identifier/path words and optional `focus` files. Focus files and their neighbours come first. With no explicit focus, open editor files supply it. `max_bytes` defaults to 12,000, from 1,024 to 32,000. |

These tools read only the chat's project. Paths are relative to its root;
absolute paths and `..` are refused. The tools also work for read-only
specialists. They write derived index data in the daemon's private state
directory without modifying project files.

The repository map uses a strict UTF-8 byte budget so its size does not
depend on a particular provider's tokenizer. All tools return bounded
results. `more` or a final message says when there are further definitions;
narrow the query to find them.

An outline is a navigation aid. It contains declaration signatures and
locations, not the implementations an agent needs to read before changing
behaviour. Search uses identifier and path words; it does not understand
arbitrary natural-language questions. Import declarations are recorded as
syntax. Calls, references and import targets are not resolved, and this
version does not use PageRank.

## Freshness and unsaved files

Changed source files are reindexed individually. A directory that's
created, deleted or moved has what's under it walked again, and nothing
else. A change to ignore rules, or filesystem events the daemon missed,
asks for the whole project to be walked again.

While every directory is watched, that's all: the project isn't walked on
a timer. If some couldn't be watched (there are more than 8,192, the
system has no watches left, or the project's own folder was removed), the
whole project is checked every two minutes while it remains in use. A
directory moved out of the project stops being watched; one made in its
place is watched afresh. Unchanged files reuse their persistent
records, and a walk writes to the index in batches. Changing the installed
grammar or extraction version invalidates those records the next time the
file is met.

Unsaved editor text replaces disk records in a query's results and is marked
`unsaved`. It is never written into the persistent source index. Each
unsaved file is parsed once for each version of its text, however many
queries read it. One that can't be parsed is left out, and the answer says
there is more than it gives. So is anything past 32 unsaved files or 16 MiB
of them.

If the unsaved text a query read changes while it runs, or the file it was
asked about does, the agent is asked to retry rather than being given an
outline of the earlier text. An edit anywhere else in the project doesn't
make an answer stale.

Coverage reports whether reconciliation is complete, which work is in
progress, how many files were indexed or unsupported, cache hits, skipped
files and errors. It also counts the indexed files that aren't whole:
`truncated` ones, which have more than 4,096 definitions and keep the
first, and `failed` ones, which can't be parsed as they are: their grammar
doesn't load.

Complete reconciliation means the configured source set has been visited;
it does not mean every language or generated file was indexed. A file that
fails doesn't leave the index incomplete: it's recorded as failed, counted
each time, and not parsed again until it or its grammar changes. A file
that only ran out of its parsing budget, as on a busy machine, is left out
and counted as skipped, and tried again on the next whole walk or when its
own outline is asked for. A folder that couldn't be read leaves the index
incomplete until a whole walk can read it, as something in it may be
missing; the project is then checked on the two-minute timer too.
Filesystem contents can change after any query's snapshot.

Cancelling indexing leaves its existing cache available as partial data.
Another query can restart indexing after cancellation completes. A daemon
restart reuses the on-disk index and reconciles it before claiming it is
current. Recent activity history is separate from the persistent index.

## Languages and limits

Indexing uses installed [Tree-sitter language packs](~clackatoo/reference/languages).
It never starts language servers or installs grammars automatically. Common
function, method, type, class, module and constant constructs are recognised;
coverage varies with each language's syntax. A file without an installed
grammar is counted as unsupported.

The index respects `.gitignore` and `.ignore`, excludes source symlinks,
and skips `.git`, `node_modules`, `target`, `vendor`, `dist`, `build`,
`__pycache__` and minified JavaScript/CSS. Files over 4 MiB are excluded.
Extraction has a 500 ms parsing budget and at most 4,096 definitions per
file. Traversal stops at one million eligible files. Errors and truncated
coverage are reported rather than silently presented as a complete map.
In C, a struct or enum counts where it's defined with a body, not where
its name is used.

Up to four projects retain live index state; unused projects release it
after ten minutes. Syntax trees are discarded after extraction, and symbol
records stay in a disk database with a small page cache. Each background
worker reuses at most four grammars, avoiding repeated compilation for
warm file updates without retaining whole-project syntax trees. Directory watches
are bounded at 8,192 per project; coverage says when watches are incomplete,
and metadata reconciliation still checks for changes. A database is capped
at 512 MiB.

Cached databases remain available for later sessions, but not for ever.
Each time a project's index is loaded, the daemon clears out the indexes
whose project folder no longer exists, those not written to for 30 days,
and then the least recently written until all of them together take no
more than 2 GiB. Loading a project writes to its index, so that's the one
loaded longest ago. An index in use is never removed, even one loaded
while the clear-out runs. Indexes from before this
layout are built again and the old ones removed.
