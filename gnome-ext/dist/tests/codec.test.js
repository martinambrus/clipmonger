import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodeEnvelope, decodeEnvelope, helloRequest, pingRequest, captureReportRequest, hotkeyFiredRequest, hotkeyConflictRequest, blobPutRequests, chunkPayload, sha256Hex, base64Encode, base64Decode, createIdGen, CodecError, CHUNK_RAW_BYTES, } from '../src/codec.js';
test('envelope roundtrip: hello keys are exactly the wire shape', () => {
    const line = encodeEnvelope(helloRequest('e1', 'tok123'));
    const obj = JSON.parse(line);
    assert.deepEqual(Object.keys(obj).sort(), ['id', 'kind', 'method', 'params', 'v']);
    assert.equal(obj.v, 'ipc/v1');
    assert.equal(obj.kind, 'req');
    assert.equal(obj.method, 'hello');
    assert.deepEqual(obj.params, { min: 1, max: 1, token: 'tok123', client: 'gnome-ext' });
    assert.deepEqual(decodeEnvelope(line), obj);
});
test('builders: ping / hotkey.fired / hotkey.conflict / capture.report (G-F3/G-F4)', () => {
    assert.deepEqual(JSON.parse(encodeEnvelope(pingRequest('e2'))).params, {});
    assert.deepEqual(JSON.parse(encodeEnvelope(hotkeyFiredRequest('e3', 1))).params, { id: 1 });
    const conflict = JSON.parse(encodeEnvelope(hotkeyConflictRequest('e4', 'ctrl+alt+v', 'grabbed')));
    assert.equal(conflict.method, 'hotkey.conflict');
    assert.deepEqual(conflict.params, { chord: 'ctrl+alt+v', reason: 'grabbed' });
    const report = JSON.parse(encodeEnvelope(captureReportRequest('e5', { clip: 'c1', origin: 'Clipboard', source_app: null, sensitive: false, offers: [] }, { 'text/plain': 'aGk=' })));
    assert.equal(report.method, 'capture.report');
    assert.deepEqual(report.params.payloads, { 'text/plain': 'aGk=' });
});
test('decode: golden res/event/cancel fixtures with kind (not type)', () => {
    const res = decodeEnvelope('{"v":"ipc/v1","id":"e1","kind":"res","result":{"chosen":1,"daemon_version":"0.1.0"}}');
    assert.deepEqual(res.result, { chosen: 1, daemon_version: '0.1.0' });
    assert.equal(decodeEnvelope('{"v":"ipc/v1","id":"s1","kind":"event","method":"config.changed","params":{}}').kind, 'event');
    assert.equal(decodeEnvelope('{"v":"ipc/v1","id":"e9","kind":"cancel"}').kind, 'cancel');
});
test('decode/encode: version_mismatch and oversize rejected', () => {
    assert.throws(() => decodeEnvelope('{"v":"ipc/v2","id":"e1","kind":"req"}'), (e) => e instanceof CodecError && e.code === 'version_mismatch');
    const big = 'x'.repeat(1_048_577);
    assert.throws(() => decodeEnvelope(big), (e) => e instanceof CodecError && e.code === 'oversize');
    const huge = { v: 'ipc/v1', id: 'e1', kind: 'req', method: 'blob.put', params: { data: big } };
    assert.throws(() => encodeEnvelope(huge), (e) => e instanceof CodecError && e.code === 'oversize');
});
test('sha256 FIPS vectors', () => {
    assert.equal(sha256Hex(''), 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    assert.equal(sha256Hex('hi'), '8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4');
    assert.equal(sha256Hex('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq'), '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
});
test('base64 padding cases roundtrip', () => {
    for (const [raw, b64] of [['', ''], ['f', 'Zg=='], ['fo', 'Zm8='], ['foo', 'Zm9v'], ['hi', 'aGk=']]) {
        assert.equal(base64Encode(new TextEncoder().encode(raw)), b64);
        assert.deepEqual([...base64Decode(b64)], [...new TextEncoder().encode(raw)]);
    }
});
test('chunk math: 0B, 1B, boundary, and 3MiB -> exactly 12 chunks', () => {
    const empty = chunkPayload(new Uint8Array(0));
    assert.equal(empty.length, 1);
    assert.equal(empty[0].eof, true);
    assert.equal(empty[0].data_b64, '');
    assert.equal(empty[0].sha256, sha256Hex(''));
    assert.equal(chunkPayload(new Uint8Array(1)).length, 1);
    const exact = chunkPayload(new Uint8Array(CHUNK_RAW_BYTES));
    assert.equal(exact.length, 1);
    assert.equal(exact[0].eof, true);
    assert.ok(exact[0].sha256);
    const plusOne = chunkPayload(new Uint8Array(CHUNK_RAW_BYTES + 1));
    assert.equal(plusOne.length, 2);
    assert.equal(plusOne[0].sha256, undefined);
    assert.equal(plusOne[1].seq, 1);
    const threeMiB = new Uint8Array(3 * 1024 * 1024).map((_, i) => i % 251);
    const chunks = chunkPayload(threeMiB);
    assert.equal(chunks.length, 12);
    chunks.forEach((c, i) => {
        assert.equal(c.seq, i);
        assert.equal(c.eof, i === 11);
        if (i < 11) {
            assert.equal(c.sha256, undefined);
            assert.equal(c.data_b64.length, 349528);
        }
    });
    assert.equal(chunks[11].sha256, sha256Hex(threeMiB));
    const joined = Buffer.concat(chunks.map((c) => Buffer.from(base64Decode(c.data_b64))));
    assert.deepEqual(new Uint8Array(joined), threeMiB);
});
test('blobPutRequests: seq/eof/sha placement and id sequence', () => {
    const frames = blobPutRequests(createIdGen('e'), 'clip-1', 'image/png', new Uint8Array(CHUNK_RAW_BYTES + 1));
    assert.equal(frames.length, 2);
    assert.equal(frames[0].id, 'e1');
    assert.equal(frames[1].id, 'e2');
    const p0 = frames[0].params;
    const p1 = frames[1].params;
    assert.equal(p0.seq, 0);
    assert.equal('sha256' in p0, false);
    assert.equal(p1.seq, 1);
    assert.equal(p1.eof, true);
    assert.equal(typeof p1.sha256, 'string');
});
test('id gen is monotonic', () => {
    const gen = createIdGen();
    assert.deepEqual([gen(), gen(), gen()], ['e1', 'e2', 'e3']);
});
