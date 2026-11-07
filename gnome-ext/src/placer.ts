// Shell glue: place the app-ui popup on Wayland, where a client cannot
// position its own window (seg-09: Wayland delegates placement to the
// compositor). X11 sessions keep the UI's own placement.
import GLib from 'gi://GLib?version=2.0';
import Meta from 'gi://Meta';
import { POPUP_TITLE } from './config.js';
import { placePopup, type Placement } from './placement.js';
import { isWaylandSession } from './session.js';

/** How long after the first placement Mutter's own moves are undone. */
const SETTLE_MS = 1500;

export class PopupPlacer {
  placement: Placement = 'cursor';
  private handler: number | null = null;

  enable(): void {
    if (!isWaylandSession(Meta as any)) return;
    const display = (globalThis as any).global?.display;
    this.handler = display?.connect('window-created', (_display: any, win: any) => this.watch(win)) ?? null;
  }
  /** Mutter places a new window after it is shown and the UI then sizes
   * the popup to its clips, so the position is re-applied whenever the
   * window moves or resizes during SETTLE_MS; afterwards the user may drag
   * it. The pointer is read once, at the first attempt. Other windows stop
   * being watched as soon as their title is known. */
  private watch(win: any): void {
    let pointer: [number, number] | null = null;
    let settle: number | null = null;
    let activated = false;
    const ids: Array<[any, number]> = [];
    let actor: any = null;
    // Once the window is unmanaged its actor is already disposed: touching
    // it logs a GJS error, and its handlers went with it anyway.
    const done = (unmanaged = false) => {
      for (const [obj, id] of ids) {
        if (unmanaged && obj === actor) continue;
        try { obj.disconnect(id); } catch { /* gone */ }
      }
      ids.length = 0;
      if (settle !== null) { GLib.source_remove(settle); settle = null; }
    };
    const attempt = () => {
      const title = win.get_title?.();
      if (title !== POPUP_TITLE) { if (title) done(); return; }
      pointer ??= (globalThis as any).global.get_pointer().slice(0, 2) as [number, number];
      // Mutter's focus-stealing prevention (GNOME 48+) can map the popup
      // behind the focused window's input: it shows, but typing goes to
      // the window below. The popup opens on the user's own hotkey, so
      // give it the focus once it is on screen.
      if (!activated && win.showing_on_its_workspace?.() !== false && win.get_compositor_private?.()) {
        activated = true;
        try { win.activate((globalThis as any).global.get_current_time()); } catch { /* best effort */ }
      }
      if (this.place(win, pointer) && settle === null) {
        settle = GLib.timeout_add(GLib.PRIORITY_DEFAULT, SETTLE_MS, () => { settle = null; done(); return GLib.SOURCE_REMOVE; });
      }
    };
    actor = win?.get_compositor_private?.() ?? null;
    const signals: Array<[any, string]> = [[win, 'shown'], [win, 'notify::title'], [win, 'size-changed'], [win, 'position-changed'], [actor, 'first-frame']];
    for (const [obj, signal] of signals) {
      if (obj) { try { ids.push([obj, obj.connect(signal, attempt)]); } catch { /* signal absent */ } }
    }
    try { ids.push([win, win.connect('unmanaged', () => done(true))]); } catch { /* signal absent */ }
    attempt();
  }
  /** Move the popup into place; false until its size is known. */
  private place(win: any, pointer: [number, number]): boolean {
    try {
      const frame = win.get_frame_rect();
      if (!(frame.width > 0 && frame.height > 0)) return false;
      const area = win.get_work_area_for_monitor((globalThis as any).global.display.get_current_monitor());
      const [x, y] = placePopup(this.placement, pointer,
        { x: area.x, y: area.y, width: area.width, height: area.height }, [frame.width, frame.height]);
      if (frame.x !== x || frame.y !== y) win.move_frame(true, x, y);
      return true;
    } catch { return false; /* placement is best effort; the popup still opens */ }
  }
  disable(): void {
    if (this.handler !== null) {
      try { (globalThis as any).global?.display?.disconnect(this.handler); } catch { /* shell gone */ }
      this.handler = null;
    }
  }
}
