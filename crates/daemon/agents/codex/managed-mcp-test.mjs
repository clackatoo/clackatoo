// Exercise the exact session configuration method installed by the backport.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
let method = readFileSync(new URL('managed-mcp.js', import.meta.url), 'utf8');
if (process.argv[2]) {
  const bundle = readFileSync(process.argv[2], 'utf8');
  const start = bundle.indexOf('  async createSessionConfig(');
  method = bundle.slice(start, bundle.indexOf('  async getConfigMcpServerNames(', start));
  assert.equal((bundle.match(/request\._meta\?\.clackatoo\?\.nativeMcp/g) || []).length, 4);
}
const Adapter = new Function('forceGitRootTurnDiffPaths', 'mergeGatewayConfig',
  'mergeSandboxWorkspaceWriteRoots', 'sanitizeMcpServerName',
  'shouldDeduplicateMcpConflicts', 'logger', `return class {${method}}`)
  (v => v, v => v, v => v, v => v.replaceAll(/\s/g, '_'), () => true, {log() {}});
const adapter = new Adapter();
adapter.config = {mcp_servers: {inline: {command: 'inline'}}};
adapter.getNativeProviderConfig = () => ({});
adapter.getModelProvider = () => 'fixture';
adapter.getConfigMcpServerNames = async path => {
  assert.equal(path, '/project');
  return new Set(['user', 'project', 'inline', 'managed_search']);
};
adapter.createMcpSeverConfig = server => ({command: server.command});
const managed = [{name: 'managed_search', command: 'clackatoo'}];
const result = await adapter.createSessionConfig('/project', [], managed, false);
for (const name of ['user', 'project', 'inline']) {
  assert.deepEqual(result.config.mcp_servers[name], {enabled: false});
}
assert.deepEqual(result.config.mcp_servers.managed_search, {command: 'clackatoo', enabled: true});
assert.equal(result.config['features.apps'], false);
assert.deepEqual(result.skippedMcpServers, []);
const empty = await adapter.createSessionConfig('/project', [], [], false);
assert.equal(empty.config.mcp_servers.user.enabled, false);
assert.equal(empty.config.mcp_servers.inline.enabled, false);
const optIn = await adapter.createSessionConfig('/project', [], managed, true);
assert.deepEqual(optIn.skippedMcpServers, ['managed_search']);
assert.equal(optIn.config.mcp_servers.inline.command, 'inline');
process.env.CLACKATOO_NATIVE_MCP = 'off';
assert.equal((await adapter.createSessionConfig('/project', [], [])).config.mcp_servers.user.enabled, false);
console.log('managed MCP adapter checks passed');
