// settings-quicksettings-gnome.js — the GNOME extension's model (rimed.js) against a fake rimed on a private system bus.
//
//   gjs -m settings-quicksettings-gnome.js <path to rimed.js>
//
// DBUS_SYSTEM_BUS_ADDRESS names the private bus; fake-rimed.py (rime-shell tests/lib) owns org.rimeos.Rimed1 there.
// What is asserted is the model's view as it stood at its last `changed` signal (the snapshot the extension redraws
// from), never a fresh read: a model that misses PropertiesChanged keeps a stale snapshot and fails.
//
//   - the Gaming toggle follows Mode.Active changed elsewhere (the fake's Test.Change: the CLI, another desktop);
//   - "Game running" follows GameMode.Active;
//   - a click calls Mode.Set with the other mode, and the toggle shows rimed's answer;
//   - the Update item is hidden while up to date, shown for available (click: Updates.Apply) and restart, and
//     hidden again when up to date;
//   - nothing is read while nothing changes (no polling): the fake logs every property read.
//
// Mutant (checks.settings-quicksettings-gnome.mutants.no-properties-changed): rimed.js without its
// g-properties-changed handler must FAIL here.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import System from 'system';

const modPath = ARGV[0];
const {RimedModel} = await import(`file://${modPath}`);

let pass = 0, fail = 0;
const ok = msg => { print(`  PASS  ${msg}`); pass++; };
const bad = msg => { print(`  FAIL  ${msg}`); fail++; };

const bus = Gio.DBus.system;
const call = (iface, method, params, type = null) => bus.call_sync('org.rimeos.Rimed1', '/org/rimeos/Rimed1', iface,
    method, params, type ? new GLib.VariantType(type) : null, Gio.DBusCallFlags.NONE, -1, null);
const change = (iface, prop, value) => call('org.rimeos.Rimed1.Test', 'Change',
    new GLib.Variant('(ssv)', [iface, prop, value]));
const calls = () => call('org.rimeos.Rimed1.Test', 'Calls', null, '(as)').deepUnpack()[0];

const model = new RimedModel();
let snapshot = null;
let signals = 0;
model.connect('changed', () => {
    snapshot = model.view();
    signals++;
});

const ctx = GLib.MainContext.default();
function waitFor(pred, ms = 3000) {
    const end = GLib.get_monotonic_time() + ms * 1000;
    while (GLib.get_monotonic_time() < end) {
        if (snapshot && pred(snapshot))
            return true;
        ctx.iteration(false);
        GLib.usleep(5000);
    }
    return snapshot !== null && pred(snapshot);
}
function settle(ms) {
    const end = GLib.get_monotonic_time() + ms * 1000;
    while (GLib.get_monotonic_time() < end) {
        ctx.iteration(false);
        GLib.usleep(5000);
    }
}
async function awaitCall(promise, ms = 3000) {
    let done = false, err = null;
    promise.then(() => { done = true; }, e => { done = true; err = e; });
    const end = GLib.get_monotonic_time() + ms * 1000;
    while (!done && GLib.get_monotonic_time() < end) {
        ctx.iteration(false);
        GLib.usleep(5000);
    }
    if (err)
        throw err;
    return done;
}

model.start();

// ── initial state ────────────────────────────────────────────────────────────
waitFor(s => s.rimed) ? ok('the model sees rimed') : bad('the model never saw rimed');
waitFor(s => !s.gamingChecked) ? ok('Gaming starts off (Mode.Active daily)') : bad(`Gaming starts ${JSON.stringify(snapshot)}`);
!snapshot.updateVisible ? ok('Update hidden while up to date') : bad('Update shown while up to date');

// ── Gaming follows Mode.Active changed elsewhere ────────────────────────────
change('org.rimeos.Rimed1.Mode', 'Active', new GLib.Variant('s', 'gaming'));
waitFor(s => s.gamingChecked, 2000) ? ok('Gaming turns on when Mode.Active changes elsewhere')
    : bad(`Gaming did not follow Mode.Active=gaming: ${JSON.stringify(snapshot)}`);
