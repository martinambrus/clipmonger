export class Relay {
    state = 'Disconnected';
    capturePaused = false;
    clock;
    rng;
    onIntent;
    heartbeatMs;
    missLimit;
    failureLimit;
    attempt = 0;
    misses = 0;
    failures = 0;
    heartbeatTimer = null;
    reconnectTimer = null;
    disabled = false;
    pending = new Map();
    constructor(opts) {
        this.clock = opts.clock;
        this.rng = opts.rng ?? (() => 0.5);
        this.onIntent = opts.onIntent;
        this.heartbeatMs = opts.heartbeatMs ?? 15000;
        this.missLimit = opts.missLimit ?? 3;
        this.failureLimit = opts.failureLimit ?? 3;
    }
    emit(i) { if (!this.disabled)
        this.onIntent?.(i); }
    /** Backoff 1s,2s,4s,8s,16s capped 30s, +/-20% jitter. rng()=0.5 gives the exact sequence. */
    backoffDelayMs(attempt = this.attempt) {
        const base = Math.min(30000, 1000 * 2 ** attempt);
        return Math.round(base * (1 + (this.rng() * 0.4 - 0.2)));
    }
    start() {
        if (this.state !== 'Disconnected' && this.state !== 'Degraded')
            return;
        this.disabled = false;
        this.clearReconnect();
        this.state = 'Connecting';
        this.emit({ type: 'connect' });
    }
    handleConnected() {
        if (this.state !== 'Connecting')
            return;
        this.state = 'Hello';
        this.emit({ type: 'send-hello' });
    }
    handleHelloOk() {
        this.state = 'Ready';
        this.attempt = 0;
        this.misses = 0;
        this.failures = 0;
        if (this.capturePaused) {
            this.capturePaused = false;
            this.emit({ type: 'capture-resumed' });
        }
        // Resync ordering: subscribe, then config.get (executed by glue from these intents).
        this.emit({ type: 'send', method: 'events.subscribe' });
        this.emit({ type: 'send', method: 'config.get' });
        this.emit({ type: 'resync' });
        this.scheduleHeartbeat();
    }
    handleHelloError(code) {
        if (code === 'auth_failed')
            this.emit({ type: 'auth-error' });
        this.scheduleReconnect();
    }
    handleDisconnect() { if (this.state !== 'Disconnected')
        this.scheduleReconnect(); }
    scheduleReconnect() {
        this.clearHeartbeat();
        this.rejectAllPending(new Error('disconnected'));
        if (this.disabled) {
            this.state = 'Disconnected';
            return;
        }
        this.state = 'Degraded';
        const delay = this.backoffDelayMs();
        this.attempt++;
        this.clearReconnect();
        this.reconnectTimer = this.clock.setTimeout(() => { this.reconnectTimer = null; this.start(); }, delay);
    }
    scheduleHeartbeat() {
        this.clearHeartbeat();
        if (this.disabled || this.state !== 'Ready')
            return;
        this.heartbeatTimer = this.clock.setTimeout(() => {
            this.heartbeatTimer = null;
            if (this.state !== 'Ready')
                return;
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
    handlePong() { this.misses = 0; }
    get missedHeartbeats() { return this.misses; }
    get reconnectAttempt() { return this.attempt; }
    registerPending(id) {
        return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
    }
    /** Resolve by envelope id; out-of-order responses are normal. Returns false for unknown ids. */
    resolveResponse(id, result) {
        const p = this.pending.get(id);
        if (!p)
            return false;
        this.pending.delete(id);
        p.resolve(result);
        return true;
    }
    rejectResponse(id, error) {
        const p = this.pending.get(id);
        if (!p)
            return false;
        this.pending.delete(id);
        p.reject(error);
        return true;
    }
    get pendingCount() { return this.pending.size; }
    rejectAllPending(e) { for (const p of this.pending.values())
        p.reject(e); this.pending.clear(); }
    /** 3 consecutive capture/IPC failures -> capture-paused, emitted once until a success. */
    recordFailure() {
        this.failures++;
        if (this.failures >= this.failureLimit && !this.capturePaused) {
            this.capturePaused = true;
            this.emit({ type: 'capture-paused', failures: this.failures });
        }
    }
    recordSuccess() {
        this.failures = 0;
        if (this.capturePaused) {
            this.capturePaused = false;
            this.emit({ type: 'capture-resumed' });
        }
    }
    get consecutiveFailures() { return this.failures; }
    disable() {
        this.disabled = true;
        this.clearHeartbeat();
        this.clearReconnect();
        this.rejectAllPending(new Error('disabled'));
        this.state = 'Disconnected';
    }
    clearHeartbeat() { if (this.heartbeatTimer !== null) {
        this.clock.clearTimeout(this.heartbeatTimer);
        this.heartbeatTimer = null;
    } }
    clearReconnect() { if (this.reconnectTimer !== null) {
        this.clock.clearTimeout(this.reconnectTimer);
        this.reconnectTimer = null;
    } }
}
/** Deterministic clock for tests. */
export class FakeClock {
    t = 0;
    next = 1;
    timers = new Map();
    now() { return this.t; }
    setTimeout(fn, ms) { const id = this.next++; this.timers.set(id, { at: this.t + ms, fn }); return id; }
    clearTimeout(h) { this.timers.delete(h); }
    advance(ms) {
        const end = this.t + ms;
        for (;;) {
            let best = null;
            for (const e of this.timers)
                if (e[1].at <= end && (!best || e[1].at < best[1].at))
                    best = e;
            if (!best)
                break;
            this.timers.delete(best[0]);
            this.t = best[1].at;
            best[1].fn();
        }
        this.t = end;
    }
    get pendingTimers() { return this.timers.size; }
}
