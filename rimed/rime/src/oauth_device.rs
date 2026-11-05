//! RFC 8628, the OAuth 2.0 device authorization grant, for any authorisation
//! server in [`rime_secret_core::account::OAUTH`].
//!
//! ```text
//! rime cf connect                  # Cloudflare, through this
//! rime account add google.work     # Google and Microsoft, through this
//! ```
//!
//! ## Why it is here and not in `cloudflare.rs`
//!
//! It was in `cloudflare.rs`, because Cloudflare was the only provider that
//! had one. Nothing in the flow is Cloudflare's: §3.1 asks a device endpoint
//! for a code, §3.4 polls a token endpoint until something decides, and §3.5
//! says what `slow_down` means. What *was* Cloudflare's — the service names the
//! two tokens are filed under, the API host the access token is pinned to, the
//! words printed afterwards — stays in `cloudflare.rs`, which calls this.
//!
//! The split is the point rather than tidiness. `rime account add` for a
//! `Flow::DeviceCode` provider is the same flow against a different
//! authorisation server, and a second copy of a poll loop is a second place for
//! §3.5's "add five seconds, permanently" to be got wrong. One copy also means
//! one set of loopback tests covers both callers.
//!
//! ## Nothing here is launched, and nothing here is stored
//!
//! The verification URL and the user code are **printed**. A browser on this
//! machine is the thing the device grant exists to avoid — P2-018 is the item
//! next to this one, and a machine with no working graphics still has to be
//! able to sign in. And this module hands its caller a [`Grant`]; where the
//! tokens go, and under what names, is the caller's decision because it is the
//! caller that knows which credential is which.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rime_secret_core::account::OAuth;

/// RFC 8628 §3.4's `grant_type`.
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Longest token this build will accept from a token endpoint.
///
/// The same number `rime-secretd`'s `oauth` provider uses, and deliberately
/// not smaller: a CLI that refused a token the daemon would have accepted
/// would turn a working sign-in into "that reply was not a token". It is well
/// under [`rime_secret_core::SecretValue::MAX_BYTES`], so a token this accepts
/// is a token the store will take.
///
/// It is larger than the 4096 this was written with, for a reason that only
/// shows up off Cloudflare: a Microsoft access token is a JWT carrying the
/// caller's claims, and a well-populated one goes past 4 KB.
const MAX_TOKEN: usize = 8192;

/// A build whose CLI cap outgrew the store would accept a token at the prompt
/// and fail to save it, which is the worst moment to find out. Checked at
/// compile time rather than in a test, because there is nothing to run.
const _: () = assert!(MAX_TOKEN <= rime_secret_core::SecretValue::MAX_BYTES);

/// The two endpoints the flow uses.
///
/// A struct rather than constants so the poll loop can be run against a
/// loopback server that serves RFC 8628's states in order. Without that the
/// only way to exercise this code is to have an account at one of the three
/// authorisation servers in the table.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub device: String,
    pub token: String,
    /// Shortest gap between polls. The server's `interval` wins when it is
    /// larger; a test sets this low so the loop does not take a minute.
    pub floor: Duration,
    /// How long to keep asking, whatever the server said.
    pub limit: Duration,
    /// What `slow_down` adds to the interval. RFC 8628 §3.5 says five seconds
    /// and that is what the real one uses; it is a field for the same reason
    /// `floor` is, so a test can exercise the branch without waiting out three
    /// real intervals to do it.
    pub backoff: Duration,
}

impl Endpoints {
    /// The endpoints of one authorisation server in the shared table.
    ///
    /// Off [`OAuth`] rather than built from a host, because the device
    /// endpoint has to be on the same auth domain as the token endpoint it is
    /// paired with and `Provider::validate` is what holds the table to that.
    pub fn for_oauth(oauth: &OAuth) -> Endpoints {
        Endpoints {
            device: oauth.device_url.to_string(),
            token: oauth.token_url.to_string(),
            floor: Duration::from_secs(5),
            limit: Duration::from_secs(300),
            backoff: Duration::from_secs(5),
        }
    }
}

/// The OAuth client this machine asks as.
///
/// The `id` is public by definition — it is on the consent screen the user
/// reads — so it may come from argv. The secret may not, and no caller here
/// takes one from argv: `rime account add` reads it from stdin, which is the
/// same rule `rime secret add` follows and for the same reason.
#[derive(Debug, Clone, Copy)]
pub struct OAuthClient<'a> {
    pub id: &'a str,
    /// Sent on **the poll** when the authorisation server requires one —
    /// [`rime_secret_core::account::ClientSecret`] says which do, and §3.1's
    /// device request never carries it because neither provider documents one
    /// there. `None` for a public client, where sending an empty one would be
    /// a different request from sending none.
    pub secret: Option<&'a str>,
}

