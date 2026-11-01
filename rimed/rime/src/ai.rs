//! `rime ai` — the client half of §14's local inference service.
//!
//! Six verbs, and each one is either a *pure decision* rendered through
//! `rimed_core::ai`, a read of the model store, or a request to the per-user
//! daemon. Nothing here reimplements a policy: the backend choice, the VRAM fit
//! and the pull provenance rules all come from that module, so what this prints
//! is what the daemon does.
//!
//! ── The parse/execute split, and why it is not tidiness ────────────────────
//!
//! `rime ai run --on desktop` is coming, and it is not this branch's job. What
//! *is* this branch's job is making sure the remote form is a thin wrapper
//! rather than a second argument parser:
//!
//! ```text
//! RunArgs  --plan_run(args, stdin)-->  RunPlan  --run_local(plan)-->  exit code
//!                                          \--run_remote(host, plan)-->  later
//! ```
//!
//! [`plan_run`] is pure and total: it takes the parsed flags and whatever was
//! piped in, and returns either a [`RunPlan`] or a refusal. It opens nothing.
//! [`RunPlan::request_body`] then renders exactly the JSON that goes on the
//! wire. A remote executor swaps only the last arrow — and because the plan is
//! a value with an asserted shape, the two cannot come to disagree about what a
//! given command line means.
//!
//! ── Why this file contains an HTTP client and the daemon does not ──────────
//!
//! The API endpoint relays the backend's own HTTP API untouched, which is what
//! lets any OpenAI-compatible client use it. Somebody has to speak that
//! protocol, and the right somebody is the *client*: putting it in the daemon
//! would mean the daemon parsing requests it is supposed to forward verbatim.
//!
//! So there is a small HTTP/1.1 client here — request builder, header parser,
//! chunked-transfer decoder, server-sent-events splitter — and every piece of
//! it is a pure function over bytes with unit tests, because a streaming parser
//! that is subtly wrong drops the last token of every answer and nothing
//! notices.
//!
//! ── STATED LIMITATION: a client that only accepts a base URL cannot reach it ─
//!
//! The endpoint is a Unix socket, and that is a deliberate security decision
//! with a real cost that belongs written down rather than discovered.
//!
//! **What works.** Anything that can be told to dial a Unix socket. `curl
//! --unix-socket <path>` (verified present in this image's curl, alongside
//! `--abstract-unix-socket`) is the reference form and is what
//! [`serve`](AiCmd::Serve) prints. HTTP libraries that expose their transport —
//! a custom dialer, a `socketPath`, a UDS transport — can be pointed at it the
//! same way, and `rime ai run` itself is proof the protocol needs nothing
//! special.
//!
//! **What does not.** An SDK whose entire configuration surface is
//! `base_url = "http://host:port"` has nowhere to put a path. That is a genuine
//! gap in §14's "allow agent clients to use local inference through the same
//! service": such a client cannot use it without a bridge.
//!
//! **Why Rime ships no bridge.** A TCP listener forwarding to the socket would
//! restore exactly the exposure the endpoint refuses — a port on 127.0.0.1 with
//! no peer credential, open to every account on the machine and to every
//! sandboxed application holding the network permission — and it would do so
//! under an `rime` verb, which would read as Rime having decided it was safe.
//! It is not safe; it is a trade. So the trade is *printed* instead, as the
//! `socat` line that makes it, with the consequence stated next to it. A person
//! who wants it can have it in one command, and the command and its cost live
//! in the same place.
//!
//! The bridge Rime does provide is the remote one: `rime ai run --on <host>`
//! over §20's ssh transport, where the credential is the user's own ssh
//! identity rather than the absence of one.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use rimed_core::ai::{
    self, Catalogue, Endpoints, Manifest, ModelInfo, PullSpec, Request, Response, Settings,
    Status, Store, PROTOCOL_VERSION,
};
use rimed_core::aiprobe;
use rimed_core::gpu::RealNvidiaSmi;
use clap::{Args, Subcommand};

use crate::ops;

/// `rime ai <verb>`.
#[derive(Subcommand)]
pub enum AiCmd {
    /// Models in the shared store, and what the image's catalogue offers.
    ///
    /// Reads the store directly, so it works with the daemon stopped — "what
    /// have I got" must never require a running service. `--available` prints
    /// the curated catalogue instead: names, sizes, licences and digests, so a
    /// licence can be read before several gigabytes are downloaded.
    ///
    /// It deliberately does NOT search a remote index. The catalogue ships in
    /// the signed image, which is what makes a name-to-digest mapping worth
    /// trusting; fetching one would make the digest check prove nothing.
    Models {
        /// Print the image's catalogue rather than what is installed.
        #[arg(long)]
        available: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },

    /// Download a model into the shared store and verify it. Requires root.
    ///
    /// Root because the store is root-owned and world-readable: one copy of a
    /// multi-gigabyte file for every account, and no account able to alter
    /// another's weights — including the inference process itself, which runs
    /// as you.
    ///
    /// A bare name resolves through the image's catalogue. `--url` requires
    /// `--digest`, and a URL with no digest is refused: verifying a download
    /// against a digest the same server handed you proves only that it sent the
    /// same bytes twice.
    ///
    /// It never downloads twice. The store is content-addressed, so a model
    /// whose blob is already present is recorded and skipped.
    Pull {
        /// A catalogue name, optionally `name@sha256:<hex>` to assert what you
        /// expect.
        #[arg(value_name = "NAME")]
        name: String,
        /// Fetch from here instead of the catalogue. Requires --digest.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// The SHA-256 the download must match: `sha256:<64 hex>`.
        #[arg(long, value_name = "DIGEST")]
        digest: Option<String>,
        /// Print what would be fetched, verified and written, and do none of
        /// it. Performs no network access and no writes.
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a model from the shared store. Requires root.
    ///
    /// The weights are deleted only when no other model shares them. That is
    /// not a special case: the store is content-addressed, so two names for one
    /// file is the normal result of pinning a digest twice.
    Rm {
        #[arg(value_name = "NAME")]
        name: String,
    },

    /// Generate from a local model.
    ///
    /// Streams tokens as they arrive. The prompt is the positional arguments;
    /// anything piped in is appended as context, so `git diff | rime ai run
    /// "review this"` works.
    ///
    /// It starts the model if it is not resident and leaves it resident, which
    /// is the whole point of a service — the next question does not pay the
    /// load again. `rime ai status` says when it will unload.
    Run(RunArgs),

    /// Stop the resident model and release its VRAM now.
    ///
    /// The service unloads on its own timer — see `rime ai status` for the
    /// timeout in force — so this exists for the case the timer cannot serve:
    /// you want the VRAM back *now*, before starting a game or a render.
    ///
    /// It refuses while a client is attached rather than cutting a generation
    /// off mid-answer. The model reloads on the next request.
    ///
    /// It deliberately does NOT stop the service. A stopped daemon would also
    /// stop answering `rime ai status`, and "why is there no model loaded" is
    /// exactly the question you would then be unable to ask. Use
    /// `systemctl --user stop rime-aid` if that is what you mean.
    Unload,

    /// What the service decided, and what it would decide.
    ///
    /// Answers with the daemon when it is running and without it when it is
    /// not: the backend choice, the device, the fit and the store are all
    /// derived from one shared resolver, so the offline answer is the same
    /// answer. Read-only, so it needs no root.
    Status {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },

    /// Where applications should connect, and how to start the service.
    ///
    /// Prints the socket path and a request that works, because "one Rime
    /// local-inference API" is only usable if a program can find it.
    ///
    /// There is no `--listen`: the flag is accepted so the refusal can explain
    /// that a TCP port carries no peer credential and is therefore open to
    /// every account on the machine.
    Serve {
        /// **Refused.** See the message.
        #[arg(long, value_name = "ADDRESS")]
        listen: Option<String>,
        /// Run the service in this terminal instead of under systemd, for
        /// debugging. Its log goes to stderr.
        #[arg(long)]
        foreground: bool,
    },
}

/// `rime ai run` — the flags, separated from the plan they produce.
#[derive(Args, Clone, Debug, Default)]
pub struct RunArgs {
    /// The prompt. Everything after the verb.
    #[arg(value_name = "PROMPT", trailing_var_arg = true)]
    pub prompt: Vec<String>,
    /// Use this model rather than the selected or configured one.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// A system message.
    #[arg(long, value_name = "TEXT")]
    pub system: Option<String>,
    /// Stop after this many generated tokens.
    #[arg(long, value_name = "N")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature, 0.0-2.0. Absent leaves the model's own default.
    #[arg(long, value_name = "T")]
    pub temperature: Option<f32>,
    /// Print the whole response as JSON instead of streaming text.
    #[arg(long)]
    pub json: bool,
    /// Print the plan — model, backend, device, layers, context — and generate
    /// nothing.
    #[arg(long)]
    pub explain: bool,

