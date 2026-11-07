// Pure paste-assist plan (daemon `paste.assist` on GNOME Wayland). No gi:// imports.
/** Largest chunked paste accepted (the daemon's default capture cap). */
export const MAX_ASSIST_BYTES = 64 * 1024 * 1024;
/** Most chunks one paste may take. */
export const MAX_ASSIST_CHUNKS = 1024;
/** Join pulled chunks; `null` unless they add up to exactly `size`. */
export function joinChunks(parts, size) {
    const total = parts.reduce((sum, part) => sum + part.length, 0);
    if (total !== size)
        return null;
    const out = new Uint8Array(size);
    let at = 0;
    for (const part of parts) {
        out.set(part, at);
        at += part.length;
    }
    return out;
}
/** Pause between the clipboard write and Ctrl+V once the target is
 * focused. Apps learn of the new clipboard asynchronously: with 60 ms
 * Firefox (Photopea) first pasted the previous image, then replaced it.
 * Short text keeps the quick pause, where the wait would be felt. */
export function injectSettleMs(mime, size) {
    return isTextMime(mime) && size < SMALL_TEXT_BYTES ? 60 : 200;
}
/** Text below this size pastes with the short pause. */
export const SMALL_TEXT_BYTES = 64 * 1024;
/** How long a popup's target stays valid while the popup is open. */
export const PRESS_TARGET_MAX_AGE_MS = 10 * 60 * 1000;
/** How long to wait for the focus to leave the popup after a pick. */
export const FOCUS_WAIT_MS = 600;
/** Validate the event params; `null` for anything malformed. */
export function parseAssist(params) {
    if (typeof params !== 'object' || params === null)
        return null;
    const p = params;
    if (!Number.isSafeInteger(p.id) || typeof p.mime !== 'string' || !p.mime)
        return null;
    const base = { id: p.id, mime: p.mime, inject: p.inject === true };
    if (typeof p.data_b64 === 'string')
        return { ...base, data_b64: p.data_b64 };
    const size = p.size;
    const chunks = p.chunks;
    if (!Number.isSafeInteger(size) || size <= 0 || size > MAX_ASSIST_BYTES
        || !Number.isSafeInteger(chunks) || chunks <= 0 || chunks > MAX_ASSIST_CHUNKS)
        return null;
    return { ...base, size, chunks };
}
/** Whether `mime` is written as text (`St.Clipboard.set_text`). */
export function isTextMime(mime) {
    return mime.split(';')[0].trim() === 'text/plain' || mime === 'UTF8_STRING';
}
/**
 * Next step once the clip is on the clipboard. Ctrl+V is pressed only into
 * the popup's target (the window focused right before the popup took the
 * focus), once it is focused again; any doubt (no target, popup still in
 * front past the wait, another window) leaves the clip on the clipboard
 * with a "press Ctrl+V" toast.
 */
export function assistStep(o) {
    if (!o.inject || o.pressTarget === null || o.pressAgeMs > PRESS_TARGET_MAX_AGE_MS)
        return 'toast';
    if (o.popupFocused || o.focused === null)
        return o.waitedMs >= FOCUS_WAIT_MS ? 'toast' : 'wait';
    return o.focused === o.pressTarget ? 'inject' : 'toast';
}
/** Terminal emulators by any window identity (executable, WM_CLASS, app
 * id): they paste with Ctrl+Shift+V. Same list as
 * crates/platform/src/paste_policy.rs SHIFT_PASTE_APPS. */
const SHIFT_PASTE_APPS = new Set([
    'gnome-terminal', 'gnome-terminal-server', 'org.gnome.terminal', 'kgx', 'org.gnome.console',
    'ptyxis', 'org.gnome.ptyxis', 'konsole', 'org.kde.konsole', 'xfce4-terminal', 'tilix',
    'com.gexperts.tilix', 'terminator', 'alacritty', 'kitty', 'foot', 'footclient', 'wezterm',
    'wezterm-gui', 'org.wezfurlong.wezterm', 'guake', 'tilda', 'terminology', 'st',
    'com.system76.cosmicterm', 'cosmic-term',
]);
/** Whether a paste into a window with these identities needs Ctrl+Shift+V. */
export function pasteNeedsShift(ids) {
    return ids.some((id) => SHIFT_PASTE_APPS.has(id.trim().toLowerCase().replace(/\.desktop$/, '')));
}
