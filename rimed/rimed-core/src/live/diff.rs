//! What changed between the booted tree and the staged one.
//!
//! Two sources, both reading content that is already on disk and already part
//! of a verified deployment, so a plan never needs a second download:
//!
//! * `ostree diff <booted-commit> <staged-commit>` on an ostree-backed machine.
//!   Measured on the L16 for 2026.10.09 → .10: 14 lines in 0.23 s. Fast
//!   because ostree compares object checksums and reads no file data.
//! * `composefs-info dump` of each deployment's composefs image, which works on
//!   both backends (ostree writes one per deployment as `.ostree.cfs`; the
//!   composefs backend has nothing else). 210,974 lines per image, 0.9 s each.
//!   A file's dump line carries its content digest, so comparing lines compares
//!   contents without reading them.
//!
//! The package lists are `rpm -qa --qf '%{NAME} %{EVR} %{ARCH}\n'` over each
//! tree's rpmdb. Not used to decide what changed (the file diff does that, and
//! the rpmdb itself is rewritten by every build, so it is always "changed"),
//! but to say which version a component moves between.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
}

/// One path that differs between the booted and staged trees.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Change {
    pub kind: ChangeKind,
    /// Absolute, as it appears in the running system (`/usr/bin/rimed`).
    pub path: String,
}

/// Parse `ostree diff` output: one `<A|M|D> <spaces> <path>` per line.
///
/// A line that is not in that shape is an error, not a skip: a planner that
/// silently drops what it cannot parse would leave the dropped file at its old
/// version beside new files that may depend on it.
pub fn parse_ostree_diff(text: &str) -> Result<Vec<Change>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (flag, rest) = line.split_at(1);
        let kind = match flag {
            "A" => ChangeKind::Added,
            "M" => ChangeKind::Modified,
            "D" => ChangeKind::Removed,
            _ => return Err(format!("ostree diff line {}: unrecognised: {line:?}", n + 1)),
        };
        let path = rest.trim_start();
        if !path.starts_with('/') || path.contains('\0') {
            return Err(format!("ostree diff line {}: not an absolute path: {line:?}", n + 1));
        }
        out.push(Change { kind, path: path.to_string() });
    }
    out.sort();
    Ok(out)
}

/// The fields of a composefs dump line that describe a file's identity.
///
/// Format (composefs-dump(5)): `PATH SIZE MODE NLINK UID GID RDEV MTIME
/// PAYLOAD CONTENT DIGEST [XATTR...]`. NLINK and MTIME are dropped: a link
/// count is a property of the rest of the tree, and ostree zeroes mtimes, so
/// neither says anything about whether this file's bytes changed.
fn identity(fields: &[&str]) -> String {
    let mut id = String::new();
    for (i, f) in fields.iter().enumerate().skip(1) {
        if i == 3 || i == 7 {
            continue;
        }
        id.push_str(f);
        id.push(' ');
    }
    id
}

/// Index one dump by path.
pub fn index_dump(text: &str) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::with_capacity(text.len() / 120);
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.len() < 11 || !fields[0].starts_with('/') {
            return Err(format!("composefs dump line {}: malformed", n + 1));
        }
        map.insert(unescape_dump_path(fields[0])?, identity(&fields));
    }
    Ok(map)
}

/// composefs-dump escapes bytes outside printable ASCII, the space and the
/// backslash as `\xHH`.
pub fn unescape_dump_path(p: &str) -> Result<String, String> {
    if !p.contains('\\') {
        return Ok(p.to_string());
    }
    let b = p.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if b.get(i + 1) == Some(&b'x') && i + 4 <= b.len() {
                let hex = std::str::from_utf8(&b[i + 2..i + 4]).map_err(|e| e.to_string())?;
                let v = u8::from_str_radix(hex, 16).map_err(|_| format!("bad escape in {p:?}"))?;
                out.push(v);
                i += 4;
                continue;
            }
            if b.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            return Err(format!("bad escape in {p:?}"));
        }
        out.push(b[i]);
        i += 1;
    }
    let s = String::from_utf8(out).map_err(|_| format!("non-UTF-8 path {p:?}"))?;
    if s.contains('\0') {
        return Err(format!("NUL in path {p:?}"));
    }
    Ok(s)
}

/// Compare two dumps (booted, staged).
pub fn diff_dumps(booted: &str, staged: &str) -> Result<Vec<Change>, String> {
    let a = index_dump(booted)?;
    let b = index_dump(staged)?;
    let mut out = Vec::new();
    for (path, id) in &b {
        match a.get(path) {
            None => out.push(Change { kind: ChangeKind::Added, path: path.clone() }),
            Some(old) if old != id => {
                out.push(Change { kind: ChangeKind::Modified, path: path.clone() })
            }
            Some(_) => {}
        }
    }
    for path in a.keys() {
        if !b.contains_key(path) {
            out.push(Change { kind: ChangeKind::Removed, path: path.clone() });
        }
    }
    out.sort();
    Ok(out)
}

/// One package's version on each side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    pub name: String,
    pub arch: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `NAME EVR ARCH` per line → name.arch → EVR.
pub fn parse_rpm_list(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(format!("rpm list line {}: expected NAME EVR ARCH: {line:?}", n + 1));
        }
        // gpg-pubkey entries have no arch and say nothing about the tree.
        m.insert(format!("{}.{}", parts[0], parts[2]), parts[1].to_string());
    }
    Ok(m)
}

