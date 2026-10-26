// Enable/disable wiring only. All logic lives in the pure modules + glue.
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import { notify } from 'resource:///org/gnome/shell/ui/main.js';
import { IpcClient } from './ipc.js';
import { CaptureRelay } from './capture.js';
import { PollDiff } from './polddiff.js';
import { Hotkeys } from './hotkeys.js';
import { PasteAssist } from './paste.js';
import { mergeConfig } from './config.js';
export default class ClipmongerExtension extends Extension {
    ipc = null;
    capture = null;
    hotkeys = null;
    paste = null;
    enable() {
        const settings = this.getSettings();
        const profileDir = settings.get_string('profile-dir') ?? '';
        const prefDenylist = settings.get_strv('denylist') ?? [];
        this.ipc = new IpcClient(profileDir);
        const diff = new PollDiff({ denylist: prefDenylist });
        this.capture = new CaptureRelay(this.ipc, diff);
        this.paste = new PasteAssist(this.capture);
        this.hotkeys = new Hotkeys(settings, this.ipc);
        const hotkeys = this.hotkeys;
        this.ipc.onConfig = (result) => {
            const cfg = mergeConfig(result);
            diff.denylist = [...cfg.privacy.denylist, ...prefDenylist, ...cfg.monitor.ignores];
            diff.maxBytes = cfg.monitor.max_bytes;
            this.capture?.setPollMs(cfg.monitor.poll_ms);
            hotkeys.registerPopup(cfg.popup.hotkey);
        };
        // Pre-config fallback so the popup chord exists before the first resync.
        this.hotkeys.registerPopup('ctrl+alt+v');
        this.ipc.start();
        this.capture.start();
    }
    disable() {
        this.capture?.stop();
        this.hotkeys?.unregisterAll();
        this.ipc?.disable();
        this.capture = null;
        this.hotkeys = null;
        this.ipc = null;
        this.paste = null;
    }
}
void notify;
