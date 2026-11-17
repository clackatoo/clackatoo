---
title: MCP Servers
type: reference
description: Configure shared MCP servers, keep credentials private, review imports and control provider-managed connections.
related:
  - clackatoo/reference/agent-settings
  - clackatoo/reference/skills
  - clackatoo/reference/specialists
  - clackatoo/reference/config
---

Open **Settings → Agents → MCP Servers** to add, edit, disable or remove a
server on the connected daemon's machine. Choose a local command over stdio
or a Streamable HTTP URL. Commands receive literal arguments, without shell
expansion. HTTP requires HTTPS, except for loopback development servers.

Each entry shows its source, connection state and the tools from its last
connection or test. **Test connection** initializes the server and lists its
tools without calling them. A successful test closes its temporary connection;
**Ready** means the test passed, and **Connected** means a chat has a live
connection. A server that failed, or needs a sign-in, keeps saying why until
it next connects. Settings refreshes while the page is open. Network, login
and process work runs outside the daemon's event loop, and a test never holds
up the page's other actions.

The editor keeps an unsaved draft until **Save** or **Cancel**. Enter finishes
a field, Escape undoes that field edit, and Ctrl+S saves the draft. Arguments,
environment and headers accept JSON; Ctrl+J inserts a line in those fields.
Concurrent edits are refused without overwriting the newer definition.

## Configuration files

Personal servers live in `mcp.json` in clackatoo's configuration directory
(`~/.config/clackatoo/mcp.json` by default). `~/.agents/mcp.json` is also read.
Project files are `.agents/mcp.json`, searched from the chat's working
directory up to the repository boundary. The nearest project definition wins,
then personal configuration, then bundled definitions. An invalid shadowing
entry stays invalid rather than enabling a lower-priority server.

```json
{
  "servers": {
    "docs": {
      "url": "https://mcp.example.org/mcp",
      "headers": {"Authorization": {"secret": "docs-token"}},
      "timeout_seconds": 30,
      "call_timeout_seconds": 600
    },
    "local-search": {
      "command": "my-search-server",
      "args": ["--stdio"],
      "env": {"API_KEY": {"secret": "search-key"}},
      "read_only": true,
      "enabled": true
    }
  }
}
```

A file that can't be read, as one caught half written, costs a chat only the
servers defined in it. The chat says which file, the rest start as usual, and
a connection already using one of that file's servers keeps the definition it
has.

### Timeouts

| Setting | Default | Range | Applies to |
|---|---|---|---|
| `timeout_seconds` | 30 | 1 to 600 | Connecting, `initialize`, and every request that isn't a call, such as listing tools. |
| `call_timeout_seconds` | 600 | 1 to 86400 | Each tool call, and any other request that needs approval, such as reading a resource. |

A call's time starts when it's sent to the server, after any approval, and
starts again each time the server reports progress on it. A call that runs
out is cancelled at the server and fails with an error naming the setting; it
may still have taken effect. `initialize` is never cancelled: a server that
doesn't answer it in time is connected to again instead.

The Settings editor shows `timeout_seconds` as **Request timeout**. Set
`call_timeout_seconds` in the file: saving a server from the editor keeps
the value the file has.

Project and bundled commands need an explicit review of their exact
definition before they start. **Review and enable** approves that revision on
this machine; changing it requires another review. Saving a project definition
through Settings also approves the saved revision. A bundled definition is
customized by copying it into personal configuration; its package stays intact.
Duplicate names in different bundles require unique names or a personal
override. Removing an override may reveal a lower-priority definition.

New sessions receive the current enabled selection. Direct file edits are
picked up on the next catalogue refresh: use **Refresh** in MCP Settings for
that project, open a new chat, or call `list_skills`. They are not watched
continuously in this version.

## Connections

Each chat reaches each of its servers through a proxy process of its own,
which the agent starts as an ordinary MCP server. The proxy stays up for the
life of the agent's session, whatever happens on either side of it:

| What happens | What the agent sees |
|---|---|
| The server crashes, or its connection drops | Calls in flight fail with an error saying their outcome is unknown. The proxy connects again, waiting half a second at first and up to 30 seconds between tries, sends the server the agent's `initialize` again, and tells the agent its tool list changed. |
| The server needs a sign-in | The proxy answers `initialize` itself and lists no tools. Calls fail with an error asking for a sign-in. Once you sign in it connects and tells the agent its tool list changed. |
| The server can't be started or reached | As for a sign-in, with the reason; the proxy keeps trying. |
| You save a change to the definition, choose **Reconnect**, or change a credential it uses | Calls waiting or in flight fail with an error, and the same proxy connects again with the new definition. |
| The daemon restarts or upgrades | Calls are refused with an error saying to try again, since they can't be checked. Everything else carries on, and the server's connection is untouched. The proxy gives up only after two minutes without the daemon. |
| An answer is over 4 MiB | That call fails with an error giving the size. |

Only three things end a proxy: the agent closing it, the daemon staying away
for two minutes, and the chat losing the server. A chat loses a server when
you remove or disable it, change a project or bundled definition without
reviewing it, turn the chat's **Tools** off, or pick a specialist that doesn't
have it. If the agent starts a second proxy for the same server, it takes over
at once and the first stops.

**Reconnect** starts the server's connections again without testing it first,
so a server that holds a lock or a port is never started twice. No call is
sent a second time. Interrupted or timed-out calls may already have had
effects; check their outcome before retrying. Pending approvals don't cross a
daemon upgrade: those calls fail with an error saying to try again.

The daemon's log records each proxy opening and closing, every refusal,
reconnection and change of state, and each approval asked and answered, with
the chat, the server and the reason.

## Credentials and browser login

