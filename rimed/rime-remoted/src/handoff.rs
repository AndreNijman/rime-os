//! Handoff cards between the owner's paired computers (contract §2.15, M3b).
//!
//! `rime remote computers open <computer> <url>` offers a web link to another
//! computer ([`send`]). That computer shows it as a card, a notification
//! "Open a link from <Laptop>?" whose body names the link's host first, with
//! one button, Open ([`answer`]). The link is opened only when a person
//! presses Open ([`on_action`]): dismissing the card, letting it expire (ten
//! minutes), removing the computer or turning its switch off drops it, and
//! nothing is ever opened by itself.
//!
//! ## Everything is checked, on both sides
//!
//! The sender may be a paired computer that was stolen, so the receiver
//! trusts nothing on the line ([`checked_url`], [`checked_title`]) and the
//! sender checks the same before it dials. A link is `http` or `https`
//! (parsed, not prefix-matched; the scheme in any case), with a host and no
//! user name or password, at most [`MAX_URL`] bytes, with no space, control
//! character or invisible or direction-changing character; the host is plain
//! ASCII (no lookalike letters), and never this computer's own loopback (the
//! sender's `localhost` is not this one's, so such a card could only aim at
//! a service here). The title is plain text, at most [`MAX_TITLE`]
//! characters. Anything else is refused whole.
//!
//! The name the card shows is this computer's record of the sender, never a
//! field of the line, and the host is the one the same parse found, so what
//! the card says is what Open opens. What Open opens comes from this
//! process's own table, by the card's number, never from the notification.
//!
//! ## Flood
//!
//! At most [`MAX_PENDING`] cards wait from one computer and one is taken
//! every [`SPACING`]; past either the sender is told `busy`. The same link
//! from the same computer while its card waits replaces that card. Cards are
//! kept in memory only.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control::Reply;
use crate::mirror::{Job, Shown};
use crate::state::State;

/// The peer verb that carries one card.
pub const PEER_VERB: &str = "peer_handoff";
/// The longest link taken, in bytes.
pub const MAX_URL: usize = 2048;
/// The longest title taken, in characters.
pub const MAX_TITLE: usize = 200;
/// Cards waiting from one computer at most.
pub const MAX_PENDING: usize = 3;
/// One card taken from a computer at most this often.
pub const SPACING: Duration = Duration::from_secs(2);
/// How long a card waits for Open.
pub const LIFETIME: Duration = Duration::from_secs(600);
/// The notification key prefix and its one button.
pub const KEY_PREFIX: &str = "handoff:";
pub const OPEN_ACTION: &str = "open";
/// How much of the link the card shows, in characters.
const URL_SHOWN: usize = 160;

/// A link that passed [`checked_url`]: what Open opens, and the host the
/// card names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The link as given, with its scheme in lower case.
    pub url: String,
    /// The host in lower case, with the port when one was given.
    pub host: String,
}

/// Whether `c` may never appear in a link or a title: a control character
/// (C0, DEL, C1), or an invisible or direction-changing one.
fn hidden(c: char) -> bool {
    c.is_control() || crate::files::is_format(c)
}

/// A link another computer offered, or why it is refused.
pub fn checked_url(raw: &str) -> Result<Link, String> {
    if raw.len() > MAX_URL {
        return Err(format!("a link may be at most {MAX_URL} bytes"));
    }
    if raw.chars().any(hidden) {
        return Err("a link may not hold a control, invisible or direction-changing character".into());
    }
    if raw.chars().any(|c| c.is_whitespace() || matches!(c, '\\' | '<' | '>' | '"')) {
        return Err("a link may not hold a space, a backslash, < > or \"".into());
    }
    let Some((scheme, rest)) = raw.split_once(':') else {
        return Err("only http and https links are offered".into());
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("only http and https links are offered".into());
    }
    let Some(after) = rest.strip_prefix("//") else {
        return Err("a link needs // and a host after its scheme".into());
    };
    let authority = &after[..after.find(['/', '?', '#']).unwrap_or(after.len())];
    if authority.contains('@') {
        return Err("a link may not carry a user name or password".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let Some((inside, tail)) = v6.split_once(']') else {
            return Err("a link's IPv6 address is not closed with ]".into());
        };
        let ip: Ipv6Addr = inside.parse().map_err(|_| "a link's IPv6 address does not parse".to_string())?;
        if ip.is_loopback() || ip.is_unspecified() || ip.to_ipv4_mapped().is_some_and(own_v4) {
            return Err("a link may not point at this computer itself".into());
        }
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':').ok_or("a link's address is followed by something that is not a port")?),
        };
        (format!("[{ip}]"), port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        };
        (checked_host(host)?, port)
    };
    let shown = match port {
        None => host,
        Some(p) => {
            let n: u32 = (!p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
                .filter(|n| (1..=65535).contains(n))
                .ok_or("a link's port must be a number from 1 to 65535")?;
            format!("{host}:{n}")
        }
    };
    Ok(Link { url: format!("{scheme}:{rest}"), host: shown })
}

