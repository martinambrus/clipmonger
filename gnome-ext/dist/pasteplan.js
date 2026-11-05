// Pure paste-assist plan (daemon `paste.assist` on GNOME Wayland). No gi:// imports.
/** How long a popup's target stays valid while the popup is open. */
export const PRESS_TARGET_MAX_AGE_MS = 10 * 60 * 1000;
/** How long to wait for the focus to leave the popup after a pick. */
export const FOCUS_WAIT_MS = 600;
/** Validate the event params; `null` for anything malformed. */
export function parseAssist(params) {
    if (typeof params !== 'object' || params === null)
        return null;
    const p = params;
    if (!Number.isSafeInteger(p.id) || typeof p.mime !== 'string' || !p.mime
        || typeof p.data_b64 !== 'string')
        return null;
    return { id: p.id, mime: p.mime, data_b64: p.data_b64, inject: p.inject === true };
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
]);
/** Whether a paste into a window with these identities needs Ctrl+Shift+V. */
export function pasteNeedsShift(ids) {
    return ids.some((id) => SHIFT_PASTE_APPS.has(id.trim().toLowerCase().replace(/\.desktop$/, '')));
}
