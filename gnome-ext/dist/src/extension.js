// Enable/disable wiring only. All logic lives in the pure modules + glue.
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import { notify } from 'resource:///org/gnome/shell/ui/main.js';
import { IpcClient } from './ipc.js';
import { CaptureRelay } from './capture.js';
import { PollDiff } from './polddiff.js';
import { Hotkeys } from './hotkeys.js';
import { PasteAssist } from './paste.js';
import { PopupPlacer } from './placer.js';
import { DEFAULT_POPUP_HOTKEY, mergeConfig } from './config.js';
import { pasteAssistChunkRequest, pasteAssistDoneRequest, permanentListRequest, useNativeBase64 } from './codec.js';
import GLib from 'gi://GLib?version=2.0';
import { PermanentHotkeys } from './permhotkeys.js';
import { parsePermanents } from './permbind.js';
export default class ClipmongerExtension extends Extension {
    ipc = null;
    capture = null;
    hotkeys = null;
    placer = null;
    permanents = null;
    paste = null;
    enable() {
        useNativeBase64((bytes) => GLib.base64_encode(bytes), (text) => GLib.base64_decode(text));
        const settings = this.getSettings();
        const profileDir = settings.get_string('profile-dir') ?? '';
        const prefDenylist = settings.get_strv('denylist') ?? [];
        this.ipc = new IpcClient(profileDir);
        const diff = new PollDiff({ denylist: prefDenylist });
        this.capture = new CaptureRelay(this.ipc, diff);
        this.paste = new PasteAssist(this.capture);
        this.hotkeys = new Hotkeys(settings, this.ipc);
        const hotkeys = this.hotkeys;
        const paste = this.paste;
        const ipc = this.ipc;
        paste.enable();
        const permanents = new PermanentHotkeys(ipc, paste);
        this.permanents = permanents;
        permanents.enable();
        const loadPermanents = () => {
            if (!permanents.active)
                return;
            void ipc.request(permanentListRequest(ipc.nextId()))
                .then((result) => permanents.sync(parsePermanents(result)))
                .catch(() => { });
        };
        // GNOME Wayland popup paste: the daemon pushes `paste.assist`; per-clip
        // hotkeys follow `permanents.changed`.
        ipc.onEvent = (env) => {
            if (env.method === 'permanents.changed') {
                loadPermanents();
                return;
            }
            if (env.method !== 'paste.assist')
                return;
            const fetchChunk = async (assist, seq) => {
                const result = await ipc.request(pasteAssistChunkRequest(ipc.nextId(), assist, seq));
                if (typeof result?.data_b64 !== 'string')
                    throw new Error('no chunk data');
                return result.data_b64;
            };
            void paste.assist(env.params, fetchChunk)
                .then((done) => ipc.request(pasteAssistDoneRequest(ipc.nextId(), done)))
                .catch(() => { });
        };
        const placer = new PopupPlacer();
        this.placer = placer;
        this.ipc.onConfig = (result) => {
            const cfg = mergeConfig(result);
            diff.denylist = [...cfg.privacy.denylist, ...prefDenylist, ...cfg.monitor.ignores];
            diff.maxBytes = cfg.monitor.max_bytes;
            diff.formats = cfg.monitor.formats;
            diff.strict = cfg.privacy.strict;
            this.capture?.setPollMs(cfg.monitor.poll_ms);
            hotkeys.registerPopup(cfg.popup.hotkey);
            placer.placement = cfg.popup.placement;
            loadPermanents();
        };
        // Pre-config fallback so the popup chord exists before the first resync.
        this.hotkeys.registerPopup(DEFAULT_POPUP_HOTKEY);
        this.ipc.start();
        this.capture.start();
        placer.enable();
    }
    disable() {
        this.placer?.disable();
        this.permanents?.disable();
        this.paste?.disable();
        this.capture?.stop();
        this.hotkeys?.unregisterAll();
        this.ipc?.disable();
        this.capture = null;
        this.hotkeys = null;
        this.placer = null;
        this.permanents = null;
        this.ipc = null;
        this.paste = null;
    }
}
void notify;
