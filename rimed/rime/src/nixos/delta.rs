//! Delta downloads (docs/nixos/delta.md §7 and §10, the G2/G3 transport): a path the machine is missing is rebuilt
//! from a `zstd --patch-from` delta against a path it already has, instead of downloading the path whole.
//!
//! The release manifest lists the deltas (`delta.files`): which path, which base, the delta file's name, sha256 and
//! size, so the cosign signature over the manifest covers every byte the client downloads. Per path:
//!
//! 1. the path's signed narinfo comes from the cache its `origin` names (the same document `nix copy` would read);
//!    its NarHash and NarSize must be the manifest's;
//! 2. the delta file is downloaded from `delta_url` (the client's own configuration, never a URL from the network)
//!    and refused unless its size and sha256 are the manifest's;
//! 3. the base path's NAR is dumped from the local store and refused unless it is the base the manifest names
//!    (`base_nar_hash`, `base_nar_size`): a local copy that differs is a wrong base;
//! 4. `zstd -d --patch-from` rebuilds the NAR and this process streams it, hashing as it goes, through a FIFO into
//!    `nix copy --from file://<one-path cache>` whose narinfo is the signed one with only `URL`/`Compression`
//!    rewritten (fields the signature does not cover). Nix checks the signature and the NarHash again and adds the
//!    path atomically: a NAR that does not hash right is never valid.
//!
//! Any refusal (no base, a window over the cap, a download or hash mismatch, a decoder or import failure) falls back
//! to the whole path from its cache, as without deltas. Paths run in parallel, references first.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rimed_core::nixrel::hash;
use rimed_core::nixrel::manifest::{is_store_path, Manifest};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::Config;

/// zstd's default window limit (2^27, 128 MiB) is the floor; `--patch-from` raises the window to cover the base.
const MIN_WINDOW_LOG: u32 = 27;
/// zstd's limit on 64-bit.
const MAX_WINDOW_LOG: u32 = 31;
/// The decoder window cap when the configuration names none (delta.md §7 item 4): 512 MiB, raised to 1 GiB on a
/// machine with 8 GiB of memory or more (linux-firmware's base needs 1 GiB; its delta saves ~780 MB).
const DEFAULT_WINDOW_LOG: u32 = 29;
const LARGE_WINDOW_LOG: u32 = 30;
const LARGE_MEMORY: u64 = 8 << 30;
/// How long `nix copy` may go without reading the rebuilt NAR before the delta is given up.
const OPEN_TIMEOUT: Duration = Duration::from_secs(600);
/// A narinfo is a few hundred bytes; anything this large is not one.
const NARINFO_MAX: u64 = 1 << 20;

/// One `delta.files` entry, resolved: `path` is the store path (the manifest names it by index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub base: String,
    pub base_nar_hash: String,
    pub base_nar_size: u64,
    /// `<release tag>/<nix32>.delta`, under the configured `delta_url`.
    pub file: String,
    pub file_hash: String,
    pub file_size: u64,
    pub window_log: u32,
}

#[derive(Deserialize)]
struct RawTable {
    #[serde(default)]
    files: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    path: usize,
    base: String,
    base_nar_hash: String,
    base_nar_size: u64,
    file: String,
    file_hash: String,
    file_size: u64,
    window_log: u32,
}

fn valid_file_name(f: &str) -> bool {
    let Some((tag, name)) = f.split_once('/') else { return false };
    let tag_ok = !tag.is_empty()
        && tag.len() <= 100
        && !tag.starts_with('.')
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    let name_ok = name.strip_suffix(".delta").is_some_and(|h| {
        h.len() == 52 && h.bytes().all(|b| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&b))
    });
    tag_ok && name_ok
}

/// The manifest's delta table (`delta.files`), by store path. A manifest without one has no deltas. A table that does
/// not validate is an `Err`: the caller then downloads every path whole (deltas are an optimisation, never a reason
/// to refuse a release the manifest validation accepted).
pub fn entries(m: &Manifest) -> Result<BTreeMap<String, Entry>, String> {
    let Some(v) = &m.delta else { return Ok(BTreeMap::new()) };
    if v.is_null() {
        return Ok(BTreeMap::new());
    }
    let t: RawTable = serde_json::from_value(v.clone()).map_err(|e| format!("delta table: {e}"))?;
    let mut out = BTreeMap::new();
    for (i, r) in t.files.into_iter().enumerate() {
        let what = |s: &str| format!("delta.files[{i}]: {s}");
        let Some(p) = m.paths.get(r.path) else { return Err(what("path index out of range")) };
        if !is_store_path(&r.base) {
            return Err(what("base is not a store path"));
        }
        if r.base == p.path {
            return Err(what("base is the path itself"));
        }
        if hash::nar_hash_bytes(&r.base_nar_hash).is_none() || hash::nar_hash_bytes(&r.file_hash).is_none() {
            return Err(what("a hash does not parse"));
        }
        if r.base_nar_size == 0 || r.file_size == 0 {
            return Err(what("a size is zero"));
        }
        if !valid_file_name(&r.file) {
            return Err(what("file is not <tag>/<nix32>.delta"));
        }
        if !(10..=MAX_WINDOW_LOG).contains(&r.window_log) {
            return Err(what("window_log is out of range"));
        }
        let e = Entry {
            path: p.path.clone(),
            base: r.base,
            base_nar_hash: r.base_nar_hash,
            base_nar_size: r.base_nar_size,
            file: r.file,
            file_hash: r.file_hash,
            file_size: r.file_size,
            window_log: r.window_log,
        };
        if out.insert(e.path.clone(), e).is_some() {
            return Err(what("a second delta for the same path"));
        }
    }
    Ok(out)
}

