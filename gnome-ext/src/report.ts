// Pure production relay decision: oversized captures send metadata before
// any ordinary offer can reach blob.put EOF (which independently ingests).
import { base64Encode, captureReportRequest, blobPutRequests, newClipId, CHUNK_RAW_BYTES, type CaptureEvent, type Envelope } from './codec.js';
import type { ReportedOffer } from './polddiff.js';

export async function emitCaptureReport(
  nextId: () => string,
  request: (frame: Envelope) => Promise<unknown>,
  offers: ReportedOffer[],
  truncated: string[],
  sourceApp: string | null,
  truncatedOffers: Array<{ mime: string; bytes_hint: number }> = [],
): Promise<void> {
  const clip = newClipId();
  if (truncated.length > 0) {
    // Keep size-only metadata for ordinary offers, including strict privacy
    // classification in the daemon. Preserve known truncated sizes; omitted
    // hints remain honestly unknown under the existing event.offers contract.
    const event: CaptureEvent = {
      clip, origin: 'Clipboard', source_app: sourceApp, sensitive: false,
      offers: [...offers.map((offer) => ({ mime: offer.mime, bytes_hint: offer.bytes_hint })), ...truncatedOffers],
      truncated,
    };
    await request(captureReportRequest(nextId(), event));
    return;
  }
  const payloads: Record<string, string> = {};
  const eventOffers: CaptureEvent['offers'] = [];
  for (const offer of offers) {
    const bytes = offer.bytes;
    if (bytes && bytes.length > CHUNK_RAW_BYTES) {
      for (const frame of blobPutRequests(nextId, clip, offer.mime, bytes)) await request(frame);
      eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, blob: true });
    } else if (bytes) {
      payloads[offer.mime] = base64Encode(bytes);
      eventOffers.push({ mime: offer.mime, bytes_hint: bytes.length, inline_b64: payloads[offer.mime] });
    } else eventOffers.push({ mime: offer.mime, bytes_hint: offer.bytes_hint });
  }
  const event: CaptureEvent = { clip, origin: 'Clipboard', source_app: sourceApp, sensitive: false, offers: eventOffers, truncated };
  await request(captureReportRequest(nextId(), event, payloads));
}
