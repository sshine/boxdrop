import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const BUS_NAME = 'org.boxdrop.Daemon';
const OBJECT_PATH = '/org/boxdrop/Daemon';
const INTERFACE = 'org.boxdrop.Daemon';
const REFRESH_SECONDS = 1;

const ICONS = {
    resting: 'folder-download-symbolic',
    scanning: 'emblem-synchronizing-symbolic',
    syncing: 'emblem-synchronizing-symbolic',
    paused: 'media-playback-pause-symbolic',
    conflict: 'dialog-warning-symbolic',
    offline: 'network-offline-symbolic',
    error: 'dialog-error-symbolic',
};

export default class BoxdropExtension extends Extension {
    enable() {
        this._connection = Gio.DBus.session;
        this._status = null;
        this._timeoutId = null;

        this._indicator = new PanelMenu.Button(0.0, this.metadata.name, false);
        this._icon = new St.Icon({
            icon_name: ICONS.offline,
            style_class: 'system-status-icon',
        });
        this._indicator.add_child(this._icon);

        this._percent = new St.Label({
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'boxdrop-percent',
        });
        this._indicator.add_child(this._percent);

        this._buildMenu();
        Main.panel.addToStatusArea(this.uuid, this._indicator);
        this._refresh();
        this._timeoutId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            REFRESH_SECONDS,
            () => {
                this._refresh();
                return GLib.SOURCE_CONTINUE;
            }
        );
    }

    disable() {
        if (this._timeoutId !== null) {
            GLib.source_remove(this._timeoutId);
            this._timeoutId = null;
        }
        this._indicator?.destroy();
        this._indicator = null;
        this._icon = null;
        this._percent = null;
        this._status = null;
        this._connection = null;
    }

    _buildMenu() {
        const menu = this._indicator.menu;

        this._stateItem = new PopupMenu.PopupMenuItem('Boxdrop is not running', {
            reactive: false,
        });
        menu.addMenuItem(this._stateItem);

        const progressItem = new PopupMenu.PopupBaseMenuItem({
            reactive: false,
            can_focus: false,
        });
        this._progress = new St.ProgressBar({
            value: 0,
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        progressItem.add_child(this._progress);
        menu.addMenuItem(progressItem);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._pauseItem = new PopupMenu.PopupMenuItem('Pause');
        this._pauseItem.connect('activate', () => this._togglePause());
        menu.addMenuItem(this._pauseItem);

        this._syncItem = new PopupMenu.PopupMenuItem('Sync now');
        this._syncItem.connect('activate', () => this._call('SyncNow'));
        menu.addMenuItem(this._syncItem);

        this._conflictItem = new PopupMenu.PopupMenuItem('Conflicts: 0', {
            reactive: false,
        });
        menu.addMenuItem(this._conflictItem);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._recentItem = new PopupMenu.PopupMenuItem('Recently synced', {
            reactive: false,
        });
        menu.addMenuItem(this._recentItem);

        this._trafficItem = new PopupMenu.PopupMenuItem('Traffic: --', {
            reactive: false,
        });
        menu.addMenuItem(this._trafficItem);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._openItem = new PopupMenu.PopupMenuItem('Open folder');
        this._openItem.connect('activate', () => this._openFolder());
        menu.addMenuItem(this._openItem);
    }

    _call(method) {
        if (!this._connection)
            return;
        this._connection.call(
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            method,
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            null
        );
        this._refresh();
    }

    _togglePause() {
        if (this._status?.paused)
            this._call('Resume');
        else
            this._call('Pause');
    }

    _openFolder() {
        const root = this._status?.root;
        if (!root)
            return;
        const uri = Gio.File.new_for_path(root).get_uri();
        Gio.AppInfo.launch_default_for_uri(uri, null);
    }

    _refresh() {
        if (!this._connection || !this._indicator)
            return;
        this._connection.call(
            BUS_NAME,
            OBJECT_PATH,
            'org.freedesktop.DBus.Properties',
            'Get',
            new GLib.Variant('(ss)', [INTERFACE, 'StatusJson']),
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            (_connection, result) => {
                try {
                    const reply = this._connection.call_finish(result);
                    const [value] = reply.deepUnpack();
                    if (!this._indicator)
                        return;
                    const status = JSON.parse(value.deepUnpack());
                    this._applyStatus(status);
                } catch (_error) {
                    if (this._indicator)
                        this._applyOffline();
                }
            }
        );
    }

    _applyOffline() {
        this._status = null;
        this._icon.icon_name = ICONS.offline;
        this._percent.text = '';
        this._stateItem.label.text = 'Boxdrop is not running';
        this._progress.value = 0;
        this._pauseItem.label.text = 'Pause';
        this._pauseItem.sensitive = false;
        this._syncItem.sensitive = false;
        this._conflictItem.label.text = 'Conflicts: 0';
        this._recentItem.label.text = 'Recently synced: --';
        this._trafficItem.label.text = 'Traffic: --';
        this._openItem.sensitive = false;
    }

    _applyStatus(status) {
        this._status = status;
        const state = status.state ?? 'offline';
        const icon = state === 'resting' && status.conflicts > 0
            ? ICONS.conflict
            : (ICONS[state] ?? ICONS.offline);
        this._icon.icon_name = icon;
        this._percent.text = state === 'syncing' || state === 'scanning'
            ? `${status.progress ?? 0}%`
            : '';
        this._percent.visible = this._percent.text !== '';

        const label = state === 'syncing' || state === 'scanning'
            ? `Boxdrop is ${state} (${status.progress ?? 0}%)`
            : `Boxdrop is ${state}`;
        this._stateItem.label.text = label;
        this._progress.value = (status.progress ?? 0) / 100;
        this._pauseItem.label.text = status.paused ? 'Resume' : 'Pause';
        this._pauseItem.sensitive = true;
        this._syncItem.sensitive = !status.paused;
        this._conflictItem.label.text = `Conflicts: ${status.conflicts ?? 0}`;
        this._recentItem.label.text = this._recentText(status.recent ?? []);
        const up = formatBytes(status.bytes_up ?? 0);
        const down = formatBytes(status.bytes_down ?? 0);
        this._trafficItem.label.text = `Traffic: up ${up}, down ${down}`;
        this._openItem.sensitive = Boolean(status.root);
    }

    _recentText(recent) {
        if (!recent.length)
            return 'Recently synced: --';
        return recent
            .slice(0, 4)
            .map(item => {
                const when = formatAge(item.at_ms ?? 0);
                const sample = item.sample?.[0];
                return sample
                    ? `${when}: ${sample}`
                    : `${when}: ${item.paths ?? 0} paths`;
            })
            .join('\n');
    }
}

function formatBytes(value) {
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
    let amount = Number(value) || 0;
    let unit = 0;
    while (amount >= 1024 && unit < units.length - 1) {
        amount /= 1024;
        unit++;
    }
    return `${amount.toFixed(amount < 10 && unit > 0 ? 1 : 0)} ${units[unit]}`;
}

function formatAge(ms) {
    if (!ms)
        return 'just now';
    const seconds = Math.max(0, Math.floor((Date.now() - ms) / 1000));
    if (seconds < 60)
        return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60)
        return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    return `${hours}h ago`;
}