/// The decoder window a delta needs: zstd's rule, the highest bit of the larger NAR (base or new) plus one, never below
/// the default 2^27. Computed from the sizes, not taken from the manifest's `window_log`.
pub fn window_log(base_nar_size: u64, nar_size: u64) -> u32 {
    let biggest = base_nar_size.max(nar_size).max(1);
    (64 - biggest.leading_zeros()).clamp(MIN_WINDOW_LOG, MAX_WINDOW_LOG)
}

/// What the configuration allows: programs by store path, where deltas come from, the window cap, parallelism.
#[derive(Debug, Clone)]
pub struct Settings {
    pub zstd: String,
    pub curl: String,
    pub url: String,
    pub window_log_max: u32,
    pub jobs: usize,
    /// The sum of the decoder windows of the deltas running at once (one always runs).
    pub budget: u64,
}

fn meminfo(key: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with(key))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

impl Settings {
    /// `None`: the configuration names no zstd, curl or delta URL, or turns deltas off; every path is fetched whole.
    pub fn from_config(cfg: &Config) -> Option<Settings> {
        if !cfg.deltas {
            return None;
        }
        let (zstd, curl, url) = (cfg.zstd.clone()?, cfg.curl.clone()?, cfg.delta_url.clone()?);
        let total = meminfo("MemTotal:").unwrap_or(0);
        let window_log_max = cfg
            .delta_window_log_max
            .unwrap_or(if total >= LARGE_MEMORY { LARGE_WINDOW_LOG } else { DEFAULT_WINDOW_LOG })
            .clamp(10, MAX_WINDOW_LOG);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let jobs = cfg.delta_jobs.unwrap_or((cores / 2).clamp(1, 6)).max(1);
        let budget = meminfo("MemAvailable:").map(|a| a / 2).unwrap_or(1 << 30);
        Some(Settings { zstd, curl, url, window_log_max, jobs, budget })
    }
}

/// One path to download: its references among the paths being downloaded (indices), the cache its origin names,
/// and the delta to try first, if any.
#[derive(Debug, Clone)]
pub struct Node {
    pub path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub file_size: u64,
    pub cache: String,
    /// Whole paths are fetched one cache at a time in this order (0 = upstream, 1 = Rime's cache).
    pub rank: u8,
    pub refs: Vec<usize>,
    pub delta: Option<Entry>,
}

impl Node {
    fn window(&self) -> u64 {
        self.delta.as_ref().map(|e| 1u64 << window_log(e.base_nar_size, self.nar_size)).unwrap_or(0)
    }
}

/// Why a path in the delta table was not tried (fetched whole), before anything is downloaded.
pub fn why_not(e: &Entry, nar_size: u64, s: &Settings, valid_bases: &BTreeSet<String>) -> Option<String> {
    if !valid_bases.contains(&e.base) {
        return Some(format!("its base {} is not in this machine's store", e.base));
    }
    let w = window_log(e.base_nar_size, nar_size);
    if w > s.window_log_max {
        return Some(format!("it needs a 2^{w} B decoder window and the cap is 2^{}", s.window_log_max));
    }
    None
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub from_delta: usize,
    pub delta_bytes: u64,
    pub whole: usize,
    pub whole_bytes: u64,
    /// Deltas that were tried and refused, then fetched whole: (path, why).
    pub refused: Vec<(String, String)>,
}

/// What [`schedule`] runs; the real one is [`NixExec`], tests use a fake.
pub trait Exec: Sync {
    fn delta(&self, node: &Node, e: &Entry) -> Result<(), String>;
    fn whole(&self, cache: &str, paths: &[&str]) -> Result<(), String>;
}

enum Ev {
    Delta(usize, Result<(), String>),
    Whole(Vec<usize>, Result<(), String>),
}