/// The words this flow is printed in.
///
/// Two strings rather than one because they answer different questions and a
/// generic version of either reads as a bug: "To connect the provider" is not
/// a sentence, and "start again" does not say with what.
#[derive(Debug, Clone, Copy)]
pub struct Prompt<'a> {
    /// Named in "To connect `<label>`, open this on any device".
    pub label: &'a str,
    /// The command to run again when the code expires, without a leading verb.
    pub retry: &'a str,
}

/// What the token endpoint gave back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Seconds. Printed, because a token that expires this afternoon and a
    /// token that expires next year are different things to be handed.
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
}

/// Why the flow stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// The person said no.
    Denied,
    /// The code ran out before it was approved. Carries the command to run
    /// again, because "expired" without one is a dead end.
    Expired(String),
    /// The server said something RFC 8628 does not define, or nothing usable.
    Unusable(String),
    /// The endpoint could not be reached.
    Unreachable(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Denied => {
                f.write_str("the request was declined in the browser, so nothing was stored")
            }
            DeviceError::Expired(retry) => write!(
                f,
                "the code ran out before it was approved. Run `{retry}` again"
            ),
            DeviceError::Unusable(what) => write!(
                f,
                "the authorisation server answered with something this build \
                 cannot use: {what}"
            ),
            DeviceError::Unreachable(what) => {
                write!(f, "the authorisation server could not be reached: {what}")
            }
        }
    }
}

impl std::error::Error for DeviceError {}

/// Whether a token is a shape this build will put in a request.
///
/// Checked here as well as in the daemon, because a token that is only refused
/// at use time is one somebody stored, granted, and then watched fail with a
/// message about the far side rather than about their paste. The commonest bad
/// paste is a trailing newline, which `trim` handles, and the second commonest
/// is the whole `Authorization: Bearer …` line, which this catches — the space
/// and the colon are both outside the set.
///
/// The set is the unreserved characters of RFC 3986 plus base64's `+` `/` `=`,
/// which covers an opaque token, a JWT and a base64 blob alike.
pub fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
        })
}

/// How long to wait between polls.
///
/// RFC 8628 §3.5: the server's `interval` is honoured when it is given, and a
/// client that ignored it would be told `slow_down` for the rest of the flow.
/// The floor applies when the server said nothing, or said something smaller
/// than the floor — it is the only value under this build's control, so it is
/// where a test can make the loop run at a speed a test can wait for.
pub fn poll_interval(server: Option<u64>, floor: Duration) -> Duration {
    match server.map(Duration::from_secs) {
        Some(given) if given > floor => given,
        _ => floor,
    }
}

