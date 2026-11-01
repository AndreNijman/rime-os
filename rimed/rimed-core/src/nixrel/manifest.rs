//! The release manifest (docs/nixos/updates-and-release.md §3): its shape, and the validation that runs on it
//! before anything is downloaded (§3.2). Pure: the caller fetched and verified the bytes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{hash, release::ReleaseId, MANIFEST_SCHEMA, RELEASE_KIND};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub rime_os: String,
    #[serde(default)]
    pub rime_shell: String,
    #[serde(default)]
    pub nixpkgs: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub url_hint: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boot {
    pub kernel: usize,
    pub initrd: usize,
    #[serde(default)]
    pub kernel_modules: Option<usize>,
    #[serde(default)]
    pub kernel_params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    pub toplevel: usize,
    pub boot: Boot,
    #[serde(default)]
    pub components: BTreeMap<String, Vec<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueEntry {
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub out: usize,
    #[serde(default)]
    pub class: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    pub path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub file_size: u64,
    #[serde(default)]
    pub references: Vec<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    #[serde(default)]
    pub mass_rebuild: bool,
    #[serde(default)]
    pub nixpkgs_moved: bool,
    #[serde(default)]
    pub security: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub kind: String,
    pub release: String,
    #[serde(default)]
    pub issued: u64,
    #[serde(default)]
    pub previous: Option<String>,
    pub system: String,
    pub store_dir: String,
    #[serde(default = "no_source")]
    pub source: Source,
    pub cache: Cache,
    pub variants: BTreeMap<String, Variant>,
    #[serde(default)]
    pub catalogue: Vec<CatalogueEntry>,
    pub paths: Vec<PathEntry>,
    #[serde(default)]
    pub batch: Option<Batch>,
    /// Informational (§3.1); kept as raw JSON so a publisher's additions never fail a parse.
    #[serde(default)]
    pub delta: Option<serde_json::Value>,
}

fn no_source() -> Source {
    Source { rime_os: String::new(), rime_shell: String::new(), nixpkgs: String::new() }
}

/// Why a manifest was refused. The two kinds read differently to the user (§14 F8/F9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `schema` is newer than this client (F9).
    NeedsNewerClient { release: String, schema: u32 },
    /// Anything else (F8).
    Unusable { release: String, reason: String },
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::NeedsNewerClient { release, .. } => {
                format!("rime: release {release} needs a newer rime than this one. Nothing was changed.")
            }
            Refusal::Unusable { release, reason } => {
                format!("rime: the release manifest for {release} is not usable: {reason}. Nothing was changed.")
            }
        }
    }
}

/// Parse and validate in one step: the schema is read first, so a schema-2 document with an incompatible
/// shape is reported as "needs a newer rime", not as unparseable.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Refusal> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| Refusal::Unusable {
        release: "?".into(),
        reason: format!("it is not JSON ({e})"),
    })?;
    let release = v.get("release").and_then(|r| r.as_str()).unwrap_or("?").to_string();
    match v.get("schema").and_then(|s| s.as_u64()) {
        Some(s) if s == MANIFEST_SCHEMA as u64 => {}
        Some(s) if s > MANIFEST_SCHEMA as u64 => {
            return Err(Refusal::NeedsNewerClient { release, schema: s as u32 })
        }
        other => {
            return Err(Refusal::Unusable { release, reason: format!("schema is {other:?}, expected 1") })
        }
    }
    let m: Manifest = serde_json::from_value(v)
        .map_err(|e| Refusal::Unusable { release: release.clone(), reason: format!("it does not parse ({e})") })?;
    m.validate().map_err(|reason| Refusal::Unusable { release, reason })?;
    Ok(m)
}

/// A syntactically valid store path under /nix/store.
pub fn is_store_path(p: &str) -> bool {
    let Some(base) = p.strip_prefix("/nix/store/") else { return false };
    if base.contains('/') {
        return false;
    }
    let Some((h, name)) = base.split_once('-') else { return false };
    h.len() == 32
        && h.bytes().all(|b| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&b))
        && !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
}