/// Download `nodes` in reference order: a path starts only once every reference it has among `nodes` is valid
/// (`nix copy --no-recursive` refuses a path whose reference is not). Up to `jobs` deltas at once, their windows
/// summing to at most `budget` (one always runs). Whole paths go in batches, one `nix copy` per cache (in `rank`
/// order: upstream before Rime, which may refer to it), running beside the deltas; a batch takes every whole path
/// whose unfinished references are all in the same batch (`nix copy` orders a batch itself). A refused delta
/// re-enters as a whole path; a whole download that fails stops everything.
pub fn schedule(nodes: &[Node], exec: &dyn Exec, jobs: usize, budget: u64) -> Result<Outcome, String> {
    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Waiting,
        Started,
        Done,
    }
    let n = nodes.len();
    let mut st = vec![St::Waiting; n];
    let mut whole: Vec<bool> = nodes.iter().map(|x| x.delta.is_none()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, x) in nodes.iter().enumerate() {
        for &r in &x.refs {
            if r >= n || r == i {
                return Err(format!("{}: a reference outside the download set", x.path));
            }
            dependents[r].push(i);
        }
    }
    let mut out = Outcome::default();
    let mut ready_delta: VecDeque<usize> = VecDeque::new();
    let mut queued = vec![false; n];
    let refs_done = |i: usize, st: &[St]| nodes[i].refs.iter().all(|&r| st[r] == St::Done);
    for i in 0..n {
        if !whole[i] && refs_done(i, &st) {
            ready_delta.push_back(i);
            queued[i] = true;
        }
    }
    std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<Ev>();
        let (mut running, mut used, mut whole_running, mut done) = (0usize, 0u64, false, 0usize);
        loop {
            while running < jobs.max(1) {
                let Some(&i) = ready_delta.front() else { break };
                let w = nodes[i].window();
                if running > 0 && used.saturating_add(w) > budget {
                    break;
                }
                ready_delta.pop_front();
                st[i] = St::Started;
                running += 1;
                used += w;
                let tx = tx.clone();
                let node = &nodes[i];
                s.spawn(move || {
                    let r = exec.delta(node, node.delta.as_ref().expect("a delta node"));
                    let _ = tx.send(Ev::Delta(i, r));
                });
            }
            if !whole_running {
                // Every waiting whole path whose unfinished references are done or already in this batch.
                let mut batch: Vec<usize> = Vec::new();
                let mut inb = vec![false; n];
                loop {
                    let before = batch.len();
                    for i in 0..n {
                        if whole[i] && st[i] == St::Waiting && !inb[i] && nodes[i].refs.iter().all(|&r| st[r] == St::Done || inb[r]) {
                            inb[i] = true;
                            batch.push(i);
                        }
                    }
                    if batch.len() == before {
                        break;
                    }
                }
                if !batch.is_empty() {
                    for &i in &batch {
                        st[i] = St::Started;
                    }
                    whole_running = true;
                    let tx = tx.clone();
                    s.spawn(move || {
                        let mut by_cache: BTreeMap<(u8, &str), Vec<&str>> = BTreeMap::new();
                        for &i in &batch {
                            by_cache.entry((nodes[i].rank, nodes[i].cache.as_str())).or_default().push(nodes[i].path.as_str());
                        }
                        let r = by_cache.iter().try_for_each(|((_, c), ps)| exec.whole(c, ps));
                        let _ = tx.send(Ev::Whole(batch, r));
                    });
                }
            }
            if done == n {
                return Ok(());
            }
            if running == 0 && !whole_running {
                return Err("the download order has a cycle; nothing more can start".to_string());
            }
            let mut finish = |i: usize, st: &mut Vec<St>, rd: &mut VecDeque<usize>| {
                st[i] = St::Done;
                done += 1;
                for &d in &dependents[i] {
                    if !whole[d] && !queued[d] && st[d] == St::Waiting && refs_done(d, st) {
                        queued[d] = true;
                        rd.push_back(d);
                    }
                }
            };
            match rx.recv().map_err(|e| e.to_string())? {
                Ev::Delta(i, r) => {
                    running -= 1;
                    used -= nodes[i].window();
                    match r {
                        Ok(()) => {
                            out.from_delta += 1;
                            out.delta_bytes += nodes[i].delta.as_ref().map_or(0, |e| e.file_size);
                            finish(i, &mut st, &mut ready_delta);
                        }
                        Err(why) => {
                            out.refused.push((nodes[i].path.clone(), why));
                            whole[i] = true;
                            st[i] = St::Waiting;
                        }
                    }
                }
                Ev::Whole(batch, Ok(())) => {
                    whole_running = false;
                    for i in batch {
                        out.whole += 1;
                        out.whole_bytes += nodes[i].file_size;
                        finish(i, &mut st, &mut ready_delta);
                    }
                }
                Ev::Whole(_, Err(e)) => return Err(e),
            }
        }
    })?;
    Ok(out)
}


/// The download of one release's missing paths, with the deltas that qualify.
pub struct Prepared {
    pub nodes: Vec<Node>,
    pub settings: Option<Settings>,
    /// Deltas in the manifest that are not tried, and why (no base, window over the cap, deltas off, a bad table).
    pub skipped: Vec<String>,
}

impl Prepared {
    /// True when at least one path is tried as a delta; otherwise the caller downloads as it always has.
    pub fn any(&self) -> bool {
        self.nodes.iter().any(|n| n.delta.is_some())
    }

    /// Bytes to download when every tried delta lands.
    pub fn download_bytes(&self) -> u64 {
        self.nodes.iter().map(|n| n.delta.as_ref().map_or(n.file_size, |e| e.file_size)).sum()
    }

    /// Scratch space the deltas need at once beside the store: a base NAR and a delta file per running job.
    pub fn scratch_bytes(&self) -> u64 {
        let mut v: Vec<u64> = self.nodes.iter().filter_map(|n| n.delta.as_ref()).map(|e| e.base_nar_size + e.file_size).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.iter().take(self.settings.as_ref().map_or(0, |s| s.jobs)).sum()
    }

    /// Download everything, deltas first where they qualify, whole paths otherwise or after a refusal.
    pub fn run(&self, cfg: &Config, keys: &[String], keep_fsync: bool) -> Result<Outcome, String> {
        let s = self.settings.as_ref().ok_or("deltas are off")?;
        sweep_stale();
        let work = work_dir();
        fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let exec = NixExec::new(cfg, s, keys, keep_fsync, work.clone());
        let r = schedule(&self.nodes, &exec, s.jobs, s.budget);
        let _ = fs::remove_dir_all(&work);
        r
    }
}

/// What the download did, for the terminal: how many paths came as deltas, how many whole, and why deltas were refused.
pub fn report(o: &Outcome) {
    const MB: f64 = 1e6;
    println!(
        "rime: {} paths rebuilt from deltas ({:.1} MB), {} downloaded whole ({:.1} MB)",
        o.from_delta,
        o.delta_bytes as f64 / MB,
        o.whole,
        o.whole_bytes as f64 / MB
    );
    for (path, why) in &o.refused {
        println!("rime: delta refused, downloaded whole instead: {path}: {why}");
    }
}

