//! Local inference as an OS service (roadmap §14): schema, validation, and the
//! pure planners.
//!
//! §14 asks for `rime ai models`, `rime ai pull qwen3-coder`, `rime ai run`, and
//! behind them:
//!
//! > Expose one Rime local-inference service/API to applications. […] Abstract
//! > runtimes such as llama.cpp, Ollama, vLLM or future engines. […] Manage
//! > model storage, VRAM, idle unloading, CPU/GPU backend selection and power
//! > use. […] Support CUDA, ROCm, Vulkan and CPU inference where appropriate.
//! > […] Allow agent clients to use local inference through the same service.
//! > […] Think of it as a shared system service similar in spirit to PipeWire,
//! > but for model inference.
//!
//! Nothing in this module performs I/O. It owns the model-id and digest
//! vocabulary, the store layout, the catalogue and manifest formats, the
//! backend-selection policy, the VRAM fit arithmetic, the idle-unload decision
//! and the runtime argv planner. `rime-aid` and `rime ai` do the reading,
//! writing and spawning.
//!
//! ── THE DECISION THIS MODULE EXISTS TO RECORD: per-user, not system ─────────
//!
//! "A shared system service similar in spirit to PipeWire" is the roadmap's
//! phrasing, and it is worth noticing that **PipeWire is itself a per-user
//! session daemon** — `pipewire.service` is a `systemd --user` unit. What makes
//! PipeWire feel like a system service is that it is *the one* endpoint every
//! application talks to and that it arbitrates a contended device. Neither of
//! those requires running as root or serving every account from one process.
//!
//! So there were two candidate shapes:
//!
//! **(a) One system daemon, shared socket.** One `rime-aid.service` under
//! `systemd`, one socket in `/run`, every user's requests handled by one
//! process. Rejected. Three reasons, in order of how badly each fails:
//!
//! 1. **It puts every user's prompts in one address space.** A prompt is the
//!    most private thing a person types into a computer — it contains the code
//!    they are debugging, the mail they are drafting, the question they would
//!    not ask a colleague. A single process serving several accounts makes
//!    "user A's context reached user B's session" a bug away rather than
//!    impossible: one mistake in request routing, one reused KV-cache slot, one
//!    shared prompt-cache file, and the leak is silent because nothing on the
//!    machine is watching for it. Per-user, the kernel enforces the boundary
//!    and no code of ours has to be right for it to hold.
//! 2. **It is a privileged daemon whose entire job is untrusted content.**
//!    `AGENTS.md`'s agent-runtime rules are emphatic that `rime-agentd` stays
//!    unprivileged and per-user *because* it handles untrusted model output. A
//!    local inference service does not merely handle model output — it
//!    generates it, from prompt text it also did not write. That is the same
//!    argument one step earlier in the pipeline, and it points the same way.
//! 3. **It would need an authorisation story Rime is forbidden to build.**
//!    `AGENTS.md` bans giving this a polkit action or a system-bus name. A
//!    system daemon with no authorisation model and a socket several users can
//!    open is worse than either.
//!
//! **(b) A per-user daemon with a shared, read-only model store.** Chosen.
//! `rime-aid` is a `systemd --user` unit, opt-in exactly as `rime-agentd` is,
//! and its two endpoints are Unix sockets at mode `0600` inside a `0700`
//! directory in `$XDG_RUNTIME_DIR`, whose peer credentials are checked with
//! `SO_PEERCRED` anyway. The weights — the only genuinely large, genuinely
//! shareable part — live once under [`STORE_ROOT`], root-owned and
//! world-readable, written only by `sudo rime ai pull`.
//!
//! Tested against the questions that decide it:
//!
//! * *Can user A's prompt or context reach user B's session?* No. There is no
//!   process, socket, file or directory that both sessions can open. A's daemon
//!   runs as A, its sockets are in A's `$XDG_RUNTIME_DIR` (itself `0700`), and
//!   the only shared object is a read-only file of weights.
//! * *Is a localhost TCP OpenAI-compatible endpoint reachable by a local user
//!   you did not intend?* Yes, always, and that is why Rime does not offer one.
//!   `SO_PEERCRED` exists on Unix sockets only; a TCP connection carries no
//!   credential at all, so `127.0.0.1:11434` is open to every uid on the box,
//!   to every Flatpak that holds the network permission, and to anything that
//!   can persuade a browser to issue a request. The setting is *recognised in
//!   order to be refused* — see [`Settings::validate`] and
//!   [`refuse_tcp_endpoint`] — rather than merely undocumented.
//! * *What happens on a multi-user box?* Each account gets its own daemon and
//!   its own backend process, sharing the weights. Which is correct for
//!   privacy and is also where the one real limitation lives:
//!
//! ── STATED LIMITATION: VRAM is not arbitrated across users ─────────────────
//!
//! VRAM is genuinely contended and this design does not arbitrate it between
//! accounts. Within one user the daemon serialises: it holds at most one
//! backend process, and loading a second model unloads the first
//! ([`plan_idle`], [`Fit`]). Across users it cannot, because the thing that
//! could — one privileged process that sees every session's allocation — is
//! precisely the system daemon rejected above.
//!
//! The concrete failure is ordinary and survivable: the second user's backend
//! fails to allocate and reports it, and [`plan_fit`] is what turns that into a
//! smaller offload rather than a crash, because it plans against *free* VRAM
//! read at launch time and not against the card's total. What is lost is
//! fairness — first come, first served, with no queue and no eviction.
//!
//! This is recorded rather than fixed because the trade is not close for
//! Rime's target. A Daily or Gaming machine has one interactive user; the case
//! that would benefit is a shared workstation with two people generating at
//! once, and buying fairness for it would cost every single-user machine a
//! privileged daemon holding everyone's prompts. If that case ever matters, the
//! honest fix is a *small* system arbiter that hands out VRAM leases and never
//! sees a prompt — not moving inference into root.
//!
//! ── Backends are not in the image, and this module never installs one ──────
//!
//! llama.cpp with CUDA is gigabytes. `Containerfile.core` is the slow-moving
//! tier and a rebuild there costs the whole fleet a multi-gigabyte update, so
//! no inference runtime is baked in. P1 already built the two mechanisms that
//! serve this, and `AGENTS.md` forbids inventing a third:
//!
//! * `sudo rime install <runtime>` — a system extension overlaid on `/usr`,
//!   which is how a CPU or Vulkan build of `llama-server` arrives;
//! * `rime env create cuda` — a capsule with the NVIDIA driver passed through,
//!   which is how a CUDA build arrives without putting CUDA on the host.
//!
//! [`Runtime::install_hint`] is the whole of Rime's involvement: it returns the
//! exact command to type. Nothing here, and nothing in the daemon, downloads a
//! runtime.
//!
//! ── Where a model comes from, and what "verify by digest" actually proves ───
//!
//! A digest check proves the bytes that arrived match the digest you compared
//! them to. It proves nothing about *what you asked for* if the name-to-digest
//! mapping came over the same network. So the mapping is not fetched:
//!
//! | spec | mapping comes from | trust anchor |
//! | --- | --- | --- |
//! | `qwen3-coder` | [`Catalogue`] at [`CATALOGUE_PATH`], shipped in the image | the image is cosign-signed and digest-pinned in CI |
//! | `<url> --digest sha256:<hex>` | the person typing it | the user, explicitly |
//! | `<url>` with no digest | nowhere | **refused** — see [`PullError::NoDigest`] |
//!
//! There is deliberately no trust-on-first-use path. Pinning whatever the first
//! download happened to be and re-checking it afterwards would make every
//! subsequent verification pass while proving only that the file had not
//! changed since a moment nobody was watching.
//!
//! ── The store, and why the inference process cannot write to it ────────────
//!
//! ```text
//! /var/lib/rime/ai/
//!   models/blobs/sha256-<64 hex>   0444 root:root   the weights
//!   models/manifests/<id>.json     0444 root:root   id -> digest, and what it is
//!   staging/                       0700 root:root   download target; verified, then renamed
//! ```
//!
//! Root-owned and mode `0444`, so the backend — which runs as the user, loads
//! untrusted weights and speaks a network protocol — cannot alter a model, its
//! own or anyone else's. `/usr` is read-only so the store cannot live there, and
//! `/var` is where machine-local mutable state belongs.
//!
//! A blob is **verified in staging and then renamed** into `blobs/`. The rename
//! is atomic within a filesystem, so a partially written or wrong-digest file
//! is never visible under its final content-addressed name — the failure mode
//! that would otherwise make a corrupt download look like a cached model
//! forever.
//!
//! ── The two kinds of state, kept apart ─────────────────────────────────────
//!
//! [`crate::gameprofile`]'s rule, applied once more: state written only in
//! response to an explicit user command is user-owned and hand-editable;
//! anything a program produces on its own is generated and lives elsewhere.
//!
//! | file | kind | unknown keys |
//! | --- | --- | --- |
//! | `~/.config/rime/ai.toml` ([`Settings`]) | desired, user-owned | refused — one program writer, so an unknown key is a typo |
//! | `models/manifests/<id>.json` ([`Manifest`]) | generated | kept |
//!
//! The manifest keeps unknown keys for a reason `games.toml` does not have:
//! `bootc rollback` exists. A model pulled by version N of `rime` is read by
//! version N-1 after a rollback, so a field the reading build does not
//! recognise is a newer writer, not a mistake — the same argument
//! [`crate::host::HostCaps`] makes for a probe cache.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── constants ────────────────────────────────────────────────────────────────

/// Format version for [`Catalogue`], [`Manifest`] and [`Settings`]. Absent
/// means this.
pub const SCHEMA_VERSION: u32 = 1;

/// Control-protocol version spoken on the daemon's control socket. Bumped when
/// a change is not backward compatible, exactly as `rime_agent_core`'s
/// `PROTOCOL_VERSION` is.
pub const PROTOCOL_VERSION: u32 = 1;

/// The shared, read-only model store. Under `/var` because `/usr` is
/// image-owned and read-only, and machine-local mutable state belongs here.
pub const STORE_ROOT: &str = "/var/lib/rime/ai";

/// The curated catalogue, shipped in the image. Its integrity is the image's
/// integrity: cosign-signed, digest-pinned in CI, rolled back with the OS.
pub const CATALOGUE_PATH: &str = rime_paths::AI_CATALOGUE;

/// Directory under `$XDG_RUNTIME_DIR` holding the daemon's sockets. Created
/// `0700`.
pub const RUNTIME_SUBDIR: &str = "rime-ai";

/// Line-framed JSON control endpoint: status, model selection, unload.
pub const CONTROL_SOCKET: &str = "control.sock";

/// The inference endpoint. Applications and agent clients connect here and
/// speak the backend's own HTTP API; the daemon relays bytes and owns the
/// lifecycle. Mode `0600`.
pub const API_SOCKET: &str = "api.sock";

/// Where the backend child is told to listen. The `.sock` suffix is
/// load-bearing — see [`Runtime::listen_suffix_required`].
pub const BACKEND_SOCKET: &str = "backend.sock";

/// Idle timeout on AC, in seconds. Five minutes: long enough that a person
/// reading the previous answer before asking the next question does not pay a
/// reload, short enough that a forgotten session does not hold VRAM all day.
pub const IDLE_TIMEOUT_AC_SECS: u64 = 300;

/// Idle timeout on battery, in seconds.
///
/// Shorter, and this is the whole of §14's "power use" that Rime can honestly
/// claim. The mechanism: a process holding VRAM keeps the discrete GPU out of
/// its deepest idle state, so an *unused* loaded model costs power for nothing.
/// Measured on the katana (RTX 3070 Laptop, driver 580.178.04) with no compute
/// client attached: `pstate P8`, `clocks.gr 210 MHz`, `power.draw 14.24 W`,
/// `memory.used 52 MiB`. That is the floor this timeout returns the card to.
///
/// What is deliberately NOT claimed: a number for the loaded-and-idle case.
/// Measuring it needs a model resident on that card, which this branch never
/// had, and inventing one would be exactly the aspirational-as-measured
/// documentation `AGENTS.md` rejects.
pub const IDLE_TIMEOUT_BATTERY_SECS: u64 = 60;

/// VRAM left alone, in MiB, on a card the desktop is also using.
///
/// Taking every free byte is how a compositor loses its buffers and the screen
/// stops repainting. On the katana the discrete card reports 52 MiB in use
/// because that machine runs its desktop on the Alder Lake iGPU; a single-GPU
/// machine's compositor, browser and shell hold far more, and the reserve is
/// sized for that case rather than for the measurement that flatters it.
///
/// ── STATED LIMITATION: this is wrong for an APU, conservatively ────────────
///
/// Measured on the developer's laptop, an AMD APU with no discrete card:
/// `device 0 card1 — 1024 MiB total, 818 used, 0 spendable`. The reserve
/// consumed what was left of a 1 GiB carveout and [`select_backend`] correctly
/// fell through to [`Backend::Cpu`].
///
/// "Correctly" is doing some work there. On an APU, VRAM *is* system RAM:
/// `mem_info_vram_total` reports only the BIOS carveout, and a runtime can
/// allocate far beyond it through GTT. So a 32 GiB APU that could hold a large
/// model is reported as having a gigabyte, and this planner declines to offload
/// to it. The failure direction is the safe one — CPU inference on an APU is
/// most of the speed anyway, because both paths share one memory bus — but it
/// is a real underestimate and it is recorded rather than papered over.
///
/// Fixing it properly needs GTT accounting, which sysfs does not expose in any
/// portable shape (`mem_info_gtt_total` exists on amdgpu but means something
/// different again, and there is no i915/xe equivalent). Guessing "APU
/// therefore system RAM" from a missing discrete card would be exactly the
/// confident wrong number this module refuses elsewhere, so the honest move is
/// to under-promise and say so.
pub const VRAM_RESERVE_MIB: u64 = 512;

/// Fixed VRAM cost of having a backend at all, in MiB: the driver context,
/// scratch buffers and the compute graph, none of which scale with the model.
/// Charged before any layer is offloaded.
pub const VRAM_OVERHEAD_MIB: u64 = 256;

/// The longest a model id may be. It is a path component and a CLI argument.
const MAX_MODEL_ID: usize = 96;

/// The longest a source URL may be.
const MAX_URL: usize = 2048;

/// Length of a hex-encoded SHA-256.
const SHA256_HEX: usize = 64;

// ── model ids ────────────────────────────────────────────────────────────────

/// Why a model id, digest, URL or setting was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// A schema version this build does not understand.
    UnsupportedVersion(u32),
    /// The id is empty, too long, or carries a character that cannot appear in
    /// both a path component and an argv entry.
    BadModelId { id: String, why: String },
    /// The id is valid apart from its case, so the message can name the fix.
    UppercaseModelId { id: String, lower: String },
    /// Not `sha256:` followed by 64 lowercase hex digits.
    BadDigest(String),
    /// Not a URL this build will fetch.
    BadUrl { url: String, why: String },
    /// A setting that exists only so its refusal can explain itself.
    Refused { key: &'static str, because: &'static str },
    /// A value outside the range the daemon can honour.
    OutOfRange { key: &'static str, value: u64, min: u64, max: u64 },
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(
                f,
                "version {v} is not a schema this rime understands (expected {SCHEMA_VERSION})"
            ),
            Self::BadModelId { id, why } => write!(
                f,
                "model id {id:?} is not usable: {why}. Ids are 1-{MAX_MODEL_ID} characters of \
                 lowercase letters, digits, '.', '_', '+' or '-', starting with a letter or a digit"
            ),
            Self::UppercaseModelId { id, lower } => write!(
                f,
                "model id {id:?} has uppercase letters. Ids are lowercase so that one model \
                 cannot become two entries in the store — try {lower:?}"
            ),
            Self::BadDigest(d) => write!(
                f,
                "{d:?} is not a digest: expected 'sha256:' followed by {SHA256_HEX} lowercase \
                 hex digits"
            ),
            Self::BadUrl { url, why } => write!(f, "{url:?} is not a URL rime will fetch: {why}"),
            Self::Refused { key, because } => {
                write!(f, "{key} is not a setting Rime accepts: {because}")
            }
            Self::OutOfRange { key, value, min, max } => {
                write!(f, "{key} = {value} is outside {min}-{max}")
            }
        }
    }
}

impl std::error::Error for AiError {}

