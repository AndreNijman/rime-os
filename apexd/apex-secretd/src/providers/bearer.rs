//! A second provider, so the framework is tested by something that is not git.
//!
//! An abstraction with one implementation is a description of that
//! implementation. This is an HTTP API provider — bearer token in a header,
//! resources addressed as paths, a short-lived credential minted per operation
//! — which shares nothing with git except the framework between them. It is the
//! shape §14's `CloudflareProvider`, `AWSProvider` and `GCPProvider` will have,
//! and P1-002 can read it as the worked example.
//!
//! It is compiled only for tests, and that is deliberate rather than a
//! shortcut. A provider that shipped would need a real service behind it to be
//! honest about, and the point here is the *framework*: every step of
//! `Service::use_capability` runs against it for real, with a real socket, a
//! real credential in a real store, and a real HTTP request that a loopback
//! server answers only when the header is right.
//!
//! What that proves, and a mock could not:
//!
//! * a provider with a different **credential presentation** (a header, not a
//!   git credential helper) needs no framework change;
//! * a provider with a different **resource shape** (`bucket/key`, not a remote
//!   name) needs no framework change;
//! * the **host pin** is applied to it without it doing anything, because the
//!   framework applies it between `bind` and `perform`;
//! * `mint` works, and the framework scrubs the **minted** credential as well
//!   as the stored one — which §13.4 requires and which no git test can reach.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use apex_secret_core::operation::{
    Effect, OperationSpec, ParamSpec, ProviderSpec, ResourceKind, Syntax,
};
use apex_secret_core::SecretValue;

use crate::provider::{Approval, Bind, Bound, Endpoint, Lease, Minted, Performed, Provider, ProviderError};

pub const SPEC: ProviderSpec = ProviderSpec {
    id: "demo",
    summary: "an http api, used to test the framework against something not git",
    operations: &[
        OperationSpec {
            id: "demo.account.read",
            summary: "read the account this credential belongs to",
            effect: Effect::Read,
            resource: ResourceKind::None,
            params: &[],
            aliases: &[],
            // The honest twin of `cloudflare.account.read`: the same
            // declaration — no resource, no parameters — and a `bind` that
            // reads only the stored record. Declared true so the framework has
            // a case where the claim holds as well as one where it does not.
            same_everywhere: true,
        },
        OperationSpec {
            id: "demo.object.read",
            summary: "read an object",
            effect: Effect::Read,
            resource: ResourceKind::Path,
            params: &[],
            aliases: &[],
            same_everywhere: false,
        },
        OperationSpec {
            id: "demo.object.write",
            summary: "write an object",
            effect: Effect::Write,
            resource: ResourceKind::Path,
            params: &[ParamSpec {
                name: "note",
                syntax: Syntax::Text,
                required: false,
                summary: "an annotation to store with it",
            }],
            aliases: &[],
            same_everywhere: false,
        },
    ],
};

/// The provider, carrying its own configuration.
///
/// A real one carries the same kind of thing: P1-002's Cloudflare provider
/// carries §13.1's account and zone binding. The framework never looks inside
/// it.
pub struct BearerProvider {
    pub port: u16,
    /// What [`Provider::mint`] answers. §13.4's four outcomes, so the
    /// framework's branch for each one is exercised rather than assumed.
    pub mints: Minting,
    /// Whether [`Provider::perform`] fails with the credential in its error
    /// message — the mistake a provider makes when it hands back whatever the
    /// tool it drove said about a failed request.
    pub fails_with_token: bool,
    /// Every lease this provider was asked to revoke, in order. Shared with
    /// the test that built it, because the provider itself disappears into the
    /// registry — and the question a test needs to answer is whether the
    /// framework asked at all, which cannot be seen from outside.
    pub revoked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Whether the revoke reports that it could not be done.
    pub revoke_fails: bool,
}

/// What the test provider's [`Provider::mint`] will answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Minting {
    /// A short-lived credential. §13.4's path.
    Narrowed,
    /// No narrower form exists. What git and MCP mean.
    None,
    /// The far side refused to issue one.
    Denied,
    /// The attempt did not reach a conclusion.
    CouldNotRun,
    /// The project requires a narrowed credential and there is not one, so the
    /// operation must not run at all.
    Refuses,
}