    /// Run this on a trusted device's inference service instead (§20).
    ///
    /// The whole invocation is forwarded to that machine's own `rime ai run`,
    /// which selects its own backend and device against *its* hardware. That
    /// is the point of dispatching: a laptop asking a desktop to generate
    /// wants the desktop's plan, not its own.
    ///
    /// Nothing is uploaded but the prompt, and nothing is downloaded but the
    /// answer — the weights stay on the machine that has them.
    #[arg(long = "on", value_name = "HOST")]
    pub on: Option<String>,
}

impl RunArgs {
    /// Rebuild this invocation for a remote `rime ai run`.
    ///
    /// Reconstructed from the parsed struct rather than from
    /// `std::env::args`, so `--on` cannot leak into the remote command and
    /// make it dispatch again — and so a value clap normalised is forwarded in
    /// its normalised form.
    ///
    /// The prompt goes last and unflagged, matching `trailing_var_arg`.
    ///
    /// Includes the `ai` verb, because [`crate::dispatch::forward_to_host`]
    /// prepends only `rime`. Omitting it sent `rime run …` to the far side,
    /// which answered "unrecognized subcommand 'run'" — a confusing remote
    /// error for a local mistake, and the reason there is now a test asserting
    /// the first two elements.
    pub fn forward_argv(&self) -> Vec<String> {
        let mut out = vec!["ai".to_string(), "run".to_string()];
        if let Some(m) = &self.model {
            out.push("--model".into());
            out.push(m.clone());
        }
        if let Some(s) = &self.system {
            out.push("--system".into());
            out.push(s.clone());
        }
        if let Some(n) = self.max_tokens {
            out.push("--max-tokens".into());
            out.push(n.to_string());
        }
        if let Some(t) = self.temperature {
            out.push("--temperature".into());
            out.push(t.to_string());
        }
        if self.json {
            out.push("--json".into());
        }
        if self.explain {
            out.push("--explain".into());
        }
        out.extend(self.prompt.iter().cloned());
        out
    }
}

// ── the plan ─────────────────────────────────────────────────────────────────

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
}

/// Everything `rime ai run` will do, decided before anything is opened.
///
/// A value, not a closure over the arguments, so a test can assert its exact
/// contents and so a remote executor can be handed it unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct RunPlan {
    /// The model to ask for, when the command line named one.
    pub model: Option<String>,
    /// The messages, in order.
    pub messages: Vec<Message>,
    /// Token cap.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Whether to stream. Off for `--json`, because a caller parsing JSON wants
    /// one object and not a sequence of deltas.
    pub stream: bool,
    /// Print the plan and generate nothing.
    pub explain: bool,
}

/// The largest prompt this build will send.
///
/// 1 MiB. Not a model limit — the model has its own, and exceeding it is the
/// server's error to give — but a bound on what a mistyped pipe can do. `rime
/// ai run "summarise" < /dev/sda` should be a refusal, not a machine that spends
/// ten minutes tokenising a disk image.
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;

/// Turn the flags and whatever was piped in into a plan.
///
/// Pure and total: no environment, no filesystem, no clock. `stdin_text` is
/// `None` when stdin is a terminal, which is what makes `rime ai run "hello"`
/// not block waiting for input.
pub fn plan_run(args: &RunArgs, stdin_text: Option<&str>) -> Result<RunPlan, String> {
    if let Some(m) = &args.model {
        ai::validate_model_id(m).map_err(|e| e.to_string())?;
    }
    if let Some(t) = args.temperature {
        if !(0.0..=2.0).contains(&t) || !t.is_finite() {
            return Err(format!(
                "--temperature {t} is outside 0.0-2.0. 0 is deterministic, 0.8 is a common \
                 default, above 2 is noise"
            ));
        }
    }
    if args.max_tokens == Some(0) {
        return Err(
            "--max-tokens 0 would generate nothing. Leave it out for the model's own limit"
                .to_string(),
        );
    }

    let typed = args.prompt.join(" ").trim().to_string();
    let piped = stdin_text.map(str::trim).unwrap_or("").to_string();

    if typed.is_empty() && piped.is_empty() {
        return Err(
            "no prompt. Pass it as arguments — `rime ai run \"why is the sky blue\"` — or pipe \
             it in: `git diff | rime ai run \"review this\"`"
                .to_string(),
        );
    }

    // Typed first, piped second. The instruction has to precede the material
    // it is about, and a model told "review this" after ten thousand lines of
    // diff attends to it far less.
    let content = match (typed.is_empty(), piped.is_empty()) {
        (false, false) => format!("{typed}\n\n{piped}"),
        (false, true) => typed,
        (true, false) => piped,
        (true, true) => unreachable!("both empty was refused above"),
    };
    if content.len() > MAX_PROMPT_BYTES {
        return Err(format!(
            "the prompt is {} bytes, over the {MAX_PROMPT_BYTES}-byte limit this build sends. \
             That is usually a pipe that read something unintended",
            content.len()
        ));
    }

    let mut messages = Vec::new();
    if let Some(s) = &args.system {
        if !s.trim().is_empty() {
            messages.push(Message { role: "system", content: s.trim().to_string() });
        }
    }
    messages.push(Message { role: "user", content });

    Ok(RunPlan {
        model: args.model.clone(),
        messages,
        max_tokens: args.max_tokens,
        temperature: args.temperature,
        // `--json` turns streaming off: a caller piping into `jq` wants one
        // object, and reassembling deltas to produce it would be this client
        // doing the server's job.
        stream: !args.json,
        explain: args.explain,
    })
}

impl RunPlan {
    /// The exact JSON body that goes on the wire.
    ///
    /// Built with `serde_json`, never by formatting strings: a prompt contains
    /// quotes, backslashes and newlines by definition, and a hand-built body is
    /// how a prompt with a `"` in it becomes a parse error on the server.
    pub fn request_body(&self) -> String {
        let messages: Vec<serde_json::Value> = self
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        let mut body = serde_json::json!({
            "messages": messages,
            "stream": self.stream,
        });
        // `model` is sent when the command line named one so the server can
        // reject a mismatch. Which model is actually resident is the daemon's
        // decision, made through the control socket before this request.
        if let Some(m) = &self.model {
            body["model"] = serde_json::Value::String(m.clone());
        }
        if let Some(n) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        if let Some(t) = self.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body.to_string()
    }
}

// ── the HTTP client ──────────────────────────────────────────────────────────

/// The chat-completions path. OpenAI-compatible, which is what `llama-server`
/// and every other runtime worth abstracting serve.
pub const CHAT_PATH: &str = "/v1/chat/completions";

/// Build an HTTP/1.1 POST.
///
/// `Host: localhost` because the transport is a Unix socket and there is no
/// host; HTTP/1.1 requires the header regardless. `Connection: close` so the
/// end of the body is the end of the socket and no keep-alive state has to be
/// tracked.
pub fn build_post(path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         User-Agent: rime-ai\r\n\
         Accept: text/event-stream\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// What a response's headers said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Head {
    /// Status code.
    pub status: u16,
    /// `Transfer-Encoding: chunked`.
    pub chunked: bool,
    /// `Content-Length`, when given.
    pub content_length: Option<usize>,
}

/// Parse a response head, returning it and how many bytes it occupied.
///
/// `None` when the headers are not complete yet, which is the normal state on a
/// short first read — so a caller loops rather than concluding the response is
/// malformed.
///
/// Header names are matched case-insensitively because HTTP says they are, and
/// a server that sends `transfer-encoding` in lower case is not unusual.
pub fn parse_head(buf: &[u8]) -> Option<(Head, usize)> {
    let end = find(buf, b"\r\n\r\n")? + 4;
    let text = String::from_utf8_lossy(&buf[..end]);
    let mut lines = text.lines();
    let status_line = lines.next()?;
    // `HTTP/1.1 200 OK`
    let status = status_line.split_whitespace().nth(1)?.parse().ok()?;

    let mut head = Head { status, chunked: false, content_length: None };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "transfer-encoding" => {
                head.chunked = value.to_ascii_lowercase().contains("chunked");
            }
            "content-length" => head.content_length = value.parse().ok(),
            _ => {}
        }
    }
    Some((head, end))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Incremental decoder for `Transfer-Encoding: chunked`.
///
/// A state machine rather than a whole-body parse, because the point of
/// streaming is that the first token appears before the last one exists. Each
/// [`Chunked::feed`] returns whatever is now decodable and keeps the remainder.
#[derive(Debug, Clone, Default)]
pub struct Chunked {
    buf: Vec<u8>,
    /// Bytes still owed for the chunk being read. `None` between chunks, when
    /// the next thing expected is a size line.
    need: Option<usize>,
    done: bool,
}

