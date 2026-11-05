// Shell glue: St.Clipboard 500 ms poll feeding PollDiff. Async reads only; a
// wedged read degrades to capture-paused (3 failures) and never blocks Shell.
import St from 'gi://St';
import Meta from 'gi://Meta';
import GLib from 'gi://GLib?version=2.0';
import { captureReportRequest, newClipId } from './codec.js';
import { emitCaptureReport } from './report.js';
import { POPUP_TITLE, copySource } from './config.js';
/** Every identity of `win` the denylist may name: GTK app id, WM_CLASS
 * class and instance, and the executable name of its process. */
export function windowAppIds(win) {
    if (!win)
        return [];
    const ids = [];
    try {
        ids.push(win.get_gtk_application_id?.());
    }
    catch { /* none */ }
    try {
        ids.push(win.get_wm_class?.(), win.get_wm_class_instance?.());
    }
    catch { /* none */ }
    try {
        const pid = win.get_pid?.();
        if (typeof pid === 'number' && pid > 0)
            ids.push(GLib.file_read_link(`/proc/${pid}/exe`).split('/').pop());
    }
    catch { /* process gone or not ours */ }
    return ids.filter((id) => typeof id === 'string' && id !== '');
}
/** SHA-256 hex of `bytes` in GLib's C code (the pure-JS hash blocks the
 * Shell's main loop on large payloads); `undefined` lets the pure fallback
 * run if GLib fails. */
function nativeSha256(bytes) {
    try {
        return GLib.compute_checksum_for_bytes(GLib.ChecksumType.SHA256, GLib.Bytes.new(bytes)) ?? undefined;
    }
    catch {
        return undefined;
    }
}
/** Bytes of one clipboard format through `St.Clipboard.get_content`
 * (callback or promise form, as the Shell version offers); `null` when the
 * read fails or does not answer within 1.5 s. */
