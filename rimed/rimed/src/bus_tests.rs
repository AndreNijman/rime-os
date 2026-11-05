//! rimed on a private bus: the power-profiles API (ppd.rs), `Mode.Set`/`Mode.Active` and `Updates` (updates.rs),
//! called the way a desktop calls them, through a real dbus-daemon, with a fake polkit and a fake systemd on the same
//! bus. Nothing here touches the machine: the writer is a mock and every path is a fixture.
//!
//! dbus-daemon must be on PATH (nix/pkgs/rimed.nix has it in nativeCheckInputs). Its absence fails these tests; they
//! never skip themselves.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rimed_core::gpu::MockNvidiaSmi;
use rimed_core::syswriter::MockWriter;
use rimed_core::tier::Tier;
use rimed_core::{ProfileSet, Selection};
use zbus::export::futures_util::StreamExt;
use zbus::{interface, Connection};
use zvariant::{OwnedObjectPath, OwnedValue, Type, Value};

use crate::ppd;
use crate::state::{Ctx, State};
use crate::updates;

const PROFILE: &str = r#"
    id = "test-ppd"
    kind = "device"
    [defaults]
    ac = "balanced"
    battery = "power-saver"
    [tiers.performance]
    governor = "performance"
    [tiers.balanced]
    governor = "powersave"
    [tiers.power-saver]
    governor = "powersave"
    [gamemode]
    tier = "performance"
    cpuset = "off"
    irq = "off"
    scx = "scx_lavd"
    [gamemode.nvidia]
    enabled = false
"#;

const RIMED: &str = "org.rimeos.Rimed1";
const RIMED_PATH: &str = "/org/rimeos/Rimed1";
const PROPS: &str = "org.freedesktop.DBus.Properties";

// ── the bus ──────────────────────────────────────────────────────────────────

struct Bus {
    child: Child,
    addr: String,
    root: PathBuf,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("rimed-bus-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn start_bus(root: &Path) -> Bus {
    let sock = root.join("bus");
    let conf = root.join("bus.conf");
    std::fs::write(
        &conf,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
  </policy>
</busconfig>
"#,
            sock.display()
        ),
    )
    .unwrap();
    let child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", conf.display()))
        .arg("--nofork")
        .arg("--nopidfile")
        .stdout(Stdio::null())
        .spawn()
        .expect("dbus-daemon is on PATH (nix/pkgs/rimed.nix nativeCheckInputs)");
    for _ in 0..200 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(sock.exists(), "dbus-daemon made no socket");
    Bus { child, addr: format!("unix:path={}", sock.display()), root: root.to_path_buf() }
}

async fn client(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.addr.as_str()).unwrap().build().await.unwrap()
}

// ── fake polkit and systemd ─────────────────────────────────────────────────

#[derive(Default)]
struct Record {
    /// (action, flags) of every check.
    checks: Vec<(String, u32)>,
    deny: HashSet<String>,
    units: Vec<String>,
}

#[derive(serde::Deserialize, Type)]
struct Subject {
    _kind: String,
    _details: HashMap<String, OwnedValue>,
}

#[derive(serde::Serialize, Type)]
struct AuthResult {
    is_authorized: bool,
    is_challenge: bool,
    details: HashMap<String, String>,
}

struct FakePolkit(Arc<Mutex<Record>>);

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl FakePolkit {
    async fn check_authorization(
        &self,
        _subject: Subject,
        action_id: String,
        _details: HashMap<String, String>,
        flags: u32,
        _cancellation_id: String,
    ) -> AuthResult {
        let mut r = self.0.lock().unwrap();
        r.checks.push((action_id.clone(), flags));
        AuthResult { is_authorized: !r.deny.contains(&action_id), is_challenge: false, details: HashMap::new() }
    }
}

struct FakeSystemd(Arc<Mutex<Record>>);

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl FakeSystemd {
    async fn start_unit(&self, name: String, _mode: String) -> OwnedObjectPath {
        self.0.lock().unwrap().units.push(name);
        OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap()
    }
}

// ── rimed under test ─────────────────────────────────────────────────────────

struct Rig {
    bus: Bus,
    ctx: Arc<Ctx>,
    rimed: Connection,
    record: Arc<Mutex<Record>>,
    _fakes: Connection,
    sys: PathBuf,
    upd: updates::Paths,
    /// The writer the daemon writes through, to see what it wrote.
    writer: Arc<MockWriter>,
}