/// A narinfo public key: `<name>:<base64 of 32 bytes>`.
pub fn is_cache_key(k: &str) -> bool {
    match k.split_once(':') {
        Some((name, key)) => {
            !name.is_empty()
                && !name.contains(char::is_whitespace)
                && hash::b64_decode(key).is_some_and(|b| b.len() == 32)
        }
        None => false,
    }
}

impl Manifest {
    /// §3.2, every rule. The first failure is the reason.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(format!("schema {} is not 1", self.schema));
        }
        if self.kind != RELEASE_KIND {
            return Err(format!("kind is {:?}, not {RELEASE_KIND}", self.kind));
        }
        if self.system != "x86_64-linux" {
            return Err(format!("it is for {}, not x86_64-linux", self.system));
        }
        if self.store_dir != "/nix/store" {
            return Err(format!("its store is {}, not /nix/store", self.store_dir));
        }
        if ReleaseId::parse(&self.release).is_none() {
            return Err(format!("{:?} is not a release ID", self.release));
        }
        let n = self.paths.len();
        let mut seen = BTreeSet::new();
        for (i, p) in self.paths.iter().enumerate() {
            if !is_store_path(&p.path) {
                return Err(format!("path {i} ({:?}) is not a store path", p.path));
            }
            if !seen.insert(p.path.as_str()) {
                return Err(format!("{} is listed twice", p.path));
            }
            if hash::nar_hash_bytes(&p.nar_hash).is_none() {
                return Err(format!("{} has an unreadable nar_hash {:?}", p.path, p.nar_hash));
            }
            if p.nar_size == 0 || p.file_size == 0 {
                return Err(format!("{} has a zero size", p.path));
            }
            if let Some(r) = p.references.iter().find(|r| **r >= n) {
                return Err(format!("{} refers to index {r}, outside the table", p.path));
            }
        }
        if self.cache.keys.is_empty() {
            return Err("it announces no cache keys".into());
        }
        if let Some(k) = self.cache.keys.iter().find(|k| !is_cache_key(k)) {
            return Err(format!("cache key {k:?} is not <name>:<base64 ed25519 key>"));
        }
        if self.variants.is_empty() {
            return Err("it has no variants".into());
        }
        for (name, v) in &self.variants {
            if v.toplevel >= n {
                return Err(format!("variant {name}'s toplevel is outside the table"));
            }
            let closure = self.closure_of(v.toplevel);
            let mut need: Vec<(String, usize)> = vec![("boot.kernel".into(), v.boot.kernel), ("boot.initrd".into(), v.boot.initrd)];
            if let Some(m) = v.boot.kernel_modules {
                need.push(("boot.kernel_modules".into(), m));
            }
            for (c, idx) in &v.components {
                for i in idx {
                    need.push((format!("components.{c}"), *i));
                }
            }
            for (what, i) in need {
                if i >= n {
                    return Err(format!("variant {name}'s {what} is outside the table"));
                }
                if !closure.contains(&i) {
                    return Err(format!("variant {name}'s {what} ({}) is not in its toplevel's closure", self.paths[i].path));
                }
            }
        }
        for c in &self.catalogue {
            if c.out >= n {
                return Err(format!("catalogue entry {} is outside the table", c.name));
            }
        }
        Ok(())
    }

    /// Indices reachable from `root` through `references` (root included). Out-of-range references are skipped
    /// here; [`Self::validate`] refuses them.
    pub fn closure_of(&self, root: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if i >= self.paths.len() || !seen.insert(i) {
                continue;
            }
            stack.extend(self.paths[i].references.iter().copied());
        }
        seen
    }

    pub fn variant(&self, name: &str) -> Option<&Variant> {
        self.variants.get(name)
    }

    pub fn path(&self, i: usize) -> &str {
        &self.paths[i].path
    }

    /// The closure of a variant's toplevel, as path → entry.
    pub fn variant_closure(&self, v: &Variant) -> BTreeMap<&str, &PathEntry> {
        self.closure_of(v.toplevel).into_iter().map(|i| (self.paths[i].path.as_str(), &self.paths[i])).collect()
    }
}

