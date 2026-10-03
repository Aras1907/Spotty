// Spotty GNOME Shell extension — positions the Spotty launcher window.
//
// Global shortcuts are handled by GNOME custom keybindings (see
// src/keybindings.rs); this extension only does what a Wayland application
// cannot do for itself: move its own window. When a Spotty window appears it
// is placed horizontally centered, with its top edge at 25% of the work area
// (75% of the screen height below it), on the monitor holding the pointer —
// and the overview is closed first, so a summon from there doesn't strand
// the window in the overview's own layout slot.
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

// Spotty writes its daemon pid here — the most reliable way to recognise its
// window (wm_class/title are populated slightly later than creation).
const PID_FILE = `${GLib.get_home_dir()}/.config/spotty/spotty.pid`;
const decoder = new TextDecoder();

export default class SpottyExtension extends Extension {
    constructor(metadata) {
        super(metadata);
        this._placementId = 0;
    }

    enable() {
        this._placementId = global.display.connect('window-created',
            (_display, win) => {
                // class/pid/title may not be populated at creation time —
                // resolve once idle, then pin the position.
                Meta.later_add(Meta.LaterType.IDLE, () => {
                    try {
                        if (this._isSpotty(win))
                            this._trackPlacement(win);
                    } catch (e) { /* window already gone */ }
                    return false; // GLib.SOURCE_REMOVE
                });
            });
    }

    disable() {
        if (this._placementId) {
            global.display.disconnect(this._placementId);
            this._placementId = 0;
        }
    }

    _trackPlacement(win) {
        // Summoning from the overview hands the window to the overview's
        // layout — close it so Spotty maps as a normal toplevel.
        try {
            if (Main.overview.visible)
                Main.overview.hide();
        } catch (e) { /* overview unavailable */ }
        this._placeSpotty(win);
        // The window starts bar-only and grows when results appear; the
        // formula is height-independent, so re-applying keeps the top pinned
        // and corrects any re-placement Mutter did while mapping.
        try {
            win.connect('size-changed', () => this._placeSpotty(win));
        } catch (e) { /* signal unavailable */ }
    }

    _isSpotty(win) {
        try {
            const cls = (win.get_wm_class() || '').toLowerCase();
            if (cls === 'spotty' || cls === 'com.spotty.spotty')
                return true;
            if ((win.get_title() || '') === 'Spotty')
                return true;
        } catch (e) { /* fall through to the pid check */ }
        try {
            const pid = parseInt(decoder.decode(
                GLib.file_get_contents(PID_FILE)[1]).trim(), 10);
            return Number.isInteger(pid) && pid > 0 && win.get_pid() === pid;
        } catch (e) {
            return false;
        }
    }

    _placeSpotty(win) {
        try {
            const r = win.get_frame_rect();
            if (r.width <= 0 || r.height <= 0)
                return;
            const [px, py] = global.get_pointer();
            let monitor = Main.layoutManager.primaryIndex;
            const monitors = Main.layoutManager.monitors;
            for (let i = 0; i < monitors.length; i++) {
                const m = monitors[i];
                if (px >= m.x && px < m.x + m.width &&
                    py >= m.y && py < m.y + m.height) {
                    monitor = i;
                    break;
                }
            }
            const area = Main.layoutManager.getWorkAreaForMonitor(monitor);
            const x = area.x + Math.round((area.width - r.width) / 2);
            const y = area.y + Math.round(area.height * 0.25);
            win.move_frame(true, x, y);
            if (!win._spottyPos || win._spottyPos[0] !== x ||
                win._spottyPos[1] !== y) {
                win._spottyPos = [x, y];
                log(`[spotty] placed at ${x},${y} (${r.width}x${r.height}) monitor ${monitor}`);
            }
        } catch (e) {
            log(`[spotty] place failed: ${e.message}`);
        }
    }
}
