// Shell glue: Main.wm.addKeybinding registration + conflict reporting.
import { wm, notify } from 'resource:///org/gnome/shell/ui/main.js';
// VERIFY (T-702): Meta.KeyBindingFlags / Shell.ActionMode import paths and
// values, and the exact addKeybinding failure signal (expected: returns 0).
import { chordToAccel } from './config.js';
import { hotkeyFiredRequest, hotkeyConflictRequest, hotkeyConflictClearedRequest } from './codec.js';
import type { IpcClient } from './ipc.js';

/** The app-ui popup's fixed window title (crates/app-ui/src/main.rs). */
const POPUP_TITLE = 'clipmonger';

/** Process of the focused window when it is titled like the popup; the
 * daemon matches it against the UI's process before hiding anything. */
function focusedPopupPid(): number | null {
  try {
    const win = (globalThis as any).global?.display?.get_focus_window?.();
    if (!win || win.get_title?.() !== POPUP_TITLE) return null;
    const pid = win.get_pid?.();
    return typeof pid === 'number' && pid > 0 ? pid : null;
  } catch { return null; }
}

export class Hotkeys {
  private settings: any;
  private ipc: IpcClient;
  private registered: string[] = [];
  private currentAccel: string | null = null;
  private conflicted = false;
  constructor(settings: any, ipc: IpcClient) { this.settings = settings; this.ipc = ipc; }

  /** Register the popup binding (id 1). Returns false on conflict; a refused
   * change keeps the previous chord bound (ADR-004). */
  registerPopup(chord: string): boolean {
    const accel = chordToAccel(chord);
    if (!accel) {
      notify('Clipmonger', `Unparseable shortcut: ${chord}`);
      this.ipc.notify(hotkeyConflictRequest(this.ipc.nextId(), chord, 'unparseable chord'));
      this.conflicted = true;
      return false;
    }
    // enable() registers a fallback chord, then onConfig registers again:
    // re-adding the same binding makes addKeybinding report a false conflict.
    if (this.registered.includes('popup') && this.currentAccel === accel) return true;
    const previous = this.currentAccel;
    this.unbindPopup();
    if (!this.bindPopup(accel)) {
      notify('Clipmonger', `Shortcut ${chord} is already in use`);
      // G-F4: hotkey.conflict {chord, reason}
      this.ipc.notify(hotkeyConflictRequest(this.ipc.nextId(), chord, 'accel already grabbed'));
      this.conflicted = true;
      if (previous) this.bindPopup(previous);
      return false;
    }
    if (this.conflicted) {
      this.ipc.notify(hotkeyConflictClearedRequest(this.ipc.nextId(), chord));
      this.conflicted = false;
    }
    return true;
  }
  private bindPopup(accel: string): boolean {
    this.settings.set_strv('popup', [accel]);
    // VERIFY: Meta.KeyBindingFlags.NONE === 0 and Shell.ActionMode.NORMAL|MESSAGE_TRAY values.
    const result = wm.addKeybinding('popup', this.settings, 0, 1 | 2, () => {
      this.ipc.notify(hotkeyFiredRequest(this.ipc.nextId(), 1, focusedPopupPid()));
    });
    if (!result) return false;
    this.registered.push('popup');
    this.currentAccel = accel;
    return true;
  }
  private unbindPopup(): void {
    if (!this.registered.includes('popup')) return;
    try { wm.removeKeybinding('popup'); } catch { /* not registered */ }
    this.registered = this.registered.filter((name) => name !== 'popup');
    this.currentAccel = null;
  }
  unregisterAll(): void {
    for (const name of this.registered) { try { wm.removeKeybinding(name); } catch { /* VERIFY name */ } }
    this.registered = [];
    this.currentAccel = null;
  }
}
