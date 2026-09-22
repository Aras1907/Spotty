// Spotty GNOME Shell extension — global keybindings for Spotty trigger
// keywords, registered with Meta.Display.grab_accelerator().
// GNOME 45+ ES module format.
//
// Reads shortcuts from Spotty's config (~/.config/spotty/config.json, the
// main toggle shortcut + command keywords) and from installed trigger
// manifests (~/.config/spotty/triggers/*.json). When a keybinding fires,
// summons Spotty via the CLI — the CLI sends SIGUSR1 to the running daemon
// (fast path). The binary to run is written by Spotty to
// ~/.config/spotty/spotty_bin (absolute path, or the flatpak run command);
// falls back to `spotty` on PATH.

import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

const CONFIG_FILE = `${GLib.get_home_dir()}/.config/spotty/config.json`;
const TRIGGERS_DIR = `${GLib.get_home_dir()}/.config/spotty/triggers`;
const BIN_FILE = `${GLib.get_home_dir()}/.config/spotty/spotty_bin`;

const SPAWN_FLAGS = GLib.SpawnFlags.SEARCH_PATH |
    GLib.SpawnFlags.STDOUT_TO_DEV_NULL |
    GLib.SpawnFlags.STDERR_TO_DEV_NULL;

const decoder = new TextDecoder();

// Convert human-readable accelerator "Super+Ctrl+F" to GTK format
// "<Super><Control>f" — the only format mutter's accelerator parser accepts.
function toGtkAccel(str) {
    const parts = str.split('+').map(s => s.trim());
    const mods = [];
    let key = '';
    for (const p of parts) {
        const lower = p.toLowerCase();
        if (lower === 'super') mods.push('<Super>');
        else if (lower === 'ctrl' || lower === 'control') mods.push('<Control>');
        else if (lower === 'alt' || lower === 'meta') mods.push('<Alt>');
        else if (lower === 'shift') mods.push('<Shift>');
        else key = lower;
    }
    return mods.join('') + key;
}

export default class SpottyExtension extends Extension {
    constructor(metadata) {
        super(metadata);
        this._bindings = [];
        this._handlerId = 0;
    }

    enable() {
        this._grabAll();
    }

    disable() {
        if (this._handlerId) {
            global.display.disconnect(this._handlerId);
            this._handlerId = 0;
        }
        for (const b of this._bindings) {
            try {
                Main.wm.allowKeybinding(b.name, Shell.ActionMode.NONE);
            } catch (e) { /* already released */ }
            try {
                global.display.ungrab_accelerator(b.action);
            } catch (e) { /* already released */ }
        }
        this._bindings = [];
    }

    _grabAll() {
        const shortcuts = this._readShortcuts();
        const seen = new Set();
        for (const { keyword, accelerator } of shortcuts) {
            if (!accelerator || seen.has(accelerator))
                continue;
            seen.add(accelerator);
            const gtk = toGtkAccel(accelerator);
            const action = global.display.grab_accelerator(gtk, 0);
            if (action === 0) {
                log(`[spotty] failed to grab: ${accelerator} (parsed: ${gtk})`);
                continue;
            }
            // Without allowKeybinding the grabbed accelerator never fires.
            const name = Meta.external_binding_name_for_action(action);
            Main.wm.allowKeybinding(name, Shell.ActionMode.ALL);
            this._bindings.push({ action, name, keyword });
        }
        if (this._bindings.length > 0) {
            this._handlerId = global.display.connect('accelerator-activated',
                (_display, action) => {
                    const hit = this._bindings.find(b => b.action === action);
                    if (hit)
                        this._onActivated(hit.keyword);
                });
        }
    }

    _onActivated(keyword) {
        const argv = GLib.shell_parse_argv(this._readBin() || 'spotty')[1];
        argv.push(keyword ? `--keyword=${keyword}` : '--toggle');
        try {
            GLib.spawn_async(null, argv, null, SPAWN_FLAGS, null);
        } catch (e) {
            log(`[spotty] spawn failed: ${e.message}`);
        }
    }

    _readBin() {
        try {
            return decoder.decode(GLib.file_get_contents(BIN_FILE)[1]).trim();
        } catch (e) {
            return null;
        }
    }

    _readShortcuts() {
        const result = [];
        try {
            const config = JSON.parse(decoder.decode(
                GLib.file_get_contents(CONFIG_FILE)[1]));
            if (config.shortcut)
                result.push({ keyword: '', accelerator: config.shortcut });
            for (const kw of config.command_keywords || []) {
                if (kw.shortcut && kw.enabled !== false)
                    result.push({ keyword: kw.id, accelerator: kw.shortcut });
            }
        } catch (e) {
            log(`[spotty] config read failed: ${e.message}`);
        }
        try {
            const dir = GLib.Dir.open(TRIGGERS_DIR, 0);
            let name;
            while ((name = dir.read_name()) !== null) {
                if (!name.endsWith('.json'))
                    continue;
                try {
                    const m = JSON.parse(decoder.decode(
                        GLib.file_get_contents(`${TRIGGERS_DIR}/${name}`)[1]));
                    if (m.shortcut && m.enabled !== false)
                        result.push({ keyword: m.id, accelerator: m.shortcut });
                } catch (e) { /* skip unreadable manifest */ }
            }
            dir.close();
        } catch (e) { /* no triggers dir yet */ }
        return result;
    }
}
