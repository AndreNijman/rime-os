//! Thin zbus client proxies for the frozen `org.rimeos.Rimed1` surface, plus
//! helpers for connecting and probing whether the daemon is actually running.

use zbus::proxy;

pub const BUS_NAME: &str = "org.rimeos.Rimed1";

#[proxy(
    interface = "org.rimeos.Rimed1.Power",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Power {
    fn set_tier(&self, tier: &str) -> zbus::Result<()>;
    fn set_auto_switch(&self, enabled: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn tier(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn tiers(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn on_ac_power(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn auto_switch(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.rimeos.Rimed1.Battery",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Battery {
    fn set_charge_thresholds(&self, start: u8, end: u8) -> zbus::Result<()>;
    fn set_travel_mode(&self, enabled: bool) -> zbus::Result<()>;
    fn calibrate(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn charge_start(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn charge_end(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn travel_mode(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn capacity(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.rimeos.Rimed1.Profile",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Profile {
    #[zbus(property)]
    fn active(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn device(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.rimeos.Rimed1.Metrics",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Metrics {
    /// The `a{sv}` telemetry snapshot: `tier`, `on_ac`, and whichever of
    /// `ppt_watts`, `battery_uwh` and `temp_<zone>` this machine can report.
    #[zbus(property)]
    fn snapshot(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
}

#[proxy(
    interface = "org.rimeos.Rimed1.Fan",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Fan {
    fn set_mode(&self, mode: &str) -> zbus::Result<()>;
    fn set_pwm(&self, pwm: u8) -> zbus::Result<()>;
    fn restore_firmware(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn mode(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn supported(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn modes(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn pwm(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn fans(&self) -> zbus::Result<Vec<std::collections::HashMap<String, zvariant::OwnedValue>>>;
}

#[proxy(
    interface = "org.rimeos.Rimed1.GameMode",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait GameMode {
    fn set_active(&self, active: bool) -> zbus::Result<()>;
    fn start_for_pid(&self, pid: u32) -> zbus::Result<()>;
    /// Enter game mode and hand rimed the process whose death ends the
    /// session. The owner is watched, not pinned — see `rimed/src/dbus.rs`.
    fn start_owned_by(&self, owner_pid: u32) -> zbus::Result<()>;
    fn attach_pid(&self, pid: u32) -> zbus::Result<()>;
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn supported(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn status(&self) -> zbus::Result<std::collections::HashMap<String, zvariant::OwnedValue>>;
}

/// `org.rimeos.Rimed1.Mode` (2026-10-10): the mode rimed keeps across
/// restarts. Absent on an older rimed, which callers must tolerate.
#[proxy(
    interface = "org.rimeos.Rimed1.Mode",
    default_service = "org.rimeos.Rimed1",
    default_path = "/org/rimeos/Rimed1"
)]
pub trait Mode {
    fn hold(&self, mode: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn held(&self) -> zbus::Result<String>;
}

/// Connect to the system bus, returning None (never panicking) if the bus
/// itself is unreachable.
pub async fn connect() -> Option<zbus::Connection> {
    zbus::Connection::system().await.ok()
}

/// True when `rimed` currently owns its well-known name on the bus.
pub async fn daemon_running(conn: &zbus::Connection) -> bool {
    match zbus::fdo::DBusProxy::new(conn).await {
        Ok(dbus) => dbus
            .name_has_owner(BUS_NAME.try_into().unwrap())
            .await
            .unwrap_or(false),
        Err(_) => false,
    }
}
