// Shell glue: St.Clipboard 500 ms poll feeding PollDiff. Async reads only; a
// wedged read degrades to capture-paused (3 failures) and never blocks Shell.
import St from 'gi://St';
import GLib from 'gi://GLib?version=2.0';
import { base64Encode, captureReportRequest, blobPutRequests, newClipId, CHUNK_RAW_BYTES } from './codec.js';
export class CaptureRelay {
    timer = null;
    diff;
    ipc;
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
            const mimes = typeof clipboard.get_mimetypes === 'function' ? await clipboard.get_mimetypes() : ['text/plain'];
            const offers = [];
            for (const mime of mimes) {
                if (mime.startsWith('text/plain') || mime === 'UTF8_STRING') {
                    const text = await new Promise((resolve) => clipboard.get_text(St.SelectionType.SELECTION_CLIPBOARD, (_c, t) => resolve(t ?? '')));
                    if (text)
                        offers.push({ mime: 'text/plain', bytes: new TextEncoder().encode(text) });
                }
                else {
                    // VERIFY: read via MetaSelection transfer; until proven, size-only offer.
                    offers.push({ mime, sizeHint: 0 });
                }
            }
            // VERIFY: focused app id via global.display.get_focus_window()
            // ?.get_gtk_application_id() / Shell.WindowTracker — exact call per version.
            const focusedAppId = globalThis.global?.display?.get_focus_window?.()?.get_gtk_application_id?.() ?? null;
            const hint = offers.find((o) => o.mime === 'x-kde-passwordManagerHint');
            void hint;
            const decision = this.diff.decide({ offers, focusedAppId, nowMs: Date.now() });
            if (decision.kind === 'Report')
                await this.emitReport(decision.offers, decision.truncated, focusedAppId);
            this.ipc.relay.recordSuccess();
        }
        catch {
            this.ipc.relay.recordFailure(); // 3 consecutive -> capture-paused intent
        }
        finally {
            this.diff.setReading(false);
        }
    }
    async emitReport(offers, truncated, sourceApp) {
        const clip = newClipId();
        const payloads = {};
        const eventOffers = [];
        for (const offer of offers) {
            const bytes = offer.bytes;
            if (bytes && bytes.length > CHUNK_RAW_BYTES) {
                for (const frame of blobPutRequests(this.ipc.nextId, clip, offer.mime, bytes))
                    await this.ipc.request(frame);
                eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, blob: true });
            }
            else if (bytes) {
                payloads[offer.mime] = base64Encode(bytes);
                eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, inline_b64: payloads[offer.mime] });
            }
            else
                eventOffers.push({ mime: offer.mime, bytes_hint: offer.bytes_hint });
        }
        const event = { clip, origin: 'Clipboard', source_app: sourceApp, sensitive: false, offers: eventOffers, truncated };
        await this.ipc.request(captureReportRequest(this.ipc.nextId(), event, payloads));
    }
}
