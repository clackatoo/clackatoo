# Codex steering backport

The dependency remains the locked upstream `@agentclientprotocol/codex-acp@2.2.2`.
Its idle steering fallback starts a turn without a host-owned completion response
([upstream issue #440](https://github.com/agentclientprotocol/codex-acp/issues/440)).
The parser and delivery snippets backport the `promptRequired` contract from
[PR #593](https://github.com/agentclientprotocol/codex-acp/pull/593), including
returning promptly while the original prompt waits for approval. The snippets
are derived from Apache-2.0 upstream code.

`src/agents/codex_steering.rs` checks the exact upstream bundle SHA-256, leaves it
intact, and atomically installs a neighbouring `clackatoo.js` entry point. The
separate install directory and protocol identity are `2.2.2+clackatoo.2`.
Unmodified PATH/custom adapters do not gain native steering merely by advertising
support. Installation work happens on the existing installer worker.

`node steering-test.mjs [prepared-bundle.js]` tests the contract without calling
a model; with a bundle argument it extracts and tests the installed methods.
The Rust suite invokes the same test. The ignored `prepare_the_pinned_adapter`
test verifies/prepares an installed bundle named by `CLACKATOO_CODEX_BACKPORT_DIR`.

When upstream releases this contract, remove the backport and pin the verified
upstream version in both the runtime and the native-steering allowlist. Do not
loosen the source hash check to accommodate a different bundle.

The managed-MCP backport replaces `createSessionConfig` and its four call
sites. Unless native MCP is explicitly allowed, it enumerates the resolved
configuration layers, disables each native server, disables app connectors,
and re-enables only the host-supplied servers. The per-session metadata takes
precedence over the process default, so a side question can disable MCP even
when the parent permits native configuration. Requested server names remain
separate from the built-in Clackatoo MCP identity.

`node managed-mcp-test.mjs [prepared-bundle.js]` checks the installed method's
native-server suppression, requested-server collisions and explicit opt-in.
