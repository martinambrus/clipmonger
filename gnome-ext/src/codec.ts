// Pure IPC wire codec (crates/ipc/src/proto.rs + proto/methods.rs).
// No gi:// imports: unit-testable under node --test.
import { sha256Hex } from './sha256.js';
export { sha256Hex, sha256 } from './sha256.js';

export const IPC_VERSION = 'ipc/v1';
export const MAX_LINE_BYTES = 1_048_576;
export const CHUNK_RAW_BYTES = 262_144;

export type FrameKind = 'req' | 'res' | 'event' | 'cancel';
export interface IpcErrorShape { code: string; message: string; retryable: boolean; backend?: string }
export interface Envelope {
  v: string; id: string; kind: FrameKind;
  method?: string; params?: unknown; result?: unknown; error?: IpcErrorShape; seq?: number;
}
export class CodecError extends Error {
  constructor(public code: string, message: string) { super(message); this.name = 'CodecError'; }
}

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
export function base64Encode(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i], b = i + 1 < bytes.length ? bytes[i + 1] : 0, c = i + 2 < bytes.length ? bytes[i + 2] : 0;
    const n = (a << 16) | (b << 8) | c;
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63]
      + (i + 1 < bytes.length ? B64[(n >> 6) & 63] : '=')
      + (i + 2 < bytes.length ? B64[n & 63] : '=');
  }
  return out;
}
export function base64Decode(text: string): Uint8Array {
  const clean = text.replace(/[^A-Za-z0-9+/=]/g, '');
  const out: number[] = [];
  for (let i = 0; i < clean.length; i += 4) {
    const v = [0, 1, 2, 3].map((j) => { const ch = clean[i + j]; return ch === '=' || ch === undefined ? 64 : B64.indexOf(ch); });
    const n = (v[0] << 18) | (v[1] << 12) | ((v[2] & 63) << 6) | (v[3] & 63);
    out.push((n >> 16) & 255);
    if (v[2] !== 64) out.push((n >> 8) & 255);
    if (v[3] !== 64) out.push(n & 255);
  }
  return new Uint8Array(out);
}

export function createIdGen(prefix = 'e'): () => string {
  let n = 0;
  return () => `${prefix}${++n}`;
}

export function encodeEnvelope(env: Envelope): string {
  if (env.v !== IPC_VERSION) throw new CodecError('version_mismatch', `expected ${IPC_VERSION}, got ${env.v}`);
  const line = JSON.stringify(env);
  if (new TextEncoder().encode(line).length > MAX_LINE_BYTES)
    throw new CodecError('oversize', `frame exceeds ${MAX_LINE_BYTES} bytes`);
  return line;
}
export function decodeEnvelope(line: string): Envelope {
  if (new TextEncoder().encode(line).length > MAX_LINE_BYTES)
    throw new CodecError('oversize', `frame exceeds ${MAX_LINE_BYTES} bytes`);
  let env: Envelope;
  try { env = JSON.parse(line) as Envelope; } catch (e) { throw new CodecError('decode_failed', String(e)); }
  if (env.v !== IPC_VERSION) throw new CodecError('version_mismatch', `expected ${IPC_VERSION}, got ${env.v}`);
  if (!['req', 'res', 'event', 'cancel'].includes(env.kind)) throw new CodecError('decode_failed', `bad kind ${String(env.kind)}`);
  if (typeof env.id !== 'string' || !env.id) throw new CodecError('decode_failed', 'missing id');
  return env;
}

export interface BlobChunk { seq: number; data_b64: string; eof: boolean; sha256?: string }
/** Split raw bytes into 256 KiB blob.put chunks; sha256 (total hex) only on the eof chunk. */
export function chunkPayload(bytes: Uint8Array): BlobChunk[] {
  const total = sha256Hex(bytes);
  if (bytes.length === 0) return [{ seq: 0, data_b64: '', eof: true, sha256: total }];
  const chunks: BlobChunk[] = [];
  const count = Math.ceil(bytes.length / CHUNK_RAW_BYTES);
  for (let i = 0; i < count; i++) {
    const piece = bytes.subarray(i * CHUNK_RAW_BYTES, (i + 1) * CHUNK_RAW_BYTES);
    const last = i === count - 1;
    const chunk: BlobChunk = { seq: i, data_b64: base64Encode(piece), eof: last };
    if (last) chunk.sha256 = total;
    chunks.push(chunk);
  }
  return chunks;
}

const req = (id: string, method: string, params: unknown): Envelope => ({ v: IPC_VERSION, id, kind: 'req', method, params });
export const helloRequest = (id: string, token: string): Envelope =>
  req(id, 'hello', { min: 1, max: 1, token, client: 'gnome-ext' });
export const pingRequest = (id: string): Envelope => req(id, 'ping', {});
export const configGetRequest = (id: string): Envelope => req(id, 'config.get', {});
export const eventsSubscribeRequest = (id: string): Envelope =>
  req(id, 'events.subscribe', { topics: ['config.changed'] });
export const hotkeyFiredRequest = (id: string, hotkeyId: number): Envelope =>
  req(id, 'hotkey.fired', { id: hotkeyId });
export const hotkeyConflictRequest = (id: string, chord: string, reason: string): Envelope =>
  req(id, 'hotkey.conflict', { chord, reason });
export interface CaptureEvent {
  clip: string; origin: string; source_app: string | null; sensitive: boolean;
  offers: Array<{ mime: string; bytes_hint: number; inline_b64?: string; blob?: boolean }>;
  truncated?: string[];
}
/** G-F3: capture.report params = {event, payloads:{mime:base64}}; large payloads go via blobPutRequests first. */
export function captureReportRequest(id: string, event: CaptureEvent, payloads: Record<string, string> = {}): Envelope {
  return req(id, 'capture.report', { event, payloads });
}
export function blobPutRequests(nextId: () => string, clip: string, mime: string, bytes: Uint8Array): Envelope[] {
  return chunkPayload(bytes).map((c) => {
    const params: Record<string, unknown> = { clip, mime, seq: c.seq, data_b64: c.data_b64, eof: c.eof };
    if (c.sha256 !== undefined) params.sha256 = c.sha256;
    return req(nextId(), 'blob.put', params);
  });
}
/** Mint a UUID-shaped clip id (ext-side, per BlobPutParams.clip). Not RFC4122 v7 timing; random form. */
export function newClipId(rng: () => number = Math.random): string {
  const b = new Uint8Array(16);
  for (let i = 0; i < 16; i++) b[i] = Math.floor(rng() * 256);
  b[6] = (b[6] & 0x0f) | 0x70; b[8] = (b[8] & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
