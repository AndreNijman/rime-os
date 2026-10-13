//! The only place any of this has been run.
//!
//! There is no Cloudflare account and no Cloudflare token on this machine, so
//! the alternative to a loopback double was a provider nobody had ever seen
//! execute. The double speaks Cloudflare's envelope — `{"success":…,"result":…,
//! "errors":[…]}` — answers the documented paths, and **refuses anything with
//! no `Authorization` header**, which is what makes a passing test mean
//! something: a provider that quietly did nothing would come back 401, not 200.
//!
//! What that proves and what it does not:
//!
//! * proved — the request the provider builds is the shape it means to build,
//!   the credential reaches the far side, none of it reaches the caller, the
//!   binding decides what a name means, and the framework's checks all fire;
//! * not proved — that `api.cloudflare.com` accepts any of these bodies. Every
//!   path is the documented one and not one of them has been called for real.
//!   That is what P1-004's `partial` is for.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use apex_secret_core::audit::{self, AuditEvent};
use apex_secret_core::capability::CapabilityRecord;
use apex_secret_core::protocol::{ErrorKind, Response};
use apex_secret_core::store::Store;
use apex_secret_core::SecretValue;

use crate::peer::Peer;
use crate::provider::Registry;
use crate::service::{NewService, Service};

use super::*;

/// The stored credential. Distinctive enough that a grep for it cannot match
/// by accident, and long enough to look like a token.
const TOKEN: &str = "apex-cf-sentinel-4f21c9ae7b3d-do-not-leak";

/// The account id the fixture's project binds.
const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

/// The zone id it binds.
const ZONE: &str = "fedcba9876543210fedcba9876543210";

/// The ids §13.1's file binds for P1-006's four resource tables. A D1 database
/// is a UUID and the other three are 32 hex, which is Cloudflare's own split
/// and the reason the binding has two id shapes rather than one.
const DB_ID: &str = "1a2b3c4d-5e6f-4a8b-9c0d-1e2f3a4b5c6d";
const KV_ID: &str = "00112233445566778899aabbccddeeff";
const QUEUE_ID: &str = "ffeeddccbbaa99887766554433221100";
const HD_ID: &str = "0f0e0d0c0b0a09080706050403020100";

/// What the double has stored under the one KV key a test reads. Not JSON —
/// KV answers a read with the value's bytes — and it carries the credential the
/// request arrived with.
const KV_VALUE: &str = "stored by apex, read back with {{authorization}}";

/// The two record ids the double hands out. 32 lowercase hex, which is the
/// shape the provider checks before one becomes part of a URL.
const RECORD_A: &str = "aa11bb22cc33dd44ee55ff6677889900";
const RECORD_B: &str = "00998877ff66ee55dd44cc33bb22aa11";

/// The one bucket §13.1's file binds.
const BUCKET: &str = "example-assets";

/// §13.10's two ids the fixture's file binds. An Access application id is
/// `oneOf [32 hex, uuid]` in Cloudflare's own schema, so the fixture uses the
/// UUID half — the shape a validator written for hex alone would refuse.
const APP_ID: &str = "f174e90a-fafe-4643-bbbc-4a0ed4fc8415";
const TUNNEL_ID: &str = "f70ff985-a4ef-4643-bbbc-4a0ed4fc8415";

/// What the double answers with when a service token is issued. The secret is
/// the thing this whole unit exists to keep out of the caller's hands, so it is
/// distinctive enough that finding it anywhere means it travelled.
const CLIENT_ID: &str = "8a1b2c3d4e5f60718293a4b5c6d7e8f9.access";
const CLIENT_SECRET: &str = "apex-cf-service-token-secret-0d4e1a-do-not-leak";

/// The SaaS client secret the double puts in an Access application, which a
/// self-hosted one would not carry and an OIDC SaaS one does.
const APP_SECRET: &str = "apex-cf-saas-client-secret-77b2-do-not-leak";

/// §13.6's store, and the two secrets in it. The second name CONTAINS the
/// first, which is the whole hazard: the list endpoint's filter is `search`,
/// and `search` is a substring match.
const STORE_ID: &str = "8c8b1387108e49be85669169793e7bd2";
const SECRET_ID: &str = "3fd85f74b32742f1bff64a85009dda07";
const OLD_SECRET_ID: &str = "11112222333344445555666677778888";

/// What a test sends as a secret's value. Distinctive, so finding it in the
/// body the double received means the caller's own bytes arrived — and finding
/// it anywhere else means they leaked.
const SECRET_VALUE: &str = "apex-secret-store-value-5c2f-do-not-leak";

/// §13.11's two, as §13.1's file binds them. A gateway id is a slug the ACCOUNT
/// chose and a model name starts with `@` — neither is a shape any other id in
/// this provider has.
const GATEWAY_ID: &str = "apex-gateway";
const MODEL: &str = "@cf/meta/llama-3.1-8b-instruct";

/// The exporter credential the double puts on a gateway, nested inside an
/// array — which is where a real one is, and where a scrub that walked only
/// objects would never look.
const OTEL_SECRET: &str = "apex-otel-authorization-9f31-do-not-leak";

/// The connector token the double would hand back for a tunnel. Nothing in this
/// build asks for it; the constant is here so a test can prove that.
const TUNNEL_TOKEN: &str = "apex-cf-tunnel-token-3e9c-do-not-leak";

/// What the double stores for the one object a test reads back. Not JSON, and
/// it carries the credential the request arrived with, so a read that came back
/// unscrubbed would show it.
const OBJECT: &str = "-- apex backup\n-- fetched with {{authorization}}\nCREATE TABLE t (id INTEGER);\n";

/// The `wss://` URL the double hands back for a tail session. It carries its
/// own authorisation, which is why the provider must not pass it on.
const TAIL_URL: &str = "wss://tail.example.invalid/session/apex-tail-secret-91be";

/// §13.1's file, with the ids the REST API needs.
const PROJECT_FILE: &str = r#"
[identity.cloudflare]
account = "example-account"
account_id = "0123456789abcdef0123456789abcdef"

[cloudflare]
zone = "example.com"
zone_id = "fedcba9876543210fedcba9876543210"
buckets = ["example-assets"]

[cloudflare.d1]
project-db = "1a2b3c4d-5e6f-4a8b-9c0d-1e2f3a4b5c6d"

[cloudflare.kv]
cache = "00112233445566778899aabbccddeeff"

[cloudflare.queues]
jobs = "ffeeddccbbaa99887766554433221100"

[cloudflare.hyperdrive]
pg = "0f0e0d0c0b0a09080706050403020100"

[cloudflare.access]
dashboard = "f174e90a-fafe-4643-bbbc-4a0ed4fc8415"

[cloudflare.secrets]
app = "8c8b1387108e49be85669169793e7bd2"

[cloudflare.gateways]
main = "apex-gateway"

[cloudflare.models]
fast = "@cf/meta/llama-3.1-8b-instruct"

[cloudflare.tunnels]
office = "f70ff985-a4ef-4643-bbbc-4a0ed4fc8415"

# §13.7 needs three workers, because the lookup that a staged rollout depends
# on has three shapes and each of them has to be reachable: one version
# serving, none, and two already sharing.
[cloudflare.staging]
worker = "never-deployed"

# Unattended for the same reason production is, and it is the clearest
# demonstration of §13.8's polarity in this file: `canary` is not spelled
# `production`, and it is protected anyway until this line says otherwise.
[cloudflare.canary]
worker = "already-split"
unattended = true

[cloudflare.preview]
worker = "project-preview"

# §13.8: this fixture's project has opted INTO unattended production, so the
# deployment mechanics below can be exercised without an owner's approval in
# every one of them. The protection itself is tested against
# `PROTECTED_PROJECT_FILE`, which is this file without this line — and the
# tests that use it are the ones that go red if `protection()` stops being
# called at all.
[cloudflare.production]
worker = "project"
unattended = true
"#;

/// The version serving traffic in the double, and the one being rolled out.
/// Both are uuids because a Worker version id is one.
const OLD_VERSION: &str = "1c4dd6be-0000-4000-8000-abcdefabcdef";
const NEW_VERSION: &str = "2d5ee7cf-1111-4111-9111-bcdefabcdef0";

/// A stand-in for `wrangler` and for `terraform`.
///
/// It prints what it was given rather than doing anything, which is the whole
/// point: the question P1-012 has to answer is *what environment did the
/// broker build for this child*, and that cannot be measured from outside the
/// child. It echoes the credential deliberately — a test where the token never
/// comes back cannot tell a working scrub from a tool that printed nothing —
/// and it names `CARGO_PKG_NAME`, which cargo puts in the environment of the
/// process running these tests and which nothing in [`crate::broker::run_tool`]
/// puts in a child's, so a build that stopped clearing the environment would be
/// caught by that line turning from `<unset>` into a value. The test that reads
/// it refuses to run if its own environment does not carry it, because a test
/// that measures the absence of something that was never there measures
/// nothing.
const STUB_TOOL: &str = r#"#!/bin/sh
echo "apex-stub: $(basename "$0") $*"
echo "token=${CLOUDFLARE_API_TOKEN:-<unset>}"
echo "account=${CLOUDFLARE_ACCOUNT_ID:-<unset>}"
echo "cwd=$(pwd)"
echo "inherited=${CARGO_PKG_NAME:-<unset>}"
echo "home=${HOME:-<unset>}"
exit 0
"#;

/// One request the double saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    /// What the request said its body was. R2 puts an object's media type here
    /// and the provider chooses it from a table, so it is worth recording.
    content_type: Option<String>,
    /// Every header the request carried, lower-cased. §13.11 puts the gateway
    /// and the task's own id in headers, and a test cannot measure what the
    /// double does not record.
    headers: BTreeMap<String, String>,
    body: String,
}

/// A stand-in for `api.cloudflare.com`.
struct Fake {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// Whether the double behaves, or answers 401 with the header echoed back the
/// way a badly written API reports an auth failure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    EchoUnauthorized,
    /// An account whose stored credential may create account-owned tokens, so
    /// §13.4's exchange succeeds and the operation spends a token that did not
    /// exist a moment ago.
    ///
    /// A mode rather than the default, because it is a property of the
    /// *account* and not of this build: Cloudflare requires Super
    /// Administrator to create an account-owned token, so most stored
    /// credentials cannot, and [`Mode::Normal`] is the ordinary case where the
    /// exchange is attempted and comes back with nothing.
    Minting,
    /// An account that answers the permission-group list but refuses to create
    /// a token. The shape that separates "would not issue one" from "there is
    /// not one".
    MintingDenied,
    /// An account that refuses the permission-group list itself.
    ///
    /// A separate mode from [`Mode::MintingDenied`] because it is a separate
    /// refusal in a separate place: Cloudflare gates reading the list and
    /// creating a token on the same permission, so a credential that lacks it
    /// can be turned away at either, and a build that got one of the two right
    /// would look correct until an account turned it away at the other.
    ListDenied,
    /// An account that issues a token and then will not take the revoke back.
    /// The credential stands until it expires, and the trail has to say so.
    RevokeFails,
}

impl Mode {
    /// Whether the double knows about account-owned tokens at all.
    fn mints(self) -> bool {
        matches!(
            self,
            Mode::Minting | Mode::MintingDenied | Mode::ListDenied | Mode::RevokeFails
        )
    }
}

/// The token the double issues, and the id it issues it under. Distinct from
/// the stored credential so a test can say which of the two reached an
/// operation.
const MINTED: &str = "apex-minted-cf-6b31d0a4-do-not-leak";
const MINTED_ID: &str = "abcdef0123456789abcdef0123456789";

/// Whether a path is the credential exchange rather than an operation.
///
/// [`Fake::seen`] hides these and [`Fake::minting`] shows them, because they
/// answer different questions: *what did this operation do* and *what did it
/// cost to narrow the credential first*. A test about `dns.create` should not
/// have to know that the request before it asked which permission groups the
/// account has.
fn is_credential_exchange(path: &str) -> bool {
    let path = path.strip_prefix("/client/v4").unwrap_or(path);
    let path = path.split('?').next().unwrap_or(path);
    path.starts_with(&format!("/accounts/{ACCOUNT}/tokens"))
}

impl Fake {
    fn start(mode: Mode) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorder = Arc::clone(&recorder);
                std::thread::spawn(move || serve(stream, &recorder, mode));
            }
        });
        Fake { port, seen }
    }

    /// What the OPERATION did — every request except §13.4's credential
    /// exchange. See [`is_credential_exchange`] for why the two are apart.
    fn seen(&self) -> Vec<Seen> {
        self.everything()
            .into_iter()
            .filter(|s| !is_credential_exchange(&s.path))
            .collect()
    }

    /// What narrowing the credential cost: the permission-group list, the
    /// token creation, and the revoke.
    fn minting(&self) -> Vec<Seen> {
        self.everything()
            .into_iter()
            .filter(|s| is_credential_exchange(&s.path))
            .collect()
    }

    /// Both, in the order they happened. Only the tests that care about the
    /// ordering of one against the other use this.
    fn everything(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    fn authorizations(&self) -> Vec<String> {
        self.seen()
            .into_iter()
            .filter_map(|s| s.authorization)
            .collect()
    }
}