/// What a narinfo said about one path, as `nix path-info --json --json-format 1` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarInfo {
    pub nar_hash: String,
    pub nar_size: u64,
    pub download_size: Option<u64>,
    pub references: BTreeSet<String>,
}

/// Parse `nix path-info --json --json-format 1` output (an object keyed by path; a missing path is `null`).
pub fn parse_path_info(text: &str) -> Result<BTreeMap<String, Option<NarInfo>>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("path-info JSON: {e}"))?;
    let obj = v.as_object().ok_or("path-info JSON is not an object")?;
    let mut out = BTreeMap::new();
    for (path, info) in obj {
        if info.is_null() {
            out.insert(path.clone(), None);
            continue;
        }
        let nar_hash = info.get("narHash").and_then(|x| x.as_str()).ok_or_else(|| format!("{path}: no narHash"))?;
        let nar_size = info.get("narSize").and_then(|x| x.as_u64()).ok_or_else(|| format!("{path}: no narSize"))?;
        let download_size = info.get("downloadSize").and_then(|x| x.as_u64());
        let references = info
            .get("references")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        out.insert(path.clone(), Some(NarInfo { nar_hash: nar_hash.to_string(), nar_size, download_size, references }));
    }
    Ok(out)
}

/// §8.5 step 2: every narinfo for `paths` must equal the manifest's entry (narHash, narSize, FileSize,
/// references). `Err` carries the first offending path and what differed; a path the cache does not hold is
/// `Missing` (F13), anything else is `Mismatch` (F14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    Missing(String),
    Mismatch { path: String, what: String },
}

pub fn bind(m: &Manifest, paths: &[&str], infos: &BTreeMap<String, Option<NarInfo>>) -> Result<(), BindError> {
    let index: BTreeMap<&str, &PathEntry> = m.paths.iter().map(|p| (p.path.as_str(), p)).collect();
    for p in paths {
        let Some(want) = index.get(p) else {
            return Err(BindError::Mismatch { path: p.to_string(), what: "it is not in the manifest".into() });
        };
        let Some(Some(got)) = infos.get(*p) else { return Err(BindError::Missing(p.to_string())) };
        if !hash::same_nar_hash(&got.nar_hash, &want.nar_hash) {
            return Err(BindError::Mismatch { path: p.to_string(), what: format!("narHash {} != {}", got.nar_hash, want.nar_hash) });
        }
        if got.nar_size != want.nar_size {
            return Err(BindError::Mismatch { path: p.to_string(), what: format!("narSize {} != {}", got.nar_size, want.nar_size) });
        }
        if got.download_size != Some(want.file_size) {
            return Err(BindError::Mismatch {
                path: p.to_string(),
                what: format!("FileSize {:?} != {}", got.download_size, want.file_size),
            });
        }
        let want_refs: BTreeSet<String> = want.references.iter().map(|i| m.paths[*i].path.clone()).collect();
        if got.references != want_refs {
            return Err(BindError::Mismatch { path: p.to_string(), what: "references differ".into() });
        }
    }
    Ok(())
}

