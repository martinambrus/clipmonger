// Pure poll-diff: offers-set + per-format fingerprint diff -> CaptureDecision. No gi:// imports.
import { sha256Hex } from './sha256.js';
import { isDenylisted, withinCap, DEFAULT_MAX_BYTES, DEFAULT_FORMATS, type AppIds } from './config.js';

export interface OfferInput {
  mime: string;
  bytes?: Uint8Array;
  sizeHint?: number;
  hash?: string;
}
export interface ReportedOffer { mime: string; bytes_hint: number; bytes?: Uint8Array; hash: string }
export type CaptureDecision =
  | { kind: 'Ignore'; reason: 'no-change' | 'denylist' | 'self-write' | 'oversize' | 'empty' }
  | { kind: 'Report'; offers: ReportedOffer[]; truncated: string[]; truncatedOffers: Array<{ mime: string; bytes_hint: number }> }
  | { kind: 'Sensitive'; reason: 'password-hint' | 'strict' };

export const PASSWORD_HINT_MIME = 'x-kde-passwordManagerHint';

export function fingerprintOffer(o: OfferInput): string {
  const size = o.bytes ? o.bytes.length : (o.sizeHint ?? 0);
  const hash = o.hash ?? (o.bytes ? sha256Hex(o.bytes) : '');
  return `${o.mime}:${size}:${hash}`;
}
export function fingerprintOffers(offers: OfferInput[]): string {
  return offers.map(fingerprintOffer).sort().join('|');
}

export interface PollDiffOptions { denylist?: string[]; maxBytes?: number; selfWriteTtlMs?: number; formats?: string[]; strict?: boolean }
export class PollDiff {
  private lastFingerprint = '';
  private selfWriteFp = '';
  private selfWriteExpiry = 0;
  private reading = false;
  denylist: string[];
  maxBytes: number;
  formats: string[];
  strict: boolean;
  readonly selfWriteTtlMs: number;
  constructor(opts: PollDiffOptions = {}) {
    this.denylist = opts.denylist ?? [];
    this.maxBytes = opts.maxBytes ?? DEFAULT_MAX_BYTES;
    this.formats = opts.formats ?? [...DEFAULT_FORMATS];
    this.strict = opts.strict ?? false;
    this.selfWriteTtlMs = opts.selfWriteTtlMs ?? 5000;
  }
  setReading(v: boolean): void { this.reading = v; }
  get isReading(): boolean { return this.reading; }
  /** Register a paste-assist self-write so the next poll of the same content is ignored (one-shot, 5 s TTL). */
  markSelfWrite(offers: OfferInput[], nowMs: number): void {
    this.selfWriteFp = fingerprintOffers(offers);
    this.selfWriteExpiry = nowMs + this.selfWriteTtlMs;
  }
  private configuredWants(mime: string): boolean {
    const canonical = mime === 'UTF8_STRING' ? 'text/plain' : mime;
    return this.formats.some((format) => format === canonical ||
      (format.split(';')[0] === 'text/plain' && canonical.split(';')[0] === 'text/plain'));
  }
  wants(mime: string): boolean {
    return this.configuredWants(mime) && (!this.strict || mime.split(';')[0] === 'text/plain' || mime === 'UTF8_STRING');
  }
  preflight(mimes: string[], focusedAppId: AppIds = null): CaptureDecision | null {
    if (mimes.includes(PASSWORD_HINT_MIME)) return { kind: 'Sensitive', reason: 'password-hint' };
    if (mimes.length > 32 || mimes.some((mime) => mime.length > 128 || !/^[\x20-\x7e]+$/.test(mime)))
      return { kind: 'Ignore', reason: 'empty' };
    if (isDenylisted(focusedAppId, this.denylist)) return { kind: 'Ignore', reason: 'denylist' };
    if (this.strict && mimes.some((mime) => this.configuredWants(mime)) && !mimes.some((mime) => this.wants(mime)))
      return { kind: 'Sensitive', reason: 'strict' };
    return null;
  }
  decide(input: { offers: OfferInput[]; focusedAppId?: AppIds; hintValue?: string | null; nowMs: number }): CaptureDecision {
    const { offers, focusedAppId = null, nowMs } = input;
    if (offers.some((offer) => offer.mime === PASSWORD_HINT_MIME)) return { kind: 'Sensitive', reason: 'password-hint' };
    if (offers.length > 32 || offers.some((offer) => offer.sizeHint !== undefined &&
      (!Number.isSafeInteger(offer.sizeHint) || offer.sizeHint < 0))) return { kind: 'Ignore', reason: 'empty' };
    const preflight = this.preflight(offers.map((offer) => offer.mime), focusedAppId);
    if (preflight) return preflight;
    if (offers.length === 0) return { kind: 'Ignore', reason: 'empty' };

    const wanted = offers.filter((offer) => this.wants(offer.mime));
    const truncatedOffers = wanted.filter((offer) => !withinCap(offer.bytes?.length ?? offer.sizeHint ?? 0, this.maxBytes))
      .map((offer) => ({ mime: offer.mime, bytes_hint: offer.bytes?.length ?? offer.sizeHint ?? 0 }));
    const truncated = truncatedOffers.map((offer) => offer.mime);
    // Decide whole-capture truncation before hashing any payload. Oversized
    // observations need only bounded MIME/size metadata, including for diff.
    const fp = truncated.length > 0
      ? wanted.map((offer) => `${offer.mime}:${offer.bytes?.length ?? offer.sizeHint ?? '?'}:oversize`).sort().join('|')
      : fingerprintOffers(wanted);
    if (fp === this.selfWriteFp && this.selfWriteFp !== '') {
      if (nowMs <= this.selfWriteExpiry) { this.selfWriteFp = ''; return { kind: 'Ignore', reason: 'self-write' }; }
      this.selfWriteFp = '';
    }
    if (fp === this.lastFingerprint) return { kind: 'Ignore', reason: 'no-change' };

    const reported: ReportedOffer[] = [];
    for (const offer of wanted) {
      const size = offer.bytes?.length ?? offer.sizeHint ?? 0;
      if (!withinCap(size, this.maxBytes)) continue;
      reported.push({ mime: offer.mime, bytes_hint: size,
        bytes: truncated.length > 0 ? undefined : offer.bytes,
        hash: truncated.length > 0 ? '' : offer.hash ?? (offer.bytes ? sha256Hex(offer.bytes) : '') });
    }
    if (reported.length === 0 && truncated.length === 0) return { kind: 'Ignore', reason: 'empty' };
    this.lastFingerprint = fp;
    return { kind: 'Report', offers: reported, truncated, truncatedOffers };
  }
  reset(): void { this.lastFingerprint = ''; this.selfWriteFp = ''; this.reading = false; }
}