impl Chunked {
    /// Feed bytes, get decoded body out.
    pub fn feed(&mut self, input: &[u8]) -> Result<Vec<u8>, String> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        loop {
            if self.done {
                return Ok(out);
            }
            match self.need {
                None => {
                    let Some(nl) = find(&self.buf, b"\r\n") else {
                        return Ok(out);
                    };
                    let line = String::from_utf8_lossy(&self.buf[..nl]).to_string();
                    // A chunk size may carry extensions after a ';'. They are
                    // legal and ignorable, and failing to strip one turns the
                    // hex parse into an error on a conforming server.
                    let hex = line.split(';').next().unwrap_or("").trim();
                    let size = usize::from_str_radix(hex, 16)
                        .map_err(|_| format!("not a chunk size: {line:?}"))?;
                    self.buf.drain(..nl + 2);
                    if size == 0 {
                        self.done = true;
                        return Ok(out);
                    }
                    self.need = Some(size);
                }
                Some(n) => {
                    // The chunk plus its trailing CRLF.
                    if self.buf.len() < n + 2 {
                        return Ok(out);
                    }
                    out.extend_from_slice(&self.buf[..n]);
                    self.buf.drain(..n + 2);
                    self.need = None;
                }
            }
        }
    }

    /// Whether the terminating zero-length chunk has arrived.
    pub fn done(&self) -> bool {
        self.done
    }
}

/// Splits a server-sent-events stream into `data:` payloads.
///
/// Line-buffered, because a `data:` line can be split across two reads at any
/// byte — including in the middle of a UTF-8 character, which is why the buffer
/// is bytes and the decode happens per complete line.
#[derive(Debug, Clone, Default)]
pub struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    /// Feed bytes, get complete `data:` payloads out. `[DONE]` is dropped.
    pub fn feed(&mut self, input: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            let rest = rest.trim();
            if rest.is_empty() || rest == "[DONE]" {
                continue;
            }
            out.push(rest.to_string());
        }
        out
    }
}

/// The text a streaming chunk adds, if any.
///
/// Three shapes are accepted because three exist in the wild and a client that
/// handled one would silently print nothing against the others:
/// `choices[0].delta.content` (OpenAI streaming), `choices[0].message.content`
/// (a non-streaming reply that arrived over the same path) and
/// `choices[0].text` (the completions endpoint).
pub fn delta_text(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let c = v.get("choices")?.get(0)?;
    for path in [["delta", "content"], ["message", "content"]] {
        if let Some(s) = c.get(path[0]).and_then(|d| d.get(path[1])).and_then(|s| s.as_str()) {
            return Some(s.to_string());
        }
    }
    c.get("text").and_then(|s| s.as_str()).map(str::to_string)
}

/// The reason a response carried instead of text, when it carried one.
pub fn error_text(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    for path in [["error", "message"], ["error", "type"]] {
        if let Some(s) = v.get(path[0]).and_then(|e| e.get(path[1])).and_then(|s| s.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

// ── the daemon client ────────────────────────────────────────────────────────

/// Where the daemon listens for this user.
fn endpoints() -> Endpoints {
    Endpoints::new(&runtime_dir())
}

/// `$XDG_RUNTIME_DIR`, or the conventional path. The CLI already links
/// `rime_agent_core`, so this is its helper rather than a fourth copy.
fn runtime_dir() -> PathBuf {
    rime_agent_core::paths::runtime_dir()
}

/// `~/.config/rime/ai.toml`.
fn settings_path() -> PathBuf {
    rime_agent_core::paths::config_home().join("rime/ai.toml")
}

/// Read the user's settings, or defaults, reporting a bad file rather than
/// hiding it.
fn load_settings() -> Settings {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => match Settings::parse(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rime: {} is not usable: {e}", path.display());
                eprintln!("rime: continuing with defaults");
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

/// The message printed when the service is not running.
///
/// The same shape `rime agent` uses, and for the same reason: the daemon is
/// opt-in, so "not running" is an ordinary state with one command as its
/// answer, not an error to diagnose.
fn not_running() -> String {
    "the local inference service is not running.\n  \
     Start it with:  systemctl --user enable --now rime-aid\n  \
     It is opt-in on purpose — a per-user daemon holding a multi-gigabyte model \
     resident should not start for people who never ask for one."
        .to_string()
}

/// Send one control request and read one reply.
fn ask(request: &Request) -> Result<Response, String> {
    let socket = endpoints().control();
    let mut stream = UnixStream::connect(&socket).map_err(|_| not_running())?;
    let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("writing to {}: {e}", socket.display()))?;
    stream.flush().ok();

    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    // Read exactly one line. A bounded loop, because the framing is line-based
    // and a daemon that never sends a newline must not hang the CLI forever.
    for _ in 0..(4 * 1024 * 1024) {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => reply.push(byte[0]),
            Err(e) => return Err(format!("reading from {}: {e}", socket.display())),
        }
    }
    let text = String::from_utf8_lossy(&reply);
    serde_json::from_str(&text).map_err(|e| format!("unparseable reply {text:?}: {e}"))
}

/// Ask for status, checking the protocol version first.
///
/// The handshake is not ceremony: a daemon left running across an OS update —
/// or an older one still alive after `bootc rollback` — is exactly the case
/// where a silently misread reply would produce a confident wrong answer.
fn ask_status() -> Result<Status, String> {
    match ask(&Request::Hello)? {
        Response::Hello { version, .. } if version != PROTOCOL_VERSION => {
            return Err(format!(
                "the running rime-aid speaks control protocol {version} and this rime speaks \
                 {PROTOCOL_VERSION}. Restart it: systemctl --user restart rime-aid"
            ))
        }
        Response::Hello { .. } => {}
        other => return Err(format!("unexpected reply to hello: {other:?}")),
    }
    match ask(&Request::Status)? {
        Response::Status(s) => Ok(*s),
        Response::Error { message, .. } => Err(message),
        other => Err(format!("unexpected reply to status: {other:?}")),
    }
}

// ── dispatch ─────────────────────────────────────────────────────────────────

/// `rime ai …`. Returns the process exit code.
pub fn main(cmd: AiCmd) -> i32 {
    match cmd {
        AiCmd::Models { available, json } => models(available, json),
        AiCmd::Pull { name, url, digest, dry_run } => {
            pull(&name, url.as_deref(), digest.as_deref(), dry_run)
        }
        AiCmd::Rm { name } => rm(&name),
        AiCmd::Run(args) => match &args.on {
            // §20. Checked before anything local happens, so a remote run
            // never starts a local backend as a side effect.
            Some(host) => {
                let h = host.clone();
                let argv = args.forward_argv();
                // A terminal only if there is one to forward: streaming to a
                // pipe must not make ssh warn, and --json is usually piped.
                let tty = if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 && !args.json {
                    rimed_core::host::Tty::Interactive
                } else {
                    rimed_core::host::Tty::None
                };
                match crate::dispatch::forward_to_host(
                    &h,
                    &argv,
                    tty,
                    Some(crate::dispatch::Capability::Ai),
                ) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("rime ai run: {e:#}");
                        crate::blueprint::EXIT_ERROR
                    }
                }
            }
            None => run(&args),
        },
        AiCmd::Unload => unload(),
        AiCmd::Status { json } => status(json),
        AiCmd::Serve { listen, foreground } => serve(listen.as_deref(), foreground),
    }
}

// ── models ───────────────────────────────────────────────────────────────────

/// Read the image's catalogue.
fn catalogue() -> Result<Catalogue, String> {
    let path = catalogue_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Catalogue::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The catalogue path, overridable only for the test suite — the same shape
/// `RIME_AI_STORE` has, and for the same reason: the suite must be able to
/// describe a catalogue without an image build.
fn catalogue_path() -> PathBuf {
    match std::env::var_os("RIME_AI_CATALOGUE") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(ai::CATALOGUE_PATH),
    }
}

fn store() -> Store {
    aiprobe::store_from_env()
}

fn models(available: bool, json: bool) -> i32 {
    if available {
        let cat = match catalogue() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rime: no model catalogue: {e}");
                return 1;
            }
        };
        if json {
            match serde_json::to_string_pretty(&cat) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("rime: {e}");
                    return 1;
                }
            }
            return 0;
        }
        if cat.model.is_empty() {
            println!("The catalogue in this image lists no models.");
            return 0;
        }
        println!("Available (digests ship in the signed image):");
        for (id, e) in &cat.model {
            println!(
                "  {id:<20} {:>7} MiB  {:<8} {:<14} {}",
                e.weights_mib, e.quant, e.license, e.title
            );
            println!("  {:<20} {}", "", e.digest);
            if let Some(n) = &e.note {
                println!("  {:<20} {n}", "");
            }
        }
        println!("\n  sudo rime ai pull <name>");
        return 0;
    }

    // Installed. Read the store directly, so this works with no daemon — and
    // ask the daemon only for which model is selected and resident.
    let st = ask_status().ok();
    let list = match aiprobe::model_infos(
        &store(),
        st.as_ref().and_then(|s| s.selected.as_deref()),
        st.as_ref().and_then(|s| s.loaded.as_deref()),
    ) {
        Ok(l) => l,
        Err(e) => {
            // Never the same message an empty store prints: "no models are
            // installed" would be a claim about a directory this never read.
            eprintln!("rime: {e} — installed models are unknown, not necessarily absent");
            return 1;
        }
    };
    if json {
        match serde_json::to_string_pretty(&list) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("rime: {e}");
                return 1;
            }
        }
        return 0;
    }
    if list.is_empty() {
        println!("No models are installed in {}.", store().root().display());
        println!("  rime ai models --available     what this image offers");
        println!("  sudo rime ai pull <name>       install one");
        return 0;
    }
    print_models(&list);
    0
}