fn set_ac(sys: &Path, online: bool) {
    let d = sys.join("class/power_supply/AC");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("type"), "Mains\n").unwrap();
    std::fs::write(d.join("online"), if online { "1\n" } else { "0\n" }).unwrap();
}

async fn rig(tag: &str) -> Rig {
    let root = scratch(tag);
    let bus = start_bus(&root);
    let sys = root.join("sys");
    set_ac(&sys, true);
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/test-ppd.toml"), PROFILE).unwrap();
    let set = ProfileSet::load(Some(&root.join("profiles"))).unwrap();
    let selection = Selection {
        generic: "test-ppd".into(),
        class: None,
        device: Some("test-ppd".into()),
        active: "test-ppd".into(),
    };
    let fingerprint = rimed_core::Fingerprint::detect_from(&root, &sys);
    let writer = Arc::new(MockWriter::new());
    let ctx = Ctx::new(
        set,
        selection,
        fingerprint,
        writer.clone(),
        false,
        State { tier: Tier::Balanced, auto_switch: true, on_ac: true, travel_mode: false, charge_start: 0, charge_stop: 100 },
        &sys,
        root.join("no-irqs"),
        root.join("proc"),
        Arc::new(MockNvidiaSmi::default()),
        root.join("state/mode"),
    );
    let record = Arc::new(Mutex::new(Record::default()));
    let fakes = zbus::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .name("org.freedesktop.PolicyKit1")
        .unwrap()
        .name("org.freedesktop.systemd1")
        .unwrap()
        .serve_at("/org/freedesktop/PolicyKit1/Authority", FakePolkit(record.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/systemd1", FakeSystemd(record.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let upd = updates::Paths {
        check: root.join("run/rime-update/state"),
        status: root.join("run/rime-live/status.json"),
        nixos_marker: root.join("etc/NIXOS"),
        current_system: root.join("run/current-system"),
    };
    std::fs::create_dir_all(root.join("run")).unwrap();
    let rimed = crate::serve(zbus::connection::Builder::address(bus.addr.as_str()).unwrap(), ctx.clone(), upd.clone(), true)
        .await
        .unwrap();
    // The holders' watch subscribes in a task of its own; let it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    Rig { bus, ctx, rimed, record, _fakes: fakes, sys, upd, writer }
}

async fn get(c: &Connection, dest: &str, path: &str, iface: &str, prop: &str) -> zbus::Result<OwnedValue> {
    let r = c.call_method(Some(dest), path, Some(PROPS), "Get", &(iface, prop)).await?;
    let v: OwnedValue = r.body().deserialize()?;
    Ok(v)
}

async fn get_s(c: &Connection, dest: &str, path: &str, iface: &str, prop: &str) -> String {
    String::try_from(get(c, dest, path, iface, prop).await.unwrap()).unwrap()
}

async fn active(c: &Connection) -> String {
    get_s(c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile").await
}

async fn set_prop(c: &Connection, dest: &str, path: &str, iface: &str, prop: &str, v: Value<'_>) -> zbus::Result<()> {
    c.call_method(Some(dest), path, Some(PROPS), "Set", &(iface, prop, v)).await.map(|_| ())
}

async fn hold(c: &Connection, profile: &str, app: &str) -> zbus::Result<u32> {
    let r = c
        .call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "HoldProfile", &(profile, "testing", app))
        .await?;
    r.body().deserialize()
}

async fn holds(c: &Connection) -> Vec<(String, String)> {
    let v = get(c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfileHolds").await.unwrap();
    let list: Vec<HashMap<String, OwnedValue>> = Vec::try_from(v).unwrap();
    list.into_iter()
        .map(|m| {
            let s = |k: &str| String::try_from(m[k].try_clone().unwrap()).unwrap();
            (s("ApplicationId"), s("Profile"))
        })
        .collect()
}

fn error_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        zbus::Error::FDO(f) => format!("{f:?}"),
        other => format!("{other}"),
    }
}

