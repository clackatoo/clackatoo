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
[work indicator](~clackatoo/reference/background-work). Queries can return
useful partial results while the rest of the project is being indexed.

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

Changed source files are reindexed individually. New or moved directories,
changes to ignore rules, and missed filesystem events request a background
reconciliation. Metadata reconciliation also runs every two minutes while a
project remains in use. Unchanged files reuse their persistent records.
Changing the installed grammar or extraction version invalidates those
records on the next reconciliation.

Unsaved editor text replaces disk records in a query's results and is marked
`unsaved`. It is never written into the persistent source index. If editor
text changes while a query runs, the agent is asked to retry rather than
being given an outline of the earlier editor version.

Coverage reports whether reconciliation is complete, which work is in
progress, how many files were indexed or unsupported, cache hits, skipped
files and errors. Complete reconciliation means the configured source set
has been visited; it does not mean every language or generated file was
indexed. Filesystem contents can change after any query's snapshot.

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

Up to four projects retain live index state; unused projects release it
after ten minutes. Syntax trees are discarded after extraction, and symbol
records stay in a disk database with a small page cache. Each background
worker reuses at most four grammars, avoiding repeated compilation for
warm file updates without retaining whole-project syntax trees. Directory watches
are bounded at 8,192 per project; coverage says when watches are incomplete,
and metadata reconciliation still checks for changes. A database is capped
at 512 MiB. Cached databases remain available for later sessions.