fn serve(mut stream: TcpStream, recorder: &Arc<Mutex<Vec<Seen>>>, mode: Mode) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut words = first.split_whitespace();
    let method = words.next().unwrap_or("").to_string();
    let path = words.next().unwrap_or("").to_string();

    let mut authorization = None;
    let mut content_type = None;
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return,
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.trim().strip_prefix("Authorization: ") {
            authorization = Some(value.to_string());
        }
        if let Some(value) = line.trim().strip_prefix("Content-Length: ") {
            length = value.trim().parse().unwrap_or(0);
        }
        if let Some(value) = line.trim().strip_prefix("Content-Type: ") {
            content_type = Some(value.to_string());
        }
        if let Some((name, value)) = line.trim().split_once(": ") {
            headers.insert(name.to_ascii_lowercase(), value.to_string());
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    recorder.lock().expect("lock").push(Seen {
        method: method.clone(),
        path: path.clone(),
        authorization: authorization.clone(),
        content_type: content_type.clone(),
        headers,
        body,
    });

    // The property that makes every 200 below mean something: with no
    // credential there is no answer.
    let Some(authorization) = authorization else {
        return reply(&mut stream, 401, r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"result":null}"#);
    };
    if mode == Mode::EchoUnauthorized {
        // What a badly written API does, and the case the framework's scrub
        // exists for. The provider does not scrub this; the framework does.
        let body = format!(
            r#"{{"success":false,"errors":[{{"code":10000,"message":"Authentication error: you sent {authorization}"}}],"result":null}}"#
        );
        return reply(&mut stream, 401, &body);
    }

    let (status, body) = if is_credential_exchange(&path) {
        tokens(&method, &path, mode)
    } else {
        answer(&method, &path)
    };
    // Every success carries the credential back too, in a `messages` entry.
    // Cloudflare does not do this; the point is that it would not matter if it
    // did, and a test where the token never comes back cannot tell a working
    // scrub from a missing one.
    let body = body.replace(
        r#""messages":[]"#,
        &format!(r#""messages":[{{"code":1,"message":"authenticated with {authorization}"}}]"#),
    );
    // ...and a reply that is not the envelope carries it too. R2 and KV answer
    // a read with the stored bytes and `application/octet-stream`, so there is
    // no `messages` array to put it in, and a scrub that only ever ran on JSON
    // would go unnoticed on exactly the two operations whose replies are not
    // JSON.
    let body = body.replace("{{authorization}}", &authorization);
    reply(&mut stream, status, &body);
}

/// §13.4's three endpoints, as the pinned schema documents them.
///
/// Kept apart from [`answer`] so that the ordinary surface and the credential
/// exchange cannot be confused for one another, and so the permission-group
/// list can be wrong in one specific way — see
/// `a_permission_group_this_build_does_not_recognise_is_not_a_refusal`.
fn tokens(method: &str, target: &str, mode: Mode) -> (u16, String) {
    let target = target.strip_prefix("/client/v4").unwrap_or(target);
    let path = target.split('?').next().unwrap_or(target);
    let base = format!("/accounts/{ACCOUNT}/tokens");
    let ok = |result: &str| {
        (
            200u16,
            format!(r#"{{"success":true,"errors":[],"messages":[],"result":{result}}}"#),
        )
    };
    // An account that cannot create tokens does not have a permission-group
    // list to show either: both are the same permission at Cloudflare.
    if !mode.mints() {
        return (
            404,
            r#"{"success":false,"errors":[{"code":7003,"message":"No route for that URI"}],"messages":[],"result":null}"#
                .to_string(),
        );
    }
    match (method, path) {
        ("GET", p) if p == format!("{base}/permission_groups") => {
            if mode == Mode::ListDenied {
                return (
                    403,
                    r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#
                        .to_string(),
                );
            }
            ok(&permission_groups())
        }
        ("POST", p) if p == base => {
            if mode == Mode::MintingDenied {
                return (
                    403,
                    r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"messages":[],"result":null}"#
                        .to_string(),
                );
            }
            ok(&format!(
                r#"{{"id":"{MINTED_ID}","name":"apex","status":"active","value":"{MINTED}"}}"#
            ))
        }
        ("DELETE", p) if p == format!("{base}/{MINTED_ID}") => {
            if mode == Mode::RevokeFails {
                return (
                    500,
                    r#"{"success":false,"errors":[{"code":1000,"message":"internal"}],"messages":[],"result":null}"#
                        .to_string(),
                );
            }
            ok(&format!(r#"{{"id":"{MINTED_ID}"}}"#))
        }
        _ => (
            404,
            r#"{"success":false,"errors":[{"code":7003,"message":"No route for that URI"}],"messages":[],"result":null}"#
                .to_string(),
        ),
    }
}

/// The account's permission groups, as `GET .../permission_groups` answers.
///
/// Every name [`super::temporary::POLICY`] can ask for, so that the tests
/// measure the exchange rather than a gap in the fixture — except one, which
/// is deliberately spelled the way this build does *not* expect, so there is a
/// row whose absence is a real absence. See
/// `a_permission_group_this_build_does_not_recognise_is_not_a_refusal`.
fn permission_groups() -> String {
    let mut groups: Vec<String> = Vec::new();
    let mut id = 0u32;
    let push = |name: &str, groups: &mut Vec<String>, id: &mut u32| {
        *id += 1;
        groups.push(format!(
            r#"{{"id":"{:032x}","name":"{name}","scopes":["com.cloudflare.api.account"]}}"#,
            0xcf00_0000u32 + *id
        ));
    };
    for name in super::temporary::POLICY
        .iter()
        .filter_map(|(_, policy)| match policy {
            super::temporary::Narrowest::Token { groups, .. } => Some(groups.iter()),
            super::temporary::Narrowest::Nothing(_) => None,
        })
        .flatten()
        .map(|slot| slot[0])
        .collect::<std::collections::BTreeSet<_>>()
    {
        // The one gap: an account that does not offer Hyperdrive at all.
        if name == "Hyperdrive Read" {
            continue;
        }
        push(name, &mut groups, &mut id);
    }
    format!("[{}]", groups.join(","))
}

/// The documented paths, with the documented shapes.
///
/// The query string is split off before matching: DNS is the only surface here
/// that carries one, and it carries it on the *lookup* that decides which
/// record a name means, so a double that matched on the whole request-target
/// would 404 exactly the request whose answer the mutation depends on.
fn answer(method: &str, target: &str) -> (u16, String) {
    let target = target.strip_prefix("/client/v4").unwrap_or(target);
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    let param = |name: &str| -> String {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .unwrap_or("")
            .to_string()
    };
    let ok = |result: &str| {
        (
            200u16,
            format!(r#"{{"success":true,"errors":[],"messages":[],"result":{result}}}"#),
        )
    };
    match (method, path) {
        ("GET", "/accounts") => ok(&format!(
            r#"[{{"id":"{ACCOUNT}","name":"example-account"}}]"#
        )),
        ("GET", p) if p == format!("/accounts/{ACCOUNT}") => {
            ok(&format!(r#"{{"id":"{ACCOUNT}","name":"example-account"}}"#))
        }
        ("GET", p) if p == format!("/zones/{ZONE}/workers/routes") => ok(
            r#"[{"id":"route1","pattern":"example.com/*","script":"project"},
                {"id":"route2","pattern":"admin.example.com/*","script":"somebody-else"}]"#,
        ),
        ("GET", p) if p.ends_with("/settings") => {
            // A binding that is already there. `secret.bind` sends the whole
            // list back, so a build that forgot to merge would take this one
            // off the worker — silently, and only noticed in production.
            //
            // The `secret_text` one is the hazard: its `text` is `writeOnly`
            // and REQUIRED, so the API returns it without a value and a build
            // that echoed the object back would write an empty secret or be
            // refused for a missing field.
            ok(
                r#"{"bindings":[{"type":"plain_text","name":"GREETING","text":"hi"},{"type":"secret_text","name":"OLD_SECRET"}],"compatibility_date":"2026-09-01","usage_model":"standard"}"#,
            )
        }
        // §13.7: which version is in front of traffic. The worker named
        // `never-deployed` has an empty list and `already-split` has two
        // versions sharing, so the three shapes the lookup has to tell apart
        // are all reachable without a second double.
        ("GET", p) if p.ends_with("/deployments") => {
            if p.contains("/never-deployed/") {
                return ok(r#"{"deployments":[]}"#);
            }
            if p.contains("/already-split/") {
                return ok(&format!(
                    r#"{{"deployments":[{{"id":"d1","strategy":"percentage","versions":[
                        {{"version_id":"{OLD_VERSION}","percentage":80}},
                        {{"version_id":"{NEW_VERSION}","percentage":20}}]}}]}}"#
                ));
            }
            ok(&format!(
                r#"{{"deployments":[{{"id":"d1","strategy":"percentage","versions":[
                    {{"version_id":"{OLD_VERSION}","percentage":100}}]}}]}}"#
            ))
        }
        ("POST", p) if p.ends_with("/versions") => {
            ok(r#"{"id":"1c4dd6be-0000-4000-8000-abcdefabcdef","number":7}"#)
        }
        ("POST", p) if p.starts_with("/accounts/") && p.contains("/deployments") => {
            ok(r#"{"id":"dep-9","strategy":"percentage","versions":[{"version_id":"1c4dd6be-0000-4000-8000-abcdefabcdef","percentage":100}]}"#)
        }
        ("POST", p) if p.ends_with("/tails") => ok(&format!(
            r#"{{"id":"tail-1","url":"{TAIL_URL}","expires_at":"2026-09-07T00:00:00Z"}}"#
        )),

        // ── §13.5, R2 ───────────────────────────────────────────────────────
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/r2/buckets") => ok(
            r#"{"name":"example-assets","location":"apac","storage_class":"Standard","creation_date":"2026-09-12T00:00:00.000Z"}"#,
        ),
        ("GET", p) if p == objects_path("") => ok(
            r#"[{"key":"db/today.sql","size":19,"etag":"aa","last_modified":"2026-09-12T00:00:00.000Z"}]"#,
        ),
        // A read answers with the object's own bytes and no envelope at all —
        // `application/octet-stream`, which is what the documented endpoint
        // returns on success even though its failures are JSON.
        ("GET", p) if p.starts_with(&objects_path("/")) => (200, OBJECT.to_string()),
        ("PUT", p) if p.starts_with(&objects_path("/")) => {
            ok(r#"{"key":"db/today.sql","size":19,"etag":"bb","version":"v2"}"#)
        }

        // ── §13.3's storage surfaces ────────────────────────────────────────
        ("GET", p) if p == format!("/accounts/{ACCOUNT}/d1/database/{DB_ID}") => ok(&format!(
            r#"{{"uuid":"{DB_ID}","name":"project-db","num_tables":3,"file_size":16384}}"#
        )),
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/d1/database/{DB_ID}/query") => ok(
            r#"[{"success":true,"results":[{"n":1}],"meta":{"changes":0,"duration":0.4}}]"#,
        ),
        // A KV read answers with the value's bytes and no envelope, the same
        // way an R2 object read does.
        ("GET", p) if p.starts_with(&kv_path("")) => (200, KV_VALUE.to_string()),
        ("PUT", p) if p.starts_with(&kv_path("")) => ok("null"),
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/queues/{QUEUE_ID}/messages") => {
            ok(r#"{"errors":[],"messages":[]}"#)
        }
        ("PATCH", p) if p == format!("/accounts/{ACCOUNT}/queues/{QUEUE_ID}") => ok(&format!(
            r#"{{"queue_id":"{QUEUE_ID}","queue_name":"jobs","settings":{{"delivery_paused":true}}}}"#
        )),
        // The documentation says an origin password is write-only and never
        // comes back. This double sends one anyway: what a build does when the
        // far side hands it a secret it did not ask for is not something to
        // find out in production.
        ("GET", p) if p == format!("/accounts/{ACCOUNT}/hyperdrive/configs/{HD_ID}") => ok(&format!(
            r#"{{"id":"{HD_ID}","name":"pg","origin":{{"host":"db.example.invalid","port":5432,"database":"app","user":"app","password":"{ORIGIN_PASSWORD}","scheme":"postgres"}},"caching":{{"disabled":false}}}}"#
        )),
        ("PATCH", p) if p == format!("/accounts/{ACCOUNT}/hyperdrive/configs/{HD_ID}") => {
            ok(&format!(r#"{{"id":"{HD_ID}","name":"pg","caching":{{"disabled":false}}}}"#))
        }

        // ── §13.11, Workers AI and the AI Gateway ───────────────────────────
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/ai/run/{MODEL}") => {
            ok(r#"{"response":"Cloudflare is a network.","usage":{"prompt_tokens":9,"completion_tokens":6}}"#)
        }
        // **Every value here is one no fallback could have produced.** The
        // read-modify-write fills in five defaults for fields the schema marks
        // required, so a gateway answering with those same defaults would make
        // "it was preserved" and "it was defaulted" the same observation — and
        // a mutation that dropped the read entirely stayed green against an
        // earlier version of this reply. `rate_limiting_limit` is 42 against a
        // fallback of 0, `cache_invalidate_on_update` is true against false,
        // and `logpush` and `retry_max_attempts` are not defaulted at all.
        ("GET", p) if p == gateway_path() => ok(&format!(
            r#"{{"id":"{GATEWAY_ID}","cache_ttl":60,"collect_logs":true,"rate_limiting_limit":42,"rate_limiting_interval":60,"cache_invalidate_on_update":true,"logpush":true,"retry_max_attempts":3,"created_at":"2026-09-01T00:00:00Z","modified_at":"2026-09-01T00:00:00Z","is_default":false}}"#
        )),
        ("GET", p) if p == gateway_path_of("with-otel") => ok(&format!(
            r#"{{"id":"with-otel","cache_ttl":60,"collect_logs":true,"rate_limiting_limit":0,"rate_limiting_interval":0,"cache_invalidate_on_update":false,"otel":[{{"url":"https://otel.example.invalid","authorization":"{OTEL_SECRET}"}}]}}"#
        )),
        ("PUT", p) if p == gateway_path() => ok(&format!(
            r#"{{"id":"{GATEWAY_ID}","cache_ttl":300,"collect_logs":true}}"#
        )),

        // ── §13.6, the Secrets Store ────────────────────────────────────────
        //
        // Every reply is METADATA. `secrets-store_value` is `writeOnly` in
        // Cloudflare's own schema — "the API never returns this value" — so a
        // double that echoed a value back would be testing against an API that
        // does not exist.
        ("POST", p) if p == secrets_path("") => ok(&format!(
            r#"[{{"id":"{SECRET_ID}","name":"API_KEY","store_id":"{STORE_ID}","status":"active","scopes":["workers"],"created":"2026-09-12T00:00:00Z"}}]"#
        )),
        ("GET", p) if p == secrets_path("") => {
            // `search` is a SUBSTRING match, and this double behaves like one.
            // A build that trusted the filter would rotate whichever of these
            // came back first.
            let wanted = param("search");
            let one = |id: &str, name: &str| {
                format!(
                    r#"{{"id":"{id}","name":"{name}","store_id":"{STORE_ID}","status":"active","scopes":["workers"]}}"#
                )
            };
            let mut found: Vec<String> = Vec::new();
            for (id, name) in [(SECRET_ID, "API_KEY"), (OLD_SECRET_ID, "API_KEY_OLD")] {
                if wanted.is_empty() || name.contains(&wanted) {
                    found.push(one(id, name));
                }
            }
            let n = found.len();
            (
                200,
                format!(
                    r#"{{"success":true,"errors":[],"messages":[],"result":[{}],"result_info":{{"count":{n},"page":1,"per_page":100,"total_count":{n}}}}}"#,
                    found.join(",")
                ),
            )
        }
        ("PATCH", p) if p == secrets_path(&format!("/{SECRET_ID}")) => ok(&format!(
            r#"{{"id":"{SECRET_ID}","name":"API_KEY","store_id":"{STORE_ID}","status":"active","scopes":["workers"]}}"#
        )),
        ("PATCH", p) if p.ends_with("/settings") => {
            ok(r#"{"bindings":[],"compatibility_date":"2026-09-01"}"#)
        }

        // ── §13.10, Cloudflare One ──────────────────────────────────────────
        //
        // The Access application is an OIDC SaaS one, so it carries a
        // `client_secret` the documented schema really does return. A double
        // that answered with a self-hosted application would let a build with
        // no scrub at all pass.
        ("GET", p) if p == format!("/accounts/{ACCOUNT}/access/apps/{APP_ID}") => ok(&format!(
            r#"{{"id":"{APP_ID}","name":"dashboard","domain":"admin.example.com","type":"saas","saas_app":{{"client_id":"{CLIENT_ID}","client_secret":"{APP_SECRET}","auth_type":"oidc"}}}}"#
        )),
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/access/apps/{APP_ID}/revoke_tokens") => {
            ok("true")
        }
        ("POST", p) if p == format!("/accounts/{ACCOUNT}/access/service_tokens") => (
            201,
            format!(
                r#"{{"success":true,"errors":[],"messages":[],"result":{{"id":"1e0b9a4c-0000-4000-8000-abcdefabcdef","name":"ci","client_id":"{CLIENT_ID}","client_secret":"{CLIENT_SECRET}","duration":"8760h"}}}}"#
            ),
        ),
        ("GET", p) if p == format!("/accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL_ID}") => ok(&format!(
            r#"{{"id":"{TUNNEL_ID}","name":"office","status":"healthy","connections":[],"created_at":"2026-09-01T00:00:00Z"}}"#
        )),
        ("PATCH", p) if p == format!("/accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL_ID}") => ok(&format!(
            r#"{{"id":"{TUNNEL_ID}","name":"branch-office","status":"healthy"}}"#
        )),
        // The endpoint this build does not declare. It answers, so that a test
        // asserting the token never comes back is measuring a refusal to ask
        // rather than a double that had nothing to give.
        ("GET", p) if p == format!("/accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL_ID}/token") => {
            ok(&format!(r#""{TUNNEL_TOKEN}""#))
        }

        // ── §13.9, DNS ──────────────────────────────────────────────────────
        //
        // The zone answers about the name it was asked about, and each name
        // below is one of the five things a lookup can run into. A double that
        // only ever answered "here is your record" would let a build that
        // could not tell a 403 from an empty zone pass every test in this file.
        ("GET", p) if p == records_path() => {
            let name = param("name.exact");
            let kind = param("type");
            let one = |id: &str, name: &str, content: &str| {
                format!(r#"{{"id":"{id}","name":"{name}","type":"{kind}","content":"{content}","ttl":1,"proxied":false}}"#)
            };
            match name.as_str() {
                // Denied. A body that looks like every other failure, with the
                // one documented denial signal in it.
                "denied.example.com" => (
                    403,
                    r#"{"success":false,"errors":[{"code":10000,"message":"Forbidden","documentation_url":"https://developers.cloudflare.com/api/resources/dns/subresources/records/methods/list"}],"messages":[],"result":null}"#.to_string(),
                ),
                // Absent: the zone answered, successfully, with nothing.
                "gone.example.com" => ok("[]"),
                // Ambiguous: two A records at one name is ordinary
                // round-robin, and picking either would be a guess.
                "many.example.com" => ok(&format!(
                    "[{},{}]",
                    one(RECORD_A, "many.example.com", "203.0.113.1"),
                    one(RECORD_B, "many.example.com", "203.0.113.2")
                )),
                // A zone that ignored the filter and answered with the whole
                // thing. The build has to notice that none of it is the record
                // it asked for.
                "unfiltered.example.com" => ok(&format!(
                    "[{},{}]",
                    one(RECORD_A, "www.example.com", "203.0.113.1"),
                    one(RECORD_B, "other.example.com", "203.0.113.2")
                )),
                other => ok(&format!("[{}]", one(RECORD_A, other, "203.0.113.1"))),
            }
        }
        ("POST", p) if p == records_path() => ok(&format!(
            r#"{{"id":"{RECORD_A}","name":"new.example.com","type":"A","content":"203.0.113.7"}}"#
        )),
        ("PATCH", p) if p == format!("{}/{RECORD_A}", records_path()) => ok(&format!(
            r#"{{"id":"{RECORD_A}","name":"www.example.com","type":"A","content":"203.0.113.8"}}"#
        )),
        ("DELETE", p) if p == format!("{}/{RECORD_A}", records_path()) => {
            ok(&format!(r#"{{"id":"{RECORD_A}"}}"#))
        }

        _ => (
            404,
            format!(
                r#"{{"success":false,"errors":[{{"code":7003,"message":"no route for {method} {path}"}}],"result":null}}"#
            ),
        ),
    }
}

/// The password the double puts in a Hyperdrive reply, which the documented
/// API never would. Distinctive, so finding it anywhere downstream means it
/// travelled rather than merely resembling something.
const ORIGIN_PASSWORD: &str = "apex-hyperdrive-origin-4c7f-do-not-leak";

/// Where the double keeps one namespace's values.
fn kv_path(suffix: &str) -> String {
    format!("/accounts/{ACCOUNT}/storage/kv/namespaces/{KV_ID}/values{suffix}")
}

/// Where this project's one gateway lives.
fn gateway_path() -> String {
    gateway_path_of(GATEWAY_ID)
}

fn gateway_path_of(id: &str) -> String {
    format!("/accounts/{ACCOUNT}/ai-gateway/gateways/{id}")
}

/// Where the double keeps this store's secrets.
fn secrets_path(suffix: &str) -> String {
    format!("/accounts/{ACCOUNT}/secrets_store/stores/{STORE_ID}/secrets{suffix}")
}

/// Where the double keeps this zone's records.
fn records_path() -> String {
    format!("/zones/{ZONE}/dns_records")
}

fn reply(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    // An object's bytes are not the envelope, and saying they are would be the
    // one lie in this double that a provider could come to depend on.
    let kind = if body.starts_with('{') || body.starts_with('[') {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// The prefix every R2 object path in the double shares, with `suffix` after
/// `objects` — `""` for the bucket's listing, `"/"` for one object.
fn objects_path(suffix: &str) -> String {
    format!("/accounts/{ACCOUNT}/r2/buckets/{BUCKET}/objects{suffix}")
}

struct Fixture {
    service: Service,
    store: PathBuf,
    project: PathBuf,
    fake: Fake,
    tools: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.store).ok();
        std::fs::remove_dir_all(&self.project).ok();
        std::fs::remove_dir_all(&self.tools).ok();
    }
}

fn me() -> Peer {
    // Safe: getuid/getgid cannot fail.
    Peer {
        pid: std::process::id() as libc::pid_t,
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    }
}

impl Fixture {
    /// A service serving ONLY the Cloudflare provider, with a credential
    /// stored for the double and a project bound the way §13.1 says.
    fn new(name: &str, mode: Mode, granted: &[&str]) -> Fixture {
        Fixture::with_project(name, mode, granted, PROJECT_FILE)
    }

    /// The same, with a project file of the caller's choosing. §13.4's
    /// strength is a line in that file, so a test of it needs a different one.
    fn with_project(name: &str, mode: Mode, granted: &[&str], file: &str) -> Fixture {
        let fake = Fake::start(mode);
        let tag = format!("{name}-{}-{}", std::process::id(), fake.port);
        let store = std::env::temp_dir().join(format!("apex-cf-store-{tag}"));
        let project = std::env::temp_dir().join(format!("apex-cf-project-{tag}"));
        std::fs::remove_dir_all(&store).ok();
        std::fs::remove_dir_all(&project).ok();
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(project.join("apex.toml"), file).expect("apex.toml");

        // Neither `wrangler` nor `terraform` is installed on the machine this
        // was built on, and a build that could only run the real ones could
        // not be tested at all — the same reason `Api` is a field. Each stub
        // prints what it was given, so a test can measure the environment its
        // child was built with rather than trusting that it was.
        let tools = std::env::temp_dir().join(format!("apex-cf-tools-{tag}"));
        std::fs::remove_dir_all(&tools).ok();
        std::fs::create_dir_all(&tools).expect("tools");
        for name in ["wrangler", "terraform"] {
            let path = tools.join(name);
            std::fs::write(&path, STUB_TOOL).expect("stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let mut registry = Registry::new();
        registry
            .register(Box::new(CloudflareProvider::at(fake.port).with_tools(&tools)))
            .expect("register");
        let service = Service::new(Store::new(store.clone()), false, registry);

        let peer = me();
        // `http` is allowed for a loopback host: nothing crosses a network.
        assert_eq!(
            service.add(
                peer,
                NewService {
                    service: "cloudflare",
                    host: "127.0.0.1",
                    scheme: "http",
                    username: None,
                    path: "",
                    auth: None,
                    port: None,
                },
                SecretValue::new(TOKEN.as_bytes().to_vec()),
            ),
            Response::Ok
        );
        for operation in granted {
            assert!(service
                .grant(
                    peer,
                    project.to_str().expect("utf8"),
                    "cloudflare",
                    operation,
                    false
                )
                .as_error()
                .is_none());
        }
        Fixture {
            service,
            store,
            project,
            fake,
            tools,
        }
    }

    fn record(&self, operation: &str, resource: &str) -> CapabilityRecord {
        let mut rec = CapabilityRecord::new("cloudflare", operation, resource);
        rec.project = Some(self.project.to_string_lossy().into_owned());
        rec
    }

    fn use_it(&self, record: CapabilityRecord) -> Response {
        self.service.use_capability(me(), record, Vec::new())
    }

    /// The same, with bytes after the request line — which is how a secret's
    /// value travels, and the reason it is not an option.
    fn use_with_input(&self, record: CapabilityRecord, input: &str) -> Response {
        self.service
            .use_capability(me(), record, input.as_bytes().to_vec())
    }

    fn trail(&self) -> String {
        std::fs::read_to_string(Store::new(self.store.clone()).audit_path()).unwrap_or_default()
    }
}

/// One row of [`every_operation`]: the operation id, the resource to ask it
/// for, the options its own declaration accepts, and **how many authenticated
/// requests performing it should take**.
///
/// The count is a field rather than an assumption because it stopped being one
/// everywhere. `dns.update` and `dns.delete` ask the zone which record a name
/// means before changing it, so they are two requests each — and a test that
/// asserted "one call per operation" would either have to be loosened into
/// meaninglessness or be wrong. Written down, it stays a measurement: an
/// operation that quietly started making an extra call would fail here.
type OperationCase = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
    usize,
);

/// Every operation the provider declares, with a resource and options that its
/// own declaration accepts.
fn every_operation() -> Vec<OperationCase> {
    vec![
        // P1-012's four make NO authenticated request: the operation is a
        // child process, not a call. They are in this table anyway, and the
        // count is 0 rather than the row being absent, because the sweep below
        // asserts that every declared operation left a trail line — and an
        // operation missing from here would quietly stop being swept.
        ("cloudflare.wrangler.deploy", "project", vec![], 0),
        ("cloudflare.wrangler.versions-upload", "project", vec![], 0),
        ("cloudflare.terraform.plan", "", vec![], 0),
        ("cloudflare.terraform.apply", "", vec![], 0),
        ("cloudflare.account.read", "", vec![], 1),
        ("cloudflare.worker.read", "project", vec![], 1),
        (
            "cloudflare.worker.upload-version",
            "project",
            vec![
                ("script", "dist/worker.js"),
                ("compatibility-date", "2026-09-01"),
                ("message", "released by apex"),
            ],
            1,
        ),
        (
            "cloudflare.worker.deploy",
            "project",
            vec![("version", "1c4dd6be-0000-4000-8000-abcdefabcdef")],
            1,
        ),
        (
            "cloudflare.worker.rollback",
            "project",
            vec![("version", "0a0b0c0d-0000-4000-8000-000000000000")],
            1,
        ),
        ("cloudflare.worker.tail", "project", vec![], 1),
        ("cloudflare.worker.route.read", "project", vec![], 1),
        ("cloudflare.r2.object.read", "example-assets/db/today.sql", vec![], 1),
        (
            "cloudflare.r2.object.write",
            "example-assets/db/today.sql",
            vec![("file", "dist/today.sql")],
            1,
        ),
        (
            "cloudflare.r2.bucket.create",
            "example-assets",
            vec![("location", "apac"), ("storage-class", "Standard")],
            1,
        ),
        ("cloudflare.d1.read", "project-db", vec![], 1),
        ("cloudflare.d1.query", "project-db", vec![("sql", "SELECT 1")], 1),
        (
            "cloudflare.d1.migrate",
            "project-db",
            vec![("file", "migrations/001.sql")],
            1,
        ),
        ("cloudflare.kv.read", "cache/greeting", vec![], 1),
        ("cloudflare.kv.write", "cache/greeting", vec![("value", "hello")], 1),
        ("cloudflare.queue.publish", "jobs", vec![("message", "work to do")], 1),
        ("cloudflare.queue.manage", "jobs", vec![("paused", "true")], 1),
        ("cloudflare.hyperdrive.read", "pg", vec![], 1),
        ("cloudflare.hyperdrive.edit", "pg", vec![("caching", "on")], 1),
        (
            "cloudflare.secret.create",
            "app/API_KEY",
            vec![("scopes", "workers"), ("file", "secrets/value.txt")],
            1,
        ),
        (
            "cloudflare.secret.rotate",
            "app/API_KEY",
            vec![("file", "secrets/value.txt")],
            2,
        ),
        (
            "cloudflare.secret.bind",
            "app/API_KEY",
            vec![("worker", "project"), ("binding", "API_KEY")],
            2,
        ),
        ("cloudflare.workers-ai.run", "fast", vec![("prompt", "What is Cloudflare?")], 1),
        (
            "cloudflare.ai-gateway.run",
            "main/fast",
            vec![("prompt", "What is Cloudflare?")],
            1,
        ),
        ("cloudflare.ai-gateway.edit", "main", vec![("cache-ttl", "300")], 2),
        ("cloudflare.access.read", "dashboard", vec![], 1),
        ("cloudflare.access.edit", "dashboard", vec![], 1),
        (
            "cloudflare.access.service-token.create",
            "admin.example.com",
            vec![("name", "ci"), ("duration", "8760h")],
            1,
        ),
        ("cloudflare.tunnel.read", "office", vec![], 1),
        ("cloudflare.tunnel.edit", "office", vec![("name", "branch-office")], 1),
        ("cloudflare.dns.read", "www.example.com", vec![("type", "A")], 1),
        (
            "cloudflare.dns.create",
            "new.example.com",
            vec![
                ("type", "A"),
                ("content", "203.0.113.7"),
                ("ttl", "300"),
                ("proxied", "false"),
                ("comment", "added by apex"),
            ],
            1,
        ),
        (
            "cloudflare.dns.update",
            "www.example.com",
            vec![("type", "A"), ("content", "203.0.113.8")],
            2,
        ),
        ("cloudflare.dns.delete", "old.example.com", vec![("type", "A")], 2),
    ]
}

/// How many authenticated requests the whole surface should take.
fn expected_calls() -> usize {
    every_operation().iter().map(|(_, _, _, calls)| calls).sum()
}

/// The files the project holds for the operations that upload one: a Worker
/// module, and something to put in a bucket.
fn with_files(fixture: &Fixture) {
    std::fs::create_dir_all(fixture.project.join("dist")).expect("dist");
    std::fs::write(
        fixture.project.join("dist/worker.js"),
        "export default { fetch: () => new Response('hi') };\n",
    )
    .expect("worker.js");
    std::fs::write(fixture.project.join("dist/today.sql"), UPLOADED).expect("today.sql");
    std::fs::create_dir_all(fixture.project.join("migrations")).expect("migrations");
    std::fs::write(fixture.project.join("migrations/001.sql"), MIGRATION).expect("001.sql");
    std::fs::create_dir_all(fixture.project.join("secrets")).expect("secrets");
    std::fs::write(fixture.project.join("secrets/value.txt"), SECRET_VALUE).expect("value.txt");
}

/// A migration: several statements over several lines, which is exactly what a
/// `Syntax::Text` parameter could not have carried.
const MIGRATION: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY);\nINSERT INTO t VALUES (1);\n";

/// What a test puts into a bucket. Distinctive, so finding it in the body the
/// double received means the project's own file arrived and not something that
/// merely has the right length.
const UPLOADED: &str = "INSERT INTO t VALUES (1);\n";

fn granted_everything() -> Vec<&'static str> {
    SPEC.operations.iter().map(|op| op.id).collect()
}

#[test]
fn the_double_answers_nothing_without_a_credential() {
    // The assumption every other test in this file rests on. If the double
    // answered 200 to an unauthenticated request, a provider that never sent
    // the token would still pass, which is the failure mode this whole
    // program keeps finding.
    let fake = Fake::start(Mode::Normal);
    let mut stream = TcpStream::connect(("127.0.0.1", fake.port)).expect("connect");
    write!(
        stream,
        "GET /client/v4/accounts HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .expect("write");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("read");
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    assert!(answer.contains("Authentication error"), "{answer}");
}

#[test]
fn every_declared_operation_reaches_cloudflare_with_the_credential_and_returns_without_it() {
    // P1-002's third criterion, as a measurement rather than an assertion.
    // The agent side is handed the whole operation surface; for each one the
    // credential is shown to have reached the far side, and shown not to be in
    // the reply, the output, the serialised response or the audit trail.
    let f = Fixture::new("surface", Mode::Normal, &granted_everything());
    with_files(&f);

    for (operation, resource, options, _) in every_operation() {
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        let Response::Performed {
            endpoint,
            exit_code,
            output,
            record,
        } = &reply
        else {
            panic!("{operation} was refused: {reply:?}");
        };
        assert_eq!(*exit_code, 0, "{operation}: {output}");
        assert_eq!(endpoint, "http://127.0.0.1", "{operation}");
        assert_eq!(record.operation, operation);
        assert_eq!(record.approval_policy, "grant");
        assert!(!output.contains(TOKEN), "{operation} handed back the token");
        // The far side echoed it in every reply, so an unscrubbed answer would
        // contain it and a scrubbed one says where it was.
        assert!(
            output.contains("«redacted»"),
            "{operation}: the reply was not scrubbed, it just had nothing in it"
        );
        assert!(
            !serde_json::to_string(&reply).unwrap().contains(TOKEN),
            "{operation}: the serialised reply carries the token"
        );
    }

    // One authenticated request per operation, every one of them carrying the
    // stored credential. A provider that exited zero without calling anything
    // would leave this empty.
    let authorizations = f.fake.authorizations();
    assert_eq!(
        authorizations.len(),
        expected_calls(),
        "every request the surface makes, and no others: {authorizations:?}"
    );
    for authorization in &authorizations {
        assert_eq!(authorization, &format!("Bearer {TOKEN}"));
    }

    let trail = f.trail();
    assert!(!trail.contains(TOKEN), "the audit trail holds the credential");
    assert_eq!(
        trail.matches("\"event\":\"used\"").count(),
        SPEC.operations.len(),
        "every operation should have left a used line"
    );
    // One trail line per operation even where an operation took two requests:
    // the trail records what was authorised, not what the wire carried.
    assert!(expected_calls() > SPEC.operations.len());
}

#[test]
fn the_agent_side_holds_no_credential_even_when_it_holds_every_operation() {
    // The same criterion from the other end: with every capability granted,
    // there is no request in the protocol whose reply carries a value, and the
    // store the value is in is not the caller's to read. `Response` cannot
    // even be constructed with one — `SecretValue` implements no `Serialize` —
    // so this checks the reachable surface rather than restating the type.
    let f = Fixture::new("noread", Mode::Normal, &granted_everything());
    with_files(&f);
    let peer = me();

    let mut replies = vec![
        f.service.hello(),
        f.service.list(peer),
        f.service.grants(peer),
        f.service.audit(peer, 100, None),
    ];
    for (operation, resource, options, _) in every_operation() {
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        replies.push(f.use_it(rec));
    }

    for reply in &replies {
        let text = serde_json::to_string(reply).expect("every reply serialises");
        assert!(
            !text.contains(TOKEN),
            "{} carried the credential",
            reply.variant()
        );
    }
    // ...and it did reach Cloudflare, so this is a brokered operation and not
    // a set of calls that never happened.
    assert_eq!(f.fake.authorizations().len(), expected_calls());
}

#[test]
fn an_api_that_echoes_the_credential_back_does_not_get_to_hand_it_over() {
    // The scrub, against a Cloudflare credential, on the path a real API takes
    // when it reports an auth failure badly. The provider does nothing here;
    // the framework does it, which is what makes it hold for the next provider
    // as well.
    let f = Fixture::new("echo", Mode::EchoUnauthorized, &["cloudflare.account.read"]);
    let reply = f.use_it(f.record("cloudflare.account.read", ""));
    let Response::Performed { exit_code, output, .. } = &reply else {
        panic!("expected a result carrying the failure: {reply:?}");
    };
    assert_eq!(*exit_code, 1, "a 401 is not a success");
    assert!(output.contains("HTTP 401"), "{output}");
    // The far side put the credential in the body; it is not in the answer.
    assert!(!output.contains(TOKEN), "{output}");
    assert!(output.contains("«redacted»"), "{output}");
    assert!(!serde_json::to_string(&reply).unwrap().contains(TOKEN));
    assert!(!f.trail().contains(TOKEN), "the trail holds the credential");
    // It really was sent, so the echo really was of the real thing.
    assert_eq!(f.fake.authorizations(), vec![format!("Bearer {TOKEN}")]);

    // The line this real path composed is the line `credential_refused` reads
    // — the two are four hundred lines apart and only `answered_http` holds
    // them together. `only_a_401_is_the_far_side_refusing_the_credential_itself`
    // builds its own input and so cannot see them drift; this one cannot miss
    // it, because the string here came out of `perform`. A build where they
    // drifted would go back to reporting a minted token that has not
    // propagated yet as the operation's own answer, which is the live defect
    // of 2026-09-12.
    assert!(
        CloudflareProvider::at(1).credential_refused(&Performed {
            code: *exit_code,
            output: output.clone(),
            created: None,
        }),
        "perform's own 401 line is not the one credential_refused reads: {output}"
    );
}

#[test]
fn a_tail_session_url_is_a_credential_and_does_not_come_back() {
    // `POST …/tails` answers with a wss:// URL that authorises whoever holds
    // it to read the worker's live logs. Returning it would hand the agent a
    // credential by another name.
    let f = Fixture::new("tail", Mode::Normal, &["cloudflare.worker.tail"]);
    let reply = f.use_it(f.record("cloudflare.worker.tail", "project"));
    let Response::Performed { output, .. } = &reply else {
        panic!("refused: {reply:?}");
    };
    // The session was created — the id and the expiry are there.
    assert!(output.contains("tail-1"), "{output}");
    assert!(output.contains("expires_at"), "{output}");
    // ...and the URL is not.
    assert!(!output.contains(TAIL_URL), "the tail URL came back: {output}");
    assert!(!output.contains("wss://"), "{output}");
    assert!(output.contains("not returned"), "{output}");
    assert!(!f.trail().contains(TAIL_URL));
    // The double really did send one, so this is a removal and not an absence.
    let sent = f.fake.seen();
    assert!(sent.iter().any(|s| s.path.ends_with("/tails")), "{sent:?}");
}

#[test]
fn a_name_this_project_did_not_bind_never_reaches_cloudflare() {
    // The semantic check. `project` is bound to production; `somebody-elses`
    // is not bound to anything, and the difference is decided before a
    // credential is read.
    let f = Fixture::new("unbound", Mode::Normal, &granted_everything());
    let reply = f.use_it(f.record("cloudflare.worker.read", "somebody-elses-worker"));
    let (kind, message) = reply.as_error().expect("an unbound name must be refused");
    assert_eq!(kind, ErrorKind::BadRequest);
    assert!(message.contains("somebody-elses-worker"), "{message}");
    assert!(message.contains("apex.toml"), "{message}");
    assert!(message.contains("project and project-preview"), "{message}");
    assert!(
        f.fake.seen().is_empty(),
        "a refused request still reached the api"
    );
}

#[test]
fn a_project_that_binds_no_cloudflare_account_is_told_what_to_write() {
    // A project with no binding at all. The account read still works, because
    // it is how you find out what to put in the file; everything else refuses
    // with the file to edit named.
    let f = Fixture::new("nobinding", Mode::Normal, &granted_everything());
    std::fs::write(f.project.join("apex.toml"), "[something.else]\nkey = \"v\"\n").expect("write");

    let reply = f.use_it(f.record("cloudflare.worker.deploy", "project").param("version", "v1"));
    let (_, message) = reply.as_error().expect("refused");
    assert!(message.contains("[identity.cloudflare]"), "{message}");

    // ...and the discovery path still answers, against /accounts.
    let reply = f.use_it(f.record("cloudflare.account.read", ""));
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");
    assert_eq!(f.fake.seen()[0].path, "/client/v4/accounts");
}

#[test]
fn the_binding_decides_which_account_and_which_worker_the_call_names() {
    // "Account/zone/project binding works", as a path rather than a promise:
    // the account id and the worker name in the URL came out of apex.toml, and
    // the zone id in the routes call came out of the same file.
    let f = Fixture::new("paths", Mode::Normal, &granted_everything());
    f.use_it(f.record("cloudflare.worker.read", "project-preview"));
    f.use_it(f.record("cloudflare.worker.route.read", "project"));

    let paths: Vec<String> = f.fake.seen().into_iter().map(|s| s.path).collect();
    assert_eq!(
        paths,
        vec![
            format!("/client/v4/accounts/{ACCOUNT}/workers/scripts/project-preview/settings"),
            format!("/client/v4/zones/{ZONE}/workers/routes"),
        ]
    );
}

#[test]
fn an_upload_carries_the_project_s_own_module_and_the_metadata_that_names_it() {
    // The multipart body is built here rather than by curl's `-F`, so this is
    // where it gets checked: the module arrives, the metadata names it, and
    // the annotation goes in the body rather than on a command line.
    let f = Fixture::new("upload", Mode::Normal, &["cloudflare.worker.upload-version"]);
    with_files(&f);
    let reply = f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "dist/worker.js")
            .param("compatibility-date", "2026-09-01")
            .param("message", "released by apex"),
    );
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");

    let sent = f.fake.seen();
    let upload = sent.first().expect("one request");
    assert_eq!(upload.method, "POST");
    assert!(upload.path.ends_with("/versions"), "{}", upload.path);
    assert!(upload.body.contains("name=\"metadata\""), "{}", upload.body);
    assert!(upload.body.contains(r#""main_module":"worker.js""#), "{}", upload.body);
    assert!(upload.body.contains(r#""compatibility_date":"2026-09-01""#));
    assert!(upload.body.contains(r#""workers/message":"released by apex""#));
    assert!(upload.body.contains("filename=\"worker.js\""));
    assert!(upload.body.contains("export default { fetch:"), "the module itself");
}

#[test]
fn an_upload_will_not_follow_a_link_out_of_the_project() {
    // The provider reads a caller-named path as root. A module that is a
    // symlink to somebody else's file is refused, and nothing is sent.
    let f = Fixture::new("uploadlink", Mode::Normal, &["cloudflare.worker.upload-version"]);
    std::fs::create_dir_all(f.project.join("dist")).expect("dist");
    std::os::unix::fs::symlink("/etc/hostname", f.project.join("dist/worker.js")).expect("link");
    let reply = f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "dist/worker.js"),
    );
    let (_, message) = reply.as_error().expect("a symlinked module must be refused");
    assert!(message.contains("symbolic link"), "{message}");
    assert!(f.fake.seen().is_empty(), "it was sent anyway");
}

#[test]
fn a_module_that_is_not_a_module_is_refused_before_it_is_read() {
    let f = Fixture::new("notmodule", Mode::Normal, &["cloudflare.worker.upload-version"]);
    std::fs::write(f.project.join("secrets.env"), "TOKEN=x\n").expect("write");
    let reply = f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "secrets.env"),
    );
    let (_, message) = reply.as_error().expect("refused");
    assert!(message.contains(".js or .mjs"), "{message}");
    assert!(f.fake.seen().is_empty());
}

#[test]
fn a_grant_for_one_worker_verb_does_not_grant_its_siblings() {
    // §13.2's whole argument: semantic operations rather than one broad
    // "Cloudflare write". Deploying is not rolling back and neither is
    // uploading, so a grant for one has to stop at one.
    let f = Fixture::new("siblings", Mode::Normal, &["cloudflare.worker.deploy"]);
    with_files(&f);
    let allowed = f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", "1c4dd6be-0000-4000-8000-abcdefabcdef"),
    );
    assert!(matches!(allowed, Response::Performed { exit_code: 0, .. }), "{allowed:?}");

    for refused in [
        "cloudflare.worker.rollback",
        "cloudflare.worker.upload-version",
        "cloudflare.worker.read",
        "cloudflare.worker.tail",
        "cloudflare.account.read",
    ] {
        let reply = f.use_it(
            f.record(refused, "project")
                .param("version", "1c4dd6be-0000-4000-8000-abcdefabcdef")
                .param("script", "dist/worker.js"),
        );
        assert!(
            reply.as_error().is_some(),
            "'{refused}' went through on a grant for deploy"
        );
    }
    // One call: the allowed one.
    assert_eq!(f.fake.seen().len(), 1);
}

#[test]
fn a_rollback_is_told_apart_from_a_deploy_on_the_wire() {
    // Same endpoint, and the grant is what separates them — so the request
    // still has to say which it is, or Cloudflare refuses a return to an older
    // version as a stale deployment.
    let f = Fixture::new("rollback", Mode::Normal, &granted_everything());
    f.use_it(f.record("cloudflare.worker.deploy", "project").param("version", "v-new"));
    f.use_it(f.record("cloudflare.worker.rollback", "project").param("version", "v-old"));
    let sent = f.fake.seen();
    assert!(sent[0].path.ends_with("/deployments"), "{}", sent[0].path);
    assert!(sent[1].path.ends_with("/deployments?force=true"), "{}", sent[1].path);
    assert!(sent[0].body.contains("v-new"));
    assert!(sent[1].body.contains("v-old"));
}

#[test]
fn a_message_with_a_quote_in_it_survives_two_layers_of_escaping() {
    // `message` is Syntax::Text, which admits a quote and a backslash. It is
    // then escaped by serde_json, and the deployment body is escaped AGAIN by
    // the curl config writer, and curl unescapes once. Three transformations
    // and no test on the round trip is how every quoted deploy message ends up
    // as malformed JSON at Cloudflare — which is a functional bug a scrub test
    // and a binding test would both pass straight over.
    let f = Fixture::new("quoting", Mode::Normal, &granted_everything());
    with_files(&f);
    let awkward = r#"say "hi" \ and "then" stop"#;

    // The JSON body path: through `quoted()` into a curl config line.
    f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", "1c4dd6be-0000-4000-8000-abcdefabcdef")
            .param("message", awkward),
    );
    // The multipart path: written to a file as raw bytes, no curl escaping.
    f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "dist/worker.js")
            .param("message", awkward),
    );

    let sent = f.fake.seen();
    let deploy: serde_json::Value = serde_json::from_str(&sent[0].body)
        .unwrap_or_else(|e| panic!("the deployment body is not JSON: {e}\n{}", sent[0].body));
    assert_eq!(
        deploy["annotations"]["workers/message"].as_str(),
        Some(awkward),
        "the message did not survive the round trip"
    );

    // The metadata part is JSON inside a multipart body; pull it back out.
    let body = &sent[1].body;
    let start = body.find('{').expect("metadata json");
    let end = body[start..].find("\r\n--").map(|i| start + i).unwrap_or(body.len());
    let metadata: serde_json::Value = serde_json::from_str(body[start..end].trim())
        .unwrap_or_else(|e| panic!("the metadata part is not JSON: {e}\n{body}"));
    assert_eq!(
        metadata["annotations"]["workers/message"].as_str(),
        Some(awkward)
    );
}

#[test]
fn a_route_read_answers_about_the_worker_that_was_named_and_no_other() {
    // The zone endpoint returns every route in the zone. The caller named one
    // worker, the grant is for one worker, and the audit line claims the
    // answer is about that worker — so the answer has to be.
    let f = Fixture::new("routes", Mode::Normal, &["cloudflare.worker.route.read"]);
    let reply = f.use_it(f.record("cloudflare.worker.route.read", "project"));
    let Response::Performed { output, .. } = &reply else {
        panic!("refused: {reply:?}");
    };
    assert!(output.contains("route1"), "{output}");
    assert!(
        !output.contains("somebody-else") && !output.contains("route2"),
        "the rest of the zone's routing came back: {output}"
    );
}

#[test]
fn the_host_pin_refuses_a_credential_stored_for_anywhere_else() {
    // Cloudflare's endpoint is a constant, so the pin is a flat statement: a
    // token stored for another host cannot be spent on a Cloudflare operation.
    // The provider contains no such check and cannot skip one.
    let f = Fixture::new("pin", Mode::Normal, &["cloudflare.account.read"]);
    let peer = me();
    assert_eq!(
        f.service.add(
            peer,
            NewService {
                service: "elsewhere",
                host: "github.com",
                scheme: "https",
                username: None,
                path: "",
                auth: None,
                port: None,
            },
            SecretValue::new(TOKEN.as_bytes().to_vec()),
        ),
        Response::Ok
    );
    assert!(f
        .service
        .grant(
            peer,
            f.project.to_str().expect("utf8"),
            "elsewhere",
            "cloudflare.account.read",
            false
        )
        .as_error()
        .is_none());

    let mut rec = f.record("cloudflare.account.read", "");
    rec.provider = "elsewhere".into();
    let reply = f.use_it(rec);
    let (kind, message) = reply.as_error().expect("the pin must refuse this");
    assert_eq!(kind, ErrorKind::PermissionDenied);
    assert!(message.contains("github.com"), "{message}");
    assert!(f.fake.seen().is_empty());
}

#[test]
fn a_binding_that_moves_between_the_check_and_the_call_stops_the_call() {
    // `Bound` carries an endpoint and a sentence and nothing a provider
    // defines, so what `bind` resolved cannot be handed to `perform` and the
    // project file is read twice. The owner is the caller and can change it in
    // between; the sentence is rebuilt and compared, so what runs is what was
    // authorised or nothing runs.
    struct Moving {
        inner: CloudflareProvider,
        project: Mutex<Option<PathBuf>>,
    }
    impl Provider for Moving {
        fn spec(&self) -> &'static ProviderSpec {
            self.inner.spec()
        }
        fn bind(&self, req: &Bind<'_>) -> Result<Bound, ProviderError> {
            let bound = self.inner.bind(req);
            // Between the two, exactly as a caller's own process could.
            if let Some(project) = self.project.lock().expect("lock").take() {
                // The account id, not the worker name: a changed name would
                // stop resolving and be refused for that reason instead, and
                // the interesting case is the one where everything still
                // resolves and the request would go somewhere else.
                std::fs::write(
                    project.join("apex.toml"),
                    PROJECT_FILE.replace(ACCOUNT, "99999999999999999999999999999999"),
                )
                .expect("rewrite");
            }
            bound
        }
        fn perform(
            &self,
            req: &Bind<'_>,
            bound: &Bound,
            value: &SecretValue,
        ) -> Result<Performed, ProviderError> {
            self.inner.perform(req, bound, value)
        }
    }

    let fake = Fake::start(Mode::Normal);
    let tag = format!("moving-{}-{}", std::process::id(), fake.port);
    let store = std::env::temp_dir().join(format!("apex-cf-store-{tag}"));
    let project = std::env::temp_dir().join(format!("apex-cf-project-{tag}"));
    std::fs::remove_dir_all(&store).ok();
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(project.join("apex.toml"), PROJECT_FILE).expect("apex.toml");

    let mut registry = Registry::new();
    registry
        .register(Box::new(Moving {
            inner: CloudflareProvider::at(fake.port),
            project: Mutex::new(Some(project.clone())),
        }))
        .expect("register");
    let service = Service::new(Store::new(store.clone()), false, registry);
    let peer = me();
    service.add(
        peer,
        NewService {
            service: "cloudflare",
            host: "127.0.0.1",
            scheme: "http",
            username: None,
            path: "",
            auth: None,
            port: None,
        },
        SecretValue::new(TOKEN.as_bytes().to_vec()),
    );
    service.grant(
        peer,
        project.to_str().expect("utf8"),
        "cloudflare",
        "cloudflare.worker.read",
        false,
    );

    let mut rec = CapabilityRecord::new("cloudflare", "cloudflare.worker.read", "project");
    rec.project = Some(project.to_string_lossy().into_owned());
    let reply = service.use_capability(peer, rec, Vec::new());
    let (_, message) = reply
        .as_error()
        .expect("a binding that moved must stop the call");
    assert!(message.contains("changed while the request"), "{message}");
    assert!(fake.seen().is_empty(), "it was sent anyway");

    std::fs::remove_dir_all(&store).ok();
    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn the_declared_shape_is_enforced_before_the_provider_is_asked_anything() {
    // All of it from the provider's own declaration, all of it checked by the
    // framework: an operation that takes no resource, a resource that is a
    // URL, an option nobody declared, a required option left out.
    let f = Fixture::new("shape", Mode::Normal, &granted_everything());
    with_files(&f);
    for (operation, resource, options) in [
        ("cloudflare.account.read", "something", vec![]),
        ("cloudflare.worker.read", "https://attacker.example/x", vec![]),
        ("cloudflare.worker.read", "../../etc", vec![]),
        ("cloudflare.worker.read", "-f", vec![]),
        ("cloudflare.worker.deploy", "project", vec![]),
        (
            "cloudflare.worker.read",
            "project",
            vec![("branch", "main")],
        ),
        (
            "cloudflare.worker.upload-version",
            "project",
            vec![("script", "/etc/passwd")],
        ),
        (
            "cloudflare.worker.upload-version",
            "project",
            vec![("script", "dist/worker.js"), ("message", "two\nlines")],
        ),
    ] {
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        let (kind, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("{operation} {resource} {options:?} was accepted"));
        assert_eq!(kind, ErrorKind::BadRequest, "{operation} {resource}: {message}");
    }
    assert!(f.fake.seen().is_empty());
}

#[test]
fn an_operation_no_provider_declares_is_refused_even_under_the_cloudflare_prefix() {
    // The vocabulary is closed by the registry, and a name that looks like one
    // of §13.2's twenty-five not-yet-implemented operations is not one this
    // build offers.
    let f = Fixture::new("closed", Mode::Normal, &granted_everything());
    for evil in [
        "cloudflare.dns.delete",
        "cloudflare.r2.object.write",
        "cloudflare.worker.delete",
        "cloudflare.secret.rotate",
        "git.push",
        "exec",
    ] {
        let reply = f.use_it(f.record(evil, "project"));
        assert_eq!(
            reply.as_error().map(|(k, _)| k),
            Some(ErrorKind::BadRequest),
            "'{evil}' was not refused"
        );
    }
    assert!(f.fake.seen().is_empty());
}

#[test]
fn the_trail_records_the_operation_in_the_provider_s_own_words() {
    let f = Fixture::new("trail", Mode::Normal, &["cloudflare.worker.deploy"]);
    f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", "1c4dd6be-0000-4000-8000-abcdefabcdef"),
    );
    let lines = audit::tail(&Store::new(f.store.clone()).audit_path(), 10);
    let used = lines
        .iter()
        .find(|l| l.event == AuditEvent::Used)
        .expect("a use was recorded");
    assert_eq!(used.operation, "cloudflare.worker.deploy");
    assert_eq!(used.resource, "project");
    assert_eq!(
        used.detail,
        format!(
            "deploy version 1c4dd6be-0000-4000-8000-abcdefabcdef of project \
             (production) in account example-account [{ACCOUNT}]"
        )
    );
    assert_eq!(used.endpoint.as_deref(), Some("http://127.0.0.1"));
    assert_eq!(used.exit_code, Some(0));
}

#[test]
fn the_declaration_is_well_formed_and_every_name_is_one_section_thirteen_two_lists() {
    // §13.2 is the specification, and a name invented here would be a name no
    // other task in the Cloudflare block could depend on.
    SPEC.validate().expect("the shipped cloudflare vocabulary must validate");
    let listed: Vec<&str> = SECTION_13_2.to_vec();
    for op in SPEC.operations {
        // `worker.route.read` is §13.3's "Worker routes" rather than an
        // invention, `access.service-token.create` is §13.3's "service
        // tokens", and P1-012's four are §13.4's *"a broker-owned
        // `wrangler`/API child process"* — the tool half of a sentence whose
        // API half is the other thirty-two. §13.2 is a vocabulary for the API
        // surface and says nothing about running a tool; a build that refused
        // to name these because §13.2 does not would have nowhere to put them.
        if op.id == "cloudflare.worker.route.read"
            || op.id == "cloudflare.access.service-token.create"
            || super::tools::brokered(op.id).is_some()
        {
            continue;
        }
        assert!(listed.contains(&op.id), "'{}' is not in §13.2", op.id);
    }
    // All thirty-two of §13.2's, plus six that are not §13.2's: two from
    // §13.3 and P1-012's four tool subcommands. The arithmetic is asserted
    // because the module note states it and a later task will read that note
    // to work out what is left.
    let from_13_2 = SPEC
        .operations
        .iter()
        .filter(|op| listed.contains(&op.id))
        .count();
    assert_eq!(from_13_2, 32, "every name §13.2 lists is implemented");
    assert_eq!(SPEC.operations.len(), 38);
    // Every operation that is not §13.2's is one this build can account for.
    // A name that is in neither list is a name somebody added without saying
    // where it came from.
    let unaccounted: Vec<&str> = SPEC
        .operations
        .iter()
        .map(|op| op.id)
        .filter(|id| {
            !listed.contains(id)
                && *id != "cloudflare.worker.route.read"
                && *id != "cloudflare.access.service-token.create"
                && super::tools::brokered(id).is_none()
        })
        .collect();
    assert!(unaccounted.is_empty(), "names from nowhere: {unaccounted:?}");
    assert_eq!(SECTION_13_2.len() - from_13_2, 0, "still unimplemented");

    // **Running a model is a write, and this is the only thing that says so.**
    //
    // Nothing stored, nothing changed — so `Effect::Read` looks defensible, and
    // it was reachable: flipping `cloudflare.workers-ai.run` to `Read` turned
    // no test red before this assertion existed. It has to be `Write` because
    // `Effect` is what a grant is scoped by, and a read-only grant that can
    // spend the account's balance is a read-only grant in name only. Inference
    // costs money and does not give the same answer twice; both are what
    // `Write` means here.
    for op in SPEC.operations {
        if op.id.ends_with(".run") {
            assert!(
                op.effect.is_write(),
                "'{}' costs money, so a Read grant must not reach it",
                op.id
            );
        }
    }

    // Nothing is declared twice, and every summary reads as a sentence about
    // what the owner is being asked to allow.
    let mut ids: Vec<&str> = SPEC.operations.iter().map(|op| op.id).collect();
    ids.sort_unstable();
    let mut unique = ids.clone();
    unique.dedup();
    assert_eq!(ids, unique);
    for op in SPEC.operations {
        assert!(op.summary.len() > 20, "'{}' has a summary nobody can act on", op.id);
    }
}

// ── §13.5, R2 ───────────────────────────────────────────────────────────────
//
// Six mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back so that cargo rebuilt rather than reusing
// the mutant's binary. Every one turns a named test red:
//
// * the bucket dropped out of the object path — `an_r2_object_is_addressed_…`;
// * `binding.bucket()` replaced by the caller's own string, which is the
//   defect §13.5 exists to prevent — `a_bucket_this_project_does_not_bind_…`;
// * the upload sending the path instead of the file's bytes, the media type
//   table answering `text/plain` for `.sql`, and the `Content-Type` header
//   dropped from a raw body — all three, `an_upload_to_r2_carries_…`;
// * the location hint forwarded instead of checked against Cloudflare's own
//   set — `a_bucket_is_created_with_the_name_…`.

#[test]
fn an_r2_object_is_addressed_by_the_bucket_this_project_bound_and_the_key_under_it() {
    // The documented paths, exactly. `example-assets` on its own lists the
    // bucket; `example-assets/db/today.sql` is one object in it; and the
    // account id in front of both came out of apex.toml rather than out of the
    // request.
    let f = Fixture::new("r2paths", Mode::Normal, &granted_everything());
    with_files(&f);
    f.use_it(f.record("cloudflare.r2.object.read", "example-assets"));
    f.use_it(f.record("cloudflare.r2.object.read", "example-assets/db/today.sql"));
    f.use_it(
        f.record("cloudflare.r2.object.write", "example-assets/db/today.sql")
            .param("file", "dist/today.sql"),
    );
    f.use_it(f.record("cloudflare.r2.bucket.create", "example-assets"));

    let seen: Vec<(String, String)> = f
        .fake
        .seen()
        .into_iter()
        .map(|s| (s.method, s.path))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("GET".into(), format!("/client/v4/accounts/{ACCOUNT}/r2/buckets/{BUCKET}/objects")),
            (
                "GET".into(),
                format!("/client/v4/accounts/{ACCOUNT}/r2/buckets/{BUCKET}/objects/db/today.sql")
            ),
            (
                "PUT".into(),
                format!("/client/v4/accounts/{ACCOUNT}/r2/buckets/{BUCKET}/objects/db/today.sql")
            ),
            ("POST".into(), format!("/client/v4/accounts/{ACCOUNT}/r2/buckets")),
        ]
    );
}

