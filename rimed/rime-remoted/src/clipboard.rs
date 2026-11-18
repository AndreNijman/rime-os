//! The computer's clipboard on a phone (contract §2.14).
//!
//! The other direction of §1.8's `clipboard_write`. This daemon never reads
//! the clipboard itself: it is a service, and the clipboard belongs to the
//! graphical session. The Rime Shell watches it, while its "Send my clipboard
//! to my phone" switch is on, and hands each new text to `rime remote
//! clipboard` on stdin. That request comes over the control socket and is
//! written, as a `clipboard` line, onto the `device_events` channel of every
//! connected phone whose report says `"clipboard"` in `can` (the person's own
//! switch on the phone, and the app unlocked).
//!
//! ## The loop guard
//!
//! Text a phone just put on this computer's clipboard with `clipboard_write`
//! changes the clipboard, so the Shell sees it and sends it on. Back to that
//! same phone it would only be noise (and on a phone that shows a "copied"
//! notice, a visible echo), so [`Echo`] remembers the last `clipboard_write`
//! text per device for [`ECHO_FOR`], and a `clipboard` request whose text is
//! byte for byte that text skips that device. Other phones still get it: that
//! is the point of a shared clipboard.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// What `remote_hello` adds once this machine sends its clipboard to phones
/// that ask for it. A phone says `"clipboard"` in `can` only to such a
/// machine; an older one drops the word anyway.
pub const FEATURE: &str = "phone_clipboard";

/// The most text one `clipboard` line carries, in bytes of UTF-8. Larger is
/// refused rather than cut: half a paste is worse than none.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The longest `clipboard` event line, newline included. JSON escapes a quote
/// or a newline to two bytes and other control characters to six, so text
/// within [`MAX_TEXT_BYTES`] can still make a line too long for a phone's
/// event reader (contract §2.14: 256 KiB); one that would is refused here.
pub const MAX_LINE: usize = 192 * 1024;

/// How long a phone's own `clipboard_write` text is not sent back to it.
pub const ECHO_FOR: Duration = Duration::from_secs(10);

/// How long one phone's line may take to go into its events channel before
/// the channel is given up as stalled (see [`crate::phone::Events::send_whole`]).
pub const WRITE_LIMIT: Duration = Duration::from_secs(2);

/// The event line for `text`, or why it is not sent. `sensitive` is carried
/// only when true, so the phone marks the clip as sensitive (a password
/// manager's copy, as the caller judged it).
pub fn line(text: &str, sensitive: bool) -> Result<Value, String> {
    if text.is_empty() {
        return Err("the clipboard holds no text".to_string());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(format!(
            "that text is {} bytes, and a clipboard is sent to a phone only up to {MAX_TEXT_BYTES}",
            text.len()
        ));
    }
    let mut v = json!({"event": "clipboard", "text": text});
    if sensitive {
        v["sensitive"] = json!(true);
    }
    let encoded = v.to_string().len() + 1;
    if encoded > MAX_LINE {
        return Err(format!(
            "that text is {encoded} bytes once escaped for the phone, more than {MAX_LINE}: it is mostly \
             control characters"
        ));
    }
    Ok(v)
}

/// The text of a phone's `clipboard_write` line (§1.8), or `None` for any
/// other line. Read only to remember it: the line itself is still forwarded
/// to `rime-agentd` untouched, which decides whether it is allowed.
pub fn phone_wrote(line: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("cmd")?.as_str()? != "clipboard_write" {
        return None;
    }
    v.get("text")?.as_str().map(str::to_string)
}

/// The last text each phone put on this computer's clipboard, and when.
#[derive(Default)]
pub struct Echo {
    last: Mutex<HashMap<String, (String, Instant)>>,
}

impl Echo {
    /// `device_id` asked to set this computer's clipboard to `text`.
    pub fn wrote(&self, device_id: &str, text: String) {
        self.wrote_at(device_id, text, Instant::now());
    }

    fn wrote_at(&self, device_id: &str, text: String, at: Instant) {
        lock(&self.last).insert(device_id.to_string(), (text, at));
    }

