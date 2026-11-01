//! Every absolute `/usr/...` path in the workspace's non-test code comes from
//! `rime_paths`, apart from the entries in [`ALLOWED`].
//!
//! A new `"/usr/bin/foo"` written straight into a crate builds and passes every
//! test on the bootc image, where `/usr/bin/foo` exists, and then fails on NixOS
//! where it does not. This test is what catches it before a NixOS machine does.
//!
//! "Non-test code" is read the way the planning audit counted it: test
//! directories, `#[cfg(test)]` items (inline modules, functions, and the files a
//! `#[cfg(test)] mod x;` points at) and `//` comment lines are left out.

use std::fs;
use std::path::{Path, PathBuf};

/// Literals that may stay, each with its reason. Matched by file and by the
/// literal's leading text (everything after the opening quote).
///
/// Everything here describes the bootc IMAGE (a tree under a deployment root, or
/// a classification table over one), not the machine the binary runs on, so on
/// NixOS it is never executed: those verbs are rewritten against Nix
/// generations (PLAN.md 4.3, "Thrown away" / "Rewritten").
const ALLOWED: &[(&str, &str, &str)] = &[
    ("rimed-core/src/live/classify.rs", "/usr/", "bootc live engine: classifies paths inside a staged ostree deployment"),
    ("rimed-core/src/live/plan.rs", "/usr/lib/systemd/system/", "bootc live engine: a changed unit inside the staged deployment"),
    ("rime/src/live.rs", "/usr/", "bootc live engine: paths inside a staged deployment and its library dirs"),
    ("rimed-core/src/blueprint.rs", "/usr/", "a blueprint may not write under /usr: a policy prefix, not a file"),
    ("rime-aid/src/runtime.rs", "/usr/", "rime-aid sandbox: binds /usr and refuses to re-bind under it; its NixOS layout is the sandbox rewrite (TODO(handover))"),
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if name == "target" || name == "tests" || name == "testdata" || name == "benches" {
                continue;
            }
            rust_files(&p, out);
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

/// Where `mod name;` declared in `file` lives.
fn module_file(file: &Path, name: &str) -> Vec<PathBuf> {
    let dir = file.parent().unwrap();
    let stem = file.file_stem().unwrap().to_string_lossy();
    let base = if stem == "mod" || stem == "lib" || stem == "main" {
        dir.to_path_buf()
    } else {
        dir.join(&*stem)
    };
    vec![base.join(format!("{name}.rs")), base.join(name).join("mod.rs")]
}

/// The lines of `text` that are not test code, with their 1-based numbers, and
/// the files of any `#[cfg(test)] mod x;` it declares.
fn non_test_lines(file: &Path, text: &str) -> (Vec<(usize, String)>, Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = Vec::new();
    let mut test_files = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t == "#[cfg(test)]" {
            // Skip further attributes, then the item they decorate.
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().starts_with("#[") {
                j += 1;
            }
            if j >= lines.len() {
                break;
            }
            let item = lines[j].trim();
            if let Some(rest) = item.strip_prefix("mod ").or_else(|| item.strip_prefix("pub mod ")).or_else(|| item.strip_prefix("pub(crate) mod ")) {
                if let Some(name) = rest.strip_suffix(';') {
                    test_files.extend(module_file(file, name.trim()));
                    i = j + 1;
                    continue;
                }
            }
            // Brace-count to the end of the item (a one-line item ends at `;`).
            let mut depth: i64 = 0;
            let mut opened = false;
            let mut k = j;
            while k < lines.len() {
                for c in lines[k].chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if (opened && depth <= 0) || (!opened && lines[k].trim_end().ends_with(';')) {
                    break;
                }
                k += 1;
            }
            i = k + 1;
            continue;
        }
        if !t.starts_with("//") {
            keep.push((i + 1, lines[i].to_string()));
        }
        i += 1;
    }
    (keep, test_files)
}

#[test]
fn every_usr_literal_comes_from_rime_paths() {
    let root = workspace();
    let mut files = Vec::new();
    for crate_dir in fs::read_dir(&root).unwrap().flatten() {
        let p = crate_dir.path();
        if p.is_dir() && p.file_name().unwrap() != "rime-paths" && p.join("Cargo.toml").is_file() {
            rust_files(&p.join("src"), &mut files);
        }
    }
    files.sort();
    assert!(files.len() > 100, "found only {} source files under {}", files.len(), root.display());

    let mut test_files = Vec::new();
    let mut scanned = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        let (lines, tf) = non_test_lines(f, &text);
        test_files.extend(tf);
        scanned.push((f.clone(), lines));
    }

    let mut stray = Vec::new();
    let mut allowed_hits = vec![0usize; ALLOWED.len()];
    for (f, lines) in &scanned {
        if test_files.contains(f) {
            continue;
        }
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        for (n, line) in lines {
            let mut rest = line.as_str();
            while let Some(at) = rest.find("\"/usr/") {
                let lit = &rest[at + 1..];
                let lit_end = lit[1..].find('"').map(|e| e + 1).unwrap_or(lit.len());
                let literal = &lit[..lit_end];
                match ALLOWED.iter().position(|(af, pre, _)| *af == rel && literal.starts_with(pre)) {
                    Some(k) => allowed_hits[k] += 1,
                    None => stray.push(format!("{rel}:{n}: \"{literal}\"")),
                }
                rest = &lit[lit_end..];
            }
        }
    }
    assert!(
        stray.is_empty(),
        "absolute /usr paths outside rime-paths (route them through rime_paths, or add them to ALLOWED with the reason):\n  {}",
        stray.join("\n  ")
    );
    // An allowance that matches nothing is stale: remove it so it cannot hide a new literal later.
    for (k, hits) in allowed_hits.iter().enumerate() {
        assert!(*hits > 0, "ALLOWED entry {:?} matches nothing; remove it", ALLOWED[k]);
    }
}
