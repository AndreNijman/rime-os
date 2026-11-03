/*
    RimedModel.qml — what Rime's Plasma applet shows, read from rimed on the system bus.

    The whole D-Bus side of the applet: main.qml only draws what this says and calls its two actions, and
    nix/tests/settings-quicksettings-kde.qml runs this file alone against a fake rimed on a private bus.

    The contract (docs/nixos/desktops.md §6.2), all on org.rimeos.Rimed1 at /org/rimeos/Rimed1:
      org.rimeos.Rimed1.Mode      Active (s daily|gaming), Held (s)          Set(s)
      org.rimeos.Rimed1.GameMode  Active (b)
      org.rimeos.Rimed1.Updates   State, Available, Running, Message (s)     Check(), Apply()

    Each interface is read once with GetAll when rimed appears on the bus (and again if it restarts), and from then on
    only org.freedesktop.DBus.Properties.PropertiesChanged updates it. Nothing here runs on a timer.

    No power-profile control: Plasma's own power profile slider drives rimed's power-profiles API (§7).
*/
pragma ComponentBehavior: Bound

import QtQuick
import org.kde.plasma.workspace.dbus as DBus

QtObject {
    id: model

    readonly property string service: "org.rimeos.Rimed1"
    readonly property string path: "/org/rimeos/Rimed1"
    readonly property var ifaces: ({
        "org.rimeos.Rimed1.Mode": "mode",
        "org.rimeos.Rimed1.GameMode": "game",
        "org.rimeos.Rimed1.Updates": "updates",
    })

    // The property caches, one object per interface (replaced, never mutated, so bindings see each change).
    property var mode: ({})
    property var game: ({})
    property var updates: ({})

    // Emitted after every change the applet would redraw for (the test hook and the unit test listen).
    signal redrawn()

    readonly property bool rimed: rimedWatcher.registered
    // String(): the plugin may hand a D-Bus value back as a typed wrapper (DBus.bool, ...), whose String() is the value.
    readonly property bool gaming: rimed && String(mode.Active) === "gaming"
    readonly property bool gameRunning: rimed && String(game.Active) === "true"
    readonly property string gamingSubtitle: gameRunning ? "Game running" : ""
    readonly property string updateState: rimed && updates.State !== undefined ? String(updates.State) : "unknown"
    readonly property string release: updates.Available !== undefined ? String(updates.Available) : ""
    readonly property string message: updates.Message !== undefined ? String(updates.Message) : ""
    // Shown for an action the user can take now, or one under way. Up to date, unknown, failed and deferred hide it:
    // Rime Settings' Updates page says why an update failed or waits.
    readonly property bool updateVisible: rimed && ["available", "downloading", "installing", "restart"].includes(updateState)
    readonly property bool updateSensitive: updateState === "available" || updateState === "restart"
    readonly property string updateTitle: updateState === "available" ? (release !== "" ? "Update to " + release : "Update")
        : (updateState === "downloading" || updateState === "installing") ? "Updating"
        : updateState === "restart" ? "Restart to finish" : "Update"
    readonly property string updateSubtitle: (updateState === "downloading" || updateState === "installing")
        ? (message !== "" ? message : release) : (updateState === "restart" ? release : "")

    // What the applet draws, as one object (the test hook sends it as JSON).
    function view() {
        return {
            rimed: rimed,
            gamingChecked: gaming,
            gamingSubtitle: gamingSubtitle,
            updateVisible: updateVisible,
            updateSensitive: updateSensitive,
            updateState: updateState,
            updateTitle: updateTitle,
            updateSubtitle: updateSubtitle,
        };
    }

    function _merge(iface, changed) {
        const key = ifaces[iface];
        if (key === undefined)
            return;
        const next = Object.assign({}, model[key]);
        for (const k in changed)
            next[k] = changed[k];
        model[key] = next;
        model.redrawn();
    }

    function reload() {
        if (!rimedWatcher.registered) {
            mode = {};
            game = {};
            updates = {};
            model.redrawn();
            return;
        }
        for (const iface in ifaces) {
            DBus.SystemBus.asyncCall({
                service: service,
                path: path,
                iface: "org.freedesktop.DBus.Properties",
                member: "GetAll",
                // The plugin encodes `arguments` as ONE value of `signature`: several arguments are a struct.
                signature: "(s)",
                arguments: [iface],
            }, reply => model._merge(iface, reply.value), reply => console.warn("rime: GetAll", iface, "failed:", reply.error.message));
        }
    }

    function _call(iface, member, signature, args) {
        DBus.SystemBus.asyncCall({
            service: service,
            path: path,
            iface: iface,
            member: member,
            signature: signature,
            arguments: args,
        }, () => {}, reply => console.warn("rime:", member, "refused:", reply.error.message));
    }

    // The Gaming switch: Daily <-> Gaming through rimed's own composition (Mode.Set). The switch does not flip itself;
    // it shows Mode.Active once rimed says it changed.
    function toggleGaming() {
        _call("org.rimeos.Rimed1.Mode", "Set", "(s)", [gaming ? "daily" : "gaming"]);
    }

    // The Update button while an update is available (rimed asks for an administrator through Plasma's polkit agent).
    function applyUpdate() {
        _call("org.rimeos.Rimed1.Updates", "Apply", "", []);
    }

    // rimed appeared, restarted or went away: read everything again.
    property DBus.DBusServiceWatcher rimedWatcher: DBus.DBusServiceWatcher {
        busType: DBus.BusType.System
        watchedService: model.service
        onRegisteredChanged: model.reload()
    }

    // Every property change rimed makes, for the three interfaces above.
    property DBus.SignalWatcher propertiesWatcher: DBus.SignalWatcher {
        busType: DBus.BusType.System
        service: model.service
        path: model.path
        iface: "org.freedesktop.DBus.Properties"

        function dbusPropertiesChanged(iface, changed, invalidated) {
            model._merge(String(iface), changed);
        }
    }

    Component.onCompleted: reload()
}
