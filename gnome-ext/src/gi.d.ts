// Minimal ambient declarations for Shell-only modules so `tsc --noEmit` is
// clean headless. @girs/gnome-shell 46.0.2 is pinned for API reference, but
// its generated ambient modules are not loaded here (they conflict across
// @girs generations); every declaration below is VERIFY-on-GNOME (T-702).
// Pure modules must NOT import any of these.
declare module 'gi://Gio?version=2.0' { const m: any; export default m; }
declare module 'gi://GLib?version=2.0' { const m: any; export default m; }
declare module 'gi://St' { const m: any; export default m; }
declare module 'gi://Gtk?version=4.0' { const m: any; export default m; }
declare module 'gi://Adw?version=1' { const m: any; export default m; }
declare module 'resource:///org/gnome/shell/ui/main.js' { export const wm: any; export function notify(title: string, body?: string): void; }
declare module 'resource:///org/gnome/shell/extensions/extension.js' {
  export class Extension { constructor(init: any); enable(): void; disable(): void; getSettings(): any; }
  export class ExtensionPreferences { fillPreferencesWindow(window: any): void; getSettings(): any; }
}
