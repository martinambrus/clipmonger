import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseAssist, isTextMime, assistStep, FOCUS_WAIT_MS, PRESS_TARGET_MAX_AGE_MS } from '../src/pasteplan.js';

test('parseAssist validates the daemon request', () => {
  assert.deepEqual(parseAssist({ id: 3, mime: 'text/plain', data_b64: 'aGk=', inject: true }),
    { id: 3, mime: 'text/plain', data_b64: 'aGk=', inject: true });
  assert.equal(parseAssist({ id: 3, mime: 'text/plain', data_b64: 'aGk=' })?.inject, false);
  assert.equal(parseAssist({ id: '3', mime: 'text/plain', data_b64: '' }), null);
  assert.equal(parseAssist({ id: 3, mime: '', data_b64: '' }), null);
  assert.equal(parseAssist(null), null);
});

test('text mimes are written as text', () => {
  assert.equal(isTextMime('text/plain;charset=utf-8'), true);
  assert.equal(isTextMime('UTF8_STRING'), true);
  assert.equal(isTextMime('image/png'), false);
});

test('assistStep injects only into the press target, else toasts or waits', () => {
  const base = { inject: true, pressTarget: 7, pressAgeMs: 1000, focused: 7, popupFocused: false, waitedMs: 0 };
  assert.equal(assistStep(base), 'inject');
  assert.equal(assistStep({ ...base, focused: 8 }), 'toast', 'another window: never paste there');
  assert.equal(assistStep({ ...base, popupFocused: true }), 'wait', 'popup still closing');
  assert.equal(assistStep({ ...base, focused: null }), 'wait');
  assert.equal(assistStep({ ...base, popupFocused: true, waitedMs: FOCUS_WAIT_MS }), 'toast');
  assert.equal(assistStep({ ...base, pressTarget: null }), 'toast', 'no window before the popup: no target');
  assert.equal(assistStep({ ...base, pressAgeMs: PRESS_TARGET_MAX_AGE_MS + 1 }), 'toast');
  assert.equal(assistStep({ ...base, inject: false }), 'toast');
});