/// Plan the download of `want` (paths of `m` this machine lacks). `None` when a path's origin has no configured
/// cache (the caller refuses that release by name, as without deltas).
pub fn prepare(cfg: &Config, m: &Manifest, want: &[&str], no_deltas: bool) -> Option<Prepared> {
    use rimed_core::nixrel::manifest::Origin;
    let index: BTreeMap<&str, usize> = m.paths.iter().enumerate().map(|(i, p)| (p.path.as_str(), i)).collect();
    let pos: BTreeMap<&str, usize> = want.iter().enumerate().map(|(i, p)| (*p, i)).collect();
    let mut skipped = Vec::new();
    let settings = if no_deltas { None } else { Settings::from_config(cfg) };
    let table = match (&settings, entries(m)) {
        (Some(_), Ok(t)) => t,
        (_, Err(e)) => {
            skipped.push(format!("the release's delta table is not usable ({e})"));
            BTreeMap::new()
        }
        (None, Ok(t)) => {
            let n = want.iter().filter(|p| t.contains_key(**p)).count();
            if n > 0 {
                skipped.push(format!("{n} deltas not used: {}", if no_deltas { "--no-deltas" } else { "this machine's update.toml turns deltas off" }));
            }
            BTreeMap::new()
        }
    };
    let bases: Vec<&str> = want.iter().filter_map(|p| table.get(*p)).map(|e| e.base.as_str()).collect();
    let invalid_bases = if bases.is_empty() { BTreeSet::new() } else { super::store::invalid(cfg, &bases).unwrap_or_else(|_| bases.iter().map(|b| b.to_string()).collect()) };
    let valid_bases: BTreeSet<String> = bases.iter().filter(|b| !invalid_bases.contains(**b)).map(|b| b.to_string()).collect();
    let mut nodes = Vec::with_capacity(want.len());
    for p in want {
        let e = &m.paths[*index.get(p)?];
        let cache = cfg.cache_for(e.origin())?.to_string();
        let refs = e.references.iter().filter_map(|&r| m.paths.get(r)).filter_map(|r| pos.get(r.path.as_str()).copied()).filter(|&r| r != pos[p]).collect();
        let delta = match (table.get(*p), &settings) {
            (Some(d), Some(s)) => match why_not(d, e.nar_size, s, &valid_bases) {
                None => Some(d.clone()),
                Some(why) => {
                    skipped.push(format!("{p}: {why}"));
                    None
                }
            },
            _ => None,
        };
        nodes.push(Node {
            path: e.path.clone(),
            nar_hash: e.nar_hash.clone(),
            nar_size: e.nar_size,
            file_size: e.file_size,
            cache,
            rank: if e.origin() == Origin::Upstream { 0 } else { 1 },
            refs,
            delta,
        });
    }
    Some(Prepared { nodes, settings, skipped })
}

// ── the real executor ────────────────────────────────────────────────────────

/// Runs `zstd`, `curl`, `nix-store --dump` and `nix copy` by the store paths in the configuration.
pub struct NixExec<'a> {
    pub cfg: &'a Config,
    pub settings: &'a Settings,
    pub keys: String,
    pub keep_fsync: bool,
    pub work: PathBuf,
    next: AtomicUsize,
}

impl<'a> NixExec<'a> {
    pub fn new(cfg: &'a Config, settings: &'a Settings, keys: &[String], keep_fsync: bool, work: PathBuf) -> Self {
        NixExec { cfg, settings, keys: keys.join(" "), keep_fsync, work, next: AtomicUsize::new(0) }
    }

    fn copy_args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "--extra-experimental-features", "nix-command", "copy", "--no-recursive",
            "--option", "require-sigs", "true",
            "--option", "substituters", "",
            "--option", "fallback", "false",
            "--option", "min-free", "0",
            "--option", "narinfo-cache-negative-ttl", "0",
            "--option", "narinfo-cache-positive-ttl", "0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.extend(["--option".into(), "trusted-public-keys".into(), self.keys.clone()]);
        if self.keep_fsync {
            a.extend(["--option".into(), "fsync-store-paths".into(), "true".into()]);
        }
        a
    }

    fn curl(&self, url: &str, max: u64, dest: Option<&Path>) -> Result<Vec<u8>, String> {
        let max_s = max.to_string();
        let mut args: Vec<&str> = vec![
            "-fsSL", "--proto", "=https,http,file", "--proto-redir", "=https,http", "--retry", "2",
            "--connect-timeout", "30", "--speed-limit", "1024", "--speed-time", "300", "--max-filesize", &max_s,
        ];
        let d;
        if let Some(p) = dest {
            d = p.display().to_string();
            args.extend(["-o", d.as_str()]);
        }
        args.push(url);
        let out = Command::new(&self.settings.curl).args(&args).stdin(Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(out.stdout)
    }

    fn try_delta(&self, node: &Node, e: &Entry, dir: &Path) -> Result<(), String> {
        let hash32 = node.path.strip_prefix("/nix/store/").map(|b| &b[..32]).ok_or("not a store path")?;
        // 1. the signed narinfo, bound to the manifest.
        let text = self.curl(&format!("{}/{hash32}.narinfo", node.cache.trim_end_matches('/')), NARINFO_MAX, None)?;
        let text = String::from_utf8(text).map_err(|_| "the narinfo is not text".to_string())?;
        let ni = rewrite_narinfo(&text, &node.path, &node.nar_hash, node.nar_size)?;
        // 2. the delta, bound to the manifest.
        let delta = dir.join("delta");
        self.curl(&format!("{}/{}", self.settings.url.trim_end_matches('/'), e.file), e.file_size, Some(&delta))?;
        let (got, size) = hash_file(&delta)?;
        if size != e.file_size || !hash::nar_hash_bytes(&e.file_hash).is_some_and(|h| h == got) {
            return Err(format!("the delta file is not the one the manifest names ({size} B)"));
        }
        // 3. the base, which must be the base the delta was made against.
        let base = dir.join("base.nar");
        dump(&self.cfg.nix_store, &e.base, &base, e.base_nar_size, &e.base_nar_hash)?;
        // 4. a one-path cache whose NAR is a FIFO, fed by the decoder through this process.
        let cache = dir.join("cache");
        fs::create_dir_all(cache.join("nar")).map_err(|e| e.to_string())?;
        fs::write(cache.join("nix-cache-info"), "StoreDir: /nix/store\n").map_err(|e| e.to_string())?;
        fs::write(cache.join(format!("{hash32}.narinfo")), ni).map_err(|e| e.to_string())?;
        let fifo = cache.join("nar/stream.nar");
        let c = std::ffi::CString::new(fifo.display().to_string()).map_err(|e| e.to_string())?;
        // SAFETY: mkfifo on a path we own.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            return Err(format!("mkfifo: {}", std::io::Error::last_os_error()));
        }
        let w = window_log(e.base_nar_size, node.nar_size);
        stream(
            &self.settings.zstd,
            &[
                "-d".into(), "-q".into(), "-c".into(), format!("--long={w}"),
                format!("--patch-from={}", base.display()), delta.display().to_string(),
            ],
            &self.cfg.nix,
            &{
                let mut a = self.copy_args();
                a.extend(["--from".into(), format!("file://{}", cache.display()), node.path.clone()]);
                a
            },
            &fifo,
            node.nar_size,
            &node.nar_hash,
        )
    }
}

