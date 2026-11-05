// Shell glue: paste-assist. Write the clip to the clipboard, then press
// Ctrl+V through a Clutter virtual keyboard into the window that had the
// focus right before the popup took it (pasteplan.ts decides), else toast
// "press Ctrl+V".
import St from 'gi://St';
import GLib from 'gi://GLib?version=2.0';
import Clutter from 'gi://Clutter';
import { notify } from 'resource:///org/gnome/shell/ui/main.js';
import { base64Decode } from './codec.js';
import { POPUP_TITLE } from './config.js';
import { parseAssist, isTextMime, assistStep } from './pasteplan.js';
/** Pause between the focus settling and the injected Ctrl+V. */
const INJECT_SETTLE_MS = 60;
const POLL_MS = 25;
const sleep = (ms) => new Promise((resolve) => {
    GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => { resolve(); return GLib.SOURCE_REMOVE; });
});
export class PasteAssist {
    capture;
    keyboard = null;
    focusHandler = null;
    /** Last focused window that is not the popup: stable sequence and time. */
    lastFocus = null;
    /** `lastFocus` when the popup took the focus: this popup's paste target. */
    popupTarget = null;
    constructor(capture = null) { this.capture = capture; }
    /** Track the focus so each popup knows the window it was opened over
     * (hotkey, tray or CLI alike). */
    enable() {
        const display = globalThis.global?.display;
        this.focusHandler = display?.connect('notify::focus-window', () => this.onFocus()) ?? null;
        this.onFocus();
    }
    onFocus() {
        try {
            const focus = globalThis.global?.display?.get_focus_window?.();
            if (!focus)
                return;
            if (focus.get_title?.() === POPUP_TITLE) {
                this.popupTarget = this.lastFocus;
            }
            else {
                this.lastFocus = { id: focus.get_stable_sequence(), at: Date.now() };
            }
        }
        catch { /* no focus info: the paste toasts */ }
    }
    /** Handle one daemon `paste.assist`; never throws. */
    async assist(params) {
        const req = parseAssist(params);
        const id = req?.id ?? 0;
        if (!req)
            return { id, ok: false, injected: false, error: 'malformed paste request' };
        try {
            this.write(req.mime, base64Decode(req.data_b64));
        }
        catch (e) {
            return { id, ok: false, injected: false, error: `clipboard write failed: ${e instanceof Error ? e.message : String(e)}` };
        }
        // Each popup's target serves one paste.
        const target = this.popupTarget;
        this.popupTarget = null;
        for (let waited = 0;; waited += POLL_MS) {
            const focus = globalThis.global?.display?.get_focus_window?.() ?? null;
            const step = assistStep({
                inject: req.inject,
                pressTarget: target?.id ?? null,
                pressAgeMs: target ? Date.now() - target.at : 0,
                focused: focus?.get_stable_sequence?.() ?? null,
                popupFocused: focus?.get_title?.() === POPUP_TITLE,
                waitedMs: waited,
            });
            if (step === 'wait') {
                await sleep(POLL_MS);
                continue;
            }
            if (step === 'inject') {
                await sleep(INJECT_SETTLE_MS);
                try {
                    this.pressCtrlV();
                    return { id, ok: true, injected: true };
                }
                catch { /* fall back to the toast */ }
            }
            notify('Clipmonger', 'Copied — press Ctrl+V');
            return { id, ok: true, injected: false };
        }
    }
    write(mime, bytes) {
        const clipboard = St.Clipboard.get_default();
        if (isTextMime(mime)) {
            clipboard.set_text(St.ClipboardType.CLIPBOARD, new TextDecoder().decode(bytes));
            this.capture?.markSelfWrite([{ mime: 'text/plain', bytes }]);
        }
        else {
            clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, GLib.Bytes.new(bytes));
            this.capture?.markSelfWrite([{ mime, bytes }]);
        }
    }
    pressCtrlV() {
        if (!this.keyboard) {
            const seat = Clutter.get_default_backend().get_default_seat();
            this.keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        }
        const now = () => GLib.get_monotonic_time();
        this.keyboard.notify_keyval(now(), Clutter.KEY_Control_L, Clutter.KeyState.PRESSED);
        this.keyboard.notify_keyval(now(), Clutter.KEY_v, Clutter.KeyState.PRESSED);
        this.keyboard.notify_keyval(now(), Clutter.KEY_v, Clutter.KeyState.RELEASED);
        this.keyboard.notify_keyval(now(), Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);
    }
    disable() {
        if (this.focusHandler !== null) {
            try {
                globalThis.global?.display?.disconnect(this.focusHandler);
            }
            catch { /* shell gone */ }
            this.focusHandler = null;
        }
        try {
            this.keyboard?.run_dispose?.();
        }
        catch { /* already gone */ }
        this.keyboard = null;
    }
}
