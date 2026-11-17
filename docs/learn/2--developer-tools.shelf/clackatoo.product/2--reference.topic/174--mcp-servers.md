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

The page lists the servers for the project of the pane that was focused when
Settings opened, and says which under its title: `Servers chats can use, for
~/notes on desk`. Each row says where the server is from, what it runs or
where it is, and how it's connected: **Needs review**, **Not in use**,
**Connecting…**, **Connected**, **Needs sign in**, **Failed**, **Ready** or
**Off**. <kbd>Enter</kbd> or **Open** opens a server; **Add a server**, or
`a`, starts a new one. The list is read when the page is shown, after
anything done on it, when a lost connection comes back, and every two seconds
while the page is shown, so there is nothing to refresh by hand. Network,
login and process work runs outside the daemon's event loop.

A server opens as a form, titled `MCP Servers › name`:

| Section | Holds |
|---|---|
| Connection | How it's connected, and what can be done about it as it is now: **Review and enable**, **Test**, **Reconnect**, **Sign in**, **Finish a sign in from another machine** and **Forget the sign in**. Only those that apply are shown: a command has no sign in |
| Server | Its fields. A command has **Command**, **Arguments** and **Environment**; an address has **Address**, **Headers** and **Sign in with a browser**, which adds **Client ID** and **Scopes**. Then **Enabled**, **Read-only server** and how long a call may take |
| Tools | The tools from its last connection or test |
| Remove | **Remove**, pressed twice, for a server that's yours to remove |

**Test** initializes the server and lists its tools without calling them. A
successful test closes its temporary connection; **Ready** means the test
passed, and **Connected** means a chat has a live connection. What an action
came to is said along the foot, and stays there until the next one; what
went wrong is said in full under the title.

Nothing in a form is saved until **Save**, or <kbd>Ctrl</kbd>+<kbd>S</kbd>.
Typing on a field starts it, <kbd>Enter</kbd> finishes it and goes to the
next, <kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd> go from field to
field, and <kbd>Esc</kbd> puts back what the field held. A field with
something wrong in it says so underneath as the keys leave it. Arguments are
written with blanks between them, and quotes round one that has a blank in
it; environment and headers are written as JSON. With changes in the form,
<kbd>Esc</kbd>, **Cancel**, the cog and a click off the panel ask before
they're thrown away. Concurrent edits are refused without overwriting the
newer definition.

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
picked up on the next catalogue refresh: show the MCP Servers page for that
project, open a new chat, or call `list_skills`. They are not watched
continuously in this version.
**Reconnect** restarts an existing proxy's upstream connection without replaying
tool calls. Interrupted or timed-out calls may already have had effects; check
their outcome before retrying. If the provider already disconnected its proxy,
reopen that chat. Active proxy processes survive live daemon upgrades; pending
approvals are withdrawn, and a Settings action interrupted by one says so, for
you to check the list before trying it again.

## Credentials and browser login

Use **Private credential**, under the list, to save a value under a local
name, then reference
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

For OAuth servers, turn on **Sign in with a browser**, optionally set a public
client ID and scopes, save, then choose **Sign in** in the server's form. The
flow discovers server metadata and
uses PKCE, state validation and a loopback callback. Tokens refresh through
the SDK and are bound to this server's configuration source and OAuth client.
**Forget the sign in**, pressed twice, cancels sign-in and revokes its local
credential generation, preventing a late callback or refresh from restoring
forgotten credentials. It does not revoke tokens at the remote service.

A server that needs a login has a gold **Sign in** button on its row of the
server list, before **Open**: a click, or <kbd>Enter</kbd> with the keys on
it (`h` and `l` move between a row's buttons, which start on **Open**),
starts the same sign-in. The
row says **Signing in…** while the browser is open, then **Signed in**, or
why it failed, with **Sign in** to try again. When a chat's agent can't use a
managed server because it needs a login, whichever agent it is, the chat
shows an **MCP sign in** shelf above its box with the same button and the
same states, in every window showing the chat. Signing in reconnects the
server's live connections; an agent that already gave up on the server uses
it from its next session. Only the user signs in: an agent with access to
the window, a phone and the web are refused, as they are for the rest of MCP
Servers.

For a remote daemon, complete sign-in in your local browser. If the final
localhost redirect cannot reach the daemon, copy the complete callback URL
from the browser's address bar into **Finish a sign in from another machine**,
in the server's form while it's signing in. That field is masked. A login expires after five minutes. Confidential-client secrets and
legacy HTTP+SSE endpoints are not supported.

## Tools and permissions

Every server keeps its own identity as `managed_<name>`; clackatoo's built-in
tools remain separate. With **Tools: Ask**, external calls ask for **Allow
once** or **Reject** before forwarding. Allowing one does not enable other
servers or change the built-in tools' permission. Providers may also apply
their own approval controls. **Tools: Off** and side questions receive no
managed servers.

A specialist with read-only or no-command limits receives only servers you
explicitly mark **Read-only server**. This is your classification of the whole
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

Whether agents also use the servers set up in their own settings is on
**Settings → Agents → Agents**, under **Servers from the agent's own
configuration**, beside the native plugins. **Let agents use their own MCP
servers** is off by default. **Claude Code's own servers** and **Codex's own
servers** each choose **As above**, which follows that switch, **Never** or
**Always**. Like the native plugin policy, changes take effect after a daemon
restart or upgrade.

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

**Copy servers from Claude Code** and **Copy servers from Codex**, under the
list, first show the agent's own servers. Pick each server to copy into
personal configuration, then **Copy**. Existing names are
never overwritten. Transport credentials move to private references, and
provider files remain untouched. Imports refuse tool-filter or permission
settings they cannot preserve. Review those entries manually instead.

The native Settings page manages servers for the project it names under its
title, on its connected daemon. The phone uses those servers through its chats; it cannot
edit their configuration or credentials.
