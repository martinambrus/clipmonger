import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Relay, FakeClock } from '../src/relay.js';
function harness() {
    const clock = new FakeClock();
    const intents = [];
    const relay = new Relay({ clock, onIntent: (i) => intents.push(i) });
    return { clock, intents, relay };
}
const kinds = (intents) => intents.map((i) => (i.type === 'send' ? `send:${i.method}` : i.type));
test('connect -> hello -> ready: subscribe then config resync ordering', () => {
    const { relay, intents } = harness();
    relay.start();
    relay.handleConnected();
    relay.handleHelloOk();
    assert.equal(relay.state, 'Ready');
    assert.deepEqual(kinds(intents), ['connect', 'send-hello', 'send:events.subscribe', 'send:config.get', 'resync']);
});
test('backoff sequence 1,2,4,8,16,30 cap and reset on welcome', () => {
    const { relay } = harness();
    assert.deepEqual([0, 1, 2, 3, 4, 5, 6].map((a) => relay.backoffDelayMs(a)), [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
    relay.start();
    relay.handleConnected();
    relay.handleHelloOk();
    assert.equal(relay.backoffDelayMs(), 1000);
});
test('auth_failed -> backoff, no spin (single reconnect timer)', () => {
    const { relay, clock, intents } = harness();
    relay.start();
    relay.handleConnected();
    relay.handleHelloError('auth_failed');
    assert.equal(relay.state, 'Degraded');
    assert.ok(intents.some((i) => i.type === 'auth-error'));
    assert.equal(clock.pendingTimers, 1);
    clock.advance(999);
    assert.equal(relay.state, 'Degraded');
    clock.advance(1);
    assert.equal(relay.state, 'Connecting');
});
test('heartbeat: ping cadence, pong resets, 3 misses -> reconnect', () => {
    const { relay, clock, intents } = harness();
    relay.start();
    relay.handleConnected();
    relay.handleHelloOk();
    intents.length = 0;
    clock.advance(15000);
    assert.deepEqual(kinds(intents), ['send:ping']);
    relay.handlePong();
    assert.equal(relay.missedHeartbeats, 0);
    clock.advance(15000);
    clock.advance(15000);
    assert.equal(relay.state, 'Ready');
    clock.advance(15000); // third consecutive miss
    assert.equal(relay.state, 'Degraded');
    assert.ok(intents.some((i) => i.type === 'close'));
});
test('pending map resolves out-of-order responses', async () => {
    const { relay } = harness();
    const p1 = relay.registerPending('e1');
    const p2 = relay.registerPending('e2');
    assert.equal(relay.resolveResponse('e2', 'second'), true);
    assert.equal(relay.resolveResponse('e1', 'first'), true);
    assert.equal(await p2, 'second');
    assert.equal(await p1, 'first');
    assert.equal(relay.resolveResponse('nope', null), false);
    assert.equal(relay.pendingCount, 0);
});
test('wedge: 3 failures -> capture-paused once, success recovers', () => {
    const { relay, intents } = harness();
    relay.recordFailure();
    relay.recordFailure();
    assert.equal(relay.capturePaused, false);
    relay.recordFailure();
    relay.recordFailure();
    assert.equal(relay.capturePaused, true);
    assert.equal(intents.filter((i) => i.type === 'capture-paused').length, 1);
    relay.recordSuccess();
    assert.equal(relay.capturePaused, false);
    assert.equal(relay.consecutiveFailures, 0);
});
test('disable cancels all timers and emits nothing after', () => {
    const { relay, clock, intents } = harness();
    relay.start();
    relay.handleConnected();
    relay.handleHelloOk();
    relay.disable();
    assert.equal(relay.state, 'Disconnected');
    assert.equal(clock.pendingTimers, 0);
    const count = intents.length;
    clock.advance(120000);
    assert.equal(intents.length, count);
});