fn print_models(list: &[ModelInfo]) {
    println!("{:<20} {:>9}  {:<10} {:<8} STATE", "MODEL", "SIZE", "RUNTIME", "CONTEXT");
    for m in list {
        let mut state = Vec::new();
        if m.loaded {
            state.push("loaded".to_string());
        }
        if m.selected {
            state.push("selected".to_string());
        }
        // Checked first: a refused blob stat is not the same finding as a
        // confirmed-missing one, and must never print as "WEIGHTS MISSING" —
        // that tells the user to re-pull, which does nothing for a
        // permission problem.
        if let Some(why) = &m.present_unavailable {
            state.push(format!("weights unavailable: {why}"));
        } else if !m.present {
            state.push("WEIGHTS MISSING".to_string());
        }
        if m.user_supplied_digest {
            state.push("digest: yours".to_string());
        }
        println!(
            "{:<20} {:>5} MiB  {:<10} {:<8} {}",
            m.id,
            m.weights_mib,
            m.runtime,
            m.max_context,
            state.join(", ")
        );
    }
}

// ── pull ─────────────────────────────────────────────────────────────────────

fn pull(name: &str, url: Option<&str>, digest: Option<&str>, dry_run: bool) -> i32 {
    // The spec and the plan are decided before root is required, so a typo or a
    // missing digest is a refusal that costs no password.
    let spec = match ai::parse_pull_spec(name, url, digest) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    // A catalogue is needed only for a name; an explicit URL carries its own
    // source, so a machine with no catalogue can still pull one.
    let cat = match &spec {
        PullSpec::Url { .. } => catalogue().unwrap_or_default(),
        _ => match catalogue() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rime: no model catalogue: {e}");
                eprintln!(
                    "rime: pull an explicit source instead:\n    \
                     sudo rime ai pull <name> --url <https://…> --digest sha256:<64 hex>"
                );
                return 1;
            }
        },
    };
    let store = store();
    let plan = match ai::plan_pull(&spec, &cat, &store) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };

    if dry_run {
        println!("Would pull {}:", plan.id);
        println!("  from     {}", plan.url);
        println!("  digest   {}", plan.digest);
        if plan.weights_mib > 0 {
            println!("  size     {} MiB", plan.weights_mib);
        }
        println!("  staging  {}", plan.staging.display());
        println!("  blob     {}", plan.blob.display());
        println!("  manifest {}", plan.manifest.display());
        for n in &plan.notes {
            println!("  - {n}");
        }
        if plan.blob.exists() {
            println!("  (the weights are already in the store; only the manifest would change)");
        }
        return 0;
    }

    if ops::require_root("ai pull").is_err() {
        return 1;
    }

    for (dir, mode) in [
        (store.root().to_path_buf(), 0o755),
        (store.models_dir(), 0o755),
        (store.blobs_dir(), 0o755),
        (store.manifests_dir(), 0o755),
        // Staging is 0700: a partially written blob is not even readable by the
        // account that will eventually use it.
        (store.staging_dir(), 0o700),
    ] {
        if let Err(e) = mkdir_mode(&dir, mode) {
            eprintln!("rime: cannot create {}: {e}", dir.display());
            return 1;
        }
    }

    // Content-addressed, so a blob that is already present and already correct
    // is not downloaded again. Verified rather than assumed: a file with the
    // right name and the wrong contents is exactly what a partial write under a
    // previous version would leave.
    let have = plan.blob.exists()
        && match sha256(&plan.blob) {
            Ok(d) => d == plan.digest,
            Err(_) => false,
        };
    if have {
        println!("rime: {} is already in the store; recording the manifest", plan.digest);
    } else {
        if plan.blob.exists() {
            eprintln!(
                "rime: {} exists but does not match its own name — removing it",
                plan.blob.display()
            );
            let _ = std::fs::remove_file(&plan.blob);
        }
        println!("rime: fetching {} -> {}", plan.url, plan.staging.display());
        if let Err(e) = fetch(&plan.url, &plan.staging) {
            eprintln!("rime: {e}");
            let _ = std::fs::remove_file(&plan.staging);
            return 1;
        }
        match sha256(&plan.staging) {
            Ok(got) if got == plan.digest => {}
            Ok(got) => {
                eprintln!(
                    "rime: digest mismatch — refusing.\n  expected {}\n  got      {got}\n\
                     Nothing was installed and the download has been deleted.",
                    plan.digest
                );
                let _ = std::fs::remove_file(&plan.staging);
                return 1;
            }
            Err(e) => {
                eprintln!("rime: cannot verify the download: {e}");
                let _ = std::fs::remove_file(&plan.staging);
                return 1;
            }
        }
        // 0444 BEFORE the rename, so the file is never briefly writable under
        // its final name.
        if let Err(e) = chmod(&plan.staging, 0o444) {
            eprintln!("rime: cannot set the mode on {}: {e}", plan.staging.display());
            return 1;
        }
        if let Err(e) = std::fs::rename(&plan.staging, &plan.blob) {
            eprintln!(
                "rime: cannot install {} -> {}: {e}",
                plan.staging.display(),
                plan.blob.display()
            );
            return 1;
        }
    }

    let entry = cat.get(&plan.id);
    let manifest = Manifest {
        version: ai::SCHEMA_VERSION,
        id: plan.id.clone(),
        digest: plan.digest.clone(),
        weights_mib: entry.map(|e| e.weights_mib).unwrap_or_else(|| {
            std::fs::metadata(&plan.blob)
                .map(|m| m.len() / (1024 * 1024))
                .unwrap_or(0)
        }),
        layers: entry.map(|e| e.layers).unwrap_or(0),
        kv_mib_per_1k: entry.map(|e| e.kv_mib_per_1k).unwrap_or(0),
        max_context: entry.map(|e| e.max_context).unwrap_or(0),
        runtime: entry
            .map(|e| e.runtime.clone())
            .unwrap_or_else(|| ai::Runtime::LlamaCpp.as_str().to_string()),
        url: Some(plan.url.clone()),
        pulled_at: unix_now(),
        user_supplied_digest: plan.user_supplied_digest,
        unknown: Default::default(),
    };
    let text = match manifest.to_json() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("rime: cannot render the manifest: {e}");
            return 1;
        }
    };
    if let Err(e) = write_readonly(&plan.manifest, &text) {
        eprintln!("rime: cannot write {}: {e}", plan.manifest.display());
        return 1;
    }

    println!("rime: {} installed ({})", plan.id, plan.digest);
    if manifest.layers == 0 {
        println!(
            "rime: this model is not in the catalogue, so its layer count and KV cost are \
             unknown. It will load, but `rime ai status` cannot plan a partial offload for it."
        );
    }
    println!("rime: `rime ai run --model {}` to use it", plan.id);
    0
}

/// Download a URL to a path.
///
/// `curl`, not a Rust HTTP client, and the reason is not effort: `curl` is
/// already a hard dependency asserted in `Containerfile.base` for the package
/// engine, it handles redirects, resume, proxies and the system CA store the
/// way the rest of the machine does, and adding an HTTP-plus-TLS stack to
/// `rime` to fetch one file would be a new attack surface in the CLI for no
/// gain.
fn fetch(url: &str, to: &Path) -> Result<(), String> {
    // `--` before the URL. `url` has been through `validate_url`, so it cannot
    // begin with `-`, and this is the second line at the argv boundary — the
    // same rule `rimed_core::host::ssh_argv` follows.
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--output",
        ])
        .arg(to)
        .arg("--")
        .arg(url)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed ({status}) fetching {url}"));
    }
    Ok(())
}

/// `sha256:<hex>` of a file.
///
/// Shelled out to `sha256sum`, which `Containerfile.base` already asserts for
/// the package engine. The alternative is a SHA-256 implementation inside
/// `rime`, and a hand-rolled hash on the verification path of untrusted
/// downloads is a worse idea than a subprocess.
fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg("--binary")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|e| format!("cannot run sha256sum: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "sha256sum failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let hex = text
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256sum printed nothing".to_string())?;
    let digest = format!("sha256:{}", hex.to_ascii_lowercase());
    ai::validate_digest(&digest).map_err(|e| e.to_string())?;
    Ok(digest)
}

