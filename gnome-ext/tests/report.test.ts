import test from 'node:test';
import assert from 'node:assert/strict';
import { emitCaptureReport } from '../src/report.js';
import { CHUNK_RAW_BYTES, createIdGen, type Envelope } from '../src/codec.js';
import { PollDiff } from '../src/polddiff.js';

test('actual relay preflights mixed truncation before any blob upload', async () => {
  const decision = new PollDiff({ maxBytes: CHUNK_RAW_BYTES + 1 }).decide({
    offers: [
      { mime: 'text/plain', bytes: new Uint8Array(CHUNK_RAW_BYTES + 1) },
      { mime: 'image/png', sizeHint: CHUNK_RAW_BYTES + 2 },
    ], nowMs: 0,
  });
  assert.equal(decision.kind, 'Report');
  assert.ok(decision.kind === 'Report');
  const frames: Envelope[] = [];
  await emitCaptureReport(createIdGen(), async (frame) => { frames.push(frame); }, decision.offers, decision.truncated, null, decision.truncatedOffers);
  assert.deepEqual(frames.map((frame) => frame.method), ['capture.report']);
  const params = frames[0].params as { payloads: object; event: { truncated: string[]; offers: object[] } };
  assert.deepEqual(params.payloads, {});
  assert.deepEqual(params.event.truncated, ['image/png']);
  assert.deepEqual(params.event.offers, [
    { mime: 'text/plain', bytes_hint: CHUNK_RAW_BYTES + 1 },
    { mime: 'image/png', bytes_hint: CHUNK_RAW_BYTES + 2 },
  ]);
});

test('ordinary blob storage rejection propagates and stops capture report', async () => {
  const methods: string[] = [];
  await assert.rejects(emitCaptureReport(createIdGen(), async (frame) => {
    methods.push(frame.method!);
    throw new Error('storage failure');
  }, [{ mime: 'text/plain', bytes_hint: CHUNK_RAW_BYTES + 1, bytes: new Uint8Array(CHUNK_RAW_BYTES + 1), hash: '' }], [], null), /storage failure/);
  assert.deepEqual(methods, ['blob.put']);
});

test('privacy preflight and current format filter run before any read', () => {
  const diff = new PollDiff({ strict: true });
  assert.equal(diff.preflight(['image/png'])?.kind, 'Sensitive');
  assert.equal(diff.preflight(['text/plain', 'image/png']), null);
  assert.equal(diff.wants('image/png'), false);
  assert.equal(diff.wants('text/plain'), true);
  assert.equal(diff.preflight(['text/plain', 'x-kde-passwordManagerHint'])?.kind, 'Sensitive');
  const filtered = new PollDiff({ formats: ['text/plain'], maxBytes: 4 });
  const decision = filtered.decide({ offers: [{ mime: 'image/png', sizeHint: 8 }, { mime: 'text/plain', bytes: new Uint8Array(2) }], nowMs: 0 });
  assert.ok(decision.kind === 'Report');
  assert.deepEqual(decision.truncated, []);
  assert.deepEqual(decision.offers.map((offer) => offer.mime), ['text/plain']);
});

test('oversize production decision does not hash ordinary or oversized payloads', () => {
  const noHash = () => { throw new Error('payload hash must not be read'); };
  const decision = new PollDiff({ maxBytes: 4 }).decide({ offers: [
    { mime: 'text/plain', sizeHint: 2, get hash(): string { return noHash(); } },
    { mime: 'image/png', sizeHint: 5, get hash(): string { return noHash(); } },
  ], nowMs: 0 });
  assert.ok(decision.kind === 'Report');
  assert.deepEqual(decision.truncatedOffers, [{ mime: 'image/png', bytes_hint: 5 }]);
});