/// §8.6: what is in the local store must be EXACTLY the variant's closure, same narHash for every path.
pub fn check_local(m: &Manifest, v: &Variant, local: &BTreeMap<String, Option<NarInfo>>) -> Result<(), String> {
    let want = m.variant_closure(v);
    for (p, info) in local {
        let Some(e) = want.get(p.as_str()) else {
            return Err(format!("{p} is in the installed closure and not in the signed release"));
        };
        match info {
            Some(i) if hash::same_nar_hash(&i.nar_hash, &e.nar_hash) => {}
            Some(i) => return Err(format!("{p}: installed narHash {} differs from the signed {}", i.nar_hash, e.nar_hash)),
            None => return Err(format!("{p} is not valid in the local store")),
        }
    }
    if let Some(p) = want.keys().find(|p| !local.contains_key(**p)) {
        return Err(format!("{p} is in the signed release and not in the installed closure"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sp(c: char, name: &str) -> String {
        format!("/nix/store/{}-{name}", c.to_string().repeat(32))
    }

    const H1: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const H1_NIX32: &str = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
    const H2: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const KEY: &str = "rime-test-cache-1:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    /// A small closed manifest: toplevel(0) -> kernel(1), initrd(2), shell(3) -> glibc(4).
    pub(crate) fn good() -> serde_json::Value {
        let e = |c, n: &str, refs: Vec<usize>| {
            serde_json::json!({"path": sp(c, n), "nar_hash": H1, "nar_size": 100, "file_size": 40, "references": refs})
        };
        serde_json::json!({
            "schema": 1, "kind": "rime-nixos-release", "release": "2026.12.03", "issued": 1796260000,
            "previous": "2026.11.26", "system": "x86_64-linux", "store_dir": "/nix/store",
            "source": {"rime_os": "a".repeat(40), "rime_shell": "b".repeat(40), "nixpkgs": "c".repeat(40)},
            "cache": {"url_hint": "https://cache.rimeos.com", "keys": [KEY]},
            "variants": {"plain": {"toplevel": 0, "boot": {"kernel": 1, "initrd": 2, "kernel_params": ["quiet"]},
                         "components": {"shell": [3], "kernel": [1]}}},
            "catalogue": [],
            "paths": [e('0', "nixos-system-a", vec![1, 2, 3]), e('1', "linux-6.12", vec![]), e('2', "initrd-linux-6.12", vec![]),
                      e('3', "rime-shell-1", vec![4]), e('4', "glibc-2.42", vec![])],
            "batch": {"mass_rebuild": false, "nixpkgs_moved": false, "security": []},
            "some_future_field": {"ignored": true}
        })
    }

    fn parse_v(v: &serde_json::Value) -> Result<Manifest, Refusal> {
        parse(serde_json::to_vec(v).unwrap().as_slice())
    }

    fn reason(v: &serde_json::Value) -> String {
        match parse_v(v) {
            Err(Refusal::Unusable { reason, .. }) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_good_manifest_parses_and_unknown_fields_are_ignored() {
        let m = parse_v(&good()).unwrap();
        assert_eq!(m.release, "2026.12.03");
        assert_eq!(m.closure_of(0).len(), 5);
    }

    #[test]
    fn a_newer_schema_needs_a_newer_client() {
        let mut v = good();
        v["schema"] = 2.into();
        v["paths"] = "a different shape".into();
        assert!(matches!(parse_v(&v), Err(Refusal::NeedsNewerClient { schema: 2, .. })));
        assert!(parse_v(&v).unwrap_err().message().contains("needs a newer rime"));
    }

    #[test]
    fn every_validation_rule_refuses() {
        let cases: Vec<(&str, Box<dyn Fn(&mut serde_json::Value)>)> = vec![
            ("kind", Box::new(|v| v["kind"] = "rime-bootc-release".into())),
            ("aarch64", Box::new(|v| v["system"] = "aarch64-linux".into())),
            ("store", Box::new(|v| v["store_dir"] = "/gnu/store".into())),
            ("release ID", Box::new(|v| v["release"] = "latest".into())),
            ("not a store path", Box::new(|v| v["paths"][1]["path"] = "/usr/lib/vmlinuz".into())),
            ("listed twice", Box::new(|v| v["paths"][2]["path"] = v["paths"][1]["path"].clone())),
            ("nar_hash", Box::new(|v| v["paths"][1]["nar_hash"] = "md5:abc".into())),
            ("zero size", Box::new(|v| v["paths"][1]["file_size"] = 0.into())),
            ("outside the table", Box::new(|v| v["paths"][3]["references"] = serde_json::json!([9]))),
            ("no cache keys", Box::new(|v| v["cache"]["keys"] = serde_json::json!([]))),
            ("cache key", Box::new(|v| v["cache"]["keys"] = serde_json::json!(["rime-cache-1:short"]))),
            // initrd dropped from the toplevel's references: the boot path is no longer in its closure.
            ("closure", Box::new(|v| v["paths"][0]["references"] = serde_json::json!([1, 3]))),
            // the shell path is no longer reachable from the toplevel.
            ("components.shell", Box::new(|v| v["paths"][0]["references"] = serde_json::json!([1, 2]))),
        ];
        for (want, f) in cases {
            let mut v = good();
            f(&mut v);
            let r = reason(&v);
            assert!(r.contains(want), "{want}: {r}");
        }
    }

    #[test]
    fn bind_compares_hash_size_filesize_and_references() {
        let m = parse_v(&good()).unwrap();
        let shell = sp('3', "rime-shell-1");
        let glibc = sp('4', "glibc-2.42");
        let ok = NarInfo { nar_hash: H1_NIX32.into(), nar_size: 100, download_size: Some(40), references: [glibc.clone()].into() };
        let mut infos = BTreeMap::new();
        infos.insert(shell.clone(), Some(ok.clone()));
        assert_eq!(bind(&m, &[&shell], &infos), Ok(()));

        let mut bad = ok.clone();
        bad.nar_hash = H2.into();
        infos.insert(shell.clone(), Some(bad));
        assert!(matches!(bind(&m, &[&shell], &infos), Err(BindError::Mismatch { what, .. }) if what.contains("narHash")));

        let mut big = ok.clone();
        big.download_size = Some(4_000_000_000);
        infos.insert(shell.clone(), Some(big));
        assert!(matches!(bind(&m, &[&shell], &infos), Err(BindError::Mismatch { what, .. }) if what.contains("FileSize")));

        let mut refs = ok.clone();
        refs.references = BTreeSet::new();
        infos.insert(shell.clone(), Some(refs));
        assert!(matches!(bind(&m, &[&shell], &infos), Err(BindError::Mismatch { what, .. }) if what.contains("references")));

        infos.insert(shell.clone(), None);
        assert_eq!(bind(&m, &[&shell], &infos), Err(BindError::Missing(shell.clone())));
    }

    #[test]
    fn the_local_closure_must_be_exactly_the_signed_one() {
        let m = parse_v(&good()).unwrap();
        let v = m.variant("plain").unwrap();
        let mut local: BTreeMap<String, Option<NarInfo>> = m
            .paths
            .iter()
            .map(|p| (p.path.clone(), Some(NarInfo { nar_hash: H1.into(), nar_size: 100, download_size: None, references: BTreeSet::new() })))
            .collect();
        assert_eq!(check_local(&m, v, &local), Ok(()));
        local.get_mut(&sp('3', "rime-shell-1")).unwrap().as_mut().unwrap().nar_hash = H2.into();
        assert!(check_local(&m, v, &local).unwrap_err().contains("differs from the signed"));
        local.remove(&sp('3', "rime-shell-1"));
        assert!(check_local(&m, v, &local).unwrap_err().contains("not in the installed closure"));
        local.insert(sp('3', "rime-shell-1"), Some(NarInfo { nar_hash: H1.into(), nar_size: 1, download_size: None, references: BTreeSet::new() }));
        local.insert(sp('9', "extra"), Some(NarInfo { nar_hash: H1.into(), nar_size: 1, download_size: None, references: BTreeSet::new() }));
        assert!(check_local(&m, v, &local).unwrap_err().contains("not in the signed release"));
    }

    #[test]
    fn path_info_json_parses_with_missing_paths() {
        let text = format!(
            r#"{{"{}":{{"narHash":"{H1}","narSize":100,"downloadSize":40,"references":["{}"],"signatures":["x:y"]}},"{}":null}}"#,
            sp('3', "rime-shell-1"),
            sp('4', "glibc-2.42"),
            sp('5', "gone")
        );
        let m = parse_path_info(&text).unwrap();
        assert_eq!(m[&sp('3', "rime-shell-1")].as_ref().unwrap().download_size, Some(40));
        assert!(m[&sp('5', "gone")].is_none());
    }
}
