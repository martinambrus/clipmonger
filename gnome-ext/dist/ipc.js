// Shell glue: Gio Unix-socket client. Async only — never blocks the Shell main loop.
// VERIFY (T-702): Gio.SocketClient/DataInputStream async shapes on GNOME 46-51.
import Gio from 'gi://Gio-2.0';
import GLib from 'gi://GLib-2.0';
import { decodeEnvelope, encodeEnvelope, createIdGen, helloRequest, pingRequest, configGetRequest, eventsSubscribeRequest, } from './codec.js';
import { Relay } from './relay.js';
export const systemClock = {
    now: () => GLib.get_monotonic_time() / 1000,
    setTimeout: (fn, ms) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => { fn(); return GLib.SOURCE_REMOVE; }),
    clearTimeout: (h) => { if (h)
        GLib.source_remove(h); },
};
function readTextFile(path) {
    try {
        const [ok, bytes] = Gio.File.new_for_path(path).load_contents(null);
        if (!ok)
            return null;
        return new TextDecoder().decode(bytes);
    }
    catch {
        return null;
    }
}
/** G-F1: default profile = data_dir()/clipmonger; pref `profile-dir` overrides. Re-resolved on every connect. */
export function discoverDaemon(profileDirPref) {
    const profile = profileDirPref || `${GLib.get_user_data_dir()}/clipmonger`;
    const rendezvousRaw = readTextFile(`${profile}/rendezvous.json`);
    const tokenRaw = readTextFile(`${profile}/token`);
    if (!rendezvousRaw || tokenRaw === null)
        return null;
    try {
        const rendezvous = JSON.parse(rendezvousRaw);
        if (!rendezvous.socket)
            return null;
        return { socket: rendezvous.socket, token: tokenRaw.trim() };
    }
    catch {
        return null;
    }
}
export class IpcClient {
    relay;
    nextId = createIdGen('e');
    onConfig = () => { };
    onEvent = () => { };
    conn = null;
    out = null;
    input = null;
    profileDirPref;
    constructor(profileDirPref = '') {
        this.profileDirPref = profileDirPref;
        this.relay = new Relay({
            clock: systemClock,
            onIntent: (intent) => {
                if (intent.type === 'connect')
                    void this.connect();
                else if (intent.type === 'close')
                    this.closeSocket();
                else if (intent.type === 'send-hello')
                    void this.sendHello();
                else if (intent.type === 'send' && intent.method === 'ping')
                    void this.request(pingRequest(this.nextId())).then(() => this.relay.handlePong()).catch(() => { });
                else if (intent.type === 'send' && intent.method === 'events.subscribe')
                    void this.request(eventsSubscribeRequest(this.nextId())).catch(() => { });
                else if (intent.type === 'send' && intent.method === 'config.get')
                    void this.request(configGetRequest(this.nextId())).then((r) => this.onConfig(r)).catch(() => { });
            },
        });
    }
    start() { this.relay.start(); }
    async connect() {
        const found = discoverDaemon(this.profileDirPref);
        if (!found) {
            this.relay.handleDisconnect();
            return;
        }
        try {
            const client = new Gio.SocketClient();
            const address = Gio.UnixSocketAddress.new(found.socket);
            this.conn = await new Promise((resolve, reject) => client.connect_async(address, null, (c, res) => {
                try {
                    resolve(c.connect_finish(res));
                }
                catch (e) {
                    reject(e);
                }
            }));
            this.out = Gio.DataOutputStream.new(this.conn.get_output_stream());
            this.input = Gio.DataInputStream.new(this.conn.get_input_stream());
            this._token = found.token;
            this.relay.handleConnected();
            void this.readLoop();
        }
        catch {
            this.relay.handleDisconnect();
        }
    }
    async sendHello() {
        try {
            const result = await this.request(helloRequest(this.nextId(), this._token ?? ''));
            void result;
            this.relay.handleHelloOk();
        }
        catch (e) {
            const code = e instanceof Error ? e.message : 'auth_failed';
            this.relay.handleHelloError(code.includes('auth_failed') ? 'auth_failed' : code);
        }
    }
    request(env) {
        const promise = this.relay.registerPending(env.id);
        try {
            this.writeLine(encodeEnvelope(env));
        }
        catch (e) {
            this.relay.rejectResponse(env.id, e);
        }
        return promise;
    }
    notify(env) { try {
        this.writeLine(encodeEnvelope(env));
    }
    catch { /* relay records failure upstream */ } }
    writeLine(line) {
        if (!this.out)
            throw new Error('not connected');
        this.out.put_string(`${line}\n`, null);
    }
    async readLoop() {
        try {
            for (;;) {
                const line = await new Promise((resolve, reject) => this.input.read_line_async(GLib.PRIORITY_DEFAULT, null, (s, res) => {
                    try {
                        const [text] = s.read_line_finish_utf8(res);
                        resolve(text);
                    }
                    catch (e) {
                        reject(e);
                    }
                }));
                if (line === null)
                    break;
                let env;
                try {
                    env = decodeEnvelope(line);
                }
                catch {
                    continue;
                }
                if (env.kind === 'res') {
                    if (env.error)
                        this.relay.rejectResponse(env.id, new Error(env.error.code));
                    else
                        this.relay.resolveResponse(env.id, env.result);
                }
                else if (env.kind === 'event')
                    this.onEvent(env);
            }
        }
        catch { /* fall through to disconnect */ }
        this.relay.handleDisconnect();
    }
    closeSocket() { try {
        this.conn?.close(null);
    }
    catch { /* already closed */ } this.conn = null; this.out = null; this.input = null; }
    disable() { this.relay.disable(); this.closeSocket(); }
}
