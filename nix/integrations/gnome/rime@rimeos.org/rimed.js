// rimed.js — what Rime's GNOME Quick Settings show, read from rimed on the system bus.
//
// No GNOME Shell import here: this module is the whole D-Bus side of the extension, so it runs (and is tested) under
// plain gjs against a fake rimed on a private bus (nix/tests/settings-quicksettings-gnome.js). extension.js only draws
// what `view()` says and calls the two actions.
//
// The contract (docs/nixos/desktops.md §6.2), all on org.rimeos.Rimed1 at /org/rimeos/Rimed1:
//   org.rimeos.Rimed1.Mode      Active (s daily|gaming), Held (s)          Set(s)
//   org.rimeos.Rimed1.GameMode  Active (b)
//   org.rimeos.Rimed1.Updates   State, Available, Running, Message (s)     Check(), Apply()
// rimed emits org.freedesktop.DBus.Properties.PropertiesChanged for every one of these properties. The model follows
// that signal (and the bus name's owner, for a rimed restart) and never polls: nothing here runs on a timer.
//
// No power-profile control: GNOME's own Power Mode toggle drives rimed's power-profiles API (§7), and a second
// control would only disagree with it.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';

export const BUS_NAME = 'org.rimeos.Rimed1';
export const OBJECT_PATH = '/org/rimeos/Rimed1';

const IFACES = `<node>
  <interface name="org.rimeos.Rimed1.Mode">
    <property name="Active" type="s" access="read"/>
    <property name="Held" type="s" access="read"/>
    <method name="Set"><arg name="mode" type="s" direction="in"/></method>
  </interface>
  <interface name="org.rimeos.Rimed1.GameMode">
    <property name="Active" type="b" access="read"/>
  </interface>
  <interface name="org.rimeos.Rimed1.Updates">
    <property name="State" type="s" access="read"/>
    <property name="Available" type="s" access="read"/>
    <property name="Running" type="s" access="read"/>
    <property name="Message" type="s" access="read"/>
    <method name="Check"/>
    <method name="Apply"/>
  </interface>
</node>`;

const NODE = Gio.DBusNodeInfo.new_for_xml(IFACES);
const info = name => NODE.lookup_interface(name);

// Update states the item is shown in. Up to date, unknown, failed and deferred hide it: the Updates page in Rime
// Settings says why an update failed or waits; the panel is for an action the user can take now.
const UPDATE_SHOWN = ['available', 'downloading', 'installing', 'restart'];

export const RimedModel = GObject.registerClass({
    Signals: {'changed': {}},
}, class RimedModel extends GObject.Object {
    constructor(params = {}) {
        super();
        this._bus = params.bus ?? null;
        this._proxies = {};
        this._ids = [];
        this._cancellable = new Gio.Cancellable();
    }

    // Creates one proxy per interface. Each loads its properties once (GetAll) and from then on is told every change
    // by PropertiesChanged; `changed` is emitted for each.
    start() {
        const bus = this._bus ?? Gio.DBus.system;
        for (const [key, iface] of [['mode', 'org.rimeos.Rimed1.Mode'],
            ['game', 'org.rimeos.Rimed1.GameMode'],
            ['updates', 'org.rimeos.Rimed1.Updates']]) {
            Gio.DBusProxy.new(bus, Gio.DBusProxyFlags.NONE, info(iface), BUS_NAME, OBJECT_PATH, iface,
                this._cancellable, (_o, res) => {
                    let proxy;
                    try {
                        proxy = Gio.DBusProxy.new_finish(res);
                    } catch (e) {
                        if (!e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                            console.warn(`rime: no proxy for ${iface}: ${e.message}`);
                        return;
                    }
                    this._proxies[key] = proxy;
                    this._ids.push([proxy, proxy.connect('g-properties-changed', () => this.emit('changed'))]);
                    // rimed restarted (or started after the panel): the proxy reloads its cache on the new owner.
                    this._ids.push([proxy, proxy.connect('notify::g-name-owner', () => this.emit('changed'))]);
                    this.emit('changed');
                });
        }
    }

    destroy() {
        this._cancellable.cancel();
        for (const [proxy, id] of this._ids)
            proxy.disconnect(id);
        this._ids = [];
        this._proxies = {};
    }

    _get(key, prop, fallback) {
        const p = this._proxies[key];
        if (!p || !p.g_name_owner)
            return fallback;
        const v = p.get_cached_property(prop);
        return v ? v.deepUnpack() : fallback;
    }

    get rimedRunning() {
        return Boolean(this._proxies.mode?.g_name_owner);
    }

    get gaming() {
        return this._get('mode', 'Active', 'daily') === 'gaming';
    }

    get gameRunning() {
        return this._get('game', 'Active', false) === true;
    }

    get updateState() {
        return this._get('updates', 'State', 'unknown');
    }

    // What the panel shows: the toggles draw exactly this.
    view() {
        const state = this.updateState;
        const release = this._get('updates', 'Available', '');
        const message = this._get('updates', 'Message', '');
        let updateTitle = 'Update';
        let updateSubtitle = '';
        switch (state) {
        case 'available':
            updateTitle = 'Update';
            updateSubtitle = release !== '' ? `Rime ${release}` : 'Rime';
            break;
        case 'downloading':
        case 'installing':
            updateTitle = 'Updating';
            updateSubtitle = message !== '' ? message : (release !== '' ? `Rime ${release}` : '');
            break;
        case 'restart':
            updateTitle = 'Restart to finish';
            updateSubtitle = release !== '' ? `Rime ${release}` : '';
            break;
        }
        return {
            rimed: this.rimedRunning,
            gamingChecked: this.gaming,
            gamingSubtitle: this.gameRunning ? 'Game running' : '',
            updateVisible: this.rimedRunning && UPDATE_SHOWN.includes(state),
            updateSensitive: state === 'available' || state === 'restart',
            updateState: state,
            updateTitle,
            updateSubtitle,
        };
    }

    _call(key, method, params) {
        const p = this._proxies[key];
        if (!p)
            return Promise.reject(new Error('rimed is not running'));
        return new Promise((resolve, reject) => {
            // Interactive: rimed's polkit check may ask the desktop's agent (Updates.Apply is auth_admin_keep).
            p.call(method, params, Gio.DBusCallFlags.ALLOW_INTERACTIVE_AUTHORIZATION, -1, null, (_p, res) => {
                try {
                    resolve(p.call_finish(res));
                } catch (e) {
                    reject(e);
                }
            });
        });
    }

    // The Gaming toggle's click: Daily <-> Gaming through rimed's own composition (Mode.Set). The toggle does not
    // flip itself; it shows Mode.Active when rimed says it changed.
    toggleGaming() {
        return this._call('mode', 'Set', new GLib.Variant('(s)', [this.gaming ? 'daily' : 'gaming']));
    }

    // The Update item's click while an update is available.
    applyUpdate() {
        return this._call('updates', 'Apply', null);
    }
});