/// Seconds as something a person reads.
///
/// Plural handled rather than fudged with "(s)", and the boundaries are on the
/// units they name: 3600 seconds is an hour, not sixty minutes.
pub fn roughly(seconds: u64) -> String {
    let (count, unit) = match seconds {
        0..=90 => (seconds, "second"),
        91..=3599 => (seconds / 60, "minute"),
        3600..=172_799 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// Ask for a code, print it, and poll until something decides.
pub fn device_grant(
    ends: &Endpoints,
    client: &OAuthClient<'_>,
    scopes: &[&str],
    prompt: &Prompt<'_>,
    out: &mut impl Write,
) -> Result<Grant, DeviceError> {
    let scope = scopes.join(" ");
    let mut pairs: Vec<(&str, &str)> = vec![("client_id", client.id)];
    // §3.1 has no `scope` requirement and Cloudflare's entry asks for none, so
    // an empty list sends no field rather than an empty one — `scope=` is a
    // request for a scope named "" at a server that reads it literally.
    if !scope.is_empty() {
        pairs.push(("scope", &scope));
    }
    // No `client_secret` here. Neither Google's limited-input-device guide nor
    // Microsoft's device-code documentation lists one on §3.1's request — both
    // want it on the poll — and sending an undocumented field to an
    // authorisation server is a request whose handling nobody has written down.
    let body = form(&pairs)
        .ok_or_else(|| DeviceError::Unusable("that client id cannot be sent".into()))?;
    let reply = post(&ends.device, &body).map_err(DeviceError::Unreachable)?;
    let json = parse(&reply.body)
        .ok_or_else(|| DeviceError::Unusable(describe(reply.status, &reply.body)))?;

    let device_code = text(&json, "device_code")
        .ok_or_else(|| DeviceError::Unusable(describe(reply.status, &reply.body)))?;
    let user_code = text(&json, "user_code")
        .ok_or_else(|| DeviceError::Unusable("no user code".into()))?;
    // `verification_uri` is RFC 8628 §3.2's spelling and `verification_url` is
    // Google's, documented that way in its limited-input-device guide. Both are
    // read, because accepting only the standard one would make every real
    // Google sign-in fail with "no verification address" while a loopback
    // double that spells it correctly stayed green.
    let uri = text(&json, "verification_uri")
        .or_else(|| text(&json, "verification_url"))
        .ok_or_else(|| DeviceError::Unusable("no verification address".into()))?;
    let expires_in = number(&json, "expires_in").unwrap_or(300);
    let interval = poll_interval(number(&json, "interval"), ends.floor);

    // Printed, never opened. A browser here is the thing the whole flow exists
    // to avoid, and this machine may not have one.
    let _ = writeln!(out, "To connect {}, open this on any device:", prompt.label);
    let _ = writeln!(out, "  {uri}");
    let _ = writeln!(out, "and enter the code:");
    let _ = writeln!(out, "  {user_code}");
    if let Some(complete) = text(&json, "verification_uri_complete") {
        let _ = writeln!(out, "or open this, which fills the code in:");
        let _ = writeln!(out, "  {complete}");
    }
    let _ = writeln!(
        out,
        "waiting up to {} for you to approve it.",
        roughly(expires_in.min(ends.limit.as_secs()))
    );
    let _ = out.flush();

    poll(ends, client, prompt, &device_code, interval, expires_in)
}

/// RFC 8628 §3.4: ask the token endpoint until it stops saying "not yet".
fn poll(
    ends: &Endpoints,
    client: &OAuthClient<'_>,
    prompt: &Prompt<'_>,
    device_code: &str,
    mut interval: Duration,
    expires_in: u64,
) -> Result<Grant, DeviceError> {
    let started = Instant::now();
    let deadline = ends.limit.min(Duration::from_secs(expires_in));
    let mut pairs: Vec<(&str, &str)> = vec![
        ("client_id", client.id),
        ("device_code", device_code),
        ("grant_type", DEVICE_GRANT),
    ];
    // Google lists `client_secret` as required on this request and it is what
    // `ClientSecret::RequiredToObtain` names. A public client sends none.
    if let Some(secret) = client.secret {
        pairs.push(("client_secret", secret));
    }
    let Some(body) = form(&pairs) else {
        // The device code came from the server, so a shape this cannot send is
        // the server's, not the caller's.
        return Err(DeviceError::Unusable(
            "the device code it issued is not a shape this build can send".into(),
        ));
    };

    loop {
        std::thread::sleep(interval);
        if started.elapsed() > deadline {
            return Err(DeviceError::Expired(prompt.retry.to_string()));
        }
        let reply = match post(&ends.token, &body) {
            Ok(reply) => reply,
            // A poll that could not be sent is not a decision. Keep going
            // until the deadline rather than failing a login because one
            // request lost a race with a sleeping wifi card.
            Err(_) => continue,
        };
        let Some(json) = parse(&reply.body) else {
            if reply.status >= 500 || reply.status == 429 {
                continue;
            }
            return Err(DeviceError::Unusable(describe(reply.status, &reply.body)));
        };

        // The error is checked BEFORE the token, because a body carrying both
        // is a body this build does not understand, and reading the token out
        // of it would be reading the half that suits us.
        if let Some(error) = text(&json, "error") {
            match error.as_str() {
                "authorization_pending" => continue,
                // §3.5: add five seconds and carry on, permanently.
                "slow_down" => {
                    interval += ends.backoff;
                    continue;
                }
                "access_denied" => return Err(DeviceError::Denied),
                "expired_token" => return Err(DeviceError::Expired(prompt.retry.to_string())),
                other => {
                    let detail =
                        text(&json, "error_description").unwrap_or_else(|| other.to_string());
                    return Err(DeviceError::Unusable(detail));
                }
            }
        }

        if let Some(access_token) = text(&json, "access_token") {
            if !valid_token(&access_token) {
                return Err(DeviceError::Unusable(
                    "the token it issued is not a shape this build will send".into(),
                ));
            }
            return Ok(Grant {
                access_token,
                refresh_token: text(&json, "refresh_token").filter(|t| valid_token(t)),
                expires_in: number(&json, "expires_in"),
                scope: text(&json, "scope"),
            });
        }

        // Neither an error nor a token. A proxy or a WAF, most likely — and
        // falling through to the success path here is how a login page gets
        // stored as a credential.
        if reply.status >= 500 || reply.status == 429 {
            continue;
        }
        return Err(DeviceError::Unusable(describe(reply.status, &reply.body)));
    }
}

fn describe(status: u16, body: &str) -> String {
    let first: String = body.chars().take(120).filter(|c| !c.is_control()).collect();
    if first.trim().is_empty() {
        format!("HTTP {status}, with an empty body")
    } else {
        format!("HTTP {status}")
    }
}

// ── http, the same shape the daemon uses ────────────────────────────────────

/// A reply from the authorisation server.
struct Reply {
    status: u16,
    body: String,
}

/// Whether a value can go in a form body without being escaped.
///
/// Everything this sends is a client id, a client secret, a device code, a
/// grant-type URN or a space-separated scope list, all of which are already
/// this shape. A value that is not is refused rather than encoded, so there is
/// no encoder here to get wrong — and no `&` or `=` means no way to turn one
/// field into two.
pub fn valid_form_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'/' | b' ')
        })
}

