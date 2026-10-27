// Shell glue: Gio Unix-socket client. Async only — never blocks the Shell main loop.
// VERIFY (T-702): Gio.SocketClient/DataInputStream async shapes on GNOME 46-51.
import Gio from 'gi://Gio?version=2.0';
import GLib from 'gi://GLib?version=2.0';
import {
  decodeEnvelope, encodeEnvelope, createIdGen, helloRequest, pingRequest,
  configGetRequest, eventsSubscribeRequest, type Envelope,
} from './codec.js';
import { Relay, type Clock } from './relay.js';

export const systemClock: Clock = {
  now: () => GLib.get_monotonic_time() / 1000,
  setTimeout: (fn, ms) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => { fn(); return GLib.SOURCE_REMOVE; }),
  clearTimeout: (h) => { if (h) GLib.source_remove(h as number); },
};

function readTextFile(path: string): string | null {
  try {
    const [ok, bytes] = Gio.File.new_for_path(path).load_contents(null);
    if (!ok) return null;
    return new TextDecoder().decode(bytes as Uint8Array);
  } catch { return null; }
}

/** G-F1: default profile = data_dir()/clipmonger; pref `profile-dir` overrides. Re-resolved on every connect. */
export function discoverDaemon(profileDirPref: string): { socket: string; token: string } | null {
  const profile = profileDirPref || `${GLib.get_user_data_dir()}/clipmonger`;
  const rendezvousRaw = readTextFile(`${profile}/rendezvous.json`);
  const tokenRaw = readTextFile(`${profile}/token`);
  if (!rendezvousRaw || tokenRaw === null) return null;
  try {
    const rendezvous = JSON.parse(rendezvousRaw) as { socket?: string };
    if (!rendezvous.socket) return null;
    return { socket: rendezvous.socket, token: tokenRaw.trim() };
  } catch { return null; }
}

export class IpcClient {
  readonly relay: Relay;
  readonly nextId = createIdGen('e');
  onConfig: (result: unknown) => void = () => {};
  onEvent: (env: Envelope) => void = () => {};
  private conn: any = null;
  private out: any = null;
  private input: any = null;
  private profileDirPref: string;

  constructor(profileDirPref = '') {
    this.profileDirPref = profileDirPref;
    this.relay = new Relay({
      clock: systemClock,
      onIntent: (intent) => {
        if (intent.type === 'connect') void this.connect();
        else if (intent.type === 'close') this.closeSocket();
        else if (intent.type === 'send-hello') void this.sendHello();
        else if (intent.type === 'send' && intent.method === 'ping') void this.request(pingRequest(this.nextId())).then(() => this.relay.handlePong()).catch(() => {});
        else if (intent.type === 'send' && intent.method === 'events.subscribe') void this.request(eventsSubscribeRequest(this.nextId())).catch(() => {});
        else if (intent.type === 'send' && intent.method === 'config.get') void this.request(configGetRequest(this.nextId())).then((r) => this.onConfig(r)).catch(() => {});
      },
    });
  }
  start(): void { this.relay.start(); }
  private async connect(): Promise<void> {
    const found = discoverDaemon(this.profileDirPref);
    if (!found) { this.relay.handleDisconnect(); return; }
    try {
      const client = new Gio.SocketClient();
      const address = Gio.UnixSocketAddress.new(found.socket);
      this.conn = await new Promise((resolve, reject) =>
        client.connect_async(address, null, (c: any, res: any) => {
          try { resolve(c.connect_finish(res)); } catch (e) { reject(e); }
        }));
      this.out = Gio.DataOutputStream.new(this.conn.get_output_stream());
      this.input = Gio.DataInputStream.new(this.conn.get_input_stream());
      (this as any)._token = found.token;
      this.relay.handleConnected();
      void this.readLoop();
    } catch { this.relay.handleDisconnect(); }
  }
  private async sendHello(): Promise<void> {
    try {
      const result = await this.request(helloRequest(this.nextId(), (this as any)._token ?? ''));
      void result;
      this.relay.handleHelloOk();
    } catch (e) {
      const code = e instanceof Error ? e.message : 'auth_failed';
      this.relay.handleHelloError(code.includes('auth_failed') ? 'auth_failed' : code);
    }
  }
  request(env: Envelope): Promise<unknown> {
    const promise = this.relay.registerPending(env.id);
    try { this.writeLine(encodeEnvelope(env)); } catch (e) { this.relay.rejectResponse(env.id, e as Error); }
    return promise;
  }
  notify(env: Envelope): void { try { this.writeLine(encodeEnvelope(env)); } catch { /* relay records failure upstream */ } }
  private writeLine(line: string): void {
    if (!this.out) throw new Error('not connected');
    this.out.put_string(`${line}\n`, null);
  }
  private async readLoop(): Promise<void> {
    try {
      for (;;) {
        const line: string | null = await new Promise((resolve, reject) =>
          this.input.read_line_async(GLib.PRIORITY_DEFAULT, null, (s: any, res: any) => {
            try { const [text] = s.read_line_finish_utf8(res); resolve(text); } catch (e) { reject(e); }
          }));
        if (line === null) break;
        let env: Envelope;
        try { env = decodeEnvelope(line); } catch { continue; }
        if (env.kind === 'res') {
          if (env.error) this.relay.rejectResponse(env.id, new Error(env.error.code));
          else this.relay.resolveResponse(env.id, env.result);
        } else if (env.kind === 'event') this.onEvent(env);
      }
    } catch { /* fall through to disconnect */ }
    this.relay.handleDisconnect();
  }
  private closeSocket(): void { try { this.conn?.close(null); } catch { /* already closed */ } this.conn = null; this.out = null; this.input = null; }
  disable(): void { this.relay.disable(); this.closeSocket(); }
}
