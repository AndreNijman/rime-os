//! What Rime's COSMIC applet shows, read from rimed on the system bus.
//!
//! The whole D-Bus side of the applet: `cosmic-applet-rime` only draws the [`View`] this sends and calls the two
//! actions, and `tests/fake_rimed.rs` runs this crate alone against a fake rimed on a private bus.
//!
//! The contract (docs/nixos/desktops.md §6.2), all on `org.rimeos.Rimed1` at `/org/rimeos/Rimed1`:
//!
//! | interface                    | properties                                  | methods          |
//! |------------------------------|---------------------------------------------|------------------|
//! | `org.rimeos.Rimed1.Mode`     | `Active` (s daily/gaming), `Held` (s)       | `Set(s)`         |
//! | `org.rimeos.Rimed1.GameMode` | `Active` (b)                                |                  |
//! | `org.rimeos.Rimed1.Updates`  | `State`, `Available`, `Running`, `Message`  | `Check`, `Apply` |
//!
//! Each interface is read once with `GetAll` when rimed appears on the bus (and again when it restarts); from then on
//! only `org.freedesktop.DBus.Properties.PropertiesChanged` updates it. Nothing here runs on a timer.
//!
//! No power-profile control: COSMIC's own power applet drives rimed's power-profiles API (§7).

use std::collections::HashMap;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::zvariant::Value;
use zbus::{Connection, fdo, names::InterfaceName};

pub const BUS_NAME: &str = "org.rimeos.Rimed1";
pub const OBJECT_PATH: &str = "/org/rimeos/Rimed1";
pub const MODE: &str = "org.rimeos.Rimed1.Mode";
pub const GAME_MODE: &str = "org.rimeos.Rimed1.GameMode";
pub const UPDATES: &str = "org.rimeos.Rimed1.Updates";
const INTERFACES: [&str; 3] = [MODE, GAME_MODE, UPDATES];

/// Update states the item is shown in. Up to date, unknown, failed and deferred hide it: Rime Settings' Updates page
/// says why an update failed or waits; the panel is for an action the user can take now.
const UPDATE_SHOWN: [&str; 4] = ["available", "downloading", "installing", "restart"];

/// What the applet draws.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub rimed: bool,
    pub gaming_checked: bool,
    pub gaming_subtitle: String,
    pub update_visible: bool,
    pub update_sensitive: bool,
    pub update_state: String,
    pub update_title: String,
    pub update_subtitle: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Str(String),
    Bool(bool),
}

fn val(v: &Value<'_>) -> Option<Val> {
    match v {
        Value::Str(s) => Some(Val::Str(s.to_string())),
        Value::Bool(b) => Some(Val::Bool(*b)),
        Value::Value(inner) => val(inner),
        _ => None,
    }
}

/// rimed's properties as last read or told: `"<interface>.<property>"` -> value.
#[derive(Clone, Debug, Default)]
pub struct State {
    rimed: bool,
    props: HashMap<String, Val>,
}

impl State {
    fn str(&self, iface: &str, prop: &str) -> &str {
        match self.props.get(&format!("{iface}.{prop}")) {
            Some(Val::Str(s)) => s,
            _ => "",
        }
    }

    fn bool(&self, iface: &str, prop: &str) -> bool {
        matches!(self.props.get(&format!("{iface}.{prop}")), Some(Val::Bool(true)))
    }

