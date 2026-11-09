---
title: Agents
type: reference
description: A pane can hold a chat with an agent such as Claude Code or Codex instead of a shell. Which page covers chatting, the agent's settings, what a chat shows, an agent's changes to files, chats working together, the tools agents can use, and what lasts.
related:
  - clackatoo/reference/window
  - clackatoo/reference/cli
---

A pane can hold a chat with an agent, such as Claude Code, instead of a shell.
You type a message in the box along the bottom, the agent answers above it,
and it works on the files in the pane's directory as it would in its own
program.

These pages cover chats and their agents, each listed with its sections:

- [Chatting with an Agent](~clackatoo/reference/chatting):
  <span id="opening-a-chat"></span>[Opening a chat](~clackatoo/reference/chatting#opening-a-chat),
  <span id="the-start-page"></span>[The start page](~clackatoo/reference/chatting#the-start-page),
  <span id="queuing-and-steering"></span>[Queuing and steering](~clackatoo/reference/chatting#queuing-and-steering),
  <span id="viewing-images"></span>[Viewing images](~clackatoo/reference/chatting#viewing-images),
  <span id="images-returned-by-tools"></span>[Images returned by tools](~clackatoo/reference/chatting#images-returned-by-tools),
  <span id="pasting-images"></span>[Pasting images](~clackatoo/reference/chatting#pasting-images),
  <span id="typing"></span>[Typing](~clackatoo/reference/chatting#typing),
  <span id="slash-commands"></span>[Slash commands](~clackatoo/reference/chatting#slash-commands),
  <span id="mentioning-files"></span>[Mentioning files](~clackatoo/reference/chatting#mentioning-files),
  <span id="messages-sent-and-drafts"></span>[Messages sent and drafts](~clackatoo/reference/chatting#messages-sent-and-drafts).
- [An Agent's Settings](~clackatoo/reference/agent-settings):
  <span id="gemini-through-antigravity"></span>[Gemini through Antigravity](~clackatoo/reference/agent-settings#gemini-through-antigravity),
  <span id="the-agent-s-settings"></span>[The agent's settings](~clackatoo/reference/agent-settings#the-agent-s-settings),
  <span id="moving-a-chat-to-another-agent"></span>[Moving a chat to another agent](~clackatoo/reference/agent-settings#moving-a-chat-to-another-agent),
  <span id="which-agent"></span>[Which agent](~clackatoo/reference/agent-settings#which-agent).
- [What a Chat Shows](~clackatoo/reference/chat-view):
  <span id="normal-mode"></span>[Normal mode](~clackatoo/reference/chat-view#normal-mode),
  <span id="yanking"></span>[Yanking](~clackatoo/reference/chat-view#yanking),
  <span id="searching"></span>[Searching](~clackatoo/reference/chat-view#searching),
  <span id="what-the-chat-shows"></span>[What the chat shows](~clackatoo/reference/chat-view#what-the-chat-shows),
  <span id="a-command-s-output-in-a-terminal"></span>[A command's output in a terminal](~clackatoo/reference/chat-view#a-command-s-output-in-a-terminal),
  <span id="what-it-runs-in-the-background"></span>[What it runs in the background](~clackatoo/reference/chat-view#what-it-runs-in-the-background),
  <span id="what-a-background-command-printed"></span>[What a background command printed](~clackatoo/reference/chat-view#what-a-background-command-printed),
  <span id="claude-code-s-own-subagents"></span>[Claude Code's own subagents](~clackatoo/reference/chat-view#claude-code-s-own-subagents),
  <span id="the-agent-s-plan"></span>[The agent's plan](~clackatoo/reference/chat-view#the-agent-s-plan),
  <span id="turns-the-agent-takes-by-itself"></span>[Turns the agent takes by itself](~clackatoo/reference/chat-view#turns-the-agent-takes-by-itself),
  <span id="when-a-turn-fails"></span>[When a turn fails](~clackatoo/reference/chat-view#when-a-turn-fails),
  <span id="what-else-a-turn-says"></span>[What else a turn says](~clackatoo/reference/chat-view#what-else-a-turn-says),
  <span id="when-the-agent-asks"></span>[When the agent asks](~clackatoo/reference/chat-view#when-the-agent-asks),
  <span id="questions-forms-and-plans"></span>[Questions, forms, and plans](~clackatoo/reference/chat-view#questions-forms-and-plans),
  <span id="selecting-and-copying"></span>[Selecting and copying](~clackatoo/reference/chat-view#selecting-and-copying),
  <span id="desktop-notifications"></span>[Desktop notifications](~clackatoo/reference/chat-view#desktop-notifications).
- [An Agent's Changes to Files](~clackatoo/reference/agent-changes):
  <span id="the-files-it-changes"></span>[The files it changes](~clackatoo/reference/agent-changes#the-files-it-changes),
  <span id="undoing-a-turn"></span>[Undoing a turn](~clackatoo/reference/agent-changes#undoing-a-turn),
  <span id="rewinding-and-branches"></span>[Rewinding and branches](~clackatoo/reference/agent-changes#rewinding-and-branches),
  <span id="reviewing-its-changes"></span>[Reviewing its changes](~clackatoo/reference/agent-changes#reviewing-its-changes).
- [Chats Working Together](~clackatoo/reference/chats-together):
  <span id="chats-working-together"></span>[Chats working together](~clackatoo/reference/chats-together#chats-working-together),
  <span id="subagents"></span>[Subagents](~clackatoo/reference/chats-together#subagents),
  <span id="a-subagent-s-pane"></span>[A subagent's pane](~clackatoo/reference/chats-together#a-subagent-s-pane),
  <span id="closing"></span>[Closing](~clackatoo/reference/chats-together#closing),
  <span id="profiles"></span>[Profiles](~clackatoo/reference/chats-together#profiles),
  <span id="subagent-limits"></span>[Subagent limits](~clackatoo/reference/chats-together#subagent-limits).
- [What an Agent Can Use](~clackatoo/reference/agent-tools):
  <span id="what-the-agent-can-show-you"></span>[What the agent can show you](~clackatoo/reference/agent-tools#what-the-agent-can-show-you),
  <span id="the-browser"></span>[The browser](~clackatoo/reference/agent-tools#the-browser),
  <span id="native-window-access"></span>[Native window access](~clackatoo/reference/agent-tools#native-window-access),
  <span id="managed-activities-and-recovery"></span>[Managed activities and recovery](~clackatoo/reference/agent-tools#managed-activities-and-recovery),
  <span id="inspecting-capabilities"></span>[Inspecting capabilities](~clackatoo/reference/agent-tools#inspecting-capabilities).
- [What Lasts](~clackatoo/reference/agents-lasting):
  <span id="what-lasts"></span>[What lasts](~clackatoo/reference/agents-lasting#what-lasts),
  <span id="through-an-upgrade"></span>[Through an upgrade](~clackatoo/reference/agents-lasting#through-an-upgrade),
  <span id="known-limits"></span>[Known limits](~clackatoo/reference/agents-lasting#known-limits).
