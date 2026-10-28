---
title: Tracing agents
type: how-to
description: Keep every message clackatoo and an agent's adapter exchange over ACP, to see what went wrong in a chat, and run the tests that talk to a real Claude Code or Codex.
related:
  - clackatoo/reference/agents
  - clackatoo/reference/cli
---

A chat talks to its agent through the agent's adapter, a program speaking ACP, the Agent Client Protocol: JSON-RPC messages, one a line, over the adapter's stdin and stdout. When a chat does something you didn't expect, the messages show whether the agent said it or clackatoo made it so.

## Turning a trace on

Start the daemon with `CLACKATOO_ACP_TRACE` set to a directory:

    CLACKATOO_ACP_TRACE=~/acp-traces clackatoo daemon

Every agent the daemon starts from then on writes to a file of its own there, named for the agent and the adapter's process ID, as `claude-12345.log`. A chat whose agent stops and starts again gets a new file. The directory is made if it isn't there, readable by you only, and so is each file.

Each line is one message or one line the adapter wrote to stderr:

    2026-10-28T22:08:42.021Z agent claude[12345] -> {"jsonrpc":"2.0","id":0,"method":"initialize",...}

The time is in UTC. `->` is what clackatoo sent, `<-` what the adapter sent, and `!!` a line of its stderr.

## What's left out

A trace is meant to be shareable, so obvious secrets are replaced by `[redacted]`:

- the string values of fields named for keys, tokens, passwords, secrets, cookies and credentials (counts like `inputTokens` stay);
- every value of an MCP server's environment and headers (their names stay);
- bearer tokens, and API keys by their prefixes, such as `sk-`, `ghp_` and `xoxb-`, wherever they are.

This is a safety net, not a guarantee: files' text and your messages are in a trace as they were sent, so read one before you share it.

A message over 1 MiB, such as an edit to a big file, whose diff carries the whole file, is traced as its first 64 KiB and its length.

## How big it gets

Past 16 MiB, a trace file is set aside as `….log.1`, replacing the one set aside before, and a new one begun. So each agent keeps at most about 32 MiB, however long the daemon runs with the trace on. Old files aren't deleted: clear the directory when you're done.

## Tests with real agents

The daemon's tests named `a_real_…` talk to a real Claude Code or Codex. Each is ignored unless asked for, uses the cheapest model, and traces to `$TMPDIR/acp-NAME`:

    cargo build --workspace --examples
    cd crates/daemon
    CLACKATOO_DISPLAY=off cargo test --test daemon a_real_session_of_six_turns -- --ignored --nocapture

Run them one at a time. They use the accounts `claude` and `codex` are logged in to. Most run the adapter the daemon would run, unless `CLACKATOO_REAL_AGENT` (Claude Code) or `CLACKATOO_REAL_CODEX` (Codex) gives a command, and with none say they were skipped and pass; a few older ones need `CLACKATOO_REAL_AGENT`.