#[test]
fn a_bucket_this_project_does_not_bind_never_reaches_cloudflare() {
    // §13.5 asks for bucket-scoped capabilities, and this is what scoped means
    // here: the grant is per operation, and *which* bucket it may touch is the
    // project file's answer, not the caller's. All three R2 operations are
    // checked, because a guard on the two that read a key and not on the one
    // that creates a bucket would let an agent make buckets in the account at
    // will.
    let f = Fixture::new("r2unbound", Mode::Normal, &granted_everything());
    with_files(&f);
    for (operation, resource, options) in [
        ("cloudflare.r2.object.read", "somebody-elses-bucket/db/today.sql", vec![]),
        (
            "cloudflare.r2.object.write",
            "somebody-elses-bucket/db/today.sql",
            vec![("file", "dist/today.sql")],
        ),
        ("cloudflare.r2.bucket.create", "somebody-elses-bucket", vec![]),
    ] {
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        let (kind, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("{operation} accepted an unbound bucket"));
        assert_eq!(kind, ErrorKind::BadRequest, "{operation}");
        assert!(message.contains("somebody-elses-bucket"), "{message}");
        assert!(message.contains("example-assets"), "{message}");
        assert!(message.contains("apex.toml"), "{message}");
    }
    assert!(
        f.fake.seen().is_empty(),
        "an unbound bucket still reached the api"
    );
}

#[test]
fn an_upload_to_r2_carries_the_project_s_own_file_and_a_media_type_from_a_table() {
    // The body IS the object, so there is nothing here to escape and nothing
    // to wrap — which makes the two things worth checking the two that could
    // go wrong: that the bytes are the project's file rather than a path, and
    // that the media type came from this build rather than from the caller.
    let f = Fixture::new("r2upload", Mode::Normal, &["cloudflare.r2.object.write"]);
    with_files(&f);
    let reply = f.use_it(
        f.record("cloudflare.r2.object.write", "example-assets/db/today.sql")
            .param("file", "dist/today.sql"),
    );
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");

    let sent = f.fake.seen();
    let put = sent.first().expect("one request");
    assert_eq!(put.method, "PUT");
    assert_eq!(put.body, UPLOADED, "the object's bytes are the project's file");
    assert_eq!(put.content_type.as_deref(), Some("application/sql"));
    assert_eq!(sent.len(), 1);
}

#[test]
fn an_object_named_for_something_this_build_does_not_know_is_bytes_rather_than_a_guess() {
    let f = Fixture::new("r2type", Mode::Normal, &["cloudflare.r2.object.write"]);
    with_files(&f);
    std::fs::write(f.project.join("dist/blob.whatever"), "x").expect("blob");
    f.use_it(
        f.record("cloudflare.r2.object.write", "example-assets/db/blob.whatever")
            .param("file", "dist/blob.whatever"),
    );
    assert_eq!(
        f.fake.seen()[0].content_type.as_deref(),
        Some("application/octet-stream")
    );
}

#[test]
fn an_upload_to_r2_will_not_follow_a_link_out_of_the_project() {
    // The same root-reading-a-caller's-path problem as a Worker module, on the
    // operation that was written second. A guard that held for one upload and
    // not the other would be no guard at all.
    let f = Fixture::new("r2link", Mode::Normal, &["cloudflare.r2.object.write"]);
    std::fs::create_dir_all(f.project.join("dist")).expect("dist");
    std::os::unix::fs::symlink("/etc/hostname", f.project.join("dist/today.sql")).expect("link");
    let reply = f.use_it(
        f.record("cloudflare.r2.object.write", "example-assets/db/today.sql")
            .param("file", "dist/today.sql"),
    );
    let (_, message) = reply.as_error().expect("a symlinked payload must be refused");
    assert!(message.contains("symbolic link"), "{message}");
    assert!(f.fake.seen().is_empty(), "it was sent anyway");
}

#[test]
fn a_write_that_names_only_a_bucket_is_refused_rather_than_writing_the_bucket() {
    // `example-assets` is a legal resource for this operation's declaration —
    // a `Path` of one segment — and it names no object. Sending it would `PUT`
    // the bucket's own listing URL.
    let f = Fixture::new("r2nokey", Mode::Normal, &["cloudflare.r2.object.write"]);
    with_files(&f);
    let reply = f.use_it(
        f.record("cloudflare.r2.object.write", "example-assets")
            .param("file", "dist/today.sql"),
    );
    let (error, message) = reply.as_error().expect("refused");
    assert!(message.contains("names a bucket and not an object"), "{message}");
    // The KIND, not just that it was refused: a resource that does not resolve
    // is a `BadRequest`, and reporting it as `PermissionDenied` would send
    // somebody who made a typo to look at the grant table.
    assert_eq!(error, ErrorKind::BadRequest, "{message}");
    assert!(f.fake.seen().is_empty());
}