Use **Private credential** to save a value under a local name, then reference
it with `{"secret":"name"}` in an environment variable or HTTP header.
Changing a credential reconnects the servers that reference it, and no others.
For an Authorization header, store the complete value, including `Bearer `
when needed. `{"env":"VARIABLE"}` reads a variable from the daemon's
environment. Plain strings are for nonsecret configuration. Do not put secrets
in command arguments or URLs.

Credentials and browser tokens are stored separately under `mcp-private/` in
the personal configuration directory, with owner-only directory and file
permissions. They are never copied into project files. Credential fields are
masked in Settings. Known stored credential values are removed from retained
connection diagnostics. Local commands receive a minimal environment plus
their configured variables, and never receive the proxy's Clackatoo session
token. They still run as your user; this is not an operating-system sandbox.

For OAuth servers, enable **OAuth required**, optionally set a public client ID
and scopes, then choose **Sign in** on the server's details page. The flow discovers server metadata and
uses PKCE, state validation and a loopback callback. Tokens refresh through
the SDK and are bound to this server's configuration source and OAuth client.
**Forget browser login** cancels sign-in and revokes its local credential
generation, preventing a late callback or refresh from restoring forgotten
credentials. It does not revoke tokens at the remote service.

A server that needs a login has a gold **Sign in** button on its row of the
server list, beside **Manage**: a click, or <kbd>Enter</kbd> with the keys on
it (`l` and `h` move between a row's buttons), starts the same sign-in. The
row says **Signing in…** while the browser is open, then **Signed in**, or
why it failed, with **Sign in** to try again. When a chat's agent can't use a
managed server because it needs a login, whichever agent it is, the chat
shows an **MCP sign in** shelf above its box with the same button and the
same states, in every window showing the chat. Signing in connects the
server for every chat waiting on it, with no need to reopen them. Only an
HTTP server can ask for a sign-in; a command that fails for want of
credentials shows as an error, with what it wrote. Only the user signs in: an agent with access to
the window, a phone and the web are refused, as they are for the rest of MCP
Servers.

For a remote daemon, complete sign-in in your local browser. If the final
localhost redirect cannot reach the daemon, copy the complete callback URL
from the browser's address bar into **Complete remote sign-in**. That field is
masked. A login expires after five minutes. Confidential-client secrets and
legacy HTTP+SSE endpoints are not supported.

## Tools and permissions

Every server keeps its own identity as `managed_<name>`; clackatoo's built-in
tools remain separate. **Tools: Off** and side questions receive no managed
servers. With **Tools: Allow**, calls go straight to the server. With
**Tools: Ask**, each call asks first:

| Answer | Effect |
|---|---|
| **Allow** | This call goes ahead. |
| **Always allow this server** | This call goes ahead, and the chat stops asking about this server. Other servers, and the built-in tools, still ask. |
| **Reject** | The call fails, and the agent is told you refused it. |

Setting the chat's **Tools** again, to any value, takes back every server
allowed this way. Under **Clackatoo permission rules** the rules decide
instead, with a tool written `managed_<name>/<tool>`: a call no rule covers
asks with **Allow** and **Reject** only.

The question waits for as long as you take, as the built-in tools' questions
do. It's taken back, and the call fails with the reason, when the agent
cancels the call, you stop the turn, the server is reconnected, or the chat
loses the server. The agent is told you refused only when you chose
**Reject**. Parameters over 64 KiB can't be shown, so a call that large fails
rather than ask; allow the server for the chat to send it. Calls over 256 KiB
are always refused.

You're asked once per call. Claude Code and Codex may ask about the same call
themselves in their manual modes; where the agent's adapter says the call is
for a server clackatoo's proxy is serving, that question is answered for you
and only clackatoo's is shown. Other agents' own questions still appear.

A specialist with read-only or no-command limits receives only servers you
explicitly mark **Read-only**. This is your classification of the whole
server, independent of server-supplied tool annotations; it does not inspect
or sandbox each tool's implementation. Classify only servers you trust to
honour that boundary. Calls still follow the chat's Tools setting.

## Skillsets and specialists

A skillset or specialist can include `mcp.json` beside its manifest. Ordinary
skillset servers join the project's catalogue; a server belonging only to a
specialist becomes available when that specialist is selected. Bundles still
require local review before execution.

Use `mcp_servers: [docs, local-search]` in `SPECIALIST.md` frontmatter to select
specific configured names, or `mcp_servers: []` for none. With no explicit
selection, a specialist inherits shared personal/project servers and servers
from its selected skillsets and included bundles. Selection and source review
both apply; a specialist never approves a project command for you.

## Provider configuration

**Allow provider-managed MCP servers** is off by default. Separate Claude Code
and Codex overrides choose **Inherit**, **Off** or **Allow**. Like the native
plugin policy, changes take effect after a daemon restart or upgrade.

The bundled Claude adapter uses strict MCP configuration. The bundled Codex
adapter disables servers from its native configuration layers before adding
Clackatoo's selection, and disables its app connector feature in managed-only
sessions. Older unverified Claude/Codex adapters are refused in managed-only
mode. Where clackatoo can install the bundled adapter, the chat offers it, with
an Install button, and a chat already running an older one starts again on the
bundled adapter once it's there; otherwise the chat says to let clackatoo run
its own adapter or to allow native MCP.
Other custom adapters receive the managed selection, but their native MCP
suppression is not verified; the chat warns about this limitation. Native
plugin controls remain a separate setting.

**Import from Claude Code** and **Import from Codex** first show a preview.
Select each server to import into personal configuration. Existing names are
never overwritten. Transport credentials move to private references, and
provider files remain untouched. Imports refuse tool-filter or permission
settings they cannot preserve. Review those entries manually instead.

The native Settings page manages servers for the selected project on its
connected daemon. The phone uses those servers through its chats; it cannot
edit their configuration or credentials.
