---
title: Editing Files
type: reference
description: An editor pane edits a file with Vim's keys, its text and undo history kept by the daemon. Which page covers files and buffers, languages and language servers, registers, macros and marks, substituting, the view and indenting, and the keys.
related:
  - clackatoo/reference/window
  - clackatoo/reference/cli
  - clackatoo/reference/config
---

An editor pane edits one file, with Vim's keys. Its text lives in the daemon,
in a buffer, as a terminal's screen does, and each window showing it keeps a
copy that it changes as you type, so a key never waits for the daemon.

```text
:e src/main.rs          edit a file in a new tab
:|Editor src/main.rs    the same, beside the focused pane
:w                      write it
:q                      close the pane
```

These pages cover the editor, each listed with its sections:

- [Files and Buffers](~clackatoo/reference/editor-files):
  <span id="opening-a-file"></span>[Opening a file](~clackatoo/reference/editor-files#opening-a-file),
  <span id="folders"></span>[Folders](~clackatoo/reference/editor-files#folders),
  <span id="buffers"></span>[Buffers](~clackatoo/reference/editor-files#buffers),
  <span id="writing"></span>[Writing](~clackatoo/reference/editor-files#writing),
  <span id="recovery"></span>[Recovery](~clackatoo/reference/editor-files#recovery).
- [Languages and Language Servers](~clackatoo/reference/languages):
  <span id="languages"></span>[Languages](~clackatoo/reference/languages#languages),
  <span id="language-packs"></span>[Language packs](~clackatoo/reference/languages#language-packs),
  <span id="language-servers"></span>[Language servers](~clackatoo/reference/languages#language-servers),
  <span id="trusted-folders"></span>[Trusted folders](~clackatoo/reference/languages#trusted-folders),
  <span id="completion"></span>[Completion](~clackatoo/reference/languages#completion).
- [Registers, Macros, and Marks](~clackatoo/reference/registers-macros-marks):
  <span id="registers"></span>[Registers](~clackatoo/reference/registers-macros-marks#registers),
  <span id="macros"></span>[Macros](~clackatoo/reference/registers-macros-marks#macros),
  <span id="marks"></span>[Marks](~clackatoo/reference/registers-macros-marks#marks).
- [Substituting and Lines](~clackatoo/reference/substituting):
  <span id="substituting"></span>[Substituting](~clackatoo/reference/substituting#substituting),
  <span id="lines"></span>[Lines](~clackatoo/reference/substituting#lines).
- [The Editor's View](~clackatoo/reference/editor-view):
  <span id="git-signs"></span>[Git signs](~clackatoo/reference/editor-view#git-signs),
  <span id="agents-changes"></span>[Agents' changes](~clackatoo/reference/editor-view#agents-changes),
  <span id="line-numbers"></span>[Line numbers](~clackatoo/reference/editor-view#line-numbers),
  <span id="tabs-and-indenting"></span>[Tabs and indenting](~clackatoo/reference/editor-view#tabs-and-indenting),
  <span id="the-view"></span>[The view](~clackatoo/reference/editor-view#the-view),
  <span id="rendered-markdown"></span>[Rendered Markdown](~clackatoo/reference/editor-view#rendered-markdown).
- [Editor Keys](~clackatoo/reference/editor-keys):
  <span id="keys"></span>[Keys](~clackatoo/reference/editor-keys#keys),
  <span id="moving"></span>[Moving](~clackatoo/reference/editor-keys#moving),
  <span id="shown-as-you-type"></span>[Shown as you type](~clackatoo/reference/editor-keys#shown-as-you-type),
  <span id="lit-matches"></span>[Lit matches](~clackatoo/reference/editor-keys#lit-matches),
  <span id="jumps"></span>[Jumps](~clackatoo/reference/editor-keys#jumps),
  <span id="changing"></span>[Changing](~clackatoo/reference/editor-keys#changing),
  <span id="insert-mode"></span>[Insert mode](~clackatoo/reference/editor-keys#insert-mode),
  <span id="the-mouse"></span>[The mouse](~clackatoo/reference/editor-keys#the-mouse),
  <span id="known-limits"></span>[Known limits](~clackatoo/reference/editor-keys#known-limits).
