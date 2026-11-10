// Spotty Focus: GNOME only lets Shell code raise another app's window, so
// Spotty asks this extension to focus an app that is already running.
// One method on the Shell's own bus connection, callable only by Spotty.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {DBusSenderChecker} from 'resource:///org/gnome/shell/misc/util.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const IFACE = `<node>
  <interface name="com.spotty.Focus">
    <method name="FocusApp">
      <arg type="s" direction="in" name="desktop_id"/>
      <arg type="b" direction="out" name="focused"/>
    </method>
  </interface>
</node>`;

export default class SpottyFocus extends Extension {
    enable() {
        this._checker = new DBusSenderChecker([
            'com.spotty.Spotty', // Flatpak and AppImage
            'com.spotty.Spotty.Launcher', // native packages
        ]);
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, '/com/spotty/Focus');
    }

    disable() {
        this._dbus?.unexport();
        this._dbus = null;
        this._checker?.destroy();
        this._checker = null;
    }

    // Raise the app's most recently used window; false if it has none.
    async FocusAppAsync([desktopId], invocation) {
        try {
            await this._checker.checkInvocation(invocation);
        } catch (e) {
            invocation.return_gerror(e);
            return;
        }
        const app = Shell.AppSystem.get_default().lookup_app(desktopId);
        const win = app?.get_windows()[0];
        if (win)
            Main.activateWindow(win);
        invocation.return_value(new GLib.Variant('(b)', [Boolean(win)]));
    }
}