/// A model id must work as a path component (`manifests/<id>.json`), as an argv
/// entry, and as a TOML table key.
///
/// The same allowlist-never-escape rule [`crate::host`] is built on: a value
/// either matches a narrow character class or it is refused by name. There is
/// no quoting function here.
pub fn validate_model_id(id: &str) -> Result<(), AiError> {
    let bad = |why: &str| AiError::BadModelId { id: id.to_string(), why: why.to_string() };

    if id.is_empty() {
        return Err(bad("it is empty"));
    }
    if id.len() > MAX_MODEL_ID {
        return Err(bad("it is too long"));
    }
    // Before the character class, so a person who typed a real model's
    // capitalised name is told about case rather than about punctuation.
    if id.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(AiError::UppercaseModelId {
            id: id.to_string(),
            lower: id.to_ascii_lowercase(),
        });
    }
    if id.starts_with('-') {
        return Err(bad("it starts with '-', which an argument parser would read as a flag"));
    }
    if !id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return Err(bad("it must start with a letter or a digit"));
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '+' | '-')))
    {
        if c == '/' {
            return Err(bad(
                "it contains '/', and the id is a path component under models/manifests",
            ));
        }
        return Err(bad(&format!("it contains {c:?}")));
    }
    // `.` and `..` are legal under the class above and are exactly the two
    // path components that must never be one.
    if id == "." || id == ".." {
        return Err(bad("'.' and '..' are directory entries, not names"));
    }
    Ok(())
}

/// A digest: `sha256:` and 64 lowercase hex digits, and nothing else.
///
/// Lowercase only, because the digest becomes a filename
/// (`blobs/sha256-<hex>`) and accepting both cases would let one blob occupy
/// two names — which defeats the point of content-addressing.
pub fn validate_digest(digest: &str) -> Result<(), AiError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(AiError::BadDigest(digest.to_string()));
    };
    if hex.len() != SHA256_HEX
        || !hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(AiError::BadDigest(digest.to_string()));
    }
    Ok(())
}

/// The blob filename for a digest: `sha256-<hex>`.
///
/// A `-` rather than the `:` the digest uses, following OCI layout, because a
/// `:` in a path is the one character whose meaning differs between a local
/// path and a remote reference.
pub fn blob_name(digest: &str) -> Result<String, AiError> {
    validate_digest(digest)?;
    Ok(digest.replacen(':', "-", 1))
}

/// A source URL rime will fetch. HTTPS only, no credentials, no fragment.
///
/// `http://` is refused rather than downgraded-with-a-warning: a model is
/// gigabytes of code-adjacent data executed by a process holding the user's
/// GPU, and the digest check that would catch tampering is only as good as the
/// digest, which on the plain-URL path came from the same place. HTTPS costs
/// nothing here — every source of weights that matters serves it.
pub fn validate_url(url: &str) -> Result<(), AiError> {
    let bad = |why: &str| AiError::BadUrl { url: url.to_string(), why: why.to_string() };

    if url.len() > MAX_URL {
        return Err(bad("it is longer than the maximum this build accepts"));
    }
    let Some(rest) = url.strip_prefix("https://") else {
        if url.starts_with("http://") {
            return Err(bad(
                "it is plain HTTP. Weights are fetched over HTTPS only — the digest that would \
                 catch tampering is not independent of the connection that delivered it",
            ));
        }
        return Err(bad("it does not start with https://"));
    };
    if rest.is_empty() {
        return Err(bad("it has no host"));
    }
    if rest.contains('@') {
        return Err(bad("it embeds credentials, which Rime never stores or transmits"));
    }
    // Whitespace and control characters reach curl's argv and a shell log.
    if let Some(c) = url.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(&format!("it contains {c:?}")));
    }
    Ok(())
}

// ── the store layout ─────────────────────────────────────────────────────────

/// Pure path arithmetic over the model store. Takes its root as an argument so
/// a test can point it at a temporary directory without an environment
/// variable and without this module touching a filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Store::new(Path::new(STORE_ROOT))
    }
}

impl Store {
    /// A store rooted anywhere.
    pub fn new(root: &Path) -> Store {
        Store { root: root.to_path_buf() }
    }

    /// The root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/models`.
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    /// `<root>/models/blobs` — the weights, content-addressed.
    pub fn blobs_dir(&self) -> PathBuf {
        self.models_dir().join("blobs")
    }

    /// `<root>/models/manifests` — one JSON per installed model.
    pub fn manifests_dir(&self) -> PathBuf {
        self.models_dir().join("manifests")
    }

    /// `<root>/staging` — download target. `0700 root:root`, so an
    /// in-progress file is not even readable by the account that will use it.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// Where a digest's weights live once verified.
    pub fn blob(&self, digest: &str) -> Result<PathBuf, AiError> {
        Ok(self.blobs_dir().join(blob_name(digest)?))
    }

    /// Where a model's manifest lives. Validates the id, so a lookup can never
    /// be the thing that lets a bad id through.
    pub fn manifest(&self, id: &str) -> Result<PathBuf, AiError> {
        validate_model_id(id)?;
        Ok(self.manifests_dir().join(format!("{id}.json")))
    }

    /// The staging path for a digest.
    ///
    /// Named for the digest and not for the id, because staging is where the
    /// bytes are before anyone knows whether they are that digest — and two
    /// ids that resolve to the same weights must not fight over one temporary
    /// file. The `.part` suffix means the name can never collide with a
    /// finished blob if the two directories are ever the same one.
    pub fn staging(&self, digest: &str) -> Result<PathBuf, AiError> {
        Ok(self.staging_dir().join(format!("{}.part", blob_name(digest)?)))
    }
}

// ── the catalogue ────────────────────────────────────────────────────────────

/// `/usr/share/rimeos/ai/catalogue.toml` — the curated, image-shipped
/// name-to-digest mapping.
///
/// Image-shipped rather than fetched, and that is the point: see the module
/// docs. It is also why this file is *not* under `/var` — it is image content,
/// it rolls back with the OS, and nothing on the machine may rewrite it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    /// Format version. Absent means [`SCHEMA_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Models by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model: BTreeMap<String, CatalogueEntry>,
}

/// One curated model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    /// Human label for listings.
    pub title: String,
    /// Where the weights come from. HTTPS, validated by [`validate_url`].
    pub url: String,
    /// `sha256:<hex>` of the file at `url`.
    pub digest: String,
    /// Size of the weights on disk, in MiB. Also the approximate VRAM cost of
    /// the weights themselves, which is what [`plan_fit`] charges.
    pub weights_mib: u64,
    /// Repeating transformer blocks — what `llama-server -ngl` counts.
    pub layers: u32,
    /// KV-cache cost in MiB per 1024 tokens of context, for this model's
    /// architecture and this build's default cache precision.
    ///
    /// A declared per-model constant rather than a formula, because computing
    /// it needs `n_kv_heads`, `head_dim` and the cache type, none of which are
    /// knowable without parsing the GGUF — and a formula that guessed them
    /// would produce a confident wrong number instead of an honest table entry.
    pub kv_mib_per_1k: u64,
    /// Largest context the weights were trained for.
    pub max_context: u32,
    /// Quantisation, for listings (`Q4_K_M`).
    pub quant: String,
    /// Licence identifier, so `rime ai models --available` can show it before
    /// anyone downloads several gigabytes.
    pub license: String,
    /// Which runtime can load this file.
    pub runtime: String,
    /// A free-text note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Catalogue {
    /// Parse and validate.
    pub fn parse(text: &str) -> Result<Catalogue, anyhow::Error> {
        let cat: Catalogue = toml::from_str(text)?;
        cat.validate()?;
        Ok(cat)
    }

    /// Serialise. Deterministic: the map is sorted and empty fields are
    /// skipped.
    pub fn to_toml(&self) -> Result<String, anyhow::Error> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Every reason this catalogue is unusable, or `Ok`.
    ///
    /// Strict, because it is image content: a malformed entry here is a build
    /// mistake that a static check should have caught, not something a user can
    /// fix, so refusing loudly is the only useful behaviour.
    pub fn validate(&self) -> Result<(), AiError> {
        if let Some(v) = self.version {
            if v > SCHEMA_VERSION {
                return Err(AiError::UnsupportedVersion(v));
            }
        }
        for (id, e) in &self.model {
            validate_model_id(id)?;
            validate_url(&e.url)?;
            validate_digest(&e.digest)?;
            if e.layers == 0 {
                return Err(AiError::OutOfRange { key: "layers", value: 0, min: 1, max: u32::MAX as u64 });
            }
            if e.weights_mib == 0 {
                return Err(AiError::OutOfRange {
                    key: "weights_mib",
                    value: 0,
                    min: 1,
                    max: u64::MAX,
                });
            }
            if e.max_context == 0 {
                return Err(AiError::OutOfRange {
                    key: "max_context",
                    value: 0,
                    min: 1,
                    max: u32::MAX as u64,
                });
            }
            if e.runtime.parse::<Runtime>().is_err() {
                return Err(AiError::BadModelId {
                    id: id.clone(),
                    why: format!(
                        "runtime = {:?} is not one of: {}",
                        e.runtime,
                        Runtime::all_ids().join(", ")
                    ),
                });
            }
        }
        Ok(())
    }

    /// An entry by id.
    pub fn get(&self, id: &str) -> Option<&CatalogueEntry> {
        self.model.get(id)
    }

    /// Ids in listing order.
    pub fn ids(&self) -> Vec<&str> {
        self.model.keys().map(String::as_str).collect()
    }
}

// ── the manifest ─────────────────────────────────────────────────────────────

/// `models/manifests/<id>.json` — what `rime ai pull` recorded.
///
/// Keeps unknown keys, unlike [`Catalogue`] and [`Settings`]: `bootc rollback`
/// means a manifest written by a newer `rime` is read by an older one, so an
/// unrecognised field is version skew and not a typo. Same argument
/// [`crate::host::HostCaps`] makes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Manifest {
    /// Format version.
    #[serde(default)]
    pub version: u32,
    /// The id this model is known by.
    pub id: String,
    /// `sha256:<hex>` of the weights, and the name of the blob.
    pub digest: String,
    /// Size on disk in MiB.
    #[serde(default)]
    pub weights_mib: u64,
    /// Repeating blocks, for [`plan_fit`].
    #[serde(default)]
    pub layers: u32,
    /// KV cost per 1024 tokens, for [`plan_fit`].
    #[serde(default)]
    pub kv_mib_per_1k: u64,
    /// Largest context the weights support.
    #[serde(default)]
    pub max_context: u32,
    /// Which runtime loads it.
    #[serde(default)]
    pub runtime: String,
    /// Where it came from. Recorded so `rime ai models` can say, and so a
    /// re-pull after a store wipe needs no catalogue lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Unix seconds of the pull.
    #[serde(default)]
    pub pulled_at: i64,
    /// True when the digest came from the person typing rather than from the
    /// image-shipped catalogue. Surfaced by `rime ai models`, because "you
    /// vouched for this one" is a fact the user should keep being told.
    #[serde(default)]
    pub user_supplied_digest: bool,
    /// Anything a newer `rime` recorded that this one does not know.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Manifest {
    /// Parse and validate.
    pub fn parse(text: &str) -> Result<Manifest, anyhow::Error> {
        let m: Manifest = serde_json::from_str(text)?;
        m.validate()?;
        Ok(m)
    }

    /// Serialise.
    pub fn to_json(&self) -> Result<String, anyhow::Error> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Refuse a manifest whose id or digest could not have come from a
    /// validated pull. It names a path and a blob, so it is re-validated on
    /// read rather than trusted because a root-owned program wrote it.
    pub fn validate(&self) -> Result<(), AiError> {
        if self.version > SCHEMA_VERSION {
            return Err(AiError::UnsupportedVersion(self.version));
        }
        validate_model_id(&self.id)?;
        validate_digest(&self.digest)?;
        if let Some(u) = &self.url {
            validate_url(u)?;
        }
        Ok(())
    }

    /// The runtime this model needs, or `None` when the manifest names one this
    /// build does not know — which after a rollback is possible.
    pub fn runtime(&self) -> Option<Runtime> {
        self.runtime.parse().ok()
    }
}

// ── settings ─────────────────────────────────────────────────────────────────

/// `~/.config/rime/ai.toml` — user-owned, hand-editable, refused if not
/// understood.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Format version. Absent means [`SCHEMA_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The model `rime ai run` uses when none is named, and the one the daemon
    /// loads for a bare API connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Force a compute backend instead of letting [`select_backend`] rank
    /// them. `cuda`, `rocm`, `vulkan` or `cpu`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Context size in tokens. Absent means the model's own maximum, capped by
    /// what fits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    /// Seconds of idle before the backend is stopped. Absent means
    /// [`IDLE_TIMEOUT_AC_SECS`] / [`IDLE_TIMEOUT_BATTERY_SECS`] by power
    /// source. `0` means never, which [`plan_idle`] honours and reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<u64>,

    // ── recognised only to be refused ────────────────────────────────────────
    //
    // `deny_unknown_fields` already rejects an unknown key, with a message that
    // lists the legal ones and explains nothing. These four are declared so the
    // refusal can explain the design instead — and the first three are the
    // exact settings someone arriving from Ollama or an OpenAI-compatible
    // server will reach for first. None can survive `validate`, so none is ever
    // serialised.
    /// **Refused.** See [`Settings::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    /// **Refused.** See [`Settings::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// **Refused.** See [`Settings::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// **Refused.** See [`Settings::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// The refusal every TCP-shaped setting produces.
///
/// One function so the four keys, the `--listen` flag and the daemon's own
/// guard cannot come to disagree about why. The reason is the load-bearing part
/// — a bare "unsupported" would read as "not implemented yet" and invite
/// someone to implement it.
pub fn refuse_tcp_endpoint(key: &'static str) -> AiError {
    AiError::Refused {
        key,
        because: "Rime's inference endpoint is a Unix socket in your $XDG_RUNTIME_DIR at mode \
                  0600, and it cannot be a TCP port. A TCP connection carries no peer \
                  credential — SO_PEERCRED works on Unix sockets only — so a listener on \
                  127.0.0.1 is open to every account on this machine, to every sandboxed \
                  application holding the network permission, and to anything that can make an \
                  HTTP request from a page. There is no way to tell those apart from you. To \
                  reach the service from another machine, use the ssh transport `rime host` \
                  already provides",
    }
}

impl Settings {
    /// Parse and validate.
    pub fn parse(text: &str) -> Result<Settings, anyhow::Error> {
        let s: Settings = toml::from_str(text)?;
        s.validate()?;
        Ok(s)
    }

    /// Serialise.
    pub fn to_toml(&self) -> Result<String, anyhow::Error> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Everything wrong with these settings, or `Ok`.
    pub fn validate(&self) -> Result<(), AiError> {
        if let Some(v) = self.version {
            if v > SCHEMA_VERSION {
                return Err(AiError::UnsupportedVersion(v));
            }
        }
        // The refusals first, before anything that might mask them: someone who
        // wrote `port = 11434` should be told about peer credentials, not about
        // a character class somewhere else in the file.
        if self.listen.is_some() {
            return Err(refuse_tcp_endpoint("listen"));
        }
        if self.host.is_some() {
            return Err(refuse_tcp_endpoint("host"));
        }
        if self.port.is_some() {
            return Err(refuse_tcp_endpoint("port"));
        }
        if self.api_key.is_some() {
            return Err(AiError::Refused {
                key: "api_key",
                because: "there is no key to configure. The endpoint is a Unix socket only you \
                          can open, so authentication is the file mode and the kernel's peer \
                          credentials rather than a shared secret in a config file",
            });
        }
        if let Some(m) = &self.model {
            validate_model_id(m)?;
        }
        if let Some(b) = &self.backend {
            if b.parse::<Backend>().is_err() {
                return Err(AiError::BadModelId {
                    id: b.clone(),
                    why: format!("backend must be one of: {}", Backend::all_ids().join(", ")),
                });
            }
        }
        if let Some(c) = self.context {
            if c != 0 && !(256..=1_048_576).contains(&c) {
                return Err(AiError::OutOfRange {
                    key: "context",
                    value: c as u64,
                    min: 256,
                    max: 1_048_576,
                });
            }
        }
        if let Some(t) = self.idle_timeout {
            if t > 86_400 {
                return Err(AiError::OutOfRange {
                    key: "idle_timeout",
                    value: t,
                    min: 0,
                    max: 86_400,
                });
            }
        }
        Ok(())
    }

    /// The backend the user pinned, if any and if parseable.
    pub fn backend_pref(&self) -> Option<Backend> {
        self.backend.as_deref().and_then(|b| b.parse().ok())
    }
}

// ── compute backends ─────────────────────────────────────────────────────────

/// A compute backend a runtime can be built against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Backend {
    /// NVIDIA's compute API.
    Cuda,
    /// AMD's compute stack.
    Rocm,
    /// The portable one. Works on NVIDIA, AMD and Intel, including iGPUs.
    Vulkan,
    /// No accelerator. Always available, and on a machine with enough RAM the
    /// only backend that can hold a large model at all.
    Cpu,
}

impl Backend {
    /// Ranked best first. This order is **policy**, and the reason is that the
    /// vendor compute backends are the ones upstream tunes and tests against,
    /// while Vulkan is the portable fallback that works everywhere and is
    /// generally slower on the same silicon. It is not a measurement made by
    /// this branch, and `[ai] backend` overrides it.
    pub const ALL: [Backend; 4] = [Backend::Cuda, Backend::Rocm, Backend::Vulkan, Backend::Cpu];