function readContent(clipboard, mime) {
    return new Promise((resolve) => {
        let settled = false;
        const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1500, () => { finish(null); return GLib.SOURCE_REMOVE; });
        function finish(bytes) {
            if (settled)
                return;
            settled = true;
            GLib.source_remove(timer);
            try {
                resolve(bytes ? new Uint8Array(bytes.get_data?.() ?? bytes.toArray?.() ?? []) : null);
            }
            catch {
                resolve(null);
            }
        }
        try {
            if (typeof clipboard.get_content !== 'function') {
                finish(null);
                return;
            }
            const returned = clipboard.get_content(St.ClipboardType.CLIPBOARD, mime, (_c, bytes) => finish(bytes));
            if (returned && typeof returned.then === 'function') {
                returned.then(finish, () => finish(null));
            }
        }
        catch {
            finish(null);
        }
    });
}
export class CaptureRelay {
    timer = null;
    diff;
    ipc;
    loggedFirstRead = false;
    /** The focused window's identities at the previous poll, and of the
     * last focused window that was not the popup. */
    previousFocus = { ids: [], appId: null };
    lastOtherFocus = { ids: [], appId: null };
    /** Non-text offers read since the clipboard owner last changed: an image
     * is read once per change, not on every poll. */
    binaryOffers = new Map();
    ownerChanged = true;
    /** The sensitive clipboard already reported (one privacy skip per copy,
     * not per poll): set until the owner changes; the offered formats key it
     * when the owner-changed signal is missing. */
    sensitiveReported = null;
    selectionHandler = null;
    pollMs;
    constructor(ipc, diff, pollMs = 500) { this.ipc = ipc; this.diff = diff; this.pollMs = pollMs; }
    setPollMs(ms) { this.pollMs = ms; }
    markSelfWrite(offers) { this.diff.markSelfWrite(offers, Date.now()); }
    start() {
        if (this.timer !== null)
            return;
        try {
            const selection = globalThis.global?.display?.get_selection?.();
            this.selectionHandler = selection?.connect('owner-changed', (_s, type) => {
                if (type === Meta.SelectionType.SELECTION_CLIPBOARD) {
                    this.ownerChanged = true;
                    this.sensitiveReported = null;
                }
            }) ?? null;
        }
        catch {
            this.selectionHandler = null;
        }
        const tick = () => { void this.pollOnce(); return GLib.SOURCE_CONTINUE; };
        this.timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, this.pollMs, tick);
    }
    stop() {
        if (this.timer !== null) {
            GLib.source_remove(this.timer);
            this.timer = null;
        }
        if (this.selectionHandler !== null) {
            try {
                globalThis.global?.display?.get_selection?.()?.disconnect(this.selectionHandler);
            }
            catch { /* shell gone */ }
            this.selectionHandler = null;
        }
    }
    async pollOnce() {
        if (this.diff.isReading || this.ipc.relay.capturePaused)
            return;
        this.diff.setReading(true);
        try {
            // VERIFY (T-702, Phase-0): St.Clipboard.get_mimetypes() shape and the
            // MetaSelection rich read (Meta.get_backend()/display selection,
            // get_mimetypes + async transfer returning GLib.Bytes) for
            // image/png + text/uri-list bytes in-Shell. Text path below is the
            // known-good St.Clipboard text API; rich offers stay behind this call.
            const clipboard = St.Clipboard.get_default();
            // get_mimetypes has no watchdog of its own: if the clipboard owner never
            // answers, this await hangs, isReading wedges true, and every later tick
            // exits silently at the guard. Bound it; on timeout assume text/plain so
            // the (already bounded) text read below still gets its chance.
            const mimes = await new Promise((resolve) => {
                const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1500, () => { resolve(['text/plain']); return GLib.SOURCE_REMOVE; });
                const done = (m) => { GLib.source_remove(timer); resolve(m ?? ['text/plain']); };
                try {
                    if (typeof clipboard.get_mimetypes !== 'function') {
                        done(null);
                        return;
                    }
                    const returned = clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD);
                    if (Array.isArray(returned))
                        done(returned);
                    else if (returned && typeof returned.then === 'function')
                        returned.then(done, () => done(null));
                    else
                        done(null);
                }
                catch {
                    done(null);
                }
            });
            // Privacy signals precede source-app queries and all ordinary reads.
            const privacy = this.diff.preflight(mimes);
            if (privacy?.kind === 'Sensitive') {
                const key = mimes.join('\n');
                if (this.sensitiveReported === key) {
                    this.ipc.relay.recordSuccess();
                    return;
                }
                this.sensitiveReported = key;
                await this.ipc.request(captureReportRequest(this.ipc.nextId(), {
                    clip: newClipId(), origin: 'Clipboard', source_app: null,
                    sensitive: true, offers: [],
                }));
                this.ipc.relay.recordSuccess();
                return;
            }
            const focusWindow = globalThis.global?.display?.get_focus_window?.() ?? null;
            const current = {
                ids: windowAppIds(focusWindow),
                appId: focusWindow?.get_gtk_application_id?.() ?? null,
                // No focused window (between the popup and the next window) counts
                // like the popup: the copy came from the last real window.
                popup: !focusWindow || focusWindow.get_title?.() === POPUP_TITLE,
            };
            const { ids: focusedAppIds, appId: focusedAppId } = copySource(current, this.previousFocus, this.lastOtherFocus);
            this.previousFocus = current;
            if (!current.popup)
                this.lastOtherFocus = current;
            if (this.diff.preflight(mimes, focusedAppIds)) {
                this.ipc.relay.recordSuccess();
                return;
            }
            const offers = [];
            let textRead = false;
            // A change during this read sets the flag again for the next poll.
            const changed = this.ownerChanged || this.selectionHandler === null;
            this.ownerChanged = false;
            for (const mime of mimes) {
                if (!this.diff.wants(mime))
                    continue;
                if (mime.startsWith('text/plain') || mime === 'UTF8_STRING') {
                    // Every text target carries the same text: read it once.
                    if (textRead)
                        continue;
                    textRead = true;
                    // get_text may answer via callback or via a returned promise,
                    // depending on Shell version; a callback that never fires must
                    // not wedge isReading.
                    const text = await new Promise((resolve) => {
                        const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1500, () => { resolve(''); return GLib.SOURCE_REMOVE; });
                        const done = (t) => { GLib.source_remove(timer); resolve(t ?? ''); };
                        const returned = clipboard.get_text(St.ClipboardType.CLIPBOARD, (_c, t) => done(t));
                        if (returned && typeof returned.then === 'function') {
                            returned.then(done, () => done(''));
                        }
                    });
                    if (!this.loggedFirstRead) {
                        this.loggedFirstRead = true;
                        console.log(`clipmonger: clipboard read ok (${mimes.length} mimes, text=${text.length} chars)`);
                    }
                    if (text) {
                        const bytes = new TextEncoder().encode(text);
                        offers.push({ mime: 'text/plain', bytes, hash: nativeSha256(bytes) });
                    }
                }
                else {
                    // Images, file lists and rich text: read the bytes, so the extension
                    // (the only capture path on GNOME Wayland) stores the whole clip.
                    // Without the owner-changed signal every poll reads (fallback).
                    const cached = changed ? undefined : this.binaryOffers.get(mime);
                    if (cached) {
                        offers.push(cached);
                        continue;
                    }
                    const bytes = await readContent(clipboard, mime);
                    // Hash once per owner change, natively: the cached offer keeps it,
                    // so later polls never re-hash a large image.
                    const offer = bytes && bytes.length > 0 ? { mime, bytes, hash: nativeSha256(bytes) } : { mime, sizeHint: 0 };
                    this.binaryOffers.set(mime, offer);
                    offers.push(offer);
                }
            }
            if (changed) {
                for (const mime of [...this.binaryOffers.keys()])
                    if (!mimes.includes(mime))
                        this.binaryOffers.delete(mime);
            }
            const decision = this.diff.decide({ offers, focusedAppId: focusedAppIds, nowMs: Date.now() });
            if (decision.kind === 'Report') {
                console.log(`clipmonger: capture report (${decision.offers.length} offers, app=${focusedAppId ?? '?'})`);
                await emitCaptureReport(this.ipc.nextId, (frame) => this.ipc.request(frame), decision.offers, decision.truncated, focusedAppId, decision.truncatedOffers);
            }
            this.ipc.relay.recordSuccess();
        }
        catch (e) {
            console.log(`clipmonger: capture poll failed: ${e instanceof Error ? e.message : String(e)}`);
            this.ipc.relay.recordFailure(); // 3 consecutive -> capture-paused intent
        }
        finally {
            this.diff.setReading(false);
        }
    }
}
