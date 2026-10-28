// Shell glue: paste-assist v1 = write clipboard + toast. No key injection.
import St from 'gi://St';
import { notify } from 'resource:///org/gnome/shell/ui/main.js';
export class PasteAssist {
    capture;
    constructor(capture = null) { this.capture = capture; }
    /** Write text, suppress our own poll echo, and toast (Ctrl+V is the user's). */
    pasteText(text) {
        // VERIFY (T-702): St.Clipboard.set_text selection-type argument on 46-51.
        St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text);
        this.capture?.markSelfWrite([{ mime: 'text/plain', bytes: new TextEncoder().encode(text) }]);
        notify('Clipmonger', 'Copied — press Ctrl+V');
    }
    pasteBytes(mime, bytes) {
        // VERIFY (T-702): set_content / MetaSelection write shape for non-text mimes.
        const clipboard = St.Clipboard.get_default();
        if (typeof clipboard.set_content === 'function')
            clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, bytes);
        else {
            notify('Clipmonger', 'Copied — press Ctrl+V');
            return;
        }
        this.capture?.markSelfWrite([{ mime, bytes }]);
        notify('Clipmonger', 'Copied — press Ctrl+V');
    }
}
