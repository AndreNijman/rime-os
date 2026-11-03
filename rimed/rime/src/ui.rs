//! `rime ui` — the input-triggered UI boost from the command line, and the
//! Hyprland watcher that asks for it on every window and workspace change.
//!
//! rimed decides everything (cap, coalescing, rate limit, which tiers are
//! boosted, the exact restore; see docs/rimed-dbus.md, `org.rimeos.Rimed1.Ui`).
//! This side only asks, and asks without waiting for an answer, so a
//! compositor event is never held up by the bus.

use std::time::{Duration, Instant};

use clap::Subcommand;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::proxy::UiProxy;

#[derive(Subcommand, Debug)]
pub enum UiCmd {
    /// Ask rimed for a UI boost of `ms` milliseconds (rimed caps it at 600),
    /// print how long it lasts (0 means nothing was boosted) and stay on the
    /// bus until it ends: rimed ends a boost early when its caller leaves.
    Boost {
        /// Milliseconds; 0 gives this caller's share back.
        ms: u32,
    },
    /// Follow Hyprland's event socket and ask for a boost on every window
    /// open/close, workspace switch and fullscreen change. Started by the
    /// Hyprland session; does nothing outside one.
    HyprBoost {
        /// Milliseconds asked per event (rimed caps it at 600).
        #[arg(long, default_value_t = HYPR_BOOST_MS)]
        ms: u32,
    },
}

/// One Hyprland window animation (`rimeArrive`, speed 4.8 = 480 ms) plus a
/// frame of slack.
pub const HYPR_BOOST_MS: u32 = 500;

/// The least time between two asks. A workspace switch emits several events
/// in the same frame (`workspace`, `workspacev2`, `focusedmon`, ...); rimed
/// coalesces them anyway, this keeps the watcher well inside rimed's per-caller
/// rate (20 a second) so a scrub is never answered `LimitsExceeded`.
pub const MIN_GAP: Duration = Duration::from_millis(100);

/// Whether a socket2 line starts an animation worth a boost.
pub fn wants_boost(line: &str) -> bool {
    let event = line.split_once(">>").map_or(line, |(e, _)| e);
    matches!(
        event,
        "openwindow"
            | "closewindow"
            | "workspace"
            | "workspacev2"
            | "activespecial"
            | "activespecialv2"
            | "fullscreen"
            | "movewindow"
            | "movewindowv2"
    )
}

/// Client-side throttle: at most one ask per [`MIN_GAP`].
#[derive(Debug, Default)]
pub struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|l| now.duration_since(l) < MIN_GAP) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

pub async fn main(cmd: UiCmd) -> i32 {
    match cmd {
        UiCmd::Boost { ms } => boost(ms).await,
        UiCmd::HyprBoost { ms } => hypr_boost(ms).await,
    }
}

async fn proxy() -> Result<UiProxy<'static>, String> {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| format!("system bus unreachable: {e}"))?;
    UiProxy::new(&conn)
        .await
        .map_err(|e| format!("rimed unreachable: {e}"))
}

async fn boost(ms: u32) -> i32 {
    let p = match proxy().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime ui boost: {e}");
            return 1;
        }
    };
    match p.boost(ms).await {
        Ok(got) => {
            let lever = p.lever().await.unwrap_or_default();
            if got == 0 {
                println!(
                    "not boosted (0 ms){}",
                    if lever.is_empty() {
                        ": this tier is not boosted"
                    } else {
                        ""
                    }
                );
            } else {
                println!("boosted for {got} ms: {lever}");
                tokio::time::sleep(Duration::from_millis(u64::from(got))).await;
            }
            0
        }
        Err(e) => {
            eprintln!("rime ui boost: {e}");
            1
        }
    }
}

/// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`.
fn hypr_socket() -> Option<std::path::PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())?;
    let run = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty())?;
    Some(
        std::path::Path::new(&run)
            .join("hypr")
            .join(sig)
            .join(".socket2.sock"),
    )
}

async fn hypr_boost(ms: u32) -> i32 {
    let Some(sock) = hypr_socket() else {
        eprintln!("rime ui hypr-boost: not inside a Hyprland session; nothing to watch");
        return 0;
    };
    let stream = match tokio::net::UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime ui hypr-boost: {}: {e}", sock.display());
            return 1;
        }
    };
    let p = match proxy().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime ui hypr-boost: {e}");
            return 1;
        }
    };
    let mut lines = BufReader::new(stream).lines();
    let mut throttle = Throttle::default();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if wants_boost(&line) && throttle.allow(Instant::now()) {
                    // Fire and forget: a refused ask (rate, cooldown, a tier
                    // that is not boosted) is rimed's answer, not an error here.
                    let _ = p.boost_no_reply(ms).await;
                }
            }
            // Hyprland exited: the session is over.
            Ok(None) => return 0,
            Err(e) => {
                eprintln!("rime ui hypr-boost: reading {}: {e}", sock.display());
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_events_ask_and_the_rest_do_not() {
        for l in [
            "openwindow>>80a6f50,2,kitty,Kitty",
            "closewindow>>80a6f50",
            "workspace>>2",
            "workspacev2>>2,2",
            "activespecial>>special:term,DP-1",
            "fullscreen>>1",
        ] {
            assert!(wants_boost(l), "{l}");
        }
        for l in [
            "activewindow>>kitty,~",
            "focusedmon>>DP-1,2",
            "windowtitle>>80a6f50",
            "openwindowx>>1",
            "",
        ] {
            assert!(!wants_boost(l), "{l}");
        }
    }

    #[test]
    fn the_throttle_lets_one_ask_through_per_gap() {
        let mut t = Throttle::default();
        let t0 = Instant::now();
        assert!(t.allow(t0));
        assert!(!t.allow(t0 + Duration::from_millis(10)));
        assert!(!t.allow(t0 + Duration::from_millis(99)));
        assert!(t.allow(t0 + MIN_GAP));
    }
}
