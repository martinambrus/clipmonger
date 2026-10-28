// Shell glue: St.Clipboard 500 ms poll feeding PollDiff. Async reads only; a
// wedged read degrades to capture-paused (3 failures) and never blocks Shell.
import St from 'gi://St';
import GLib from 'gi://GLib?version=2.0';
import { PollDiff, type OfferInput } from './polddiff.js';
import { base64Encode, captureReportRequest, blobPutRequests, newClipId, CHUNK_RAW_BYTES, type CaptureEvent } from './codec.js';
import type { IpcClient } from './ipc.js';

export class CaptureRelay {
  private timer: number | null = null;
  private diff: PollDiff;
  private ipc: IpcClient;
  private pollMs: number;
  constructor(ipc: IpcClient, diff: PollDiff, pollMs = 500) { this.ipc = ipc; this.diff = diff; this.pollMs = pollMs; }
  setPollMs(ms: number): void { this.pollMs = ms; }
  markSelfWrite(offers: OfferInput[]): void { this.diff.markSelfWrite(offers, Date.now()); }

  start(): void {
    if (this.timer !== null) return;
    const tick = () => { void this.pollOnce(); return GLib.SOURCE_CONTINUE; };
    this.timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, this.pollMs, tick);
  }
  stop(): void { if (this.timer !== null) { GLib.source_remove(this.timer); this.timer = null; } }

  private async pollOnce(): Promise<void> {
    if (this.diff.isReading || this.ipc.relay.capturePaused) return;
    this.diff.setReading(true);
    try {
      // VERIFY (T-702, Phase-0): St.Clipboard.get_mimetypes() shape and the
      // MetaSelection rich read (Meta.get_backend()/display selection,
      // get_mimetypes + async transfer returning GLib.Bytes) for
      // image/png + text/uri-list bytes in-Shell. Text path below is the
      // known-good St.Clipboard text API; rich offers stay behind this call.
      const clipboard = St.Clipboard.get_default();
      const mimes: string[] = typeof clipboard.get_mimetypes === 'function' ? await clipboard.get_mimetypes(St.SelectionType.SELECTION_CLIPBOARD) : ['text/plain'];
      const offers: OfferInput[] = [];
      for (const mime of mimes) {
        if (mime.startsWith('text/plain') || mime === 'UTF8_STRING') {
          // A get_text callback that never fires must not wedge isReading.
          const text: string = await new Promise((resolve) => {
            const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1500, () => { resolve(''); return GLib.SOURCE_REMOVE; });
            clipboard.get_text(St.SelectionType.SELECTION_CLIPBOARD, (_c: any, t: string | null) => {
              GLib.source_remove(timer);
              resolve(t ?? '');
            });
          });
          if (text) offers.push({ mime: 'text/plain', bytes: new TextEncoder().encode(text) });
        } else {
          // VERIFY: read via MetaSelection transfer; until proven, size-only offer.
          offers.push({ mime, sizeHint: 0 });
        }
      }
      // VERIFY: focused app id via global.display.get_focus_window()
      // ?.get_gtk_application_id() / Shell.WindowTracker — exact call per version.
      const focusedAppId: string | null = (globalThis as any).global?.display?.get_focus_window?.()?.get_gtk_application_id?.() ?? null;
      const hint = offers.find((o) => o.mime === 'x-kde-passwordManagerHint');
      void hint;
      const decision = this.diff.decide({ offers, focusedAppId, nowMs: Date.now() });
      if (decision.kind === 'Report') {
        console.log(`clipmonger: capture report (${decision.offers.length} offers, app=${focusedAppId ?? '?'})`);
        await this.emitReport(decision.offers, decision.truncated, focusedAppId);
      }
      this.ipc.relay.recordSuccess();
    } catch (e) {
      console.log(`clipmonger: capture poll failed: ${e instanceof Error ? e.message : String(e)}`);
      this.ipc.relay.recordFailure(); // 3 consecutive -> capture-paused intent
    } finally { this.diff.setReading(false); }
  }

  private async emitReport(offers: Array<{ mime: string; bytes_hint: number; bytes?: Uint8Array }>, truncated: string[], sourceApp: string | null): Promise<void> {
    const clip = newClipId();
    const payloads: Record<string, string> = {};
    const eventOffers: CaptureEvent['offers'] = [];
    for (const offer of offers) {
      const bytes = offer.bytes;
      if (bytes && bytes.length > CHUNK_RAW_BYTES) {
        for (const frame of blobPutRequests(this.ipc.nextId, clip, offer.mime, bytes)) await this.ipc.request(frame);
        eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, blob: true });
      } else if (bytes) {
        payloads[offer.mime] = base64Encode(bytes);
        eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, inline_b64: payloads[offer.mime] });
      } else eventOffers.push({ mime: offer.mime, bytes_hint: offer.bytes_hint });
    }
    const event: CaptureEvent = { clip, origin: 'Clipboard', source_app: sourceApp, sensitive: false, offers: eventOffers, truncated };
    await this.ipc.request(captureReportRequest(this.ipc.nextId(), event, payloads));
  }
}
