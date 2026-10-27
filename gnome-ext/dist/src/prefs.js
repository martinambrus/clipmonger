// Minimal Gtk4/Adw prefs: profile-dir + denylist fields.
// VERIFY (T-702): Adw PreferencesPage/EntryRow API on GNOME 46-51.
import Gtk from 'gi://Gtk?version=4.0';
import Adw from 'gi://Adw?version=1';
import { ExtensionPreferences } from 'resource:///org/gnome/shell/extensions/extension.js';
export default class ClipmongerPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        const page = new Adw.PreferencesPage();
        const group = new Adw.PreferencesGroup({ title: 'Clipmonger', description: 'History lives in the daemon; the extension only relays.' });
        const profileRow = new Adw.EntryRow({ title: 'Profile directory (blank = default)' });
        profileRow.set_text(settings.get_string('profile-dir') ?? '');
        profileRow.connect('changed', () => settings.set_string('profile-dir', profileRow.get_text()));
        const denylistRow = new Adw.EntryRow({ title: 'Denylist app ids (comma-separated)' });
        denylistRow.set_text((settings.get_strv('denylist') ?? []).join(', '));
        denylistRow.connect('changed', () => settings.set_strv('denylist', denylistRow.get_text().split(',').map((s) => s.trim()).filter(Boolean)));
        group.add(profileRow);
        group.add(denylistRow);
        page.add(group);
        window.add(page);
        void Gtk;
    }
}