change('org.rimeos.Rimed1.GameMode', 'Active', new GLib.Variant('b', true));
waitFor(s => s.gamingSubtitle === 'Game running', 2000) ? ok('subtitle "Game running" while GameMode.Active')
    : bad(`subtitle is '${snapshot.gamingSubtitle}'`);
change('org.rimeos.Rimed1.GameMode', 'Active', new GLib.Variant('b', false));
waitFor(s => s.gamingSubtitle === '', 2000) ? ok('subtitle clears when the game ends') : bad('subtitle stayed');
change('org.rimeos.Rimed1.Mode', 'Active', new GLib.Variant('s', 'daily'));
waitFor(s => !s.gamingChecked, 2000) ? ok('Gaming turns off when Mode.Active changes elsewhere')
    : bad('Gaming did not follow Mode.Active=daily');

// ── a click calls Mode.Set ──────────────────────────────────────────────────
await awaitCall(model.toggleGaming());
calls().includes('org.rimeos.Rimed1.Mode.Set gaming') ? ok('a click calls Mode.Set gaming')
    : bad(`no Mode.Set gaming in ${JSON.stringify(calls())}`);
waitFor(s => s.gamingChecked, 2000) ? ok("the toggle shows rimed's answer") : bad('the toggle did not turn on');
await awaitCall(model.toggleGaming());
calls().includes('org.rimeos.Rimed1.Mode.Set daily') ? ok('a second click calls Mode.Set daily')
    : bad('no Mode.Set daily');
waitFor(s => !s.gamingChecked, 2000) ? ok('and it shows Daily') : bad('the toggle did not turn off');

// ── Update item ──────────────────────────────────────────────────────────────
change('org.rimeos.Rimed1.Updates', 'Available', new GLib.Variant('s', '2026.11.04'));
change('org.rimeos.Rimed1.Updates', 'State', new GLib.Variant('s', 'available'));
waitFor(s => s.updateVisible, 2000) ? ok('Update shown when an update is available')
    : bad(`Update hidden for available: ${JSON.stringify(snapshot)}`);
snapshot.updateSubtitle === 'Rime 2026.11.04' ? ok('it names the release') : bad(`subtitle '${snapshot.updateSubtitle}'`);
await awaitCall(model.applyUpdate());
calls().includes('org.rimeos.Rimed1.Updates.Apply ') ? ok('a click calls Updates.Apply')
    : bad(`no Updates.Apply in ${JSON.stringify(calls())}`);
waitFor(s => s.updateState === 'installing', 2000) && snapshot.updateVisible && !snapshot.updateSensitive
    ? ok('while installing it is shown and not clickable') : bad(`installing: ${JSON.stringify(snapshot)}`);
change('org.rimeos.Rimed1.Updates', 'State', new GLib.Variant('s', 'restart'));
waitFor(s => s.updateState === 'restart', 2000) && snapshot.updateVisible && snapshot.updateTitle === 'Restart to finish'
    ? ok('restart: shown as "Restart to finish"') : bad(`restart: ${JSON.stringify(snapshot)}`);
change('org.rimeos.Rimed1.Updates', 'State', new GLib.Variant('s', 'up-to-date'));
waitFor(s => !s.updateVisible, 2000) ? ok('hidden again when up to date') : bad('Update stayed visible');
for (const st of ['failed', 'unknown', 'deferred']) {
    change('org.rimeos.Rimed1.Updates', 'State', new GLib.Variant('s', st));
    waitFor(s => s.updateState === st, 2000) && !snapshot.updateVisible ? ok(`hidden for ${st}`) : bad(`shown for ${st}`);
}

// ── no polling ───────────────────────────────────────────────────────────────
const reads = () => calls().filter(c => c.startsWith('Get ')).length;
const before = reads();
settle(3000);
const after = reads();
after === before ? ok(`no property read while nothing changed (${before} reads, all at start)`)
    : bad(`the model read ${after - before} properties in 3 s of quiet`);
print(`signals: ${signals}`);

model.destroy();
print(`RESULT: passed=${pass} failed=${fail}`);
System.exit(fail === 0 ? 0 : 1);