/// Collect `PropertiesChanged` on `path` until `want` is seen (property, string value) or the time runs out.
async fn props_stream(c: &Connection, path: &str) -> zbus::MessageStream {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(PROPS)
        .unwrap()
        .member("PropertiesChanged")
        .unwrap()
        .path(path.to_string())
        .unwrap()
        .build();
    zbus::MessageStream::for_match_rule(rule, c, Some(64)).await.unwrap()
}

async fn saw_change(s: &mut zbus::MessageStream, iface: &str, prop: &str, want: Value<'_>) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(Ok(m))) = tokio::time::timeout(left, s.next()).await else { return false };
        let Ok((i, changed, _)) = m.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else {
            continue;
        };
        if i == iface {
            if let Some(v) = changed.get(prop) {
                if **v == want {
                    return true;
                }
            }
        }
    }
}

async fn released_stream(c: &Connection) -> zbus::MessageStream {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(ppd::NAME)
        .unwrap()
        .member("ProfileReleased")
        .unwrap()
        .build();
    zbus::MessageStream::for_match_rule(rule, c, Some(64)).await.unwrap()
}

async fn next_released(s: &mut zbus::MessageStream) -> Option<u32> {
    let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(5), s.next()).await else { return None };
    m.body().deserialize::<u32>().ok()
}