    /// The stable id used in settings, argv and output.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Rocm => "rocm",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }

    /// Every id, for a refusal that lists the real ones.
    pub fn all_ids() -> Vec<&'static str> {
        Backend::ALL.iter().map(|b| b.as_str()).collect()
    }

    /// Whether this backend offloads to a GPU at all.
    pub fn uses_gpu(self) -> bool {
        !matches!(self, Backend::Cpu)
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Backend {
    type Err = AiError;

    fn from_str(s: &str) -> Result<Backend, AiError> {
        match s {
            "cuda" | "nvidia" => Ok(Backend::Cuda),
            "rocm" | "hip" | "amd" => Ok(Backend::Rocm),
            "vulkan" | "vk" => Ok(Backend::Vulkan),
            "cpu" | "none" => Ok(Backend::Cpu),
            _ => Err(AiError::BadModelId {
                id: s.to_string(),
                why: format!("backend must be one of: {}", Backend::all_ids().join(", ")),
            }),
        }
    }
}

/// Where the caller looks to decide which backends exist. Constants rather than
/// paths built at the call site, so the daemon, the CLI and the tests cannot
/// disagree about what evidence means.
pub mod probe_paths {
    /// The NVIDIA driver's control device. Present iff the kernel module is
    /// loaded, which is what CUDA actually needs — a `libcuda.so` with no
    /// device is a driver that is installed but not running.
    pub const NVIDIA_CONTROL: &str = "/dev/nvidiactl";
    /// The NVIDIA userspace driver. Verified present on the katana as
    /// `/usr/lib64/libcuda.so.1` (driver 580.178.04).
    pub const LIBCUDA: &str = rime_paths::LIBCUDA;
    /// AMD's compute device. The definitive ROCm signal: ROCm cannot run
    /// without `amdkfd`, and `/opt/rocm` existing proves only that somebody
    /// installed a package. Absent on the katana, which has no AMD GPU.
    pub const KFD: &str = "/dev/kfd";
    /// Render nodes. A Vulkan ICD with no render node is an ICD that will fall
    /// back to software.
    pub const DRI_DIR: &str = "/dev/dri";
    /// Where Mesa and the NVIDIA driver drop Vulkan ICD manifests. Verified on
    /// the katana: 26 files including `nvidia_icd.x86_64.json`,
    /// `radeon_icd.x86_64.json`, `intel_icd.x86_64.json` and `lvp_icd.x86_64.json`.
    pub const VULKAN_ICD_DIR: &str = rime_paths::VULKAN_ICD_DIR;
}

/// ICD filename stems that are software rasterisers, not GPUs.
///
/// This list is why "is Vulkan available" is not "is there an ICD". Mesa ships
/// `lvp_icd.<arch>.json` — lavapipe, a CPU implementation of Vulkan — on every
/// single machine, including ones with no GPU at all. Counting it would make
/// [`select_backend`] choose "Vulkan" everywhere and then run a CPU renderer
/// while reporting GPU offload, which is worse than choosing [`Backend::Cpu`]
/// honestly.
pub const SOFTWARE_VULKAN_ICDS: &[&str] = &["lvp", "swrast", "vk_swiftshader"];

/// The raw evidence a caller gathered. Every field is something read from the
/// filesystem by the caller; the classification is done here, so it can be
/// tested without a machine that has any of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccelEvidence {
    /// [`probe_paths::NVIDIA_CONTROL`] exists.
    pub nvidia_control_dev: bool,
    /// [`probe_paths::LIBCUDA`] exists.
    pub libcuda: bool,
    /// [`probe_paths::KFD`] exists.
    pub kfd_dev: bool,
    /// How many `renderD*` nodes are in [`probe_paths::DRI_DIR`].
    pub render_nodes: u32,
    /// File names found in [`probe_paths::VULKAN_ICD_DIR`], as read.
    pub vulkan_icds: Vec<String>,
}

/// Which backends this machine can actually run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accel {
    /// NVIDIA driver loaded and its userspace library present.
    pub cuda: bool,
    /// `amdkfd` present.
    pub rocm: bool,
    /// A hardware Vulkan ICD and a render node.
    pub vulkan: bool,
}

impl AccelEvidence {
    /// Classify the evidence.
    pub fn accel(&self) -> Accel {
        Accel {
            // Both halves. A device node with no library cannot be used, and a
            // library with no device node is an unloaded module.
            cuda: self.nvidia_control_dev && self.libcuda,
            rocm: self.kfd_dev,
            vulkan: self.render_nodes > 0 && self.has_hardware_vulkan_icd(),
        }
    }

    /// Whether any ICD present describes real hardware.
    pub fn has_hardware_vulkan_icd(&self) -> bool {
        self.vulkan_icds.iter().any(|f| !is_software_icd(f))
    }

    /// The hardware ICDs, for reporting.
    pub fn hardware_vulkan_icds(&self) -> Vec<&str> {
        self.vulkan_icds
            .iter()
            .filter(|f| !is_software_icd(f))
            .map(String::as_str)
            .collect()
    }
}

/// Whether an ICD file name is a software rasteriser.
///
/// Matched on the stem before the first `_`, because the shipped names are
/// `<driver>_icd.<arch>.json` — so `lvp_icd.x86_64.json` and
/// `lvp_icd.i686.json` are both lavapipe and neither is a GPU.
pub fn is_software_icd(file_name: &str) -> bool {
    let stem = file_name.split('_').next().unwrap_or(file_name);
    SOFTWARE_VULKAN_ICDS.contains(&stem)
}

impl Accel {
    /// Whether a backend is available here. [`Backend::Cpu`] always is.
    pub fn has(&self, b: Backend) -> bool {
        match b {
            Backend::Cuda => self.cuda,
            Backend::Rocm => self.rocm,
            Backend::Vulkan => self.vulkan,
            Backend::Cpu => true,
        }
    }

    /// Every available backend, best first.
    pub fn available(&self) -> Vec<Backend> {
        Backend::ALL.into_iter().filter(|b| self.has(*b)).collect()
    }
}

/// One accelerator, as the caller measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Index as the runtime numbers them.
    pub index: u32,
    /// Name, for reporting.
    pub name: String,
    /// Total VRAM in MiB.
    pub total_mib: u64,
    /// VRAM in use by anything, in MiB.
    pub used_mib: u64,
}

impl Device {
    /// VRAM this planner may spend: free, less [`VRAM_RESERVE_MIB`] for
    /// whatever is drawing the screen. Saturating, so a card that is already
    /// full yields zero rather than wrapping.
    pub fn budget_mib(&self) -> u64 {
        self.total_mib
            .saturating_sub(self.used_mib)
            .saturating_sub(VRAM_RESERVE_MIB)
    }
}

/// What [`select_backend`] decided, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendChoice {
    /// The backend to use.
    pub backend: Backend,
    /// The device index, when the backend uses one.
    pub device: Option<u32>,
    /// Why this one. Printed by `rime ai status`, because a selection nobody
    /// can see is one the next edit changes by accident.
    pub why: String,
    /// Each backend that was not chosen, and the reason. Reported rather than
    /// dropped: "why is this running on the CPU" is the first question anyone
    /// asks, and the answer must not require reading the source.
    pub rejected: Vec<(Backend, String)>,
}

/// Choose a compute backend.
///
/// Pure, and total: it always returns a choice, because [`Backend::Cpu`] is
/// always available. `want` is the user's pin from `[ai] backend` or
/// `--backend`.
///
/// **An unavailable pin is refused, never downgraded.** `AGENTS.md`'s rule that
/// a policy must not be silently weakened applies directly: someone who pinned
/// `cuda` and got CPU inference at a fortieth of the speed would conclude the
/// machine is slow, not that the pin did not take.
pub fn select_backend(
    accel: &Accel,
    devices: &[Device],
    want: Option<Backend>,
) -> Result<BackendChoice, AiError> {
    if let Some(w) = want {
        if !accel.has(w) {
            return Err(AiError::Refused {
                key: "backend",
                because: match w {
                    Backend::Cuda => "this machine has no usable CUDA device. \
                                      /dev/nvidiactl and libcuda.so.1 must both be present — \
                                      a driver package alone is not enough, the module has to \
                                      be loaded",
                    Backend::Rocm => "this machine has no /dev/kfd, so ROCm cannot run here. \
                                      An /opt/rocm directory is not the test: amdkfd is",
                    Backend::Vulkan => "this machine has no hardware Vulkan ICD and a render \
                                        node. Mesa's lavapipe ICD is present on every machine \
                                        and is a CPU renderer, so it does not count",
                    // Unreachable via `has`, which returns true for Cpu.
                    Backend::Cpu => "cpu is always available",
                },
            });
        }
        return Ok(BackendChoice {
            backend: w,
            device: pick_device(w, devices),
            why: format!("{w} was pinned in your settings or on the command line"),
            rejected: Backend::ALL
                .into_iter()
                .filter(|b| *b != w)
                .map(|b| (b, "not chosen: a backend was pinned".to_string()))
                .collect(),
        });
    }

    let mut rejected = Vec::new();
    for b in Backend::ALL {
        if !accel.has(b) {
            rejected.push((b, unavailable_reason(b).to_string()));
            continue;
        }
        // The GPU backends need a device with room, not merely a driver. A
        // machine whose only card is full is a machine that must run on the CPU,
        // and saying so here is what keeps `rime ai status` honest.
        if b.uses_gpu() && pick_device(b, devices).is_none() {
            rejected.push((
                b,
                if devices.is_empty() {
                    format!("{b} is installed, but no device reported its VRAM")
                } else {
                    format!(
                        "{b} is installed, but no device has {VRAM_RESERVE_MIB} MiB free beyond \
                         the display reserve"
                    )
                },
            ));
            continue;
        }
        let why = match b {
            Backend::Cuda => "CUDA: the NVIDIA driver is loaded and a device has room".to_string(),
            Backend::Rocm => "ROCm: /dev/kfd is present and a device has room".to_string(),
            Backend::Vulkan => format!(
                "Vulkan: a hardware ICD and a render node are present, and no vendor compute \
                 backend is ({})",
                rejected
                    .iter()
                    .map(|(b, _)| b.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Backend::Cpu => format!(
                "CPU: no GPU backend is usable here ({})",
                rejected
                    .iter()
                    .map(|(b, _)| b.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        // Everything after the winner is unexamined, and says so rather than
        // claiming a reason it did not compute.
        for lower in Backend::ALL.into_iter().skip_while(|x| *x != b).skip(1) {
            rejected.push((lower, format!("not examined: {b} was chosen first")));
        }
        return Ok(BackendChoice { backend: b, device: pick_device(b, devices), why, rejected });
    }
    // Unreachable: Backend::Cpu is always available. Written as a value rather
    // than an `unreachable!()` because a panic in a planner the daemon calls is
    // exactly the failure `rimed/AGENTS.md` forbids.
    Ok(BackendChoice {
        backend: Backend::Cpu,
        device: None,
        why: "CPU: nothing else is available".to_string(),
        rejected,
    })
}

/// Why a backend is unavailable, for the rejection list.
fn unavailable_reason(b: Backend) -> &'static str {
    match b {
        Backend::Cuda => "no /dev/nvidiactl and libcuda.so.1",
        Backend::Rocm => "no /dev/kfd",
        Backend::Vulkan => "no hardware Vulkan ICD with a render node",
        Backend::Cpu => "always available",
    }
}

/// The device with the most spendable VRAM, or `None` for a CPU backend or a
/// machine whose cards have no room.
fn pick_device(b: Backend, devices: &[Device]) -> Option<u32> {
    if !b.uses_gpu() {
        return None;
    }
    devices
        .iter()
        .filter(|d| d.budget_mib() > VRAM_OVERHEAD_MIB)
        // `max_by_key` returns the LAST maximum; the first is wanted, so ties
        // resolve to the lower index and the choice is deterministic.
        .rev()
        .max_by_key(|d| d.budget_mib())
        .map(|d| d.index)
}

// ── the VRAM fit ─────────────────────────────────────────────────────────────

/// Where a model's layers ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Every layer in VRAM.
    Gpu { layers: u32 },
    /// Some layers in VRAM, the rest on the CPU. Slow but usable, and far
    /// better than refusing.
    Split { layers: u32, total: u32 },
    /// Nothing offloaded.
    Cpu,
}

impl Placement {
    /// The `-ngl` value this placement means.
    pub fn gpu_layers(self) -> u32 {
        match self {
            Placement::Gpu { layers } => layers,
            Placement::Split { layers, .. } => layers,
            Placement::Cpu => 0,
        }
    }
}

/// What [`plan_fit`] decided, and the arithmetic behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    /// The placement.
    pub placement: Placement,
    /// Context that will actually be used, after any reduction.
    pub context: u32,
    /// VRAM the plan expects to occupy, in MiB.
    pub vram_mib: u64,
    /// VRAM available to spend, in MiB — the input, echoed so a report can show
    /// both sides of the comparison.
    pub budget_mib: u64,
    /// Why. Printed by `rime ai status` and `rime ai run --explain`.
    pub notes: Vec<String>,
}

impl Fit {
    /// Whether every layer is on the GPU.
    pub fn is_full_offload(&self) -> bool {
        matches!(self.placement, Placement::Gpu { .. })
    }
}

/// Decide how much of a model fits, given a measured VRAM budget.
///
/// Pure, so `rime ai status` and the daemon's launch path compute the same
/// answer from the same inputs — which is what makes the printed plan a report
/// rather than a rehearsal of different code.
///
/// ## The arithmetic, and where it is deliberately approximate
///
/// ```text
/// need(ctx) = VRAM_OVERHEAD_MIB + weights_mib + kv_mib_per_1k * ctx / 1024
/// ```
///
/// Two approximations, both stated because both can be wrong in a direction
/// that matters:
///
/// 1. **Per-layer cost is `weights_mib / layers`.** Layers are not equal — the
///    token embedding and output projection are not repeating blocks and
///    `llama-server -ngl` does not count them — so the real per-layer figure is
///    slightly lower and a computed split can be one layer too greedy. The
///    consequence is an allocation failure at *startup*, which the runtime
///    reports and the daemon must surface rather than retry blindly. So the
///    split path spends one layer's worth of margin ([`SPLIT_MARGIN_LAYERS`]).
/// 2. **KV cost comes from the model's declared `kv_mib_per_1k`**, not from a
///    formula. See [`CatalogueEntry::kv_mib_per_1k`].
///
/// Context is reduced before layers are given up, in that order and
/// deliberately: halving the context of a model that then runs entirely on the
/// GPU is far faster than keeping the full context and moving half the weights
/// to system RAM. It stops at [`MIN_USEFUL_CONTEXT`], below which the model
/// cannot hold a conversation and the honest answer is a split or the CPU.
pub fn plan_fit(
    weights_mib: u64,
    layers: u32,
    kv_mib_per_1k: u64,
    want_context: u32,
    budget_mib: u64,
) -> Fit {
    let mut notes = Vec::new();

    // `layers == 0` means the manifest declares NO layer count, which is what
    // `rime ai pull --url` writes for every model outside the catalogue. It
    // does not mean the model has one layer.
    //
    // This line used to be `let layers = layers.max(1)`, which was added to
    // stop the split path dividing by zero and did — but it also made every
    // uncatalogued model plan as a one-layer model. A 4 GiB model that fitted
    // entirely in VRAM was reported as "all 1 layers fit" and launched with
    // `--n-gpu-layers 1`, so one layer went to the GPU and the rest ran on the
    // CPU, at CPU speed, while `rime ai pull` had just printed "cannot plan a
    // partial offload for it" about that same model. The two halves of the
    // product disagreed and the slower one was the one that ran.
    //
    // Unknown layers therefore means: offload EVERYTHING or nothing. A split
    // needs a per-layer cost, and `weights_mib / layers` is not a per-layer
    // cost when the denominator is a guess.
    let unknown_layers = layers == 0;
    let layers = if unknown_layers { ALL_LAYERS_UNKNOWN } else { layers };

    // How the notes name the layer count, since "all 999 layers" would be a
    // worse lie than the one being fixed.
    let all_layers = if unknown_layers {
        "every layer (this model declares no layer count)".to_string()
    } else {
        format!("all {layers} layers")
    };

    let kv_for = |ctx: u32| -> u64 {
        kv_mib_per_1k.saturating_mul(u64::from(ctx).div_ceil(1024))
    };
    let need_for = |ctx: u32| -> u64 {
        VRAM_OVERHEAD_MIB
            .saturating_add(weights_mib)
            .saturating_add(kv_for(ctx))
    };

    // Full offload at the requested context.
    if need_for(want_context) <= budget_mib {
        notes.push(format!(
            "{all_layers} fit: {} MiB needed of {budget_mib} MiB available \
             ({VRAM_OVERHEAD_MIB} MiB runtime + {weights_mib} MiB weights + {} MiB KV for \
             {want_context} tokens)",
            need_for(want_context),
            kv_for(want_context)
        ));
        return Fit {
            placement: Placement::Gpu { layers },
            context: want_context,
            vram_mib: need_for(want_context),
            budget_mib,
            notes,
        };
    }

    // Halve the context while that alone would make it fit.
    let mut ctx = want_context;
    while ctx / 2 >= MIN_USEFUL_CONTEXT {
        ctx /= 2;
        if need_for(ctx) <= budget_mib {
            notes.push(format!(
                "context reduced from {want_context} to {ctx} tokens so that {all_layers} \
                 still fit — a smaller context on the GPU beats a full context split \
                 with system RAM"
            ));
            return Fit {
                placement: Placement::Gpu { layers },
                context: ctx,
                vram_mib: need_for(ctx),
                budget_mib,
                notes,
            };
        }
    }

    // A split needs to know what one layer costs. With no declared layer count
    // there is no honest way to compute that, so the answer is the CPU — and
    // saying why, because "nothing is offloaded" on a machine with a capable
    // GPU is otherwise indistinguishable from a detection failure.
    //
    // This is also exactly what `rime ai pull` promises the user when it writes
    // such a manifest: "it will load, but `rime ai status` cannot plan a
    // partial offload for it".
    if unknown_layers {
        notes.push(format!(
            "nothing is offloaded: this model declares no layer count, so the cost of one \
             layer is unknown and a partial offload cannot be planned. It needs {} MiB for a \
             full offload and {budget_mib} MiB is available — `rime ai pull` from the \
             catalogue, or a manifest with a layer count, would allow a split",
            need_for(MIN_USEFUL_CONTEXT.min(want_context))
        ));
        return Fit {
            placement: Placement::Cpu,
            context: want_context,
            vram_mib: 0,
            budget_mib,
            notes,
        };
    }

    // Split. At the smallest useful context, so the layers get what is left.
    let ctx = MIN_USEFUL_CONTEXT.min(want_context);
    let fixed = VRAM_OVERHEAD_MIB.saturating_add(kv_for(ctx));
    let per_layer = weights_mib.div_ceil(u64::from(layers)).max(1);
    let for_layers = budget_mib.saturating_sub(fixed);
    let mut fit_layers = (for_layers / per_layer).min(u64::from(layers)) as u32;
    fit_layers = fit_layers.saturating_sub(SPLIT_MARGIN_LAYERS);

    if fit_layers == 0 {
        notes.push(format!(
            "nothing is offloaded: {budget_mib} MiB available does not cover \
             {VRAM_OVERHEAD_MIB} MiB of runtime overhead plus {} MiB of KV cache for \
             {ctx} tokens plus one {per_layer} MiB layer",
            kv_for(ctx)
        ));
        return Fit {
            placement: Placement::Cpu,
            context: want_context,
            vram_mib: 0,
            budget_mib,
            notes,
        };
    }

    notes.push(format!(
        "split: {fit_layers} of {layers} layers on the GPU, the rest in system RAM. \
         {per_layer} MiB per layer is weights/layers, which overstates a repeating block \
         slightly, so {SPLIT_MARGIN_LAYERS} layer of margin is left unspent — a computed \
         split that is one layer too greedy fails at startup rather than running slowly"
    ));
    if ctx < want_context {
        notes.push(format!(
            "context also reduced to {ctx} tokens, the smallest this build treats as useful"
        ));
    }
    Fit {
        placement: Placement::Split { layers: fit_layers, total: layers },
        context: ctx,
        vram_mib: fixed.saturating_add(u64::from(fit_layers).saturating_mul(per_layer)),
        budget_mib,
        notes,
    }
}

/// The smallest context this build will reduce to before giving up layers.
///
/// 2048 tokens: below that a model cannot hold a system prompt, a file and a
/// question at once, which is the minimum an agent client needs to be worth
/// calling at all.
pub const MIN_USEFUL_CONTEXT: u32 = 2048;

/// Layers left unspent on the split path. See [`plan_fit`].
pub const SPLIT_MARGIN_LAYERS: u32 = 1;

/// The `-ngl` value that means "all of them" when the real count is unknown.
///
/// `llama-server` clamps `--n-gpu-layers` to the model's actual layer count, so
/// any value above every plausible count offloads the whole model and nothing
/// more. This is what [`plan_fit`] uses for a model whose manifest declares no
/// layer count — `rime ai pull --url` writes `layers: 0` for everything outside
/// the catalogue, and 0 there means UNKNOWN, not "one layer".
pub const ALL_LAYERS_UNKNOWN: u32 = 999;

// ── idle unloading ───────────────────────────────────────────────────────────

/// What the daemon knows when it wonders whether to unload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleInputs {
    /// A backend is running.
    pub loaded: bool,
    /// Clients currently attached to the API socket.
    pub open_connections: u32,
    /// Seconds since the last byte moved in either direction.
    pub idle_secs: u64,
    /// The user's `[ai] idle_timeout`, if set. `Some(0)` means never.
    pub configured_timeout: Option<u64>,
    /// Whether the machine is on battery.
    pub on_battery: bool,
}

/// What to do about an idle backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleDecision {
    /// Leave it running, with the reason.
    Keep(String),
    /// Stop it, with the reason.
    Unload(String),
}