fn mkdir_mode(dir: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    let mut perms = std::fs::metadata(dir)?.permissions();
    if perms.mode() & 0o777 != mode {
        perms.set_mode(mode);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Write a file and make it read-only.
///
/// The write goes to a temporary name in the same directory and is renamed, so
/// a manifest is never half-written under its real name — the same rule the
/// blob follows, one file along.
fn write_readonly(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, text)?;
    chmod(&tmp, 0o444)?;
    std::fs::rename(&tmp, path)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── rm ───────────────────────────────────────────────────────────────────────

fn rm(name: &str) -> i32 {
    let store = store();
    let manifest_path = match store.manifest(name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime: {e}");
            return 1;
        }
    };
    let listed = match aiprobe::installed(&store) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rime: {e} — installed models are unknown, not necessarily absent");
            return 1;
        }
    };
    let Some((target, _)) = listed.iter().find(|(m, _)| m.id == name) else {
        eprintln!(
            "rime: no model named {name:?} in {}. `rime ai models` lists what is installed",
            store.root().display()
        );
        return 1;
    };
    let digest = target.digest.clone();

    if ops::require_root("ai rm").is_err() {
        return 1;
    }

    // The blob goes only when nothing else names it. The store is
    // content-addressed, so two ids sharing one file is the normal result of
    // pulling the same weights under two names — deleting it for the first
    // would silently break the second.
    let shared: Vec<&str> = listed
        .iter()
        .filter(|(m, _)| m.digest == digest && m.id != name)
        .map(|(m, _)| m.id.as_str())
        .collect();

    if let Err(e) = std::fs::remove_file(&manifest_path) {
        eprintln!("rime: cannot remove {}: {e}", manifest_path.display());
        return 1;
    }
    if shared.is_empty() {
        match store.blob(&digest) {
            Ok(blob) => {
                if let Err(e) = std::fs::remove_file(&blob) {
                    // Not fatal: the model is gone as far as the user is
                    // concerned, and a leftover blob is recovered by the next
                    // pull of the same digest.
                    eprintln!("rime: {name} removed, but {} remains: {e}", blob.display());
                    return 0;
                }
            }
            Err(e) => eprintln!("rime: {name} removed, but its blob path is unusable: {e}"),
        }
        println!("rime: {name} removed, and its weights with it");
    } else {
        println!(
            "rime: {name} removed. Its weights stay, because {} still uses them",
            shared.join(", ")
        );
    }
    0
}

// ── run ──────────────────────────────────────────────────────────────────────

fn run(args: &RunArgs) -> i32 {
    let piped = read_piped_stdin();
    let plan = match plan_run(args, piped.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rime: {e}");
            return 2;
        }
    };
    run_local(&plan)
}

/// Read stdin, but only when it is not a terminal.
///
/// Without the check, `rime ai run "hello"` would block forever waiting for
/// input nobody is going to type — the single most common way a CLI that
/// accepts piped input becomes unusable interactively.
fn read_piped_stdin() -> Option<String> {
    // Safe: isatty inspects a file descriptor and has no side effects.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return None;
    }
    let mut text = String::new();
    // Bounded: `plan_run` refuses an over-long prompt, but reading an unbounded
    // stream into memory first would defeat that. One byte over the limit is
    // enough to detect it.
    let mut limited = std::io::stdin().take((MAX_PROMPT_BYTES + 1) as u64);
    match limited.read_to_string(&mut text) {
        Ok(0) => None,
        Ok(_) => Some(text),
        Err(_) => None,
    }
}

/// Execute a plan against the local service.
///
/// Separated from [`plan_run`] so that a remote executor — `rime ai run --on
/// desktop`, which is not this branch's work — replaces only this function.
pub fn run_local(plan: &RunPlan) -> i32 {
    // Selecting the model is a control-socket request and must happen before
    // the API connection: the API endpoint has no handshake by design, so the
    // daemon loads whatever is selected when the first byte arrives.
    if let Some(model) = &plan.model {
        match ask(&Request::Select { model: model.clone() }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { message, .. }) => {
                eprintln!("rime: {message}");
                return 1;
            }
            Ok(other) => {
                eprintln!("rime: unexpected reply selecting {model}: {other:?}");
                return 1;
            }
            Err(e) => {
                eprintln!("rime: {e}");
                return 1;
            }
        }
    }

    if plan.explain {
        return explain(plan);
    }

    let socket = endpoints().api();
    let mut stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("rime: {}", not_running());
            return 1;
        }
    };

    let body = plan.request_body();
    if let Err(e) = stream.write_all(build_post(CHAT_PATH, &body).as_bytes()) {
        eprintln!("rime: cannot send the request: {e}");
        return 1;
    }
    let _ = stream.flush();

    stream_response(&mut stream, plan)
}

/// Read, decode and print a response.
///
/// Split out so the framing logic is exercised by the unit tests through the
/// pure pieces it composes, and so the socket handling is one place.
fn stream_response(stream: &mut UnixStream, plan: &RunPlan) -> i32 {
    let mut raw: Vec<u8> = Vec::new();
    let mut head: Option<Head> = None;
    let mut chunked = Chunked::default();
    let mut sse = Sse::default();
    let mut body = Vec::new();
    let mut printed_any = false;
    let mut buf = [0u8; 16 * 1024];
    let mut out = std::io::stdout();

    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("\nrime: the connection failed: {e}");
                return 1;
            }
        };
        if head.is_none() {
            raw.extend_from_slice(&buf[..n]);
            let Some((h, used)) = parse_head(&raw) else {
                continue;
            };
            head = Some(h);
            let rest: Vec<u8> = raw.split_off(used);
            raw.clear();
            if let Err(e) = consume(
                &rest, &h, &mut chunked, &mut sse, &mut body, plan, &mut printed_any, &mut out,
            ) {
                eprintln!("\nrime: {e}");
                return 1;
            }
            // A short answer can arrive head-and-all in one read, so the
            // end-of-body check belongs here too. Without it the whole response
            // would be printed and then the command would block.
            if h.chunked && chunked.done() {
                break;
            }
            continue;
        }
        let h = head.expect("checked");
        if let Err(e) = consume(
            &buf[..n], &h, &mut chunked, &mut sse, &mut body, plan, &mut printed_any, &mut out,
        ) {
            eprintln!("\nrime: {e}");
            return 1;
        }
        // A chunked body ends at its zero-length chunk, not at the close of the
        // socket. `Connection: close` asks the server to close, but a server
        // that ignores the header — or a relay that keeps the pair open — would
        // otherwise leave this blocked in `read` after the answer had already
        // finished printing, which reads as a hung command. The terminating
        // chunk is the authoritative end of the response, so it is what ends
        // the loop.
        if h.chunked && chunked.done() {
            break;
        }
    }

    let Some(h) = head else {
        eprintln!("rime: the service closed the connection without a response");
        return 1;
    };

    if h.status != 200 {
        let text = String::from_utf8_lossy(&body);
        let reason = error_text(&text).unwrap_or_else(|| text.trim().to_string());
        eprintln!(
            "rime: the inference service returned {} — {}",
            h.status,
            if reason.is_empty() { "no detail" } else { &reason }
        );
        return 1;
    }

    if !plan.stream {
        let text = String::from_utf8_lossy(&body);
        println!("{}", text.trim());
        return 0;
    }
    if printed_any {
        // A newline the model did not send, so the shell prompt starts on its
        // own line. Only when something was printed, so an empty answer does
        // not gain a blank line.
        println!();
        0
    } else {
        eprintln!("rime: the model produced no output");
        1
    }
}

/// Feed one read into the decoders and print whatever it produced.
#[allow(clippy::too_many_arguments)]
fn consume(
    input: &[u8],
    head: &Head,
    chunked: &mut Chunked,
    sse: &mut Sse,
    body: &mut Vec<u8>,
    plan: &RunPlan,
    printed_any: &mut bool,
    out: &mut std::io::Stdout,
) -> Result<(), String> {
    let decoded = if head.chunked {
        chunked.feed(input)?
    } else {
        input.to_vec()
    };
    if !plan.stream || head.status != 200 {
        // Collected whole: a caller asked for one object, or this is an error
        // body that has to be readable in full before it means anything.
        body.extend_from_slice(&decoded);
        return Ok(());
    }
    for data in sse.feed(&decoded) {
        if let Some(text) = delta_text(&data) {
            if text.is_empty() {
                continue;
            }
            *printed_any = true;
            let _ = out.write_all(text.as_bytes());
            // Flushed per token: the point of streaming is that it appears.
            let _ = out.flush();
        }
    }
    Ok(())
}

/// `rime ai run --explain` — the plan, and nothing generated.
fn explain(plan: &RunPlan) -> i32 {
    println!("Request:");
    println!("  endpoint  {}", endpoints().api().display());
    println!("  path      {CHAT_PATH}");
    println!("  streaming {}", plan.stream);
    for m in &plan.messages {
        let head: String = m.content.chars().take(120).collect();
        println!(
            "  {:<9} {}{}",
            m.role,
            head.replace('\n', " "),
            if m.content.chars().count() > 120 { " …" } else { "" }
        );
    }
    println!("  body      {} bytes", plan.request_body().len());
    println!();
    status(false)
}

// ── unload ───────────────────────────────────────────────────────────────────