impl Exec for NixExec<'_> {
    fn delta(&self, node: &Node, e: &Entry) -> Result<(), String> {
        let dir = self.work.join(self.next.fetch_add(1, Ordering::SeqCst).to_string());
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let r = self.try_delta(node, e, &dir);
        let _ = fs::remove_dir_all(&dir);
        r
    }
    fn whole(&self, cache: &str, paths: &[&str]) -> Result<(), String> {
        let keys: Vec<String> = self.keys.split(' ').map(str::to_string).collect();
        super::store::copy_from_cache(self.cfg, cache, paths, &keys, self.keep_fsync)
    }
}

/// The signed narinfo with `URL` pointing at the local FIFO and `Compression: none`; `FileHash`/`FileSize` dropped.
/// None of those four is in the signed fingerprint (path, narHash, narSize, references), so the `Sig` lines stay
/// valid. Refused unless it describes `path` with exactly the manifest's NarHash and NarSize.
pub fn rewrite_narinfo(text: &str, path: &str, nar_hash: &str, nar_size: u64) -> Result<String, String> {
    let mut out = String::new();
    let (mut sp, mut nh, mut ns) = (None, None, None);
    for line in text.lines() {
        let Some((k, v)) = line.split_once(": ") else {
            if line.trim().is_empty() {
                continue;
            }
            return Err(format!("narinfo line {line:?} is malformed"));
        };
        match k {
            "URL" | "Compression" | "FileHash" | "FileSize" => continue,
            "StorePath" => sp = Some(v.to_string()),
            "NarHash" => nh = Some(v.to_string()),
            "NarSize" => ns = v.parse::<u64>().ok(),
            _ => {}
        }
        out.push_str(line);
        out.push('\n');
    }
    if sp.as_deref() != Some(path) {
        return Err(format!("the narinfo describes {sp:?}, not {path}"));
    }
    if !nh.as_deref().is_some_and(|h| hash::same_nar_hash(h, nar_hash)) || ns != Some(nar_size) {
        return Err("the narinfo's NarHash/NarSize are not the manifest's".to_string());
    }
    out.push_str("URL: nar/stream.nar\nCompression: none\n");
    Ok(out)
}

fn hash_file(p: &Path) -> Result<(Vec<u8>, u64), String> {
    let mut f = fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((h.finalize().to_vec(), n))
}

/// `nix-store --dump base` into `dest`, refused unless it is `size` bytes hashing to `nar_hash`.
fn dump(nix_store: &str, base: &str, dest: &Path, size: u64, nar_hash: &str) -> Result<(), String> {
    let mut child = Command::new(nix_store)
        .args(["--dump", base])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("nix-store --dump: {e}"))?;
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let mut f = fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    let mut over = false;
    loop {
        let k = out.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        n += k as u64;
        if n > size {
            over = true;
            break;
        }
        h.update(&buf[..k]);
        f.write_all(&buf[..k]).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    drop(out);
    if over {
        let _ = child.kill();
    }
    let st = child.wait_with_output().map_err(|e| e.to_string())?;
    if !over && !st.status.success() {
        return Err(format!("nix-store --dump {base}: {}", String::from_utf8_lossy(&st.stderr).trim()));
    }
    if over || n != size || !hash::nar_hash_bytes(nar_hash).is_some_and(|x| x == h.finalize().to_vec()) {
        return Err(format!("the local {base} is not the base the delta was made against"));
    }
    Ok(())
}