    /// Whether sending `text` to `device_id` would only hand it back what it
    /// put here less than [`ECHO_FOR`] ago.
    pub fn is_echo(&self, device_id: &str, text: &str) -> bool {
        self.is_echo_at(device_id, text, Instant::now())
    }

    fn is_echo_at(&self, device_id: &str, text: &str, now: Instant) -> bool {
        let mut last = lock(&self.last);
        match last.get(device_id) {
            Some((_, at)) if now.saturating_duration_since(*at) >= ECHO_FOR => {
                last.remove(device_id);
                false
            }
            Some((t, _)) => t == text,
            None => false,
        }
    }

    /// Revoke.
    pub fn forget(&self, device_id: &str) {
        lock(&self.last).remove(device_id);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_carries_the_text_verbatim_and_sensitive_only_when_true() {
        let v = line("  two\nlines \u{1F600}  ", false).expect("line");
        assert_eq!(v, json!({"event": "clipboard", "text": "  two\nlines \u{1F600}  "}));
        assert!(v.get("sensitive").is_none(), "{v}");
        assert_eq!(line("pw", true).expect("line")["sensitive"], true);
    }

    #[test]
    fn empty_too_long_and_too_long_once_escaped_are_refused() {
        assert!(line("", false).is_err());
        let most = "a".repeat(MAX_TEXT_BYTES);
        assert!(line(&most, false).is_ok(), "exactly the cap is sent");
        assert!(line(&format!("{most}a"), false).unwrap_err().contains("bytes"));
        // Multi-byte characters count as their bytes, not as characters.
        let wide = "\u{00E9}".repeat(MAX_TEXT_BYTES / 2 + 1);
        assert!(line(&wide, false).is_err());
        // Quotes double: at the cap that is a line of 128 KiB, which fits.
        assert!(line(&"\"".repeat(MAX_TEXT_BYTES), false).is_ok());
        // Other control characters become six bytes each: that does not.
        let controls = "\u{1}".repeat(MAX_TEXT_BYTES);
        assert!(line(&controls, false).unwrap_err().contains("escaped"));
        // The boundary is the line with its newline: exactly MAX_LINE goes,
        // one byte more does not.
        let overhead = json!({"event": "clipboard", "text": ""}).to_string().len();
        let extra = MAX_LINE - 1 - overhead - MAX_TEXT_BYTES; // what escaping must add
        let (controls, quotes) = (extra / 5, extra % 5);
        let at = |more: usize| {
            format!(
                "{}{}{}",
                "\u{1}".repeat(controls),
                "\"".repeat(quotes + more),
                "a".repeat(MAX_TEXT_BYTES - controls - quotes - more)
            )
        };
        let v = line(&at(0), false).expect("a line of exactly MAX_LINE");
        assert_eq!(v.to_string().len() + 1, MAX_LINE);
        assert!(line(&at(1), false).is_err(), "one byte over MAX_LINE");
    }

    #[test]
    fn only_a_clipboard_write_is_remembered() {
        assert_eq!(phone_wrote(br#"{"cmd":"clipboard_write","text":"a b\n"}"#).as_deref(), Some("a b\n"));
        assert_eq!(phone_wrote(br#"{"cmd":"clipboard"}"#), None);
        assert_eq!(phone_wrote(br#"{"cmd":"clipboard_write"}"#), None);
        assert_eq!(phone_wrote(br#"{"cmd":"clipboard_write","text":7}"#), None);
        assert_eq!(phone_wrote(b"not json"), None);
    }

    #[test]
    fn a_phones_own_text_is_not_sent_back_to_it_for_ten_seconds() {
        let echo = Echo::default();
        let t0 = Instant::now();
        echo.wrote_at("pixel", "hello".into(), t0);
        assert!(echo.is_echo_at("pixel", "hello", t0 + Duration::from_secs(9)));
        assert!(!echo.is_echo_at("pixel", "hello!", t0), "other text goes");
        assert!(!echo.is_echo_at("tablet", "hello", t0), "other phones get it");
        assert!(!echo.is_echo_at("pixel", "hello", t0 + ECHO_FOR), "after ten seconds it is news again");
        echo.wrote_at("pixel", "x".into(), t0);
        echo.forget("pixel");
        assert!(!echo.is_echo_at("pixel", "x", t0));
    }
}
