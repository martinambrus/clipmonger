import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chordToAccel, accelToChord, isDenylisted, withinCap, mergeConfig, DEFAULT_MAX_BYTES } from '../src/config.js';

test('chord -> accel conversion', () => {
  assert.equal(chordToAccel('ctrl+alt+v'), '<Control><Alt>v');
  assert.equal(chordToAccel('Control+Shift+k'), '<Control><Shift>k');
  assert.equal(chordToAccel('super+v'), '<Super>v');
  assert.equal(chordToAccel('meta+space'), '<Super>space');
  assert.equal(chordToAccel('ctrl+F5'), '<Control>F5');
  assert.equal(chordToAccel('v'), 'v');
});

test('invalid chords rejected', () => {
  for (const bad of ['', 'ctrl+', '+v', 'ctrl+banana+x', 'hyper+v', 'ctrl+ctrl+v', 'ctrl']) {
    assert.equal(chordToAccel(bad), null, bad);
  }
});

test('accel -> chord roundtrip', () => {
  assert.equal(accelToChord('<Control><Alt>v'), 'ctrl+alt+v');
  assert.equal(accelToChord(chordToAccel('shift+super+a')!), 'shift+super+a');
});

test('denylist match: exact, .desktop suffix, case-insensitive', () => {
  assert.equal(isDenylisted('keepassxc', ['KeePassXC']), true);
  assert.equal(isDenylisted('org.gnome.Secrets.desktop', ['org.gnome.secrets']), true);
  assert.equal(isDenylisted('firefox', ['keepassxc']), false);
  assert.equal(isDenylisted(null, ['keepassxc']), false);
});

test('size cap boundary: ==max ok, max+1 skip', () => {
  assert.equal(withinCap(100, 100), true);
  assert.equal(withinCap(101, 100), false);
  assert.equal(withinCap(0, 100), true);
});

test('config merge: defaults on empty, partial overrides keep other defaults', () => {
  const def = mergeConfig({});
  assert.equal(def.monitor.max_bytes, DEFAULT_MAX_BYTES);
  assert.equal(def.monitor.poll_ms, 500);
  assert.equal(def.popup.hotkey, 'ctrl+alt+h');
  const merged = mergeConfig({ config: { monitor: { max_bytes: 42 }, privacy: { denylist: ['a'] } } });
  assert.equal(merged.monitor.max_bytes, 42);
  assert.deepEqual(merged.privacy.denylist, ['a']);
  assert.equal(merged.popup.hotkey, 'ctrl+alt+h');
  assert.equal(mergeConfig(null).monitor.poll_ms, 500);
  assert.equal(mergeConfig({ monitor: { max_bytes: -5 } }).monitor.max_bytes, DEFAULT_MAX_BYTES);
});