/// `rime ai unload` — release the VRAM now rather than on the idle timer.
///
/// A no-op when nothing is loaded, and it says so rather than reporting
/// success: "there was nothing to unload" and "the model has been unloaded"
/// are different facts, and a user freeing VRAM before starting a game wants
/// to know which one happened.
fn unload() -> i32 {
    // Asked first, so the "nothing was loaded" case can be distinguished from
    // the "it has been stopped" one. Both go through the same request; only
    // the wording differs.
    let was = ask_status().ok().and_then(|s| s.loaded);
    match ask(&Request::Unload) {
        Ok(Response::Ok) => {
            match was {
                Some(model) => println!("rime: {model} unloaded; its VRAM is free"),
                None => println!("rime: nothing was loaded"),
            }
            0
        }
        Ok(Response::Error { message, .. }) => {
            eprintln!("rime: {message}");
            1
        }
        Ok(other) => {
            eprintln!("rime: unexpected reply to unload: {other:?}");
            1
        }
        Err(e) => {
            eprintln!("rime: {e}");
            1
        }
    }
}

// ── status ───────────────────────────────────────────────────────────────────

fn status(json: bool) -> i32 {
    match ask_status() {
        Ok(s) => {
            if json {
                match serde_json::to_string_pretty(&s) {
                    Ok(t) => println!("{t}"),
                    Err(e) => {
                        eprintln!("rime: {e}");
                        return 1;
                    }
                }
                return 0;
            }
            print_status(&s);
            0
        }
        Err(e) => {
            // The daemon is not running. Report what can be determined without
            // it — through the SAME resolver the daemon uses, so this is the
            // real answer and not an approximation of it.
            let offline = offline_status();
            if json {
                match serde_json::to_string_pretty(&offline) {
                    Ok(t) => println!("{t}"),
                    Err(e) => {
                        eprintln!("rime: {e}");
                        return 1;
                    }
                }
            } else {
                println!("service: not running");
                print_status(&offline);
                println!("\n{e}");
            }
            // Not a failure: the daemon being opt-in means "not running" is an
            // ordinary state, and a non-zero exit would make `rime ai status`
            // unusable in a shell prompt or a health check.
            0
        }
    }
}

/// The status a machine can report with no daemon.
///
/// Everything except the live fields — idle time, attached clients, what is
/// resident — because those exist only inside a running daemon. The plan comes
/// from `rimed_core::aiprobe::resolve`, which is exactly what the daemon calls,
/// so the backend and fit printed here are the ones that would be used.
fn offline_status() -> Status {
    let store = store();
    let roots = aiprobe::Roots::from_env();
    let settings = load_settings();
    let accel = aiprobe::accel(&roots);
    let devices = aiprobe::devices(&roots, &RealNvidiaSmi);
    let ends = endpoints();

    let mut st = Status {
        protocol: PROTOCOL_VERSION,
        api_socket: ends.api().display().to_string(),
        store: store.root().display().to_string(),
        selected: settings.model.clone(),
        accel: accel.available().iter().map(|b| b.as_str().to_string()).collect(),
        devices: devices.iter().map(ai::DeviceInfo::from).collect(),
        on_battery: aiprobe::on_battery(&roots),
        ..Default::default()
    };
    st.idle_timeout = ai::idle_timeout(settings.idle_timeout, st.on_battery);

    match aiprobe::resolve(&store, &roots, &settings, None, &ends, &RealNvidiaSmi) {
        Ok(r) => {
            st.runtime = Some(r.located.runtime.as_str().to_string());
            st.runtime_path = Some(r.located.path.display().to_string());
            st.backend = Some(r.choice.backend.as_str().to_string());
            st.device = r.choice.device;
            st.gpu_layers = Some(r.fit.placement.gpu_layers());
            st.total_layers = match r.fit.placement {
                ai::Placement::Split { total, .. } => Some(total),
                // A full offload of a model whose layer count is not declared
                // knows the placement, not the total. Reporting the sentinel as
                // the total would make `rime ai status` print a count nobody
                // measured.
                ai::Placement::Gpu { layers } if layers == ai::ALL_LAYERS_UNKNOWN => None,
                ai::Placement::Gpu { layers } => Some(layers),
                ai::Placement::Cpu => None,
            };
            st.context = Some(r.fit.context);
            st.vram_mib = Some(r.fit.vram_mib);
            st.notes.extend(r.notes);
        }
        Err(e) => {
            st.notes.push(e);
            if let Some(l) = aiprobe::locate_any() {
                st.runtime = Some(l.runtime.as_str().to_string());
                st.runtime_path = Some(l.path.display().to_string());
            } else {
                let backend = ai::select_backend(&accel, &devices, None)
                    .map(|c| c.backend)
                    .unwrap_or(ai::Backend::Cpu);
                st.install_hint = Some(ai::Runtime::LlamaCpp.install_hint(backend));
            }
        }
    }
    st
}

/// The `fit:` line of `rime ai status`.
///
/// Pure, so the wording is exercised by the unit tests rather than by reading
/// it — the same split the response framing above uses.
///
/// `ALL_LAYERS_UNKNOWN` is the `-ngl` value that means "all of them" for a model
/// whose manifest declares no layer count. It is the right thing to PASS to
/// llama-server, which clamps it, and the wrong thing to PRINT: "999 layer(s)
/// on the GPU" is the same class of lie as the "all 1 layers fit" this branch
/// set out to fix, just in the other direction. So the sentinel is named here
/// rather than rendered.
fn fit_line(layers: u32, total: Option<u32>, ctx: u32, vram_mib: Option<u64>) -> String {
    let placed = if layers == ai::ALL_LAYERS_UNKNOWN {
        "every layer (this model declares no layer count)".to_string()
    } else {
        format!(
            "{layers} layer(s){}",
            match total {
                Some(t) if t != layers => format!(" of {t}"),
                _ => String::new(),
            }
        )
    };
    format!(
        "{placed} on the GPU, {ctx}-token context{}",
        match vram_mib {
            Some(v) if v > 0 => format!(", {v} MiB of VRAM"),
            _ => String::new(),
        }
    )
}

fn print_status(s: &Status) {
    println!("endpoint:  {}", s.api_socket);
    println!("store:     {}", s.store);
    println!(
        "models:    selected {}, loaded {}",
        s.selected.as_deref().unwrap_or("(none)"),
        s.loaded.as_deref().unwrap_or("(none)")
    );
    println!(
        "runtime:   {} {}",
        s.runtime.as_deref().unwrap_or("(none installed)"),
        s.runtime_path.as_deref().unwrap_or("")
    );
    if let Some(hint) = &s.install_hint {
        println!("           install one with:  {hint}");
    }
    println!(
        "backend:   {}{}",
        s.backend.as_deref().unwrap_or("(undecided)"),
        match s.device {
            Some(i) => format!(" on device {i}"),
            None => String::new(),
        }
    );
    println!(
        "available: {}",
        if s.accel.is_empty() {
            "cpu".to_string()
        } else {
            s.accel.join(", ")
        }
    );
    for d in &s.devices {
        println!(
            "  device {} {} — {} MiB total, {} used, {} spendable",
            d.index, d.name, d.total_mib, d.used_mib, d.budget_mib
        );
    }
    if let (Some(layers), Some(ctx)) = (s.gpu_layers, s.context) {
        println!("fit:       {}", fit_line(layers, s.total_layers, ctx, s.vram_mib));
    }
    println!(
        "idle:      {}s of {}s{}",
        s.idle_secs,
        s.idle_timeout,
        if s.on_battery { " (on battery)" } else { "" }
    );
    if s.open_connections > 0 {
        println!("clients:   {}", s.open_connections);
    }
    if !s.notes.is_empty() {
        println!("why:");
        for n in &s.notes {
            println!("  - {n}");
        }
    }
}

// ── serve ────────────────────────────────────────────────────────────────────

/// The daemon binary. A constant so the CLI and the unit cannot disagree.
pub const DAEMON: &str = rime_paths::RIME_AID;

