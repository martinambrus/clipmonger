// Pure per-clip hotkey planning (GNOME Wayland permanents). No gi:// imports.
import { chordToAccel } from './config.js';
/** Permanents with a parseable hotkey from a `permanent.list` result. */
export function parsePermanents(result) {
    const list = result?.permanents;
    if (!Array.isArray(list))
        return [];
    const out = [];
    for (const item of list) {
        const p = item;
        if (typeof p.id !== 'string' || typeof p.hotkey !== 'string' || !p.hotkey.trim())
            continue;
        const accel = chordToAccel(p.hotkey);
        if (accel)
            out.push({ id: p.id, accel });
    }
    return out;
}
/** What to grab and release so `bound` (id -> accel) matches `wanted`. */
export function bindPlan(bound, wanted) {
    const want = new Map(wanted.map((p) => [p.id, p.accel]));
    const release = Object.keys(bound).filter((id) => want.get(id) !== bound[id]);
    const grab = wanted.filter((p) => bound[p.id] !== p.accel);
    return { grab, release };
}
