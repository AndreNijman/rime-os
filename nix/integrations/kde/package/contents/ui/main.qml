/*
    Rime in Plasma's system tray (docs/nixos/desktops.md §6, D10).

      Gaming       a switch: rimed Mode.Set("gaming"|"daily"); "Game running" while GameMode.Active.
      Update       shown while rimed's Updates.State is available (click: Updates.Apply, Plasma's polkit agent asks
                   for an administrator), downloading/installing (progress, not clickable) or restart (click: Plasma's
                   own restart dialog).
      Open Rime Settings   the rime-settings: URL, which org.rimeos.Settings.desktop handles.

    The applet sits in the tray's hidden items while Daily and up to date, and shows itself while Gaming is on or an
    update waits. Everything is read by RimedModel.qml, which follows PropertiesChanged; this file only draws.

    Test hook: when a process owns org.rimeos.QuickSettingsTest on the session bus (only the VM tests run one), every
    redraw is reported to it as JSON. Nothing is sent otherwise: the name is watched, not polled.
*/
pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.components as PlasmaComponents3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami
import org.kde.plasma.workspace.dbus as DBus

PlasmoidItem {
    id: root

    RimedModel {
        id: rimed
        onRedrawn: root.report()
    }

    function openSettings(page) {
        Qt.openUrlExternally("rime-settings:" + page);
    }

    function restart() {
        // Plasma's own logout/restart dialog (it asks and counts down).
        DBus.SessionBus.asyncCall({
            service: "org.kde.LogoutPrompt",
            path: "/LogoutPrompt",
            iface: "org.kde.LogoutPrompt",
            member: "promptReboot",
            signature: "",
            arguments: [],
        }, () => {}, reply => console.warn("rime: no restart prompt:", reply.error.message));
    }

    Plasmoid.icon: rimed.updateState === "restart" ? "system-reboot"
        : rimed.updateVisible ? "system-software-update" : "input-gaming"
    Plasmoid.status: (rimed.gaming || rimed.updateSensitive) ? PlasmaCore.Types.ActiveStatus
        : PlasmaCore.Types.PassiveStatus
    toolTipMainText: "Rime"
    toolTipSubText: {
        const lines = [rimed.gaming ? "Gaming" : "Daily"];
        if (rimed.gameRunning)
            lines.push("Game running");
        if (rimed.updateVisible)
            lines.push(rimed.updateTitle);
        return lines.join("\n");
    }

    fullRepresentation: PlasmaExtras.Representation {
        Layout.minimumWidth: Kirigami.Units.gridUnit * 16
        Layout.minimumHeight: content.implicitHeight + Kirigami.Units.largeSpacing * 2
        collapseMarginsHint: true

        ColumnLayout {
            id: content
            anchors.fill: parent
            anchors.margins: Kirigami.Units.largeSpacing
            spacing: Kirigami.Units.largeSpacing

            RowLayout {
                Layout.fillWidth: true
                Kirigami.Icon {
                    source: "input-gaming"
                    implicitWidth: Kirigami.Units.iconSizes.medium
                    implicitHeight: implicitWidth
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    PlasmaComponents3.Label {
                        text: "Gaming"
                        Layout.fillWidth: true
                    }
                    PlasmaComponents3.Label {
                        text: rimed.gamingSubtitle
                        visible: text !== ""
                        opacity: 0.7
                        Layout.fillWidth: true
                    }
                }
                PlasmaComponents3.Switch {
                    // Shows rimed's Mode.Active; a click asks rimed, the answer comes back as a property.
                    checked: rimed.gaming
                    enabled: rimed.rimed
                    onToggled: {
                        checked = Qt.binding(() => rimed.gaming);
                        rimed.toggleGaming();
                    }
                    Accessible.name: "Gaming"
                }
            }

            PlasmaComponents3.Button {
                Layout.fillWidth: true
                visible: rimed.updateVisible
                enabled: rimed.updateSensitive
                icon.name: rimed.updateState === "restart" ? "system-reboot" : "system-software-update"
                text: rimed.updateSubtitle !== "" ? rimed.updateTitle + " — " + rimed.updateSubtitle : rimed.updateTitle
                onClicked: rimed.updateState === "restart" ? root.restart() : rimed.applyUpdate()
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            RowLayout {
                Layout.fillWidth: true
                PlasmaComponents3.ToolButton {
                    text: "Gaming settings"
                    icon.name: "input-gaming"
                    onClicked: root.openSettings("gaming")
                }
                Item {
                    Layout.fillWidth: true
                }
                PlasmaComponents3.ToolButton {
                    text: "Open Rime Settings"
                    icon.name: "configure"
                    onClicked: root.openSettings("")
                }
            }
        }
    }

    // ── test hook ────────────────────────────────────────────────────────────
    readonly property string testName: "org.rimeos.QuickSettingsTest"

    DBus.DBusServiceWatcher {
        id: testWatcher
        busType: DBus.BusType.Session
        watchedService: root.testName
        onRegisteredChanged: root.report()
    }

    function report() {
        if (!testWatcher.registered)
            return;
        const v = rimed.view();
        const state = {
            gaming: { checked: v.gamingChecked, subtitle: v.gamingSubtitle, reactive: v.rimed },
            update: { visible: v.updateVisible, title: v.updateTitle, subtitle: v.updateSubtitle },
            status: Plasmoid.status === PlasmaCore.Types.ActiveStatus ? "active" : "passive",
            icon: String(Plasmoid.icon),
        };
        DBus.SessionBus.asyncCall({
            service: root.testName,
            path: "/org/rimeos/QuickSettingsTest",
            iface: root.testName,
            member: "Report",
            signature: "(ss)",
            arguments: ["kde", JSON.stringify(state)],
        }, () => {}, () => {});
    }
}
