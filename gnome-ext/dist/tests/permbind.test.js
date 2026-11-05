import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parsePermanents, bindPlan } from '../src/permbind.js';
test('parsePermanents keeps permanents with a parseable hotkey', () => {
    assert.deepEqual(parsePermanents({ permanents: [
            { id: 'a', name: 'Sig', hotkey: 'ctrl+alt+1' },
            { id: 'b', name: 'No key', hotkey: null },
            { id: 'c', name: 'Bad', hotkey: 'ctrl+' },
        ] }), [{ id: 'a', accel: '<Control><Alt>1' }]);
    assert.deepEqual(parsePermanents(null), []);
});
test('bindPlan grabs new, releases removed and rebinds changed chords', () => {
    const bound = { a: '<Control><Alt>1', b: '<Control><Alt>2' };
    const plan = bindPlan(bound, [{ id: 'a', accel: '<Control><Alt>1' }, { id: 'b', accel: '<Control><Alt>3' }, { id: 'c', accel: '<Control><Alt>4' }]);
    assert.deepEqual(plan.release, ['b']);
    assert.deepEqual(plan.grab, [{ id: 'b', accel: '<Control><Alt>3' }, { id: 'c', accel: '<Control><Alt>4' }]);
    assert.deepEqual(bindPlan(bound, []).release, ['a', 'b']);
    assert.deepEqual(bindPlan({}, []).grab, []);
});