impl IdleDecision {
    /// Whether this decision stops the backend.
    pub fn unloads(&self) -> bool {
        matches!(self, IdleDecision::Unload(_))
    }

    /// The reason, either way.
    pub fn reason(&self) -> &str {
        match self {
            IdleDecision::Keep(r) | IdleDecision::Unload(r) => r,
        }
    }
}

/// The idle timeout in force, given the settings and the power source.
///
/// A configured value wins on both power sources: someone who wrote a number
/// meant it, and quietly halving it on battery would be the silent policy
/// downgrade `AGENTS.md` forbids. Absent, the default is
/// [`IDLE_TIMEOUT_AC_SECS`] or [`IDLE_TIMEOUT_BATTERY_SECS`].
pub fn idle_timeout(configured: Option<u64>, on_battery: bool) -> u64 {
    match configured {
        Some(t) => t,
        None if on_battery => IDLE_TIMEOUT_BATTERY_SECS,
        None => IDLE_TIMEOUT_AC_SECS,
    }
}

/// Whether to stop an idle backend.
///
/// Pure. The daemon calls this on a timer and acts on the answer, so every rule
/// below is unit-testable without a GPU, a model or a clock.
pub fn plan_idle(i: &IdleInputs) -> IdleDecision {
    if !i.loaded {
        return IdleDecision::Keep("nothing is loaded".to_string());
    }
    // An open connection outranks everything, including a zero timeout. A
    // client may be mid-generation with no bytes moving — a long prompt
    // evaluation produces nothing for seconds — and unloading under it would
    // truncate an answer to save power.
    if i.open_connections > 0 {
        return IdleDecision::Keep(format!(
            "{} client{} attached; idle time only counts once the last one leaves",
            i.open_connections,
            if i.open_connections == 1 { " is" } else { "s are" }
        ));
    }
    let timeout = idle_timeout(i.configured_timeout, i.on_battery);
    if timeout == 0 {
        return IdleDecision::Keep(
            "idle_timeout = 0, so the model stays resident until you unload it".to_string(),
        );
    }
    if i.idle_secs < timeout {
        return IdleDecision::Keep(format!(
            "idle {}s of {timeout}s{}",
            i.idle_secs,
            if i.on_battery && i.configured_timeout.is_none() {
                " (the battery timeout — a resident model keeps the GPU out of its lowest \
                 power state)"
            } else {
                ""
            }
        ));
    }
    IdleDecision::Unload(format!(
        "idle {}s, past the {timeout}s timeout{}; VRAM is released and the next request \
         reloads it",
        i.idle_secs,
        if i.on_battery && i.configured_timeout.is_none() {
            " that applies on battery"
        } else {
            ""
        }
    ))
}

// ── runtimes ─────────────────────────────────────────────────────────────────

/// An inference runtime Rime can put behind its endpoint.
///
/// The abstraction §14 asks for. One variant is implemented and the other two
/// are **recognised in order to be refused**, with the reason — the same idiom
/// [`crate::host`] uses for `identity_file`. A variant that silently produced no
/// plan would be a runtime the store could name and the daemon could never
/// start, and `every_runtime_either_launches_or_explains` asserts that cannot
/// happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Runtime {
    /// `llama-server` from llama.cpp. Implemented.
    LlamaCpp,
    /// Ollama. Recognised, not launched — see [`Runtime::unsupported_because`].
    Ollama,
    /// vLLM. Recognised, not launched.
    Vllm,
}

impl Runtime {
    /// Every runtime. Used by the exhaustiveness tests, so adding a variant
    /// without deciding what it does is a test failure rather than a silent
    /// gap.
    pub const ALL: [Runtime; 3] = [Runtime::LlamaCpp, Runtime::Ollama, Runtime::Vllm];

    /// The stable id used in manifests, the catalogue and output.
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::LlamaCpp => "llama.cpp",
            Runtime::Ollama => "ollama",
            Runtime::Vllm => "vllm",
        }
    }

    /// Every id, for a refusal that lists the real ones.
    pub fn all_ids() -> Vec<&'static str> {
        Runtime::ALL.iter().map(|r| r.as_str()).collect()
    }

    /// The program name the daemon looks for on `PATH`.
    pub fn program(self) -> &'static str {
        match self {
            Runtime::LlamaCpp => "llama-server",
            Runtime::Ollama => "ollama",
            Runtime::Vllm => "vllm",
        }
    }

    /// Why this runtime is not launched, or `None` when it is.
    ///
    /// Ollama and vLLM are refused for the same structural reason and it is not
    /// effort: each owns its own model store and its own long-lived daemon.
    /// Adopting either would give the machine two stores with two provenance
    /// stories and two idle policies, and §14's store is the one that is
    /// root-owned, digest-verified and read-only to the inference process.
    /// A runtime Rime launches has to be one it can point at a file.
    pub fn unsupported_because(self) -> Option<&'static str> {
        match self {
            Runtime::LlamaCpp => None,
            Runtime::Ollama => Some(
                "ollama keeps its own model store under ~/.ollama and runs its own daemon, so \
                 Rime cannot point it at a verified, read-only blob in /var/lib/rime/ai — the \
                 machine would end up with two stores, two provenance stories and two idle \
                 policies. Use `ollama` directly if you want that; `rime ai` will not pretend \
                 to own it",
            ),
            Runtime::Vllm => Some(
                "vLLM loads a HuggingFace model directory and expects a Python environment \
                 rather than a single file, which is a capsule's job (`rime env create cuda`) \
                 and not something the store models yet",
            ),
        }
    }

    /// Whether this runtime can be told to listen on a Unix socket.
    ///
    /// **Measured, not assumed.** `llama-server --help` from
    /// `ghcr.io/ggml-org/llama.cpp:server` (version `0.3.0-dev`, build 10775,
    /// commit `67a17c17c`) says:
    ///
    /// ```text
    /// --host HOST     ip address to listen, or bind to an UNIX socket if the address ends
    ///                 with .sock (default: 127.0.0.1)
    /// ```
    ///
    /// That is the whole reason [`BACKEND_SOCKET`] is named `backend.sock` and
    /// [`Runtime::listen_suffix_required`] exists: the suffix is not
    /// decoration, it is how the flag selects an address family.
    pub fn supports_unix_socket(self) -> bool {
        matches!(self, Runtime::LlamaCpp)
    }

    /// The filename suffix a Unix socket path must have for this runtime to
    /// bind it rather than parse it as an address.
    pub fn listen_suffix_required(self) -> Option<&'static str> {
        match self {
            Runtime::LlamaCpp => Some(".sock"),
            _ => None,
        }
    }

    /// The exact command that installs this runtime, for the backend named.
    ///
    /// This is the entirety of Rime's involvement in getting a runtime onto the
    /// machine. Nothing in `rimed-core`, `rime` or `rime-aid` downloads one:
    /// `AGENTS.md` forbids a second package mechanism, and P1 already built
    /// both of the ones named here.
    pub fn install_hint(self, backend: Backend) -> String {
        match (self, backend) {
            // CUDA and ROCm builds pull in a vendor toolkit, which is exactly
            // the multi-gigabyte content that must not reach the host image.
            (Runtime::LlamaCpp, Backend::Cuda) => {
                "rime env create cuda && rime env install cuda llama-cpp".to_string()
            }
            (Runtime::LlamaCpp, Backend::Rocm) => {
                "rime env create rocm && rime env install rocm llama-cpp".to_string()
            }
            // Vulkan and CPU builds are ordinary RPMs, so the system extension
            // engine serves them and the runtime lands at /usr/bin.
            (Runtime::LlamaCpp, _) => "sudo rime install llama-cpp".to_string(),
            (Runtime::Ollama, _) | (Runtime::Vllm, _) => {
                format!("{} is not launched by rime ai", self.as_str())
            }
        }
    }
}

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Runtime {
    type Err = AiError;

    fn from_str(s: &str) -> Result<Runtime, AiError> {
        match s {
            "llama.cpp" | "llama-cpp" | "llamacpp" | "gguf" => Ok(Runtime::LlamaCpp),
            "ollama" => Ok(Runtime::Ollama),
            "vllm" => Ok(Runtime::Vllm),
            _ => Err(AiError::BadModelId {
                id: s.to_string(),
                why: format!("runtime must be one of: {}", Runtime::all_ids().join(", ")),
            }),
        }
    }
}

// ── the launch plan ──────────────────────────────────────────────────────────

/// Where a backend child is told to listen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// A filesystem path. The default, and the only one this build produces.
    ///
    /// A path-named `AF_UNIX` socket is not affected by network namespaces —
    /// only the abstract namespace is — so the child can be started with no
    /// network reachability at all and still be reached by its parent through
    /// the filesystem. No port exists for another account to find.
    Unix(PathBuf),
    /// A loopback port. Modelled so the refusal can be specific, and never
    /// produced by [`plan_launch`]: see [`refuse_tcp_endpoint`].
    Loopback { port: u16 },
}

impl Listen {
    /// How this address is spelled for a runtime's `--host`.
    pub fn as_host_arg(&self) -> String {
        match self {
            Listen::Unix(p) => p.display().to_string(),
            Listen::Loopback { .. } => "127.0.0.1".to_string(),
        }
    }
}

/// Everything [`plan_launch`] needs. A struct rather than eight arguments,
/// because the failure mode of eight arguments is two of the same type in the
/// wrong order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest<'a> {
    /// Which runtime.
    pub runtime: Runtime,
    /// Resolved program path or bare name, as the caller found it.
    pub program: &'a str,
    /// The verified blob to load.
    pub model_path: &'a Path,
    /// Where to listen.
    pub listen: Listen,
    /// The fit this launch implements.
    pub fit: &'a Fit,
    /// The chosen backend and device.
    pub choice: &'a BackendChoice,
    /// CPU threads for the layers that stay in system RAM. `0` lets the runtime
    /// decide.
    pub threads: u32,
}

/// The argv to run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The program.
    pub program: String,
    /// Arguments, in order, one per element. Never a shell string — the daemon
    /// spawns this directly, so there is nothing to quote and nothing to
    /// misquote.
    pub argv: Vec<String>,
    /// Environment to set, on top of a cleared base.
    pub env: Vec<(String, String)>,
    /// Where the child will be reachable.
    pub listen: Listen,
    /// Why the plan is shaped this way.
    pub notes: Vec<String>,
}

