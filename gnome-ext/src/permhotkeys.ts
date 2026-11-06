// Shell glue: per-clip (permanent) hotkeys on GNOME Wayland, where only the
// Shell can bind global shortcuts. X11 sessions keep the daemon's own grab
// (binding both would paste twice). A press remembers the focused window as
// the paste target and asks the daemon to run the clip; the daemon pastes it
// back through `paste.assist`.
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import { wm } from 'resource:///org/gnome/shell/ui/main.js';
import { permanentExecRequest } from './codec.js';
import type { IpcClient } from './ipc.js';
import type { PasteAssist } from './paste.js';
import { bindPlan, type PermanentChord } from './permbind.js';
import { isWaylandSession } from './session.js';

export class PermanentHotkeys {
  private ipc: IpcClient;
  private paste: PasteAssist;
  private handler: number | null = null;
  /** Permanent id -> bound accelerator, and Mutter action -> id. */
  private bound: Record<string, string> = {};
  private actions = new Map<number, string>();
  private ids = new Map<string, number>();
  constructor(ipc: IpcClient, paste: PasteAssist) { this.ipc = ipc; this.paste = paste; }

  /** Only a Wayland session needs the Shell to bind these. */
  get active(): boolean { return isWaylandSession(Meta as any); }

  enable(): void {
    if (!this.active) return;
    const display = (globalThis as any).global?.display;
    this.handler = display?.connect('accelerator-activated', (_d: any, action: number) => this.fire(action)) ?? null;
  }

  /** Bind exactly `wanted`; a refused accelerator stays unbound. */
  sync(wanted: PermanentChord[]): void {
    if (!this.active) return;
    const display = (globalThis as any).global?.display;
    const plan = bindPlan(this.bound, wanted);
    for (const id of plan.release) this.release(display, id);
    for (const p of plan.grab) {
      const action = display?.grab_accelerator?.(p.accel, Meta.KeyBindingFlags.NONE) ?? 0;
      if (!action) { console.log(`clipmonger: permanent hotkey ${p.accel} is already in use`); continue; }
      wm.allowKeybinding(Meta.external_binding_name_for_action(action), Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW);
      this.bound[p.id] = p.accel;
      this.actions.set(action, p.id);
      this.ids.set(p.id, action);
    }
  }

  private release(display: any, id: string): void {
    const action = this.ids.get(id);
    if (action !== undefined) {
      try { display?.ungrab_accelerator?.(action); } catch { /* already gone */ }
      this.actions.delete(action);
      this.ids.delete(id);
    }
    delete this.bound[id];
  }

  private fire(action: number): void {
    const id = this.actions.get(action);
    if (!id) return;
    this.paste.targetFocused();
    void this.ipc.request(permanentExecRequest(this.ipc.nextId(), id)).catch(() => { /* the daemon logs it */ });
  }

  disable(): void {
    const display = (globalThis as any).global?.display;
    for (const id of Object.keys(this.bound)) this.release(display, id);
    if (this.handler !== null) {
      try { display?.disconnect(this.handler); } catch { /* shell gone */ }
      this.handler = null;
    }
  }
}
