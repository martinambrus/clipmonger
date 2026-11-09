import { test } from 'node:test';
import assert from 'node:assert/strict';
import { placePopup, popupHeight } from '../src/placement.js';

const area = { x: 0, y: 27, width: 1920, height: 1053 };

test('cursor popup stops at the bottom of the work area', () => {
  assert.equal(popupHeight('cursor', [400, 300], area, 1053), 780);
  assert.equal(popupHeight('cursor', [400, 300], area, 200), 200);
  assert.equal(popupHeight('center', [400, 300], area, 1000), 1000);
});

test('cursor popup near the bottom keeps a usable height', () => {
  assert.equal(popupHeight('cursor', [400, 1000], area, 1053), 422);
});

test('cursor placement opens at the pointer', () => {
  assert.deepEqual(placePopup('cursor', [400, 300], area, [480, 360]), [400, 300]);
});

test('cursor placement clamps inside the work area', () => {
  assert.deepEqual(placePopup('cursor', [1900, 1070], area, [480, 360]), [1440, 720]);
  assert.deepEqual(placePopup('cursor', [5, 5], area, [480, 360]), [5, 27]);
});

test('second monitor work area keeps its offset', () => {
  const right = { x: 1920, y: 0, width: 2560, height: 1440 };
  assert.deepEqual(placePopup('cursor', [4470, 100], right, [480, 360]), [4000, 100]);
});

test('center placement and an oversized popup', () => {
  assert.deepEqual(placePopup('center', [10, 10], area, [480, 360]), [720, 373]);
  assert.deepEqual(placePopup('center', [10, 10], area, [2000, 1200]), [0, 27]);
  assert.deepEqual(placePopup('cursor', [800, 600], area, [2000, 1200]), [0, 27]);
});
