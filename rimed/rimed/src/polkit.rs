//! Polkit authorization for the privileged D-Bus methods.
//!
//! The daemon runs as root on the system bus; the D-Bus policy lets any local
//! user *send* to it, and polkit decides whether the caller may actually act.
//! The shipped policy grants `allow_active = yes` (passwordless) so the
//! logged-in user's shell can flip tiers and charge thresholds without a
//! prompt, while inactive/remote callers need admin auth.

use std::collections::HashMap;

use zbus::message::Header;
use zbus::{proxy, Connection};
use zvariant::{OwnedValue, Type, Value};

/// Polkit action for power/tier changes.
pub const ACTION_POWER: &str = "org.rimeos.rimed.manage-power";
/// Polkit action for battery/charge changes.
pub const ACTION_BATTERY: &str = "org.rimeos.rimed.manage-battery";

#[derive(Type, serde::Serialize)]
struct Subject {
    kind: String,
    details: HashMap<String, OwnedValue>,
}

#[derive(Type, serde::Deserialize)]
struct AuthResult {
    is_authorized: bool,
    #[allow(dead_code)]
    is_challenge: bool,
    #[allow(dead_code)]
    details: HashMap<String, String>,
}

#[proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &Subject,
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<AuthResult>;
}

/// Authorize `action` for the D-Bus caller in `hdr`. `Ok(())` means allowed;
/// otherwise an `AccessDenied` fdo error (also on any polkit failure — fail
/// closed).
pub async fn authorize(
    conn: &Connection,
    hdr: &Header<'_>,
    action: &str,
) -> zbus::fdo::Result<()> {
    check(conn, hdr, action, 0).await
}

/// [`authorize`], letting polkit ask the caller's authentication agent (an `auth_admin_keep` action such as
/// `Updates.Apply`: the desktop shows its password prompt while the call waits).
pub async fn authorize_interactive(
    conn: &Connection,
    hdr: &Header<'_>,
    action: &str,
) -> zbus::fdo::Result<()> {
    check(conn, hdr, action, 1).await
}

async fn check(conn: &Connection, hdr: &Header<'_>, action: &str, flags: u32) -> zbus::fdo::Result<()> {
    let sender = hdr
        .sender()
        .map(|s| s.to_string())
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("no sender on message".into()))?;

    let mut details: HashMap<String, OwnedValue> = HashMap::new();
    if let Ok(v) = Value::from(sender).try_to_owned() {
        details.insert("name".to_string(), v);
    }
    let subject = Subject {
        kind: "system-bus-name".to_string(),
        details,
    };

    let authority = AuthorityProxy::new(conn)
        .await
        .map_err(|e| zbus::fdo::Error::AccessDenied(format!("polkit unreachable: {e}")))?;

    // flags = 0: no interactive prompt — active users are pre-authorized by the
    // shipped policy, everyone else is simply denied (never a hang). 1 only
    // from authorize_interactive.
    let result = authority
        .check_authorization(&subject, action, HashMap::new(), flags, "")
        .await
        .map_err(|e| zbus::fdo::Error::AccessDenied(format!("polkit check failed: {e}")))?;

    if result.is_authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "not authorized for {action}"
        )))
    }
}