/// The handle the test provider's lease carries.
pub const LEASE: &str = "lease-7f2a";

/// What `mint` hands back. Distinct from the stored value so a test can tell
/// which one reached the far side, and so both must be scrubbed.
pub const MINTED: &str = "apex-minted-2c8f04b1-do-not-leak";

impl BearerProvider {
    fn path(&self, req: &Bind<'_>) -> String {
        match req.resource {
            "" => "/account".to_string(),
            resource => format!("/{resource}"),
        }
    }

    fn url(&self, req: &Bind<'_>) -> String {
        format!("http://{}:{}{}", req.service.host, self.port, self.path(req))
    }
}

impl Provider for BearerProvider {
    fn spec(&self) -> &'static ProviderSpec {
        &SPEC
    }

    /// What a resource name means here: a path under this provider's own base.
    ///
    /// Nothing resolves it against anything the caller controls, unlike git —
    /// which is the other half of the point. The framework does not care which
    /// it is; it cares that the provider names an endpoint before it is given a
    /// credential.
    fn bind(&self, req: &Bind<'_>) -> Result<Bound, ProviderError> {
        let url = self.url(req);
        Ok(Bound {
            endpoint: Endpoint::from_url(&url)?,
            detail: format!("{} {}", req.operation.id, self.path(req)),
            creates: None,
            // A bearer request reaches only the endpoint pinned when its
            // credential was stored, so there is no second thing for the owner
            // to be asked about.
            approval: Approval::Standing,
        })
    }

    /// §13.4: a scoped credential for this one operation.
    fn mint(
        &self,
        _req: &Bind<'_>,
        _bound: &Bound,
        _value: &SecretValue,
    ) -> Result<Minted, ProviderError> {
        Ok(match self.mints {
            Minting::Narrowed => Minted::Narrowed {
                value: SecretValue::new(MINTED.as_bytes().to_vec()),
                lease: Lease {
                    handle: LEASE.to_string(),
                    expires_ms: 4_102_444_800_000,
                },
            },
            Minting::None => Minted::NoNarrowerForm("there is no narrower form".to_string()),
            Minting::Denied => Minted::Denied("the far side would not issue one".to_string()),
            Minting::CouldNotRun => {
                Minted::CouldNotRun("the far side could not be reached".to_string())
            }
            Minting::Refuses => {
                return Err(ProviderError::Refused(
                    "this project runs only on a narrowed credential".to_string(),
                ))
            }
        })
    }

    fn revoke(
        &self,
        _req: &Bind<'_>,
        _bound: &Bound,
        _stored: &SecretValue,
        lease: &Lease,
    ) -> Result<(), String> {
        self.revoked.lock().expect("lock").push(lease.handle.clone());
        if self.revoke_fails {
            return Err("the far side would not take the revoke".to_string());
        }
        Ok(())
    }

    /// How the credential is presented: `Authorization: Bearer`.
    fn perform(
        &self,
        req: &Bind<'_>,
        _bound: &Bound,
        value: &SecretValue,
    ) -> Result<Performed, ProviderError> {
        let token = value
            .as_str()
            .ok_or_else(|| ProviderError::Failed("that credential is not text".into()))?;
        if self.fails_with_token {
            // Badly written, on purpose. The framework is what makes it not
            // matter, and a provider written next year will do this by
            // accident.
            return Err(ProviderError::Failed(format!(
                "the api rejected the request: upstream said `Bearer {token}` \
                 was not accepted"
            )));
        }
        let method = if req.operation.effect.is_write() {
            "PUT"
        } else {
            "GET"
        };
        let (status, body) = call(&req.service.host, self.port, &self.path(req), method, token)
            .map_err(|e| ProviderError::Failed(format!("reaching the api: {e}")))?;
        Ok(Performed {
            code: if status == 200 { 0 } else { 1 },
            // Deliberately unscrubbed. The far side echoes the header back, the
            // way a badly written API reports an auth failure, and the
            // framework is what keeps that out of the caller's hands.
            output: format!("{status} {body}"),
            created: None,
        })
    }

    /// The far side not recognising the credential, which here is a 401.
    ///
    /// The real one this stands in for is Cloudflare, where a token seconds
    /// old is refused by some products' APIs and accepted by others' — see
    /// [`crate::provider::Provider::credential_refused`]. A 403 would be the
    /// far side recognising the credential and refusing the *operation*, and
    /// this deliberately does not claim one.
    fn credential_refused(&self, performed: &Performed) -> bool {
        performed.code != 0 && performed.output.starts_with("401 ")
    }
}

