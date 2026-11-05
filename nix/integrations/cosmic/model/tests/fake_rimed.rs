//! The COSMIC applet's model against a fake rimed on a private system bus (checks.settings-quicksettings-cosmic).
//!
//! DBUS_SYSTEM_BUS_ADDRESS names the private bus; rime-shell's tests/lib/fake-rimed.py owns org.rimeos.Rimed1 there.
//! What is asserted is the view the model last sent (what the applet draws), never a fresh read: a model that misses
//! PropertiesChanged keeps a stale view and fails.
//!
//! Mutant (checks.settings-quicksettings-cosmic.mutants.no-properties-changed): the model without its
//! PropertiesChanged stream must FAIL here.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_cosmic_model::{View, apply_update, follow, set_gaming};
use zbus::Connection;
use zbus::zvariant::Value;

const TEST_IFACE: &str = "org.rimeos.Rimed1.Test";

async fn change(conn: &Connection, iface: &str, prop: &str, value: Value<'_>) {
    conn.call_method(
        Some("org.rimeos.Rimed1"),
        "/org/rimeos/Rimed1",
        Some(TEST_IFACE),
        "Change",
        &(iface, prop, value),
    )
    .await
    .expect("Test.Change");
}

async fn calls(conn: &Connection) -> Vec<String> {
    let reply = conn
        .call_method(Some("org.rimeos.Rimed1"), "/org/rimeos/Rimed1", Some(TEST_IFACE), "Calls", &())
        .await
        .expect("Test.Calls");
    reply.body().deserialize::<Vec<String>>().expect("as")
}

struct Seen(Arc<Mutex<Option<View>>>);

impl Seen {
    async fn wait(&self, what: &str, pred: impl Fn(&View) -> bool) -> Result<View, String> {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(v) = self.0.lock().unwrap().clone() {
                if pred(&v) {
                    return Ok(v);
                }
                if Instant::now() > end {
                    return Err(format!("{what}: {v:?}"));
                }
            } else if Instant::now() > end {
                return Err(format!("{what}: no view yet"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_rimed() {
    assert!(
        std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS").is_some(),
        "run under the fake bus (tests/lib/fake-system-bus.sh): DBUS_SYSTEM_BUS_ADDRESS is not set"
    );
    let conn = Connection::system().await.expect("private system bus");

    let latest = Arc::new(Mutex::new(None));
    let sink = latest.clone();
    let follower_conn = conn.clone();
    tokio::spawn(async move {
        let _ = follow(&follower_conn, move |v| *sink.lock().unwrap() = Some(v)).await;
    });
    let seen = Seen(latest);

    let mut failures = Vec::new();
    let mut passed = 0;
    let mut check = |r: Result<View, String>, msg: &str| match r {
        Ok(_) => {
            println!("  PASS  {msg}");
            passed += 1;
        }
        Err(e) => {
            println!("  FAIL  {msg}: {e}");
            failures.push(msg.to_string());
        }
    };

    // ── initial state ────────────────────────────────────────────────────────
    check(seen.wait("rimed", |v| v.rimed).await, "the model sees rimed");
    check(seen.wait("daily", |v| v.rimed && !v.gaming_checked).await, "Gaming starts off (Mode.Active daily)");
    check(seen.wait("hidden", |v| v.rimed && !v.update_visible).await, "Update hidden while up to date");

    // ── Gaming follows Mode.Active changed elsewhere ────────────────────────
    change(&conn, "org.rimeos.Rimed1.Mode", "Active", Value::from("gaming")).await;
    check(seen.wait("gaming", |v| v.gaming_checked).await, "Gaming turns on when Mode.Active changes elsewhere");
    change(&conn, "org.rimeos.Rimed1.GameMode", "Active", Value::from(true)).await;
    check(
        seen.wait("game running", |v| v.gaming_subtitle == "Game running").await,
        "subtitle \"Game running\" while GameMode.Active",
    );
    change(&conn, "org.rimeos.Rimed1.GameMode", "Active", Value::from(false)).await;
    check(seen.wait("game ended", |v| v.gaming_subtitle.is_empty()).await, "subtitle clears when the game ends");
    change(&conn, "org.rimeos.Rimed1.Mode", "Active", Value::from("daily")).await;
    check(seen.wait("daily", |v| !v.gaming_checked).await, "Gaming turns off when Mode.Active changes elsewhere");

    // ── a click calls Mode.Set ──────────────────────────────────────────────
    set_gaming(&conn, true).await.expect("Mode.Set");
    let c = calls(&conn).await;
    check(
        if c.iter().any(|l| l == "org.rimeos.Rimed1.Mode.Set gaming") { Ok(View::default()) } else { Err(format!("{c:?}")) },
        "a click calls Mode.Set gaming",
    );
    check(seen.wait("answer", |v| v.gaming_checked).await, "the toggle shows rimed's answer");
    set_gaming(&conn, false).await.expect("Mode.Set");
    check(seen.wait("daily", |v| !v.gaming_checked).await, "a second click shows Daily");

    // ── Update item ──────────────────────────────────────────────────────────
    change(&conn, "org.rimeos.Rimed1.Updates", "Available", Value::from("2026.11.04")).await;
    change(&conn, "org.rimeos.Rimed1.Updates", "State", Value::from("available")).await;
    check(
        seen.wait("available", |v| v.update_visible && v.update_sensitive && v.update_subtitle == "Rime 2026.11.04").await,
        "Update shown, naming the release, when an update is available",
    );
    apply_update(&conn).await.expect("Updates.Apply");
    let c = calls(&conn).await;
    check(
        if c.iter().any(|l| l.starts_with("org.rimeos.Rimed1.Updates.Apply")) { Ok(View::default()) } else { Err(format!("{c:?}")) },
        "a click calls Updates.Apply",
    );
    check(
        seen.wait("installing", |v| v.update_state == "installing" && v.update_visible && !v.update_sensitive).await,
        "while installing it is shown and not clickable",
    );
    change(&conn, "org.rimeos.Rimed1.Updates", "State", Value::from("restart")).await;
    check(
        seen.wait("restart", |v| v.update_visible && v.update_title == "Restart to finish").await,
        "restart: shown as \"Restart to finish\"",
    );
    change(&conn, "org.rimeos.Rimed1.Updates", "State", Value::from("up-to-date")).await;
    check(seen.wait("up to date", |v| v.update_state == "up-to-date" && !v.update_visible).await, "hidden again when up to date");
    for st in ["failed", "unknown", "deferred"] {
        change(&conn, "org.rimeos.Rimed1.Updates", "State", Value::from(st)).await;
        check(seen.wait(st, |v| v.update_state == st && !v.update_visible).await, &format!("hidden for {st}"));
    }

    // ── no polling ───────────────────────────────────────────────────────────
    let reads = |c: &[String]| c.iter().filter(|l| l.starts_with("Get ")).count();
    let before = reads(&calls(&conn).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = reads(&calls(&conn).await);
    check(
        if after == before { Ok(View::default()) } else { Err(format!("{} reads in 3 s of quiet", after - before)) },
        &format!("no property read while nothing changed ({before} reads, all at start)"),
    );

    println!("RESULT: passed={passed} failed={}", failures.len());
    assert!(failures.is_empty(), "failed: {failures:?}");
}
