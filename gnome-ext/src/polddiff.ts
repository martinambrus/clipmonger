// Pure poll-diff: offers-set + per-format fingerprint diff -> CaptureDecision. No gi:// imports.
import { sha256Hex } from './sha256.js';
import { isDenylisted, withinCap, DEFAULT_MAX_BYTES } from './config.js';

export interface OfferInput {
  mime: string;
  bytes?: Uint8Array;
  sizeHint?: number;
  hash?: string;
}
export interface ReportedOffer { mime: string; bytes_hint: number; bytes?: Uint8Array; hash: string }
export type CaptureDecision =
  | { kind: 'Ignore'; reason: 'no-change' | 'denylist' | 'self-write' | 'oversize' | 'empty' }
  | { kind: 'Report'; offers: ReportedOffer[]; truncated: string[] }
  | { kind: 'Sensitive'; reason: 'password-hint' };

export const PASSWORD_HINT_MIME = 'x-kde-passwordManagerHint';

export function fingerprintOffer(o: OfferInput): string {
  const size = o.bytes ? o.bytes.length : (o.sizeHint ?? 0);
  const hash = o.hash ?? (o.bytes ? sha256Hex(o.bytes) : '');
  return `${o.mime}:${size}:${hash}`;
}
export function fingerprintOffers(offers: OfferInput[]): string {
  return offers.map(fingerprintOffer).sort().join('|');
}

export interface PollDiffOptions { denylist?: string[]; maxBytes?: number; selfWriteTtlMs?: number }
export class PollDiff {
  private lastFingerprint = '';
  private selfWriteFp = '';
  private selfWriteExpiry = 0;
  private reading = false;
  denylist: string[];
  maxBytes: number;
  readonly selfWriteTtlMs: number;
  constructor(opts: PollDiffOptions = {}) {
    this.denylist = opts.denylist ?? [];
    this.maxBytes = opts.maxBytes ?? DEFAULT_MAX_BYTES;
    this.selfWriteTtlMs = opts.selfWriteTtlMs ?? 5000;
  }
  setReading(v: boolean): void { this.reading = v; }
  get isReading(): boolean { return this.reading; }
  /** Register a paste-assist self-write so the next poll of the same content is ignored (one-shot, 5 s TTL). */
  markSelfWrite(offers: OfferInput[], nowMs: number): void {
    this.selfWriteFp = fingerprintOffers(offers);
    this.selfWriteExpiry = nowMs + this.selfWriteTtlMs;
  }
  decide(input: { offers: OfferInput[]; focusedAppId?: string | null; hintValue?: string | null; nowMs: number }): CaptureDecision {
    const { offers, focusedAppId = null, nowMs } = input;
    // Denylist first: emit nothing at all, request no payload reads.
    if (isDenylisted(focusedAppId, this.denylist)) return { kind: 'Ignore', reason: 'denylist' };
    // NOTE: no `reading` check here. pollOnce sets the guard flag before the
    // async read and tests it at tick entry to drop overlapping ticks; decide
    // itself must always evaluate the owning tick's offers. A check here made
    // every poll return Ignore('reading'), so capture never reported.
    if (offers.length === 0) return { kind: 'Ignore', reason: 'empty' };
    // Password hint: suppress before any payload handling.
    const hintOffer = offers.find((o) => o.mime === PASSWORD_HINT_MIME);
    const hintText = input.hintValue ?? (hintOffer?.bytes ? new TextDecoder().decode(hintOffer.bytes).trim().toLowerCase() : null);
    if (hintOffer && hintText === 'secret') return { kind: 'Sensitive', reason: 'password-hint' };

    const fp = fingerprintOffers(offers);
    if (fp === this.selfWriteFp && this.selfWriteFp !== '') {
      if (nowMs <= this.selfWriteExpiry) { this.selfWriteFp = ''; return { kind: 'Ignore', reason: 'self-write' }; }
      this.selfWriteFp = '';
    }
    if (fp === this.lastFingerprint) return { kind: 'Ignore', reason: 'no-change' };

    const reported: ReportedOffer[] = [];
    const truncated: string[] = [];
    for (const o of offers) {
      if (o.mime === PASSWORD_HINT_MIME) continue;
      const size = o.bytes ? o.bytes.length : (o.sizeHint ?? 0);
      if (!withinCap(size, this.maxBytes)) { truncated.push(o.mime); continue; }
      reported.push({ mime: o.mime, bytes_hint: size, bytes: o.bytes, hash: o.hash ?? (o.bytes ? sha256Hex(o.bytes) : '') });
    }
    if (reported.length === 0) return { kind: 'Ignore', reason: 'oversize' };
    this.lastFingerprint = fp;
    return { kind: 'Report', offers: reported, truncated };
  }
  reset(): void { this.lastFingerprint = ''; this.selfWriteFp = ''; this.reading = false; }
}