/// A loopback or "this network" IPv4 address: this computer itself.
fn own_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.octets()[0] == 0
}

/// A host name or IPv4 address, in lower case, or why it is refused.
fn checked_host(host: &str) -> Result<String, String> {
    if host.is_empty() {
        return Err("a link needs a host".into());
    }
    if host.len() > 253 || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
        return Err("a link's host must be plain letters, digits, dots and hyphens (no lookalike letters)".into());
    }
    let host = host.to_ascii_lowercase();
    let bare = host.strip_suffix('.').unwrap_or(&host);
    let labels: Vec<&str> = bare.split('.').collect();
    if labels.iter().any(|l| l.is_empty() || l.len() > 63) {
        return Err("a link's host has an empty or overlong part".into());
    }
    // A browser reads a host whose last part is a number as an IPv4
    // address, in forms like `0x7f.1` or `2130706433`; only the plain
    // dotted form is taken, so the card shows the address that opens.
    let last = labels.last().copied().unwrap_or("");
    let numeric = last.bytes().all(|b| b.is_ascii_digit())
        || (last.starts_with("0x") && last[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    if numeric {
        let ip: Ipv4Addr = host
            .parse()
            .ok()
            .filter(|ip: &Ipv4Addr| ip.to_string() == host)
            .ok_or("a link's IPv4 address must be written as four plain numbers, like 192.168.1.20")?;
        if own_v4(ip) {
            return Err("a link may not point at this computer itself".into());
        }
    } else if bare == "localhost" || bare.ends_with(".localhost") {
        return Err("a link may not point at this computer itself".into());
    }
    Ok(host)
}

/// A card's title, or why it is refused: plain text, with no control,
/// invisible or direction-changing character, at most [`MAX_TITLE`]
/// characters. Empty is none.
pub fn checked_title(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(t) = raw else { return Ok(None) };
    if t.chars().any(hidden) {
        return Err("a title may not hold a control, invisible or direction-changing character".into());
    }
    if t.chars().count() > MAX_TITLE {
        return Err(format!("a title may be at most {MAX_TITLE} characters"));
    }
    let t = t.trim();
    Ok((!t.is_empty()).then(|| t.to_string()))
}

/// A `peer_handoff` line's link and title, checked whole.
fn parse_card(v: &Value) -> Result<(Link, Option<String>), String> {
    let url = v["url"].as_str().ok_or("url must be text")?;
    let title = match &v["title"] {
        Value::Null => None,
        Value::String(s) => Some(s.as_str()),
        _ => return Err("title must be text".into()),
    };
    Ok((checked_url(url)?, checked_title(title)?))
}

/// The cards waiting for Open: memory only.
#[derive(Default)]
pub struct Handoff {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    cards: Vec<Card>,
    /// When each computer's last card was taken.
    last: HashMap<String, Instant>,
    next: u64,
}

#[derive(Debug, Clone)]
struct Card {
    n: u64,
    from: String,
    url: String,
    at: Instant,
}

/// What [`Handoff::admit`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// A new card, number `n`.
    New(u64),
    /// The same link was waiting from that computer: its card `n` again.
    Replace(u64),
    /// Too many waiting, or too soon after the last.
    Busy,
}

impl Handoff {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take out the cards past their life: `(computer, n)` of each, for the
    /// caller to close.
    fn expire(g: &mut Inner, now: Instant) -> Vec<(String, u64)> {
        let (old, kept): (Vec<Card>, Vec<Card>) =
            g.cards.drain(..).partition(|c| now.saturating_duration_since(c.at) >= LIFETIME);
        g.cards = kept;
        old.into_iter().map(|c| (c.from, c.n)).collect()
    }

    /// Room for a card from computer `from` for `url`, and the cards that
    /// expired meanwhile.
    pub fn admit(&self, from: &str, url: &str, now: Instant) -> (Admit, Vec<(String, u64)>) {
        let mut g = self.lock();
        let expired = Handoff::expire(&mut g, now);
        if g.last.get(from).is_some_and(|t| now.saturating_duration_since(*t) < SPACING) {
            return (Admit::Busy, expired);
        }
        let decided = if let Some(c) = g.cards.iter_mut().find(|c| c.from == from && c.url == url) {
            c.at = now;
            Admit::Replace(c.n)
        } else if g.cards.iter().filter(|c| c.from == from).count() >= MAX_PENDING {
            return (Admit::Busy, expired);
        } else {
            g.next += 1;
            let n = g.next;
            g.cards.push(Card { n, from: from.to_string(), url: url.to_string(), at: now });
            Admit::New(n)
        };
        g.last.insert(from.to_string(), now);
        (decided, expired)
    }

    /// Card `n`'s link, taken out, if it is still waiting, from `from`.
    pub fn take(&self, n: u64, from: &str, now: Instant) -> Option<String> {
        let mut g = self.lock();
        let at = g.cards.iter().position(|c| c.n == n)?;
        let card = g.cards.remove(at);
        (card.from == from && now.saturating_duration_since(card.at) < LIFETIME).then_some(card.url)
    }