// ── power-profiles ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_active_profile_write_is_set_tier_and_releases_every_hold() {
    let r = rig("write").await;
    let c = client(&r.bus).await;
    let mut rel = released_stream(&c).await;
    assert_eq!(active(&c).await, "balanced");
    let cookie = hold(&c, "performance", "org.example.Build").await.unwrap();
    assert_eq!(active(&c).await, "performance");
    assert_eq!(holds(&c).await, vec![("org.example.Build".to_string(), "performance".to_string())]);

    set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile", Value::from("power-saver")).await.unwrap();
    assert_eq!(r.ctx.state.lock().await.tier, Tier::PowerSaver);
    assert_eq!(active(&c).await, "power-saver");
    assert!(holds(&c).await.is_empty(), "the write released the hold");
    assert_eq!(next_released(&mut rel).await, Some(cookie));
    assert!(r.ctx.state.lock().await.auto_switch, "auto-switch untouched, as Power.SetTier leaves it");
    assert!(r.record.lock().unwrap().checks.iter().any(|(a, f)| a == ppd::ACTION_SWITCH && *f == 0));
    // Through the old name, and the Rime surface agrees.
    let legacy = get_s(&c, ppd::LEGACY_NAME, ppd::LEGACY_PATH, ppd::LEGACY_NAME, "ActiveProfile").await;
    assert_eq!(legacy, "power-saver");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, "org.rimeos.Rimed1.Power", "Tier").await, "power-saver");

    let e = set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile", Value::from("turbo")).await.unwrap_err();
    assert!(error_name(&e).contains("InvalidArgs"), "{e}");
    r.record.lock().unwrap().deny.insert(ppd::ACTION_SWITCH.into());
    let e = set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile", Value::from("performance")).await.unwrap_err();
    assert!(error_name(&e).contains("AccessDenied"), "{e}");
    assert_eq!(r.ctx.state.lock().await.tier, Tier::PowerSaver);
    drop(r.rimed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn power_saver_holds_beat_performance_holds_and_the_last_release_returns() {
    let r = rig("prec").await;
    let a = client(&r.bus).await;
    let b = client(&r.bus).await;
    let ca = hold(&a, "performance", "org.example.A").await.unwrap();
    assert_eq!(active(&a).await, "performance");
    let cb = hold(&b, "power-saver", "org.example.B").await.unwrap();
    assert_eq!(active(&a).await, "power-saver");
    let ca2 = hold(&a, "performance", "org.example.A2").await.unwrap();
    assert_eq!(active(&a).await, "power-saver", "a later performance hold does not beat power-saver");
    a.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(cb)).await.unwrap();
    assert_eq!(active(&a).await, "performance");
    a.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(ca)).await.unwrap();
    a.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(ca2)).await.unwrap();
    assert_eq!(active(&a).await, "balanced", "back to the tier before the first hold");
    let e = a.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(ca)).await.unwrap_err();
    assert!(error_name(&e).contains("InvalidArgs"), "{e}");
    let e = hold(&a, "balanced", "org.example.A").await.unwrap_err();
    assert!(error_name(&e).contains("InvalidArgs"), "{e}");
    r.record.lock().unwrap().deny.insert(ppd::ACTION_HOLD.into());
    let e = hold(&a, "performance", "org.example.A").await.unwrap_err();
    assert!(error_name(&e).contains("AccessDenied"), "{e}");
    assert_eq!(active(&a).await, "balanced");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hold_goes_when_its_holder_leaves_the_bus() {
    let r = rig("owner").await;
    let watcher = client(&r.bus).await;
    let mut rel = released_stream(&watcher).await;
    {
        let holder = client(&r.bus).await;
        let cookie = hold(&holder, "performance", "org.example.Gone").await.unwrap();
        assert_eq!(active(&watcher).await, "performance");
        drop(holder);
        assert_eq!(next_released(&mut rel).await, Some(cookie));
    }
    for _ in 0..50 {
        if holds(&watcher).await.is_empty() && active(&watcher).await == "balanced" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the hold outlived its holder: {:?}", holds(&watcher).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ac_change_is_announced_and_holds_keep_the_tier() {
    let r = rig("ac").await;
    let c = client(&r.bus).await;
    let mut ch = props_stream(&c, ppd::PATH).await;
    let mut degraded = String::new();
    set_ac(&r.sys, false);
    crate::ac_tick(&r.ctx, &r.rimed, &mut degraded).await;
    assert_eq!(r.ctx.state.lock().await.tier, Tier::PowerSaver);
    assert!(
        saw_change(&mut ch, ppd::NAME, "ActiveProfile", Value::from("power-saver")).await,
        "no PropertiesChanged for ActiveProfile from the AC path"
    );
    // A hold keeps the tier through an AC change, and its release lands on what auto-switch wants then.
    let cookie = hold(&c, "performance", "org.example.Build").await.unwrap();
    set_ac(&r.sys, true);
    crate::ac_tick(&r.ctx, &r.rimed, &mut degraded).await;
    assert_eq!(active(&c).await, "performance");
    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(cookie)).await.unwrap();
    assert_eq!(active(&c).await, "balanced", "the AC default, not the tier before the hold");
    // PerformanceDegraded follows sysfs.
    hold(&c, "performance", "org.example.Build").await.unwrap();
    let d = r.sys.join("bus/platform/devices/thinkpad_acpi");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("dytc_lapmode"), "1\n").unwrap();
    crate::ac_tick(&r.ctx, &r.rimed, &mut degraded).await;
    assert!(saw_change(&mut ch, ppd::NAME, "PerformanceDegraded", Value::from("lap-detected")).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rest_of_the_api() {
    let r = rig("rest").await;
    let c = client(&r.bus).await;
    assert_eq!(get_s(&c, ppd::NAME, ppd::PATH, ppd::NAME, "Version").await, "0.30");
    assert_eq!(get_s(&c, ppd::NAME, ppd::PATH, ppd::NAME, "PerformanceInhibited").await, "");
    let profiles: Vec<HashMap<String, OwnedValue>> =
        Vec::try_from(get(&c, ppd::NAME, ppd::PATH, ppd::NAME, "Profiles").await.unwrap()).unwrap();
    let names: Vec<String> =
        profiles.iter().map(|m| String::try_from(m["Profile"].try_clone().unwrap()).unwrap()).collect();
    assert_eq!(names, ["power-saver", "balanced", "performance"], "lowest first, as power-profiles-daemon lists them");
    for m in &profiles {
        assert_eq!(String::try_from(m["Driver"].try_clone().unwrap()).unwrap(), "rimed");
    }
    let actions: Vec<String> = Vec::try_from(get(&c, ppd::NAME, ppd::PATH, ppd::NAME, "Actions").await.unwrap()).unwrap();
    assert!(actions.is_empty());
    let e = c
        .call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "SetActionEnabled", &("trickle_charge", true))
        .await
        .unwrap_err();
    assert!(error_name(&e).contains("NotSupported"), "{e}");
    // BatteryAware is Power.AutoSwitch.
    set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "BatteryAware", Value::from(false)).await.unwrap();
    assert!(!r.ctx.state.lock().await.auto_switch);
    assert!(r.record.lock().unwrap().checks.iter().any(|(a, _)| a == ppd::ACTION_BATTERY_AWARE));
    let e = set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "Version", Value::from("9")).await.unwrap_err();
    assert!(error_name(&e).contains("PropertyReadOnly"), "{e}");
    // The old name has no BatteryAware.
    let e = get(&c, ppd::LEGACY_NAME, ppd::LEGACY_PATH, ppd::LEGACY_NAME, "BatteryAware").await.unwrap_err();
    assert!(error_name(&e).contains("UnknownProperty"), "{e}");
    // GetAll answers through the replaced Properties interface.
    let r2 = c.call_method(Some(ppd::LEGACY_NAME), ppd::LEGACY_PATH, Some(PROPS), "GetAll", &(ppd::LEGACY_NAME)).await.unwrap();
    let all: HashMap<String, OwnedValue> = r2.body().deserialize().unwrap();
    assert!(all.contains_key("ActiveProfile") && all.contains_key("ActiveProfileHolds"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn game_mode_is_a_performance_hold_and_a_write_sets_where_it_returns() {
    let r = rig("game").await;
    let c = client(&r.bus).await;
    let mut ch = props_stream(&c, ppd::PATH).await;
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(true)).await.unwrap();
    assert_eq!(active(&c).await, "performance");
    assert_eq!(holds(&c).await, vec![(ppd::GAME_APP_ID.to_string(), "performance".to_string())]);
    assert!(saw_change(&mut ch, ppd::NAME, "ActiveProfile", Value::from("performance")).await);
    set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile", Value::from("power-saver")).await.unwrap();
    assert_eq!(active(&c).await, "performance", "game mode keeps its tier");
    assert!(saw_change(&mut ch, ppd::NAME, "ActiveProfile", Value::from("performance")).await, "re-emitted");
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(false)).await.unwrap();
    assert_eq!(active(&c).await, "power-saver", "the write chose where game mode returns");
    assert!(holds(&c).await.is_empty());
}