#[test]
fn a_bucket_is_created_with_the_name_the_project_declares_and_options_from_a_closed_set() {
    let f = Fixture::new("r2create", Mode::Normal, &["cloudflare.r2.bucket.create"]);
    let reply = f.use_it(
        f.record("cloudflare.r2.bucket.create", "example-assets")
            .param("location", "apac")
            .param("storage-class", "InfrequentAccess"),
    );
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");
    let sent = f.fake.seen();
    let create = sent.first().expect("one request");
    assert_eq!(create.method, "POST");
    assert!(create.body.contains(r#""name":"example-assets""#), "{}", create.body);
    assert!(create.body.contains(r#""locationHint":"apac""#), "{}", create.body);
    assert!(create.body.contains(r#""storageClass":"InfrequentAccess""#), "{}", create.body);

    // ...and a value outside the documented set is refused here rather than
    // spent on a request that will fail at the other end.
    for (param, value) in [("location", "mars"), ("storage-class", "Cheap")] {
        let reply = f.use_it(
            f.record("cloudflare.r2.bucket.create", "example-assets").param(param, value),
        );
        let (_, message) = reply.as_error().unwrap_or_else(|| panic!("{param}={value} was sent"));
        assert!(message.contains(value), "{message}");
    }
    assert_eq!(f.fake.seen().len(), 1, "a refused option still reached the api");
}

#[test]
fn an_object_read_answers_with_bytes_and_still_has_the_credential_taken_out() {
    // R2 answers a read with the object itself and no envelope. Every other
    // operation's reply is JSON, so a scrub that happened to key on the
    // envelope would pass everywhere except here — and here is where a
    // project's own data comes back through the broker.
    let f = Fixture::new("r2read", Mode::Normal, &["cloudflare.r2.object.read"]);
    let reply = f.use_it(f.record("cloudflare.r2.object.read", "example-assets/db/today.sql"));
    let Response::Performed { output, .. } = &reply else {
        panic!("refused: {reply:?}");
    };
    // The object arrived...
    assert!(output.contains("CREATE TABLE t (id INTEGER);"), "{output}");
    // ...the double really did echo the credential into it...
    assert!(output.contains("fetched with Bearer"), "{output}");
    // ...and it is not in what came back.
    assert!(!output.contains(TOKEN), "the object read handed back the token");
    assert!(output.contains("«redacted»"), "{output}");
    assert!(!f.trail().contains(TOKEN));
}

// ── P2-002: what a backup actually asks R2 for ──────────────────────────────
//
// The three tests below are the backup framework's own call sequence, run
// through the real broker against the double that 401s an unauthenticated
// request. They live here rather than in `apex-backup-core` because this is
// where a real credential, a real store, a real grant check and a real project
// binding exist — `apex-backup-core` has the target's logic and a broker
// double, and neither of those can prove that the token is spent and does not
// come back.
//
// They are Rust and not a shell suite for a measured reason: `Api::loopback` is
// `#[cfg(test)]`, so a SHIPPED apex-secretd always addresses
// api.cloudflare.com — and even if it did not, `service.rs`'s host pin refuses
// a credential stored for one host being sent to another. There is no way to
// aim the shipped daemon at a double, which is why every Cloudflare unit in
// this repository is proven here and none of them has a shell test.

/// Where a backup stages a chunk inside the project. Underscore and not a dot,
/// because `valid_name` requires the first byte of a path segment to be
/// alphanumeric or `_` — asserted below, end to end, rather than believed.
const BACKUP_STAGING: &str = "_apex-backup";
const BACKUP_SNAPSHOT: &str = "20260912T014233Z-0badc0de";

/// A sealed chunk is not text. This is what one looks like after base64, which
/// is the framing the backup format uses because a brokered reply is
/// `String::from_utf8_lossy` of curl's stdout.
const BACKUP_CHUNK_BASE64: &str = "q83vASNFZ4mrze//AAECA/79/A==";

fn stage_a_chunk(fixture: &Fixture) -> String {
    let relative = format!("{BACKUP_STAGING}/{BACKUP_SNAPSHOT}/data.000000");
    let path = fixture.project.join(&relative);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("staging dir");
    std::fs::write(&path, BACKUP_CHUNK_BASE64).expect("the staged chunk");
    relative
}

#[test]
fn a_backups_chunk_reaches_r2_under_the_key_it_chose_and_the_token_does_not_come_back() {
    let f = Fixture::new(
        "r2backup",
        Mode::Normal,
        &["cloudflare.r2.object.read", "cloudflare.r2.object.write"],
    );
    let staged = stage_a_chunk(&f);

    // 1. The bucket listing, which is a read with no key — one operation and
    //    not two, which is why versioned restore needs no new capability.
    let listing = f.use_it(f.record("cloudflare.r2.object.read", BUCKET));
    let Response::Performed { output, .. } = &listing else {
        panic!("the listing was refused: {listing:?}");
    };
    assert!(output.contains("\"key\""), "{output}");

    // 2. The upload, naming the staged file by a path relative to the project.
    let mut write = f.record(
        "cloudflare.r2.object.write",
        &format!("{BUCKET}/apex-backup/{BACKUP_SNAPSHOT}/data.000000"),
    );
    write.params.insert("file".into(), staged.clone());
    let reply = f.use_it(write);
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("the upload was refused: {reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");

    let seen = f.fake.seen();
    let put = seen
        .iter()
        .find(|s| s.method == "PUT")
        .expect("the chunk was uploaded");
    assert_eq!(
        put.path,
        format!(
            "/client/v4/accounts/{ACCOUNT}/r2/buckets/{BUCKET}/objects/apex-backup/{BACKUP_SNAPSHOT}/data.000000"
        ),
        "the object key is not the one the backup chose"
    );
    // The project's own staged bytes arrived, unchanged and still ASCII.
    assert_eq!(put.body, BACKUP_CHUNK_BASE64);
    assert!(put.body.is_ascii(), "what went up is not ASCII");

    // The credential was spent...
    assert!(
        f.fake.authorizations().iter().any(|a| a.contains(TOKEN)),
        "the token never reached the double, so nothing was authenticated"
    );
    // ...and is in neither the reply nor the trail.
    assert!(!output.contains(TOKEN), "the upload handed back the token");
    assert!(!f.trail().contains(TOKEN));

    // 3. And reading one back is addressed by the same key.
    let read = f.use_it(f.record(
        "cloudflare.r2.object.read",
        &format!("{BUCKET}/apex-backup/{BACKUP_SNAPSHOT}/data.000000"),
    ));
    let Response::Performed { output, .. } = &read else {
        panic!("the read was refused: {read:?}");
    };
    assert!(!output.contains(TOKEN));
    let after = f.fake.seen();
    assert!(
        after.iter().any(|s| s.method == "GET"
            && s.path.ends_with(&format!(
                "/objects/apex-backup/{BACKUP_SNAPSHOT}/data.000000"
            ))),
        "the read did not address the object the upload wrote"
    );
}

/// A backup cannot reach a bucket the project never wrote down, and the check
/// happens before anything leaves the machine.
#[test]
fn a_backup_to_a_bucket_this_project_did_not_bind_never_reaches_cloudflare() {
    let f = Fixture::new(
        "r2backupunbound",
        Mode::Normal,
        &["cloudflare.r2.object.read", "cloudflare.r2.object.write"],
    );
    let staged = stage_a_chunk(&f);

    let mut write = f.record(
        "cloudflare.r2.object.write",
        &format!("someone-elses-bucket/apex-backup/{BACKUP_SNAPSHOT}/data.000000"),
    );
    write.params.insert("file".into(), staged);
    let reply = f.use_it(write);
    let (kind, message) = reply.as_error().expect("refused");
    assert_eq!(kind, ErrorKind::BadRequest, "{message}");
    assert!(message.contains("example-assets"), "{message}");
    assert!(
        f.fake.seen().is_empty(),
        "a bucket the project never bound produced a request anyway"
    );
}

/// The staging directory's name, proven against the framework rather than
/// argued from its source. `.apex-backup/` is refused; `_apex-backup/` is not.
#[test]
fn a_dot_prefixed_staging_directory_is_refused_and_the_underscore_one_is_not() {
    let f = Fixture::new(
        "r2backupdot",
        Mode::Normal,
        &["cloudflare.r2.object.write"],
    );
    let dotted = format!(".apex-backup/{BACKUP_SNAPSHOT}/data.000000");
    let path = f.project.join(&dotted);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("staging dir");
    std::fs::write(&path, BACKUP_CHUNK_BASE64).expect("the staged chunk");

    let mut write = f.record(
        "cloudflare.r2.object.write",
        &format!("{BUCKET}/apex-backup/{BACKUP_SNAPSHOT}/data.000000"),
    );
    write.params.insert("file".into(), dotted);
    let reply = f.use_it(write);
    assert!(
        reply.as_error().is_some(),
        "a dot-prefixed staging path was accepted: {reply:?}"
    );
    assert!(
        f.fake.seen().is_empty(),
        "a refused path still produced a request"
    );

    // And the one the backup format actually uses goes through.
    let staged = stage_a_chunk(&f);
    let mut write = f.record(
        "cloudflare.r2.object.write",
        &format!("{BUCKET}/apex-backup/{BACKUP_SNAPSHOT}/data.000000"),
    );
    write.params.insert("file".into(), staged);
    let reply = f.use_it(write);
    assert!(
        matches!(reply, Response::Performed { exit_code: 0, .. }),
        "the underscore staging path was refused: {reply:?}"
    );
}

// ── §13.3's storage surfaces: D1, KV, Queues, Hyperdrive ────────────────────
//
// Nine mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back so that cargo rebuilt rather than reusing
// the mutant's binary. Every one turns a named test red:
//
// * one id shape for all four tables, so a KV id passes as a D1 one, and the
//   reserved-table guard removed so an environment may shadow `[cloudflare.kv]`
//   — `a_d1_id_and_a_kv_id_are_different_shapes_…`,
//   `an_environment_may_not_be_named_after_a_resource_table`;
// * `is_empty` forgetting the new tables — `a_project_that_binds_only_a_…`;
// * `NoResource` falling out of the `NoSuchResource` arm, which is the same
//   defect `NoRecord` had one unit earlier and which this test found again —
//   `a_database_namespace_queue_or_config_…`;
// * a migration accepting any file, and a queue setting skipping its
//   documented range — `a_migration_is_a_file_of_statements_…`,
//   `a_queue_change_is_checked_against_the_ranges_…`;
// * `caching = off` no longer inverted into Cloudflare's `disabled` —
//   `a_hyperdrive_edit_cannot_carry_a_database_credential`;
// * the origin scrub not descending into nested objects, which is where the
//   password actually is — `an_origin_credential_the_api_volunteers_…`;
// * a KV write given both a value and a file quietly picking one —
//   `a_kv_write_takes_a_value_or_a_file_and_never_both`.

#[test]
fn every_storage_resource_is_addressed_by_the_id_its_project_wrote_down() {
    // Nine operations, nine documented paths, and every id in them came out of
    // apex.toml. None of these four products has a name-based path, so this is
    // the whole of what "project resource binding enforced" can mean for them.
    let f = Fixture::new("p6paths", Mode::Normal, &granted_everything());
    with_files(&f);
    for (operation, resource, options, _) in every_operation() {
        if !["d1", "kv", "queue", "hyperdrive"].contains(&operation.split('.').nth(1).unwrap_or("")) {
            continue;
        }
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        assert!(
            matches!(reply, Response::Performed { exit_code: 0, .. }),
            "{operation}: {reply:?}"
        );
    }
    let seen: Vec<(String, String)> = f.fake.seen().into_iter().map(|s| (s.method, s.path)).collect();
    assert_eq!(
        seen,
        vec![
            ("GET".into(), format!("/client/v4/accounts/{ACCOUNT}/d1/database/{DB_ID}")),
            ("POST".into(), format!("/client/v4/accounts/{ACCOUNT}/d1/database/{DB_ID}/query")),
            ("POST".into(), format!("/client/v4/accounts/{ACCOUNT}/d1/database/{DB_ID}/query")),
            (
                "GET".into(),
                format!("/client/v4/accounts/{ACCOUNT}/storage/kv/namespaces/{KV_ID}/values/greeting")
            ),
            (
                "PUT".into(),
                format!("/client/v4/accounts/{ACCOUNT}/storage/kv/namespaces/{KV_ID}/values/greeting")
            ),
            ("POST".into(), format!("/client/v4/accounts/{ACCOUNT}/queues/{QUEUE_ID}/messages")),
            ("PATCH".into(), format!("/client/v4/accounts/{ACCOUNT}/queues/{QUEUE_ID}")),
            ("GET".into(), format!("/client/v4/accounts/{ACCOUNT}/hyperdrive/configs/{HD_ID}")),
            ("PATCH".into(), format!("/client/v4/accounts/{ACCOUNT}/hyperdrive/configs/{HD_ID}")),
        ]
    );
}

#[test]
fn a_database_namespace_queue_or_config_this_project_did_not_bind_never_reaches_cloudflare() {
    let f = Fixture::new("p6unbound", Mode::Normal, &granted_everything());
    with_files(&f);
    for (operation, resource, kind, options) in [
        ("cloudflare.d1.read", "somebody-elses-db", "D1 database", vec![]),
        ("cloudflare.kv.read", "somebody-elses-ns/key", "KV namespace", vec![]),
        (
            "cloudflare.queue.publish",
            "somebody-elses-queue",
            "queue",
            vec![("message", "x")],
        ),
        (
            "cloudflare.hyperdrive.read",
            "somebody-elses-config",
            "Hyperdrive config",
            vec![],
        ),
    ] {
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        let (error, message) = match reply.as_error() {
            Some(pair) => pair,
            None => panic!("{operation} accepted an unbound resource"),
        };
        assert_eq!(error, ErrorKind::BadRequest, "{operation}");
        assert!(message.contains(kind), "{message}");
        assert!(message.contains("apex.toml"), "{message}");
    }
    assert!(f.fake.seen().is_empty(), "an unbound resource reached the api");
}

#[test]
fn a_d1_id_and_a_kv_id_are_different_shapes_and_neither_is_accepted_for_the_other() {
    // Cloudflare's own split: a D1 database id is a UUID, everything else here
    // is 32 hex. One validator would either refuse every real D1 id or let 36
    // characters of anything into a URL path.
    use apex_secret_core::project::ProjectConfig;
    use std::path::Path;
    let binding = |text: &str| {
        let config = ProjectConfig::parse(Path::new("/p/apex.toml"), text).expect("parses");
        super::binding::Binding::of(&config)
    };
    let head = format!("[identity.cloudflare]\naccount_id = \"{ACCOUNT}\"\n");

    // Each in its own table: fine.
    assert!(binding(&format!("{head}[cloudflare.d1]\ndb = \"{DB_ID}\"\n")).is_ok());
    assert!(binding(&format!("{head}[cloudflare.kv]\nns = \"{KV_ID}\"\n")).is_ok());
    // Crossed over: refused, and the message names the shape that key wants.
    let err = binding(&format!("{head}[cloudflare.d1]\ndb = \"{KV_ID}\"\n")).unwrap_err();
    assert!(err.to_string().contains("UUID"), "{err}");
    let err = binding(&format!("{head}[cloudflare.kv]\nns = \"{DB_ID}\"\n")).unwrap_err();
    assert!(err.to_string().contains("32 hexadecimal"), "{err}");
    // ...and a name no request could ever carry is refused where it is written
    // rather than being silently unaddressable.
    let err = binding(&format!("{head}[cloudflare.kv]\n\"my cache\" = \"{KV_ID}\"\n")).unwrap_err();
    assert!(err.to_string().contains("my cache"), "{err}");
}

#[test]
fn an_environment_may_not_be_named_after_a_resource_table() {
    // `[cloudflare.kv]` cannot be both a list of namespaces and a worker
    // binding. Without this the key `worker` would be read as a namespace
    // called "worker" with an id of whatever the worker is called, and the
    // whole file would be refused for a bad id — a true refusal about the
    // wrong thing.
    use apex_secret_core::project::ProjectConfig;
    use std::path::Path;
    let text = format!(
        "[identity.cloudflare]\naccount_id = \"{ACCOUNT}\"\n\
         [cloudflare.kv]\nworker = \"project\"\n"
    );
    let config = ProjectConfig::parse(Path::new("/p/apex.toml"), &text).expect("parses");
    let err = super::binding::Binding::of(&config).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("[cloudflare.kv]"), "{message}");
    assert!(message.contains("Rename the environment"), "{message}");
}

#[test]
fn a_project_that_binds_only_a_namespace_is_told_to_add_a_line_not_to_write_a_file() {
    // `is_empty` decides which of two refusals a person gets, and a new binding
    // that is not counted there turns "add an account id" into "this project
    // binds no Cloudflare account at all".
    use apex_secret_core::project::ProjectConfig;
    use std::path::Path;
    let text = format!("[cloudflare.kv]\ncache = \"{KV_ID}\"\n");
    let config = ProjectConfig::parse(Path::new("/p/apex.toml"), &text).expect("parses");
    let binding = super::binding::Binding::of(&config).expect("binds");
    assert!(!binding.is_empty(), "a project binding a namespace binds something");
    let err = binding.resource("kv", "cache").unwrap_err();
    assert!(err.to_string().contains("account_id"), "{err}");
}

#[test]
fn a_migration_is_a_file_of_statements_and_a_query_is_one_line() {
    // The two D1 write verbs, and why §13.2 has both. A `Syntax::Text` option
    // is one line, so a migration could never have travelled as one.
    let f = Fixture::new("p6d1", Mode::Normal, &granted_everything());
    with_files(&f);
    f.use_it(f.record("cloudflare.d1.query", "project-db").param("sql", "SELECT 1"));
    f.use_it(
        f.record("cloudflare.d1.migrate", "project-db")
            .param("file", "migrations/001.sql"),
    );
    let sent = f.fake.seen();
    assert!(sent[0].body.contains(r#""sql":"SELECT 1""#), "{}", sent[0].body);
    // The whole file, newlines and all, JSON-escaped into the body.
    assert!(sent[1].body.contains("CREATE TABLE t (id INTEGER PRIMARY KEY);"), "{}", sent[1].body);
    assert!(sent[1].body.contains("INSERT INTO t VALUES (1);"), "{}", sent[1].body);
    assert!(sent[1].body.contains(r"\n"), "the newlines were escaped, not dropped");

    // A migration that is not a migration is refused before it is read.
    std::fs::write(f.project.join("migrations/notes.txt"), "DROP TABLE t;\n").expect("notes");
    let reply = f.use_it(
        f.record("cloudflare.d1.migrate", "project-db")
            .param("file", "migrations/notes.txt"),
    );
    let (_, message) = reply.as_error().expect("refused");
    assert!(message.contains(".sql"), "{message}");
    assert_eq!(f.fake.seen().len(), 2, "the .txt was sent anyway");
}

#[test]
fn a_kv_write_takes_a_value_or_a_file_and_never_both() {
    let f = Fixture::new("p6kv", Mode::Normal, &granted_everything());
    with_files(&f);
    f.use_it(f.record("cloudflare.kv.write", "cache/greeting").param("value", "hello"));
    assert_eq!(f.fake.seen()[0].body, "hello");

    f.use_it(
        f.record("cloudflare.kv.write", "cache/greeting").param("file", "dist/today.sql"),
    );
    assert_eq!(f.fake.seen()[1].body, UPLOADED);

    for options in [
        vec![("value", "hello"), ("file", "dist/today.sql")],
        vec![],
    ] {
        let mut rec = f.record("cloudflare.kv.write", "cache/greeting");
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        assert!(f.use_it(rec).as_error().is_some(), "{options:?} was accepted");
    }
    assert_eq!(f.fake.seen().len(), 2);

    // ...and a namespace with no key names no value.
    let reply = f.use_it(f.record("cloudflare.kv.read", "cache"));
    let (error, message) = reply.as_error().expect("refused");
    assert!(message.contains("names a namespace and not a key"), "{message}");
    assert_eq!(error, ErrorKind::BadRequest, "{message}");
}

#[test]
fn a_kv_read_answers_with_bytes_and_still_has_the_credential_taken_out() {
    let f = Fixture::new("p6kvread", Mode::Normal, &["cloudflare.kv.read"]);
    let reply = f.use_it(f.record("cloudflare.kv.read", "cache/greeting"));
    let Response::Performed { output, .. } = &reply else {
        panic!("refused: {reply:?}");
    };
    assert!(output.contains("stored by apex"), "{output}");
    assert!(output.contains("read back with Bearer"), "the double really echoed it");
    assert!(!output.contains(TOKEN), "the value read handed back the token");
    assert!(output.contains("«redacted»"), "{output}");
}

#[test]
fn a_queue_change_is_checked_against_the_ranges_cloudflare_documents() {
    let f = Fixture::new("p6queue", Mode::Normal, &granted_everything());
    f.use_it(
        f.record("cloudflare.queue.manage", "jobs")
            .param("paused", "true")
            .param("delivery-delay", "60")
            .param("retention", "345600"),
    );
    let body = &f.fake.seen()[0].body;
    assert!(body.contains(r#""delivery_paused":true"#), "{body}");
    assert!(body.contains(r#""delivery_delay":60"#), "{body}");
    assert!(body.contains(r#""message_retention_period":345600"#), "{body}");

    for (param, value) in [
        ("delivery-delay", "86401"),
        ("retention", "59"),
        ("retention", "1209601"),
        ("paused", "yes"),
        ("delivery-delay", "soon"),
    ] {
        let reply = f.use_it(f.record("cloudflare.queue.manage", "jobs").param(param, value));
        assert!(reply.as_error().is_some(), "{param}={value} was sent");
    }
    // ...and a change with nothing in it does not spend a request.
    assert!(f.use_it(f.record("cloudflare.queue.manage", "jobs")).as_error().is_some());
    assert_eq!(f.fake.seen().len(), 1);
}

#[test]
fn a_published_message_does_not_get_to_choose_its_own_content_type() {
    let f = Fixture::new("p6publish", Mode::Normal, &["cloudflare.queue.publish"]);
    f.use_it(f.record("cloudflare.queue.publish", "jobs").param("message", "work to do"));
    let body = &f.fake.seen()[0].body;
    assert!(body.contains(r#""body":"work to do""#), "{body}");
    assert!(body.contains(r#""content_type":"text""#), "{body}");
    // A content type is a header-shaped string, and this operation declares no
    // option for one — so the framework refuses it before the provider is
    // asked anything.
    let reply = f.use_it(
        f.record("cloudflare.queue.publish", "jobs")
            .param("message", "x")
            .param("content_type", "application/json"),
    );
    assert!(reply.as_error().is_some(), "a caller chose a content type");
    assert_eq!(f.fake.seen().len(), 1);
}

#[test]
fn a_hyperdrive_edit_cannot_carry_a_database_credential() {
    // The inverse of everything else here: an agent handing a secret TO the
    // broker. `edit` declares a name and three caching settings, so the
    // framework refuses the rest — and this is the assertion that would go red
    // if somebody later added an `origin` option.
    let f = Fixture::new("p6hdedit", Mode::Normal, &["cloudflare.hyperdrive.edit"]);
    for (param, value) in [
        ("password", "hunter2"),
        ("user", "postgres"),
        ("host", "db.attacker.test"),
        ("origin", "postgres://user:pw@db.attacker.test/app"),
        ("access_client_secret", "x"),
    ] {
        let reply = f.use_it(f.record("cloudflare.hyperdrive.edit", "pg").param(param, value));
        let (_, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("hyperdrive.edit accepted '{param}'"));
        assert!(message.contains("no '"), "{message}");
    }
    assert!(f.fake.seen().is_empty(), "a credential reached the api");

    // What it can do.
    f.use_it(
        f.record("cloudflare.hyperdrive.edit", "pg")
            .param("caching", "off")
            .param("max-age", "60"),
    );
    let body = &f.fake.seen()[0].body;
    assert_eq!(f.fake.seen()[0].method, "PATCH");
    // `caching = off` is Cloudflare's `disabled = true`, inverted here so the
    // owner grants something spelled the way they think about it.
    assert!(body.contains(r#""disabled":true"#), "{body}");
    assert!(body.contains(r#""max_age":60"#), "{body}");
    assert!(!body.contains("origin"), "{body}");
}

#[test]
fn an_origin_credential_the_api_volunteers_does_not_reach_the_caller() {
    // Cloudflare documents `origin.password` as write-only and says the API
    // never returns it. The double returns one anyway, because a guarantee that
    // an agent gets capabilities and not credentials cannot rest on a remark in
    // somebody else's documentation staying true.
    let f = Fixture::new("p6hdread", Mode::Normal, &["cloudflare.hyperdrive.read"]);
    let reply = f.use_it(f.record("cloudflare.hyperdrive.read", "pg"));
    let Response::Performed { output, .. } = &reply else {
        panic!("refused: {reply:?}");
    };
    // The configuration came back...
    assert!(output.contains("db.example.invalid"), "{output}");
    assert!(output.contains(r#""user":"app""#), "{output}");
    // ...and the password did not.
    assert!(!output.contains(ORIGIN_PASSWORD), "the origin password came back: {output}");
    assert!(!output.contains("password"), "{output}");
    assert!(output.contains("origin credential"), "{output}");
    assert!(!f.trail().contains(ORIGIN_PASSWORD), "the trail holds it");
    // The double really did send one, so this is a removal and not an absence.
    assert_eq!(f.fake.seen().len(), 1);
}

#[test]
fn a_grant_for_one_storage_verb_does_not_grant_its_siblings() {
    // "Semantic read/write/migrate/manage operations represented" — and
    // separately grantable, which is the point of representing them.
    let f = Fixture::new("p6verbs", Mode::Normal, &["cloudflare.d1.read"]);
    with_files(&f);
    let allowed = f.use_it(f.record("cloudflare.d1.read", "project-db"));
    assert!(matches!(allowed, Response::Performed { exit_code: 0, .. }), "{allowed:?}");
    for (refused, options) in [
        ("cloudflare.d1.query", vec![("sql", "SELECT 1")]),
        ("cloudflare.d1.migrate", vec![("file", "migrations/001.sql")]),
    ] {
        let mut rec = f.record(refused, "project-db");
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        assert!(
            f.use_it(rec).as_error().is_some(),
            "'{refused}' went through on a grant for read"
        );
    }
    assert_eq!(f.fake.seen().len(), 1);
}

// ── §13.9, DNS ──────────────────────────────────────────────────────────────
//
// Eight mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back so that cargo rebuilt rather than reusing
// the mutant's binary. Every one turns a named test red:
//
// * `inside()` suffix-matching without the label boundary, so `notexample.com`
//   is inside `example.com` — `a_name_outside_the_bound_zone_…`;
// * the §13.9 type guard skipped on writes — `changing_a_delegation_…`;
// * `Lookup::Denied` folded into `Lookup::Absent`, which is the defect this
//   whole module is shaped around — `a_lookup_that_was_refused_…`;
// * an ambiguous name answered with the first record instead of a refusal —
//   `a_name_that_more_than_one_record_answers_to_…`;
// * the name/type re-check dropped, so the far side's filter is trusted —
//   `a_zone_that_answered_about_other_records_…`;
// * `PUT` in place of `PATCH` on an update — `an_update_sends_only_the_fields_…`;
// * the project's own `records` narrowing ignored — `a_project_that_narrows_…`;
// * `is_record_id` accepting anything non-empty — `a_record_id_off_the_wire_…`.

#[test]
fn a_dns_record_is_addressed_by_name_in_the_zone_this_project_bound() {
    // The documented paths, and the shape of the two-request verbs. An update
    // asks the zone which record `www.example.com` means and only then changes
    // it, which is why there are six requests here for four operations.
    let f = Fixture::new("dnspaths", Mode::Normal, &granted_everything());
    f.use_it(f.record("cloudflare.dns.read", "www.example.com").param("type", "A"));
    f.use_it(
        f.record("cloudflare.dns.create", "new.example.com")
            .param("type", "A")
            .param("content", "203.0.113.7"),
    );
    f.use_it(
        f.record("cloudflare.dns.update", "www.example.com")
            .param("type", "A")
            .param("content", "203.0.113.8"),
    );
    f.use_it(f.record("cloudflare.dns.delete", "old.example.com").param("type", "A"));

    let seen: Vec<(String, String)> = f.fake.seen().into_iter().map(|s| (s.method, s.path)).collect();
    let records = format!("/client/v4/zones/{ZONE}/dns_records");
    assert_eq!(
        seen,
        vec![
            ("GET".into(), format!("{records}?name.exact=www.example.com&type=A")),
            ("POST".into(), records.clone()),
            ("GET".into(), format!("{records}?name.exact=www.example.com&type=A")),
            ("PATCH".into(), format!("{records}/{RECORD_A}")),
            ("GET".into(), format!("{records}?name.exact=old.example.com&type=A")),
            ("DELETE".into(), format!("{records}/{RECORD_A}")),
        ]
    );
}

#[test]
fn a_name_outside_the_bound_zone_never_reaches_cloudflare() {
    // §13.9's first sentence. The label boundary is the case worth writing
    // down: `notexample.com` ends in `example.com` and is a different
    // registration, and a suffix test without the dot would hand it over.
    let f = Fixture::new("dnszone", Mode::Normal, &granted_everything());
    for outside in [
        "notexample.com",
        "example.com.attacker.test",
        "attacker.test",
        "com",
    ] {
        let reply = f.use_it(
            f.record("cloudflare.dns.update", outside)
                .param("type", "A")
                .param("content", "203.0.113.9"),
        );
        let (kind, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("'{outside}' was accepted as a name in this zone"));
        assert_eq!(kind, ErrorKind::BadRequest, "{outside}");
        assert!(message.contains("example.com"), "{message}");
    }
    // ...and the apex itself IS in its own zone, which is the other half: a
    // build that refused everything would pass the loop above.
    let apex = f.use_it(f.record("cloudflare.dns.read", "example.com").param("type", "A"));
    assert!(matches!(apex, Response::Performed { exit_code: 0, .. }), "{apex:?}");
    assert_eq!(f.fake.seen().len(), 1, "only the apex read should have been sent");
}

#[test]
fn a_project_that_narrows_itself_to_some_records_cannot_touch_the_others() {
    // The second half of "bound zones/records": a project may cut itself down
    // to a list, and then the zone is not enough.
    let f = Fixture::new("dnsnarrow", Mode::Normal, &granted_everything());
    std::fs::write(
        f.project.join("apex.toml"),
        format!(
            "[identity.cloudflare]\naccount_id = \"{ACCOUNT}\"\n\
             [cloudflare]\nzone = \"example.com\"\nzone_id = \"{ZONE}\"\n\
             records = [\"www\", \"api.example.com\"]\n"
        ),
    )
    .expect("write");

    // Both spellings of a bound record resolve — `www` and the full name.
    for allowed in ["www.example.com", "api.example.com"] {
        let reply = f.use_it(f.record("cloudflare.dns.read", allowed).param("type", "A"));
        assert!(
            matches!(reply, Response::Performed { exit_code: 0, .. }),
            "{allowed} is bound and was refused: {reply:?}"
        );
    }
    // ...and a name in the same zone that is not on the list is not.
    let reply = f.use_it(
        f.record("cloudflare.dns.delete", "mail.example.com").param("type", "A"),
    );
    let (kind, message) = reply.as_error().expect("a narrowed project must not reach it");
    assert_eq!(kind, ErrorKind::BadRequest);
    assert!(message.contains("mail.example.com"), "{message}");
    assert!(message.contains("www and api.example.com"), "{message}");
    assert_eq!(f.fake.seen().len(), 2, "the refused name still reached the api");
}

#[test]
fn changing_a_delegation_or_a_dnssec_record_is_refused_as_an_elevated_shape() {
    // §13.9's second sentence, as far as a build with one capability class can
    // take it. The owner has granted all four DNS verbs; every reserved type
    // is still refused, and nothing reaches the api — which is the property
    // that matters whether or not an elevated class ever exists.
    let f = Fixture::new("dnselevated", Mode::Normal, &granted_everything());
    for kind in ["NS", "DS", "DNSKEY", "SOA", "ns", "ds"] {
        for (operation, options) in [
            ("cloudflare.dns.create", vec![("content", "ns1.attacker.test")]),
            ("cloudflare.dns.update", vec![("content", "ns1.attacker.test")]),
            ("cloudflare.dns.delete", vec![]),
        ] {
            let mut rec = f.record(operation, "example.com").param("type", kind);
            for (name, value) in &options {
                rec = rec.param(name, value);
            }
            let reply = f.use_it(rec);
            let (error, message) = reply
                .as_error()
                .unwrap_or_else(|| panic!("{operation} accepted a {kind} record"));
            assert_eq!(error, ErrorKind::PermissionDenied, "{operation} {kind}");
            assert!(message.contains("elevated"), "{message}");
            assert!(message.contains("§13.9"), "{message}");
        }
    }
    assert!(
        f.fake.seen().is_empty(),
        "a reserved record type reached the api: {:?}",
        f.fake.seen()
    );

    // Reading one is allowed: §13.9 reserves *changes*, and an agent that
    // cannot see where a zone is delegated cannot check its own work.
    let read = f.use_it(f.record("cloudflare.dns.read", "example.com").param("type", "NS"));
    assert!(matches!(read, Response::Performed { exit_code: 0, .. }), "{read:?}");
    assert_eq!(f.fake.seen().len(), 1);
}

#[test]
fn a_lookup_that_was_refused_is_not_reported_as_a_record_that_is_not_there() {
    // The defect shape this codebase has found about fifteen times, on the one
    // path here that could reintroduce it. The zone answers 403 — a body that
    // looks like every other failure — and the answer must not be "no such
    // record", because a caller told that would go and create a second one.
    let f = Fixture::new("dnsdenied", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.dns.update", "denied.example.com")
            .param("type", "A")
            .param("content", "203.0.113.9"),
    );
    let (kind, message) = reply.as_error().expect("a refused lookup is not a success");
    assert_eq!(
        kind,
        ErrorKind::PermissionDenied,
        "a denial must not be reported as a bad request about a missing record"
    );
    assert!(message.contains("403"), "{message}");
    assert!(message.contains("not the same as the record not being there"), "{message}");
    assert!(!message.contains("holds no"), "{message}");
    // One request — the lookup. Nothing was changed.
    let seen = f.fake.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].method, "GET");
}

#[test]
fn a_record_that_is_absent_says_the_zone_answered_and_holds_none() {
    // The other side of the same distinction, and the reason it is worth
    // having: this one really is an absence, and it says so in those words.
    let f = Fixture::new("dnsabsent", Mode::Normal, &granted_everything());
    let reply = f.use_it(f.record("cloudflare.dns.delete", "gone.example.com").param("type", "A"));
    let (kind, message) = reply.as_error().expect("deleting nothing is not a success");
    assert_eq!(kind, ErrorKind::BadRequest);
    assert!(message.contains("answered"), "{message}");
    assert!(message.contains("holds no A record"), "{message}");
    assert_eq!(f.fake.seen().len(), 1, "it deleted something anyway");
}

#[test]
fn a_name_that_more_than_one_record_answers_to_is_refused_rather_than_guessed() {
    // Two A records at one name is round-robin, not a fault. Changing the
    // first would change an arbitrary one of them.
    let f = Fixture::new("dnsmany", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.dns.update", "many.example.com")
            .param("type", "A")
            .param("content", "203.0.113.9"),
    );
    let (kind, message) = reply.as_error().expect("an ambiguous name must be refused");
    assert_eq!(kind, ErrorKind::PermissionDenied);
    assert!(message.contains("2 A records"), "{message}");
    assert_eq!(f.fake.seen().len(), 1, "one of them was changed");
}

#[test]
fn a_zone_that_answered_about_other_records_is_not_taken_at_its_word() {
    // A filter that was sent is not a filter that was applied. If the query
    // parameter were ever spelled wrongly, or stopped being honoured, the
    // lookup would come back holding the whole zone — and the first record in
    // it would be modified. Checking the names here turns that into a refusal.
    let f = Fixture::new("dnsunfiltered", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.dns.update", "unfiltered.example.com")
            .param("type", "A")
            .param("content", "203.0.113.9"),
    );
    let (kind, message) = reply.as_error().expect("records for other names are not this one");
    assert_eq!(kind, ErrorKind::BadRequest);
    assert!(message.contains("holds no A record"), "{message}");
    assert_eq!(f.fake.seen().len(), 1, "somebody else's record was changed");
}

#[test]
fn an_update_that_changes_nothing_is_refused_before_anything_is_looked_up() {
    let f = Fixture::new("dnsnoop", Mode::Normal, &granted_everything());
    let reply = f.use_it(f.record("cloudflare.dns.update", "www.example.com").param("type", "A"));
    let (_, message) = reply.as_error().expect("an update with no fields is not an update");
    assert!(message.contains("changes nothing"), "{message}");
    assert!(f.fake.seen().is_empty(), "it spent a request finding out");
}

#[test]
fn an_update_sends_only_the_fields_it_was_given() {
    // PATCH and not PUT. Cloudflare's PUT overwrites a record with what the
    // request carries, so an update that set only the content through PUT
    // would quietly reset the TTL and the proxy flag.
    let f = Fixture::new("dnspatch", Mode::Normal, &["cloudflare.dns.update"]);
    f.use_it(
        f.record("cloudflare.dns.update", "www.example.com")
            .param("type", "A")
            .param("content", "203.0.113.8"),
    );
    let sent = f.fake.seen();
    let patch = sent.last().expect("two requests");
    assert_eq!(patch.method, "PATCH");
    assert!(patch.body.contains(r#""content":"203.0.113.8""#), "{}", patch.body);
    assert!(!patch.body.contains("ttl"), "{}", patch.body);
    assert!(!patch.body.contains("proxied"), "{}", patch.body);
    assert!(!patch.body.contains("name"), "{}", patch.body);
}

#[test]
fn a_create_carries_the_whole_record_and_values_cloudflare_would_accept() {
    let f = Fixture::new("dnscreate", Mode::Normal, &["cloudflare.dns.create"]);
    f.use_it(
        f.record("cloudflare.dns.create", "new.example.com")
            .param("type", "a")
            .param("content", "203.0.113.7")
            .param("ttl", "300")
            .param("proxied", "true")
            .param("comment", "added by apex"),
    );
    let sent = f.fake.seen();
    let create = sent.first().expect("one request");
    // The type is upper-cased, because that is what the zone stores.
    assert!(create.body.contains(r#""type":"A""#), "{}", create.body);
    assert!(create.body.contains(r#""name":"new.example.com""#), "{}", create.body);
    assert!(create.body.contains(r#""content":"203.0.113.7""#), "{}", create.body);
    assert!(create.body.contains(r#""ttl":300"#), "{}", create.body);
    assert!(create.body.contains(r#""proxied":true"#), "{}", create.body);
    assert!(create.body.contains(r#""comment":"added by apex""#), "{}", create.body);

    // Values the far side would refuse are refused here, where refusing costs
    // nothing and the message can name the option.
    for (param, value) in [
        ("ttl", "30"),
        ("ttl", "999999"),
        ("ttl", "soon"),
        ("proxied", "yes"),
        ("type", "NOTATYPE"),
    ] {
        let reply = f.use_it(
            f.record("cloudflare.dns.create", "new.example.com")
                .param("type", "A")
                .param("content", "203.0.113.7")
                .param(param, value),
        );
        assert!(
            reply.as_error().is_some(),
            "{param}={value} was sent to cloudflare"
        );
    }
    assert_eq!(f.fake.seen().len(), 1, "a refused value still reached the api");
}

#[test]
fn the_four_dns_verbs_are_four_grants() {
    // §13.2's argument where it matters most: an agent that may point a
    // hostname at a new worker should not thereby be able to delete the zone's
    // records.
    let f = Fixture::new("dnsverbs", Mode::Normal, &["cloudflare.dns.update"]);
    let allowed = f.use_it(
        f.record("cloudflare.dns.update", "www.example.com")
            .param("type", "A")
            .param("content", "203.0.113.8"),
    );
    assert!(matches!(allowed, Response::Performed { exit_code: 0, .. }), "{allowed:?}");
    for refused in [
        "cloudflare.dns.read",
        "cloudflare.dns.create",
        "cloudflare.dns.delete",
    ] {
        let reply = f.use_it(
            f.record(refused, "www.example.com")
                .param("type", "A")
                .param("content", "203.0.113.8"),
        );
        assert!(reply.as_error().is_some(), "'{refused}' went through on a grant for update");
    }
    // The lookup and the patch, and nothing else.
    assert_eq!(f.fake.seen().len(), 2);
}

#[test]
fn the_trail_names_the_record_and_the_zone_that_was_changed() {
    let f = Fixture::new("dnstrail", Mode::Normal, &["cloudflare.dns.update"]);
    f.use_it(
        f.record("cloudflare.dns.update", "www.example.com")
            .param("type", "A")
            .param("content", "203.0.113.8"),
    );
    let lines = audit::tail(&Store::new(f.store.clone()).audit_path(), 10);
    let used = lines.iter().find(|l| l.event == AuditEvent::Used).expect("a use");
    assert_eq!(used.operation, "cloudflare.dns.update");
    assert_eq!(used.resource, "www.example.com");
    assert_eq!(
        used.detail,
        format!("change the A record at www.example.com in zone example.com [{ZONE}]")
    );
}

// ── §13.11, Workers AI and the AI Gateway ───────────────────────────────────
//
// Nine mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back — and then touching it, which the earlier
// rounds of this program did not have to do and this one did. `cp -p` restores
// the ORIGINAL mtime, which is older than the artifact cargo just built from
// the mutant, so cargo skips the rebuild and the next run measures the MUTANT'S
// BINARY against pristine source. That was caught here by a restored tree
// failing a test it had just passed; every verdict below was re-taken after the
// restore started bumping the mtime.
//
// Every one turns a named test red:
//
// * the header guard in `api::call` stops consulting `printable` —
//   `a_header_carrying_a_second_line_is_refused_before_curl_is_configured`;
// * `cf-aig-gateway-id` is not sent — `a_gatewayed_run_is_the_same_host_…`;
// * `binding.resource("models", …)` replaced by the caller's own string, which
//   is the defect §13.11 exists to prevent — `a_model_or_a_gateway_this_…`;
// * the project's whole PATH sent as the metadata's project —
//   `a_gatewayed_run_is_tagged_with_this_task_…`;
// * `carries_authorization` never fires — `a_gateway_carrying_an_exporter_…`;
// * the `stream` check dropped — `a_streamed_response_is_refused_…`;
// * `GATEWAY_FIELDS` put back nothing — `a_gateway_edit_puts_back_everything_…`;
// * `workers-ai.run` declared `Effect::Read` — `the_declaration_is_well_formed_…`;
// * `valid_model` stops checking segments — `a_model_name_is_checked_as_…`.
//
// **Three of those nine did not bite the first time**, and the tests were the
// thing that changed, not the code:
//
// * the project-path leak passed, because the filter that makes a name
//   header-safe strips `/` as a side effect, so a leaked path arrives with no
//   slash in it and neither "contains no slash" nor "is not the path" notices;
// * the read-modify-write passed with the read discarded entirely, because the
//   assertion asked about `collect_logs` — which the required-field fallback
//   sets to `true` regardless, so preserved and defaulted looked the same;
// * `Effect::Read` on a paid operation passed, because nothing asserted it.
//
// All three are the shape this unit was warned about: a check that is real in
// the code and invisible to the test, which is indistinguishable from no check
// at all the day somebody edits the code.

#[test]
fn a_gatewayed_run_is_the_same_host_and_the_same_credential_as_an_ungatewayed_one() {
    // The finding this design rests on. The provider-specific endpoint at
    // `gateway.ai.cloudflare.com` would be a second host, and the framework
    // pins a credential to the host it was stored for — so it would need a
    // second stored credential. Cloudflare's REST API page documents the
    // gatewayed form of the ORDINARY call instead: same URL, plus a
    // `cf-aig-gateway-id` header. This asserts that shape, because if it ever
    // stopped being the shape, the pin would be the thing that noticed.
    let f = Fixture::new("aigateway", Mode::Normal, &granted_everything());
    let plain = f.use_it(f.record("cloudflare.workers-ai.run", "fast").param("prompt", "hi"));
    assert!(matches!(plain, Response::Performed { exit_code: 0, .. }), "{plain:?}");
    let gatewayed = f.use_it(
        f.record("cloudflare.ai-gateway.run", "main/fast").param("prompt", "hi"),
    );
    assert!(matches!(gatewayed, Response::Performed { exit_code: 0, .. }), "{gatewayed:?}");

    let sent = f.fake.seen();
    let runs: Vec<&Seen> = sent.iter().filter(|s| s.path.contains("/ai/run/")).collect();
    assert_eq!(runs.len(), 2, "two runs, two requests");
    assert_eq!(runs[0].path, runs[1].path, "the gateway changed the URL");
    // The ungatewayed one carries no gateway header, and the gatewayed one
    // carries the id from the PROJECT'S FILE.
    assert!(!runs[0].headers.contains_key("cf-aig-gateway-id"), "{:?}", runs[0].headers);
    assert_eq!(
        runs[1].headers.get("cf-aig-gateway-id").map(String::as_str),
        Some(GATEWAY_ID),
        "{:?}",
        runs[1].headers
    );
}

#[test]
fn a_gatewayed_run_is_tagged_with_this_task_and_not_with_this_machine_s_paths() {
    // P1-009's third criterion: usage attributable to a task and a project.
    // The correlation key is the audit id, which this machine's own trail maps
    // to the project, the session and the origin — so the far side's log needs
    // nothing else, and the owner's directory layout stays here.
    let f = Fixture::new("aimeta", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.ai-gateway.run", "main/fast").param("prompt", "hi"),
    );
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");

    let sent = f.fake.seen();
    let run = sent.iter().find(|s| s.path.contains("/ai/run/")).expect("nothing was sent");
    let raw = run.headers.get("cf-aig-metadata").expect("no metadata header");
    let meta: serde_json::Value = serde_json::from_str(raw).expect("metadata must be json");
    let object = meta.as_object().expect("an object");

    // AI Gateway keeps the first five entries and strips anything beginning
    // `cf.`, which is its own namespace.
    assert!(object.len() <= 5, "more than five entries: {raw}");
    assert!(object.keys().all(|k| !k.starts_with("cf.")), "{raw}");
    assert!(object.values().all(|v| v.is_string() || v.is_number() || v.is_boolean()), "{raw}");

    assert_eq!(
        object.get("apex_operation").and_then(|v| v.as_str()),
        Some("cloudflare.ai-gateway.run")
    );
    // The audit id is a real one from this request, and it is what joins the
    // far side's log to this machine's trail.
    let audit = object.get("apex_audit").and_then(|v| v.as_str()).expect("no audit id");
    assert!(!audit.is_empty() && audit != "test", "{raw}");
    assert!(f.trail().contains(audit), "the id sent is not one this trail holds: {audit}");

    // The project's NAME is there and its PATH is not.
    //
    // **The equality is the assertion, and the two weaker checks below it are
    // not enough on their own.** A mutation that sent `req.project` whole was
    // run against this test and it stayed green: the character filter that
    // makes a name header-safe also strips `/`, so the full path arrives as
    // `tmpapex-cf-project-…` — no slash in it, and not equal to the path
    // either, so both of the obvious checks pass while every directory on the
    // way to the project is in somebody else's logs. Only pinning the value to
    // the directory's OWN name catches that.
    let project = f.project.to_string_lossy().into_owned();
    let expected: String = f
        .project
        .file_name()
        .and_then(|n| n.to_str())
        .expect("the fixture's project has a name")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(48)
        .collect();
    assert_eq!(
        object.get("apex_project").and_then(|v| v.as_str()),
        Some(expected.as_str()),
        "{raw}"
    );
    assert!(!raw.contains(&project), "the project's path was sent to cloudflare: {raw}");
    // The same path with its separators taken out — the exact shape the leak
    // takes once the filter has been through it.
    let squashed: String = project.chars().filter(|c| *c != '/').collect();
    assert!(
        !raw.contains(&squashed),
        "the project's path was sent with its separators stripped: {raw}"
    );
    assert!(
        object.get("apex_project").and_then(|v| v.as_str()).is_some_and(|p| !p.contains('/')),
        "{raw}"
    );
}

#[test]
fn a_model_or_a_gateway_this_project_did_not_bind_never_reaches_cloudflare() {
    // A model cannot even be NAMED as a resource — `@cf/...` is not a
    // `Syntax::Name` — so binding is not a policy choice here, it is the only
    // way to address one. The gateway is bound for a different reason worth
    // stating: Cloudflare creates a gateway on first use if the id is unknown,
    // so a caller that could name one could bill one.
    let f = Fixture::new("unboundai", Mode::Normal, &granted_everything());
    for (operation, resource) in [
        ("cloudflare.workers-ai.run", "somebody-elses-model"),
        ("cloudflare.ai-gateway.run", "somebody-elses-gateway/fast"),
        ("cloudflare.ai-gateway.run", "main/somebody-elses-model"),
        ("cloudflare.ai-gateway.edit", "somebody-elses-gateway"),
    ] {
        let mut rec = f.record(operation, resource);
        if operation.ends_with(".run") {
            rec = rec.param("prompt", "hi");
        } else {
            rec = rec.param("cache-ttl", "300");
        }
        let reply = f.use_it(rec);
        let (error, message) = match reply.as_error() {
            Some(pair) => pair,
            None => panic!("{operation} accepted '{resource}'"),
        };
        assert_eq!(error, ErrorKind::BadRequest, "{operation}: {message}");
        assert!(message.contains("apex.toml"), "{message}");
    }
    assert!(f.fake.seen().is_empty(), "an unbound model or gateway reached the api");

    // ...and the model name itself is refused by the vocabulary, which is the
    // thing that makes the binding unavoidable rather than merely advisable.
    assert!(!operation::valid_name(MODEL), "'{MODEL}' would be nameable as a resource");
}

#[test]
fn a_streamed_response_is_refused_rather_than_delivered_as_framing() {
    // The transport is a `curl` that hands back a finished reply. A streamed
    // answer would arrive as server-sent-event framing in `output`: not a
    // stream, not the JSON the caller asked for, and indistinguishable from a
    // working feature until somebody tries to parse it.
    let f = Fixture::new("aistream", Mode::Normal, &granted_everything());
    std::fs::create_dir_all(f.project.join("ai")).expect("ai");
    std::fs::write(
        f.project.join("ai/streamed.json"),
        r#"{"messages":[{"role":"user","content":"hi"}],"stream":true}"#,
    )
    .expect("write");
    let reply = f.use_it(
        f.record("cloudflare.workers-ai.run", "fast").param("file", "ai/streamed.json"),
    );
    let (_, message) = reply.as_error().expect("a streamed request must be refused");
    assert!(message.contains("streamed response"), "{message}");
    assert!(f.fake.seen().is_empty(), "it was sent anyway");

    // The same document without `stream` goes through, so this is a check on
    // one field rather than a refusal of every file.
    std::fs::write(
        f.project.join("ai/ok.json"),
        r#"{"messages":[{"role":"user","content":"hi"}]}"#,
    )
    .expect("write");
    let ok = f.use_it(f.record("cloudflare.workers-ai.run", "fast").param("file", "ai/ok.json"));
    assert!(matches!(ok, Response::Performed { exit_code: 0, .. }), "{ok:?}");
}

#[test]
fn a_gateway_edit_puts_back_everything_it_did_not_mean_to_change() {
    // `PUT` REPLACES a gateway, and its schema marks five fields required — so
    // a request carrying only the changed field is not a smaller edit, it is a
    // rejected one, and everything the gateway had would go back to a default.
    // Hence the read first, and hence this assertion: the field that was asked
    // for changed, and a field nobody mentioned survived.
    let f = Fixture::new("aiedit", Mode::Normal, &granted_everything());
    let reply = f.use_it(f.record("cloudflare.ai-gateway.edit", "main").param("cache-ttl", "300"));
    assert!(matches!(reply, Response::Performed { exit_code: 0, .. }), "{reply:?}");

    let sent = f.fake.seen();
    assert_eq!(sent.len(), 2, "a replace has to read first: {sent:?}");
    assert_eq!(sent[0].method, "GET");
    let put = &sent[1];
    assert_eq!(put.method, "PUT");
    let body: serde_json::Value = serde_json::from_str(&put.body).expect("json body");
    assert_eq!(body.get("cache_ttl").and_then(|v| v.as_u64()), Some(300), "{}", put.body);

    // Untouched, and still there — the whole reason for the read.
    //
    // **Every value checked here is one the fallbacks could not have supplied.**
    // A mutation that put back nothing at all still passed an earlier version
    // of this assertion, because it asked about `collect_logs` — which the
    // required-field fallback sets to `true` regardless, making a preserved
    // value and a defaulted one indistinguishable. These four are not
    // defaulted: `rate_limiting_limit` falls back to 0 and the gateway says 42,
    // `cache_invalidate_on_update` falls back to false and the gateway says
    // true, and `logpush` and `retry_max_attempts` have no fallback at all. If
    // any of them arrives wrong, the read did not happen or was discarded.
    assert_eq!(
        body.get("rate_limiting_limit").and_then(|v| v.as_u64()),
        Some(42),
        "a fallback overwrote what the gateway already had: {}",
        put.body
    );
    assert_eq!(
        body.get("cache_invalidate_on_update").and_then(|v| v.as_bool()),
        Some(true),
        "{}",
        put.body
    );
    assert_eq!(body.get("logpush").and_then(|v| v.as_bool()), Some(true), "{}", put.body);
    assert_eq!(
        body.get("retry_max_attempts").and_then(|v| v.as_u64()),
        Some(3),
        "a setting this build does not know about was dropped: {}",
        put.body
    );
    assert_eq!(body.get("collect_logs").and_then(|v| v.as_bool()), Some(true), "{}", put.body);
    // The five the schema marks required are all present, whatever was asked.
    for required in [
        "rate_limiting_interval",
        "rate_limiting_limit",
        "collect_logs",
        "cache_ttl",
        "cache_invalidate_on_update",
    ] {
        assert!(body.get(required).is_some(), "{required} is missing: {}", put.body);
    }
    // Read-only fields the API returns are NOT written back.
    for readonly in ["created_at", "modified_at", "id", "is_default"] {
        assert!(body.get(readonly).is_none(), "{readonly} was written back: {}", put.body);
    }
}

#[test]
fn a_gateway_carrying_an_exporter_credential_is_not_rewritten_at_all() {
    // The case a replace cannot do honestly. A gateway's OTel exporter carries
    // an `authorization`, nested inside an ARRAY; putting it back means writing
    // whatever the API chose to show, and leaving it out means deleting it.
    // Refusing is the third option and the only one that cannot silently break
    // somebody's telemetry.
    let f = Fixture::new("aiotel", Mode::Normal, &granted_everything());
    std::fs::write(
        f.project.join("apex.toml"),
        PROJECT_FILE.replace(r#"main = "apex-gateway""#, r#"main = "with-otel""#),
    )
    .expect("rewrite");
    let reply = f.use_it(f.record("cloudflare.ai-gateway.edit", "main").param("cache-ttl", "300"));
    let (error, message) = reply.as_error().expect("a gateway with a credential must be refused");
    assert_eq!(error, ErrorKind::PermissionDenied, "{message}");
    assert!(message.contains("exporter credential"), "{message}");
    assert!(message.contains("Nothing was changed"), "{message}");

    // It read, and it did not write — and the credential is not in the reply.
    let sent = f.fake.seen();
    assert!(sent.iter().all(|s| s.method != "PUT"), "the gateway was rewritten anyway");
    assert!(!message.contains(OTEL_SECRET), "the exporter credential came back: {message}");
    assert!(!f.trail().contains(OTEL_SECRET), "the exporter credential is in the trail");
}

#[test]
fn a_header_this_build_composes_can_never_carry_a_second_line() {
    // `quoted` escapes a backslash and a quote and does nothing about a
    // newline, and a newline in a header value is a second curl configuration
    // line — which is a second header, or an option. Every composed value is
    // held to printable ASCII before it gets there.
    assert!(api::printable("apex-gateway"));
    assert!(api::printable(r#"{"apex_audit":"1a0-1","apex_project":"demo"}"#));
    for evil in [
        "one\ntwo",
        "one\r\nheader: two",
        "one\u{0}two",
        "",
        "caf\u{e9}",
        "one\ttwo",
    ] {
        assert!(!api::printable(evil), "'{}' was accepted", evil.escape_debug());
    }
}

// ── §13.6, the Secrets Store ────────────────────────────────────────────────
//
// Seven mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back so that cargo rebuilt rather than reusing
// the mutant's binary. Every one turns a named test red:
//
// * the lookup trusting `search` instead of checking the exact name, which is
//   the ordinary mistake here because `search` is a SUBSTRING match and a store
//   holding `API_KEY` and `API_KEY_OLD` answers for both —
//   `rotating_a_secret_matches_the_exact_name_and_not_a_prefix_of_it`;
// * a refused credential reported as a missing secret —
//   `a_secret_that_is_not_there_is_told_apart_from_a_credential_…`;
// * "you named no secret" reported as "you may not" —
//   `a_store_this_project_did_not_bind_…`. That one is the THIRD instance of
//   this defect in the Cloudflare block, and the two it joins (R2's
//   bucket-with-no-key and KV's namespace-with-no-key) were found while writing
//   it and fixed in the same commit;
// * a value given twice silently picked, and scopes forwarded unchecked —
//   `a_value_given_twice_or_not_at_all_…`, `a_scope_cloudflare_does_not_…`;
// * the binding replacing the worker's bindings instead of merging into them,
//   which would take every other binding off the worker —
//   `a_binding_is_a_reference_and_the_worker_s_other_bindings_survive_it`;
// * the shell's trailing newline sent as part of the secret —
//   `a_secret_value_travels_as_input_and_never_as_an_option`.

#[test]
fn a_secret_value_travels_as_input_and_never_as_an_option() {
    // **The claim §13.6's block note makes, measured.** A parameter is rendered
    // into `CapabilityRecord::summary` and becomes the `detail` of every
    // refused audit line — so this test proves BOTH halves: the value the
    // caller piped in reached Cloudflare and is nowhere in the trail, while a
    // parameter of the same operation is in the trail in full. The second half
    // is what makes the first a decision rather than a coincidence.
    let f = Fixture::new("secretbody", Mode::Normal, &granted_everything());
    // Sent the way a shell sends one — `echo` puts a newline on the end, and a
    // secret that works from `printf %s` and not from `echo` is an afternoon
    // somebody does not get back. One trailing newline is taken off.
    let reply = f.use_with_input(
        f.record("cloudflare.secret.create", "app/API_KEY").param("scopes", "workers"),
        &format!("{SECRET_VALUE}\n"),
    );
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");

    // It arrived, as the value of the one secret being created.
    let sent = f.fake.seen();
    let create = sent
        .iter()
        .find(|s| s.method == "POST" && s.path.contains("/secrets_store/"))
        .expect("nothing was sent");
    assert!(
        create.body.contains(&format!(r#""value":"{SECRET_VALUE}""#)),
        "the value did not arrive, or arrived with the shell's newline on it: {}",
        create.body
    );
    assert!(create.body.contains(r#""name":"API_KEY""#), "{}", create.body);
    assert!(create.body.contains(r#""scopes":["workers"]"#), "{}", create.body);

    // It is not in the trail...
    assert!(!f.trail().contains(SECRET_VALUE), "the secret is in the audit trail");

    // ...and here is what would have happened if it HAD been an option.
    // `AuditLine::from_record` writes `CapabilityRecord::summary` as the detail
    // of every REFUSED line, and `summary` renders each parameter as
    // `name=value`. So one refused request — a store this project does not bind
    // — puts its options in the trail verbatim, on the path where the operation
    // never even happened. That is the argument for the body, measured rather
    // than asserted.
    let refused = f.use_with_input(
        f.record("cloudflare.secret.create", "somebody-elses-store/API_KEY")
            .param("scopes", "workers"),
        SECRET_VALUE,
    );
    assert!(refused.as_error().is_some(), "{refused:?}");
    let trail = f.trail();
    assert!(
        trail.contains("scopes=workers"),
        "a parameter did not reach the trail, so the reason the value is not one \
         no longer holds: {trail}"
    );
    assert!(!trail.contains(SECRET_VALUE), "the secret reached the trail on a refusal");

    // §13.6: "APEX stores only the provider reference/metadata, not fictitious
    // plaintext". Nothing was written to this service's own store.
    assert!(
        Store::new(f.store.clone()).list(me().uid).iter().all(|s| s.service == "cloudflare"),
        "a secret pushed to Cloudflare was also stored here"
    );
}

#[test]
fn no_secret_operation_declares_a_value_option() {
    // The property above, as a property of the DECLARATION rather than of one
    // code path: the framework refuses an option no operation declares, so a
    // vocabulary with no `value` cannot be handed one however the caller asks.
    for op in SPEC.operations.iter().filter(|op| op.id.starts_with("cloudflare.secret.")) {
        assert!(
            !op.params.iter().any(|p| p.name == "value" || p.name == "secret"),
            "'{}' declares an option that would put a secret in the audit trail",
            op.id
        );
    }
    let f = Fixture::new("novalue", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.secret.create", "app/API_KEY")
            .param("scopes", "workers")
            .param("value", SECRET_VALUE),
    );
    let (error, message) = reply.as_error().expect("an undeclared option must be refused");
    assert_eq!(error, ErrorKind::BadRequest, "{message}");
    assert!(f.fake.seen().is_empty(), "it was sent anyway");
}

#[test]
fn rotating_a_secret_matches_the_exact_name_and_not_a_prefix_of_it() {
    // The endpoint's filter is `search`, and `search` is a SUBSTRING match: a
    // store holding `API_KEY` and `API_KEY_OLD` answers a search for `API_KEY`
    // with both. A build that took the first result would rotate whichever came
    // back first — silently, and the caller would be told it worked.
    let f = Fixture::new("rotate", Mode::Normal, &granted_everything());
    let reply = f.use_with_input(f.record("cloudflare.secret.rotate", "app/API_KEY"), SECRET_VALUE);
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");

    let sent = f.fake.seen();
    let patch = sent.iter().find(|s| s.method == "PATCH").expect("nothing was changed");
    assert!(patch.path.ends_with(SECRET_ID), "the wrong secret was rotated: {}", patch.path);
    assert!(!patch.path.contains(OLD_SECRET_ID), "{}", patch.path);
    assert!(patch.body.contains(SECRET_VALUE), "{}", patch.body);
    // A rotate does not rename or re-scope: those are the fields a replace
    // would reset, and this endpoint is a PATCH precisely so it need not.
    assert!(!patch.body.contains(r#""name""#), "{}", patch.body);
    assert!(!f.trail().contains(SECRET_VALUE), "the new value is in the audit trail");
}

#[test]
fn a_secret_that_is_not_there_is_told_apart_from_a_credential_that_was_refused() {
    // The five answers, again, because the alternative is the defect this
    // codebase has now found about fifteen times: a refusal reported as an
    // absence. A caller told "no such secret" creates a second one beside the
    // first.
    let f = Fixture::new("fivewaysecret", Mode::Normal, &granted_everything());
    let reply = f.use_with_input(f.record("cloudflare.secret.rotate", "app/NOT_THERE"), SECRET_VALUE);
    let (error, message) = reply.as_error().expect("a missing secret must be refused");
    assert_eq!(error, ErrorKind::BadRequest, "{message}");
    assert!(message.contains("holds no secret"), "{message}");
    assert!(message.contains("Nothing was changed"), "{message}");
    // Nothing was PATCHed — the lookup ran and stopped there.
    assert!(
        f.fake.seen().iter().all(|s| s.method != "PATCH"),
        "something was changed after a failed lookup"
    );

    // ...and a credential the far side refuses is NOT reported as an absence.
    let denied = Fixture::new("deniedsecret", Mode::EchoUnauthorized, &granted_everything());
    let reply = denied.use_with_input(
        denied.record("cloudflare.secret.rotate", "app/API_KEY"),
        SECRET_VALUE,
    );
    let (error, message) = reply.as_error().expect("a refused credential is not a success");
    assert_ne!(
        error,
        ErrorKind::BadRequest,
        "a refused credential was reported as a missing secret: {message}"
    );
    assert!(message.contains("not the same as the secret not being there"), "{message}");
}

#[test]
fn a_binding_is_a_reference_and_the_worker_s_other_bindings_survive_it() {
    // §13.6: "references/metadata only". What goes onto the worker is a store
    // id and a secret NAME; nothing here ever reads the value, and there is no
    // path by which it could.
    //
    // The settings PATCH replaces the `bindings` list wholesale, so this is a
    // read-modify-write — and the pre-existing binding in the double is what
    // proves the merge happened rather than a replacement that looked fine.
    let f = Fixture::new("bind", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.secret.bind", "app/API_KEY")
            .param("worker", "project")
            .param("binding", "API_KEY"),
    );
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");

    let sent = f.fake.seen();
    let patch = sent
        .iter()
        .find(|s| s.method == "PATCH" && s.path.ends_with("/settings"))
        .expect("the worker's settings were not changed");
    assert!(patch.body.contains(r#""type":"secrets_store_secret""#), "{}", patch.body);
    assert!(patch.body.contains(&format!(r#""store_id":"{STORE_ID}""#)), "{}", patch.body);
    assert!(patch.body.contains(r#""secret_name":"API_KEY""#), "{}", patch.body);
    // The reference carries no value, and there is nowhere for one to come
    // from: Cloudflare never returns it.
    assert!(!patch.body.contains("\"text\":\"" ) || patch.body.contains("GREETING"), "{}", patch.body);
    assert!(!patch.body.contains(SECRET_VALUE), "{}", patch.body);
    // ...and the bindings that were already there are still there, as
    // `inherit` — which is what keeps `OLD_SECRET`'s value. Sending that one
    // back the way it arrived, with no `text`, is how a build silently empties
    // a secret it was never asked to touch.
    assert!(patch.body.contains("GREETING"), "an existing binding was dropped: {}", patch.body);
    assert!(patch.body.contains("OLD_SECRET"), "an existing secret binding was dropped: {}", patch.body);
    assert!(
        patch.body.contains(r#"{"name":"OLD_SECRET","type":"inherit"}"#)
            || patch.body.contains(r#"{"type":"inherit","name":"OLD_SECRET"}"#),
        "an existing secret binding was sent back without its value instead of inherited: {}",
        patch.body
    );
    assert!(
        !patch.body.contains(r#""type":"secret_text""#),
        "a write-only binding was echoed back: {}",
        patch.body
    );
}

#[test]
fn a_store_this_project_did_not_bind_never_reaches_cloudflare() {
    // The §13.1 boundary for §13.6: a project reaches the secrets in the stores
    // its own file lists. The ERROR KIND is asserted — "you did not bind that"
    // must not arrive as "you may not".
    let f = Fixture::new("unboundstore", Mode::Normal, &granted_everything());
    for (resource, why) in [
        ("somebody-elses-store/API_KEY", "a store this project does not bind"),
        ("app", "a store with no secret named in it"),
    ] {
        let reply = f.use_with_input(
            f.record("cloudflare.secret.create", resource).param("scopes", "workers"),
            SECRET_VALUE,
        );
        let (error, message) = match reply.as_error() {
            Some(pair) => pair,
            None => panic!("{why} was accepted"),
        };
        assert_eq!(error, ErrorKind::BadRequest, "{why}: {message}");
    }
    assert!(f.fake.seen().is_empty(), "an unbound store reached the api");
}

#[test]
fn a_scope_cloudflare_does_not_document_is_refused_before_the_credential_is_spent() {
    let f = Fixture::new("scopes", Mode::Normal, &granted_everything());
    for bad in ["everything", "workers,everything", "Workers", ""] {
        let reply = f.use_with_input(
            f.record("cloudflare.secret.create", "app/API_KEY").param("scopes", bad),
            SECRET_VALUE,
        );
        assert!(reply.as_error().is_some(), "'{bad}' was accepted as a scope");
    }
    assert!(f.fake.seen().is_empty(), "a bad scope reached the api");

    // ...and every scope the schema lists is accepted, so the check above is
    // not simply refusing everything.
    let ok = f.use_with_input(
        f.record("cloudflare.secret.create", "app/API_KEY")
            .param("scopes", "workers,ai_gateway,access"),
        SECRET_VALUE,
    );
    assert!(matches!(ok, Response::Performed { exit_code: 0, .. }), "{ok:?}");
}

#[test]
fn a_value_given_twice_or_not_at_all_is_refused_rather_than_guessed() {
    let f = Fixture::new("bothorneither", Mode::Normal, &granted_everything());
    with_files(&f);
    // Neither.
    let neither = f.use_it(
        f.record("cloudflare.secret.create", "app/API_KEY").param("scopes", "workers"),
    );
    let (_, message) = neither.as_error().expect("a secret with no value is not one");
    assert!(message.contains("needs the secret's value"), "{message}");
    // Both.
    let both = f.use_with_input(
        f.record("cloudflare.secret.create", "app/API_KEY")
            .param("scopes", "workers")
            .param("file", "secrets/value.txt"),
        SECRET_VALUE,
    );
    let (_, message) = both.as_error().expect("two values is not one value");
    assert!(message.contains("will not pick"), "{message}");
    assert!(f.fake.seen().is_empty(), "one of them was sent anyway");
}

// ── §13.10, Cloudflare One ──────────────────────────────────────────────────
//
// Seven mutations were run against the arms below, one at a time, each restored
// by copying the pristine file back so that cargo rebuilt rather than reusing
// the mutant's binary. Every one turns a named test red:
//
// * the provider no longer stripping `client_secret` from its own reply —
//   `the_provider_takes_the_token_secret_out_before_the_framework_ever_sees_it`.
//   Worth reading the note in that test: with BOTH layers present this mutation
//   changed nothing any test going through `use_capability` could see, because
//   the framework's scrub caught it. The test was rewritten to call `perform`
//   directly, and only then did the mutation bite. Two layers need two tests;
//   the framework's is `service.rs`'s `SEAM-M1`;
// * the created credential pinned to `api.cloudflare.com` — the API that
//   ISSUED it rather than the host it may be sent to — and the host taken from
//   the caller instead of resolved through the project's binding:
//   `an_access_service_token_is_kept_here_…`,
//   `a_host_this_project_did_not_bind_…`;
// * `access.read` no longer scrubbing a SaaS `client_secret`, and the scrub no
//   longer descending into arrays —
//   `an_access_application_that_carries_a_client_secret_…`,
//   `a_credential_nested_in_an_array_is_still_taken_out`. The second of those
//   also needed its own test: every secret the double nests today is inside an
//   OBJECT, so array descent was untested until something asserted it;
// * the duration forwarded to Cloudflare unchecked —
//   `a_service_token_duration_cloudflare_would_refuse_…`;
// * an Access application id validated as 32 hex alone, which refuses the UUID
//   half of Cloudflare's own `oneOf` —
//   `an_access_service_token_is_kept_here_…`.

#[test]
fn an_access_service_token_is_kept_here_and_the_caller_gets_a_handle() {
    // §13.10, end to end: "Broker service-token/Tunnel/Access credentials
    // directly into protected storage. The agent receives handles/capabilities,
    // not plaintext secrets."
    //
    // Three things are measured, and the third is the one a build gets wrong:
    // the secret did not come back, the secret IS in the store byte for byte,
    // and it is pinned to the HOST the project named rather than to the API
    // that issued it. A credential stored against `api.cloudflare.com` would
    // look stored and be a pin that says the wrong thing.
    let f = Fixture::new("token", Mode::Normal, &granted_everything());
    let reply = f.use_it(
        f.record("cloudflare.access.service-token.create", "admin.example.com")
            .param("name", "ci"),
    );
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");
    assert!(!output.contains(CLIENT_SECRET), "the secret came back: {output}");
    // The client id is not a secret and is useless without its other half, so
    // it does come back — an agent that could not name the token it just made
    // would have got a capability it cannot use.
    assert!(output.contains(CLIENT_ID), "{output}");
    assert!(output.contains("cf-access.admin.example.com"), "{output}");

    let store = Store::new(f.store.clone());
    let uid = me().uid;
    let info = store
        .info(uid, "cf-access.admin.example.com")
        .expect("the secret was not kept");
    assert_eq!(info.host, "admin.example.com", "pinned to the wrong host");
    assert_eq!(info.scheme, "https");
    assert_eq!(info.username, CLIENT_ID);
    assert_eq!(
        store.value(uid, "cf-access.admin.example.com").expect("value").as_str(),
        Some(CLIENT_SECRET),
        "a different secret was stored"
    );

    // Not in the trail either, which is the file an administrator greps.
    assert!(!f.trail().contains(CLIENT_SECRET), "the secret is in the audit trail");
}

#[test]
fn a_second_service_token_for_the_same_host_is_refused_before_one_is_issued() {
    // Cloudflare shows a `client_secret` once. So a second token for a host
    // whose secret is already stored has to be refused BEFORE the call, not
    // after: a refusal afterwards leaves a token that exists, is in nobody's
    // hands, and cannot be fetched again. The double's `seen()` is what proves
    // the order — nothing was asked.
    let f = Fixture::new("token2", Mode::Normal, &granted_everything());
    let first = f.use_it(
        f.record("cloudflare.access.service-token.create", "admin.example.com")
            .param("name", "ci"),
    );
    assert!(matches!(first, Response::Performed { exit_code: 0, .. }), "{first:?}");
    let calls = f.fake.seen().len();

    let second = f.use_it(
        f.record("cloudflare.access.service-token.create", "admin.example.com")
            .param("name", "ci-again"),
    );
    let (error, message) = second.as_error().expect("a second token must be refused");
    assert_eq!(error, ErrorKind::BadRequest, "{message}");
    assert!(message.contains("cf-access.admin.example.com"), "{message}");
    assert_eq!(f.fake.seen().len(), calls, "a second token was issued anyway");
}

#[test]
fn a_host_this_project_did_not_bind_cannot_have_a_credential_pinned_to_it() {
    // The pin is only worth having if the project had to be allowed to name the
    // host, so this resolves through the same label-boundary check §13.9 uses:
    // `notexample.com` is not inside `example.com`, however much it ends in the
    // same letters. The ERROR KIND is asserted and not merely that something
    // was refused — "you did not bind that" reported as "you may not" is the
    // defect this Cloudflare block has now produced twice.
    let f = Fixture::new("tokenzone", Mode::Normal, &granted_everything());
    for host in ["notexample.com", "admin.example.com.attacker.test", "elsewhere.test"] {
        let reply = f.use_it(
            f.record("cloudflare.access.service-token.create", host).param("name", "ci"),
        );
        let (error, message) = match reply.as_error() {
            Some(pair) => pair,
            None => panic!("'{host}' was accepted as a host in this project's zone"),
        };
        assert_eq!(error, ErrorKind::BadRequest, "{host}: {message}");
        assert!(message.contains("example.com"), "{message}");
    }
    assert!(f.fake.seen().is_empty(), "an unbound host reached the api");
}

#[test]
fn a_service_token_duration_cloudflare_would_refuse_is_refused_before_the_call() {
    // For every other operation, sending a value the far side rejects wastes a
    // request. For this one it can do worse: a request that fails *after*
    // creating the token leaves a secret nobody will ever see. So the duration
    // is checked here, against the syntax the schema documents.
    let f = Fixture::new("duration", Mode::Normal, &granted_everything());
    for bad in ["8760", "h", "2h45", "forever-and-ever", "-5h", "8760H", ""] {
        let reply = f.use_it(
            f.record("cloudflare.access.service-token.create", "admin.example.com")
                .param("name", "ci")
                .param("duration", bad),
        );
        assert!(
            reply.as_error().is_some(),
            "'{}' was accepted as a duration",
            bad.escape_debug()
        );
    }
    assert!(f.fake.seen().is_empty(), "a bad duration reached the api");

    // ...and the shapes the schema documents are accepted, so the check above
    // is not simply refusing everything.
    for good in ["forever", "8760h", "2h45m", "300ms", "30s"] {
        assert!(valid_duration(good), "'{good}' is a duration Cloudflare accepts");
    }
}

#[test]
fn an_access_application_that_carries_a_client_secret_does_not_hand_it_back() {
    // An Access application is not obviously a place a secret lives, and for a
    // self-hosted one it is not. An OIDC SaaS application is: Cloudflare's own
    // schema gives it a `client_secret`, and this is a READ an agent may hold.
    // The double sends one, so this measures a removal rather than an absence.
    let f = Fixture::new("appsecret", Mode::Normal, &["cloudflare.access.read"]);
    let reply = f.use_it(f.record("cloudflare.access.read", "dashboard"));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert!(!output.contains(APP_SECRET), "the saas client secret came back: {output}");
    assert!(output.contains("has been removed"), "{output}");
    // The rest of the application still arrives — a scrub that returned
    // nothing would pass the assertion above and be useless.
    assert!(output.contains("admin.example.com"), "{output}");
    assert!(output.contains(APP_ID), "{output}");
}

#[test]
fn a_tunnel_is_read_without_ever_asking_for_its_connector_token() {
    // The token endpoint is not declared, so no grant can reach it. The double
    // answers that path anyway — a test that passed because there was nothing
    // to fetch would prove nothing — and the assertion is that this build never
    // asks.
    let f = Fixture::new("tunnel", Mode::Normal, &granted_everything());
    let reply = f.use_it(f.record("cloudflare.tunnel.read", "office"));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert!(output.contains("healthy"), "{output}");
    assert!(!output.contains(TUNNEL_TOKEN), "the connector token came back: {output}");
    assert!(
        f.fake.seen().iter().all(|s| !s.path.ends_with("/token")),
        "this build asked for the connector token"
    );
    // And there is no operation that could: the vocabulary is closed by what is
    // declared, so this is a property of the build and not of one code path.
    assert!(
        !SPEC.operations.iter().any(|op| op.id.contains("tunnel.token")),
        "a tunnel token operation was declared"
    );
}

#[test]
fn the_provider_takes_the_token_secret_out_before_the_framework_ever_sees_it() {
    // **The two layers, told apart.** The framework scrubs a created credential
    // out of anything on its way to the caller — proved in `service.rs` against
    // a provider that deliberately leaks one — and this module takes it out as
    // well. With both in place, removing either one changes nothing a test
    // going through `use_capability` can see, so this one goes around the
    // framework and calls `perform` directly. What it measures is the
    // provider's own half: the value leaves here in `created` and nowhere else.
    let f = Fixture::new("twolayer", Mode::Normal, &["cloudflare.access.service-token.create"]);
    let provider = CloudflareProvider::at(f.fake.port);
    let owner = crate::broker::owner(me().uid).expect("own uid");
    let service = apex_secret_core::store::ServiceInfo {
        service: "cloudflare".into(),
        host: "127.0.0.1".into(),
        scheme: "http".into(),
        username: "x-access-token".into(),
        path: String::new(),
        auth: "bearer".into(),
        port: Some(f.fake.port),
        added: 0,
    };
    let operation = SPEC
        .operations
        .iter()
        .find(|op| op.id == "cloudflare.access.service-token.create")
        .expect("declared");
    let mut params = operation::Params::new();
    params.insert("name".into(), "ci".into());
    let project = f.project.to_string_lossy().into_owned();
    let req = Bind {
        operation,
        resource: "admin.example.com",
        params: &params,
        body: &[],
        project: &project,
        service: &service,
        owner: &owner,
        audit_id: "test-audit-id",
    };
    let bound = provider.bind(&req).expect("binds");
    assert_eq!(
        bound.creates.as_deref(),
        Some("cf-access.admin.example.com"),
        "the name has to be declared BEFORE the call, or a collision cannot be refused in time"
    );
    let performed = provider
        .perform(&req, &bound, &SecretValue::new(TOKEN.as_bytes().to_vec()))
        .expect("performs");
    assert!(
        !performed.output.contains(CLIENT_SECRET),
        "the provider handed the secret to the framework in its output: {}",
        performed.output
    );
    let created = performed.created.expect("the secret must leave here as a credential");
    assert_eq!(created.value.as_str(), Some(CLIENT_SECRET));
    assert_eq!(created.host, "admin.example.com");
    assert_eq!(created.username.as_deref(), Some(CLIENT_ID));
}

#[test]
fn a_credential_nested_in_an_array_is_still_taken_out() {
    // The scrub descends into objects AND arrays. Both matter and neither is
    // hypothetical: a Hyperdrive password is at `result.origin.password`, an
    // Access SaaS secret at `result.saas_app.client_secret`, and an AI
    // Gateway's OTel exporter carries an `authorization` inside a LIST. A
    // top-level scrub, or one that walked objects only, would report success
    // having removed nothing — which is the failure that looks exactly like
    // success.
    let body = r#"{"result":{"top":"kept","inner":{"password":"p1"},"list":[{"password":"p2"},{"keep":"yes"}]}}"#;
    let out = without(body, &["password"], "\nremoved");
    assert!(!out.contains("p1"), "an object-nested secret survived: {out}");
    assert!(!out.contains("p2"), "an array-nested secret survived: {out}");
    assert!(out.contains("kept") && out.contains("yes"), "it removed too much: {out}");
    assert!(out.ends_with("removed"), "it removed something and did not say so: {out}");

    // A reply with nothing to remove is returned unchanged and says nothing.
    let clean = r#"{"result":{"top":"kept"}}"#;
    assert!(!without(clean, &["password"], "\nremoved").contains("removed"));
    // ...and something that is not the envelope at all comes back as it is,
    // rather than becoming the string "null".
    assert_eq!(without("not json", &["password"], "\nremoved"), "not json");
}

#[test]
fn the_cloudflare_one_verbs_are_separate_grants() {
    // §13.2's whole argument, on the surface where it matters most: reading an
    // Access application, revoking its sessions and issuing a credential are
    // three decisions, and an owner gets to make them one at a time.
    let f = Fixture::new("onegrants", Mode::Normal, &["cloudflare.access.read"]);
    let allowed = f.use_it(f.record("cloudflare.access.read", "dashboard"));
    assert!(matches!(allowed, Response::Performed { exit_code: 0, .. }), "{allowed:?}");

    for (operation, resource, option) in [
        ("cloudflare.access.edit", "dashboard", None),
        (
            "cloudflare.access.service-token.create",
            "admin.example.com",
            Some(("name", "ci")),
        ),
        ("cloudflare.tunnel.read", "office", None),
        ("cloudflare.tunnel.edit", "office", Some(("name", "renamed"))),
    ] {
        let mut rec = f.record(operation, resource);
        if let Some((name, value)) = option {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        let (error, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("'{operation}' ran under a grant for access.read"));
        assert_eq!(error, ErrorKind::PermissionDenied, "{operation}: {message}");
    }
}

/// §13.2's list, so the test above compares against the roadmap rather than
/// against itself.
const SECTION_13_2: &[&str] = &[
    "cloudflare.account.read",
    "cloudflare.worker.read",
    "cloudflare.worker.upload-version",
    "cloudflare.worker.deploy",
    "cloudflare.worker.rollback",
    "cloudflare.worker.tail",
    "cloudflare.dns.read",
    "cloudflare.dns.create",
    "cloudflare.dns.update",
    "cloudflare.dns.delete",
    "cloudflare.r2.object.read",
    "cloudflare.r2.object.write",
    "cloudflare.r2.bucket.create",
    "cloudflare.d1.read",
    "cloudflare.d1.query",
    "cloudflare.d1.migrate",
    "cloudflare.kv.read",
    "cloudflare.kv.write",
    "cloudflare.queue.publish",
    "cloudflare.queue.manage",
    "cloudflare.hyperdrive.read",
    "cloudflare.hyperdrive.edit",
    "cloudflare.secret.create",
    "cloudflare.secret.bind",
    "cloudflare.secret.rotate",
    "cloudflare.access.read",
    "cloudflare.access.edit",
    "cloudflare.tunnel.read",
    "cloudflare.tunnel.edit",
    "cloudflare.workers-ai.run",
    "cloudflare.ai-gateway.run",
    "cloudflare.ai-gateway.edit",
];

/// A worker bound to two environments must resolve the same way twice.
#[test]
fn a_worker_bound_twice_resolves_the_same_way_every_time() {
    let f = Fixture::new("stable", Mode::Normal, &["cloudflare.worker.read"]);
    std::fs::write(
        f.project.join("apex.toml"),
        format!(
            "[identity.cloudflare]\naccount_id = \"{ACCOUNT}\"\n\
             [cloudflare.alpha]\nworker = \"twin\"\n\
             [cloudflare.beta]\nworker = \"twin\"\n"
        ),
    )
    .expect("write");
    let mut details = std::collections::BTreeSet::new();
    for _ in 0..5 {
        let lines = {
            f.use_it(f.record("cloudflare.worker.read", "twin"));
            audit::tail(&Store::new(f.store.clone()).audit_path(), 50)
        };
        let used = lines.iter().rev().find(|l| l.event == AuditEvent::Used).expect("used");
        details.insert(used.detail.clone());
    }
    assert_eq!(details.len(), 1, "resolution is not stable: {details:?}");
    assert!(details.iter().next().unwrap().contains("(alpha)"));
}

/// Nothing above depends on this map, but a request that carried one would.
#[test]
fn the_options_a_caller_sends_survive_the_wire_unchanged() {
    let mut params: BTreeMap<String, String> = BTreeMap::new();
    params.insert("version".into(), "1c4dd6be".into());
    let rec = CapabilityRecord::new("cloudflare", "cloudflare.worker.deploy", "project")
        .param("version", "1c4dd6be");
    assert_eq!(rec.params, params);
}

#[test]
fn a_cloudflare_operation_cannot_be_granted_in_every_project_even_though_it_names_nothing() {
    // The collision between P1-002 and P1-018, as a runtime assertion rather
    // than a declaration one.
    //
    // `--everywhere` (the `*` grant key) is gated on the operation reaching the
    // same thing in every project. P1-018 implemented that as
    // `OperationSpec::names_nothing`, which is true here: `cloudflare.account.read`
    // declares no resource and no parameters. But it resolves the account out of
    // the project's own `apex.toml` — bound, `GET /accounts/{id}` for THAT
    // project; unbound, `GET /accounts` for every account the token can see — so
    // it reaches a different thing in a different directory, and a `*` grant
    // would let an agent in a project the owner never approved read that
    // project's account with the one stored token.
    //
    // Two mutations prove this bites, and they fail it for different reasons:
    // make `may_be_granted_everywhere` compute `op.names_nothing()` again
    // instead of reading the declaration, or flip this operation's own
    // `same_everywhere` to true. Both were run, and both turn this red.
    let tag = format!("everywhere-{}", std::process::id());
    let store = std::env::temp_dir().join(format!("apex-cf-store-{tag}"));
    let project = std::env::temp_dir().join(format!("apex-cf-project-{tag}"));
    std::fs::remove_dir_all(&store).ok();
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(project.join("apex.toml"), PROJECT_FILE).expect("apex.toml");

    let mut registry = Registry::new();
    registry
        .register(Box::new(CloudflareProvider::new()))
        .expect("register");
    let service = Service::new(Store::new(store.clone()), false, registry);
    let peer = me();
    assert_eq!(
        service.add(
            peer,
            NewService {
                service: "cloudflare",
                host: "127.0.0.1",
                scheme: "http",
                username: None,
                path: "",
                auth: None,
                port: None,
            },
            SecretValue::new(TOKEN.as_bytes().to_vec()),
        ),
        Response::Ok
    );

    // Refused everywhere. `*` IS the everywhere key; the trailing bool is
    // `revoke`, not `everywhere`.
    let reply = service.grant(
        peer,
        apex_secret_core::store::ANY_PROJECT,
        "cloudflare",
        "cloudflare.account.read",
        false,
    );
    let (_, message) = reply
        .as_error()
        .expect("`*` must be refused for an operation that resolves per project");
    assert!(
        message.contains("resolves against the project"),
        "the refusal has to say why: {message}"
    );

    // ...and allowed in the project the owner named, which is the whole point
    // of refusing the other one rather than refusing both.
    let named = service.grant(
        peer,
        project.to_str().expect("utf8"),
        "cloudflare",
        "cloudflare.account.read",
        false,
    );
    assert!(
        named.as_error().is_none(),
        "the named project must still be grantable: {named:?}"
    );
    match named {
        Response::Grants { projects } => assert!(
            projects
                .get(project.to_str().expect("utf8"))
                .is_some_and(|caps| caps.iter().any(|c| c.contains("cloudflare.account.read"))),
            "{projects:?}"
        ),
        other => panic!("a grant answers with the grants: {other:?}"),
    }

    std::fs::remove_dir_all(&store).ok();
    std::fs::remove_dir_all(&project).ok();
}

// ---------------------------------------------------------------------------
// §13.4 — temporary task credentials (P1-011)
// ---------------------------------------------------------------------------

/// P1-005's second acceptance criterion, which its own evidence left open:
/// *"No broad token when narrower scope is possible."*
///
/// Half of it was already true — the agent never holds a token, and every
/// operation is scoped to a resource the project's file names — and the other
/// half was not: the token actually spent was the account-wide one the owner
/// stored. This is the half that was missing, and it is one assertion: the
/// request that did the work carried a credential that did not exist when the
/// request arrived and does not exist by the time it is answered.
///
/// Three mutations, all red:
/// * present the stored value instead of the minted one in `service.rs`
///   (`Minted::Narrowed { value, .. } => (&stored, None)`);
/// * drop the revoke on the success path;
/// * make `temporary::mint` return the stored token rather than `result.value`.
#[test]
fn an_operation_spends_a_credential_that_did_not_exist_a_moment_ago_and_does_not_outlive_it() {
    let f = Fixture::new("mint-r2", Mode::Minting, &["cloudflare.r2.object.read"]);
    let reply = f.use_it(f.record("cloudflare.r2.object.read", "example-assets/backups/latest.sql"));
    assert!(reply.as_error().is_none(), "{reply:?}");

    // What the operation itself did, and with what.
    let operation = f.fake.seen();
    assert_eq!(operation.len(), 1, "{operation:#?}");
    assert_eq!(
        operation[0].authorization.as_deref(),
        Some(format!("Bearer {MINTED}").as_str()),
        "the operation spent the stored account-wide token, not the narrow one"
    );
    assert_ne!(
        operation[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );

    // What narrowing it cost, and with what. The stored credential is spent on
    // exactly these three and nowhere else.
    let exchange = f.fake.minting();
    assert_eq!(exchange.len(), 3, "{exchange:#?}");
    let stored = Some(format!("Bearer {TOKEN}"));
    assert_eq!(
        exchange
            .iter()
            .map(|s| (s.method.as_str(), s.authorization.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("GET", stored.clone()),
            ("POST", stored.clone()),
            ("DELETE", stored),
        ],
        "the exchange must be asked for with the credential that can make it, \
         and only that one"
    );
    assert!(
        exchange[0].path.ends_with("/tokens/permission_groups"),
        "{}",
        exchange[0].path
    );
    assert!(
        exchange[2].path.ends_with(&format!("/tokens/{MINTED_ID}")),
        "the token that was spent was not the token that was revoked: {}",
        exchange[2].path
    );

    // And the policy it was created under is one product at one scope.
    let body: serde_json::Value =
        serde_json::from_str(&exchange[1].body).expect("the creation body is json");
    let policies = body["policies"].as_array().expect("policies");
    assert_eq!(policies.len(), 1, "{body}");
    assert_eq!(policies[0]["effect"], "allow");
    assert_eq!(
        policies[0]["permission_groups"]
            .as_array()
            .expect("groups")
            .len(),
        1
    );
    assert_eq!(
        policies[0]["resources"],
        serde_json::json!({ format!("com.cloudflare.api.account.{ACCOUNT}"): "*" })
    );
    assert!(
        body["expires_on"].as_str().is_some_and(|e| e.ends_with('Z')),
        "a token with no expiry is not a short-lived one: {body}"
    );
}

/// The arm every implementation forgets, and the one that matters most.
///
/// A minted credential that outlives a *failed* operation is exactly the
/// credential §13.4 says not to leave lying around — and a failure is often
/// the case where something went wrong enough to be worth not leaving one. So
/// the revoke happens before the error is even looked at.
///
/// Mutation: move the revoke inside the `Ok(out)` arm in `service.rs`. Red.
#[test]
fn a_short_lived_credential_is_revoked_even_when_the_operation_it_was_minted_for_failed() {
    let f = Fixture::new("mint-fail", Mode::Minting, &["cloudflare.dns.delete"]);
    // A name the project's zone holds no record for: the lookup runs with the
    // minted credential, answers an absence, and `perform` returns an error —
    // which is the path the revoke is easiest to leave off.
    let reply = f.use_it(
        f.record("cloudflare.dns.delete", "gone.example.com")
            .param("type", "A"),
    );
    assert!(
        reply.as_error().is_some(),
        "this test needs an operation that failed AFTER the credential was \
         presented: {reply:?}"
    );
    // It really did reach the far side with the minted credential first.
    assert_eq!(
        f.fake.seen()[0].authorization.as_deref(),
        Some(format!("Bearer {MINTED}").as_str()),
        "{:#?}",
        f.fake.seen()
    );

    let exchange = f.fake.minting();
    assert!(
        exchange.iter().any(|s| s.method == "DELETE"
            && s.path.ends_with(&format!("/tokens/{MINTED_ID}"))),
        "the operation failed and the credential minted for it was left \
         standing: {exchange:#?}"
    );
}

/// "Permission denied is not absence", in the one place a credential service
/// is most tempted to collapse the two.
///
/// An account whose stored credential may not create tokens has said nothing
/// about whether a narrower token is possible — someone else's credential on
/// the same account could make one. An account this build cannot describe a
/// policy for has said nothing either. Neither is *"there is no narrower
/// form"*, which is a conclusion, and the trail has to be able to tell an
/// operator which of the three happened so they can act on it.
///
/// Mutation: collapse `Denied` into `NoNarrowerForm` in `Minted::as_str`, or
/// return `NoNarrowerForm` from `temporary::group_ids`' 403 arm. Red.
#[test]
fn a_credential_that_may_not_narrow_itself_is_not_a_credential_with_nothing_to_narrow_to() {
    // Cloudflare gates reading the permission groups and creating a token on
    // the same permission, so a credential that lacks it is turned away at one
    // of two places. Both are `Denied`, and both are measured: a build that
    // got one right would look correct until an account refused at the other.
    let mut refusals = Vec::new();
    for (name, mode) in [
        ("denied-create", Mode::MintingDenied),
        ("denied-list", Mode::ListDenied),
    ] {
        let refused = Fixture::new(name, mode, &["cloudflare.worker.read"]);
        assert!(refused
            .use_it(refused.record("cloudflare.worker.read", "project"))
            .as_error()
            .is_none());
        refusals.push(refused.trail());
    }
    let refused_trail = refusals[0].clone();

    let silent = Fixture::new("no-tokens", Mode::Normal, &["cloudflare.worker.read"]);
    assert!(silent
        .use_it(silent.record("cloudflare.worker.read", "project"))
        .as_error()
        .is_none());
    let silent_trail = silent.trail();

    let word = |trail: &str| -> String {
        let line: serde_json::Value = serde_json::from_str(
            trail
                .lines()
                .rfind(|l| l.contains("\"used\""))
                .expect("a used line"),
        )
        .expect("json");
        line["narrowing"].as_str().expect("narrowing").to_string()
    };

    // The account refused, at either place. Not an absence.
    for trail in &refusals {
        assert_eq!(word(trail), "denied", "{trail}");
    }
    // The account could not be asked. Not an absence and not a refusal.
    assert_eq!(word(&silent_trail), "could-not-run", "{silent_trail}");
    assert_ne!(word(&refused_trail), word(&silent_trail));

    // Both still ran, on the stored credential. §13.4 says *prefer*, and
    // refusing an operation because its credential could not be narrowed would
    // break every operation for an owner whose token is not a Super
    // Administrator's — which Cloudflare documents as the requirement for
    // creating an account-owned token, and is therefore the ordinary case.
    for trail in refusals.iter().chain(std::iter::once(&silent_trail)) {
        assert!(trail.contains("\"used\""), "the operation did not run");
    }
}

/// A name this build does not recognise is this build not recognising it.
///
/// The fixture's account offers no `Hyperdrive Read` group. That is not the
/// account refusing and it is not this operation having no narrow form — it is
/// a policy this build could not assemble, and the only honest answer is that
/// it could not run.
///
/// Mutation: answer `NoNarrowerForm` from the missing-group arm. Red.
#[test]
fn a_permission_group_this_build_cannot_find_is_not_a_refusal_and_not_an_absence() {
    let f = Fixture::new("no-group", Mode::Minting, &["cloudflare.hyperdrive.read"]);
    let reply = f.use_it(f.record("cloudflare.hyperdrive.read", "pg"));
    assert!(reply.as_error().is_none(), "{reply:?}");

    // It asked, and then stopped: no token was created against a policy it
    // could not describe.
    let exchange = f.fake.minting();
    assert_eq!(exchange.len(), 1, "{exchange:#?}");
    assert_eq!(exchange[0].method, "GET");

    let trail = f.trail();
    let line: serde_json::Value = serde_json::from_str(
        trail
            .lines()
            .rfind(|l| l.contains("\"used\""))
            .expect("a used line"),
    )
    .expect("json");
    assert_eq!(line["narrowing"], "could-not-run", "{trail}");
    assert!(
        line["narrowing_detail"]
            .as_str()
            .expect("a reason")
            .contains("Hyperdrive Read"),
        "the reason must name what it looked for: {line}"
    );
    // And the operation ran anyway, on the stored credential.
    assert_eq!(
        f.fake.seen()[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

/// DNS is the one surface where the narrowing reaches past the product to the
/// resource: DNS permission groups are zone-scoped, so a token minted for a
/// DNS operation names the one zone this project bound and no other.
///
/// Mutation: make every policy account-scoped by dropping `zone_scoped` from
/// the DNS rows in `POLICY`. Red.
#[test]
fn a_token_minted_for_dns_names_the_one_zone_this_project_bound() {
    let f = Fixture::new("mint-dns", Mode::Minting, &["cloudflare.dns.read"]);
    assert!(f
        .use_it(f.record("cloudflare.dns.read", "example.com"))
        .as_error()
        .is_none());

    let exchange = f.fake.minting();
    let creation = exchange
        .iter()
        .find(|s| s.method == "POST")
        .expect("a token was created");
    let body: serde_json::Value = serde_json::from_str(&creation.body).expect("json");
    assert_eq!(
        body["policies"][0]["resources"],
        serde_json::json!({ format!("com.cloudflare.api.account.zone.{ZONE}"): "*" }),
        "a DNS token scoped to the whole account is broader than it needs to \
         be, and §13.9 is about exactly that: {body}"
    );
}

/// An operation with no row in [`super::temporary::POLICY`] goes on spending
/// the account-wide token, silently. So adding an operation and forgetting the
/// row has to fail here rather than in production.
#[test]
fn every_declared_operation_can_name_the_narrowest_token_that_carries_it() {
    let missing: Vec<&str> = SPEC
        .operations
        .iter()
        .map(|op| op.id)
        .filter(|id| super::temporary::policy_for(id).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "these operations have no §13.4 policy, so each of them would keep \
         spending the stored account-wide credential without anything saying \
         so: {missing:?}"
    );
    // …and nothing in the table names an operation that does not exist, which
    // is how a row survives a rename.
    let unknown: Vec<&str> = super::temporary::POLICY
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !SPEC.operations.iter().any(|op| op.id == *id))
        .collect();
    assert!(unknown.is_empty(), "policy rows for nothing: {unknown:?}");
}

/// A minted token is still a credential. §13.4 says so in as many words, and
/// the framework scrubs it for the same reason it scrubs the stored one.
#[test]
fn a_minted_credential_reaches_neither_the_caller_nor_the_trail() {
    let f = Fixture::new("mint-scrub", Mode::Minting, &["cloudflare.worker.read"]);
    let reply = f.use_it(f.record("cloudflare.worker.read", "project"));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert!(!output.contains(MINTED), "the minted token came back: {output}");
    assert!(!output.contains(TOKEN), "the stored token came back: {output}");
    let trail = f.trail();
    assert!(!trail.contains(MINTED), "the trail holds the minted credential");
    assert!(!trail.contains(TOKEN), "the trail holds the stored credential");
    // The handle is not a credential and is the one thing worth keeping: a
    // token that outlived its operation has to be traceable to the line that
    // says so.
    assert!(
        !trail.contains(&format!("Bearer {MINTED}")),
        "{trail}"
    );
}

/// A revoke that did not happen leaves exactly the credential the revoke
/// exists to remove, and an expiry is a backstop rather than a revocation. So
/// it is recorded, and the operation still succeeds — it already happened.
///
/// Mutation: drop the `Err` arm of the revoke in `service.rs`. Red.
#[test]
fn a_revoke_that_could_not_be_done_is_recorded_rather_than_dropped() {
    let f = Fixture::new("revoke-fails", Mode::RevokeFails, &["cloudflare.worker.read"]);
    let reply = f.use_it(f.record("cloudflare.worker.read", "project"));
    assert!(
        reply.as_error().is_none(),
        "a revoke that failed must not fail the operation that already ran: {reply:?}"
    );
    let trail = f.trail();
    let line: serde_json::Value = serde_json::from_str(
        trail
            .lines()
            .rfind(|l| l.contains("\"used\""))
            .expect("a used line"),
    )
    .expect("json");
    assert_eq!(line["narrowing"], "narrowed", "{trail}");
    let detail = line["narrowing_detail"].as_str().expect("a reason");
    assert!(
        detail.contains("could not be") && detail.contains("revoked"),
        "the trail does not say the credential is still standing: {detail}"
    );
}

/// The four answers have to stay four. Collapsing any two of them is the
/// defect the enum exists to prevent, and it would be an easy edit.
#[test]
fn the_four_answers_to_a_narrowing_request_are_four_different_words() {
    use crate::provider::Minted;
    let words = [
        Minted::NoNarrowerForm(String::new()).as_str(),
        Minted::Denied(String::new()).as_str(),
        Minted::CouldNotRun(String::new()).as_str(),
        "narrowed",
    ];
    let unique: std::collections::BTreeSet<&str> = words.iter().copied().collect();
    assert_eq!(unique.len(), 4, "{words:?}");
    // And none of them is the word an older line, or a line that never got
    // that far, deserializes to.
    assert!(!unique.contains("unknown"));
    assert!(!unique.contains(apex_secret_core::audit::NOT_ATTEMPTED));
}

/// An expiry is a fixed-width instant in UTC, and a clock that cannot produce
/// one must not produce a nonsense one.
#[test]
fn an_expiry_is_written_the_way_the_schema_asks_for_it() {
    use super::temporary::rfc3339;
    assert_eq!(rfc3339(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    assert_eq!(rfc3339(1_000_000_000).as_deref(), Some("2001-09-09T01:46:40Z"));
    // A leap day, because the civil conversion is where that goes wrong.
    assert_eq!(rfc3339(1_709_164_800).as_deref(), Some("2024-02-29T00:00:00Z"));
    assert_eq!(rfc3339(253_402_300_799).as_deref(), Some("9999-12-31T23:59:59Z"));
    // Past the end of the fixed-width format. Not a time this build will write.
    assert_eq!(rfc3339(253_402_300_800), None);
}

/// [`PROJECT_FILE`] with §13.4's strength set, spliced into the `[cloudflare]`
/// table it already has rather than appended — a second `[cloudflare]` header
/// is a duplicate table and TOML refuses the whole file, which would make
/// every test below measure a parse error instead of the setting.
fn project_with_narrowing(word: &str) -> String {
    PROJECT_FILE.replacen(
        "[cloudflare]\n",
        &format!("[cloudflare]\ntemporary_credentials = \"{word}\"\n"),
        1,
    )
}

/// §13.4's fallback is the ordinary case, not a rare one: cloudflare requires
/// Super Administrator on the account to create an account-owned token, so on
/// most accounts the exchange is refused and the stored credential is what
/// gets spent. `prefer` is right for that, and it is the default.
///
/// An owner whose credential *can* mint needs a way to say "and if it ever
/// stops, stop too" — otherwise the day the token is downgraded is the day
/// every operation quietly goes back to spending the broad one, with nothing
/// but a word in the trail to say so.
///
/// Mutations: make `require` fall through to the answer (drop the `Err`), or
/// read an unknown word as `prefer`. Both red.
#[test]
fn a_project_that_requires_a_short_lived_credential_does_not_run_on_the_stored_one() {
    let required = project_with_narrowing("require");
    // The account refuses to issue one…
    let f = Fixture::with_project(
        "require",
        Mode::MintingDenied,
        &["cloudflare.worker.read"],
        &required,
    );
    let reply = f.use_it(f.record("cloudflare.worker.read", "project"));
    let (kind, message) = reply
        .as_error()
        .expect("a project that requires a narrowed credential must not use the broad one");
    assert_eq!(kind, ErrorKind::PermissionDenied);
    assert!(message.contains("require"), "{message}");
    // …and nothing was carried out with the stored credential.
    assert!(
        f.fake.seen().is_empty(),
        "the operation ran anyway: {:#?}",
        f.fake.seen()
    );

    // The same project, on an account that can mint, runs.
    let ok = Fixture::with_project(
        "require-ok",
        Mode::Minting,
        &["cloudflare.worker.read"],
        &required,
    );
    let reply = ok.use_it(ok.record("cloudflare.worker.read", "project"));
    assert!(reply.as_error().is_none(), "{reply:?}");
    assert_eq!(
        ok.fake.seen()[0].authorization.as_deref(),
        Some(format!("Bearer {MINTED}").as_str())
    );
}

/// `off` is a project saying it does not want the two extra requests. It is
/// not the project saying there is no narrower credential — but that is what
/// the trail would say if this returned the same word as a provider with
/// nothing to offer, so the reason names the file.
#[test]
fn a_project_that_turns_narrowing_off_says_so_rather_than_looking_like_it_has_none() {
    let file = project_with_narrowing("off");
    let f = Fixture::with_project("off", Mode::Minting, &["cloudflare.worker.read"], &file);
    assert!(f
        .use_it(f.record("cloudflare.worker.read", "project"))
        .as_error()
        .is_none());
    // It did not ask.
    assert!(f.fake.minting().is_empty(), "{:#?}", f.fake.minting());
    // And it ran on the stored credential.
    assert_eq!(
        f.fake.seen()[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    let trail = f.trail();
    let line: serde_json::Value = serde_json::from_str(
        trail.lines().rfind(|l| l.contains("\"used\"")).expect("a used line"),
    )
    .expect("json");
    assert_eq!(line["narrowing"], "no-narrower-form");
    assert!(
        line["narrowing_detail"].as_str().expect("a reason").contains("off"),
        "the reason must name the setting and not look like an absence: {line}"
    );
}

/// A word this build does not know is a file the owner meant something by.
/// Reading it as the default would give them the weaker of the two things
/// they might have meant, silently.
#[test]
fn a_narrowing_setting_this_build_does_not_know_is_refused_rather_than_defaulted() {
    let file = project_with_narrowing("required");
    let f = Fixture::with_project("badword", Mode::Minting, &["cloudflare.worker.read"], &file);
    let reply = f.use_it(f.record("cloudflare.worker.read", "project"));
    let (_, message) = reply.as_error().expect("an unknown setting must be refused");
    assert!(message.contains("temporary_credentials"), "{message}");
    assert!(f.fake.seen().is_empty() && f.fake.minting().is_empty());
}

// ---------------------------------------------------------------------------
// §13.4's tool half — brokered wrangler and terraform (P1-012)
// ---------------------------------------------------------------------------

/// The line of the stub's output that starts with `key=`.
fn stub_line<'a>(output: &'a str, key: &str) -> &'a str {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("the stub printed no '{key}' line: {output}"))
}

/// P1-012's second criterion and §13.4's sentence about it: *"Do not pass even
/// the temporary token directly to the agent if a broker-owned `wrangler`
/// child process can perform the operation."*
///
/// The tool runs believing it has credentials, because it does. The agent
/// never had them: they were put in the child's environment by a process the
/// agent cannot read, and the credential that went in is P1-011's — one that
/// expires in minutes and is deleted the moment this returns.
///
/// Mutations: put the token in argv instead of the environment; present the
/// stored token rather than the minted one. Both red.
#[test]
fn a_brokered_tool_is_given_the_credential_in_its_environment_and_never_in_its_argv() {
    let f = Fixture::new("wrangler", Mode::Minting, &["cloudflare.wrangler.deploy"]);
    let reply = f.use_it(f.record("cloudflare.wrangler.deploy", "project"));
    let Response::Performed { output, exit_code, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*exit_code, 0, "{output}");

    // The tool was given a credential…
    assert_eq!(
        stub_line(output, "token"),
        "«redacted»",
        "the tool was run without a credential, or with one that was not \
         scrubbed on the way back: {output}"
    );
    // …and it was the short-lived one, not the stored one. The stub echoes
    // whatever it was given and the framework scrubs both, so the way to tell
    // them apart is the far side: the mint happened, and the stored token was
    // spent only on the exchange.
    let exchange = f.fake.minting();
    assert_eq!(exchange.len(), 3, "{exchange:#?}");
    assert!(exchange.iter().all(|s| s.authorization.as_deref()
        == Some(format!("Bearer {TOKEN}").as_str())));

    // Not in argv. `/proc/<pid>/cmdline` is world-readable and an environment
    // is not, which is the whole reason this is an environment variable.
    let argv = output.lines().next().expect("the stub prints its argv first");
    assert!(!argv.contains(TOKEN) && !argv.contains(MINTED), "{argv}");
    assert!(!argv.contains("«redacted»"), "a credential was on the command line: {argv}");

    // Not in the reply, not in the trail.
    assert!(!output.contains(TOKEN) && !output.contains(MINTED), "{output}");
    let trail = f.trail();
    assert!(!trail.contains(TOKEN) && !trail.contains(MINTED), "{trail}");
}

/// The environment is built here, not inherited.
///
/// The daemon's own environment is root's. Passing it through would hand the
/// child root's `HOME`, whatever systemd set, and any `CLOUDFLARE_*` variable
/// that happened to be in it — which would make the credential the broker put
/// there the second-most-interesting one in the room.
///
/// Mutation: delete `env_clear()` from `broker::run_tool`. Red.
#[test]
fn a_brokered_tool_gets_the_environment_this_daemon_built_and_not_the_one_it_has() {
    // A test that measures the absence of something that was never there
    // measures nothing, so the thing has to be there first.
    assert!(
        std::env::var("CARGO_PKG_NAME").is_ok(),
        "this test needs a variable in its OWN environment to watch for in the \
         child's; without one it would pass whatever the broker did"
    );
    let f = Fixture::new("env", Mode::Normal, &["cloudflare.terraform.plan"]);
    let reply = f.use_it(f.record("cloudflare.terraform.plan", ""));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(
        stub_line(output, "inherited"),
        "<unset>",
        "the child inherited this process's environment: {output}"
    );
    // And it got the owner's home rather than the daemon's.
    assert_ne!(stub_line(output, "home"), "<unset>", "{output}");
}

/// The argv is this build's and the working directory is the caller's project.
///
/// `--env` is the one thing a caller contributes to a brokered command line,
/// and it did not come from the caller: `resolve` took it out of the project's
/// own `apex.toml`, which is why a worker the project did not bind cannot put
/// anything there.
///
/// Mutations: drop `--env`; run in the daemon's directory instead of the
/// project's. Both red.
#[test]
fn a_brokered_tool_runs_the_argv_this_build_wrote_in_the_project_that_asked() {
    let f = Fixture::new("argv", Mode::Normal, &granted_everything());

    let reply = f.use_it(f.record("cloudflare.wrangler.versions-upload", "project"));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    let argv = output.lines().next().expect("argv");
    assert_eq!(
        argv, "apex-stub: wrangler versions upload --env production",
        "{output}"
    );
    assert_eq!(
        stub_line(output, "cwd"),
        f.project.to_string_lossy(),
        "the tool ran somewhere other than the project that asked: {output}"
    );
    assert_eq!(stub_line(output, "account"), ACCOUNT, "{output}");

    // Terraform takes no environment and must not be given one.
    let reply = f.use_it(f.record("cloudflare.terraform.apply", ""));
    let Response::Performed { output, .. } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(
        output.lines().next().expect("argv"),
        "apex-stub: terraform apply -input=false -no-color -auto-approve",
        "{output}"
    );
}

/// §13.1 applies to a tool exactly as it applies to a request: a name the
/// project did not bind does not resolve, and nothing runs.
#[test]
fn a_worker_this_project_did_not_bind_never_reaches_wrangler() {
    let f = Fixture::new("unbound", Mode::Normal, &["cloudflare.wrangler.deploy"]);
    let reply = f.use_it(f.record("cloudflare.wrangler.deploy", "somebody-elses-worker"));
    let (kind, message) = reply.as_error().expect("an unbound name must be refused");
    assert_eq!(kind, ErrorKind::BadRequest, "{message}");
    assert!(message.contains("does not bind"), "{message}");
}

/// A tool that is not installed is not a permission this credential lacks.
///
/// The same distinction the rest of this provider keeps, in the place it is
/// easiest to lose: both end in "the operation did not happen", and only one
/// of them is fixed by installing something.
#[test]
fn a_tool_that_is_not_installed_is_not_a_permission_this_credential_lacks() {
    let f = Fixture::new("missing", Mode::Normal, &["cloudflare.wrangler.deploy"]);
    std::fs::remove_file(f.tools.join("wrangler")).expect("remove the stub");
    let reply = f.use_it(f.record("cloudflare.wrangler.deploy", "project"));
    let (kind, message) = reply.as_error().expect("it cannot have run");
    assert_eq!(
        kind,
        ErrorKind::Internal,
        "a missing tool was reported as a permission problem: {message}"
    );
    assert!(message.contains("not installed"), "{message}");
    assert!(
        message.contains("not a permission"),
        "the message does not say which of the two this is: {message}"
    );
}

/// No brokered subcommand takes a parameter, and that is structural rather
/// than incidental.
///
/// A parameter is the only thing a caller can send that this provider composes
/// into what it runs. An operation called `cloudflare.wrangler.run` taking the
/// caller's own argv would be a grant to do everything wrangler can do — which
/// is the thing the other thirty-four names exist not to be — and the way that
/// creeps in is one `params` entry at a time.
#[test]
fn nothing_a_caller_sends_can_reach_a_brokered_command_line() {
    for op in SPEC.operations {
        let Some(brokered) = super::tools::brokered(op.id) else {
            continue;
        };
        assert!(
            op.params.is_empty(),
            "'{}' takes a parameter, and a parameter is the one thing a caller \
             contributes to what runs",
            op.id
        );
        // And every word of the argv is a literal in this build.
        assert!(
            !brokered.args.is_empty()
                && brokered.args.iter().all(|a| !a.is_empty() && !a.contains(' ')),
            "'{}' has an argv this build did not write plainly",
            op.id
        );
    }
}

/// Terraform is the honest `Nothing`: what permissions a plan needs is decided
/// by the project's own `.tf` files, which this build does not read.
///
/// A token minted broad *because we could not tell* would be worse than the
/// stored one — it would read as narrowing in the trail while granting the
/// same reach. So it says there is nothing narrower, and the trail says that
/// rather than `denied` or `could-not-run`.
///
/// Mutation: give terraform a `Narrowest::Token` row. Red.
#[test]
fn terraform_says_there_is_nothing_narrower_rather_than_minting_a_token_it_cannot_describe() {
    let f = Fixture::new("tf-mint", Mode::Minting, &["cloudflare.terraform.plan"]);
    assert!(f
        .use_it(f.record("cloudflare.terraform.plan", ""))
        .as_error()
        .is_none());
    // It did not ask for one.
    assert!(f.fake.minting().is_empty(), "{:#?}", f.fake.minting());
    let trail = f.trail();
    let line: serde_json::Value = serde_json::from_str(
        trail.lines().rfind(|l| l.contains("\"used\"")).expect("a used line"),
    )
    .expect("json");
    assert_eq!(line["narrowing"], "no-narrower-form", "{trail}");
    assert!(
        line["narrowing_detail"].as_str().expect("a reason").contains(".tf files"),
        "the reason must say why there is nothing narrower: {line}"
    );
}

// ---------------------------------------------------------------------------
// §13.7 — the transactional deployment flow (P1-013)
// ---------------------------------------------------------------------------

/// §13.7's first line: *upload Worker version -> preview -> health check ->
/// staged traffic -> full deployment*. The first arrow is the one that makes
/// the rest possible, and it is a property of the vocabulary rather than of a
/// flow: uploading a version must not put it in front of anything.
///
/// Mutation: point `worker.upload-version` at `/deployments`. Red.
#[test]
fn uploading_a_version_puts_it_in_front_of_nothing() {
    let f = Fixture::new("upload", Mode::Normal, &["cloudflare.worker.upload-version"]);
    with_files(&f);
    let reply = f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "dist/worker.js")
            .param("compatibility-date", "2026-09-01"),
    );
    assert!(reply.as_error().is_none(), "{reply:?}");
    let seen = f.fake.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert!(seen[0].path.ends_with("/versions"), "{}", seen[0].path);
    assert!(
        !seen[0].path.contains("deployments"),
        "uploading a version moved traffic: {}",
        seen[0].path
    );

    // And the grant for one does not carry the other, so a project that lets
    // an agent build cannot thereby let it release.
    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION),
    );
    assert_eq!(
        reply.as_error().map(|(kind, _)| kind),
        Some(ErrorKind::PermissionDenied),
        "the upload grant reached the deploy: {reply:?}"
    );
}

/// §13.7's staged traffic. Cloudflare's payload is the whole split every time,
/// so the remaining share has to go somewhere — and the only honest somewhere
/// is the version that has it now, which costs a credentialled lookup to find.
///
/// Mutations: send the remainder to the version being deployed; drop the
/// lookup and send 100. Both red.
#[test]
fn a_staged_rollout_leaves_the_rest_of_the_traffic_where_it_was() {
    let f = Fixture::new("staged", Mode::Normal, &["cloudflare.worker.deploy"]);
    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", NEW_VERSION)
            .param("percentage", "30"),
    );
    assert!(reply.as_error().is_none(), "{reply:?}");

    let seen = f.fake.seen();
    assert_eq!(seen.len(), 2, "a staged rollout is a lookup and a change: {seen:#?}");
    assert_eq!(seen[0].method, "GET");
    assert!(seen[0].path.ends_with("/deployments"), "{}", seen[0].path);
    assert_eq!(seen[1].method, "POST");

    let body: serde_json::Value = serde_json::from_str(&seen[1].body).expect("json");
    assert_eq!(body["strategy"], "percentage");
    let versions = body["versions"].as_array().expect("versions");
    assert_eq!(
        versions.len(),
        2,
        "a partial rollout that names one version is a full one: {body}"
    );
    assert_eq!(versions[0]["version_id"], NEW_VERSION);
    assert_eq!(versions[0]["percentage"], 30.0);
    assert_eq!(
        versions[1]["version_id"], OLD_VERSION,
        "the rest of the traffic went to the wrong version: {body}"
    );
    assert_eq!(versions[1]["percentage"], 70.0);
    let total: f64 = versions
        .iter()
        .map(|v| v["percentage"].as_f64().expect("a number"))
        .sum();
    assert!((total - 100.0).abs() < f64::EPSILON, "the split does not add up: {body}");
}

/// A rollout onto a worker with nothing serving, and one onto a worker already
/// split, are both refused — and neither refusal is the other's.
///
/// The alternative is a build that reads the first version out of the first
/// deployment and hopes. That build works until somebody runs a canary, and
/// then it ends the canary without mentioning it.
///
/// Mutation: take `serving[0]` whatever the length. Red.
#[test]
fn a_rollout_with_no_single_version_to_keep_the_rest_is_refused_rather_than_guessed() {
    let f = Fixture::new("nosplit", Mode::Normal, &["cloudflare.worker.deploy"]);

    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "never-deployed")
            .param("version", NEW_VERSION)
            .param("percentage", "25"),
    );
    let (_, message) = reply.as_error().expect("nothing to keep");
    assert!(message.contains("never been deployed"), "{message}");

    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "already-split")
            .param("version", NEW_VERSION)
            .param("percentage", "25"),
    );
    let (_, message) = reply.as_error().expect("no single version to keep");
    assert!(message.contains("already split"), "{message}");
    assert!(
        message.contains(OLD_VERSION) && message.contains(NEW_VERSION),
        "the refusal must name both, or the reader cannot act on it: {message}"
    );

    // Neither changed anything: two lookups, no deployment.
    let seen = f.fake.seen();
    assert_eq!(seen.len(), 2, "{seen:#?}");
    assert!(seen.iter().all(|s| s.method == "GET"), "{seen:#?}");
}

/// "Permission denied is not absence", in §13.7's lookup.
///
/// A worker whose deployments this credential may not read is not a worker
/// with no deployments — and treating it as one would deploy a version to
/// 100% of traffic while the caller asked for 25.
#[test]
fn a_deployments_lookup_that_was_refused_is_not_a_worker_that_was_never_deployed() {
    let f = Fixture::new("denied-lookup", Mode::EchoUnauthorized, &["cloudflare.worker.deploy"]);
    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", NEW_VERSION)
            .param("percentage", "25"),
    );
    let (kind, message) = reply.as_error().expect("a refused lookup is not a success");
    assert_eq!(kind, ErrorKind::PermissionDenied);
    assert!(message.contains("401"), "{message}");
    assert!(
        message.contains("not the same as"),
        "the refusal does not distinguish the two: {message}"
    );
    assert!(!message.contains(TOKEN), "the refusal carries the credential: {message}");
    // Nothing was deployed.
    assert!(
        f.fake.seen().iter().all(|s| s.method == "GET"),
        "{:#?}",
        f.fake.seen()
    );
}

/// A share that is not one is refused rather than clamped. A caller who wrote
/// `150` meant something, and deploying at 100 because 150 is out of range is
/// this service deciding what they meant.
#[test]
fn a_share_of_traffic_outside_the_range_cloudflare_documents_is_refused_not_clamped() {
    let f = Fixture::new("share", Mode::Normal, &["cloudflare.worker.deploy"]);
    for bad in ["0", "150", "-5", "half", "1e3"] {
        let reply = f.use_it(
            f.record("cloudflare.worker.deploy", "project")
                .param("version", NEW_VERSION)
                .param("percentage", bad),
        );
        assert!(
            reply.as_error().is_some(),
            "'{bad}' was accepted as a share of traffic: {reply:?}"
        );
    }
    assert!(f.fake.seen().is_empty(), "one of them reached cloudflare");

    // …and a hundred is a full deployment, not a split: one request, no
    // lookup, because the answer could not change anything.
    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "project")
            .param("version", NEW_VERSION)
            .param("percentage", "100"),
    );
    assert!(reply.as_error().is_none(), "{reply:?}");
    let seen = f.fake.seen();
    assert_eq!(seen.len(), 1, "a full deployment asked a question it did not need");
    let body: serde_json::Value = serde_json::from_str(&seen[0].body).expect("json");
    assert_eq!(body["versions"].as_array().expect("versions").len(), 1);
}

/// §13.7's last line: *with rollback if health checks fail*. The rollback path
/// is its own operation with its own grant, and it is what tells Cloudflare
/// that going back to an older version is deliberate rather than a deployment
/// that lost a race.
#[test]
fn the_rollback_path_puts_an_older_version_back_and_says_it_meant_to() {
    let f = Fixture::new("rollback", Mode::Normal, &["cloudflare.worker.rollback"]);
    let reply = f.use_it(
        f.record("cloudflare.worker.rollback", "project")
            .param("version", OLD_VERSION)
            .param("message", "the health check failed"),
    );
    assert!(reply.as_error().is_none(), "{reply:?}");
    let seen = f.fake.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert!(
        seen[0].path.contains("/deployments?force=true"),
        "without force, cloudflare reads this as a deployment that lost a \
         race rather than a deliberate return: {}",
        seen[0].path
    );
    let body: serde_json::Value = serde_json::from_str(&seen[0].body).expect("json");
    assert_eq!(body["versions"][0]["version_id"], OLD_VERSION);
    assert_eq!(body["versions"][0]["percentage"], 100);
    assert_eq!(body["annotations"]["workers/message"], "the health check failed");

    // And a deploy grant does not carry it: going back is not going forward.
    let f = Fixture::new("rollback2", Mode::Normal, &["cloudflare.worker.deploy"]);
    let reply = f.use_it(
        f.record("cloudflare.worker.rollback", "project").param("version", OLD_VERSION),
    );
    assert_eq!(
        reply.as_error().map(|(kind, _)| kind),
        Some(ErrorKind::PermissionDenied),
        "{reply:?}"
    );
}

/// The trail has to tell a staged rollout from a full deployment, because they
/// are the same operation with the same version id and very different
/// consequences.
#[test]
fn a_staged_rollout_and_a_full_deployment_do_not_read_the_same_in_the_trail() {
    let f = Fixture::new("trail-share", Mode::Normal, &["cloudflare.worker.deploy"]);
    assert!(f
        .use_it(
            f.record("cloudflare.worker.deploy", "project")
                .param("version", NEW_VERSION)
                .param("percentage", "30")
        )
        .as_error()
        .is_none());
    assert!(f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION))
        .as_error()
        .is_none());
    let trail = f.trail();
    assert!(trail.contains("to 30% of its traffic"), "{trail}");
    let details: Vec<&str> = trail
        .lines()
        .filter(|l| l.contains("\"used\""))
        .collect();
    assert_eq!(details.len(), 2);
    assert_ne!(
        details[0], details[1],
        "the two runs are indistinguishable in the trail"
    );
}

/// "Short-lived/scoped credentials used where provider supports them" means
/// *every* operation that has a narrow form, not the handful a test happened
/// to pick.
///
/// This is the only thing that exercises `secret.bind`'s two-group policy —
/// the one row in [`super::temporary::POLICY`] that needs more than one
/// permission group, and therefore the only one where a build that looked up
/// just the first slot would still look correct.
///
/// Mutation: take only `wanted[0]` in `temporary::group_ids`. Red.
#[test]
fn every_operation_with_a_narrow_form_actually_gets_one() {
    let f = Fixture::new("sweep-mint", Mode::Minting, &granted_everything());
    with_files(&f);

    let mut narrowed = 0;
    for (operation, resource, options, _) in every_operation() {
        let policy = super::temporary::policy_for(operation).expect("every operation has a row");
        let super::temporary::Narrowest::Token { groups, .. } = policy else {
            continue;
        };
        // The fixture's account deliberately does not offer this one, so it is
        // the could-not-run row rather than a narrowed one.
        if operation == "cloudflare.hyperdrive.read" {
            continue;
        }
        let before = f.fake.minting().len();
        let mut rec = f.record(operation, resource);
        for (name, value) in &options {
            rec = rec.param(name, value);
        }
        let reply = f.use_it(rec);
        assert!(reply.as_error().is_none(), "{operation} was refused: {reply:?}");

        let exchange = &f.fake.minting()[before..];
        assert_eq!(
            exchange.len(),
            3,
            "{operation} did not ask for, spend and give back a short-lived \
             credential: {exchange:#?}"
        );
        let creation = exchange
            .iter()
            .find(|s| s.method == "POST")
            .unwrap_or_else(|| panic!("{operation} created no token"));
        let body: serde_json::Value =
            serde_json::from_str(&creation.body).expect("the creation body is json");
        assert_eq!(
            body["policies"][0]["permission_groups"]
                .as_array()
                .expect("groups")
                .len(),
            groups.len(),
            "{operation} asked for a token carrying the wrong number of \
             permission groups: {body}"
        );
        narrowed += 1;
    }
    // A sweep that swept nothing would pass.
    assert!(narrowed >= 20, "only {narrowed} operations were measured");
}

// ── §13.8, environment protection (P1-014) ──────────────────────────────────

/// The fixture's project with §13.8 left at its defaults.
///
/// Byte-for-byte [`PROJECT_FILE`] without the two `unattended = true` lines,
/// so every difference between a test that uses this one and a test that uses
/// the other is the protection and nothing else.
fn protected_project_file() -> String {
    let file = PROJECT_FILE.replace("unattended = true\n", "");
    // The prose above each one still says the word, so the check is on lines
    // that would be READ rather than on the text: a comment mentioning
    // `unattended` is not an opt-out, and a test that could not tell the two
    // apart would either never pass or pass without checking anything.
    assert!(
        !file.lines().any(|line| line.trim_start().starts_with("unattended")),
        "the protected fixture still opts out somewhere"
    );
    assert_eq!(
        file.len(),
        PROJECT_FILE.len() - 2 * "unattended = true\n".len(),
        "the protected fixture differs from PROJECT_FILE by more than the two \
         opt-out lines"
    );
    file
}

impl Fixture {
    /// Record one owner approval, the way `apex secret approve` does.
    fn approve(&self, operation: &str, resource: &str) -> Response {
        self.service.approve(
            me(),
            crate::service::NewApproval {
                project: self.project.to_str().expect("utf8"),
                service: "cloudflare",
                operation,
                resource,
                ttl_ms: None,
                withdraw: false,
            },
        )
    }

    fn outstanding(&self) -> Vec<apex_secret_core::store::Approval> {
        match self.service.approvals(me()) {
            Response::Approvals { pending } => pending,
            other => panic!("not an approvals reply: {other:?}"),
        }
    }
}

#[test]
fn preview_and_staging_deploy_unattended_and_every_other_environment_does_not() {
    // §13.8's table, and the polarity that makes it more than a spelling
    // check. `preview` and `staging` are the WHOLE list of names an agent may
    // deploy to on its own without the owner having written anything down;
    // `canary`, `production` and a name nobody has invented yet are protected
    // by the same rule, because the rule is "not on the list" and not "called
    // production".
    let f = Fixture::with_project(
        "protection",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );

    // Unattended: staging's worker has never been deployed, so the refusal
    // that comes back is §13.7's — which is proof the request got past §13.8
    // rather than proof it was allowed to finish.
    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "never-deployed").param("version", NEW_VERSION),
    );
    assert!(
        reply.as_error().is_none(),
        "a staging deploy must not need an approval: {reply:?}"
    );

    for (worker, environment) in [("project", "production"), ("already-split", "canary")] {
        let reply = f.use_it(
            f.record("cloudflare.worker.deploy", worker).param("version", NEW_VERSION),
        );
        let (kind, message) = reply
            .as_error()
            .unwrap_or_else(|| panic!("{environment} deployed unattended: {reply:?}"));
        assert_eq!(kind, ErrorKind::PermissionDenied, "{message}");
        // The refusal has to carry both ways out, or the reader is told no and
        // nothing else.
        assert!(message.contains(environment), "{message}");
        assert!(message.contains("apex secret approve"), "{message}");
        assert!(message.contains("unattended = true"), "{message}");
    }
}

#[test]
fn an_owner_may_enable_unattended_production_for_their_own_project() {
    // §13.8's last line, and the only thing that implements it: one key in the
    // project's own file. The SAME worker, in the SAME environment, with the
    // same grant — the only difference is the line the owner wrote.
    let bare = Fixture::with_project(
        "optout-off",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    let refused = bare
        .use_it(bare.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert!(refused.as_error().is_some(), "{refused:?}");

    let opted_in = Fixture::with_project(
        "optout-on",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        PROJECT_FILE,
    );
    let allowed = opted_in.use_it(
        opted_in.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION),
    );
    assert!(
        allowed.as_error().is_none(),
        "the owner opted in and it was still refused: {allowed:?}"
    );
    // And the trail says it was a grant, not an approval — the opt-out is not
    // a silent self-approval.
    assert!(
        opted_in.trail().contains("\"approval_policy\":\"grant\""),
        "{}",
        opted_in.trail()
    );
}

#[test]
fn a_protected_deployment_runs_once_on_an_approval_and_is_refused_the_second_time() {
    // The whole of "production approval is default": the approval is SPENT.
    // A mechanism where the owner's yes kept working would be a grant with a
    // longer command.
    let f = Fixture::with_project(
        "spend",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    assert!(f.outstanding().is_empty());

    let reply = f.approve("cloudflare.worker.deploy", "project");
    assert!(reply.as_error().is_none(), "{reply:?}");
    assert_eq!(f.outstanding().len(), 1, "{:?}", f.outstanding());

    let first = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert!(first.as_error().is_none(), "the approved deployment was refused: {first:?}");
    assert!(
        f.outstanding().is_empty(),
        "the approval survived being used: {:?}",
        f.outstanding()
    );

    let second = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert_eq!(
        second.as_error().map(|(kind, _)| kind),
        Some(ErrorKind::PermissionDenied),
        "one approval authorised two deployments: {second:?}"
    );

    // §11's field tells the two apart, which is the only way an owner reading
    // the trail a week later can.
    let trail = f.trail();
    assert!(trail.contains("\"approval_policy\":\"owner\""), "{trail}");
    assert!(trail.contains("\"approval_policy\":\"approval-required\""), "{trail}");
    assert!(
        !trail.contains("\"approval_policy\":\"grant\",\"constraints\":[],\"reason\":null,\"exit_code\":0"),
        "a protected deployment was recorded as authorised by a grant: {trail}"
    );
}

#[test]
fn an_approval_for_one_worker_does_not_approve_another() {
    // The reason an approval is keyed on the resource and a grant is not.
    // Approving the preview deployment must not approve the production one —
    // they are the same operation under the same credential in the same
    // project, and the resource is the only thing that separates them.
    let f = Fixture::with_project(
        "scope",
        Mode::Normal,
        &["cloudflare.worker.deploy", "cloudflare.worker.rollback"],
        &protected_project_file(),
    );
    assert!(f.approve("cloudflare.worker.deploy", "already-split").as_error().is_none());

    let other_worker = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert!(
        other_worker.as_error().is_some(),
        "an approval for 'already-split' deployed 'project': {other_worker:?}"
    );

    // And not another operation on the same worker either.
    assert!(f.approve("cloudflare.worker.deploy", "project").as_error().is_none());
    let other_operation = f
        .use_it(f.record("cloudflare.worker.rollback", "project").param("version", OLD_VERSION));
    assert!(
        other_operation.as_error().is_some(),
        "an approval to deploy authorised a rollback: {other_operation:?}"
    );
}

#[test]
fn uploading_a_version_to_production_needs_no_approval_and_deploying_it_does() {
    // §13.7's flow is upload -> preview -> health check -> staged -> full, and
    // §13.8 sits on the last two steps. Asking the owner at step one is asking
    // them before there is anything to look at, which is how approvals get
    // approved without being read. This is the test that says so: the same
    // worker, in the same protected environment, two operations, two answers.
    let f = Fixture::with_project(
        "upload",
        Mode::Normal,
        &["cloudflare.worker.upload-version", "cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    std::fs::create_dir_all(f.project.join("dist")).expect("dist");
    std::fs::write(f.project.join("dist/worker.js"), "export default {};\n").expect("worker");

    let upload = f.use_it(
        f.record("cloudflare.worker.upload-version", "project")
            .param("script", "dist/worker.js")
            .param("compatibility-date", "2026-09-01"),
    );
    assert!(
        upload.as_error().is_none(),
        "uploading a version is not a deployment and must not need an approval: {upload:?}"
    );

    let deploy = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert!(
        deploy.as_error().is_some(),
        "putting that version in front of traffic must need one: {deploy:?}"
    );
}

#[test]
fn an_approval_is_spent_by_a_deployment_that_failed() {
    // Spent on commit, not on success. An agent that could burn a failed
    // deployment and keep the approval could retry until something worked, and
    // the owner approved one deployment rather than one successful one.
    //
    // `never-deployed` is staging, so a *partial* rollout to it is refused by
    // §13.7 — which needs a protected worker to be a useful test here. So this
    // uses production with a percentage, where the far side's lookup is what
    // fails.
    let f = Fixture::with_project(
        "burn",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    assert!(f.approve("cloudflare.worker.deploy", "already-split").as_error().is_none());

    let reply = f.use_it(
        f.record("cloudflare.worker.deploy", "already-split")
            .param("version", NEW_VERSION)
            .param("percentage", "25"),
    );
    let (_, message) = reply.as_error().expect("already split, so the rollout is refused");
    assert!(message.contains("already split"), "{message}");
    assert!(
        f.outstanding().is_empty(),
        "a failed deployment gave the approval back: {:?}",
        f.outstanding()
    );
}

#[test]
fn an_approval_that_has_expired_is_not_an_approval() {
    let f = Fixture::with_project(
        "expiry",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    let reply = f.service.approve(
        me(),
        crate::service::NewApproval {
            project: f.project.to_str().expect("utf8"),
            service: "cloudflare",
            operation: "cloudflare.worker.deploy",
            resource: "project",
            ttl_ms: Some(1),
            withdraw: false,
        },
    );
    assert!(reply.as_error().is_none(), "{reply:?}");
    std::thread::sleep(std::time::Duration::from_millis(5));

    let deploy = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    assert_eq!(
        deploy.as_error().map(|(kind, _)| kind),
        Some(ErrorKind::PermissionDenied),
        "an expired approval was spent: {deploy:?}"
    );
    // And it is not reported as outstanding either, because an approval that
    // cannot be spent is not one.
    assert!(f.outstanding().is_empty(), "{:?}", f.outstanding());
}

#[test]
fn deployments_racing_one_approval_produce_exactly_one_deployment() {
    // `apex-secretd` serves one thread per connection, so the read-check-write
    // of the approvals file is genuinely concurrent. Without the mutex, two
    // threads read the file, both find the approval, both deploy, and both
    // write back a file missing one entry — a single-use approval used twice,
    // which is the one failure this whole mechanism exists to prevent.
    //
    // ## Why this is eight threads and forty rounds and not two threads once
    //
    // Measured, not guessed. Two threads racing once passed eight times out of
    // eight with the mutex taken out, because the unprotected window is a file
    // read, a vector scan and a file write — microseconds that two threads
    // spawned in sequence rarely land inside. A test that cannot fail when the
    // thing it tests is removed is this repository's named defect, so the
    // shape had to change rather than the claim being softened: every thread
    // waits on a barrier so they arrive together, and the round is repeated
    // until the interleaving happens.
    //
    // What that buys is a test that fails within the first few rounds with the
    // mutex gone. What it does not buy is a proof: a machine that serialised
    // these threads for its own reasons would still pass. The invariant this
    // is the concurrent half of is asserted deterministically in
    // `store::tests::an_approval_is_spent_once_and_is_then_gone`.
    const THREADS: usize = 8;
    const ROUNDS: usize = 40;

    let f = Fixture::with_project(
        "race",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );

    for round in 0..ROUNDS {
        assert!(f.approve("cloudflare.worker.deploy", "project").as_error().is_none());
        let allowed = std::sync::atomic::AtomicUsize::new(0);
        let gate = std::sync::Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    // Everything expensive happens before the barrier, so what
                    // the threads race is the spend and not the allocation of
                    // the record.
                    let rec = f
                        .record("cloudflare.worker.deploy", "project")
                        .param("version", NEW_VERSION);
                    gate.wait();
                    if f.use_it(rec).as_error().is_none() {
                        allowed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(
            allowed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "round {round}: one approval authorised more than one deployment"
        );
        assert!(
            f.outstanding().is_empty(),
            "round {round}: the approval survived: {:?}",
            f.outstanding()
        );
    }
}

#[test]
fn a_project_that_spells_unattended_as_a_string_is_refused_rather_than_protected() {
    // `unattended = "true"` is somebody switching §13.8 off and being told it
    // worked. Reading it as absent would leave production protected while the
    // file says it is not — and the person would go looking for the reason in
    // the wrong place. Refused when the file is read.
    let file = PROJECT_FILE.replace("unattended = true", "unattended = \"true\"");
    let f = Fixture::with_project(
        "badbool",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &file,
    );
    let reply = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    let (_, message) = reply.as_error().expect("a quoted boolean is not a boolean");
    assert!(message.contains("true or false"), "{message}");
    assert!(message.contains("unattended"), "{message}");
}

#[test]
fn the_refusal_names_the_environment_and_not_only_the_worker() {
    // A message that said "this needs approval" would tell the reader nothing
    // they did not already know. What makes a production deploy different from
    // a preview one is invisible to everything but the binding, so the binding
    // is what has to say it.
    let f = Fixture::with_project(
        "message",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    let reply = f
        .use_it(f.record("cloudflare.worker.deploy", "project").param("version", NEW_VERSION));
    let (_, message) = reply.as_error().expect("protected");
    for expected in [
        "project",
        "production",
        "apex secret approve cloudflare cloudflare.worker.deploy project",
        "[cloudflare.production]",
        "apex.toml",
    ] {
        assert!(message.contains(expected), "'{expected}' is missing from: {message}");
    }
}

#[test]
fn approving_records_a_line_of_its_own_in_the_trail() {
    // `granted` and `approved` are different facts, and a trail that spelled
    // both `granted` could not answer "did the owner approve this deployment,
    // or had they allowed every deployment months ago".
    let f = Fixture::with_project(
        "trail",
        Mode::Normal,
        &["cloudflare.worker.deploy"],
        &protected_project_file(),
    );
    let reply = f.approve("cloudflare.worker.deploy", "project");
    assert!(reply.as_error().is_none(), "{reply:?}");
    let trail = f.trail();
    assert!(trail.contains("\"event\":\"approved\""), "{trail}");
    assert!(trail.contains("cloudflare.worker.deploy project"), "{trail}");
    assert_ne!(
        AuditEvent::Approved.as_str(),
        AuditEvent::Granted.as_str(),
        "an approval and a grant must not read the same in the trail"
    );
}

/// The sentence `perform` writes for a failure and the sentence
/// `credential_refused` reads are the same sentence.
///
/// They are four hundred lines apart, and a build where they drifted would go
/// back to reporting a token that had not propagated yet as the operation's
/// own answer — silently, because nothing else in the suite can tell a
/// re-tried operation from one that was never re-tried.
///
/// The 401/403 split is the other half. Cloudflare answers **401** when it
/// does not recognise the credential, which is what a token it issued moments
/// ago looks like to an API that has not seen it yet, and **403** when it
/// recognises the credential and will not let it do this. Waiting changes the
/// first and never changes the second.
#[test]
fn only_a_401_is_the_far_side_refusing_the_credential_itself() {
    let provider = CloudflareProvider::at(1);
    let outcome = |status: u16| Performed {
        code: 1,
        output: format!("{}\n{{\"success\":false}}", super::answered_http(status)),
        created: None,
    };

    assert!(
        provider.credential_refused(&outcome(401)),
        "a 401 is what a minted token that has not propagated looks like"
    );
    for status in [403, 404, 429, 500] {
        assert!(
            !provider.credential_refused(&outcome(status)),
            "HTTP {status} was read as the credential being refused, so every \
             one of them would be asked three times"
        );
    }

    // A success carries no such line at all, and neither does a transport
    // failure — `perform` writes a different sentence for that one.
    assert!(!provider.credential_refused(&Performed {
        code: 0,
        output: "{\"success\":true}".to_string(),
        created: None,
    }));
    assert!(!provider.credential_refused(&Performed {
        code: 1,
        output: "apex: the api could not be reached\n".to_string(),
        created: None,
    }));
}