    /// Card `n` is gone from the desktop: dismissed, expired, or opened.
    pub fn gone(&self, n: u64) {
        self.lock().cards.retain(|c| c.n != n);
    }

    /// Computer `from` was removed: its cards go. Their numbers, to close.
    pub fn forget(&self, from: &str) -> Vec<u64> {
        let mut g = self.lock();
        g.last.remove(from);
        let (gone, kept): (Vec<Card>, Vec<Card>) = g.cards.drain(..).partition(|c| c.from == from);
        g.cards = kept;
        gone.into_iter().map(|c| c.n).collect()
    }

    /// Cards waiting from `from`. For the tests.
    #[cfg(test)]
    pub fn waiting(&self, from: &str) -> usize {
        self.lock().cards.iter().filter(|c| c.from == from).count()
    }
}

/// The notification key of card `n`.
pub fn key_of(n: u64) -> String {
    format!("{KEY_PREFIX}{n}")
}

fn number_of(key: &str) -> Option<u64> {
    key.strip_prefix(KEY_PREFIX)?.parse().ok()
}

fn refused(kind: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "kind": kind, "message": message}).to_string().into_bytes()
}

fn shown_reply(shown: bool, why: Option<&str>) -> Vec<u8> {
    let mut v = json!({"reply": PEER_VERB, "shown": shown});
    if let Some(why) = why {
        v["why"] = json!(why);
    }
    v.to_string().into_bytes()
}

/// What the card shows: "Open a link from <Laptop>?", then the host, the
/// title in quotes when there is one, and the link, shortened. The name is
/// this computer's record of the sender; every part is made plain text.
pub fn card_shown(from_name: &str, link: &Link, title: Option<&str>) -> Shown {
    use crate::mirror::clean;
    let from = clean(from_name, crate::mirror::APP_CAP, 0, false);
    let from = if from.is_empty() { "a paired computer".to_string() } else { from };
    let mut body = clean(&link.host, crate::mirror::TITLE_CAP, 0, true);
    if let Some(t) = title {
        body.push_str(&format!("\n“{}”", clean(t, MAX_TITLE, 0, true)));
    }
    body.push('\n');
    body.push_str(&clean(&link.url, URL_SHOWN, 0, true));
    Shown {
        app_name: "Rime Remote".into(),
        summary: format!("Open a link from {from}?"),
        body,
        reply: None,
        actions: vec![(OPEN_ACTION.to_string(), "Open".to_string())],
        sticky: false,
        expires_ms: Some(LIFETIME.as_millis() as u32),
    }
}

/// The answer to a `peer_handoff` from paired computer `sender`.
pub fn answer(state: &State, sender: &str, line: &[u8]) -> Vec<u8> {
    answer_with(state, sender, line, Instant::now(), |job| crate::mirror::mirror().submit(job))
}

fn answer_with(state: &State, sender: &str, line: &[u8], now: Instant, mut submit: impl FnMut(Job) -> bool) -> Vec<u8> {
    let computer = match state.devices() {
        Ok(store) => store.computers.get(sender).cloned(),
        Err(e) => {
            eprintln!("rime-remoted: a link could not be checked: {e}");
            return refused("unavailable", "this computer could not read its paired computers");
        }
    };
    // Checked first, so a computer whose cards are not shown learns nothing
    // more. Removed, or introduced and not allowed yet, counts as off.
    let Some(computer) = computer.filter(|c| c.takes_handoff()) else {
        return shown_reply(false, Some("off"));
    };
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return refused("bad_request", "not a JSON object");
    };
    let (link, title) = match parse_card(&v) {
        Ok(x) => x,
        Err(why) => return refused("bad_request", &format!("refused, nothing was shown: {why}")),
    };
    let (admit, expired) = state.handoff.admit(&computer.id, &link.url, now);
    for (from, n) in expired {
        submit(Job::Remove { device: from, key: key_of(n) });
    }
    let n = match admit {
        Admit::Busy => return shown_reply(false, Some("busy")),
        Admit::New(n) | Admit::Replace(n) => n,
    };
    let shown = card_shown(&computer.name, &link, title.as_deref());
    if !submit(Job::Post { device: computer.id.clone(), key: key_of(n), shown }) {
        if admit == Admit::New(n) {
            state.handoff.gone(n);
        }
        return shown_reply(false, Some("busy"));
    }
    shown_reply(true, None)
}

/// A button pressed on a notification this process made for computer
/// `device`. Whether the key was a card's; Open opens that card's link, from
/// this process's own table, while the card still waits and its sender is
/// still paired, trusted and shown here.
pub fn on_action(state: &State, device: &str, key: &str, action: &str) -> bool {
    on_action_with(state, device, key, action, Instant::now(), |job| crate::mirror::mirror().submit(job))
}