/// One HTTP/1.1 request, hand-rolled.
///
/// No dependency for a test provider. P1-002 decides between a `wrangler` child
/// and an HTTP crate on its own merits.
fn call(
    host: &str,
    port: u16,
    path: &str,
    method: &str,
    token: &str,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect((host, port))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    )?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }
    let mut body = String::new();
    reader.read_to_string(&mut body)?;
    Ok((status, body.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use apex_secret_core::audit::{self, AuditEvent};
    use apex_secret_core::capability::CapabilityRecord;
    use apex_secret_core::protocol::{ErrorKind, Request, Response};
    use apex_secret_core::store::Store;

    use crate::peer::Peer;
    use crate::provider::Registry;
    use crate::service::{NewService, Service};

    /// Distinctive enough that a grep for it cannot match by accident.
    const STORED: &str = "apex-sentinel-9d1e77a3-do-not-leak";

    /// An API that answers only with a bearer token, and says which one it saw.
    struct Api {
        port: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Api {
        /// An API that answers `401` to the first `times`
        /// requests carrying `token`.
        ///
        /// A stand-in for what Cloudflare actually did on 2026-09-12: a token
        /// it had just issued was refused by one product's API and accepted by
        /// another's, until a few seconds had passed. The count is per token
        /// and not per request so that a test can refuse the MINTED credential
        /// while leaving the stored one alone — which is the difference
        /// between the two cases the framework has to tell apart.
        fn start_refusing(token: &str, times: u32) -> Api {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("addr").port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let refuse = Arc::new(Mutex::new((format!("Bearer {token}"), times)));
            let recorder = Arc::clone(&seen);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let recorder = Arc::clone(&recorder);
                    let refuse = Arc::clone(&refuse);
                    std::thread::spawn(move || serve(stream, &recorder, &refuse));
                }
            });
            Api { port, seen }
        }

        fn authorizations(&self) -> Vec<String> {
            self.seen.lock().expect("lock").clone()
        }
    }

    fn serve(
        mut stream: TcpStream,
        recorder: &Arc<Mutex<Vec<String>>>,
        refuse: &Arc<Mutex<(String, u32)>>,
    ) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut first = String::new();
        if reader.read_line(&mut first).is_err() {
            return;
        }
        let mut authorization = None;
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
        }
        let Some(authorization) = authorization else {
            let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
            return;
        };
        // Recorded BEFORE the refusal branch, so a test counting attempts
        // counts the refused ones too. A retry that was not made and a retry
        // that was made and refused must not look the same from here.
        recorder.lock().expect("lock").push(authorization.clone());
        {
            let mut refuse = refuse.lock().expect("lock");
            if refuse.1 > 0 && authorization == refuse.0 {
                refuse.1 -= 1;
                let _ =
                    stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                return;
            }
        }
        // Echoing the credential back is what a badly written API does, and the
        // framework's scrub is what makes it not matter.
        let body = format!("ok, you sent {authorization}");
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    }

    struct Fixture {
        service: Service,
        dir: PathBuf,
        api: Api,
        revoked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Fixture {
        /// The leases the framework asked the provider to end, in order.
        fn revoked(&self) -> Vec<String> {
            self.revoked.lock().expect("lock").clone()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
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

    /// A service serving ONLY the bearer provider, with a credential stored for
    /// the fixture and one operation granted.
    fn fixture(name: &str, mints: Minting, granted: &str) -> Fixture {
        fixture_with(name, mints, false, false, granted)
    }

    fn fixture_with(
        name: &str,
        mints: Minting,
        fails_with_token: bool,
        revoke_fails: bool,
        granted: &str,
    ) -> Fixture {
        fixture_all(name, mints, fails_with_token, revoke_fails, granted, "", 0)
    }

    /// A fixture whose API refuses `token` the first `times` it is presented.
    fn fixture_refusing(
        name: &str,
        mints: Minting,
        granted: &str,
        token: &str,
        times: u32,
    ) -> Fixture {
        fixture_all(name, mints, false, false, granted, token, times)
    }

    #[allow(clippy::too_many_arguments)]
    fn fixture_all(
        name: &str,
        mints: Minting,
        fails_with_token: bool,
        revoke_fails: bool,
        granted: &str,
        refused_token: &str,
        refused_times: u32,
    ) -> Fixture {
        let api = Api::start_refusing(refused_token, refused_times);
        let dir = std::env::temp_dir().join(format!(
            "apex-bearer-{name}-{}-{}",
            std::process::id(),
            api.port
        ));
        std::fs::remove_dir_all(&dir).ok();

        let revoked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = Registry::new();
        registry
            .register(Box::new(BearerProvider {
                port: api.port,
                mints,
                fails_with_token,
                revoked: std::sync::Arc::clone(&revoked),
                revoke_fails,
            }))
            .expect("register");
        // Milliseconds rather than the shipped three seconds: the loop under
        // test is the one that ships, and only the wait inside it is shortened
        // — skipping the sleep by skipping the code would measure nothing.
        let service = Service::new(Store::new(dir.clone()), false, registry)
            .with_refusal_pause(std::time::Duration::from_millis(10));

        let peer = me();
        // `http` is allowed for a loopback host: the credential does not cross
        // a network, which is the whole reason the carve-out exists.
        assert_eq!(
            service.add(
                peer,
                NewService {
                    service: "api",
                    host: "127.0.0.1",
                    scheme: "http",
                    username: None,
                    path: "",
                    auth: None,
                    port: None,
                },
                SecretValue::new(STORED.as_bytes().to_vec()),
            ),
            Response::Ok
        );
        assert!(service
            .grant(peer, "/tmp/apex-bearer-project", "api", granted, false)
            .as_error()
            .is_none());
        Fixture {
            service,
            dir,
            api,
            revoked,
        }
    }

    fn record(operation: &str, resource: &str) -> CapabilityRecord {
        let mut rec = CapabilityRecord::new("api", operation, resource);
        rec.project = Some("/tmp/apex-bearer-project".into());
        rec
    }

    #[test]
    fn a_provider_that_is_not_git_runs_end_to_end_through_the_framework() {
        // Every step of the pipeline, against a provider that shares nothing
        // with git: a header instead of a credential helper, a path instead of
        // a remote name, and a real HTTP request on a real socket that the far
        // side refuses without the credential.
        let f = fixture("run", Minting::None, "demo.object.read");
        let reply = f
            .service
            .use_capability(me(), record("demo.object.read", "bucket/logs/today.json"), Vec::new());

        let Response::Performed {
            record,
            endpoint,
            exit_code,
            output,
        } = &reply
        else {
            panic!("refused: {reply:?}");
        };
        assert_eq!(*exit_code, 0, "{output}");
        assert_eq!(endpoint, "http://127.0.0.1");
        assert_eq!(record.operation, "demo.object.read");
        assert_eq!(record.approval_policy, "grant");

        // The credential DID reach the provider — so this was a real
        // credential-backed operation and not a no-op that exited zero.
        assert_eq!(
            f.api.authorizations(),
            vec![format!("Bearer {STORED}")],
            "the credential did not reach the api"
        );
        // ...and did NOT reach the caller, even though the far side echoed it
        // straight back. That is the framework's scrub, not the provider's.
        assert!(!output.contains(STORED), "{output}");
        assert!(output.contains("«redacted»"), "{output}");
        assert!(!serde_json::to_string(&reply).unwrap().contains(STORED));
    }

    #[test]
    fn a_minted_credential_is_the_one_used_and_is_scrubbed_too() {
        // §13.4: prefer a short-lived credential where the provider has one,
        // and do not hand even that to the agent. No git test can reach this —
        // the git provider has no short-lived form.
        let f = fixture("mint", Minting::Narrowed, "demo.object.read");
        let reply = f
            .service
            .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let Response::Performed { output, .. } = &reply else {
            panic!("refused: {reply:?}");
        };

        // The minted one went to the provider; the stored one never left.
        assert_eq!(f.api.authorizations(), vec![format!("Bearer {MINTED}")]);
        assert!(!output.contains(MINTED), "the minted token came back: {output}");
        assert!(!output.contains(STORED), "{output}");
    }

    /// The lease ends whatever the operation did, and the framework is what
    /// ends it — a provider cannot, because it does not know whether `perform`
    /// was the last thing to happen.
    ///
    /// Both halves in one test because they are one property: a credential
    /// minted for an operation does not outlive it, and a failure is the case
    /// most likely to be forgotten.
    #[test]
    fn the_framework_ends_a_lease_however_the_operation_ended() {
        let worked = fixture("lease-ok", Minting::Narrowed, "demo.object.read");
        assert!(worked
            .service
            .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new())
            .as_error()
            .is_none());
        assert_eq!(worked.revoked(), vec![LEASE.to_string()]);

        let failed = fixture_with(
            "lease-fail",
            Minting::Narrowed,
            true,
            false,
            "demo.object.read",
        );
        assert!(failed
            .service
            .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new())
            .as_error()
            .is_some());
        assert_eq!(
            failed.revoked(),
            vec![LEASE.to_string()],
            "the operation failed and its credential was left standing"
        );
    }

    /// The four answers reach the trail as four different words, at the
    /// framework layer where every provider that will ever be written meets
    /// them.
    ///
    /// This is the "permission denied is not absence" rule in the one place it
    /// is easiest to break: three of the four arms end with the stored
    /// credential being used, so from the operation's point of view they look
    /// identical, and only the trail tells them apart.
    #[test]
    fn a_narrowing_that_failed_and_a_narrowing_with_nothing_to_do_are_not_the_same_line() {
        let word = |name: &str, mints: Minting| -> String {
            let f = fixture(name, mints, "demo.object.read");
            assert!(f
                .service
                .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new())
                .as_error()
                .is_none());
            let trail = std::fs::read_to_string(Store::new(f.dir.clone()).audit_path())
                .expect("the trail");
            let line: serde_json::Value = serde_json::from_str(
                trail
                    .lines()
                    .rfind(|l| l.contains("\"used\""))
                    .expect("a used line"),
            )
            .expect("json");
            line["narrowing"].as_str().expect("narrowing").to_string()
        };
        assert_eq!(word("w-narrowed", Minting::Narrowed), "narrowed");
        assert_eq!(word("w-none", Minting::None), "no-narrower-form");
        assert_eq!(word("w-denied", Minting::Denied), "denied");
        assert_eq!(word("w-couldnot", Minting::CouldNotRun), "could-not-run");
    }

    /// The `used` line this fixture's last operation wrote.
    fn used_line(f: &Fixture) -> serde_json::Value {
        let trail =
            std::fs::read_to_string(Store::new(f.dir.clone()).audit_path()).expect("the trail");
        serde_json::from_str(
            trail
                .lines()
                .rfind(|l| l.contains("\"used\""))
                .expect("a used line"),
        )
        .expect("json")
    }

    /// A credential minted seconds ago is not usable everywhere yet, and the
    /// operation must not be told that it failed.
    ///
    /// Measured against `api.cloudflare.com` on 2026-09-12 and not imagined:
    /// a token Cloudflare had just issued was accepted immediately by the
    /// account and Workers endpoints and refused by D1's — HTTP 401, four
    /// attempts out of four — while the same operation on the stored
    /// credential succeeded. §13.4 says *prefer* a short-lived credential, and
    /// a preference that turns a working operation into an authentication
    /// failure is a regression with a policy name.
    #[test]
    fn a_minted_credential_refused_while_it_is_new_is_tried_again_and_not_reported_as_the_answer() {
        let f = fixture_refusing("retry-clears", Minting::Narrowed, "demo.object.read", MINTED, 1);
        let reply =
            f.service
                .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let Response::Performed {
            exit_code, output, ..
        } = &reply
        else {
            panic!("refused: {reply:?}");
        };
        assert_eq!(*exit_code, 0, "the refusal was handed back as the answer: {output}");

        // Twice, and both times on the MINTED credential. The second half of
        // that matters as much as the first: a retry that quietly reached for
        // the stored token would look like a success and would have undone the
        // narrowing.
        assert_eq!(
            f.api.authorizations(),
            vec![format!("Bearer {MINTED}"), format!("Bearer {MINTED}")],
        );

        let line = used_line(&f);
        assert_eq!(line["narrowing"], "narrowed");
        let detail = line["narrowing_detail"].as_str().unwrap_or_default();
        assert!(detail.contains("attempt 2"), "the trail does not say it happened: {detail}");
    }

    /// A refusal that never clears is still not a reason to spend the stored
    /// credential.
    ///
    /// The operation fails, and it fails saying which of the two things
    /// happened — otherwise a project that asked for narrow credentials would
    /// be run on the broad one by a code path it never agreed to.
    #[test]
    fn a_minted_credential_the_far_side_never_accepts_is_not_replaced_by_the_stored_one() {
        let f = fixture_refusing("retry-never", Minting::Narrowed, "demo.object.read", MINTED, 99);
        let reply =
            f.service
                .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let Response::Performed { exit_code, .. } = &reply else {
            panic!("refused: {reply:?}");
        };
        assert_eq!(*exit_code, 1, "a refused credential was reported as a success");

        let seen = f.api.authorizations();
        assert_eq!(
            seen.len() as u32,
            1 + crate::service::NARROWED_REFUSAL_RETRIES,
            "attempts: {seen:?}"
        );
        assert!(
            seen.iter().all(|a| a == &format!("Bearer {MINTED}")),
            "the stored credential was spent behind the project's back: {seen:?}"
        );

        let detail = used_line(&f)["narrowing_detail"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(detail.contains("all 3 attempts"), "{detail}");
        assert!(detail.contains("temporary_credentials"), "{detail}");
    }

    /// The retry is keyed on the credential having been MINTED, not on the
    /// status.
    ///
    /// Without that condition every genuine 401 — a token the owner revoked, a
    /// password that changed — would cost nine seconds and three requests to
    /// arrive at the same answer, and the owner's own answer would be asked
    /// for three times.
    #[test]
    fn a_stored_credential_the_far_side_refuses_is_answered_once() {
        let f = fixture_refusing("retry-stored", Minting::None, "demo.object.read", STORED, 99);
        let reply =
            f.service
                .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let Response::Performed { exit_code, .. } = &reply else {
            panic!("refused: {reply:?}");
        };
        assert_eq!(*exit_code, 1);
        assert_eq!(
            f.api.authorizations(),
            vec![format!("Bearer {STORED}")],
            "the owner's own refusal was asked for more than once"
        );
        let detail = used_line(&f)["narrowing_detail"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !detail.contains("attempt"),
            "a run that minted nothing reported attempts at a minted credential: {detail}"
        );
    }

    /// The pause is a measurement, and a build that drops it to nothing goes
    /// back to the defect it was measured against.
    ///
    /// Three seconds was the shortest delay that made D1 accept a freshly
    /// minted token on 2026-09-12 (three, five and twenty each worked twice
    /// out of two; no delay failed four times out of four). This is the one
    /// place that number is checked, because every test shortens it.
    #[test]
    fn the_wait_between_attempts_is_not_shorter_than_the_delay_it_was_measured_against() {
        assert!(
            crate::service::NARROWED_REFUSAL_PAUSE >= std::time::Duration::from_secs(3),
            "a minted token needed three seconds to become usable against D1"
        );
        assert!(crate::service::NARROWED_REFUSAL_RETRIES >= 1);
    }

    /// §13.4 says *prefer*, so three of the four arms fall back to the stored
    /// credential rather than refusing — that is what this service already
    /// did, and refusing instead would break every operation for an owner
    /// whose stored token cannot issue credentials.
    ///
    /// A provider that has been told the project will not accept that says so
    /// with an `Err`, and then nothing runs. The credential is never
    /// presented: the refusal happens before `perform` is reached.
    #[test]
    fn a_provider_that_will_not_run_without_a_narrowed_credential_stops_the_operation() {
        let f = fixture("w-required", Minting::Refuses, "demo.object.read");
        let reply =
            f.service
                .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let (kind, message) = reply.as_error().expect("it must refuse");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("narrowed"), "{message}");
        assert!(
            f.api.authorizations().is_empty(),
            "the credential was presented anyway"
        );
        assert!(f.revoked().is_empty());
    }

    #[test]
    fn a_provider_that_puts_the_credential_in_an_error_does_not_get_to_hand_it_over() {
        // `refuse` was the one path out of `use_capability` that ran no scrub.
        // Everything else either cannot carry a value or goes through
        // `scrub_all` — so a provider that failed AFTER the value was read put
        // the credential in the reply and in the trail, both of which an agent
        // can see. There is nothing hypothetical about the mistake: handing
        // back whatever the tool said is the obvious way to write `perform`.
        let f = fixture_with("failing", Minting::None, true, false, "demo.object.read");
        let reply = f
            .service
            .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());
        let (_, message) = reply.as_error().expect("the provider failed");
        assert!(message.contains("not accepted"), "{message}");
        assert!(!message.contains(STORED), "the refusal carries the credential");
        assert!(message.contains("«redacted»"), "{message}");
        assert!(!serde_json::to_string(&reply).unwrap().contains(STORED));

        let path = Store::new(f.dir.clone()).audit_path();
        let trail = std::fs::read_to_string(&path).expect("the trail");
        assert!(!trail.contains(STORED), "the trail carries the credential");
        assert!(trail.contains("refused"), "it was recorded as a refusal");
    }

    #[test]
    fn the_host_pin_applies_to_a_provider_that_does_nothing_to_earn_it() {
        // The framework compares where the provider says it is going with where
        // the credential was stored for, between `bind` and `perform`. This
        // provider contains no such check and cannot skip one.
        let f = fixture("pin", Minting::None, "demo.object.read");
        let peer = me();
        // Re-store the credential for a different host. The provider still
        // builds its URL from `service.host`, so make them disagree the only
        // way a caller can: by storing for one host and asking the framework to
        // pin against another.
        assert_eq!(
            f.service.add(
                peer,
                NewService {
                    service: "elsewhere",
                    host: "example.invalid",
                    scheme: "https",
                    username: None,
                    path: "",
                    auth: None,
                    port: None,
                },
                SecretValue::new(STORED.as_bytes().to_vec()),
            ),
            Response::Ok
        );
        assert!(f
            .service
            .grant(peer, "/tmp/apex-bearer-project", "elsewhere", "demo.object.read", false)
            .as_error()
            .is_none());

        let mut rec = record("demo.object.read", "bucket/key");
        rec.provider = "elsewhere".into();
        let reply = f.service.use_capability(peer, rec, Vec::new());
        let (kind, message) = reply.as_error().expect("the pin must refuse this");
        assert_eq!(kind, ErrorKind::PermissionDenied);
        assert!(message.contains("example.invalid"), "{message}");
        assert!(
            f.api.authorizations().is_empty(),
            "a refused request still reached the api"
        );
    }

    #[test]
    fn an_ungranted_operation_never_reaches_the_provider() {
        // The grant check runs before `bind`, so a request that was never
        // allowed does not cause the provider to touch anything.
        let f = fixture("ungranted", Minting::None, "demo.object.read");
        let reply = f
            .service
            .use_capability(me(), record("demo.object.write", "bucket/key"), Vec::new());
        assert_eq!(reply.as_error().map(|(k, _)| k), Some(ErrorKind::PermissionDenied));
        assert!(f.api.authorizations().is_empty());
    }

    #[test]
    fn the_declared_shape_is_enforced_for_a_provider_the_framework_never_saw() {
        // Resource kind, parameter names and parameter syntax, all from the
        // provider's own declaration, all checked by the framework.
        let f = fixture("shape", Minting::None, "demo.object.write");
        let peer = me();
        for (operation, resource, param) in [
            // `demo.object.write` takes a path, not an absolute one and not a
            // URL.
            ("demo.object.write", "/etc/passwd", None),
            ("demo.object.write", "https://attacker.example/x", None),
            ("demo.object.write", "a/../b", None),
            // ...and `demo.account.read` takes no resource at all.
            ("demo.account.read", "something", None),
            // An option it does not declare is refused, not ignored.
            ("demo.object.write", "bucket/key", Some(("branch", "main"))),
            // ...and one it does declare must be the shape declared: `note` is
            // Text, which is one printable line.
            ("demo.object.write", "bucket/key", Some(("note", "two\nlines"))),
        ] {
            let mut rec = record(operation, resource);
            if let Some((name, value)) = param {
                rec = rec.param(name, value);
            }
            let reply = f.service.use_capability(peer, rec, Vec::new());
            let (kind, message) = reply
                .as_error()
                .unwrap_or_else(|| panic!("{operation} {resource} {param:?} was accepted"));
            assert_eq!(kind, ErrorKind::BadRequest, "{message}");
        }
        assert!(f.api.authorizations().is_empty());

        // And the one that IS the declared shape goes through.
        let rec = record("demo.object.write", "bucket/key").param("note", "released by apex");
        assert!(
            matches!(f.service.use_capability(peer, rec, Vec::new()), Response::Performed { .. }),
            "a well-formed request was refused"
        );
    }

    #[test]
    fn the_trail_records_the_provider_s_own_words_and_none_of_its_credential() {
        let f = fixture("trail", Minting::Narrowed, "demo.object.read");
        f.service
            .use_capability(me(), record("demo.object.read", "bucket/key"), Vec::new());

        let path = Store::new(f.dir.clone()).audit_path();
        let lines = audit::tail(&path, 10);
        let used = lines
            .iter()
            .find(|l| l.event == AuditEvent::Used)
            .expect("a use was recorded");
        assert_eq!(used.operation, "demo.object.read");
        assert_eq!(used.resource, "bucket/key");
        // `Bound::detail` — the provider's own rendering, not the framework's
        // fallback.
        assert_eq!(used.detail, "demo.object.read /bucket/key");
        assert_eq!(used.endpoint.as_deref(), Some("http://127.0.0.1"));
        assert_eq!(used.exit_code, Some(0));
        assert_eq!(used.approval_policy, "grant");

        let text = std::fs::read_to_string(&path).expect("the trail");
        assert!(!text.contains(STORED), "the trail holds the credential");
        assert!(!text.contains(MINTED), "the trail holds the minted credential");
    }

    #[test]
    fn the_service_advertises_this_provider_without_knowing_what_it_is() {
        // `apex secret capabilities` over the wire. A provider registered here
        // reaches the CLI's help with no CLI change, which is the second
        // acceptance criterion in one assertion.
        let f = fixture("hello", Minting::None, "demo.object.read");
        let Response::Hello {
            capabilities,
            vocabulary,
            ..
        } = f.service.hello()
        else {
            panic!("expected hello");
        };
        assert_eq!(
            capabilities,
            vec![
                "demo.account.read".to_string(),
                "demo.object.read".to_string(),
                "demo.object.write".to_string(),
            ]
        );
        let write = vocabulary
            .iter()
            .find(|o| o.id == "demo.object.write")
            .expect("in the vocabulary");
        assert_eq!(write.effect, "write");
        assert_eq!(write.resource, "path");
        assert_eq!(write.params.len(), 1);
        assert_eq!(write.params[0].name, "note");
    }

    #[test]
    fn a_grant_is_keyed_on_the_operation_and_does_not_spread_to_its_siblings() {
        // `demo.object.read` and `demo.object.write` share a class. Granting
        // one must not grant the other — the semantic vocabulary §13.2 asks for
        // is only worth having if the grant table respects it.
        let f = fixture("siblings", Minting::None, "demo.object.read");
        let Response::Grants { projects } = f.service.grants(me()) else {
            panic!("expected grants");
        };
        assert_eq!(
            projects.get("/tmp/apex-bearer-project"),
            Some(&vec!["api:demo.object.read".to_string()])
        );
    }

    #[test]
    fn a_request_for_an_operation_no_registered_provider_offers_is_refused() {
        // The vocabulary is closed by the registry: this service serves only
        // the bearer provider, so git's operations do not exist for it.
        let f = fixture("closed", Minting::None, "demo.object.read");
        for evil in ["git.push", "demo.object.delete", "exec", "demo.account.write"] {
            let reply = f.service.use_capability(me(), record(evil, "bucket/key"), Vec::new());
            assert_eq!(
                reply.as_error().map(|(k, _)| k),
                Some(ErrorKind::BadRequest),
                "'{evil}' was not refused"
            );
        }
        assert!(f.api.authorizations().is_empty());
    }

    #[test]
    fn the_request_verb_carries_this_provider_unchanged() {
        // The wire type, with a provider `apex-agent-core` has never heard of.
        // If this needed a new field, the protocol would still be
        // provider-shaped.
        let rec = record("demo.object.write", "bucket/key").param("note", "hello");
        let req = Request::Use {
            body_len: 0,
            record: Box::new(rec.clone()),
        };
        let text = serde_json::to_string(&req).expect("serialise");
        assert!(!text.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), req);
    }
}
