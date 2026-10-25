// Exercise the exact methods installed by the backport, without a model or credentials.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
const RequestError = {invalidParams() { return new Error('invalidParams'); }};
let methods = ['steering-parser.js', 'steering-deliver.js'].map(name => readFileSync(new URL(name, import.meta.url), 'utf8')).join('\n');
if (process.argv[2]) {
  const bundle = readFileSync(process.argv[2], 'utf8');
  const extract = (start, end) => bundle.slice(bundle.indexOf(start), bundle.indexOf(end, bundle.indexOf(start)));
  methods = extract('  parseSessionSteerParams(params) {', '  createSessionConfigOptions(')
    + extract('  async performSteeringRequest(params) {', '  /**\n   * Rejects a steering prompt');
  assert.ok(bundle.includes('version: "2.2.2+clackatoo.1"'));
}
const Adapter = new Function('RequestError', `return class {${methods}}`)(RequestError);
const message = {sessionId: 's', prompt: [{type: 'text', text: 'guidance'}], _meta: {steering: {idleBehavior: 'promptRequired'}}};
function adapter(turn = 'active') {
  const a = new Adapter();
  a.state = {sessionId: 's', currentTurnId: turn};
  a.getSessionState = () => a.state;
  a.assertSteerInputSupported = () => {};
  a.sessionIsClosing = () => false;
  a.getSteerableTurnId = () => { throw new Error('must not wait for pending approval'); };
  a.startNewTurnFromSteering = () => { throw new Error('must not start an untracked turn'); };
  return a;
}
let checks = 0;
{
  const a = adapter();
  let injections = 0;
  a.injectSteerIntoActiveTurn = async (params, turn) => {
    assert.deepEqual(params.prompt, message.prompt); assert.equal(turn, 'active'); injections++; return true;
  };
  assert.deepEqual(await a.performSteeringRequest(a.parseSessionSteerParams(message)), {outcome: 'injected'});
  assert.equal(injections, 1); checks++;
}
for (const closing of [false, true]) {
  const a = adapter(closing ? 'active' : null);
  a.sessionIsClosing = () => closing;
  const result = await a.performSteeringRequest(a.parseSessionSteerParams(message));
  assert.deepEqual(result, {outcome: 'promptRequired', reason: 'noRunningTurn'}); checks++;
}
{
  const a = adapter();
  a.injectSteerIntoActiveTurn = async () => { a.state.currentTurnId = null; return false; };
  assert.equal((await a.performSteeringRequest(a.parseSessionSteerParams(message))).outcome, 'promptRequired'); checks++;
}
{
  const a = adapter();
  a.injectSteerIntoActiveTurn = async () => { throw new Error('transport failed'); };
  await assert.rejects(a.performSteeringRequest(a.parseSessionSteerParams(message)), /transport failed/); checks++;
}
for (const meta of [undefined, null, {}, {steering: null}, {steering: {}}, {steering: {idleBehavior: 'startNewTurn'}}]) {
  const a = adapter(null);
  a.getSteerableTurnId = async () => null;
  a.startNewTurnFromSteering = async () => ({outcome: 'startedNewTurn'});
  assert.equal((await a.performSteeringRequest(a.parseSessionSteerParams({...message, _meta: meta}))).outcome, 'startedNewTurn'); checks++;
}
for (const meta of [1, [], 'wrong', {steering: []}, {steering: 'wrong'}, {steering: {idleBehavior: null}}, {steering: {idleBehavior: 'wrong'}}]) {
  assert.throws(() => adapter().parseSessionSteerParams({...message, _meta: meta}), /invalidParams/); checks++;
}
console.log(`${checks} steering backport checks passed`);
