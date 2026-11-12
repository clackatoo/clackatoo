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
connection. Settings refreshes while the page is open. Network, login and
process work runs outside the daemon's event loop.

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
      "timeout_seconds": 30
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

Project and bundled commands need an explicit review of their exact
definition before they start. **Review and enable** approves that revision on
this machine; changing it requires another review. Saving a project definition
through Settings also approves the saved revision. A bundled definition is
customized by copying it into personal configuration; its package stays intact.
Duplicate names in different bundles require unique names or a personal
override. Removing an override may reveal a lower-priority definition.

New sessions receive the current enabled selection. Saving or disabling a
definition through Settings refreshes open chats' grants and withdraws old
proxies' access; reopen the chat for a new definition. Direct file edits are
picked up on the next catalogue refresh: use **Refresh** in MCP Settings for
that project, open a new chat, or call `list_skills`. They are not watched
continuously in this version.
**Reconnect** restarts an existing proxy's upstream connection without replaying
tool calls. Interrupted or timed-out calls may already have had effects; check
their outcome before retrying. If the provider already disconnected its proxy,
reopen that chat. Active proxy processes survive live daemon upgrades; pending
approvals are withdrawn, and interrupted Settings actions require a refresh.

## Credentials and browser login

Use **Private credential** to save a value under a local name, then reference
it with `{"secret":"name"}` in an environment variable or HTTP header.
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

For a remote daemon, complete sign-in in your local browser. If the final
localhost redirect cannot reach the daemon, copy the complete callback URL
from the browser's address bar into **Complete remote sign-in**. That field is
masked. A login expires after five minutes. Confidential-client secrets and
legacy HTTP+SSE endpoints are not supported.

## Tools and permissions

Every server keeps its own identity as `managed_<name>`; clackatoo's built-in
tools remain separate. With **Tools: Ask**, external calls ask for **Allow
once** or **Reject** before forwarding. Allowing one does not enable other
servers or change the built-in tools' permission. Providers may also apply
their own approval controls. **Tools: Off** and side questions receive no
managed servers.

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