impl LaunchPlan {
    /// The whole command as one readable line, for logs and `--explain`.
    /// Rendering only — nothing parses this back.
    pub fn describe(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.argv {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

/// Build the argv for a runtime.
///
/// Pure: the daemon's start path and `rime ai status --explain` call this once
/// each and differ only in whether the result reaches a `Command`.
///
/// Refuses rather than improvises in three cases, and each refusal is a
/// property worth keeping:
///
/// * a runtime with no launcher ([`Runtime::unsupported_because`]);
/// * a TCP listen address, always ([`refuse_tcp_endpoint`]);
/// * a Unix socket whose name would make the runtime parse it as an IP address
///   instead of binding it — for `llama-server`, a path not ending `.sock`.
///   That one is the difference between "no service" and "a port on 127.0.0.1
///   that any local account can open", so it is refused rather than fixed up.
pub fn plan_launch(req: &LaunchRequest<'_>) -> Result<LaunchPlan, AiError> {
    if let Some(why) = req.runtime.unsupported_because() {
        return Err(AiError::Refused { key: "runtime", because: why });
    }
    let Listen::Unix(sock) = &req.listen else {
        return Err(refuse_tcp_endpoint("listen"));
    };
    if let Some(suffix) = req.runtime.listen_suffix_required() {
        if !sock.to_string_lossy().ends_with(suffix) {
            return Err(AiError::BadUrl {
                url: sock.display().to_string(),
                why: format!(
                    "{} binds a Unix socket only when --host ends with {suffix:?}; anything \
                     else is parsed as an IP address, which would put the backend on a \
                     loopback port every account on this machine could open",
                    req.runtime.program()
                ),
            });
        }
    }

    let mut notes = Vec::new();
    let mut argv: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();

    match req.runtime {
        Runtime::LlamaCpp => {
            argv.push("--model".into());
            argv.push(req.model_path.display().to_string());
            argv.push("--host".into());
            argv.push(req.listen.as_host_arg());
            notes.push(
                "--host is a path ending in .sock, so llama-server binds an AF_UNIX socket \
                 rather than a port. Verified against llama.cpp build 10775: \"ip address to \
                 listen, or bind to an UNIX socket if the address ends with .sock\""
                    .to_string(),
            );
            argv.push("--n-gpu-layers".into());
            argv.push(req.fit.placement.gpu_layers().to_string());
            argv.push("--ctx-size".into());
            argv.push(req.fit.context.to_string());
            if req.threads > 0 {
                argv.push("--threads".into());
                argv.push(req.threads.to_string());
            }
            if let Some(dev) = req.choice.device {
                if req.choice.backend.uses_gpu() {
                    argv.push("--main-gpu".into());
                    argv.push(dev.to_string());
                }
            }
            match req.choice.backend {
                Backend::Cpu => {
                    notes.push(
                        "no device is selected and --n-gpu-layers is 0, so the build's GPU \
                         support (if any) is unused rather than partially engaged"
                            .to_string(),
                    );
                }
                b => notes.push(format!(
                    "{b} was chosen: {}",
                    req.choice.why
                )),
            }
            // The one environment variable, and it is a refusal rather than a
            // convenience: llama-server reads LLAMA_ARG_* for most of its
            // flags, so a value inherited from the user's shell could override
            // the plan silently. Setting the two that matter to the planned
            // value makes the argv authoritative either way.
            env.push(("LLAMA_ARG_HOST".into(), req.listen.as_host_arg()));
            env.push(("LLAMA_ARG_MODEL".into(), req.model_path.display().to_string()));
            notes.push(
                "LLAMA_ARG_HOST and LLAMA_ARG_MODEL are set to the planned values because \
                 llama-server accepts both as environment variables, so an inherited one \
                 could otherwise move the socket or the model out from under the plan"
                    .to_string(),
            );
        }
        // Unreachable: refused above. A match arm rather than a wildcard, so
        // adding a runtime is a compile error here.
        Runtime::Ollama | Runtime::Vllm => {
            return Err(AiError::Refused {
                key: "runtime",
                because: "no launcher for this runtime",
            })
        }
    }

    for n in &req.fit.notes {
        notes.push(n.clone());
    }

    Ok(LaunchPlan {
        program: req.program.to_string(),
        argv,
        env,
        listen: req.listen.clone(),
        notes,
    })
}

// ── the pull plan ────────────────────────────────────────────────────────────

/// How a user named a model to pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullSpec {
    /// A catalogue id: `qwen3-coder`.
    Catalogue { id: String },
    /// A catalogue id with the digest spelled out: `qwen3-coder@sha256:…`. The
    /// digest must match the catalogue's, so this is a way of *asserting* what
    /// you expect rather than a way of overriding it.
    Pinned { id: String, digest: String },
    /// A URL with a digest the user vouches for.
    Url { id: String, url: String, digest: String },
}

/// Why a pull was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullError {
    /// The spec itself is malformed.
    Spec(AiError),
    /// A URL with no digest. The refusal §14's provenance story rests on.
    NoDigest { url: String },
    /// A name that is not in the catalogue and is not a URL.
    NotInCatalogue { id: String, known: Vec<String> },
    /// A pinned digest that disagrees with the catalogue's.
    DigestMismatch { id: String, asked: String, catalogue: String },
}

impl fmt::Display for PullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spec(e) => write!(f, "{e}"),
            Self::NoDigest { url } => write!(
                f,
                "refusing to pull {url:?} without a digest.\n  \
                 Verifying a download against a digest the same server handed you proves only \
                 that it sent the same bytes twice. Either name a model from the catalogue \
                 (`rime ai models --available`), whose digests ship inside the signed image, \
                 or say what you expect:\n    \
                 rime ai pull <name> --url {url} --digest sha256:<64 hex>"
            ),
            Self::NotInCatalogue { id, known } => {
                if known.is_empty() {
                    write!(
                        f,
                        "no model named {id:?}, and this image ships no catalogue. Pull an \
                         explicit URL with --digest, or check that \
                         /usr/share/rimeos/ai/catalogue.toml is present"
                    )
                } else {
                    write!(
                        f,
                        "no model named {id:?} in the catalogue. Available: {}",
                        known.join(", ")
                    )
                }
            }
            Self::DigestMismatch { id, asked, catalogue } => write!(
                f,
                "you asked for {id}@{asked}, but the catalogue in this image says {id} is \
                 {catalogue}. Refusing rather than picking one: a digest you typed and a \
                 digest the image ships disagreeing is either a stale command line or an \
                 image that is not the one you think it is"
            ),
        }
    }
}

impl std::error::Error for PullError {}

/// Everything a pull will do, decided before a byte is fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPlan {
    /// The id the model will be installed as.
    pub id: String,
    /// Where the bytes come from.
    pub url: String,
    /// The digest that must match.
    pub digest: String,
    /// Expected size in MiB, when known.
    pub weights_mib: u64,
    /// Download target — verified here, then renamed.
    pub staging: PathBuf,
    /// Final content-addressed path.
    pub blob: PathBuf,
    /// Manifest to write.
    pub manifest: PathBuf,
    /// True when the digest came from the person typing rather than from the
    /// image.
    pub user_supplied_digest: bool,
    /// Why this plan.
    pub notes: Vec<String>,
}

/// Parse a `pull` argument into a spec.
///
/// Accepts `name`, `name@sha256:<hex>`, and — only when `url` and `digest` are
/// both supplied — an explicit source. A bare URL is [`PullError::NoDigest`].
pub fn parse_pull_spec(
    name: &str,
    url: Option<&str>,
    digest: Option<&str>,
) -> Result<PullSpec, PullError> {
    // A URL in the name position is the mistake worth naming precisely: it is
    // what someone copying a HuggingFace link will type.
    if name.starts_with("https://") || name.starts_with("http://") {
        return Err(PullError::NoDigest { url: name.to_string() });
    }

    let (id, inline_digest) = match name.split_once('@') {
        Some((i, d)) => (i, Some(d)),
        None => (name, None),
    };
    validate_model_id(id).map_err(PullError::Spec)?;

    let digest = match (inline_digest, digest) {
        (Some(a), Some(b)) if a != b => {
            return Err(PullError::DigestMismatch {
                id: id.to_string(),
                asked: a.to_string(),
                catalogue: b.to_string(),
            })
        }
        (Some(d), _) | (None, Some(d)) => Some(d),
        (None, None) => None,
    };
    if let Some(d) = digest {
        validate_digest(d).map_err(PullError::Spec)?;
    }

    match (url, digest) {
        (Some(u), Some(d)) => {
            validate_url(u).map_err(PullError::Spec)?;
            Ok(PullSpec::Url {
                id: id.to_string(),
                url: u.to_string(),
                digest: d.to_string(),
            })
        }
        (Some(u), None) => Err(PullError::NoDigest { url: u.to_string() }),
        (None, Some(d)) => Ok(PullSpec::Pinned { id: id.to_string(), digest: d.to_string() }),
        (None, None) => Ok(PullSpec::Catalogue { id: id.to_string() }),
    }
}

/// Turn a spec and the image's catalogue into a plan, or a refusal.
///
/// Every path in the result comes from [`Store`], so a plan cannot name a file
/// outside the store even if the id somehow survived validation.
pub fn plan_pull(
    spec: &PullSpec,
    catalogue: &Catalogue,
    store: &Store,
) -> Result<PullPlan, PullError> {
    let mut notes = Vec::new();

    let (id, url, digest, weights_mib, user_supplied) = match spec {
        PullSpec::Catalogue { id } | PullSpec::Pinned { id, .. } => {
            let entry = catalogue.get(id).ok_or_else(|| PullError::NotInCatalogue {
                id: id.clone(),
                known: catalogue.ids().iter().map(|s| s.to_string()).collect(),
            })?;
            if let PullSpec::Pinned { digest, .. } = spec {
                if *digest != entry.digest {
                    return Err(PullError::DigestMismatch {
                        id: id.clone(),
                        asked: digest.clone(),
                        catalogue: entry.digest.clone(),
                    });
                }
                notes.push(
                    "the digest you named matches the one the image ships, so this pull is \
                     pinned to exactly what you asked for"
                        .to_string(),
                );
            }
            notes.push(format!(
                "digest comes from {CATALOGUE_PATH}, which is image content: signed with the \
                 image, pinned by digest in CI, and rolled back with the OS"
            ));
            (id.clone(), entry.url.clone(), entry.digest.clone(), entry.weights_mib, false)
        }
        PullSpec::Url { id, url, digest } => {
            notes.push(
                "the digest came from your command line, not from the image. `rime ai models` \
                 keeps saying so, because nothing else vouches for these weights"
                    .to_string(),
            );
            (id.clone(), url.clone(), digest.clone(), 0, true)
        }
    };

    let staging = store.staging(&digest).map_err(PullError::Spec)?;
    let blob = store.blob(&digest).map_err(PullError::Spec)?;
    let manifest = store.manifest(&id).map_err(PullError::Spec)?;

    notes.push(format!(
        "download to {} and verify there, then rename to {} — an atomic rename within one \
         filesystem, so a partial or wrong-digest file is never visible under its final \
         content-addressed name",
        staging.display(),
        blob.display()
    ));

    Ok(PullPlan {
        id,
        url,
        digest,
        weights_mib,
        staging,
        blob,
        manifest,
        user_supplied_digest: user_supplied,
        notes,
    })
}

// ── the daemon's endpoints ───────────────────────────────────────────────────

/// The daemon's socket paths, given a runtime directory.
///
/// Takes `$XDG_RUNTIME_DIR` as an argument rather than reading it, so this
/// module stays free of environment as well as of I/O — and so a test can point
/// it anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    dir: PathBuf,
}

impl Endpoints {
    /// Endpoints under `<runtime_dir>/rime-ai`.
    pub fn new(runtime_dir: &Path) -> Endpoints {
        Endpoints { dir: runtime_dir.join(RUNTIME_SUBDIR) }
    }

    /// The directory holding them. Created `0700`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Line-framed JSON control endpoint.
    pub fn control(&self) -> PathBuf {
        self.dir.join(CONTROL_SOCKET)
    }

    /// The inference endpoint applications connect to.
    pub fn api(&self) -> PathBuf {
        self.dir.join(API_SOCKET)
    }

    /// Where the backend child is told to listen. Inside the same `0700`
    /// directory, and named `.sock` because [`Runtime::LlamaCpp`] selects its
    /// address family from that suffix.
    pub fn backend(&self) -> PathBuf {
        self.dir.join(BACKEND_SOCKET)
    }
}

// ── the control protocol ─────────────────────────────────────────────────────
//
// Two endpoints, and the split is the design rather than a convenience:
//
// * [`API_SOCKET`] carries the **backend's own HTTP API, unmodified**. The
//   daemon relays bytes and parses nothing, which is what lets an
//   OpenAI-compatible client — an editor plugin, an agent, `curl` — connect
//   with no Rime-specific handshake. It is also why the daemon needs no HTTP
//   implementation of its own: a proxy that parsed requests would be a second
//   HTTP stack to keep correct, and every header it failed to forward would be
//   a bug in somebody else's client.
// * [`CONTROL_SOCKET`] carries the types below, one JSON object per line, in
//   the same shape as `rime_agent_core::protocol`. It exists because "which
//   model is loaded, on which backend, with how much VRAM" is a Rime question
//   that no backend API answers.
//
// The framing is line-based, so a serialised request or response must never
// contain a raw newline. `serde_json` escapes them inside strings, and
// `no_message_can_contain_a_raw_newline` asserts it over a message built from
// hostile text rather than trusting that.

/// A request on the control socket.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Version handshake. Always answered, even by a daemon that would refuse
    /// everything else, so a client can report a skew rather than a timeout.
    Hello,
    /// Everything `rime ai status` prints.
    Status,
    /// Installed models, read from the store.
    Models,
    /// Make a model the one the next API connection loads. Unloads a different
    /// one that is currently resident, because VRAM holds one at a time.
    Select { model: String },
    /// Stop the backend now, releasing VRAM.
    Unload,
}

/// A response on the control socket.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Response {
    /// Answer to [`Request::Hello`].
    Hello { version: u32, api_socket: String },
    /// Answer to [`Request::Status`]. Boxed because it is much larger than
    /// every other variant and clippy's `large_enum_variant` is right.
    Status(Box<Status>),
    /// Answer to [`Request::Models`].
    Models { models: Vec<ModelInfo> },
    /// A request that succeeded and has nothing to report.
    Ok,
    /// A refusal, with a machine-readable kind so the CLI can choose an exit
    /// code without matching on prose.
    Error { kind: ErrorKind, message: String },
}

impl Response {
    /// A refusal.
    pub fn error(kind: ErrorKind, message: impl Into<String>) -> Response {
        Response::Error { kind, message: message.into() }
    }
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Unparseable, or a value that failed validation.
    BadRequest,
    /// The model is not in the store.
    NoSuchModel,
    /// No inference runtime is installed. Carries the install command in the
    /// message, because the answer to this is always a command to type.
    NoRuntime,
    /// The backend would not start, or died.
    BackendFailed,
    /// A protocol version this daemon cannot speak.
    Protocol,
    /// Anything else, including an I/O failure.
    Internal,
}

/// One installed model, as `rime ai models` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelInfo {
    /// The id.
    pub id: String,
    /// `sha256:<hex>`.
    pub digest: String,
    /// Size on disk.
    pub weights_mib: u64,
    /// Which runtime loads it.
    pub runtime: String,
    /// Largest context the weights support.
    pub max_context: u32,
    /// Whether the blob named by the manifest is actually present. A manifest
    /// with no blob is what a half-finished `rime ai rm` leaves, and reporting
    /// it beats printing a model that cannot load.
    ///
    /// `false` when confirmed absent, and also when the stat could not be
    /// completed — the two are told apart by `present_unavailable`. Never
    /// `true` off a read that never happened.
    pub present: bool,
    /// Set when `present` could not be determined: the blob's own stat was
    /// refused rather than answered. `rime ai models` shows this instead of
    /// "WEIGHTS MISSING", because a permission refusal is not the same
    /// finding and re-pulling will not fix it.
    #[serde(default)]
    pub present_unavailable: Option<String>,
    /// Whether the digest came from the person typing rather than the image.
    pub user_supplied_digest: bool,
    /// Whether this is the selected model.
    pub selected: bool,
    /// Whether it is resident right now.
    pub loaded: bool,
}

/// One accelerator, as `rime ai status` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct DeviceInfo {
    /// Index as the runtime numbers them.
    pub index: u32,
    /// Name.
    pub name: String,
    /// Total VRAM.
    pub total_mib: u64,
    /// VRAM in use.
    pub used_mib: u64,
    /// What [`Device::budget_mib`] leaves to spend.
    pub budget_mib: u64,
}

impl From<&Device> for DeviceInfo {
    fn from(d: &Device) -> DeviceInfo {
        DeviceInfo {
            index: d.index,
            name: d.name.clone(),
            total_mib: d.total_mib,
            used_mib: d.used_mib,
            budget_mib: d.budget_mib(),
        }
    }
}

