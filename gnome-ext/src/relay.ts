// Pure relay state machine: Disconnected/Connecting/Hello/Ready/Degraded.
// Fake clock + rng injected; no gi:// imports, no real timers.
export type RelayState = 'Disconnected' | 'Connecting' | 'Hello' | 'Ready' | 'Degraded';
export interface Clock {
  now(): number;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}
export type RelayIntent =
  | { type: 'connect' }
  | { type: 'close' }
  | { type: 'send-hello' }
  | { type: 'send'; method: string }
  | { type: 'resync' }
  | { type: 'capture-paused'; failures: number }
  | { type: 'capture-resumed' }
  | { type: 'auth-error' };

export interface RelayOptions {
  clock: Clock;
  rng?: () => number;
  onIntent?: (intent: RelayIntent) => void;
  heartbeatMs?: number;
  missLimit?: number;
  failureLimit?: number;
}

export class Relay {
  state: RelayState = 'Disconnected';
  capturePaused = false;
  private clock: Clock;
  private rng: () => number;
  private onIntent?: (i: RelayIntent) => void;
  private heartbeatMs: number;
  private missLimit: number;
  private failureLimit: number;
  private attempt = 0;
  private misses = 0;
  private failures = 0;
  private heartbeatTimer: unknown = null;
  private reconnectTimer: unknown = null;
  private disabled = false;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

  constructor(opts: RelayOptions) {
    this.clock = opts.clock;
    this.rng = opts.rng ?? (() => 0.5);
    this.onIntent = opts.onIntent;
    this.heartbeatMs = opts.heartbeatMs ?? 15000;
    this.missLimit = opts.missLimit ?? 3;
    this.failureLimit = opts.failureLimit ?? 3;
  }
  private emit(i: RelayIntent): void { if (!this.disabled) this.onIntent?.(i); }
  /** Backoff 1s,2s,4s,8s,16s capped 30s, +/-20% jitter. rng()=0.5 gives the exact sequence. */
  backoffDelayMs(attempt = this.attempt): number {
    const base = Math.min(30000, 1000 * 2 ** attempt);
    return Math.round(base * (1 + (this.rng() * 0.4 - 0.2)));
  }
  start(): void {
    if (this.state !== 'Disconnected' && this.state !== 'Degraded') return;
    this.disabled = false;
    this.clearReconnect();
    this.state = 'Connecting';
    this.emit({ type: 'connect' });
  }
  handleConnected(): void {
    if (this.state !== 'Connecting') return;
    this.state = 'Hello';
    this.emit({ type: 'send-hello' });
  }
  handleHelloOk(): void {
    this.state = 'Ready';
    this.attempt = 0; this.misses = 0; this.failures = 0;
    if (this.capturePaused) { this.capturePaused = false; this.emit({ type: 'capture-resumed' }); }
    // Resync ordering: subscribe, then config.get (executed by glue from these intents).
    this.emit({ type: 'send', method: 'events.subscribe' });
    this.emit({ type: 'send', method: 'config.get' });
    this.emit({ type: 'resync' });
    this.scheduleHeartbeat();
  }
  handleHelloError(code: string): void {
    if (code === 'auth_failed') this.emit({ type: 'auth-error' });
    this.scheduleReconnect();
  }
  handleDisconnect(): void { if (this.state !== 'Disconnected') this.scheduleReconnect(); }
  private scheduleReconnect(): void {
    this.clearHeartbeat();
    this.rejectAllPending(new Error('disconnected'));
    if (this.disabled) { this.state = 'Disconnected'; return; }
    this.state = 'Degraded';
    const delay = this.backoffDelayMs();
    this.attempt++;
    this.clearReconnect();
    this.reconnectTimer = this.clock.setTimeout(() => { this.reconnectTimer = null; this.start(); }, delay);
  }
  private scheduleHeartbeat(): void {
    this.clearHeartbeat();
    if (this.disabled || this.state !== 'Ready') return;
    this.heartbeatTimer = this.clock.setTimeout(() => {
      this.heartbeatTimer = null;
      if (this.state !== 'Ready') return;
      this.misses++;
      if (this.misses >= this.missLimit) {
        this.emit({ type: 'close' });
        this.scheduleReconnect();
        return;
      }
      this.emit({ type: 'send', method: 'ping' });
      this.scheduleHeartbeat();
    }, this.heartbeatMs);
  }
  /** A pong (any ping response) resets the miss counter. */
  handlePong(): void { this.misses = 0; }
  get missedHeartbeats(): number { return this.misses; }
  get reconnectAttempt(): number { return this.attempt; }

  registerPending(id: string): Promise<unknown> {
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
  /** Resolve by envelope id; out-of-order responses are normal. Returns false for unknown ids. */
  resolveResponse(id: string, result: unknown): boolean {
    const p = this.pending.get(id);
    if (!p) return false;
    this.pending.delete(id);
    p.resolve(result);
    return true;
  }
  rejectResponse(id: string, error: Error): boolean {
    const p = this.pending.get(id);
    if (!p) return false;
    this.pending.delete(id);
    p.reject(error);
    return true;
  }
  get pendingCount(): number { return this.pending.size; }
  private rejectAllPending(e: Error): void { for (const p of this.pending.values()) p.reject(e); this.pending.clear(); }

  /** 3 consecutive capture/IPC failures -> capture-paused, emitted once until a success. */
  recordFailure(): void {
    this.failures++;
    if (this.failures >= this.failureLimit && !this.capturePaused) {
      this.capturePaused = true;
      this.emit({ type: 'capture-paused', failures: this.failures });
    }
  }
  recordSuccess(): void {
    this.failures = 0;
    if (this.capturePaused) { this.capturePaused = false; this.emit({ type: 'capture-resumed' }); }
  }
  get consecutiveFailures(): number { return this.failures; }

  disable(): void {
    this.disabled = true;
    this.clearHeartbeat(); this.clearReconnect();
    this.rejectAllPending(new Error('disabled'));
    this.state = 'Disconnected';
  }
  private clearHeartbeat(): void { if (this.heartbeatTimer !== null) { this.clock.clearTimeout(this.heartbeatTimer); this.heartbeatTimer = null; } }
  private clearReconnect(): void { if (this.reconnectTimer !== null) { this.clock.clearTimeout(this.reconnectTimer); this.reconnectTimer = null; } }
}

/** Deterministic clock for tests. */
export class FakeClock implements Clock {
  private t = 0;
  private next = 1;
  private timers = new Map<number, { at: number; fn: () => void }>();
  now(): number { return this.t; }
  setTimeout(fn: () => void, ms: number): unknown { const id = this.next++; this.timers.set(id, { at: this.t + ms, fn }); return id; }
  clearTimeout(h: unknown): void { this.timers.delete(h as number); }
  advance(ms: number): void {
    const end = this.t + ms;
    for (;;) {
      let best: [number, { at: number; fn: () => void }] | null = null;
      for (const e of this.timers) if (e[1].at <= end && (!best || e[1].at < best[1].at)) best = e;
      if (!best) break;
      this.timers.delete(best[0]); this.t = best[1].at; best[1].fn();
    }
    this.t = end;
  }
  get pendingTimers(): number { return this.timers.size; }
}
