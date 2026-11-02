// Shell glue: St.Clipboard 500 ms poll feeding PollDiff. Async reads only; a
// wedged read degrades to capture-paused (3 failures) and never blocks Shell.
import St from 'gi://St';
import GLib from 'gi://GLib?version=2.0';
import { captureReportRequest, newClipId } from './codec.js';
import { emitCaptureReport } from './report.js';
export class CaptureRelay {
    timer = null;
    diff;
    ipc;
    loggedFirstRead = false;
    pollMs;
    constructor(ipc, diff, pollMs = 500) { this.ipc = ipc; this.diff = diff; this.pollMs = pollMs; }
    setPollMs(ms) { this.pollMs = ms; }
    markSelfWrite(offers) { this.diff.markSelfWrite(offers, Date.now()); }
    start() {
        if (this.timer !== null)
            return;
        const tick = () => { void this.pollOnce(); return GLib.SOURCE_CONTINUE; };
        this.timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, this.pollMs, tick);
    }
    stop() { if (this.timer !== null) {
        GLib.source_remove(this.timer);
        this.timer = null;
    } }
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
                await this.ipc.request(captureReportRequest(this.ipc.nextId(), {
                    clip: newClipId(), origin: 'Clipboard', source_app: null,
                    sensitive: true, offers: [],
                }));
                this.ipc.relay.recordSuccess();
                return;
            }
            const focusedAppId = globalThis.global?.display?.get_focus_window?.()?.get_gtk_application_id?.() ?? null;
            if (this.diff.preflight(mimes, focusedAppId)) {
                this.ipc.relay.recordSuccess();
                return;
            }
            const offers = [];
            for (const mime of mimes) {
                if (!this.diff.wants(mime))
                    continue;
                if (mime.startsWith('text/plain') || mime === 'UTF8_STRING') {
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
                    if (text)
                        offers.push({ mime: 'text/plain', bytes: new TextEncoder().encode(text) });
                }
                else {
                    // VERIFY: read via MetaSelection transfer; until proven, size-only offer.
                    offers.push({ mime, sizeHint: 0 });
                }
            }
            const decision = this.diff.decide({ offers, focusedAppId, nowMs: Date.now() });
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