/// Run the decoder and the importer, and copy the decoder's output into the FIFO the importer reads, hashing it.
/// Ok only when both exit 0 and the stream was exactly `size` bytes hashing to `nar_hash` (Nix checks the same).
///
/// Nix opens a `file://` cache's NAR twice: once to try a memory map (a FIFO cannot be mapped: it closes it unread),
/// then again to read it. A plain write end would see the first close as the reader leaving (a broken pipe), or close
/// before the second open and leave Nix waiting for a writer that never comes. So this process holds the FIFO open
/// read-write: neither open by Nix blocks, the bytes wait in the pipe across Nix's reopen, and the end of the NAR is
/// signalled only once Nix has drained the pipe. Nix never sees that end unless the stream checked out.
fn stream(zstd: &str, zargs: &[String], nix: &str, nargs: &[String], fifo: &Path, size: u64, nar_hash: &str) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    // Before Nix starts, so its first open finds a writer.
    let pipe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo)
        .map_err(|e| format!("{}: {e}", fifo.display()))?;
    let mut importer = Command::new(nix)
        .args(nargs)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("nix copy: {e}"))?;
    let imp_err = importer.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut t = String::new();
            let _ = s.read_to_string(&mut t);
            t
        })
    });
    let mut decoder = match Command::new(zstd).args(zargs).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(d) => d,
        Err(e) => {
            let _ = importer.kill();
            let _ = importer.wait();
            return Err(format!("zstd: {e}"));
        }
    };
    let dec_err = decoder.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut t = String::new();
            let _ = s.read_to_string(&mut t);
            t
        })
    });
    let fd = pipe.as_raw_fd();
    let mut last = Instant::now();
    // Wait until the pipe has room (or, with `drained`, is empty); an importer that exits or stops reading for
    // OPEN_TIMEOUT ends the wait with the reason.
    let wait = |importer: &mut std::process::Child, drained: bool, last: &Instant| -> Result<(), String> {
        if let Ok(Some(st)) = importer.try_wait() {
            return Err(format!("nix copy stopped before reading the whole NAR ({st})"));
        }
        if last.elapsed() > OPEN_TIMEOUT {
            return Err(format!("nix copy read nothing for {} s", OPEN_TIMEOUT.as_secs()));
        }
        if drained {
            std::thread::sleep(Duration::from_millis(2));
        } else {
            let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            // SAFETY: one pollfd on a descriptor this function owns.
            unsafe { libc::poll(&mut p, 1, 100) };
        }
        Ok(())
    };
    let mut problem: Option<String> = None;
    let mut h = Sha256::new();
    let mut n = 0u64;
    let mut out = decoder.stdout.take().expect("piped");
    let mut buf = vec![0u8; 1 << 20];
    'read: loop {
        let k = match out.read(&mut buf) {
            Ok(0) => break,
            Ok(k) => k,
            Err(e) => {
                problem = Some(format!("reading the decoder: {e}"));
                break;
            }
        };
        n += k as u64;
        if n > size {
            problem = Some(format!("the rebuilt NAR is longer than the manifest's {size} B"));
            break;
        }
        h.update(&buf[..k]);
        let mut rest = &buf[..k];
        while !rest.is_empty() {
            match (&pipe).write(rest) {
                Ok(w) => {
                    rest = &rest[w..];
                    last = Instant::now();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::Interrupted => {
                    if let Err(p) = wait(&mut importer, false, &last) {
                        problem = Some(p);
                        break 'read;
                    }
                }
                Err(e) => {
                    problem = Some(format!("{}: {e}", fifo.display()));
                    break 'read;
                }
            }
        }
    }
    drop(out);
    if problem.is_some() {
        let _ = decoder.kill();
    }
    let dec = decoder.wait().map_err(|e| e.to_string())?;
    let dec_err = dec_err.and_then(|t| t.join().ok()).unwrap_or_default();
    let tail = |s: &str| s.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").to_string();
    if problem.is_none() && !dec.success() {
        problem = Some(format!("zstd could not rebuild the NAR: {}", tail(&dec_err)));
    }
    if problem.is_none() && (n != size || !hash::nar_hash_bytes(nar_hash).is_some_and(|x| x == h.finalize().to_vec())) {
        problem = Some(format!("the rebuilt NAR ({n} B) does not hash to the manifest's narHash"));
    }
    // The NAR checked out: let Nix drain the pipe, then end it. FIONREAD counts the bytes still in the pipe.
    let mut before = libc::c_int::MAX;
    while problem.is_none() {
        let mut queued: libc::c_int = 0;
        // SAFETY: FIONREAD writes one c_int.
        if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) } != 0 {
            problem = Some(format!("{}: {}", fifo.display(), std::io::Error::last_os_error()));
            break;
        }
        if queued == 0 {
            break;
        }
        if queued < before {
            before = queued;
            last = Instant::now();
        }
        if let Err(p) = wait(&mut importer, true, &last) {
            problem = Some(p);
        }
    }
    if problem.is_some() {
        // Before the pipe closes: a reader that has all the bytes must never see their end.
        let _ = importer.kill();
    }
    drop(pipe);
    let imp = importer.wait().map_err(|e| e.to_string())?;
    let imp_err = imp_err.and_then(|t| t.join().ok()).unwrap_or_default();
    match (problem, imp.success()) {
        (None, true) => Ok(()),
        (Some(p), _) => Err(if imp_err.trim().is_empty() { p } else { format!("{p} (nix: {})", tail(&imp_err)) }),
        (None, false) => Err(format!("nix copy refused the rebuilt NAR: {}", tail(&imp_err))),
    }
}

/// Where the per-path work happens: under the state directory, so it is on the store's file system, removed after.
pub fn work_dir() -> PathBuf {
    super::state(&format!("delta-work.{}", std::process::id()))
}

