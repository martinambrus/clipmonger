// Pure session check. No gi:// imports.

/** Whether the Shell runs as a Wayland compositor. GNOME 49 removed the X11
 * session and with it `Meta.is_wayland_compositor` (absent from Mutter 18,
 * GNOME 50), so a missing function means Wayland, not X11. */
export function isWaylandSession(meta: { is_wayland_compositor?: () => boolean }): boolean {
  return typeof meta.is_wayland_compositor === 'function' ? Boolean(meta.is_wayland_compositor()) : true;
}