/// docs/nixos/desktops.md §7 / §12.5a: a power-saver hold beats game mode's performance as it beats every other
/// performance hold. Gaming stays on (`Mode.Active` stays `gaming`, game mode's own hold stays listed), the machine runs
/// power-saver until the hold goes, and game mode ending with the hold still standing keeps power-saver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_power_saver_hold_beats_game_mode_and_gaming_stays_on() {
    let r = rig("game-ps").await;
    let c = client(&r.bus).await;
    let mode = "org.rimeos.Rimed1.Mode";
    let mut ch = props_stream(&c, ppd::PATH).await;
    c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("gaming")).await.unwrap();
    assert_eq!(active(&c).await, "performance");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "gaming");

    let cookie = hold(&c, "power-saver", "org.example.LowBattery").await.unwrap();
    assert_eq!(active(&c).await, "power-saver", "a power-saver hold beats game mode's performance");
    assert_eq!(r.ctx.state.lock().await.tier, Tier::PowerSaver, "and the machine runs it");
    assert!(saw_change(&mut ch, ppd::NAME, "ActiveProfile", Value::from("power-saver")).await);
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "gaming", "Gaming is still on");
    assert!(r.ctx.game_active().await);
    assert_eq!(
        holds(&c).await,
        vec![
            (ppd::GAME_APP_ID.to_string(), "performance".to_string()),
            ("org.example.LowBattery".to_string(), "power-saver".to_string())
        ]
    );
    // A performance hold on top changes nothing.
    let perf = hold(&c, "performance", "org.example.Build").await.unwrap();
    assert_eq!(active(&c).await, "power-saver");

    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(cookie)).await.unwrap();
    assert_eq!(active(&c).await, "performance", "back to game mode's tier");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "gaming");
    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(perf)).await.unwrap();

    // Game mode ending under a standing power-saver hold: the hold still wins.
    hold(&c, "power-saver", "org.example.LowBattery").await.unwrap();
    assert_eq!(active(&c).await, "power-saver");
    c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("daily")).await.unwrap();
    // Mode.Set ends every hold first (hold.rs), then leaves game mode: the tier before the first hold comes back.
    assert!(!r.ctx.game_active().await);
    assert!(holds(&c).await.is_empty());
    let after_daily = active(&c).await;
    assert_eq!(after_daily, "balanced", "Daily on AC: the profile's AC tier");

    // Without Mode.Set: game mode on by GameMode.SetActive, a hold, then SetActive(false).
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(true)).await.unwrap();
    assert_eq!(active(&c).await, "performance");
    let held = hold(&c, "power-saver", "org.example.LowBattery").await.unwrap();
    assert_eq!(active(&c).await, "power-saver");
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(false)).await.unwrap();
    assert_eq!(active(&c).await, "power-saver", "the hold outlives game mode");
    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(held)).await.unwrap();
    assert_eq!(active(&c).await, "balanced", "the tier before game mode");

    // A hold that stands when game mode starts keeps the machine at power-saver from the start.
    let held = hold(&c, "power-saver", "org.example.LowBattery").await.unwrap();
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(true)).await.unwrap();
    assert_eq!(active(&c).await, "power-saver");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "gaming");
    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(held)).await.unwrap();
    assert_eq!(active(&c).await, "performance");
}

