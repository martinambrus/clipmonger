// Pure per-clip hotkey planning (GNOME Wayland permanents). No gi:// imports.
import { chordToAccel } from './config.js';

/** One permanent clip with a hotkey, as `permanent.list` reports it. */
export interface PermanentChord { id: string; accel: string }

/** Permanents with a parseable hotkey from a `permanent.list` result. */
export function parsePermanents(result: unknown): PermanentChord[] {
  const list = (result as { permanents?: unknown } | null)?.permanents;
  if (!Array.isArray(list)) return [];
  const out: PermanentChord[] = [];
  for (const item of list) {
    const p = item as { id?: unknown; hotkey?: unknown };
    if (typeof p.id !== 'string' || typeof p.hotkey !== 'string' || !p.hotkey.trim()) continue;
    const accel = chordToAccel(p.hotkey);
    if (accel) out.push({ id: p.id, accel });
  }
  return out;
}

/** What to grab and release so `bound` (id -> accel) matches `wanted`. */
export function bindPlan(bound: Record<string, string>, wanted: PermanentChord[]): { grab: PermanentChord[]; release: string[] } {
  const want = new Map(wanted.map((p) => [p.id, p.accel]));
  const release = Object.keys(bound).filter((id) => want.get(id) !== bound[id]);
  const grab = wanted.filter((p) => bound[p.id] !== p.accel);
  return { grab, release };
}