fn serve(listen: Option<&str>, foreground: bool) -> i32 {
    if let Some(addr) = listen {
        eprintln!("rime: --listen {addr:?} is refused.\n\n{}", ai::refuse_tcp_endpoint("listen"));
        return 1;
    }
    if foreground {
        let program = std::env::var_os("RIME_AI_DAEMON")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DAEMON));
        match Command::new(&program).status() {
            Ok(s) => return s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("rime: cannot run {}: {e}", program.display());
                return 1;
            }
        }
    }

    let ends = endpoints();
    println!("The Rime local inference endpoint is a Unix socket:");
    println!("  {}", ends.api().display());
    println!();
    println!("It speaks the runtime's own OpenAI-compatible HTTP API, so anything that can");
    println!("talk to such a server can talk to this — including agent clients:");
    println!();
    println!(
        "  curl --unix-socket {} \\\n    -H 'Content-Type: application/json' \\\n    \
         -d '{{\"messages\":[{{\"role\":\"user\",\"content\":\"hello\"}}]}}' \\\n    \
         http://localhost{CHAT_PATH}",
        ends.api().display()
    );
    println!();
    println!("There is no TCP port, and no option adds one: a TCP connection carries no peer");
    println!("credential, so a listener on 127.0.0.1 would be open to every account on this");
    println!("machine. To reach the service from another machine, use `rime host`'s ssh");
    println!("transport:  rime ai run --on <device> \"…\"");
    println!();
    // The stated limitation, printed where the person who hits it is standing.
    // A client whose only setting is `base_url = http://host:port` has nowhere
    // to put a socket path, and pretending otherwise would make them discover
    // it by failure. Rime ships no bridge because a bridge restores exactly the
    // exposure above — so the command that makes that trade is printed with its
    // cost attached, rather than wrapped in an `rime` verb that would imply it
    // was safe.
    println!("If a tool accepts only a base URL and cannot be given a socket path, there is no");
    println!("Rime verb for that on purpose. You can bridge it yourself:");
    println!();
    println!(
        "  socat TCP-LISTEN:11434,bind=127.0.0.1,reuseaddr,fork UNIX-CONNECT:{}",
        ends.api().display()
    );
    println!();
    println!("Understand what that costs before you run it: while socat is up, every account");
    println!("on this machine — and every sandboxed application with network access — can send");
    println!("prompts through your model and read the answers. Nothing distinguishes them from");
    println!("you, because a TCP connection carries nothing to distinguish them by.");
    println!();
    if UnixStream::connect(ends.control()).is_ok() {
        println!("The service is running.");
    } else {
        println!("The service is not running. Start it with:");
        println!("  systemctl --user enable --now rime-aid");
    }
    0
}

#[cfg(test)]
mod tests {

    // ── the fit: line ────────────────────────────────────────────────────────

    #[test]
    fn an_undeclared_layer_count_is_not_printed_as_a_number() {
        // ALL_LAYERS_UNKNOWN is the right thing to hand llama-server, which
        // clamps it, and the wrong thing to show a person: "999 layer(s) on the
        // GPU" is the same class of lie as the "all 1 layers fit" that the
        // planner fix removed, only in the other direction.
        let line = fit_line(ai::ALL_LAYERS_UNKNOWN, None, 8192, Some(4096));
        assert!(line.contains("every layer"), "{line}");
        assert!(line.contains("declares no layer count"), "{line}");
        assert!(
            !line.contains(&ai::ALL_LAYERS_UNKNOWN.to_string()),
            "the sentinel leaked into the output: {line}"
        );
        // The rest of the line is unchanged.
        assert!(line.contains("8192-token context"), "{line}");
        assert!(line.contains("4096 MiB of VRAM"), "{line}");
    }

    #[test]
    fn a_declared_layer_count_still_prints_as_a_number() {
        // The negative control: the sentinel branch must not have swallowed the
        // ordinary case.
        let full = fit_line(32, Some(32), 8192, Some(4096));
        assert!(full.contains("32 layer(s) on the GPU"), "{full}");
        assert!(!full.contains(" of 32"), "a full offload names no total: {full}");

        let split = fit_line(20, Some(32), 4096, Some(3000));
        assert!(split.contains("20 layer(s) of 32 on the GPU"), "{split}");
    }
    use super::*;

    #[test]
    fn a_forwarded_run_names_the_ai_verb_before_run() {
        // forward_to_host prepends only `rime`, so this must carry `ai run`.
        // Without the `ai` the far side received `rime run …` and answered
        // "unrecognized subcommand 'run'".
        let args = RunArgs {
            prompt: vec!["hello".into()],
            model: None,
            system: None,
            max_tokens: None,
            temperature: None,
            json: false,
            explain: false,
            on: Some("katana".into()),
        };
        let argv = args.forward_argv();
        assert_eq!(argv[0], "ai");
        assert_eq!(argv[1], "run");
    }

    #[test]
    fn the_on_flag_does_not_leak_into_the_forwarded_command() {
        // If it did, the remote would try to dispatch again — to itself, or
        // onward — and the prompt would never be answered.
        let args = RunArgs {
            prompt: vec!["hi".into()],
            model: Some("qwen3".into()),
            system: None,
            max_tokens: Some(64),
            temperature: None,
            json: true,
            explain: false,
            on: Some("katana".into()),
        };
        let argv = args.forward_argv();
        assert!(!argv.iter().any(|a| a == "--on" || a == "katana"), "{argv:?}");
        assert!(argv.contains(&"--model".to_string()));
        assert!(argv.contains(&"qwen3".to_string()));
        assert!(argv.contains(&"--json".to_string()));
        assert!(argv.contains(&"--max-tokens".to_string()));
        // The prompt is last and unflagged.
        assert_eq!(argv.last().unwrap(), "hi");
    }

    #[test]
    fn absent_options_are_not_forwarded_as_defaults() {
        // Absent means unmanaged, so the remote applies its own default rather
        // than one this side invented.
        let args = RunArgs {
            prompt: vec!["hi".into()],
            model: None,
            system: None,
            max_tokens: None,
            temperature: None,
            json: false,
            explain: false,
            on: Some("k".into()),
        };
        assert_eq!(args.forward_argv(), vec!["ai", "run", "hi"]);
    }

