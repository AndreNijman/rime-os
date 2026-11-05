// Rime in GNOME's Quick Settings (docs/nixos/desktops.md §6, D10).
//
//   Gaming       on/off = rimed Mode.Set('gaming'|'daily'); subtitle "Game running" while GameMode.Active. Its menu
//                opens Rime Settings at Gaming, or at its first page.
//   Update       shown while rimed's Updates.State is available (click: Updates.Apply, rimed asks for an
//                administrator through GNOME's polkit agent), downloading/installing (progress, not clickable) or
//                restart (click: GNOME's own restart dialog).
//
// Everything is read from rimed on the system bus by rimed.js, which follows PropertiesChanged; this file only draws.
// The power profile is not here: GNOME's own Power Mode toggle already drives rimed (rimed serves the
// power-profiles-daemon API).
//
// Test hook: when a process owns org.rimeos.QuickSettingsTest on the session bus (only the VM tests run one), every
// redraw is reported to it as JSON (the toggles' own checked/subtitle/visible). Nothing is sent otherwise: the name is
// watched, not polled.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {QuickMenuToggle, QuickToggle, SystemIndicator} from 'resource:///org/gnome/shell/ui/quickSettings.js';

import {RimedModel} from './rimed.js';

const SETTINGS_APP = 'org.rimeos.Settings.desktop';
const TEST_NAME = 'org.rimeos.QuickSettingsTest';
const TEST_PATH = '/org/rimeos/QuickSettingsTest';

function openSettings(page) {
    // The .desktop entry's Exec is `rime-settings %u`; a rime-settings:<page> URI selects the page.
    const info = Gio.DesktopAppInfo.new(SETTINGS_APP);
    if (!info) {
        console.warn('rime: org.rimeos.Settings.desktop is not installed');
        return;
    }
    const ctx = global.create_app_launch_context(0, -1);
    try {
        if (page)
            info.launch_uris([`rime-settings:${page}`], ctx);
        else
            Shell.AppSystem.get_default().lookup_app(SETTINGS_APP)?.activate() ?? info.launch([], ctx);
    } catch (e) {
        console.warn(`rime: could not open Rime Settings: ${e.message}`);
    }
}

const GamingToggle = GObject.registerClass(
class RimeGamingToggle extends QuickMenuToggle {
    _init(model) {
        super._init({
            title: 'Gaming',
            iconName: 'input-gaming-symbolic',
            // The toggle shows rimed's Mode.Active; a click asks rimed and the change comes back as a property.
            toggleMode: false,
            menuButtonAccessibleName: 'Open Rime menu',
        });
        this._model = model;
        this.connect('clicked', () => this.activateGaming());

        this.menu.setHeader('input-gaming-symbolic', 'Rime');
        this.menu.addAction('Gaming settings', () => openSettings('gaming'));
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addAction('Open Rime Settings', () => openSettings(''));
    }

    // The click handler (also what a keyboard activation runs).
    activateGaming() {
        this._model.toggleGaming().catch(e => console.warn(`rime: Mode.Set refused: ${e.message}`));
    }
});

const UpdateToggle = GObject.registerClass(
class RimeUpdateToggle extends QuickToggle {
    _init(model) {
        super._init({
            title: 'Update',
            iconName: 'software-update-available-symbolic',
            toggleMode: false,
            visible: false,
        });
        this._model = model;
        this.connect('clicked', () => this.activateUpdate());
    }

    activateUpdate() {
        const state = this._model.updateState;
        if (state === 'available') {
            this._model.applyUpdate().catch(e => console.warn(`rime: Updates.Apply refused: ${e.message}`));
        } else if (state === 'restart') {
            // GNOME's own restart dialog (it asks, counts down and lists what is still open).
            Gio.DBus.session.call('org.gnome.SessionManager', '/org/gnome/SessionManager',
                'org.gnome.SessionManager', 'Reboot', null, null, Gio.DBusCallFlags.NONE, -1, null, null);
        }
    }
});

const RimeIndicator = GObject.registerClass(
class RimeIndicator extends SystemIndicator {
    _init() {
        super._init();

        // A panel icon while Gaming is on, as GNOME shows one for Night Light or Do Not Disturb.
        this._icon = this._addIndicator();
        this._icon.iconName = 'input-gaming-symbolic';

        this._model = new RimedModel();
        this._gaming = new GamingToggle(this._model);
        this._update = new UpdateToggle(this._model);
        this.quickSettingsItems.push(this._gaming, this._update);

        this._model.connect('changed', () => this._sync());

        this._testOwner = null;
        this._testWatch = Gio.bus_watch_name(Gio.BusType.SESSION, TEST_NAME, Gio.BusNameWatcherFlags.NONE,
            (_c, _n, owner) => {
                this._testOwner = owner;
                this._report();
            },
            () => {
                this._testOwner = null;
            });

        this._model.start();
        this._sync();
    }

    _sync() {
        const v = this._model.view();
        this._gaming.checked = v.gamingChecked;
        this._gaming.subtitle = v.gamingSubtitle;
        this._gaming.reactive = v.rimed;
        this._icon.visible = v.gamingChecked;

        this._update.visible = v.updateVisible;
        this._update.reactive = v.updateSensitive;
        this._update.title = v.updateTitle;
        this._update.subtitle = v.updateSubtitle;
        this._update.iconName = v.updateState === 'restart' ? 'system-reboot-symbolic' : 'software-update-available-symbolic';
        this._report();
    }

    // What is drawn, read back from the widgets themselves.
    _report() {
        if (!this._testOwner)
            return;
        const state = {
            gaming: {checked: this._gaming.checked, subtitle: this._gaming.subtitle ?? '', reactive: this._gaming.reactive},
            update: {visible: this._update.visible, title: this._update.title ?? '', subtitle: this._update.subtitle ?? ''},
            indicator: this._icon.visible,
        };
        Gio.DBus.session.call(TEST_NAME, TEST_PATH, TEST_NAME, 'Report',
            new GLib.Variant('(ss)', ['gnome', JSON.stringify(state)]), null, Gio.DBusCallFlags.NO_AUTO_START, -1,
            null, null);
    }

    destroy() {
        Gio.bus_unwatch_name(this._testWatch);
        this._model.destroy();
        this.quickSettingsItems.forEach(item => item.destroy());
        super.destroy();
    }
});

export default class RimeExtension extends Extension {
    enable() {
        this._indicator = new RimeIndicator();
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