/// rime-os #139's intents through the bus: `Power.SetTier` drops every hold and is the user's tier for this power
/// source (kept on disk), an `ActiveProfile` write is the same choice, holds record nothing, and a release that
/// leaves the holds asking for the same tier writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_choice_drops_the_holds_and_is_kept_and_a_release_that_changes_nothing_writes_nothing() {
    let r = rig("choice").await;
    let c = client(&r.bus).await;
    let mut rel = released_stream(&c).await;
    let tier_file = || std::fs::read_to_string(&r.ctx.choice_file).unwrap_or_default();

    let a = hold(&c, "performance", "org.example.A").await.unwrap();
    hold(&c, "performance", "org.example.B").await.unwrap();
    assert_eq!(tier_file(), "", "a hold is not a choice");
    r.writer.clear();
    c.call_method(Some(ppd::NAME), ppd::PATH, Some(ppd::NAME), "ReleaseProfile", &(a)).await.unwrap();
    assert_eq!(active(&c).await, "performance");
    assert!(r.writer.recorded().is_empty(), "wrote {:?}", r.writer.recorded());

    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.Power"), "SetTier", &("power-saver"))
        .await
        .unwrap();
    assert!(holds(&c).await.is_empty());
    assert_eq!(next_released(&mut rel).await, Some(a));
    assert!(next_released(&mut rel).await.is_some(), "the second hold's ProfileReleased");
    assert_eq!(active(&c).await, "power-saver");
    assert!(tier_file().contains("ac=power-saver"), "{}", tier_file());

    set_prop(&c, ppd::NAME, ppd::PATH, ppd::NAME, "ActiveProfile", Value::from("performance")).await.unwrap();
    assert!(tier_file().contains("ac=performance"), "{}", tier_file());

    // An AC change and back: auto-switch follows the choice, not the profile's AC default.
    let mut degraded = String::new();
    set_ac(&r.sys, false);
    crate::ac_tick(&r.ctx, &r.rimed, &mut degraded).await;
    assert_eq!(active(&c).await, "power-saver", "the profile's battery tier");
    set_ac(&r.sys, true);
    crate::ac_tick(&r.ctx, &r.rimed, &mut degraded).await;
    assert_eq!(active(&c).await, "performance", "the user's AC choice");
}

// ── Mode.Set / Mode.Active ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mode_set_composes_the_mode_and_active_follows() {
    let r = rig("mode").await;
    let c = client(&r.bus).await;
    let mut ch = props_stream(&c, RIMED_PATH).await;
    let mode = "org.rimeos.Rimed1.Mode";
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "daily");
    c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("gaming")).await.unwrap();
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Held").await, "gaming");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "gaming");
    assert!(r.ctx.game_active().await);
    // Emitted in this order: game mode's own change, then Mode.Active.
    assert!(saw_change(&mut ch, "org.rimeos.Rimed1.GameMode", "Active", Value::from(true)).await);
    assert!(saw_change(&mut ch, mode, "Active", Value::from("gaming")).await);
    // A held Gaming refuses SetActive(false); Set("daily") is the way out.
    let e = c
        .call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(false))
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("keeps game mode on"), "{e}");
    c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("daily")).await.unwrap();
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Active").await, "daily");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, mode, "Held").await, "");
    assert!(!r.ctx.game_active().await);
    assert!(saw_change(&mut ch, mode, "Active", Value::from("daily")).await);
    // Active also follows a lever moved elsewhere.
    c.call_method(Some(RIMED), RIMED_PATH, Some("org.rimeos.Rimed1.GameMode"), "SetActive", &(true)).await.unwrap();
    assert!(saw_change(&mut ch, mode, "Active", Value::from("gaming")).await);
    let e = c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("turbo")).await.unwrap_err();
    assert!(error_name(&e).contains("InvalidArgs"), "{e}");
    r.record.lock().unwrap().deny.insert(crate::polkit::ACTION_POWER.into());
    let e = c.call_method(Some(RIMED), RIMED_PATH, Some(mode), "Set", &("daily")).await.unwrap_err();
    assert!(error_name(&e).contains("AccessDenied"), "{e}");
}

