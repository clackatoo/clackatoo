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
while the page is shown, so there is nothing to refresh by hand. A server that failed,
or needs a sign-in, keeps saying why until it next connects. Network, login
and process work runs outside the daemon's event loop.

A server opens as a form, titled `MCP Servers › name`:

| Section | Holds |
|---|---|
| Connection | How it's connected, and what can be done about it as it is now: **Review and enable**, **Test**, **Reconnect**, **Sign in**, **Finish a sign in from another machine** and **Forget the sign in**. Only those that apply are shown: a command has no sign in |
| Server | Its fields. A command has **Command**, **Arguments** and **Environment**; an address has **Address**, **Headers** and **Sign in with a browser**, which adds **Client ID** and **Scopes**. Then **Enabled**, **Read-only server**, **Give up connecting after** and **Give up on a tool call after** |
| Tools | The tools from its last connection or test |
| Remove | **Remove**, pressed twice, for a server that's yours to remove |

**Test** initializes the server, as it's saved, and lists its tools without
calling them; with changes in the form it waits for them to be saved. One
test holds up nothing else on the page. A successful test closes its
temporary connection; **Ready** means the test passed, and **Connected**
means a chat has a live connection. A server saved as something else,
somewhere else, while its form has changes says so, and saving the form is
then refused rather than saving over it. What an action
came to is said along the foot, and stays there until the next one; what
went wrong is said in full under the title.

Nothing in a form is saved until **Save**, or <kbd>Ctrl</kbd>+<kbd>S</kbd>.
Typing on a field starts it, <kbd>Enter</kbd> finishes it and goes to the
next, <kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd> go from field to
field, and <kbd>Esc</kbd> stops typing. A field with
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

A server's form in Settings shows `timeout_seconds` as **Give up connecting
after** and `call_timeout_seconds` as **Give up on a tool call after**, each
in seconds. Saving the form saves both as it shows them.

Project and bundled commands need an explicit review of their exact
definition before they start. **Review and enable** approves that revision on
this machine; changing it requires another review. Saving a project definition
through Settings also approves the saved revision. A bundled definition is
customized by copying it into personal configuration; its package stays intact.
Duplicate names in different bundles require unique names or a personal
override. Removing an override may reveal a lower-priority definition.

New sessions receive the current enabled selection. Direct file edits are
picked up on the next catalogue refresh: show the MCP Servers page for that
project, open a new chat, or call `list_skills`. They are not watched
continuously in this version. At that refresh a chat's live connection to an edited
server connects again with the new definition, as it does when you save from
Settings.

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
| A command server writes a line over 32 MiB | The line can't be read, so its connection is given up: calls in flight fail with that reason, and the proxy connects again. |
| A command server writes a line that isn't a JSON message | The line is passed over. The first on each connection is logged and shown as the server's detail in Settings, and given as the reason if the server then fails to initialize. A command server must write one message a line, and nothing else, on its standard output. |
| An HTTP server answers 401 to an `Authorization` header from its definition | The server shows as an error naming that header. No sign-in is offered, since none would help. |

A proxy ends only when:

- the agent closes it, or its session with the chat ends;
- the chat closes;
- the daemon stays away for two minutes;
- the chat loses the server: you remove or disable it, change a project or
  bundled definition without reviewing it, turn the chat's **Tools** off, or
  pick a specialist that doesn't have it;
- the agent starts a second proxy for the same server, which takes over at
  once;
- it couldn't start: the chat already has 32 proxies, or its servers couldn't
  be read for a minute.

**Reconnect** starts the server's connections again without testing it first,
so a server that holds a lock or a port is never started twice. No call is
sent a second time. Interrupted or timed-out calls may already have had
effects; check their outcome before retrying. Pending approvals don't cross a
daemon upgrade: those calls fail with an error saying to try again. A Settings
action an upgrade or a lost connection interrupts says so, for you to check
the list before trying it again.

The daemon's log records each proxy opening and closing, every refusal,
reconnection and change of state, and each approval asked and answered, with
the chat, the server and the reason.

## Credentials and browser login

Use **Private credential**, under the list, to save a value under a local
name, then reference
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
same states, in every window showing the chat. Signing in connects the
server for every chat waiting on it, with no need to reopen them. Only an
HTTP server can ask for a sign-in; a command that fails for want of
credentials shows as an error, with what it wrote. Only the user signs in: an agent with access to
the window, a phone and the web are refused, as they are for the rest of MCP
Servers.

For a remote daemon, complete sign-in in your local browser. If the final
localhost redirect cannot reach the daemon, copy the complete callback URL
from the browser's address bar into **Finish a sign in from another machine**,
in the server's form while it's signing in. That field is masked. A login expires after five minutes. Confidential-client secrets and
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

**Always allow this server** belongs to the chat, whichever agent or branch
it's on, and to the server as it's defined now. A server defined otherwise
under the same name asks again: after any change to its definition, or when a
project's definition takes over from a personal one. A copy of the chat
starts with no servers allowed. Setting the chat's **Tools** again, to any
value, takes back every server allowed this way.

Under **clackatoo's rules** the rules decide instead. A managed
call is named `managed_<name>/<tool>` there, and only rules on that name
apply: a rule on the agent's own name for the tool, such as
`claude/mcp__managed_docs__search`, never does. A call no rule covers asks
with **Allow** and **Reject** only.

The question waits for as long as you take, as the built-in tools' questions
do. It's taken back, and the call fails with the reason, when the agent
cancels the call, you stop the turn, the server is reconnected, or the chat
loses the server. The agent is told you refused only when you chose
**Reject**. Parameters over 64 KiB can't be shown, so a call that large fails
rather than ask; allow the server for the chat to send it. Calls over 256 KiB
are always refused.

Claude Code and Codex may ask about the same call themselves, in a mode
where they ask before using tools. You're asked once, by whichever of the two
decides:

| The chat | The agent asks before tools | The agent doesn't ask |
|---|---|---|
| **Tools: Ask** | Clackatoo asks, unless you allowed the server. The agent's question is answered for you. | Clackatoo asks, unless you allowed the server. |
| **Tools: Allow** | The agent asks, with its own choices. | Nothing asks: the agent's mode allows it. |
| **clackatoo's rules** | The rules decide, and ask where no rule covers the call. The agent's question is answered for you. | The rules decide. |

Clackatoo answers the agent's question only for a server its own proxy is
serving the chat, going by what the agent's adapter says of the call, never
by the call's input. A server of the same name from the agent's own
configuration, and any other agent's questions, are asked about as ever.

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
