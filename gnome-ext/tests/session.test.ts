import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isWaylandSession } from '../src/session.js';

test('a Shell without Meta.is_wayland_compositor (GNOME 49+) is Wayland', () => {
  assert.equal(isWaylandSession({}), true);
  assert.equal(isWaylandSession({ is_wayland_compositor: () => true }), true);
  assert.equal(isWaylandSession({ is_wayland_compositor: () => false }), false, 'GNOME 46-48 on Xorg');
});
