// Pure config helpers: chord conversion, denylist match, size caps, merge. No gi:// imports.
export const DEFAULT_MAX_BYTES = 16 * 1024 * 1024;
export const DEFAULT_POLL_MS = 500;
export const DEFAULT_FORMATS = ['text/plain;charset=utf-8', 'text/plain', 'text/html', 'text/rtf', 'image/png', 'text/uri-list'];
export const DEFAULT_POPUP_HOTKEY = 'ctrl+alt+h';
export function defaultConfig() {
    return {
        monitor: { formats: [...DEFAULT_FORMATS], max_bytes: DEFAULT_MAX_BYTES, ignores: [], poll_ms: DEFAULT_POLL_MS },
        popup: { hotkey: DEFAULT_POPUP_HOTKEY },
        privacy: { denylist: [] },
    };
}
function isObj(v) { return typeof v === 'object' && v !== null && !Array.isArray(v); }
function strArr(v, fallback) {
    return Array.isArray(v) && v.every((x) => typeof x === 'string') ? [...v] : [...fallback];
}
function posNum(v, fallback) {
    return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : fallback;
}
/** Merge a (possibly partial / flat) daemon config.get payload over defaults. */
export function mergeConfig(partial) {
    const out = defaultConfig();
    if (!isObj(partial))
        return out;
    const cfg = isObj(partial.config) ? partial.config : partial;
    const mon = isObj(cfg.monitor) ? cfg.monitor : {};
    const pop = isObj(cfg.popup) ? cfg.popup : {};
    const priv = isObj(cfg.privacy) ? cfg.privacy : {};
    out.monitor.formats = strArr(mon.formats ?? cfg.formats, out.monitor.formats);
    out.monitor.max_bytes = posNum(mon.max_bytes ?? cfg.max_bytes, out.monitor.max_bytes);
    out.monitor.poll_ms = posNum(mon.poll_ms ?? cfg.poll_ms, out.monitor.poll_ms);
    out.monitor.ignores = strArr(mon.ignores ?? cfg.ignores, out.monitor.ignores);
    if (typeof pop.hotkey === 'string' && pop.hotkey)
        out.popup.hotkey = pop.hotkey;
    if (typeof cfg.hotkey === 'string' && cfg.hotkey)
        out.popup.hotkey = cfg.hotkey;
    out.privacy.denylist = strArr(priv.denylist ?? cfg.denylist, out.privacy.denylist);
    return out;
}
const stripDesktop = (s) => s.trim().toLowerCase().replace(/\.desktop$/, '');
export function isDenylisted(appId, denylist) {
    if (!appId)
        return false;
    const target = stripDesktop(appId);
    return denylist.some((entry) => stripDesktop(entry) === target && target !== '');
}
export function withinCap(size, maxBytes) { return size >= 0 && size <= maxBytes; }
const MODS = {
    ctrl: '<Control>', control: '<Control>', alt: '<Alt>', mod1: '<Alt>',
    shift: '<Shift>', super: '<Super>', meta: '<Super>', mod4: '<Super>', cmd: '<Super>', command: '<Super>',
};
const ORDER = ['<Control>', '<Alt>', '<Shift>', '<Super>'];
const NAMED = {
    space: 'space', return: 'Return', enter: 'Return', escape: 'Escape', esc: 'Escape',
    tab: 'Tab', backspace: 'BackSpace', delete: 'Delete', home: 'Home', end: 'End',
    pageup: 'Page_Up', pagedown: 'Page_Down', left: 'Left', right: 'Right', up: 'Up', down: 'Down',
};
/** Convert daemon chord `ctrl+alt+v` to Shell accel `<Control><Alt>v`. Returns null if unparseable. */
export function chordToAccel(chord) {
    if (typeof chord !== 'string' || !chord.trim())
        return null;
    const parts = chord.split('+').map((p) => p.trim());
    if (parts.some((p) => !p))
        return null;
    const keyRaw = parts[parts.length - 1];
    const mods = new Set();
    for (const m of parts.slice(0, -1)) {
        const accel = MODS[m.toLowerCase()];
        if (!accel || mods.has(accel))
            return null;
        mods.add(accel);
    }
    let key;
    const lower = keyRaw.toLowerCase();
    if (NAMED[lower])
        key = NAMED[lower];
    else if (/^f([1-9]|1[0-2])$/i.test(keyRaw))
        key = keyRaw.toUpperCase();
    else if (keyRaw.length === 1)
        key = lower;
    else
        return null;
    return ORDER.filter((m) => mods.has(m)).join('') + key;
}
export function accelToChord(accel) {
    if (typeof accel !== 'string' || !accel)
        return null;
    const rev = { '<Control>': 'ctrl', '<Alt>': 'alt', '<Shift>': 'shift', '<Super>': 'super' };
    let rest = accel;
    const mods = [];
    for (const [tag, name] of Object.entries(rev))
        if (rest.startsWith(tag)) {
            mods.push(name);
            rest = rest.slice(tag.length);
        }
    // Handle modifiers in any order.
    let changed = true;
    while (changed) {
        changed = false;
        for (const [tag, name] of Object.entries(rev))
            if (rest.startsWith(tag) && !mods.includes(name)) {
                mods.push(name);
                rest = rest.slice(tag.length);
                changed = true;
            }
    }
    if (!rest || rest.includes('<'))
        return null;
    return [...mods, rest.length === 1 ? rest.toLowerCase() : rest].join('+');
}
