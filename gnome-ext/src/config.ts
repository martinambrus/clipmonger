// Pure config helpers: chord conversion, denylist match, size caps, merge. No gi:// imports.
export const DEFAULT_MAX_BYTES = 16 * 1024 * 1024;
export const DEFAULT_POLL_MS = 500;
export const DEFAULT_FORMATS = ['text/plain;charset=utf-8', 'text/plain', 'text/html', 'text/rtf', 'image/png', 'text/uri-list'];
export const DEFAULT_POPUP_HOTKEY = 'ctrl+alt+h';
/** The app-ui popup's fixed window title (crates/app-ui/src/main.rs). */
export const POPUP_TITLE = 'clipmonger';

export interface ExtConfig {
  monitor: { formats: string[]; max_bytes: number; ignores: string[]; poll_ms: number };
  popup: { hotkey: string; placement: 'cursor' | 'center' };
  privacy: { denylist: string[]; strict: boolean };
}
export function defaultConfig(): ExtConfig {
  return {
    monitor: { formats: [...DEFAULT_FORMATS], max_bytes: DEFAULT_MAX_BYTES, ignores: [], poll_ms: DEFAULT_POLL_MS },
    popup: { hotkey: DEFAULT_POPUP_HOTKEY, placement: 'cursor' },
    privacy: { denylist: [], strict: false },
  };
}
function isObj(v: unknown): v is Record<string, unknown> { return typeof v === 'object' && v !== null && !Array.isArray(v); }
function strArr(v: unknown, fallback: string[]): string[] {
  return Array.isArray(v) && v.every((x) => typeof x === 'string') ? [...v] : [...fallback];
}
function posNum(v: unknown, fallback: number): number {
  return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : fallback;
}
/** Merge a (possibly partial / flat) daemon config.get payload over defaults. */
export function mergeConfig(partial: unknown): ExtConfig {
  const out = defaultConfig();
  if (!isObj(partial)) return out;
  const cfg = isObj(partial.config) ? partial.config : partial;
  const mon = isObj(cfg.monitor) ? cfg.monitor : {};
  const pop = isObj(cfg.popup) ? cfg.popup : {};
  const priv = isObj(cfg.privacy) ? cfg.privacy : {};
  out.monitor.formats = strArr(mon.formats ?? cfg.formats, out.monitor.formats);
  out.monitor.max_bytes = posNum(mon.max_bytes ?? cfg.max_bytes, out.monitor.max_bytes);
  out.monitor.poll_ms = posNum(mon.poll_ms ?? cfg.poll_ms, out.monitor.poll_ms);
  out.monitor.ignores = strArr(mon.ignores ?? cfg.ignores, out.monitor.ignores);
  if (typeof pop.hotkey === 'string' && pop.hotkey) out.popup.hotkey = pop.hotkey;
  if (typeof cfg.hotkey === 'string' && cfg.hotkey) out.popup.hotkey = cfg.hotkey;
  if (pop.placement === 'cursor' || pop.placement === 'center') out.popup.placement = pop.placement;
  if (typeof priv.strict === 'boolean') out.privacy.strict = priv.strict;
  out.privacy.denylist = strArr(priv.denylist ?? cfg.denylist, out.privacy.denylist);
  return out;
}

const stripDesktop = (s: string) => s.trim().toLowerCase().replace(/\.desktop$/, '');
/** Whether the focused app is denylisted. A window has several identities
 * (GTK app id `org.gnome.TextEditor`, WM_CLASS / executable
 * `gnome-text-editor`): any of them matching an entry counts. */
export function isDenylisted(appId: AppIds, denylist: string[]): boolean {
  const ids = (Array.isArray(appId) ? appId : [appId])
    .filter((id): id is string => typeof id === 'string' && id !== '')
    .map(stripDesktop);
  return ids.some((target) => target !== '' && denylist.some((entry) => stripDesktop(entry) === target));
}
/** A focused window's identities at one poll. */
export interface FocusIds { ids: string[]; appId: string | null; popup?: boolean }

/**
 * The app a detected clipboard change came from. A poll may notice a copy
 * after the focus moved on. While the Clipmonger popup is focused the copy
 * cannot be the popup's (its own writes are marked and skipped), so it is
 * judged by the last other window focused, however long ago; otherwise by
 * the windows focused at this poll and the previous one.
 */
export function copySource(current: FocusIds, previous: FocusIds, lastOther: FocusIds): { ids: string[]; appId: string | null } {
  if (current.popup) return { ids: [...lastOther.ids], appId: lastOther.appId };
  const ids = [...new Set([...current.ids, ...previous.ids])];
  return { ids, appId: current.appId ?? previous.appId };
}
/** One app identity or all of a window's identities. */
export type AppIds = string | null | undefined | ReadonlyArray<string | null | undefined>;
export function withinCap(size: number, maxBytes: number): boolean { return size >= 0 && size <= maxBytes; }

const MODS: Record<string, string> = {
  ctrl: '<Control>', control: '<Control>', alt: '<Alt>', mod1: '<Alt>',
  shift: '<Shift>', super: '<Super>', meta: '<Super>', mod4: '<Super>', cmd: '<Super>', command: '<Super>',
};
const ORDER = ['<Control>', '<Alt>', '<Shift>', '<Super>'];
const NAMED: Record<string, string> = {
  space: 'space', return: 'Return', enter: 'Return', escape: 'Escape', esc: 'Escape',
  tab: 'Tab', backspace: 'BackSpace', delete: 'Delete', home: 'Home', end: 'End',
  pageup: 'Page_Up', pagedown: 'Page_Down', left: 'Left', right: 'Right', up: 'Up', down: 'Down',
};
/** Convert daemon chord `ctrl+alt+v` to Shell accel `<Control><Alt>v`. Returns null if unparseable. */
export function chordToAccel(chord: string): string | null {
  if (typeof chord !== 'string' || !chord.trim()) return null;
  const parts = chord.split('+').map((p) => p.trim());
  if (parts.some((p) => !p)) return null;
  const keyRaw = parts[parts.length - 1];
  const mods = new Set<string>();
  for (const m of parts.slice(0, -1)) {
    const accel = MODS[m.toLowerCase()];
    if (!accel || mods.has(accel)) return null;
    mods.add(accel);
  }
  let key: string;
  const lower = keyRaw.toLowerCase();
  if (NAMED[lower]) key = NAMED[lower];
  else if (/^f([1-9]|1[0-2])$/i.test(keyRaw)) key = keyRaw.toUpperCase();
  else if (keyRaw.length === 1) key = lower;
  else return null;
  return ORDER.filter((m) => mods.has(m)).join('') + key;
}
export function accelToChord(accel: string): string | null {
  if (typeof accel !== 'string' || !accel) return null;
  const rev: Record<string, string> = { '<Control>': 'ctrl', '<Alt>': 'alt', '<Shift>': 'shift', '<Super>': 'super' };
  let rest = accel; const mods: string[] = [];
  for (const [tag, name] of Object.entries(rev)) if (rest.startsWith(tag)) { mods.push(name); rest = rest.slice(tag.length); }
  // Handle modifiers in any order.
  let changed = true;
  while (changed) { changed = false; for (const [tag, name] of Object.entries(rev)) if (rest.startsWith(tag) && !mods.includes(name)) { mods.push(name); rest = rest.slice(tag.length); changed = true; } }
  if (!rest || rest.includes('<')) return null;
  return [...mods, rest.length === 1 ? rest.toLowerCase() : rest].join('+');
}
