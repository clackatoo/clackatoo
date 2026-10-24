// Read-only provider quota probes. No user prompt is ever yielded or sent.
// Credentials remain owned by the provider CLI; stdout contains only quota data.
import { pathToFileURL } from 'node:url';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
const [provider, entry] = process.argv.slice(1);
const windows = [];
const bounded = s => String(s).replace(/[\x00-\x1f\x7f]/g, '').slice(0, 100);
function add(id, label, used, resets) {
  if (typeof used !== 'number' || !Number.isFinite(used) || windows.length >= 32) return;
  const ms = typeof resets === 'string' ? Date.parse(resets) : resets * 1000;
  windows.push({id: bounded(id), label: bounded(label), used: Math.round(Math.max(0, Math.min(100, used))),
    resets: Number.isFinite(ms) && ms >= 1577836800000 && ms <= 4102444800000 ? Math.round(ms) : null});
}
function duration(mins, fallback) {
  if (!Number.isFinite(mins) || mins <= 0) return fallback;
  return mins % 1440 === 0 ? `${mins / 1440}-day` : mins % 60 === 0 ? `${mins / 60}-hour` : `${mins}-minute`;
}
async function claude() {
  const {query} = await import(pathToFileURL(entry));
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 25000);
  const q = query({prompt: (async function* () { await new Promise(resolve => abort.signal.addEventListener('abort', resolve, {once: true})); })(),
    options: {abortController: abort, persistSession: false, settingSources: ['user'],
      settings: {disableAllHooks: true}, tools: [], allowedTools: [], mcpServers: {}, strictMcpConfig: true,
      env: {...process.env, ENABLE_CLAUDEAI_MCP_SERVERS: 'false', CLAUDE_CODE_AUTO_CONNECT_IDE: '0', CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL: '1'},
      stderr: () => {}}});
  try {
    await q.initializationResult();
    const data = await q.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({skipBehaviors: true});
    if (!data.rate_limits_available || !data.rate_limits) return 'Subscription limits are not available for this sign-in.';
    const limits = data.rate_limits;
    for (const [key, value] of Object.entries(limits)) {
      if (value && typeof value === 'object' && 'utilization' in value && key !== 'extra_usage') {
        const label = {five_hour: '5-hour', seven_day: 'Weekly', seven_day_opus: 'Weekly · Opus', seven_day_sonnet: 'Weekly · Sonnet'}[key] ?? 'Additional allowance';
        add(key, label, value.utilization, value.resets_at);
      }
    }
    for (const row of (Array.isArray(limits.model_scoped) ? limits.model_scoped : []).slice(0, 24)) {
      add(`model:${row.display_name}`, `Weekly · ${bounded(row.display_name)}`, row.utilization, row.resets_at);
    }
    const spend = limits.spend;
    if (spend?.enabled && spend.used?.currency === spend.limit?.currency && spend.used?.exponent === spend.limit?.exponent && spend.limit?.amount_minor > 0) {
      add('extra_usage', 'Monthly spend', 100 * spend.used.amount_minor / spend.limit.amount_minor);
    } else if (limits.extra_usage?.is_enabled) {
      const extra = limits.extra_usage;
      add('extra_usage', 'Monthly spend', extra.monthly_limit > 0 ? 100 * extra.used_credits / extra.monthly_limit : extra.utilization);
    }
  } finally { clearTimeout(timer); abort.abort(); q.close(); }
}
async function codex() {
  const child = spawn(process.execPath, [entry, 'app-server'], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  const pending = new Map();
  let bytes = 0;
  lines.on('line', line => {
    bytes += line.length;
    if (bytes > 1024 * 1024) { child.kill(); return; }
    try { const msg = JSON.parse(line); if (pending.has(msg.id)) { const [resolve, reject] = pending.get(msg.id); pending.delete(msg.id); msg.error ? reject(new Error('Provider refused quota read')) : resolve(msg.result); } } catch {}
  });
  child.on('error', () => { for (const [, reject] of pending.values()) reject(new Error('Provider unavailable')); });
  child.on('exit', () => { for (const [, reject] of pending.values()) reject(new Error('Provider exited')); });
  const ask = (id, method, params) => new Promise((resolve, reject) => {pending.set(id, [resolve, reject]); child.stdin.write(JSON.stringify({id, method, params}) + '\n');});
  const timer = setTimeout(() => child.kill(), 25000);
  try {
    await ask(1, 'initialize', {clientInfo: {name: 'clackatoo-usage', version: '1'}, capabilities: {experimentalApi: true}});
    child.stdin.write(JSON.stringify({method: 'initialized'}) + '\n');
    const response = await ask(2, 'account/rateLimits/read', {});
    const snapshots = Object.entries(response.rateLimitsByLimitId ?? {});
    if (!snapshots.length && response.rateLimits) snapshots.push(['codex', response.rateLimits]);
    for (const [id, snapshot] of snapshots.slice(0, 16)) {
      if (!snapshot) continue;
      const prefix = id === 'codex' ? '' : `${bounded(snapshot.limitName ?? id)} · `;
      for (const [key, fallback] of [['primary', 'Primary'], ['secondary', 'Secondary']]) {
        const row = snapshot[key];
        if (row) add(`${id}:${key}`, prefix + duration(row.windowDurationMins, fallback), row.usedPercent, row.resetsAt);
      }
      if (snapshot.individualLimit) add(`${id}:individual`, prefix + 'Individual allowance', 100 - snapshot.individualLimit.remainingPercent, snapshot.individualLimit.resetsAt);
    }
  } finally {clearTimeout(timer); lines.close(); child.stdin.destroy(); child.kill();}
}
try {
  const notice = await (provider === 'claude' ? claude() : codex());
  process.stdout.write(JSON.stringify({ok: true, limits: windows, notice: notice ?? (windows.length ? null : 'No subscription limits were reported.')}));
} catch { process.stdout.write(JSON.stringify({ok: false, limits: [], notice: 'Usage unavailable · check sign-in or try later.'})); }