/// The daemon's whole observable state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Status {
    /// Protocol this daemon speaks.
    pub protocol: u32,
    /// Where applications connect.
    pub api_socket: String,
    /// The model store in use.
    pub store: String,
    /// The model the next connection would load.
    pub selected: Option<String>,
    /// The model resident now, if any.
    pub loaded: Option<String>,
    /// Resolved runtime program, when one was found.
    pub runtime_path: Option<String>,
    /// Which runtime that is.
    pub runtime: Option<String>,
    /// The command to type when no runtime is installed. `None` when one is.
    pub install_hint: Option<String>,
    /// Chosen compute backend.
    pub backend: Option<String>,
    /// Chosen device index.
    pub device: Option<u32>,
    /// Backends this machine can run, best first.
    pub accel: Vec<String>,
    /// Accelerators found.
    pub devices: Vec<DeviceInfo>,
    /// Clients attached to the API socket.
    pub open_connections: u32,
    /// Seconds since the last byte moved.
    pub idle_secs: u64,
    /// The timeout in force.
    pub idle_timeout: u64,
    /// Whether the machine is on battery.
    pub on_battery: bool,
    /// Layers planned on the GPU, and the model's total.
    pub gpu_layers: Option<u32>,
    /// Total layers.
    pub total_layers: Option<u32>,
    /// Context in force.
    pub context: Option<u32>,
    /// VRAM the resident model is expected to occupy.
    pub vram_mib: Option<u64>,
    /// Why the plan is what it is. Every selection and fit note, in order.
    pub notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat_entry() -> CatalogueEntry {
        CatalogueEntry {
            title: "Qwen3 Coder 7B".into(),
            url: "https://example.invalid/qwen3-coder-q4.gguf".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            weights_mib: 4096,
            layers: 32,
            kv_mib_per_1k: 32,
            max_context: 32768,
            quant: "Q4_K_M".into(),
            license: "Apache-2.0".into(),
            runtime: "llama.cpp".into(),
            note: None,
        }
    }

    fn catalogue() -> Catalogue {
        let mut model = BTreeMap::new();
        model.insert("qwen3-coder".to_string(), cat_entry());
        Catalogue { version: Some(SCHEMA_VERSION), model }
    }

    fn dev(index: u32, total: u64, used: u64) -> Device {
        Device { index, name: format!("gpu{index}"), total_mib: total, used_mib: used }
    }

    // ── model ids ────────────────────────────────────────────────────────────

    #[test]
    fn ordinary_model_ids_are_accepted() {
        for id in ["qwen3-coder", "llama3.2", "phi_4", "gemma2-2b", "a", "q4"] {
            assert!(validate_model_id(id).is_ok(), "{id} was refused");
        }
    }

    #[test]
    fn path_traversal_ids_are_refused() {
        // The id is a path component under models/manifests/.
        for bad in ["..", ".", "../../etc/passwd", "a/b", "/abs"] {
            assert!(validate_model_id(bad).is_err(), "{bad:?} was accepted");
        }
        // And the slash gets its own reason, because that is the one someone
        // types by pasting a HuggingFace repo name.
        let e = validate_model_id("qwen/qwen3").unwrap_err().to_string();
        assert!(e.contains("path component"), "{e}");
    }

    #[test]
    fn shell_and_argv_metacharacters_are_refused_in_ids() {
        for bad in ["a b", "a\nb", "a;b", "a$b", "a`b", "a|b", "a&b", "a'b", "a\"b", "-rf"] {
            assert!(validate_model_id(bad).is_err(), "{bad:?} was accepted");
        }
        let e = validate_model_id("-rf").unwrap_err().to_string();
        assert!(e.contains("read as a flag"), "{e}");
    }

    #[test]
    fn an_uppercase_id_is_refused_with_the_lowercase_form_named() {
        // Not merely "invalid": someone typing a model's real capitalisation
        // must be told the fix, not the character class.
        let e = validate_model_id("Qwen3-Coder").unwrap_err();
        assert!(matches!(e, AiError::UppercaseModelId { .. }), "got {e:?}");
        assert!(e.to_string().contains("\"qwen3-coder\""), "got {e}");
    }

    #[test]
    fn an_empty_or_overlong_id_is_refused() {
        assert!(validate_model_id("").is_err());
        assert!(validate_model_id(&"a".repeat(MAX_MODEL_ID)).is_ok());
        assert!(validate_model_id(&"a".repeat(MAX_MODEL_ID + 1)).is_err());
    }

    // ── digests ──────────────────────────────────────────────────────────────

    #[test]
    fn a_digest_is_sha256_and_sixty_four_lowercase_hex() {
        assert!(validate_digest(&format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(validate_digest(&format!("sha256:{}", "abcdef0123456789".repeat(4))).is_ok());
    }

    #[test]
    fn a_digest_of_the_wrong_shape_is_refused() {
        for bad in [
            "",
            "sha256:",
            &format!("sha256:{}", "a".repeat(63)),
            &format!("sha256:{}", "a".repeat(65)),
            &format!("sha512:{}", "a".repeat(64)),
            // A bare hex string with no algorithm prefix.
            &"a".repeat(64),
            // Uppercase hex would give one blob two filenames, which defeats
            // content-addressing.
            &format!("sha256:{}", "A".repeat(64)),
            // Not hex.
            &format!("sha256:{}", "g".repeat(64)),
        ] {
            assert!(validate_digest(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_blob_name_replaces_only_the_first_colon() {
        let d = format!("sha256:{}", "b".repeat(64));
        assert_eq!(blob_name(&d).unwrap(), format!("sha256-{}", "b".repeat(64)));
        assert!(blob_name("sha256:short").is_err());
    }

    // ── urls ─────────────────────────────────────────────────────────────────

    #[test]
    fn plain_http_is_refused_and_says_why_the_digest_does_not_save_it() {
        let e = validate_url("http://example.invalid/m.gguf").unwrap_err().to_string();
        assert!(e.contains("plain HTTP"), "{e}");
        assert!(e.contains("not independent"), "{e}");
    }

    #[test]
    fn urls_with_credentials_or_whitespace_are_refused() {
        assert!(validate_url("https://user:pw@example.invalid/m.gguf").is_err());
        assert!(validate_url("https://example.invalid/a b.gguf").is_err());
        assert!(validate_url("https://example.invalid/a\nb").is_err());
        assert!(validate_url("https://").is_err());
        assert!(validate_url("ftp://example.invalid/m").is_err());
        assert!(validate_url(&format!("https://x/{}", "a".repeat(MAX_URL))).is_err());
        assert!(validate_url("https://example.invalid/m.gguf").is_ok());
    }

    // ── the store ────────────────────────────────────────────────────────────

    #[test]
    fn every_store_path_is_inside_the_root() {
        let s = Store::new(Path::new("/tmp/store"));
        let d = format!("sha256:{}", "c".repeat(64));
        for p in [
            s.models_dir(),
            s.blobs_dir(),
            s.manifests_dir(),
            s.staging_dir(),
            s.blob(&d).unwrap(),
            s.manifest("qwen3-coder").unwrap(),
            s.staging(&d).unwrap(),
        ] {
            assert!(p.starts_with("/tmp/store"), "{} escaped the root", p.display());
        }
    }

    #[test]
    fn the_default_store_is_under_var_because_usr_is_read_only() {
        assert_eq!(Store::default().root(), Path::new(STORE_ROOT));
        assert!(STORE_ROOT.starts_with("/var/"), "{STORE_ROOT}");
    }

    #[test]
    fn a_store_lookup_validates_the_id_so_it_cannot_be_the_hole() {
        let s = Store::default();
        assert!(s.manifest("../../etc/shadow").is_err());
        assert!(s.manifest("").is_err());
    }

    #[test]
    fn a_staging_path_can_never_collide_with_a_finished_blob() {
        // The property that makes "verify then rename" safe even if someone
        // ever points both directories at one place.
        let s = Store::default();
        let d = format!("sha256:{}", "d".repeat(64));
        let staging = s.staging(&d).unwrap();
        let blob = s.blob(&d).unwrap();
        assert_ne!(staging.file_name(), blob.file_name());
        assert!(staging.to_string_lossy().ends_with(".part"));
    }

    // ── the catalogue ────────────────────────────────────────────────────────

    #[test]
    fn a_catalogue_round_trips_losslessly_and_stably() {
        let c = catalogue();
        let text = c.to_toml().unwrap();
        let back = Catalogue::parse(&text).expect("parses back");
        assert_eq!(c, back, "round trip lost something:\n{text}");
        // Again, so a normalising serialiser cannot pass by converging on the
        // second pass.
        assert_eq!(text, back.to_toml().unwrap(), "serialisation is not stable");
    }

    #[test]
    fn a_catalogue_entry_with_a_bad_url_or_digest_is_refused() {
        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().url = "http://x/m".into();
        assert!(c.validate().is_err());

        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().digest = "sha256:nope".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_catalogue_entry_naming_an_unknown_runtime_is_refused_and_lists_the_real_ones() {
        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().runtime = "tensorrt".into();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("llama.cpp"), "{e}");
    }

    #[test]
    fn zero_layers_or_zero_weights_are_refused_because_plan_fit_divides_by_them() {
        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().layers = 0;
        assert!(c.validate().is_err());
        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().weights_mib = 0;
        assert!(c.validate().is_err());
        let mut c = catalogue();
        c.model.get_mut("qwen3-coder").unwrap().max_context = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn an_unknown_catalogue_key_is_a_build_mistake_and_is_refused() {
        assert!(Catalogue::parse("[model.x]\ntitel = \"typo\"\n").is_err());
    }

    #[test]
    fn a_future_catalogue_version_is_refused_rather_than_guessed_at() {
        let e = Catalogue::parse(&format!("version = {}\n", SCHEMA_VERSION + 1)).unwrap_err();
        assert!(e.to_string().contains("understands"), "{e}");
    }

    // ── the manifest ─────────────────────────────────────────────────────────

    #[test]
    fn a_manifest_keeps_fields_a_newer_rime_wrote() {
        // bootc rollback: version N writes it, N-1 reads it.
        let text = format!(
            r#"{{"version":1,"id":"m","digest":"sha256:{}","tokenizer":"future"}}"#,
            "e".repeat(64)
        );
        let m = Manifest::parse(&text).expect("parses");
        assert_eq!(m.id, "m");
        assert!(m.unknown.contains_key("tokenizer"));
        // And they survive a write.
        let back = Manifest::parse(&m.to_json().unwrap()).unwrap();
        assert_eq!(back.unknown.get("tokenizer").unwrap(), "future");
    }

    #[test]
    fn a_manifest_is_revalidated_on_read_not_trusted() {
        // It names a path and a blob. A root-owned writer having produced it is
        // not a reason to skip checking it.
        let text = r#"{"version":1,"id":"../../etc/shadow","digest":"sha256:x"}"#;
        assert!(Manifest::parse(text).is_err());
        let text = format!(r#"{{"version":1,"id":"m","digest":"sha512:{}"}}"#, "a".repeat(64));
        assert!(Manifest::parse(&text).is_err());
    }

    #[test]
    fn a_manifest_from_the_future_is_refused() {
        let text = format!(
            r#"{{"version":{},"id":"m","digest":"sha256:{}"}}"#,
            SCHEMA_VERSION + 1,
            "a".repeat(64)
        );
        assert!(Manifest::parse(&text).is_err());
    }

    // ── settings and the TCP refusal ─────────────────────────────────────────

    #[test]
    fn settings_round_trip_losslessly() {
        let s = Settings {
            version: Some(SCHEMA_VERSION),
            model: Some("qwen3-coder".into()),
            backend: Some("cuda".into()),
            context: Some(8192),
            idle_timeout: Some(600),
            ..Default::default()
        };
        let text = s.to_toml().unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), s);
    }

    #[test]
    fn every_tcp_shaped_setting_is_refused_and_explains_peer_credentials() {
        // THE assertion this module exists for. Four keys, one reason, and the
        // reason has to name the mechanism — "unsupported" would read as "not
        // implemented yet" and invite someone to implement it.
        let cases = [
            "listen = \"127.0.0.1:11434\"\n",
            "host = \"0.0.0.0\"\n",
            "port = 11434\n",
        ];
        for text in cases {
            let e = Settings::parse(text).unwrap_err().to_string();
            assert!(e.contains("SO_PEERCRED"), "{text} -> {e}");
            assert!(e.contains("no peer credential"), "{text} -> {e}");
            assert!(e.contains("every account"), "{text} -> {e}");
        }
        // api_key gets its own reason: there is no key, not a key you may not set.
        let e = Settings::parse("api_key = \"secret\"\n").unwrap_err().to_string();
        assert!(e.contains("no key to configure"), "{e}");
    }

    #[test]
    fn the_tcp_refusal_points_at_the_transport_that_does_exist() {
        // A refusal that leaves the user with no way to reach a remote machine
        // is a refusal they will work around. §20's ssh transport is the answer.
        let e = refuse_tcp_endpoint("listen").to_string();
        assert!(e.contains("rime host"), "{e}");
        assert!(e.contains("ssh"), "{e}");
    }

    #[test]
    fn an_unknown_setting_is_a_typo_and_is_refused() {
        // One program writer, so deny_unknown_fields. Same rule as games.toml.
        assert!(Settings::parse("modell = \"x\"\n").is_err());
    }

    #[test]
    fn out_of_range_settings_are_refused() {
        assert!(Settings::parse("context = 8\n").is_err());
        assert!(Settings::parse("context = 2000000\n").is_err());
        assert!(Settings::parse("idle_timeout = 999999\n").is_err());
        // 0 is legal and means "never".
        assert!(Settings::parse("idle_timeout = 0\n").is_ok());
        // 0 context means "the model's own maximum".
        assert!(Settings::parse("context = 0\n").is_ok());
    }

    #[test]
    fn an_unknown_backend_in_settings_lists_the_real_ones() {
        let e = Settings::parse("backend = \"tensorrt\"\n").unwrap_err().to_string();
        assert!(e.contains("cuda"), "{e}");
        assert!(e.contains("vulkan"), "{e}");
    }

    // ── accelerator detection ────────────────────────────────────────────────

    #[test]
    fn lavapipe_alone_is_not_vulkan_support() {
        // The one that would otherwise make every machine report GPU offload
        // and then run a CPU renderer. Mesa ships lvp_icd on all of them.
        let e = AccelEvidence {
            render_nodes: 1,
            vulkan_icds: vec!["lvp_icd.x86_64.json".into(), "lvp_icd.i686.json".into()],
            ..Default::default()
        };
        assert!(!e.accel().vulkan, "lavapipe was counted as a GPU");
        assert!(e.hardware_vulkan_icds().is_empty());
    }

    #[test]
    fn a_hardware_icd_with_a_render_node_is_vulkan_support() {
        // The katana's real set, trimmed: lavapipe plus real drivers.
        let e = AccelEvidence {
            render_nodes: 2,
            vulkan_icds: vec![
                "lvp_icd.x86_64.json".into(),
                "nvidia_icd.x86_64.json".into(),
                "intel_icd.x86_64.json".into(),
            ],
            ..Default::default()
        };
        assert!(e.accel().vulkan);
        assert_eq!(
            e.hardware_vulkan_icds(),
            vec!["nvidia_icd.x86_64.json", "intel_icd.x86_64.json"]
        );
    }

    #[test]
    fn a_hardware_icd_with_no_render_node_is_not_vulkan_support() {
        let e = AccelEvidence {
            render_nodes: 0,
            vulkan_icds: vec!["radeon_icd.x86_64.json".into()],
            ..Default::default()
        };
        assert!(!e.accel().vulkan);
    }

    #[test]
    fn cuda_needs_both_the_device_node_and_the_library() {
        // A driver package with no loaded module, and a loaded module with no
        // userspace library, are both unusable — and both are real states.
        assert!(!AccelEvidence { libcuda: true, ..Default::default() }.accel().cuda);
        assert!(!AccelEvidence { nvidia_control_dev: true, ..Default::default() }.accel().cuda);
        assert!(
            AccelEvidence { nvidia_control_dev: true, libcuda: true, ..Default::default() }
                .accel()
                .cuda
        );
    }

    #[test]
    fn rocm_is_kfd_and_nothing_else() {
        // /opt/rocm existing proves a package was installed, not that amdkfd is
        // there. The katana has neither, which is why this is a unit test.
        assert!(AccelEvidence { kfd_dev: true, ..Default::default() }.accel().rocm);
        assert!(!AccelEvidence::default().accel().rocm);
    }

    #[test]
    fn cpu_is_always_available() {
        assert!(Accel::default().has(Backend::Cpu));
        assert_eq!(Accel::default().available(), vec![Backend::Cpu]);
    }

    #[test]
    fn software_icd_matching_is_on_the_driver_stem_not_the_whole_name() {
        // lvp_icd.x86_64.json and lvp_icd.i686.json are the same driver.
        assert!(is_software_icd("lvp_icd.x86_64.json"));
        assert!(is_software_icd("lvp_icd.i686.json"));
        assert!(!is_software_icd("nvidia_icd.x86_64.json"));
        assert!(!is_software_icd("radeon_icd.i686.json"));
    }

    // ── backend selection ────────────────────────────────────────────────────

    #[test]
    fn cuda_wins_when_it_is_available() {
        let accel = Accel { cuda: true, rocm: false, vulkan: true };
        // The katana's real numbers: 8192 MiB total, 52 MiB used.
        let c = select_backend(&accel, &[dev(0, 8192, 52)], None).unwrap();
        assert_eq!(c.backend, Backend::Cuda);
        assert_eq!(c.device, Some(0));
        assert!(c.why.contains("CUDA"), "{}", c.why);
    }

    #[test]
    fn vulkan_is_chosen_only_when_no_vendor_backend_is_and_says_so() {
        let accel = Accel { cuda: false, rocm: false, vulkan: true };
        let c = select_backend(&accel, &[dev(0, 4096, 0)], None).unwrap();
        assert_eq!(c.backend, Backend::Vulkan);
        assert!(c.why.contains("cuda"), "the reason must name what was missing: {}", c.why);
        assert!(c.why.contains("rocm"), "{}", c.why);
    }

    #[test]
    fn cpu_is_chosen_when_nothing_else_is_and_names_every_rejection() {
        let c = select_backend(&Accel::default(), &[], None).unwrap();
        assert_eq!(c.backend, Backend::Cpu);
        assert_eq!(c.device, None);
        // "Why is this on the CPU" must be answerable without reading source.
        assert_eq!(c.rejected.len(), 3, "{:?}", c.rejected);
        for b in [Backend::Cuda, Backend::Rocm, Backend::Vulkan] {
            assert!(c.rejected.iter().any(|(x, _)| *x == b), "{b} missing from {:?}", c.rejected);
        }
    }

    #[test]
    fn a_driver_with_no_room_falls_through_to_the_cpu_with_the_reason() {
        // A card the desktop has filled. The honest answer is the CPU, and the
        // reason has to name the reserve rather than the driver.
        let accel = Accel { cuda: true, rocm: false, vulkan: true };
        let c = select_backend(&accel, &[dev(0, 2048, 2000)], None).unwrap();
        assert_eq!(c.backend, Backend::Cpu);
        let why = c.rejected.iter().find(|(b, _)| *b == Backend::Cuda).unwrap().1.clone();
        assert!(why.contains("free beyond"), "{why}");
    }

    #[test]
    fn an_unavailable_pin_is_refused_never_downgraded() {
        // The rule AGENTS.md states directly: never weaken a policy silently.
        // Someone who pinned cuda and got CPU would blame the machine.
        let e = select_backend(&Accel::default(), &[], Some(Backend::Cuda)).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("/dev/nvidiactl"), "{msg}");
        assert!(select_backend(&Accel::default(), &[], Some(Backend::Rocm)).is_err());
        assert!(select_backend(&Accel::default(), &[], Some(Backend::Vulkan)).is_err());
        // CPU is always honourable.
        assert!(select_backend(&Accel::default(), &[], Some(Backend::Cpu)).is_ok());
    }

    #[test]
    fn a_pin_that_is_available_is_taken_and_said_to_be_a_pin() {
        let accel = Accel { cuda: true, rocm: false, vulkan: true };
        let c = select_backend(&accel, &[dev(0, 8192, 52)], Some(Backend::Vulkan)).unwrap();
        assert_eq!(c.backend, Backend::Vulkan);
        assert!(c.why.contains("pinned"), "{}", c.why);
    }

    #[test]
    fn the_device_with_the_most_room_wins_and_ties_go_to_the_lower_index() {
        let accel = Accel { cuda: true, rocm: false, vulkan: false };
        let c = select_backend(&accel, &[dev(0, 4096, 0), dev(1, 8192, 0)], None).unwrap();
        assert_eq!(c.device, Some(1));
        // A tie must be deterministic, or two identical machines plan
        // differently.
        let c = select_backend(&accel, &[dev(0, 8192, 0), dev(1, 8192, 0)], None).unwrap();
        assert_eq!(c.device, Some(0));
    }

    #[test]
    fn selection_is_pure() {
        let accel = Accel { cuda: true, rocm: true, vulkan: true };
        let d = [dev(0, 8192, 52)];
        assert_eq!(
            select_backend(&accel, &d, None).unwrap(),
            select_backend(&accel, &d, None).unwrap()
        );
    }

    #[test]
    fn every_accel_combination_selects_something_without_panicking() {
        // 8 combinations x 3 device shapes x 5 pins. Exhaustive rather than
        // representative: this is the function every other decision hangs off.
        let mut chosen = 0;
        for cuda in [false, true] {
            for rocm in [false, true] {
                for vulkan in [false, true] {
                    let accel = Accel { cuda, rocm, vulkan };
                    for devices in [vec![], vec![dev(0, 8192, 52)], vec![dev(0, 512, 500)]] {
                        for want in [
                            None,
                            Some(Backend::Cuda),
                            Some(Backend::Rocm),
                            Some(Backend::Vulkan),
                            Some(Backend::Cpu),
                        ] {
                            // Either a choice or a named refusal — never a panic
                            // and never a silent downgrade.
                            match select_backend(&accel, &devices, want) {
                                Ok(c) => {
                                    chosen += 1;
                                    if let Some(w) = want {
                                        assert_eq!(c.backend, w, "a pin was not honoured");
                                    }
                                }
                                Err(e) => {
                                    let w = want.expect("only a pin can be refused");
                                    assert!(!accel.has(w), "{w} was refused while available: {e}");
                                }
                            }
                        }
                    }
                }
            }
        }
        // 8 accel x 3 device sets x 5 pins = 120 calls. The pinned ones that
        // must fail are exactly the unavailable pins: for each of the 8 accel
        // combinations, count the GPU backends it lacks, times 3 device sets.
        let refused: usize = (0..8)
            .map(|bits| {
                [bits & 1 == 0, bits & 2 == 0, bits & 4 == 0]
                    .iter()
                    .filter(|missing| **missing)
                    .count()
            })
            .sum::<usize>()
            * 3;
        assert_eq!(chosen, 120 - refused, "expected exactly {} choices", 120 - refused);
        assert_eq!(refused, 36, "the arithmetic itself must be right");
    }

    // ── the VRAM fit ─────────────────────────────────────────────────────────

    #[test]
    fn a_model_that_fits_is_fully_offloaded_at_the_context_asked_for() {
        // 4 GiB weights, 32 KV per 1k, 8k context on the katana's 8 GiB card
        // with 52 MiB used: 256 + 4096 + 256 = 4608 of 7628 spendable.
        let f = plan_fit(4096, 32, 32, 8192, dev(0, 8192, 52).budget_mib());
        assert_eq!(f.placement, Placement::Gpu { layers: 32 });
        assert_eq!(f.context, 8192);
        assert_eq!(f.vram_mib, 256 + 4096 + 32 * 8);
        assert!(f.is_full_offload());
    }

    #[test]
    fn the_context_is_reduced_before_any_layer_is_given_up() {
        // The ordering rule, and it is not cosmetic: a smaller context wholly
        // on the GPU is far faster than a full context split with system RAM.
        // 4096 weights + 256 overhead = 4352; budget 5000 leaves 648 for KV.
        // 32 MiB/1k means 32k ctx costs 1024 (too much), 16k costs 512 (fits).
        let f = plan_fit(4096, 32, 32, 32768, 5000);
        assert_eq!(f.placement, Placement::Gpu { layers: 32 }, "{:?}", f.notes);
        assert_eq!(f.context, 16384);
        assert!(
            f.notes.iter().any(|n| n.contains("context reduced")),
            "the reason must travel with the plan: {:?}",
            f.notes
        );
    }

    #[test]
    fn a_model_too_big_for_the_card_is_split_and_leaves_a_layer_of_margin() {
        // 8192 MiB of weights over 32 layers = 256 MiB each, on a 4 GiB card.
        // fixed = 256 overhead + 64 KV(2k) = 320; 4096-320 = 3776 -> 14 layers,
        // minus the margin = 13.
        let f = plan_fit(8192, 32, 32, 32768, 4096);
        assert_eq!(f.placement, Placement::Split { layers: 13, total: 32 }, "{:?}", f.notes);
        assert_eq!(f.context, MIN_USEFUL_CONTEXT);
        assert!(!f.is_full_offload());
        assert!(
            f.notes.iter().any(|n| n.contains("margin")),
            "the margin must be explained: {:?}",
            f.notes
        );
    }

    #[test]
    fn a_card_with_no_room_offloads_nothing_and_keeps_the_full_context() {
        // Nothing is on the GPU, so nothing constrains the context — reducing
        // it would be a cost with no benefit.
        let f = plan_fit(8192, 32, 32, 32768, 300);
        assert_eq!(f.placement, Placement::Cpu);
        assert_eq!(f.context, 32768);
        assert_eq!(f.vram_mib, 0);
        assert_eq!(f.placement.gpu_layers(), 0);
    }

    #[test]
    fn a_zero_budget_is_the_cpu_rather_than_a_division_by_zero() {
        let f = plan_fit(4096, 32, 32, 4096, 0);
        assert_eq!(f.placement, Placement::Cpu);
    }

    #[test]
    fn the_fit_never_plans_more_layers_than_the_model_has() {
        // The bound that would otherwise produce -ngl 40 for a 32-layer model.
        for budget in [0, 300, 1000, 5000, 100_000, u64::MAX / 2] {
            let f = plan_fit(4096, 32, 32, 8192, budget);
            assert!(
                f.placement.gpu_layers() <= 32,
                "budget {budget} planned {} of 32 layers",
                f.placement.gpu_layers()
            );
        }
    }

    #[test]
    fn the_fit_never_plans_more_vram_than_the_budget() {
        // The invariant the whole function exists for. Swept rather than
        // sampled, because an off-by-one here is an allocation failure at
        // startup on somebody's machine.
        for weights in [512u64, 4096, 8192, 40_000] {
            // 0 is in the sweep because it is a real manifest value —
            // `rime ai pull --url` writes it — and the budget invariant has to
            // hold on the unknown-layers path too.
            for layers in [0u32, 1, 8, 32, 80] {
                for kv in [0u64, 8, 32, 512] {
                    for ctx in [2048u32, 8192, 32768, 131_072] {
                        for budget in [0u64, 300, 1024, 4096, 8192, 24_576] {
                            let f = plan_fit(weights, layers, kv, ctx, budget);
                            assert!(
                                f.vram_mib <= budget,
                                "planned {} MiB of a {budget} MiB budget \
                                 (weights {weights}, layers {layers}, kv {kv}, ctx {ctx}): {:?}",
                                f.vram_mib,
                                f.notes
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_fit_is_pure() {
        assert_eq!(plan_fit(4096, 32, 32, 8192, 6000), plan_fit(4096, 32, 32, 8192, 6000));
    }

    #[test]
    fn a_zero_layer_model_does_not_divide_by_zero() {
        // Validation refuses this in a catalogue, but `rime ai pull --url`
        // writes `layers: 0` for everything outside it, and the daemon must not
        // panic on one.
        //
        // This used to assert `gpu_layers() <= 1`, which pinned the defect
        // rather than the requirement: the requirement is that it does not
        // divide by zero, and answering "1" satisfied that while being wrong.
        let f = plan_fit(4096, 0, 32, 8192, 6000);
        assert_ne!(f.placement, Placement::Gpu { layers: 1 }, "{:?}", f.notes);
    }

    #[test]
    fn a_model_with_no_declared_layer_count_is_offloaded_whole_or_not_at_all() {
        // `layers: 0` means UNKNOWN. Clamping it to 1 made a model that fits
        // entirely in VRAM launch with `--n-gpu-layers 1` — one layer on the
        // GPU, the rest on the CPU — under a note reading "all 1 layers fit".
        let f = plan_fit(4096, 0, 32, 8192, 6000);
        assert_eq!(
            f.placement,
            Placement::Gpu { layers: ALL_LAYERS_UNKNOWN },
            "{:?}",
            f.notes
        );
        assert!(
            f.placement.gpu_layers() > 1,
            "one layer is not a full offload: {:?}",
            f.placement
        );
        // And the note must not claim a count it does not have.
        assert!(
            f.notes.iter().any(|n| n.contains("declares no layer count")),
            "{:?}",
            f.notes
        );
        assert!(
            !f.notes.iter().any(|n| n.contains("all 1 layers")
                || n.contains(&format!("all {ALL_LAYERS_UNKNOWN} layers"))),
            "{:?}",
            f.notes
        );
    }

    #[test]
    fn a_model_with_no_declared_layer_count_is_never_split() {
        // A split needs a per-layer cost and `weights_mib / layers` is not one
        // when the denominator is a guess. The old code computed
        // `weights_mib / 1` and offered to offload a "layer" that was the
        // entire model. The CPU is the honest answer, and it is also what
        // `rime ai pull` already promises for such a manifest.
        let f = plan_fit(40_000, 0, 32, 8192, 6000);
        assert_eq!(f.placement, Placement::Cpu, "{:?}", f.notes);
        assert_eq!(f.placement.gpu_layers(), 0);
        assert_eq!(f.vram_mib, 0);
        assert!(
            f.notes
                .iter()
                .any(|n| n.contains("a partial offload cannot be planned")),
            "the refusal must say why: {:?}",
            f.notes
        );
    }

    #[test]
    fn a_declared_layer_count_still_splits_exactly_as_before() {
        // The negative control for the two above: the unknown-layers branch
        // must not have changed what a catalogued model does.
        let f = plan_fit(8192, 32, 32, 32768, 4096);
        match f.placement {
            Placement::Split { layers, total } => {
                assert_eq!(total, 32);
                assert!(layers > 0 && layers < total, "{layers} of {total}");
            }
            other => panic!("a catalogued model must still split: {other:?}"),
        }
    }

    #[test]
    fn the_display_reserve_is_actually_subtracted() {
        // A card with exactly the reserve free has nothing to spend.
        assert_eq!(dev(0, 8192, 8192 - VRAM_RESERVE_MIB).budget_mib(), 0);
        // And an over-full card saturates rather than wrapping.
        assert_eq!(dev(0, 1024, 4096).budget_mib(), 0);
        assert_eq!(dev(0, 8192, 52).budget_mib(), 8192 - 52 - VRAM_RESERVE_MIB);
    }

    // ── idle unloading ───────────────────────────────────────────────────────

    fn idle(loaded: bool, conns: u32, secs: u64, cfg: Option<u64>, batt: bool) -> IdleInputs {
        IdleInputs {
            loaded,
            open_connections: conns,
            idle_secs: secs,
            configured_timeout: cfg,
            on_battery: batt,
        }
    }

    #[test]
    fn an_attached_client_is_never_unloaded_under() {
        // A long prompt evaluation moves no bytes for seconds. Unloading under
        // it would truncate an answer to save power.
        let d = plan_idle(&idle(true, 1, 100_000, Some(1), false));
        assert!(!d.unloads(), "{}", d.reason());
        assert!(d.reason().contains("attached"), "{}", d.reason());
    }

    #[test]
    fn an_idle_backend_past_the_timeout_is_unloaded() {
        let d = plan_idle(&idle(true, 0, IDLE_TIMEOUT_AC_SECS, None, false));
        assert!(d.unloads(), "{}", d.reason());
        assert!(d.reason().contains("reloads it"), "{}", d.reason());
    }

    #[test]
    fn the_battery_timeout_is_shorter_and_the_reason_says_so() {
        // Between the two defaults: kept on AC, unloaded on battery.
        let secs = IDLE_TIMEOUT_BATTERY_SECS + 1;
        assert!(secs < IDLE_TIMEOUT_AC_SECS);
        assert!(!plan_idle(&idle(true, 0, secs, None, false)).unloads());
        let d = plan_idle(&idle(true, 0, secs, None, true));
        assert!(d.unloads(), "{}", d.reason());
        assert!(d.reason().contains("on battery"), "{}", d.reason());
    }

    #[test]
    fn a_configured_timeout_wins_on_both_power_sources() {
        // Quietly halving a number the user wrote would be the silent policy
        // downgrade AGENTS.md forbids.
        assert_eq!(idle_timeout(Some(900), true), 900);
        assert_eq!(idle_timeout(Some(900), false), 900);
        assert_eq!(idle_timeout(None, true), IDLE_TIMEOUT_BATTERY_SECS);
        assert_eq!(idle_timeout(None, false), IDLE_TIMEOUT_AC_SECS);
    }

    #[test]
    fn a_zero_timeout_means_never_and_says_that() {
        let d = plan_idle(&idle(true, 0, 86_400, Some(0), true));
        assert!(!d.unloads(), "{}", d.reason());
        assert!(d.reason().contains("until you unload it"), "{}", d.reason());
    }

    #[test]
    fn nothing_loaded_is_never_an_unload() {
        assert!(!plan_idle(&idle(false, 0, 999_999, Some(1), true)).unloads());
    }

    #[test]
    fn the_idle_decision_is_pure_and_total() {
        for loaded in [false, true] {
            for conns in [0u32, 1, 7] {
                for secs in [0u64, 59, 60, 299, 300, 86_400] {
                    for cfg in [None, Some(0), Some(10), Some(86_400)] {
                        for batt in [false, true] {
                            let i = idle(loaded, conns, secs, cfg, batt);
                            let a = plan_idle(&i);
                            assert_eq!(a, plan_idle(&i));
                            assert!(!a.reason().is_empty(), "a decision with no reason");
                        }
                    }
                }
            }
        }
    }

    // ── runtimes ─────────────────────────────────────────────────────────────

    #[test]
    fn every_runtime_either_launches_or_explains_why_not() {
        // The exhaustiveness assertion. A variant that produced neither a plan
        // nor a reason would be a runtime the store could name and the daemon
        // could never start — the silent-drop failure this repository has
        // already been bitten by.
        let mut launchable = 0;
        let mut refused = 0;
        for r in Runtime::ALL {
            match r.unsupported_because() {
                None => {
                    launchable += 1;
                    assert!(r.supports_unix_socket(), "{r} must be reachable without a port");
                    assert!(!r.program().is_empty());
                    assert!(r.install_hint(Backend::Cpu).contains("rime "), "{r}");
                }
                Some(why) => {
                    refused += 1;
                    assert!(why.len() > 40, "{r}'s refusal explains nothing: {why}");
                }
            }
        }
        assert_eq!(launchable, 1, "exactly llama.cpp is implemented");
        assert_eq!(refused, 2, "ollama and vllm are recognised and refused");
        assert_eq!(launchable + refused, Runtime::ALL.len());
    }

    #[test]
    fn every_runtime_id_parses_back_to_itself() {
        for r in Runtime::ALL {
            assert_eq!(r.as_str().parse::<Runtime>().unwrap(), r);
        }
        assert_eq!(Runtime::all_ids().len(), Runtime::ALL.len());
        assert!("tensorrt".parse::<Runtime>().is_err());
    }

    #[test]
    fn every_backend_id_parses_back_to_itself() {
        for b in Backend::ALL {
            assert_eq!(b.as_str().parse::<Backend>().unwrap(), b);
        }
        assert_eq!(Backend::all_ids().len(), Backend::ALL.len());
    }

    #[test]
    fn the_install_hint_uses_only_mechanisms_that_exist() {
        // AGENTS.md forbids a second package mechanism. The hints must name
        // P1's two and nothing else.
        assert_eq!(
            Runtime::LlamaCpp.install_hint(Backend::Cpu),
            "sudo rime install llama-cpp"
        );
        assert!(Runtime::LlamaCpp.install_hint(Backend::Cuda).contains("rime env create cuda"));
        assert!(Runtime::LlamaCpp.install_hint(Backend::Rocm).contains("rime env create rocm"));
        for b in Backend::ALL {
            let h = Runtime::LlamaCpp.install_hint(b);
            assert!(
                h.starts_with("sudo rime install") || h.starts_with("rime env create"),
                "{b}: {h}"
            );
        }
    }

    // ── the launch plan ──────────────────────────────────────────────────────

    fn fit_full() -> Fit {
        plan_fit(4096, 32, 32, 8192, 7628)
    }

    fn choice_cuda() -> BackendChoice {
        select_backend(
            &Accel { cuda: true, rocm: false, vulkan: true },
            &[dev(0, 8192, 52)],
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_llama_cpp_launch_binds_a_unix_socket_and_carries_the_fit() {
        let fit = fit_full();
        let choice = choice_cuda();
        let sock = PathBuf::from("/run/user/1000/rime-ai/backend.sock");
        let plan = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "/usr/bin/llama-server",
            model_path: Path::new("/var/lib/rime/ai/models/blobs/sha256-aa"),
            listen: Listen::Unix(sock.clone()),
            fit: &fit,
            choice: &choice,
            threads: 8,
        })
        .expect("plans");

        assert_eq!(plan.program, "/usr/bin/llama-server");
        // Exact pairs, not "contains": a flag that moved to the wrong value is
        // invisible to a substring check.
        for pair in [
            ["--model", "/var/lib/rime/ai/models/blobs/sha256-aa"],
            ["--host", "/run/user/1000/rime-ai/backend.sock"],
            ["--n-gpu-layers", "32"],
            ["--ctx-size", "8192"],
            ["--threads", "8"],
            ["--main-gpu", "0"],
        ] {
            assert!(
                plan.argv.windows(2).any(|w| w == pair),
                "{pair:?} missing from {:?}",
                plan.argv
            );
        }
        assert_eq!(plan.listen, Listen::Unix(sock));
    }

    #[test]
    fn a_launch_never_names_a_port_or_a_loopback_address() {
        // The absence IS the assertion, and it is checked over the whole argv
        // and environment rather than one flag.
        let fit = fit_full();
        let choice = choice_cuda();
        let plan = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Unix(PathBuf::from("/run/x/backend.sock")),
            fit: &fit,
            choice: &choice,
            threads: 0,
        })
        .unwrap();
        for a in &plan.argv {
            assert!(!a.contains("127.0.0.1"), "argv names loopback: {a}");
            assert!(!a.contains("0.0.0.0"), "argv names a wildcard address: {a}");
        }
        assert!(!plan.argv.iter().any(|a| a == "--port"), "{:?}", plan.argv);
        for (k, v) in &plan.env {
            assert!(!v.contains("127.0.0.1"), "{k} names loopback");
            assert!(k != "LLAMA_ARG_PORT", "the plan sets a port");
        }
    }

    #[test]
    fn a_tcp_listen_address_is_refused_by_the_planner_too() {
        // Not only by the settings parser: the daemon builds a LaunchRequest
        // directly, so the refusal has to live where the argv is made.
        let fit = fit_full();
        let choice = choice_cuda();
        let e = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Loopback { port: 11434 },
            fit: &fit,
            choice: &choice,
            threads: 0,
        })
        .unwrap_err();
        assert!(e.to_string().contains("SO_PEERCRED"), "{e}");
    }

    #[test]
    fn a_socket_path_that_llama_server_would_parse_as_an_address_is_refused() {
        // Measured behaviour: --host binds AF_UNIX only when the value ends
        // ".sock". A path without it becomes a hostname lookup and then a
        // loopback port, which is the exact outcome this module refuses. So the
        // suffix is enforced rather than appended.
        let fit = fit_full();
        let choice = choice_cuda();
        let e = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Unix(PathBuf::from("/run/x/backend")),
            fit: &fit,
            choice: &choice,
            threads: 0,
        })
        .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains(".sock"), "{msg}");
        assert!(msg.contains("loopback port"), "{msg}");
    }

    #[test]
    fn the_default_backend_socket_satisfies_the_suffix_rule() {
        // The constant and the rule must not drift apart: if BACKEND_SOCKET
        // were ever renamed without the suffix, every launch would refuse.
        let e = Endpoints::new(Path::new("/run/user/1000"));
        assert!(e
            .backend()
            .to_string_lossy()
            .ends_with(Runtime::LlamaCpp.listen_suffix_required().unwrap()));
        assert!(e.control().starts_with(e.dir()));
        assert!(e.api().starts_with(e.dir()));
        assert!(e.dir().ends_with(RUNTIME_SUBDIR));
    }

    #[test]
    fn a_cpu_launch_offloads_nothing_and_selects_no_device() {
        let fit = plan_fit(8192, 32, 32, 8192, 0);
        let choice = select_backend(&Accel::default(), &[], None).unwrap();
        let plan = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Unix(PathBuf::from("/run/x/b.sock")),
            fit: &fit,
            choice: &choice,
            threads: 4,
        })
        .unwrap();
        assert!(plan.argv.windows(2).any(|w| w == ["--n-gpu-layers", "0"]), "{:?}", plan.argv);
        assert!(!plan.argv.iter().any(|a| a == "--main-gpu"), "{:?}", plan.argv);
    }

    #[test]
    fn an_unimplemented_runtime_refuses_with_its_reason() {
        let fit = fit_full();
        let choice = choice_cuda();
        for r in [Runtime::Ollama, Runtime::Vllm] {
            let e = plan_launch(&LaunchRequest {
                runtime: r,
                program: r.program(),
                model_path: Path::new("/m.gguf"),
                listen: Listen::Unix(PathBuf::from("/run/x/b.sock")),
                fit: &fit,
                choice: &choice,
                threads: 0,
            })
            .unwrap_err();
            assert_eq!(
                e,
                AiError::Refused { key: "runtime", because: r.unsupported_because().unwrap() },
                "{r}'s refusal must be the documented one"
            );
        }
    }

    #[test]
    fn the_launch_plan_is_pure() {
        let fit = fit_full();
        let choice = choice_cuda();
        let req = LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Unix(PathBuf::from("/run/x/b.sock")),
            fit: &fit,
            choice: &choice,
            threads: 2,
        };
        assert_eq!(plan_launch(&req).unwrap(), plan_launch(&req).unwrap());
    }

    #[test]
    fn the_launch_plan_carries_the_fit_notes_so_a_log_explains_itself() {
        let fit = plan_fit(8192, 32, 32, 32768, 4096);
        let choice = choice_cuda();
        let plan = plan_launch(&LaunchRequest {
            runtime: Runtime::LlamaCpp,
            program: "llama-server",
            model_path: Path::new("/m.gguf"),
            listen: Listen::Unix(PathBuf::from("/run/x/b.sock")),
            fit: &fit,
            choice: &choice,
            threads: 0,
        })
        .unwrap();
        for n in &fit.notes {
            assert!(plan.notes.contains(n), "the plan dropped {n:?}");
        }
    }

    // ── the pull plan ────────────────────────────────────────────────────────

    #[test]
    fn a_bare_url_is_refused_and_the_message_says_what_a_digest_proves() {
        // THE provenance assertion. Content-addressing something the network
        // handed you proves only that it sent the same bytes twice.
        for spec in [
            "https://example.invalid/m.gguf",
            "http://example.invalid/m.gguf",
        ] {
            let e = parse_pull_spec(spec, None, None).unwrap_err();
            assert!(matches!(e, PullError::NoDigest { .. }), "{spec} -> {e:?}");
            let msg = e.to_string();
            assert!(msg.contains("same bytes twice"), "{msg}");
            assert!(msg.contains("--digest"), "{msg}");
        }
        // And in the --url position.
        let e = parse_pull_spec("m", Some("https://example.invalid/m.gguf"), None).unwrap_err();
        assert!(matches!(e, PullError::NoDigest { .. }), "{e:?}");
    }

    #[test]
    fn a_catalogue_name_resolves_to_the_image_shipped_digest() {
        let spec = parse_pull_spec("qwen3-coder", None, None).unwrap();
        let plan = plan_pull(&spec, &catalogue(), &Store::default()).unwrap();
        assert_eq!(plan.digest, cat_entry().digest);
        assert_eq!(plan.url, cat_entry().url);
        assert!(!plan.user_supplied_digest);
        assert!(
            plan.notes.iter().any(|n| n.contains("image content")),
            "the trust anchor must be stated: {:?}",
            plan.notes
        );
    }

    #[test]
    fn a_pinned_digest_that_disagrees_with_the_image_is_refused_rather_than_chosen_between() {
        let spec = parse_pull_spec(&format!("qwen3-coder@sha256:{}", "f".repeat(64)), None, None)
            .unwrap();
        let e = plan_pull(&spec, &catalogue(), &Store::default()).unwrap_err();
        assert!(matches!(e, PullError::DigestMismatch { .. }), "{e:?}");
        assert!(e.to_string().contains("Refusing rather than picking one"), "{e}");
    }

    #[test]
    fn a_pinned_digest_that_agrees_is_accepted_and_noted() {
        let spec =
            parse_pull_spec(&format!("qwen3-coder@{}", cat_entry().digest), None, None).unwrap();
        let plan = plan_pull(&spec, &catalogue(), &Store::default()).unwrap();
        assert!(plan.notes.iter().any(|n| n.contains("pinned to exactly")), "{:?}", plan.notes);
    }

    #[test]
    fn an_unknown_name_lists_what_the_catalogue_has() {
        let spec = parse_pull_spec("no-such-model", None, None).unwrap();
        let e = plan_pull(&spec, &catalogue(), &Store::default()).unwrap_err();
        assert!(e.to_string().contains("qwen3-coder"), "{e}");
        // And an empty catalogue says why there is nothing to list.
        let e = plan_pull(&spec, &Catalogue::default(), &Store::default()).unwrap_err();
        assert!(e.to_string().contains("catalogue.toml"), "{e}");
    }

    #[test]
    fn an_explicit_url_and_digest_is_accepted_and_recorded_as_user_supplied() {
        let spec = parse_pull_spec(
            "my-model",
            Some("https://example.invalid/m.gguf"),
            Some(&format!("sha256:{}", "1".repeat(64))),
        )
        .unwrap();
        let plan = plan_pull(&spec, &Catalogue::default(), &Store::default()).unwrap();
        assert!(plan.user_supplied_digest);
        assert!(
            plan.notes.iter().any(|n| n.contains("not from the image")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn every_pull_plan_stages_verifies_and_renames() {
        // The property that keeps a corrupt download from looking like a cached
        // model forever.
        let spec = parse_pull_spec("qwen3-coder", None, None).unwrap();
        let plan = plan_pull(&spec, &catalogue(), &Store::default()).unwrap();
        assert!(plan.staging.starts_with(Store::default().staging_dir()));
        assert!(plan.blob.starts_with(Store::default().blobs_dir()));
        assert!(plan.manifest.starts_with(Store::default().manifests_dir()));
        assert_ne!(plan.staging, plan.blob);
        assert!(plan.notes.iter().any(|n| n.contains("atomic rename")), "{:?}", plan.notes);
    }

    #[test]
    fn a_hostile_pull_name_never_reaches_a_path() {
        for bad in ["../../etc/shadow", "a/b", "-rf", "", "Qwen3"] {
            assert!(parse_pull_spec(bad, None, None).is_err(), "{bad:?} was accepted");
        }
    }

    // ── the control protocol ─────────────────────────────────────────────────

    #[test]
    fn no_message_can_contain_a_raw_newline() {
        // The framing is line-based, so one raw newline in a message desynchronises
        // the stream — and every string in these types can come from a file or a
        // command line. Asserted over hostile content rather than assumed.
        let hostile = "line one\nline two\r\nthree";
        let messages = vec![
            serde_json::to_string(&Request::Select { model: hostile.to_string() }).unwrap(),
            serde_json::to_string(&Response::error(ErrorKind::BadRequest, hostile)).unwrap(),
            serde_json::to_string(&Response::Models {
                models: vec![ModelInfo { id: hostile.to_string(), ..Default::default() }],
            })
            .unwrap(),
            serde_json::to_string(&Response::Status(Box::new(Status {
                notes: vec![hostile.to_string()],
                api_socket: hostile.to_string(),
                ..Default::default()
            })))
            .unwrap(),
        ];
        assert_eq!(messages.len(), 4);
        for m in &messages {
            assert!(!m.contains('\n'), "raw newline in {m}");
            assert!(!m.contains('\r'), "raw carriage return in {m}");
        }
    }

    #[test]
    fn every_request_and_response_round_trips_through_json() {
        let requests = vec![
            Request::Hello,
            Request::Status,
            Request::Models,
            Request::Select { model: "qwen3-coder".into() },
            Request::Unload,
        ];
        // Exactly one case per variant. A variant added without a case here is
        // a variant with no round-trip proof, which is how a rename ships.
        assert_eq!(requests.len(), 5, "one case per Request variant");
        for r in &requests {
            let text = serde_json::to_string(r).unwrap();
            assert_eq!(&serde_json::from_str::<Request>(&text).unwrap(), r, "{text}");
        }

        let responses = vec![
            Response::Hello { version: PROTOCOL_VERSION, api_socket: "/run/x/api.sock".into() },
            Response::Status(Box::default()),
            Response::Models { models: vec![ModelInfo::default()] },
            Response::Ok,
            Response::error(ErrorKind::NoRuntime, "install it"),
        ];
        assert_eq!(responses.len(), 5, "one case per Response variant");
        for r in &responses {
            let text = serde_json::to_string(r).unwrap();
            assert_eq!(&serde_json::from_str::<Response>(&text).unwrap(), r, "{text}");
        }
    }

    #[test]
    fn every_error_kind_round_trips() {
        let kinds = [
            ErrorKind::BadRequest,
            ErrorKind::NoSuchModel,
            ErrorKind::NoRuntime,
            ErrorKind::BackendFailed,
            ErrorKind::Protocol,
            ErrorKind::Internal,
        ];
        assert_eq!(kinds.len(), 6);
        for k in kinds {
            let text = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<ErrorKind>(&text).unwrap(), k);
            // snake_case on the wire, so the CLI and shell can match on it.
            assert!(text.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '"'), "{text}");
        }
    }

    #[test]
    fn a_device_becomes_its_report_including_the_reserve() {
        let d = dev(0, 8192, 52);
        let info = DeviceInfo::from(&d);
        assert_eq!(info.budget_mib, d.budget_mib());
        assert_eq!(info.budget_mib, 8192 - 52 - VRAM_RESERVE_MIB);
    }

    #[test]
    fn contradictory_digests_on_one_command_line_are_refused() {
        let a = format!("sha256:{}", "1".repeat(64));
        let b = format!("sha256:{}", "2".repeat(64));
        let e = parse_pull_spec(&format!("m@{a}"), None, Some(&b)).unwrap_err();
        assert!(matches!(e, PullError::DigestMismatch { .. }), "{e:?}");
    }
}
