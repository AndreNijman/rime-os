//! Who may call rimed's unprivileged per-session methods (today `Ui.Boost`).
//!
//! The rule is one sentence: **the caller's uid must be the user of seat0's
//! active, local session.** No polkit round trip (a boost is asked for on every
//! animation, and polkit's answer for `allow_active` is this same fact), no
//! prompt, no root exception. A process of another user, a user whose session
//! is in the background (fast user switching), an ssh login, a system service:
//! all refused.
//!
//! The facts come from logind over D-Bus (`/run/systemd/sessions/*` say "do not
//! parse" in their own header). An unreachable logind refuses: fail closed.
//!
//! Two caches keep the cost per call to a hash lookup:
//! - sender → uid, from `GetConnectionUnixUser`. A unique bus name is never
//!   reused, and the entry is dropped when the name leaves the bus
//!   ([`CallerPolicy::forget`], driven by the `NameOwnerChanged` watch).
//! - the active session, for [`SESSION_TTL`]. A user switch is honoured within
//!   that time; the worst a stale entry allows is a boost of at most 600 ms for
//!   the user who was active 2 s ago.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::names::BusName;
use zbus::proxy::CacheProperties;
use zbus::{proxy, Connection};
use zvariant::OwnedObjectPath;

/// How long a reading of seat0's active session is trusted.
pub const SESSION_TTL: Duration = Duration::from_secs(2);

/// What logind says about seat0's active session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveSession {
    pub uid: u32,
    pub remote: bool,
    pub active: bool,
}

/// The rule, pure. `Err` carries the reason the caller is told.
pub fn decide(caller_uid: u32, session: Option<&ActiveSession>) -> Result<(), String> {
    let Some(s) = session else {
        return Err("no active session on seat0".into());
    };
    if s.remote {
        return Err("seat0's active session is remote".into());
    }
    if !s.active {
        return Err("seat0's session is not active".into());
    }
    if caller_uid != s.uid {
        return Err(format!(
            "uid {caller_uid} is not the user of the active session"
        ));
    }
    Ok(())
}

#[proxy(
    interface = "org.freedesktop.login1.Seat",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1/seat/seat0"
)]
trait Seat {
    #[zbus(property)]
    fn active_session(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn user(&self) -> zbus::Result<(u32, OwnedObjectPath)>;
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
}

/// Read seat0's active session from logind. `Ok(None)`: no session (a seat
/// at the greeter between sessions answers an empty id).
pub async fn read_active_session(conn: &Connection) -> zbus::Result<Option<ActiveSession>> {
    let seat = SeatProxy::builder(conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let (id, path) = seat.active_session().await?;
    if id.is_empty() || path.as_str() == "/" {
        return Ok(None);
    }
    let session = SessionProxy::builder(conn)
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let (uid, _) = session.user().await?;
    Ok(Some(ActiveSession {
        uid,
        remote: session.remote().await?,
        active: session.active().await?,
    }))
}

#[derive(Debug, Default)]
struct Cache {
    uids: HashMap<String, u32>,
    session: Option<(Instant, Option<ActiveSession>)>,
}

/// The caches plus the rule. One per daemon, in `Ctx`.
#[derive(Debug, Default)]
pub struct CallerPolicy {
    cache: std::sync::Mutex<Cache>,
}

/// Unique names remembered before the cache is simply cleared. Every name is
/// also dropped when it leaves the bus, so this bounds a leak, nothing more.
const MAX_UIDS: usize = 1024;

impl CallerPolicy {
    /// Admit `sender` or answer `AccessDenied` with the reason.
    pub async fn admit(&self, conn: &Connection, sender: &str) -> zbus::fdo::Result<()> {
        let uid = self.uid_of(conn, sender).await?;
        let session = self.active_session(conn).await?;
        decide(uid, session.as_ref()).map_err(zbus::fdo::Error::AccessDenied)
    }

    /// `sender` left the bus.
    pub fn forget(&self, sender: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.uids.remove(sender);
        }
    }

    async fn uid_of(&self, conn: &Connection, sender: &str) -> zbus::fdo::Result<u32> {
        if let Some(uid) = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.uids.get(sender).copied())
        {
            return Ok(uid);
        }
        let name = BusName::try_from(sender)
            .map_err(|e| zbus::fdo::Error::AccessDenied(format!("bad sender: {e}")))?;
        let uid = zbus::fdo::DBusProxy::new(conn)
            .await
            .map_err(|e| zbus::fdo::Error::AccessDenied(format!("bus unreachable: {e}")))?
            .get_connection_unix_user(name)
            .await
            .map_err(|e| zbus::fdo::Error::AccessDenied(format!("caller uid unknown: {e}")))?;
        if let Ok(mut c) = self.cache.lock() {
            if c.uids.len() >= MAX_UIDS {
                c.uids.clear();
            }
            c.uids.insert(sender.to_string(), uid);
        }
        Ok(uid)
    }

    async fn active_session(&self, conn: &Connection) -> zbus::fdo::Result<Option<ActiveSession>> {
        let now = Instant::now();
        if let Some((at, s)) = self.cache.lock().ok().and_then(|c| c.session) {
            if now.duration_since(at) < SESSION_TTL {
                return Ok(s);
            }
        }
        let s = read_active_session(conn)
            .await
            .map_err(|e| zbus::fdo::Error::AccessDenied(format!("logind unreachable: {e}")))?;
        if let Ok(mut c) = self.cache.lock() {
            c.session = Some((now, s));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: ActiveSession = ActiveSession {
        uid: 1000,
        remote: false,
        active: true,
    };

    #[test]
    fn the_active_local_sessions_user_is_admitted() {
        assert_eq!(decide(1000, Some(&LOCAL)), Ok(()));
    }

    #[test]
    fn everyone_else_is_refused() {
        // Another user, root included: no exception for uid 0.
        assert!(decide(1001, Some(&LOCAL)).is_err());
        assert!(decide(0, Some(&LOCAL)).is_err());
        // No session on the seat (the greeter between logins).
        assert!(decide(1000, None).is_err());
        // The right user, but over ssh / in the background.
        assert!(decide(
            1000,
            Some(&ActiveSession {
                remote: true,
                ..LOCAL
            })
        )
        .is_err());
        assert!(decide(
            1000,
            Some(&ActiveSession {
                active: false,
                ..LOCAL
            })
        )
        .is_err());
    }

    #[test]
    fn forget_drops_a_cached_sender() {
        let p = CallerPolicy::default();
        p.cache.lock().unwrap().uids.insert(":1.5".into(), 1000);
        p.forget(":1.5");
        assert!(p.cache.lock().unwrap().uids.is_empty());
    }
}