    fn args(prompt: &[&str]) -> RunArgs {
        RunArgs {
            prompt: prompt.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    // ── plan_run: the split a remote executor depends on ─────────────────────

    #[test]
    fn a_typed_prompt_becomes_one_user_message() {
        let p = plan_run(&args(&["why", "is", "the", "sky", "blue"]), None).unwrap();
        assert_eq!(
            p.messages,
            vec![Message { role: "user", content: "why is the sky blue".into() }]
        );
        assert!(p.stream, "streaming is the default");
        assert_eq!(p.model, None);
    }

    #[test]
    fn a_system_message_comes_first() {
        // Order is not cosmetic: a system message after the user turn is
        // treated as another turn by every chat template.
        let a = RunArgs { system: Some("be terse".into()), ..args(&["hello"]) };
        let p = plan_run(&a, None).unwrap();
        assert_eq!(p.messages.len(), 2);
        assert_eq!(p.messages[0].role, "system");
        assert_eq!(p.messages[1].role, "user");
    }

    #[test]
    fn piped_input_is_appended_after_the_instruction() {
        // `git diff | rime ai run "review this"` must put "review this" first.
        // A model told what to do after ten thousand lines of diff attends to
        // it far less.
        let p = plan_run(&args(&["review", "this"]), Some("diff --git a/x b/x")).unwrap();
        let content = &p.messages[0].content;
        assert!(content.starts_with("review this"), "{content}");
        assert!(content.ends_with("diff --git a/x b/x"), "{content}");
    }

    #[test]
    fn piped_input_alone_is_the_whole_prompt() {
        let p = plan_run(&args(&[]), Some("  summarise me  ")).unwrap();
        assert_eq!(p.messages[0].content, "summarise me");
    }

    #[test]
    fn no_prompt_at_all_is_refused_with_both_ways_to_give_one() {
        let e = plan_run(&args(&[]), None).unwrap_err();
        assert!(e.contains("rime ai run"), "{e}");
        assert!(e.contains("pipe it in"), "{e}");
        // Whitespace-only counts as none.
        assert!(plan_run(&args(&["  "]), Some("\n\n")).is_err());
    }

    #[test]
    fn an_over_long_prompt_is_refused_rather_than_sent() {
        // `rime ai run "x" < /dev/sda` should refuse, not tokenise a disk.
        let big = "a".repeat(MAX_PROMPT_BYTES + 1);
        let e = plan_run(&args(&[]), Some(&big)).unwrap_err();
        assert!(e.contains("over the"), "{e}");
        // And exactly at the limit is accepted.
        let ok = "a".repeat(MAX_PROMPT_BYTES);
        assert!(plan_run(&args(&[]), Some(&ok)).is_ok());
    }

    #[test]
    fn a_hostile_model_name_never_reaches_the_wire() {
        for bad in ["../../etc/shadow", "a/b", "-rf", "Qwen3"] {
            let a = RunArgs { model: Some(bad.into()), ..args(&["hi"]) };
            assert!(plan_run(&a, None).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn an_out_of_range_temperature_is_refused_with_the_range() {
        for t in [-0.1f32, 2.1, f32::NAN, f32::INFINITY] {
            let a = RunArgs { temperature: Some(t), ..args(&["hi"]) };
            assert!(plan_run(&a, None).is_err(), "{t} was accepted");
        }
        for t in [0.0f32, 0.8, 2.0] {
            let a = RunArgs { temperature: Some(t), ..args(&["hi"]) };
            assert!(plan_run(&a, None).is_ok(), "{t} was refused");
        }
    }

    #[test]
    fn zero_max_tokens_is_refused_because_it_would_generate_nothing() {
        let a = RunArgs { max_tokens: Some(0), ..args(&["hi"]) };
        assert!(plan_run(&a, None).unwrap_err().contains("generate nothing"));
    }

    #[test]
    fn json_turns_streaming_off() {
        // A caller piping into jq wants one object, and reassembling deltas to
        // produce it would be this client doing the server's job.
        let a = RunArgs { json: true, ..args(&["hi"]) };
        assert!(!plan_run(&a, None).unwrap().stream);
    }

    #[test]
    fn planning_is_pure() {
        // The property the remote executor rests on: one command line means one
        // plan, whatever else is happening.
        let a = RunArgs {
            model: Some("qwen3-coder".into()),
            system: Some("be terse".into()),
            max_tokens: Some(64),
            temperature: Some(0.2),
            ..args(&["hello"])
        };
        assert_eq!(plan_run(&a, Some("ctx")).unwrap(), plan_run(&a, Some("ctx")).unwrap());
    }

    // ── the request body ─────────────────────────────────────────────────────

    #[test]
    fn the_body_is_json_encoded_not_string_formatted() {
        // A prompt contains quotes, backslashes and newlines by definition.
        // A hand-built body is how a prompt with a `"` becomes a server-side
        // parse error.
        let p = plan_run(&args(&[]), Some("say \"hi\"\nand \\ that")).unwrap();
        let body = p.request_body();
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            v["messages"][0]["content"], "say \"hi\"\nand \\ that",
            "{body}"
        );
        assert_eq!(v["stream"], true);
    }

    #[test]
    fn optional_fields_are_absent_rather_than_null() {
        // A null the server does not expect is worse than an absent key: it
        // overrides the model's own default with nothing.
        let body = plan_run(&args(&["hi"]), None).unwrap().request_body();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("temperature").is_none(), "{body}");
        assert!(v.get("max_tokens").is_none(), "{body}");
        assert!(v.get("model").is_none(), "{body}");
    }

    #[test]
    fn every_flag_that_was_given_reaches_the_body() {
        let a = RunArgs {
            model: Some("qwen3-coder".into()),
            max_tokens: Some(64),
            temperature: Some(0.25),
            ..args(&["hi"])
        };
        let body = plan_run(&a, None).unwrap().request_body();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "qwen3-coder");
        assert_eq!(v["max_tokens"], 64);
        assert_eq!(v["temperature"], 0.25);
    }

    // ── HTTP framing ─────────────────────────────────────────────────────────

    #[test]
    fn the_request_declares_its_length_and_closes() {
        let body = "{\"a\":1}";
        let req = build_post(CHAT_PATH, body);
        assert!(req.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Content-Length: 7\r\n"), "{req}");
        assert!(req.contains("Connection: close\r\n"), "{req}");
        assert!(req.ends_with("\r\n\r\n{\"a\":1}"), "{req}");
        // HTTP/1.1 requires Host even over a Unix socket.
        assert!(req.contains("Host: localhost\r\n"), "{req}");
    }

    #[test]
    fn a_head_is_only_parsed_once_it_is_complete() {
        // The normal state on a short first read. Concluding "malformed" here
        // would break every response that arrives in two packets.
        assert!(parse_head(b"HTTP/1.1 200 OK\r\nContent-Len").is_none());
        assert!(parse_head(b"").is_none());
    }

    #[test]
    fn header_names_are_matched_case_insensitively() {
        // HTTP says they are, and lower-case senders are common.
        let (h, used) = parse_head(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\nX: y\r\n\r\nbody",
        )
        .unwrap();
        assert_eq!(h.status, 200);
        assert!(h.chunked);
        assert_eq!(&b"HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\nX: y\r\n\r\n"[..].len(), &used);
    }

    #[test]
    fn a_content_length_response_is_recognised_and_not_chunked() {
        let (h, _) =
            parse_head(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 12\r\n\r\n").unwrap();
        assert_eq!(h.status, 503);
        assert!(!h.chunked);
        assert_eq!(h.content_length, Some(12));
    }

    #[test]
    fn chunked_decoding_reassembles_a_body_split_at_every_byte() {
        // THE test for this decoder: a streaming parser that is subtly wrong
        // drops the last token of every answer and nothing notices. So the same
        // body is fed one byte at a time and must come out identical.
        let wire = b"5\r\nhello\r\n1\r\n \r\n6\r\nworld!\r\n0\r\n\r\n";
        let mut whole = Chunked::default();
        assert_eq!(whole.feed(wire).unwrap(), b"hello world!");
        assert!(whole.done());

        let mut byte_at_a_time = Chunked::default();
        let mut out = Vec::new();
        for b in wire {
            out.extend(byte_at_a_time.feed(&[*b]).unwrap());
        }
        assert_eq!(out, b"hello world!");
        assert!(byte_at_a_time.done());
    }

    #[test]
    fn a_chunk_extension_is_ignored_rather_than_failing_the_parse() {
        // Legal HTTP, and failing to strip it turns the hex parse into an error
        // on a conforming server.
        let mut c = Chunked::default();
        assert_eq!(c.feed(b"5;foo=bar\r\nhello\r\n0\r\n\r\n").unwrap(), b"hello");
        assert!(c.done());
    }

    #[test]
    fn a_malformed_chunk_size_is_an_error_not_a_silent_truncation() {
        let mut c = Chunked::default();
        assert!(c.feed(b"zz\r\nhello\r\n").is_err());
    }

    #[test]
    fn nothing_after_the_terminating_chunk_is_decoded() {
        let mut c = Chunked::default();
        let out = c.feed(b"3\r\nabc\r\n0\r\n\r\n9\r\nleftovers\r\n").unwrap();
        assert_eq!(out, b"abc");
        assert!(c.done());
        // And a further feed produces nothing rather than resuming.
        assert!(c.feed(b"3\r\nxyz\r\n").unwrap().is_empty());
    }

    // ── server-sent events ──────────────────────────────────────────────────

    #[test]
    fn sse_payloads_survive_being_split_at_every_byte() {
        let wire = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        let mut whole = Sse::default();
        assert_eq!(whole.feed(wire), vec!["{\"a\":1}", "{\"b\":2}"]);

        let mut split = Sse::default();
        let mut got = Vec::new();
        for b in wire {
            got.extend(split.feed(&[*b]));
        }
        assert_eq!(got, vec!["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn sse_ignores_comments_events_and_blank_lines() {
        let mut s = Sse::default();
        assert_eq!(
            s.feed(b": keep-alive\nevent: message\n\ndata: {\"x\":1}\n\n"),
            vec!["{\"x\":1}"]
        );
    }

    #[test]
    fn sse_handles_crlf_as_well_as_lf() {
        let mut s = Sse::default();
        assert_eq!(s.feed(b"data: {\"x\":1}\r\n\r\n"), vec!["{\"x\":1}"]);
    }

    #[test]
    fn an_incomplete_data_line_is_held_rather_than_emitted() {
        // A `data:` line can be split mid-UTF-8, which is why the buffer is
        // bytes and the decode happens per complete line.
        let mut s = Sse::default();
        assert!(s.feed("data: {\"c\":\"é".as_bytes()).is_empty());
        assert_eq!(s.feed("\"}\n".as_bytes()), vec!["{\"c\":\"é\"}"]);
    }

    // ── extracting text ─────────────────────────────────────────────────────

    #[test]
    fn every_reply_shape_that_exists_in_the_wild_yields_its_text() {
        // Three shapes, and a client that handled one would silently print
        // nothing against the others.
        assert_eq!(
            delta_text(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).as_deref(),
            Some("hi")
        );
        assert_eq!(
            delta_text(r#"{"choices":[{"message":{"content":"hi"}}]}"#).as_deref(),
            Some("hi")
        );
        assert_eq!(delta_text(r#"{"choices":[{"text":"hi"}]}"#).as_deref(), Some("hi"));
    }

    #[test]
    fn a_chunk_with_no_text_yields_none_rather_than_an_empty_print() {
        // The final chunk of every OpenAI stream is a finish_reason with no
        // content, and treating it as text would print a stray nothing.
        assert_eq!(delta_text(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#), None);
        assert_eq!(delta_text(r#"{"choices":[]}"#), None);
        assert_eq!(delta_text("not json"), None);
        assert_eq!(delta_text("{}"), None);
    }

    #[test]
    fn an_error_body_yields_its_message() {
        assert_eq!(
            error_text(r#"{"error":{"message":"context too long","type":"invalid"}}"#).as_deref(),
            Some("context too long")
        );
        // Falls back to the type when there is no message.
        assert_eq!(
            error_text(r#"{"error":{"type":"server_error"}}"#).as_deref(),
            Some("server_error")
        );
        assert_eq!(error_text("plain text"), None);
    }

    // ── constants that must not drift ───────────────────────────────────────

    #[test]
    fn the_daemon_path_and_the_endpoint_agree_with_rimed_core() {
        assert!(DAEMON.ends_with("/rime-aid"));
        let e = endpoints();
        assert!(e.api().ends_with(ai::API_SOCKET));
        assert!(e.control().ends_with(ai::CONTROL_SOCKET));
        assert!(e.dir().ends_with(ai::RUNTIME_SUBDIR));
    }

    #[test]
    fn the_not_running_message_names_the_command_that_starts_it() {
        let m = not_running();
        assert!(m.contains("systemctl --user enable --now rime-aid"), "{m}");
        assert!(m.contains("opt-in"), "{m}");
    }
}
