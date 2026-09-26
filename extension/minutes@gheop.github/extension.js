// The Minutes indicator: shows in the top bar while Minutes records, is
// paused or writes a transcript, with the time and a menu to pause or stop.
// It reads Minutes' state over D-Bus (the `status` action of the app) and
// stays hidden the rest of the time.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';
import Clutter from 'gi://Clutter';

import {Extension, gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {describe} from './status.js';

const BUS_NAME = 'io.github.gheop.Minutes';
const OBJECT_PATH = '/io/github/gheop/Minutes';
const DESKTOP_ID = 'io.github.gheop.Minutes.desktop';

const MinutesIndicator = GObject.registerClass(
class MinutesIndicator extends PanelMenu.Button {
    _init() {
        super._init(0.5, 'Minutes');

        const box = new St.BoxLayout();
        this._icon = new St.Icon({style_class: 'system-status-icon'});
        this._label = new St.Label({
            style_class: 'minutes-indicator-label',
            y_align: Clutter.ActorAlign.CENTER,
        });
        box.add_child(this._icon);
        box.add_child(this._label);
        this.add_child(box);

        this._pauseItem = this.menu.addAction(_('Pause'), () => this._activate('pause'));
        this._stopItem = this.menu.addAction(_('Stop'), () => this._activate('stop'));
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addAction(_('Open Minutes'), () => this._openApp());

        this._actions = null;
        this._actionsSignal = 0;
        this._addedSignal = 0;
        this._status = null;
        this._tick = 0;
        this._update();

        this._watch = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            connection => this._connect(connection),
            () => this._disconnect());
    }

    _connect(connection) {
        this._disconnect();
        this._actions = Gio.DBusActionGroup.get(connection, BUS_NAME, OBJECT_PATH);
        this._actionsSignal = this._actions.connect('action-state-changed', (_group, name, state) => {
            if (name === 'status')
                this._setStatus(state);
        });
        this._addedSignal = this._actions.connect('action-added', (_group, name) => {
            if (name === 'status')
                this._setStatus(this._actions.get_action_state('status'));
        });
        // The group describes its actions on first use; this asks for them.
        this._actions.list_actions();
        this._setStatus(this._actions.get_action_state('status'));
    }

    _disconnect() {
        if (this._actions) {
            this._actions.disconnect(this._actionsSignal);
            this._actions.disconnect(this._addedSignal);
            this._actions = null;
        }
        this._setStatus(null);
    }

    _setStatus(variant) {
        this._status = variant ? variant.deepUnpack() : null;
        this._update();
    }

    _update() {
        const look = describe(this._status, GLib.get_real_time() / 1e6);
        this.visible = look.visible;
        this._icon.icon_name = look.icon;
        this._label.text = look.label;
        if (look.recording)
            this._icon.add_style_class_name('minutes-indicator-recording');
        else
            this._icon.remove_style_class_name('minutes-indicator-recording');
        this._pauseItem.label.text = look.paused ? _('Resume') : _('Pause');
        this._pauseItem.visible = look.canPause;
        this._stopItem.visible = look.canStop;

        if (look.ticking && !this._tick) {
            this._tick = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
                this._update();
                return GLib.SOURCE_CONTINUE;
            });
        } else if (!look.ticking && this._tick) {
            GLib.source_remove(this._tick);
            this._tick = 0;
        }
    }

    _activate(name) {
        this._actions?.activate_action(name, null);
    }

    _openApp() {
        Shell.AppSystem.get_default().lookup_app(DESKTOP_ID)?.activate();
    }

    destroy() {
        if (this._tick) {
            GLib.source_remove(this._tick);
            this._tick = 0;
        }
        Gio.bus_unwatch_name(this._watch);
        this._disconnect();
        super.destroy();
    }
});

export default class MinutesExtension extends Extension {
    enable() {
        this._indicator = new MinutesIndicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator);
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
