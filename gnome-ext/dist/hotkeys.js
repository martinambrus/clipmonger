// Shell glue: Main.wm.addKeybinding registration + conflict reporting.
import { wm, notify } from 'resource:///org/gnome/shell/ui/main.js';
// VERIFY (T-702): Meta.KeyBindingFlags / Shell.ActionMode import paths and
// values, and the exact addKeybinding failure signal (expected: returns 0).
import { chordToAccel } from './config.js';
import { hotkeyFiredRequest, hotkeyConflictRequest } from './codec.js';
export class Hotkeys {
    settings;
    ipc;
    registered = [];
    constructor(settings, ipc) { this.settings = settings; this.ipc = ipc; }
    /** Register the popup binding (id 1). Returns false on conflict. */
    registerPopup(chord) {
        const accel = chordToAccel(chord);
        if (!accel) {
            notify('Clipmonger', `Unparseable shortcut: ${chord}`);
            this.ipc.notify(hotkeyConflictRequest(this.ipc.nextId(), chord, 'unparseable chord'));
            return false;
        }
        this.settings.set_strv('popup', [accel]);
        // VERIFY: Meta.KeyBindingFlags.NONE === 0 and Shell.ActionMode.NORMAL|MESSAGE_TRAY values.
        const result = wm.addKeybinding('popup', this.settings, 0, 1 | 2, () => {
            this.ipc.notify(hotkeyFiredRequest(this.ipc.nextId(), 1));
        });
        if (!result) {
            notify('Clipmonger', `Shortcut ${chord} is already in use`);
            // G-F4: hotkey.conflict {chord, reason}
            this.ipc.notify(hotkeyConflictRequest(this.ipc.nextId(), chord, 'accel already grabbed'));
            return false;
        }
        this.registered.push('popup');
        return true;
    }
    unregisterAll() {
        for (const name of this.registered) {
            try {
                wm.removeKeybinding(name);
            }
            catch { /* VERIFY name */ }
        }
        this.registered = [];
    }
}
