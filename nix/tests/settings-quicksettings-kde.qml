/*
    settings-quicksettings-kde.qml — the Plasma applet's model (RimedModel.qml) against a fake rimed on a private
    system bus (checks.settings-quicksettings-kde; rime-shell tests/lib/fake-rimed.py owns org.rimeos.Rimed1 there).

    The check copies this file next to RimedModel.qml and runs qmltestrunner on it. Each step changes rimed and waits
    for the model's view as of its last `redrawn` signal (the snapshot the applet draws from), never a fresh read: a
    model that misses PropertiesChanged keeps a stale snapshot and fails.

    Mutant (checks.settings-quicksettings-kde.mutants.no-properties-changed): RimedModel.qml without its
    PropertiesChanged handler must FAIL here.
*/
pragma ComponentBehavior: Bound

import QtQuick
import QtTest
import org.kde.plasma.workspace.dbus as DBus

TestCase {
    id: t
    name: "RimeQuickSettingsKde"

    property var snapshot: null
    property int signals: 0

    RimedModel {
        id: model
        onRedrawn: {
            t.snapshot = model.view();
            t.signals++;
        }
    }

    function call(iface, member, signature, args) {
        const reply = DBus.SystemBus.asyncCall({
            service: "org.rimeos.Rimed1",
            path: "/org/rimeos/Rimed1",
            iface: iface,
            member: member,
            signature: signature,
            arguments: args,
        });
        tryVerify(() => reply.isFinished, 3000, member + " finished");
        verify(!reply.isError, member + ": " + (reply.isError ? reply.error.message : ""));
        return reply.value;
    }
    function change(iface, prop, value) {
        call("org.rimeos.Rimed1.Test", "Change", "(ssv)", [iface, prop, new DBus.variant(value)]);
    }
    function calls() {
        return call("org.rimeos.Rimed1.Test", "Calls", "", []).map(String);
    }
    function settled(pred, msg) {
        tryVerify(() => t.snapshot !== null && pred(t.snapshot), 2000, msg + ": " + JSON.stringify(t.snapshot));
    }

    function test_a_initial() {
        settled(s => s.rimed, "the model sees rimed");
        settled(s => !s.gamingChecked, "Gaming starts off (Mode.Active daily)");
        verify(!t.snapshot.updateVisible, "Update hidden while up to date");
    }

    function test_b_gaming_follows_elsewhere() {
        change("org.rimeos.Rimed1.Mode", "Active", "gaming");
        settled(s => s.gamingChecked, "Gaming turns on when Mode.Active changes elsewhere");
        change("org.rimeos.Rimed1.GameMode", "Active", true);
        settled(s => s.gamingSubtitle === "Game running", "subtitle Game running while GameMode.Active");
        change("org.rimeos.Rimed1.GameMode", "Active", false);
        settled(s => s.gamingSubtitle === "", "subtitle clears when the game ends");
        change("org.rimeos.Rimed1.Mode", "Active", "daily");
        settled(s => !s.gamingChecked, "Gaming turns off when Mode.Active changes elsewhere");
    }

    function test_c_click_calls_mode_set() {
        model.toggleGaming();
        tryVerify(() => calls().includes("org.rimeos.Rimed1.Mode.Set gaming"), 2000, "a click calls Mode.Set gaming");
        settled(s => s.gamingChecked, "the switch shows rimed's answer");
        model.toggleGaming();
        tryVerify(() => calls().includes("org.rimeos.Rimed1.Mode.Set daily"), 2000, "a second click calls Mode.Set daily");
        settled(s => !s.gamingChecked, "and it shows Daily");
    }

    function test_d_update_item() {
        change("org.rimeos.Rimed1.Updates", "Available", "2026.11.04");
        change("org.rimeos.Rimed1.Updates", "State", "available");
        settled(s => s.updateVisible && s.updateSensitive, "Update shown when an update is available");
        compare(t.snapshot.updateTitle, "Update to 2026.11.04", "it names the release");
        model.applyUpdate();
        tryVerify(() => calls().includes("org.rimeos.Rimed1.Updates.Apply "), 2000, "a click calls Updates.Apply");
        settled(s => s.updateState === "installing" && s.updateVisible && !s.updateSensitive,
            "while installing it is shown and not clickable");
        change("org.rimeos.Rimed1.Updates", "State", "restart");
        settled(s => s.updateState === "restart" && s.updateVisible && s.updateTitle === "Restart to finish",
            "restart: shown as Restart to finish");
        change("org.rimeos.Rimed1.Updates", "State", "up-to-date");
        settled(s => !s.updateVisible, "hidden again when up to date");
        for (const st of ["failed", "unknown", "deferred"]) {
            change("org.rimeos.Rimed1.Updates", "State", st);
            settled(s => s.updateState === st && !s.updateVisible, "hidden for " + st);
        }
    }

    function test_e_no_polling() {
        const reads = () => calls().filter(c => c.startsWith("Get ")).length;
        const before = reads();
        wait(3000);
        const after = reads();
        compare(after - before, 0, "no property read while nothing changed (" + before + " reads, all at start)");
        console.log("signals:", t.signals);
    }
}
