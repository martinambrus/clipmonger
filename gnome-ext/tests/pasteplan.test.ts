import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseAssist, isTextMime, assistStep, pasteNeedsShift, joinChunks, injectSettleMs, SMALL_TEXT_BYTES, FOCUS_WAIT_MS, PRESS_TARGET_MAX_AGE_MS, MAX_ASSIST_CHUNKS } from '../src/pasteplan.js';

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

test('terminals paste with Ctrl+Shift+V, other windows with Ctrl+V', () => {
  assert.equal(pasteNeedsShift(['org.gnome.Terminal', 'Gnome-terminal', 'gnome-terminal-server']), true);
  assert.equal(pasteNeedsShift(['org.gnome.Ptyxis.desktop']), true);
  assert.equal(pasteNeedsShift(['Alacritty']), true);
  assert.equal(pasteNeedsShift(['org.gnome.TextEditor', 'gnome-text-editor']), false);
  assert.equal(pasteNeedsShift(['firefox_firefox', 'firefox']), false);
  assert.equal(pasteNeedsShift(['xterm']), false, 'xterm pastes with Shift+Insert');
  assert.equal(pasteNeedsShift([]), false);
});

test('a clip too large for one frame is announced with its size and chunk count', () => {
  assert.deepEqual(parseAssist({ id: 4, mime: 'image/png', size: 700000, chunks: 3, inject: true }),
    { id: 4, mime: 'image/png', size: 700000, chunks: 3, inject: true });
  assert.equal(parseAssist({ id: 4, mime: 'image/png', size: 0, chunks: 1 }), null);
  assert.equal(parseAssist({ id: 4, mime: 'image/png', size: 10, chunks: 0 }), null);
  assert.equal(parseAssist({ id: 4, mime: 'image/png', size: 10, chunks: MAX_ASSIST_CHUNKS + 1 }), null);
  assert.equal(parseAssist({ id: 4, mime: 'image/png' }), null, 'neither inline data nor chunks');
});

test('pulled chunks join only when they add up to the announced size', () => {
  const parts = [new Uint8Array([1, 2]), new Uint8Array([3])];
  assert.deepEqual(joinChunks(parts, 3), new Uint8Array([1, 2, 3]));
  assert.equal(joinChunks(parts, 4), null);
  assert.equal(joinChunks([], 1), null);
});

test('short text pastes quickly; images and large clips wait for the app', () => {
  assert.equal(injectSettleMs('text/plain;charset=utf-8', 12), 60);
  assert.equal(injectSettleMs('text/plain', SMALL_TEXT_BYTES - 1), 60);
  assert.equal(injectSettleMs('text/plain', SMALL_TEXT_BYTES), 200);
  assert.equal(injectSettleMs('image/png', 10), 200);
  assert.equal(injectSettleMs('text/html', 10), 200);
});