    fn merge<'a>(&mut self, iface: &str, changed: impl IntoIterator<Item = (&'a str, &'a Value<'a>)>) {
        if !INTERFACES.contains(&iface) {
            return;
        }
        for (k, v) in changed {
            if let Some(v) = val(v) {
                self.props.insert(format!("{iface}.{k}"), v);
            }
        }
    }

    pub fn view(&self) -> View {
        let rimed = self.rimed;
        let state = if rimed && !self.str(UPDATES, "State").is_empty() {
            self.str(UPDATES, "State").to_string()
        } else {
            "unknown".to_string()
        };
        let release = self.str(UPDATES, "Available");
        let message = self.str(UPDATES, "Message");
        let (title, subtitle) = match state.as_str() {
            "available" => (
                "Update".to_string(),
                if release.is_empty() { "Rime".to_string() } else { format!("Rime {release}") },
            ),
            "downloading" | "installing" => (
                "Updating".to_string(),
                if !message.is_empty() {
                    message.to_string()
                } else if !release.is_empty() {
                    format!("Rime {release}")
                } else {
                    String::new()
                },
            ),
            "restart" => (
                "Restart to finish".to_string(),
                if release.is_empty() { String::new() } else { format!("Rime {release}") },
            ),
            _ => ("Update".to_string(), String::new()),
        };
        View {
            rimed,
            gaming_checked: rimed && self.str(MODE, "Active") == "gaming",
            gaming_subtitle: if rimed && self.bool(GAME_MODE, "Active") {
                "Game running".to_string()
            } else {
                String::new()
            },
            update_visible: rimed && UPDATE_SHOWN.contains(&state.as_str()),
            update_sensitive: state == "available" || state == "restart",
            update_state: state,
            update_title: title,
            update_subtitle: subtitle,
        }
    }

    /// Reads everything again: rimed appeared, restarted or went away.
    async fn reload(&mut self, dbus: &fdo::DBusProxy<'_>, props: &fdo::PropertiesProxy<'_>) {
        self.props.clear();
        self.rimed = dbus
            .name_has_owner(BUS_NAME.try_into().expect("bus name"))
            .await
            .unwrap_or(false);
        if !self.rimed {
            return;
        }
        for iface in INTERFACES {
            match props.get_all(InterfaceName::from_static_str_unchecked(iface)).await {
                Ok(all) => self.merge(iface, all.iter().map(|(k, v)| (k.as_str(), &**v))),
                Err(e) => tracing::warn!("rime: GetAll {iface} failed: {e}"),
            }
        }
    }
}

/// Follows rimed until the bus connection ends, calling `on_change` with the view once at the start and after every
/// change rimed announces.
pub async fn follow(conn: &Connection, mut on_change: impl FnMut(View) + Send) -> zbus::Result<()> {
    let dbus = fdo::DBusProxy::new(conn).await?;
    let mut owners = dbus.receive_name_owner_changed_with_args(&[(0, BUS_NAME)]).await?;
    let props = fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    // Every property change rimed makes (the one place the model learns of changes).
    let mut changes: BoxStream<'_, fdo::PropertiesChanged> = props.receive_properties_changed().await?.boxed();

    let mut state = State::default();
    state.reload(&dbus, &props).await;
    on_change(state.view());

    loop {
        tokio::select! {
            Some(sig) = changes.next() => {
                if let Ok(args) = sig.args() {
                    state.merge(args.interface_name().as_str(), args.changed_properties().iter().map(|(k, v)| (*k, v)));
                    on_change(state.view());
                }
            }
            Some(_) = owners.next() => {
                state.reload(&dbus, &props).await;
                on_change(state.view());
            }
            else => return Ok(()),
        }
    }
}

async fn proxy<'a>(conn: &'a Connection, iface: &'static str) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::proxy::Builder::<zbus::Proxy<'a>>::new(conn)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .interface(iface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// The Gaming toggle: Daily <-> Gaming through rimed's own composition (`Mode.Set`). The toggle does not flip itself;
/// it shows `Mode.Active` once rimed says it changed.
pub async fn set_gaming(conn: &Connection, on: bool) -> zbus::Result<()> {
    // Interactive: rimed's polkit check may ask the desktop's agent.
    proxy(conn, MODE)
        .await?
        .call_with_flags::<_, _, ()>("Set", MethodFlags::AllowInteractiveAuth.into(), &(if on { "gaming" } else { "daily" },))
        .await?;
    Ok(())
}

/// The Update item while an update is available.
pub async fn apply_update(conn: &Connection) -> zbus::Result<()> {
    // Interactive: Updates.Apply is auth_admin_keep, so rimed's polkit check asks the desktop's agent.
    proxy(conn, UPDATES)
        .await?
        .call_with_flags::<_, _, ()>("Apply", MethodFlags::AllowInteractiveAuth.into(), &())
        .await?;
    Ok(())
}