/// Remove what an interrupted run left (the update lock is held, so no other run is using it).
pub fn sweep_stale() {
    if let Ok(rd) = fs::read_dir(super::STATE_DIR) {
        for d in rd.flatten() {
            if d.file_name().to_string_lossy().starts_with("delta-work.") {
                let _ = fs::remove_dir_all(d.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn entry(path: &str, base: &str, size: u64) -> Entry {
        Entry {
            path: path.into(),
            base: base.into(),
            base_nar_hash: "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=".into(),
            base_nar_size: size,
            file: format!("2026.11.02/{}.delta", "0".repeat(52)),
            file_hash: "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=".into(),
            file_size: 10,
            window_log: 27,
        }
    }

    fn node(path: &str, refs: Vec<usize>, delta: bool) -> Node {
        Node {
            path: path.into(),
            nar_hash: String::new(),
            nar_size: 1000,
            file_size: 100,
            cache: "http://c".into(),
            rank: 1,
            refs,
            delta: delta.then(|| entry(path, "/nix/store/00000000000000000000000000000000-base", 1000)),
        }
    }

    /// Records the order things ran in; refuses the deltas named in `refuse`; checks references are done first.
    struct Fake {
        done: Mutex<Vec<String>>,
        refuse: Vec<&'static str>,
        refs: BTreeMap<String, Vec<String>>,
        whole_calls: Mutex<Vec<Vec<String>>>,
        fail_whole: bool,
    }

    impl Fake {
        fn new(nodes: &[Node], refuse: Vec<&'static str>) -> Fake {
            Fake {
                done: Mutex::new(Vec::new()),
                refuse,
                refs: nodes.iter().map(|n| (n.path.clone(), n.refs.iter().map(|&r| nodes[r].path.clone()).collect())).collect(),
                whole_calls: Mutex::new(Vec::new()),
                fail_whole: false,
            }
        }
        /// `with`: the rest of a whole batch, which `nix copy` orders itself.
        fn check_refs(&self, p: &str, with: &[&str]) {
            let done = self.done.lock().unwrap();
            for r in &self.refs[p] {
                assert!(done.contains(r) || with.contains(&r.as_str()), "{p} ran before its reference {r}");
            }
        }
    }

    impl Exec for Fake {
        fn delta(&self, node: &Node, _e: &Entry) -> Result<(), String> {
            self.check_refs(&node.path, &[]);
            std::thread::sleep(Duration::from_millis(5));
            if self.refuse.contains(&node.path.as_str()) {
                return Err("tampered".into());
            }
            self.done.lock().unwrap().push(node.path.clone());
            Ok(())
        }
        fn whole(&self, _cache: &str, paths: &[&str]) -> Result<(), String> {
            for p in paths {
                self.check_refs(p, paths);
            }
            if self.fail_whole {
                return Err("download failed".into());
            }
            self.whole_calls.lock().unwrap().push(paths.iter().map(|p| p.to_string()).collect());
            self.done.lock().unwrap().extend(paths.iter().map(|p| p.to_string()));
            Ok(())
        }
    }

    #[test]
    fn references_first_and_a_refused_delta_is_fetched_whole() {
        // c (delta) -> b (whole) -> a (delta); d (delta) on its own, refused.
        let nodes = vec![node("a", vec![], true), node("b", vec![0], false), node("c", vec![1], true), node("d", vec![], true)];
        let f = Fake::new(&nodes, vec!["d"]);
        let o = schedule(&nodes, &f, 4, u64::MAX).unwrap();
        assert_eq!(o.from_delta, 2);
        assert_eq!(o.whole, 2);
        assert_eq!(o.delta_bytes, 20);
        assert_eq!(o.whole_bytes, 200);
        assert_eq!(o.refused, vec![("d".to_string(), "tampered".to_string())]);
        let done = f.done.lock().unwrap();
        assert_eq!(done.len(), 4);
        assert!(done.contains(&"d".to_string()), "the refused delta was not fetched whole");
    }

    #[test]
    fn whole_paths_that_only_wait_on_each_other_share_one_batch_upstream_first() {
        // a <- b <- c all whole, d (whole, upstream) <- e (whole, Rime): one batch, two caches, upstream first.
        let mut nodes = vec![
            node("a", vec![], false),
            node("b", vec![0], false),
            node("c", vec![1], false),
            node("d", vec![], false),
            node("e", vec![3], false),
        ];
        nodes[3].cache = "http://up".into();
        nodes[3].rank = 0;
        let f = Fake::new(&nodes, vec![]);
        let o = schedule(&nodes, &f, 2, u64::MAX).unwrap();
        assert_eq!(o.whole, 5);
        let calls = f.whole_calls.lock().unwrap();
        assert_eq!(*calls, vec![vec!["d".to_string()], vec!["a".to_string(), "b".into(), "c".into(), "e".into()]]);
    }

    #[test]
    fn a_failed_whole_download_stops_the_update() {
        let nodes = vec![node("a", vec![], false), node("b", vec![0], true)];
        let mut f = Fake::new(&nodes, vec![]);
        f.fail_whole = true;
        assert_eq!(schedule(&nodes, &f, 2, u64::MAX).unwrap_err(), "download failed");
        assert!(f.done.lock().unwrap().is_empty(), "b ran before its reference a");
    }

    #[test]
    fn the_window_budget_limits_parallel_deltas_but_one_always_runs() {
        struct Count(Mutex<(usize, usize)>);
        impl Exec for Count {
            fn delta(&self, _n: &Node, _e: &Entry) -> Result<(), String> {
                {
                    let mut c = self.0.lock().unwrap();
                    c.0 += 1;
                    c.1 = c.1.max(c.0);
                }
                std::thread::sleep(Duration::from_millis(20));
                self.0.lock().unwrap().0 -= 1;
                Ok(())
            }
            fn whole(&self, _c: &str, _p: &[&str]) -> Result<(), String> {
                Ok(())
            }
        }
        let nodes: Vec<Node> = (0..6).map(|i| node(&format!("p{i}"), vec![], true)).collect();
        let w = nodes[0].window();
        assert_eq!(w, 1 << 27);
        let c = Count(Mutex::new((0, 0)));
        assert_eq!(schedule(&nodes, &c, 8, 2 * w).unwrap().from_delta, 6);
        assert_eq!(c.0.lock().unwrap().1, 2, "two windows fit the budget");
        let c = Count(Mutex::new((0, 0)));
        schedule(&nodes, &c, 8, 1).unwrap();
        assert_eq!(c.0.lock().unwrap().1, 1, "a budget below one window still runs one at a time");
        let c = Count(Mutex::new((0, 0)));
        schedule(&nodes, &c, 3, u64::MAX).unwrap();
        assert_eq!(c.0.lock().unwrap().1, 3, "jobs caps the parallelism");
    }

    #[test]
    fn a_cycle_or_a_foreign_reference_is_an_error_not_a_hang() {
        let nodes = vec![node("a", vec![1], false), node("b", vec![0], false)];
        let f = Fake::new(&nodes, vec![]);
        assert!(schedule(&nodes, &f, 2, u64::MAX).unwrap_err().contains("cycle"));
        let nodes = vec![node("a", vec![5], false)];
        let f = Fake::new(&[node("a", vec![], false)], vec![]);
        assert!(schedule(&nodes, &f, 2, u64::MAX).is_err());
    }

    #[test]
    fn window_logs_follow_zstd_and_the_cap_refuses_before_downloading() {
        assert_eq!(window_log(1000, 1000), 27);
        assert_eq!(window_log(200 << 20, 10), 28);
        assert_eq!(window_log(865_000_000, 865_100_000), 30);
        assert_eq!(window_log(u64::MAX, 0), 31);
        let s = Settings { zstd: "z".into(), curl: "c".into(), url: "u".into(), window_log_max: 29, jobs: 1, budget: 1 };
        let e = entry("/nix/store/11111111111111111111111111111111-firmware", "/nix/store/00000000000000000000000000000000-firmware", 865_000_000);
        let bases: BTreeSet<String> = [e.base.clone()].into();
        assert!(why_not(&e, 865_000_000, &s, &bases).unwrap().contains("cap is 2^29"));
        let s30 = Settings { window_log_max: 30, ..s.clone() };
        assert_eq!(why_not(&e, 865_000_000, &s30, &bases), None);
        assert!(why_not(&e, 865_000_000, &s30, &BTreeSet::new()).unwrap().contains("not in this machine's store"));
    }

    #[test]
    fn the_narinfo_keeps_its_signature_and_must_be_the_manifests() {
        let p = "/nix/store/0fsxp2bgcinx2wmqw6w5x82j2bmsvl8-hello";
        let ni = format!(
            "StorePath: {p}\nURL: nar/x.nar.zst\nCompression: zstd\nFileHash: sha256:aa\nFileSize: 9\n\
             NarHash: sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73\nNarSize: 0\nReferences: \nSig: k-1:abc==\n"
        );
        let sri = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        let out = rewrite_narinfo(&ni, p, sri, 0).unwrap();
        assert!(out.contains("Sig: k-1:abc==\n") && out.contains("URL: nar/stream.nar\nCompression: none\n"));
        assert!(!out.contains("FileHash") && !out.contains("FileSize") && !out.contains("zstd"));
        assert!(rewrite_narinfo(&ni, p, sri, 1).is_err(), "NarSize differs");
        assert!(rewrite_narinfo(&ni, p, "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 0).is_err(), "NarHash differs");
        assert!(rewrite_narinfo(&ni, "/nix/store/1fsxp2bgcinx2wmqw6w5x82j2bmsvl8-hello", sri, 0).is_err());
    }

    /// `stream` against real processes: `cat` as the decoder, `sh` as the importer.
    fn run_stream(reader: &str, data: &[u8], size: u64, claimed: &[u8]) -> (Result<(), String>, Vec<u8>, Duration) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("rime-delta-stream-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (src, fifo, out) = (dir.join("nar"), dir.join("stream.nar"), dir.join("out"));
        fs::write(&src, data).unwrap();
        let c = std::ffi::CString::new(fifo.display().to_string()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let hex: String = Sha256::digest(claimed).iter().map(|b| format!("{b:02x}")).collect();
        let t = Instant::now();
        let r = stream(
            "cat",
            &[src.display().to_string()],
            "sh",
            &["-c".into(), reader.into(), "sh".into(), fifo.display().to_string(), out.display().to_string()],
            &fifo,
            size,
            &format!("sha256:{hex}"),
        );
        let el = t.elapsed();
        let got = fs::read(&out).unwrap_or_default();
        let _ = fs::remove_dir_all(&dir);
        (r, got, el)
    }

    fn bytes(n: usize) -> Vec<u8> {
        let mut x: u32 = 2463534242;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    /// What Nix's LocalBinaryCacheStore does with a NAR: open it for a memory map, close it unread, open it again.
    const NIX_LIKE: &str = r#"exec 3<"$1"; exec 3<&-; sleep 0.2; cat "$1" > "$2""#;

    #[test]
    fn the_stream_survives_nix_opening_the_nar_twice_small_and_large() {
        for n in [41 * 1024, 3 << 20] {
            let d = bytes(n);
            let (r, got, _) = run_stream(NIX_LIKE, &d, n as u64, &d);
            assert_eq!(r, Ok(()), "{n} B");
            assert!(got == d, "{n} B: the reader got {} B, not the stream", got.len());
        }
    }

    #[test]
    fn an_importer_that_quits_without_reading_fails_the_delta_without_a_hang() {
        let d = bytes(3 << 20);
        let (r, _, el) = run_stream(r#"exec 3<"$1"; exec 3<&-; exit 1"#, &d, d.len() as u64, &d);
        assert!(r.as_ref().is_err_and(|e| e.contains("stopped before reading")), "{r:?}");
        assert!(el < Duration::from_secs(30), "{el:?}");
        let d = bytes(1000);
        let (r, _, el) = run_stream("exit 1", &d, d.len() as u64, &d);
        assert!(r.is_err(), "{r:?}");
        assert!(el < Duration::from_secs(30), "{el:?}");
    }

    #[test]
    fn a_stream_that_does_not_hash_right_never_reaches_its_end() {
        // The reader has every byte but is killed before the end: `cat` exits non-zero, so nothing is imported.
        let d = bytes(41 * 1024);
        let mut other = d.clone();
        other[7] ^= 1;
        let (r, _, el) = run_stream(NIX_LIKE, &d, d.len() as u64, &other);
        assert!(r.as_ref().is_err_and(|e| e.contains("does not hash")), "{r:?}");
        assert!(el < Duration::from_secs(30), "{el:?}");
        let (r, _, _) = run_stream(NIX_LIKE, &d, d.len() as u64 - 1, &d);
        assert!(r.as_ref().is_err_and(|e| e.contains("longer than")), "{r:?}");
    }

    #[test]
    fn delta_file_names_are_tag_slash_nix32_dot_delta() {
        let h = "0".repeat(52);
        assert!(valid_file_name(&format!("2026.11.02/{h}.delta")));
        assert!(valid_file_name(&format!("2026.11.02-part2/{h}.delta")));
        assert!(!valid_file_name(&format!("../{h}.delta")));
        assert!(!valid_file_name(&format!("a/b/{h}.delta")));
        assert!(!valid_file_name(&format!("2026.11.02/{h}.nar.zst")));
        assert!(!valid_file_name(&format!("2026.11.02/{}.delta", "e".repeat(52))));
    }
}
