//! Test double for tests/test-rimed-ui-boost.sh: a fake logind (seat0 with one
//! session) and a fake polkit, on whatever bus
//! `DBUS_SYSTEM_BUS_ADDRESS` names. NEVER run it on a real system bus: it
//! refuses to start without that variable.
//!
//! The session's facts are read from files on EVERY property read, so the
//! test can switch the active user or make the session remote mid-run:
//!   $FAKE_SESSION_DIR/uid     the session's uid (default 0)
//!   $FAKE_SESSION_DIR/remote  "1" = remote session
//!   $FAKE_SESSION_DIR/active  "0" = session not active
//!
//! The fake polkit answers the way rimed's shipped policy does for every one
//! of its actions (`allow_active=yes`, `auth_admin` otherwise) with no agent:
//! authorized only when the caller's uid is the user of the active, local
//! session; anyone else gets "not authorized, challenge" and, because rimed
//! asks without the interactive flag, a refusal.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::{interface, zvariant::OwnedObjectPath};
use zvariant::{OwnedValue, Type};

struct Facts {
    dir: PathBuf,
}

impl Facts {
    fn read(&self, f: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(f))
            .ok()
            .map(|s| s.trim().to_string())
    }
}

const SESSION_PATH: &str = "/org/freedesktop/login1/session/c1";

struct Seat;

#[interface(name = "org.freedesktop.login1.Seat")]
impl Seat {
    #[zbus(property)]
    fn active_session(&self) -> (String, OwnedObjectPath) {
        (
            "c1".into(),
            OwnedObjectPath::try_from(SESSION_PATH).expect("valid path"),
        )
    }
}

struct Session {
    facts: Facts,
}

#[interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(property)]
    fn user(&self) -> (u32, OwnedObjectPath) {
        let uid = self
            .facts
            .read("uid")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (
            uid,
            OwnedObjectPath::try_from(format!("/org/freedesktop/login1/user/_{uid}"))
                .expect("valid path"),
        )
    }

    #[zbus(property)]
    fn remote(&self) -> bool {
        self.facts.read("remote").as_deref() == Some("1")
    }

    #[zbus(property)]
    fn active(&self) -> bool {
        self.facts.read("active").as_deref() != Some("0")
    }
}

#[derive(Type, serde::Deserialize)]
struct Subject {
    #[allow(dead_code)]
    kind: String,
    details: HashMap<String, OwnedValue>,
}

struct Authority {
    facts: Facts,
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Authority {
    async fn check_authorization(
        &self,
        subject: Subject,
        _action_id: String,
        _details: HashMap<String, String>,
        _flags: u32,
        _cancellation_id: String,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> (bool, bool, HashMap<String, String>) {
        let name = subject
            .details
            .get("name")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok());
        let caller = match name.as_deref().map(zbus::names::BusName::try_from) {
            Some(Ok(n)) => match zbus::fdo::DBusProxy::new(conn).await {
                Ok(p) => p.get_connection_unix_user(n).await.ok(),
                Err(_) => None,
            },
            _ => None,
        };
        let session_uid: u32 = self
            .facts
            .read("uid")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let local_active = self.facts.read("remote").as_deref() != Some("1")
            && self.facts.read("active").as_deref() != Some("0");
        let ok = local_active && caller == Some(session_uid);
        (ok, !ok, HashMap::new())
    }
}

#[tokio::main]
async fn main() -> zbus::Result<()> {
    let addr = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    if addr.is_empty() || addr.contains("/run/dbus/system_bus_socket") {
        eprintln!("fake-session-bus: refusing to run without a private DBUS_SYSTEM_BUS_ADDRESS");
        std::process::exit(2);
    }
    let dir = PathBuf::from(std::env::var_os("FAKE_SESSION_DIR").unwrap_or_else(|| ".".into()));
    let authority = Authority {
        facts: Facts { dir: dir.clone() },
    };
    let _conn = zbus::connection::Builder::address(addr.as_str())?
        .name("org.freedesktop.login1")?
        .name("org.freedesktop.PolicyKit1")?
        .serve_at("/org/freedesktop/login1/seat/seat0", Seat)?
        .serve_at(
            SESSION_PATH,
            Session {
                facts: Facts { dir },
            },
        )?
        .serve_at("/org/freedesktop/PolicyKit1/Authority", authority)?
        .build()
        .await?;
    eprintln!("fake-session-bus: serving login1 + PolicyKit1");
    std::future::pending::<()>().await;
    Ok(())
}
