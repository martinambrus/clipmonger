// Pure paste-assist plan (daemon `paste.assist` on GNOME Wayland). No gi:// imports.

/** A validated `paste.assist` request from the daemon. */
export interface AssistRequest { id: number; mime: string; data_b64: string; inject: boolean }
/** The `paste.assist_done` answer. */
export interface AssistDone { id: number; ok: boolean; injected: boolean; error?: string }

/** How long a popup's target stays valid while the popup is open. */
export const PRESS_TARGET_MAX_AGE_MS = 10 * 60 * 1000;
/** How long to wait for the focus to leave the popup after a pick. */
export const FOCUS_WAIT_MS = 600;

/** Validate the event params; `null` for anything malformed. */
export function parseAssist(params: unknown): AssistRequest | null {
  if (typeof params !== 'object' || params === null) return null;
  const p = params as Record<string, unknown>;
  if (!Number.isSafeInteger(p.id) || typeof p.mime !== 'string' || !p.mime
    || typeof p.data_b64 !== 'string') return null;
  return { id: p.id as number, mime: p.mime, data_b64: p.data_b64, inject: p.inject === true };
}

/** Whether `mime` is written as text (`St.Clipboard.set_text`). */
export function isTextMime(mime: string): boolean {
  return mime.split(';')[0].trim() === 'text/plain' || mime === 'UTF8_STRING';
}

export type AssistStep = 'wait' | 'inject' | 'toast';

/**
 * Next step once the clip is on the clipboard. Ctrl+V is pressed only into
 * the popup's target (the window focused right before the popup took the
 * focus), once it is focused again; any doubt (no target, popup still in
 * front past the wait, another window) leaves the clip on the clipboard
 * with a "press Ctrl+V" toast.
 */
export function assistStep(o: {
  inject: boolean; pressTarget: number | null; pressAgeMs: number;
  focused: number | null; popupFocused: boolean; waitedMs: number;
}): AssistStep {
  if (!o.inject || o.pressTarget === null || o.pressAgeMs > PRESS_TARGET_MAX_AGE_MS) return 'toast';
  if (o.popupFocused || o.focused === null) return o.waitedMs >= FOCUS_WAIT_MS ? 'toast' : 'wait';
  return o.focused === o.pressTarget ? 'inject' : 'toast';
}