/// The same set once the pairs have been joined: `&` and `=` separate them and
/// `+` is the one substitution [`form`] makes.
fn valid_form_body(body: &str) -> bool {
    !body.is_empty()
        && body.len() <= 8192
        && body.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'/' | b'+' | b'&' | b'=')
        })
}

/// Build a form body, refusing any value that is not [`valid_form_value`].
///
/// `None` rather than an escaped body: a caller that produced an unexpected
/// value has a bug, and encoding around it would hide the bug and send the
/// request anyway.
pub fn form(pairs: &[(&str, &str)]) -> Option<String> {
    if pairs.iter().any(|(_, v)| !valid_form_value(v)) {
        return None;
    }
    Some(
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.replace(' ', "+")))
            .collect::<Vec<_>>()
            .join("&"),
    )
}

/// POST a form, through a `curl` this process owns.
///
/// The same arrangement `rime-secretd` uses and for the same reasons: the whole
/// request goes down the child's stdin as a config file, so nothing
/// caller-shaped reaches an option parser, and `-q` first so a `~/.curlrc`
/// cannot choose a proxy for a request that carries an authorisation code.
/// It is forty lines copied rather than shared, because the daemon's copy is
/// inside a binary crate this cannot link.
fn post(url: &str, body: &str) -> Result<Reply, String> {
    if !valid_form_body(body) {
        return Err("that request cannot be sent as written".to_string());
    }
    // The scheme comes from the URL rather than being pinned to https, so a
    // loopback double is reachable. Every entry in the shipped table is https
    // and nothing but a test ever builds anything else.
    let scheme = url.split("://").next().unwrap_or("https");
    let config = format!(
        "url = \"{url}\"\nrequest = \"POST\"\nproto = \"={scheme}\"\n\
         header = \"Content-Type: application/x-www-form-urlencoded\"\n\
         header = \"Accept: application/json\"\n\
         header = \"Expect:\"\n\
         data = \"{body}\"\n\
         max-time = 30\nconnect-timeout = 15\nsilent\nshow-error\n\
         write-out = \"\\n%{{http_code}}\"\n"
    );

    let mut child = Command::new(rime_paths::CURL)
        .arg("-q")
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (body, status) = match stdout.rsplit_once('\n') {
        Some((body, tail)) => (body.to_string(), tail.trim().parse::<u16>().ok()),
        None => (String::new(), None),
    };
    match status {
        Some(status) => Ok(Reply { status, body }),
        None => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn parse(body: &str) -> Option<serde_json::Value> {
    serde_json::from_str(body).ok()
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn number(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| v.as_u64()).filter(|n| *n > 0)
}

/// Read a whole HTTP request off a stream and answer it. Test-only, but shaped
/// here so the double and the client agree on framing.
#[cfg(test)]
pub(crate) fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut first = String::new();
    let _ = reader.read_line(&mut first);
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.trim().strip_prefix("Content-Length: ") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    (first, String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests;
