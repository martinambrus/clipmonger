import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PollDiff } from '../src/polddiff.js';

const enc = (s: string) => new TextEncoder().encode(s);
const text = (s: string) => [{ mime: 'text/plain', bytes: enc(s) }];

test('text change emits exactly once; no-change ticks emit nothing', () => {
  const d = new PollDiff();
  assert.equal(d.decide({ offers: text('hello'), nowMs: 0 }).kind, 'Report');
  assert.deepEqual(d.decide({ offers: text('hello'), nowMs: 500 }), { kind: 'Ignore', reason: 'no-change' });
  assert.equal(d.decide({ offers: text('world'), nowMs: 1000 }).kind, 'Report');
});

test('image-only change (same offers, hash differs) emits', () => {
  const d = new PollDiff();
  const img = (b: number[]) => [{ mime: 'image/png', bytes: new Uint8Array(b) }];
  assert.equal(d.decide({ offers: img([1, 2, 3]), nowMs: 0 }).kind, 'Report');
  assert.equal(d.decide({ offers: img([1, 2, 3]), nowMs: 500 }).kind, 'Ignore');
  assert.equal(d.decide({ offers: img([9, 9]), nowMs: 1000 }).kind, 'Report');
});

test('file-only (uri-list) change emits', () => {
  const d = new PollDiff();
  const files = (s: string) => [{ mime: 'text/uri-list', bytes: enc(s) }];
  assert.equal(d.decide({ offers: files('file:///a.txt\r\n'), nowMs: 0 }).kind, 'Report');
  assert.equal(d.decide({ offers: files('file:///b.txt\r\n'), nowMs: 500 }).kind, 'Report');
});

test('denylist app -> Ignore, zero payloads reported', () => {
  const d = new PollDiff({ denylist: ['keepassxc', 'org.gnome.Secrets.desktop'] });
  assert.deepEqual(d.decide({ offers: text('secret'), focusedAppId: 'KeePassXC.desktop', nowMs: 0 }),
    { kind: 'Ignore', reason: 'denylist' });
  assert.deepEqual(d.decide({ offers: text('secret'), focusedAppId: 'org.gnome.secrets', nowMs: 0 }),
    { kind: 'Ignore', reason: 'denylist' });
});

test('password hint suppresses before payload report', () => {
  const d = new PollDiff();
  const offers = [
    { mime: 'x-kde-passwordManagerHint', bytes: enc('secret') },
    { mime: 'text/plain', bytes: enc('hunter2') },
  ];
  assert.deepEqual(d.decide({ offers, nowMs: 0 }), { kind: 'Sensitive', reason: 'password-hint' });
  const viaParam = new PollDiff();
  assert.equal(viaParam.decide({ offers: [{ mime: 'x-kde-passwordManagerHint' }, ...text('x')], hintValue: 'secret', nowMs: 0 }).kind, 'Sensitive');
});

test('oversize format skipped, others reported with truncated list', () => {
  const d = new PollDiff({ maxBytes: 4 });
  const decision = d.decide({ offers: [...text('ok'), { mime: 'image/png', bytes: new Uint8Array(10) }], nowMs: 0 });
  assert.equal(decision.kind, 'Report');
  if (decision.kind === 'Report') {
    assert.deepEqual(decision.offers.map((o) => o.mime), ['text/plain']);
    assert.deepEqual(decision.truncated, ['image/png']);
  }
  const allBig = new PollDiff({ maxBytes: 1 });
  assert.deepEqual(allBig.decide({ offers: text('toolong'), nowMs: 0 }), { kind: 'Ignore', reason: 'oversize' });
});

test('self-write suppressed within TTL (one-shot), real copy after TTL kept', () => {
  const d = new PollDiff();
  d.markSelfWrite(text('pasted'), 1000);
  assert.deepEqual(d.decide({ offers: text('pasted'), nowMs: 2000 }), { kind: 'Ignore', reason: 'self-write' });
  const d2 = new PollDiff();
  d2.markSelfWrite(text('pasted'), 0);
  assert.equal(d2.decide({ offers: text('pasted'), nowMs: 6001 }).kind, 'Report');
});

test('owning tick decides while guard flag is set (pollOnce flow)', () => {
  // Mirrors capture.ts pollOnce: guard set -> read offers -> decide -> clear.
  // The guard must only stop overlapping ticks, never the owning tick.
  const d = new PollDiff();
  d.setReading(true);
  try {
    assert.equal(d.decide({ offers: text('test 222'), nowMs: 0 }).kind, 'Report');
  } finally {
    d.setReading(false);
  }
});