pub fn diff_packages(
    booted: &BTreeMap<String, String>,
    staged: &BTreeMap<String, String>,
) -> Vec<PackageChange> {
    let split = |k: &str| -> (String, String) {
        match k.rsplit_once('.') {
            Some((n, a)) => (n.to_string(), a.to_string()),
            None => (k.to_string(), String::new()),
        }
    };
    let mut out = Vec::new();
    for (k, v) in staged {
        match booted.get(k) {
            Some(old) if old == v => {}
            old => {
                let (name, arch) = split(k);
                out.push(PackageChange { name, arch, from: old.cloned(), to: Some(v.clone()) });
            }
        }
    }
    for (k, v) in booted {
        if !staged.contains_key(k) {
            let (name, arch) = split(k);
            out.push(PackageChange { name, arch, from: Some(v.clone()), to: None });
        }
    }
    out.sort_by(|a, b| (&a.name, &a.arch).cmp(&(&b.name, &b.arch)));
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The real `ostree diff` between 2026.10.09 and 2026.10.10 on the L16.
    pub const DIFF_1009_1010: &str = "\
M    /usr/bin/rime
M    /usr/bin/rimed
M    /usr/lib/sysimage/libdnf5/transaction_history.sqlite
M    /usr/lib/sysimage/libdnf5/transaction_history.sqlite-shm
M    /usr/lib/sysimage/libdnf5/transaction_history.sqlite-wal
M    /usr/lib/systemd/system/rimed.service
M    /usr/libexec/rime-gaming-discord
M    /usr/libexec/__pycache__/rime-labwc-keybindscpython-315.pyc
M    /usr/libexec/__pycache__/rime-session-migratecpython-315.pyc
M    /usr/share/rime/release.json
M    /usr/share/rpm/rpmdb.sqlite
M    /var/cache/ldconfig/aux-cache
M    /var/log/dnf5.log
M    /var/log/dnf5.log.1
";

    #[test]
    fn parses_the_real_ostree_diff() {
        let c = parse_ostree_diff(DIFF_1009_1010).unwrap();
        assert_eq!(c.len(), 14);
        assert!(c.iter().all(|c| c.kind == ChangeKind::Modified));
        assert!(c.iter().any(|c| c.path == "/usr/bin/rimed"));
    }

    #[test]
    fn ostree_diff_refuses_what_it_cannot_read() {
        assert!(parse_ostree_diff("X /usr/bin/x\n").is_err());
        assert!(parse_ostree_diff("M relative/path\n").is_err());
        let c = parse_ostree_diff("A    /usr/new\nD    /usr/old\n").unwrap();
        assert_eq!(c[0], Change { kind: ChangeKind::Added, path: "/usr/new".into() });
        assert_eq!(c[1], Change { kind: ChangeKind::Removed, path: "/usr/old".into() });
    }

    #[test]
    fn dump_diff_compares_content_not_mtime_or_links() {
        let a = "/ 4096 40755 13 0 0 0 0.0 - - - security.selinux=x\n\
/usr/bin/rimed 5025424 100755 1 0 0 0 0.0 ab/cdef - - security.selinux=bin\n\
/usr/bin/same 10 100755 1 0 0 0 0.0 11/22 - - security.selinux=bin\n\
/usr/bin/gone 10 100755 1 0 0 0 0.0 33/44 - - security.selinux=bin\n";
        let b = "/ 4096 40755 14 0 0 0 9.0 - - - security.selinux=x\n\
/usr/bin/rimed 5025424 100755 1 0 0 0 0.0 ff/0000 - - security.selinux=bin\n\
/usr/bin/same 10 100755 2 0 0 0 7.0 11/22 - - security.selinux=bin\n\
/usr/bin/new\\x20file 3 100644 1 0 0 0 0.0 55/66 - - security.selinux=bin\n";
        let c = diff_dumps(a, b).unwrap();
        assert_eq!(
            c,
            vec![
                Change { kind: ChangeKind::Added, path: "/usr/bin/new file".into() },
                Change { kind: ChangeKind::Modified, path: "/usr/bin/rimed".into() },
                Change { kind: ChangeKind::Removed, path: "/usr/bin/gone".into() },
            ]
        );
    }

    #[test]
    fn a_label_change_is_a_change() {
        let a = "/usr/x 1 100644 1 0 0 0 0.0 aa/bb - - security.selinux=usr_t\n";
        let b = "/usr/x 1 100644 1 0 0 0 0.0 aa/bb - - security.selinux=bin_t\n";
        assert_eq!(diff_dumps(a, b).unwrap().len(), 1);
    }

    #[test]
    fn dump_escapes() {
        assert_eq!(unescape_dump_path("/a\\x20b").unwrap(), "/a b");
        assert_eq!(unescape_dump_path("/a\\\\b").unwrap(), "/a\\b");
        assert!(unescape_dump_path("/a\\x00").is_err());
        assert!(unescape_dump_path("/a\\q").is_err());
        assert!(index_dump("relative 1 2 3 4 5 6 7 8 9 10\n").is_err());
    }

    #[test]
    fn package_diff() {
        let a = parse_rpm_list("systemd 262-3.fc45 x86_64\nhyprland 0.56.2-2.fc45 x86_64\nold 1-1 noarch\n").unwrap();
        let b = parse_rpm_list("systemd 262-4.fc45 x86_64\nhyprland 0.56.2-2.fc45 x86_64\nnew 1-1 noarch\n").unwrap();
        let d = diff_packages(&a, &b);
        assert_eq!(d.len(), 3);
        let sd = d.iter().find(|p| p.name == "systemd").unwrap();
        assert_eq!(sd.from.as_deref(), Some("262-3.fc45"));
        assert_eq!(sd.to.as_deref(), Some("262-4.fc45"));
        assert!(d.iter().any(|p| p.name == "old" && p.to.is_none()));
        assert!(d.iter().any(|p| p.name == "new" && p.from.is_none()));
        assert!(parse_rpm_list("only two\n").is_err());
    }
}