fn on_action_with(
    state: &State,
    device: &str,
    key: &str,
    action: &str,
    now: Instant,
    submit: impl FnOnce(Job) -> bool,
) -> bool {
    let Some(n) = number_of(key) else { return false };
    if action != OPEN_ACTION {
        return true;
    }
    let Some(url) = state.handoff.take(n, device, now) else { return true };
    let still = state.devices().ok().is_some_and(|s| s.computers.get(device).is_some_and(|c| c.takes_handoff()));
    if !still {
        eprintln!("rime-remoted: a link was not opened: its computer was removed or turned off since");
        return true;
    }
    if !submit(Job::OpenUri(url)) {
        eprintln!("rime-remoted: a link was not opened: this computer is behind on notifications");
    }
    true
}

/// A notification of this process's own is gone from the desktop: if it
/// was a card, the card goes too.
pub fn on_gone(state: &State, key: &str) {
    if let Some(n) = number_of(key) {
        state.handoff.gone(n);
    }
}

/// Computer `id` was removed: its cards go, from the desktop too.
pub fn forget(state: &State, id: &str) {
    for n in state.handoff.forget(id) {
        crate::mirror::mirror().submit(Job::Remove { device: id.to_string(), key: key_of(n) });
    }
}

/// `rime remote computers open <computer> <url>`: offer the link to that
/// computer as a card.
pub fn send(state: &State, computer: &str, url: &str, title: Option<&str>) -> Reply {
    let link = match checked_url(url) {
        Ok(l) => l,
        Err(message) => return Reply::Error { message },
    };
    let title = match checked_title(title) {
        Ok(t) => t,
        Err(message) => return Reply::Error { message },
    };
    let c = match state.devices() {
        Ok(store) => match store.find_computer(computer) {
            Ok(c) => c.clone(),
            Err(e) => return Reply::Error { message: e.to_string() },
        },
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    let name = crate::dialer::printable(&c.name);
    if !c.is_active() {
        return Reply::Error { message: format!("'{name}' was removed; nothing is sent to it") };
    }
    if !c.trusted() {
        return Reply::Error {
            message: format!(
                "'{name}' was introduced by a phone and is not allowed yet; run `rime remote computers allow` first"
            ),
        };
    }
    let told = |shown: bool, why: Option<&str>| Reply::ComputerHandoff {
        id: c.id.clone(),
        name: c.name.clone(),
        shown,
        why: why.map(str::to_string),
    };
    let mut dialled = match crate::dialer::open(state, &c, crate::dialer::Via::Any) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime-remoted: a link did not reach '{name}': {e}");
            return told(false, Some("offline"));
        }
    };
    let hello = match dialled.call(br#"{"cmd":"remote_hello"}"#) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("rime-remoted: a link did not reach '{name}': {e}");
            return told(false, Some("offline"));
        }
    };
    if !crate::share::offers(&hello, PEER_VERB) {
        return told(false, Some("unsupported"));
    }
    let mut line = json!({"cmd": PEER_VERB, "url": link.url});
    if let Some(t) = &title {
        line["title"] = json!(t);
    }
    let answer = match dialled.call(line.to_string().as_bytes()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rime-remoted: a link did not reach '{name}': {e}");
            return told(false, Some("offline"));
        }
    };
    match outcome(&name, &answer) {
        Ok((shown, why)) => told(shown, why),
        Err(message) => Reply::Error { message },
    }
}