// ── Updates ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_follow_the_files_and_start_the_units() {
    let r = rig("updates").await;
    let c = client(&r.bus).await;
    let u = "org.rimeos.Rimed1.Updates";
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, u, "State").await, "unknown");
    let mut ch = props_stream(&c, RIMED_PATH).await;
    // The directory does not exist yet: the watch on its parent sees it appear.
    std::fs::create_dir_all(r.upd.check.parent().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tmp = r.upd.check.with_extension("tmp");
    std::fs::write(&tmp, "state=available\ndigest=sha256:x\nrelease=2026.11.02\nrunning=2026.11.01\n").unwrap();
    std::fs::rename(&tmp, &r.upd.check).unwrap();
    assert!(saw_change(&mut ch, u, "State", Value::from("available")).await, "no PropertiesChanged for State");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, u, "Available").await, "2026.11.02");
    assert_eq!(get_s(&c, RIMED, RIMED_PATH, u, "Running").await, "2026.11.01");

    c.call_method(Some(RIMED), RIMED_PATH, Some(u), "Check", &()).await.unwrap();
    nixos(&r.upd);
    c.call_method(Some(RIMED), RIMED_PATH, Some(u), "Apply", &()).await.unwrap();
    {
        let rec = r.record.lock().unwrap();
        assert_eq!(rec.units, [updates::CHECK_UNIT, updates::APPLY_UNIT]);
        assert!(rec.checks.contains(&(updates::ACTION_CHECK.to_string(), 0)));
        assert!(rec.checks.contains(&(updates::ACTION_APPLY.to_string(), 1)), "Apply lets polkit ask: {:?}", rec.checks);
    }
    r.record.lock().unwrap().deny.insert(updates::ACTION_APPLY.into());
    let e = c.call_method(Some(RIMED), RIMED_PATH, Some(u), "Apply", &()).await.unwrap_err();
    assert!(error_name(&e).contains("AccessDenied"), "{e}");
    assert_eq!(r.record.lock().unwrap().units.len(), 2, "a refused Apply starts nothing");
}

/// Make the rig's root look like Rime on NixOS (`/etc/NIXOS`, `/run/current-system` into the store).
fn nixos(upd: &updates::Paths) {
    std::fs::create_dir_all(upd.nixos_marker.parent().unwrap()).unwrap();
    std::fs::write(&upd.nixos_marker, "").unwrap();
    std::os::unix::fs::symlink("/nix/store/00000000000000000000000000000000-nixos-system", &upd.current_system).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_apply_is_refused_before_polkit_off_nixos() {
    let r = rig("updates-bootc").await;
    let c = client(&r.bus).await;
    let u = "org.rimeos.Rimed1.Updates";
    // A marker without a store system is not NixOS either.
    std::fs::create_dir_all(r.upd.nixos_marker.parent().unwrap()).unwrap();
    std::fs::write(&r.upd.nixos_marker, "").unwrap();
    for _ in 0..2 {
        let e = c.call_method(Some(RIMED), RIMED_PATH, Some(u), "Apply", &()).await.unwrap_err();
        assert!(error_name(&e).contains("NotSupported"), "{e}");
        assert!(e.to_string().contains("sudo rime update"), "{e}");
        std::fs::remove_file(&r.upd.nixos_marker).unwrap_or(());
    }
    let rec = r.record.lock().unwrap();
    assert!(rec.units.is_empty(), "Apply started a unit off NixOS: {:?}", rec.units);
    assert!(!rec.checks.iter().any(|(a, _)| a == updates::ACTION_APPLY), "polkit was asked first: {:?}", rec.checks);
}