/// What the other computer's answer to `peer_handoff` means: shown, or not
/// and why. Only the fixed words are passed on, never the other computer's
/// own; its error text only made printable.
fn outcome(name: &str, answer: &Value) -> Result<(bool, Option<&'static str>), String> {
    match (answer["reply"].as_str(), answer["shown"].as_bool(), answer["why"].as_str()) {
        (Some(PEER_VERB), Some(true), _) => Ok((true, None)),
        (Some(PEER_VERB), Some(false), Some("off")) => Ok((false, Some("off"))),
        (Some(PEER_VERB), Some(false), Some("busy")) => Ok((false, Some("busy"))),
        (Some("error"), _, _) => Err(format!(
            "'{name}' refused the link: {}",
            crate::dialer::printable(answer["message"].as_str().unwrap_or("no reason given"))
        )),
        _ => Err(format!("'{name}' answered in a way this computer does not understand")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::a_state;
    use rime_remote_core::device::Device;

    fn code_paired(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let c = store
            .pair_computer(&[seed; 32], &state.identity.public_bytes(), &format!("desk {seed}"), vec![], None, 1)
            .expect("pair");
        state.save_devices(&store).expect("save");
        c
    }

    fn introduced(state: &State, seed: u8) -> Device {
        let mut store = state.devices().expect("store");
        let added = store
            .introduce_computer(
                &[seed; 32],
                &state.identity.public_bytes(),
                &format!("lab {seed}"),
                vec![],
                None,
                rime_remote_core::device::Introducer { id: "phone".into(), name: "Pixel".into() },
                5,
            )
            .expect("introduce");
        let rime_remote_core::device::Introduced::Added(c) = added else { panic!("not added") };
        state.save_devices(&store).expect("save");
        *c
    }

    fn card(url: &str) -> Vec<u8> {
        json!({"cmd": PEER_VERB, "url": url}).to_string().into_bytes()
    }

    /// Answer one line, and say what was handed to the notification worker.
    fn ask(state: &State, sender: &str, line: &[u8], now: Instant) -> (Value, Vec<Job>) {
        let mut jobs = Vec::new();
        let r = answer_with(state, sender, line, now, |j| {
            jobs.push(j);
            true
        });
        (serde_json::from_slice(&r).expect("json"), jobs)
    }

    #[test]
    fn only_a_plain_http_or_https_link_with_a_host_is_taken() {
        for (raw, url, host) in [
            ("https://example.com", "https://example.com", "example.com"),
            ("HTTP://ok", "http://ok", "ok"),
            ("HtTpS://Example.COM:8443/a/B?c=D#e", "https://Example.COM:8443/a/B?c=D#e", "example.com:8443"),
            ("http://192.168.1.20/x", "http://192.168.1.20/x", "192.168.1.20"),
            ("http://[2001:db8::1]:8080/", "http://[2001:db8::1]:8080/", "[2001:db8::1]:8080"),
            ("https://de.wikipedia.org/wiki/Straße", "https://de.wikipedia.org/wiki/Straße", "de.wikipedia.org"),
            // A browser reads the same: the host ends at #, the rest is the fragment.
            ("http://a.com#@b.com", "http://a.com#@b.com", "a.com"),
            ("https://medium.com/@someone", "https://medium.com/@someone", "medium.com"),
            ("https://example.com./", "https://example.com./", "example.com."),
        ] {
            let l = checked_url(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!((l.url.as_str(), l.host.as_str()), (url, host), "{raw}");
        }
        let longest = format!("https://example.com/{}", "a".repeat(MAX_URL - 20));
        assert_eq!(longest.len(), MAX_URL);
        assert!(checked_url(&longest).is_ok());
        for raw in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "intent://scan/#Intent;scheme=zxing;end",
            "ftp://example.com",
            "data:text/html,hi",
            "httpx://example.com",
            " https://example.com",
            "http:example.com",
            "http:/example.com",
            "http:///example.com",
            "http://",
            "http://@example.com",
            "http://user:pass@example.com",
            "https://good.com@evil.com",
            "http://a.com\\@b.com",
            "https://exa mple.com",
            "https://example.com/a b",
            "https://example.com/\u{a0}",
            "https://moc.\u{202E}example.com",
            "https://example.com/\u{2066}x",
            "https://example.com/\u{200E}",
            "https://example.com/\u{200B}",
            "https://example.com/\u{7}",
            "https://example.com/\u{85}",
            "https://example.com/<b>",
            "http://localhost:8080/",
            "http://LOCALHOST/",
            "http://admin.localhost/",
            "http://127.0.0.1/",
            "http://127.1.2.3/",
            "http://0.0.0.0/",
            "http://0x7f.1/",
            "http://2130706433/",
            "http://01.2.3.4/",
            "http://1.2.3/",
            "http://[::1]/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fe80::1%25eth0]/",
            "http://[2001:db8::1/",
            "http://[2001:db8::1]x/",
            "http://ex%61mple.com/",
            "http://exämple.com/",
            "http://раypal.com/",
            "https://good.com∕evil.com/",
            "http://a..b/",
            "http://example.com:0/",
            "http://example.com:65536/",
            "http://example.com:/",
            "http://example.com:80:1/",
            "http://example.com:+80/",
        ] {
            assert!(checked_url(raw).is_err(), "taken: {raw:?}");
        }
        assert!(checked_url(&format!("{longest}a")).is_err(), "one byte over the limit");
        // A user name is refused as one, before the host is read.
        for raw in ["http://@example.com", "https://user:pw@example.com/", "https://good.com@evil.com", "http://[2001:db8::1]@x/"] {
            let why = checked_url(raw).unwrap_err();
            assert!(why.contains("user name or password"), "{raw}: {why}");
        }
    }

    #[test]
    fn a_title_is_plain_text_of_two_hundred_characters_at_most() {
        assert_eq!(checked_title(None), Ok(None));
        assert_eq!(checked_title(Some("  ")), Ok(None));
        assert_eq!(checked_title(Some(" The docs ")), Ok(Some("The docs".into())));
        let longest = "é".repeat(MAX_TITLE);
        assert_eq!(checked_title(Some(&longest)), Ok(Some(longest.clone())));
        for bad in [format!("{longest}e"), "a\nb".into(), "a\tb".into(), "\u{1b}[31m".into(), "abc\u{202E}gpj".into(),
                    "a\u{2069}".into(), "a\u{200F}".into(), "a\u{9f}".into()] {
            assert!(checked_title(Some(&bad)).is_err(), "taken: {bad:?}");
        }
        // Over the wire: a title that is not text is refused whole.
        assert!(parse_card(&json!({"url": "https://example.com", "title": 5})).is_err());
        assert!(parse_card(&json!({"url": 5})).is_err());
        assert!(parse_card(&json!({"url": "https://example.com", "title": null})).is_ok());
    }

    #[test]
    fn the_card_names_the_computer_from_this_record_and_shows_the_host_first_as_plain_text() {
        let link = checked_url("https://evil.example/a?x=1&y=<2>").unwrap_err();
        assert!(link.contains("< >"), "{link}");
        let link = checked_url("https://evil.example/a?x=1&y=2").expect("ok");
        let s = card_shown("Laptop", &link, Some("<b>paypal.com</b> & co"));
        assert_eq!(s.summary, "Open a link from Laptop?");
        let lines: Vec<&str> = s.body.lines().collect();
        assert_eq!(lines, ["evil.example", "“‹b›paypal.com‹/b› &amp; co”", "https://evil.example/a?x=1&amp;y=2"]);
        assert_eq!(s.actions, [(OPEN_ACTION.to_string(), "Open".to_string())]);
        assert_eq!(s.expires_ms, Some(600_000));
        assert!(!s.sticky);
        // A long link is cut; the host is always whole, first.
        let long = checked_url(&format!("https://example.com/{}", "a".repeat(500))).expect("ok");
        let s = card_shown("Laptop", &long, None);
        let lines: Vec<&str> = s.body.lines().collect();
        assert_eq!(lines[0], "example.com");
        assert!(lines[1].ends_with('…') && lines[1].chars().count() <= URL_SHOWN + 1, "{}", lines[1]);
    }

    #[test]
    fn three_cards_wait_from_one_computer_one_every_two_seconds_and_the_same_link_replaces() {
        let h = Handoff::default();
        let t0 = Instant::now();
        let s = Duration::from_secs;
        assert_eq!(h.admit("a", "https://1.example", t0).0, Admit::New(1));
        // Too soon, even for another link.
        assert_eq!(h.admit("a", "https://2.example", t0 + Duration::from_millis(1999)).0, Admit::Busy);
        // Another computer is not held up by this one.
        assert_eq!(h.admit("b", "https://1.example", t0).0, Admit::New(2));
        assert_eq!(h.admit("a", "https://2.example", t0 + s(2)).0, Admit::New(3));
        // The same link while waiting: its card again, not a new one.
        assert_eq!(h.admit("a", "https://1.example", t0 + s(4)).0, Admit::Replace(1));
        assert_eq!(h.waiting("a"), 2);
        assert_eq!(h.admit("a", "https://3.example", t0 + s(6)).0, Admit::New(4));
        assert_eq!(h.admit("a", "https://4.example", t0 + s(8)).0, Admit::Busy, "a fourth waited");
        assert_eq!(h.waiting("a"), 3);
        // One dismissed: room again.
        h.gone(3);
        assert_eq!(h.admit("a", "https://4.example", t0 + s(10)).0, Admit::New(5));
        // Ten minutes after it was last offered, a card is gone, and said so
        // (b's too, offered at t0): its place is free again.
        let (got, expired) = h.admit("a", "https://5.example", t0 + s(4) + LIFETIME);
        assert_eq!(got, Admit::New(6), "an expired card still held its place");
        assert_eq!(expired, [("a".to_string(), 1), ("b".to_string(), 2)]);
        assert_eq!(h.take(4, "a", t0 + s(4) + LIFETIME), Some("https://3.example".into()));
        assert_eq!(h.take(5, "a", t0 + s(10) + LIFETIME), None, "an expired card opened");
        // Removal takes every card of that computer.
        assert_eq!(h.admit("b", "https://2.example", t0 + s(20)).0, Admit::New(7));
        assert_eq!(h.forget("b"), [7]);
        assert_eq!(h.waiting("b"), 0);
    }

    #[test]
    fn a_card_is_shown_only_from_a_trusted_computer_whose_switch_is_on_and_garbage_shows_nothing() {
        let (state, _dir) = a_state("handoff-gate");
        let desk = code_paired(&state, 71);
        let lab = introduced(&state, 72);
        let t0 = Instant::now();
        let ok = card("https://example.com/notes");
        let off = json!({"reply": PEER_VERB, "shown": false, "why": "off"});
        // Not paired at all.
        let (v, jobs) = ask(&state, "stranger", &ok, t0);
        assert_eq!((v, jobs.len()), (off.clone(), 0));
        // Introduced and not allowed: off, even with the switch set by hand.
        {
            let mut store = state.devices().expect("store");
            assert!(store.set_computer_handoff(&lab.id, true).is_err());
            store.computers.get_mut(&lab.id).expect("there").share.handoff = Some(true);
            state.save_devices(&store).expect("save");
        }
        let (v, jobs) = ask(&state, &lab.id, &ok, t0);
        assert_eq!((v, jobs.len()), (off.clone(), 0));
        // Code-paired, on by default: shown, under its record's name.
        let (v, jobs) = ask(&state, &desk.id, &ok, t0);
        assert_eq!(v, json!({"reply": PEER_VERB, "shown": true}));
        let [Job::Post { device, key, shown }] = &jobs[..] else { panic!("{jobs:?}") };
        assert_eq!((device.as_str(), key.as_str()), (desk.id.as_str(), "handoff:1"));
        assert_eq!(shown.summary, "Open a link from desk 71?");
        // Garbage is refused whole, and shows nothing.
        for bad in [
            &b"not json"[..],
            br#"{"cmd":"peer_handoff"}"#,
            br#"{"cmd":"peer_handoff","url":"file:///etc/shadow"}"#,
            br#"{"cmd":"peer_handoff","url":"https://user:pw@example.com"}"#,
            br#"{"cmd":"peer_handoff","url":"https://example.com","title":"\u202Egnp.exe"}"#,
        ] {
            let (v, jobs) = ask(&state, &desk.id, bad, t0 + Duration::from_secs(10));
            assert_eq!(v["kind"], "bad_request", "{v}");
            assert!(jobs.is_empty(), "{jobs:?}");
        }
        // Off here: off.
        {
            let mut store = state.devices().expect("store");
            store.set_computer_handoff(&desk.id, false).expect("off");
            state.save_devices(&store).expect("save");
        }
        let (v, jobs) = ask(&state, &desk.id, &card("https://example.com/2"), t0 + Duration::from_secs(20));
        assert_eq!((v, jobs.len()), (off.clone(), 0));
        // On again, then removed: off, whatever the record's switch says.
        {
            let mut store = state.devices().expect("store");
            store.set_computer_handoff(&desk.id, true).expect("on");
            store.revoke_computer(&desk.id, 7).expect("remove");
            assert_eq!(store.computers[&desk.id].share.handoff, None, "removal kept the switch");
            store.computers.get_mut(&desk.id).expect("tombstone").share.handoff = Some(true);
            state.save_devices(&store).expect("save");
        }
        let (v, jobs) = ask(&state, &desk.id, &card("https://example.com/3"), t0 + Duration::from_secs(30));
        assert_eq!((v, jobs.len()), (off, 0));
    }

    #[test]
    fn a_flood_is_busy_and_a_card_the_worker_could_not_take_holds_no_place() {
        let (state, _dir) = a_state("handoff-flood");
        let desk = code_paired(&state, 73);
        let t0 = Instant::now();
        let busy = json!({"reply": PEER_VERB, "shown": false, "why": "busy"});
        for i in 0..MAX_PENDING as u64 {
            let (v, _) = ask(&state, &desk.id, &card(&format!("https://example.com/{i}")), t0 + SPACING * i as u32);
            assert_eq!(v["shown"], true, "{v}");
        }
        let later = t0 + SPACING * 10;
        let (v, jobs) = ask(&state, &desk.id, &card("https://example.com/new"), later);
        assert_eq!((v, jobs.len()), (busy.clone(), 0));
        // The same link again replaces its card in place: the same key.
        let (v, jobs) = ask(&state, &desk.id, &card("https://example.com/0"), later + SPACING);
        assert_eq!(v["shown"], true, "{v}");
        let [Job::Post { key, .. }] = &jobs[..] else { panic!("{jobs:?}") };
        assert_eq!(key, "handoff:1");
        // Too soon after that.
        let (v, _) = ask(&state, &desk.id, &card("https://example.com/0"), later + SPACING + Duration::from_millis(5));
        assert_eq!(v, busy);
        // A full worker queue: busy, and no place is held for it.
        state.handoff.gone(2);
        let r = answer_with(&state, &desk.id, &card("https://example.com/x"), later + SPACING * 3, |_| false);
        assert_eq!(serde_json::from_slice::<Value>(&r).expect("json"), busy);
        assert_eq!(state.handoff.waiting(&desk.id), 2);
        // Ten minutes on, the cards still up are closed when the next comes.
        let (v, jobs) = ask(&state, &desk.id, &card("https://example.com/y"), later + LIFETIME * 2);
        assert_eq!(v["shown"], true, "{v}");
        let closed: Vec<&str> =
            jobs.iter().filter_map(|j| if let Job::Remove { key, .. } = j { Some(key.as_str()) } else { None }).collect();
        assert_eq!(closed, ["handoff:1", "handoff:3"], "{jobs:?}");
    }

    #[test]
    fn open_opens_only_its_own_cards_link_once_while_its_sender_is_still_shown_here() {
        let (state, _dir) = a_state("handoff-open");
        let desk = code_paired(&state, 74);
        let other = code_paired(&state, 75);
        let t0 = Instant::now();
        let press = |device: &str, key: &str, action: &str, at: Instant| {
            let mut jobs = Vec::new();
            let claimed = on_action_with(&state, device, key, action, at, |j| {
                jobs.push(j);
                true
            });
            (claimed, jobs)
        };
        let opened = |jobs: &[Job]| -> Vec<String> {
            jobs.iter().filter_map(|j| if let Job::OpenUri(u) = j { Some(u.clone()) } else { None }).collect()
        };
        let (v, _) = ask(&state, &desk.id, &card("HTTPS://example.com/a?b=1"), t0);
        assert_eq!(v["shown"], true);
        // Not ours: not claimed. Another button, or another computer's
        // press on this card: claimed, nothing opened.
        assert!(!press(&desk.id, "file:1", OPEN_ACTION, t0).0);
        assert!(!press(&desk.id, "call:ringing", OPEN_ACTION, t0).0);
        let (claimed, jobs) = press(&desk.id, "handoff:1", "default", t0);
        assert!(claimed && jobs.is_empty(), "{jobs:?}");
        // Open: exactly the checked link, from this table.
        let (claimed, jobs) = press(&desk.id, "handoff:1", OPEN_ACTION, t0);
        assert!(claimed);
        assert_eq!(opened(&jobs), ["https://example.com/a?b=1"]);
        // Twice: once.
        assert!(press(&desk.id, "handoff:1", OPEN_ACTION, t0).1.is_empty());
        // A card pressed under another computer's name opens nothing.
        let (v, _) = ask(&state, &desk.id, &card("https://example.com/b"), t0 + SPACING);
        assert_eq!(v["shown"], true);
        assert!(press(&other.id, "handoff:2", OPEN_ACTION, t0 + SPACING).1.is_empty());
        // Expired: nothing.
        let (v, _) = ask(&state, &desk.id, &card("https://example.com/c"), t0 + SPACING * 2);
        assert_eq!(v["shown"], true);
        assert!(press(&desk.id, "handoff:3", OPEN_ACTION, t0 + SPACING * 2 + LIFETIME).1.is_empty());
        // Turned off after it was shown: nothing.
        let (v, _) = ask(&state, &desk.id, &card("https://example.com/d"), t0 + SPACING * 3);
        assert_eq!(v["shown"], true);
        {
            let mut store = state.devices().expect("store");
            store.set_computer_handoff(&desk.id, false).expect("off");
            state.save_devices(&store).expect("save");
        }
        assert!(press(&desk.id, "handoff:4", OPEN_ACTION, t0 + SPACING * 3).1.is_empty());
        // Removed after it was shown: nothing.
        {
            let mut store = state.devices().expect("store");
            store.set_computer_handoff(&desk.id, true).expect("on");
            state.save_devices(&store).expect("save");
        }
        let (v, _) = ask(&state, &desk.id, &card("https://example.com/e"), t0 + SPACING * 4);
        assert_eq!(v["shown"], true);
        {
            let mut store = state.devices().expect("store");
            store.revoke_computer(&desk.id, 9).expect("remove");
            state.save_devices(&store).expect("save");
        }
        assert!(press(&desk.id, "handoff:5", OPEN_ACTION, t0 + SPACING * 4).1.is_empty());
        // Dismissed: gone, and its place is free.
        let (v, _) = ask(&state, &other.id, &card("https://example.com/f"), t0);
        assert_eq!(v["shown"], true);
        assert_eq!(state.handoff.waiting(&other.id), 1);
        on_gone(&state, "handoff:6");
        assert_eq!(state.handoff.waiting(&other.id), 0);
        assert!(press(&other.id, "handoff:6", OPEN_ACTION, t0).1.is_empty());
    }

    #[test]
    fn the_sender_passes_on_only_the_fixed_words_of_the_answer() {
        let shown = json!({"reply": PEER_VERB, "shown": true});
        assert_eq!(outcome("desk", &shown), Ok((true, None)));
        for why in ["off", "busy"] {
            assert_eq!(outcome("desk", &json!({"reply": PEER_VERB, "shown": false, "why": why})), Ok((false, Some(why))));
        }
        for odd in [
            json!({"reply": PEER_VERB, "shown": false, "why": "\u{1b}[2Jclick here"}),
            json!({"reply": PEER_VERB, "shown": false}),
            json!({"reply": PEER_VERB, "shown": "yes"}),
            json!({"reply": "peer_clipboard", "shown": true}),
            json!({}),
        ] {
            assert!(outcome("desk", &odd).unwrap_err().contains("does not understand"), "{odd}");
        }
        let refused = outcome("desk", &json!({"reply": "error", "message": "bad\u{1b}[31m\nline"})).unwrap_err();
        assert!(refused.starts_with("'desk' refused the link: ") && !refused.chars().any(char::is_control), "{refused:?}");
    }

    #[test]
    fn the_sender_checks_the_link_and_the_computer_before_it_dials() {
        let (state, _dir) = a_state("handoff-send");
        let desk = code_paired(&state, 76);
        let lab = introduced(&state, 77);
        for (computer, url, title, words) in [
            (desk.id.as_str(), "javascript:alert(1)", None, "only http and https"),
            (desk.id.as_str(), "https://u:p@example.com", None, "user name or password"),
            (desk.id.as_str(), "https://example.com", Some("a\u{202E}b"), "direction-changing"),
            (lab.id.as_str(), "https://example.com", None, "not allowed yet"),
            ("nobody", "https://example.com", None, "nobody"),
        ] {
            let Reply::Error { message } = send(&state, computer, url, title) else { panic!("{computer} {url} went") };
            assert!(message.contains(words), "{message}");
        }
        {
            let mut store = state.devices().expect("store");
            store.revoke_computer(&desk.id, 9).expect("remove");
            state.save_devices(&store).expect("save");
        }
        let Reply::Error { message } = send(&state, &desk.id, "https://example.com", None) else { panic!("went") };
        assert!(message.contains("removed"), "{message}");
    }
}
