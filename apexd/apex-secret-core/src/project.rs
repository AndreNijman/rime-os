//! Reading a project's own files, from a daemon that is root.
//!
//! P1-001 gave a provider four things to supply and fixed everything else. One
//! thing it did not give it is a way to read the project. git did not need one:
//! a git remote's meaning lives in `.git/config`, and git reads that itself, in
//! a child that has already dropped to the owner. Cloudflare has no such
//! program. §13.1 says the *project* binds the account, the zone and the
//! environment, so the provider has to open a file in a directory the caller
//! controls, while running as root, before it has any credential in hand.
//!
//! That is a different problem from anything in P0-002, and it is the reason
//! this module is here rather than in the Cloudflare provider: the next
//! provider that binds an AWS account or a Kubernetes context has it too. There
//! is nothing Cloudflare in this file, for the same reason there is nothing git
//! in [`crate::operation`].
//!
//! ## What "safely" has to mean when the reader is root
//!
//! The path is assembled from a project root the caller named and a relative
//! path the caller named. Root following a symlink there reads whatever the
//! caller pointed it at, so:
//!
//! * **every component below the project root is opened with `O_NOFOLLOW`**,
//!   one `openat` at a time. Guarding only the last component is not enough —
//!   `deploy/` can be a symlink as easily as `deploy/worker.js` can;
//! * **the file must be a regular file owned by the account the operation runs
//!   as.** A root-owned file under a user's project did not get there by the
//!   user writing it, and reading one is how `/etc/shadow` ends up in a request
//!   body. This is also what makes a symlinked *project root* harmless: it
//!   lands on somebody else's files, and somebody else's files are refused;
//! * **the size is capped before the read**, so a project cannot make the
//!   daemon allocate a filesystem's worth of anything;
//! * **a parse failure is reported as a position and never as a snippet.**
//!   `toml`'s own error message quotes the offending line, and that message
//!   would travel into the reply and into the audit trail, which is exactly
//!   where a file's contents must not appear when the daemon can be pointed at
//!   files it should not read.
//!
//! ## What it still does not protect against
//!
//! The owner can change the file between one read and the next, and the owner
//! is the caller. This is a guardrail, not a boundary: it keeps a project from
//! *accidentally* deploying to the wrong account, and it keeps root from
//! reading a file the user could not read. It does not stop a user from
//! rebinding their own project, any more than P0-002 stops one from editing
//! `.git/config`. What bounds a hostile caller is the grant, the host pin, and
//! the scope of the credential itself.

use std::collections::BTreeMap;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};

/// The file a project's identity bindings live in.
///
/// §36 ("Per-project identity") describes one file holding `[identity.*]` for
/// every provider, so the name is deliberately not provider-specific. P2-013
/// generalises the rest of it.
pub const PROJECT_FILE: &str = "apex.toml";

/// Largest project file this will read, in bytes.
pub const MAX_PROJECT_FILE: u64 = 64 * 1024;

/// Millionths of a unit of money, which is how [`ProjectConfig::money`] keeps
/// an amount once it has been read.
///
/// §13.14's budgets are written as decimals in the file and must not stay
/// floats afterwards: a running total in `f64` drifts, and a budget that drifts
/// refuses one operation early or lets one too many through. Millionths rather
/// than hundredths because Workers AI is priced per neuron, in fractions of a
/// cent, and a cap in cents could not tell any two AI operations apart.
pub const MICROS: u64 = 1_000_000;

/// Largest payload file a provider may read out of a project, in bytes.
///
/// A Worker bundle; Cloudflare's own limit for a script upload is 10 MB on the
/// paid plans, and reading more than the far side would accept is pointless.
pub const MAX_PAYLOAD: u64 = 10 * 1024 * 1024;

/// Why a project file could not be read, or could not be trusted.
///
/// Every variant renders into something the person who has to fix it can act
/// on, and none of them renders any of the file's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectError {
    /// The relative path is not one this will follow at all.
    BadPath(String),
    /// Nothing there.
    Absent { path: PathBuf },
    /// A component of the path is a symlink, and root does not follow those
    /// into a directory the caller controls.
    Symlink { path: PathBuf },
    /// There, but not a regular file.
    NotAFile { path: PathBuf },
    /// A component that has to be a directory is not one.
    NotADirectory { path: PathBuf },
    /// There, but owned by somebody else.
    NotOwned { path: PathBuf, owner: String },
    /// Bigger than the cap.
    TooBig { path: PathBuf, limit: u64 },
    /// The open or the read failed for a reason of its own.
    Unreadable { path: PathBuf, reason: String },
    /// Not valid TOML. Position only — never the text at that position.
    Malformed {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    /// Valid TOML, but a value is not the shape the key requires.
    BadValue { path: PathBuf, key: String, want: String },
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::BadPath(p) => write!(
                f,
                "'{}' is not a path inside a project. It must be relative, with \
                 no '..' and no leading '/'",
                p.escape_debug()
            ),
            ProjectError::Absent { path } => {
                write!(f, "{} does not exist", path.display())
            }
            ProjectError::Symlink { path } => write!(
                f,
                "{} is reached through a symbolic link. This service runs as \
                 root and will not follow one into a directory you control",
                path.display()
            ),
            ProjectError::NotAFile { path } => {
                write!(f, "{} is not a regular file", path.display())
            }
            ProjectError::NotADirectory { path } => {
                write!(f, "{} is not a directory", path.display())
            }
            ProjectError::NotOwned { path, owner } => write!(
                f,
                "{} is not owned by {owner}. This service reads a project's \
                 files as the account the operation runs as, so a file that \
                 account could not have written is refused",
                path.display()
            ),
            ProjectError::TooBig { path, limit } => {
                write!(f, "{} is larger than the {limit} byte limit", path.display())
            }
            ProjectError::Unreadable { path, reason } => {
                write!(f, "{} could not be read: {reason}", path.display())
            }
            ProjectError::Malformed { path, line, column } => write!(
                f,
                "{} is not valid TOML, at line {line} column {column}. The text \
                 there is deliberately not quoted here: this message reaches \
                 the audit trail",
                path.display()
            ),
            ProjectError::BadValue { path, key, want } => write!(
                f,
                "{} sets {key} to something that is not {want}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ProjectError {}

/// A file descriptor that closes itself.
///
/// `File::from_raw_fd` would do, but a directory fd opened `O_DIRECTORY` is not
/// a file anybody should be able to read from by accident.
struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // Safe: this owns the descriptor and runs once.
            unsafe { libc::close(self.0) };
        }
    }
}

fn open_root(root: &Path) -> Result<Fd, ProjectError> {
    let Ok(c) = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()) else {
        return Err(ProjectError::BadPath(root.display().to_string()));
    };
    // Safe: `c` is a NUL-terminated path that outlives the call.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(from_errno(root, std::io::Error::last_os_error()));
    }
    Ok(Fd(fd))
}

/// Open one component below the project root, never following a link.
///
/// **`O_PATH` for a directory, and not `O_DIRECTORY`.** Two measured facts,
/// neither of them what the obvious reading of `open(2)` suggests:
///
/// * `O_DIRECTORY | O_NOFOLLOW` on a symlink that points at a directory fails
///   with `ENOTDIR`, not `ELOOP` — and `ENOTDIR` is also what a real
///   non-directory in the middle of a path gives, so the errno cannot tell the
///   two apart. Reporting it as absent describes a symlink that is right there
///   as a missing file; reporting it as a symlink describes a typo as an
///   attack. Both are the same defect this codebase already has a name for:
///   a refusal reported as an absence.
/// * `O_PATH | O_NOFOLLOW` does not fail on a symlink at all. It hands back a
///   descriptor **to the link itself**, which is better than an errno: `fstat`
///   on it says `S_IFLNK` exactly, with no guessing, and says `S_IFDIR` for the
///   directory that is supposed to be there.
///
/// So the walk opens intermediate components `O_PATH` and classifies them by
/// `st_mode`. `openat` accepts an `O_PATH` descriptor as its directory, which
/// is what makes this work at all.
///
/// The last component is opened for reading, where `O_NOFOLLOW` does fail with
/// `ELOOP`, plus `O_NONBLOCK` so that a FIFO does not park the daemon's
/// connection thread inside `open`. What it is gets decided by `fstat` after.
fn open_at(dir: &Fd, name: &str, path_so_far: &Path, last: bool) -> Result<Fd, ProjectError> {
    let Ok(c) = std::ffi::CString::new(name) else {
        return Err(ProjectError::BadPath(name.to_string()));
    };
    let flags = if last {
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
    } else {
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
    };
    // Safe: `dir` owns a live descriptor and `c` outlives the call.
    let fd = unsafe { libc::openat(dir.0, c.as_ptr(), flags) };
    if fd < 0 {
        return Err(from_errno(path_so_far, std::io::Error::last_os_error()));
    }
    let fd = Fd(fd);
    if !last {
        match kind(&fd, path_so_far)? & libc::S_IFMT {
            libc::S_IFDIR => {}
            libc::S_IFLNK => {
                return Err(ProjectError::Symlink {
                    path: path_so_far.to_path_buf(),
                })
            }
            _ => {
                return Err(ProjectError::NotADirectory {
                    path: path_so_far.to_path_buf(),
                })
            }
        }
    }
    Ok(fd)
}

/// `st_mode` behind an open descriptor.
fn kind(fd: &Fd, path: &Path) -> Result<libc::mode_t, ProjectError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // Safe: `fd` is live and `st` is owned here. `fstat` works on an `O_PATH`
    // descriptor, which is why the walk can use one.
    if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
        return Err(from_errno(path, std::io::Error::last_os_error()));
    }
    Ok(st.st_mode)
}

/// The one place an errno becomes a [`ProjectError`].
///
/// `ELOOP` is the interesting one: with `O_NOFOLLOW` it means the component
/// *is* a symlink, not that a chain was too long, and saying "does not exist"
/// for it would send somebody looking for a missing file that is right there.
/// `ENOTDIR` no longer reaches here for a symlink — see [`open_at`] — so it can
/// mean the only other thing it means.
fn from_errno(path: &Path, e: std::io::Error) -> ProjectError {
    match e.raw_os_error() {
        Some(libc::ELOOP) => ProjectError::Symlink {
            path: path.to_path_buf(),
        },
        Some(libc::ENOENT) => ProjectError::Absent {
            path: path.to_path_buf(),
        },
        Some(libc::ENOTDIR) => ProjectError::NotADirectory {
            path: path.to_path_buf(),
        },
        _ => ProjectError::Unreadable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        },
    }
}

/// Whether a relative path is one this module will walk.
///
/// Deliberately narrower than the filesystem allows and the same shape
/// [`crate::operation::valid_path`] accepts, so a `Syntax::Path` parameter that
/// passed the framework's check passes this one too.
pub fn walkable(relative: &str) -> bool {
    crate::operation::valid_path(relative)
}

/// Read a file out of a project, as the account the operation runs as.
///
/// `relative` is checked with [`walkable`] and then walked one component at a
/// time with `O_NOFOLLOW`. See the module note for what each check is for.
pub fn read_file(
    root: &Path,
    relative: &str,
    owner_uid: u32,
    owner_name: &str,
    limit: u64,
) -> Result<Vec<u8>, ProjectError> {
    if !walkable(relative) {
        return Err(ProjectError::BadPath(relative.to_string()));
    }
    let full = root.join(relative);

    let mut dir = open_root(root)?;
    let components: Vec<&str> = relative.split('/').collect();
    let mut walked = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        walked.push(component);
        let last = index + 1 == components.len();
        let next = open_at(&dir, component, &walked, last)?;
        if last {
            return read_open(next, &full, owner_uid, owner_name, limit);
        }
        dir = next;
    }
    // `relative` is non-empty, so the loop always returns.
    Err(ProjectError::BadPath(relative.to_string()))
}

/// Check what is actually behind an open descriptor, then read it.
///
/// `fstat` and not `stat`: the descriptor is already open, so nothing can be
/// swapped between the check and the read. That is the whole reason the walk
/// ends with a descriptor rather than a path.
fn read_open(
    fd: Fd,
    path: &Path,
    owner_uid: u32,
    owner_name: &str,
    limit: u64,
) -> Result<Vec<u8>, ProjectError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // Safe: `fd` is live and `st` is owned here.
    if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
        return Err(from_errno(path, std::io::Error::last_os_error()));
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        // A FIFO or a device node reaches here rather than hanging the thread,
        // because the last component was opened `O_NONBLOCK`.
        return Err(ProjectError::NotAFile {
            path: path.to_path_buf(),
        });
    }
    if st.st_uid != owner_uid {
        return Err(ProjectError::NotOwned {
            path: path.to_path_buf(),
            owner: owner_name.to_string(),
        });
    }
    if st.st_size as u64 > limit {
        return Err(ProjectError::TooBig {
            path: path.to_path_buf(),
            limit,
        });
    }

    let raw = fd.0;
    std::mem::forget(fd);
    // Safe: the descriptor was just released by `forget`, so `File` is its only
    // owner from here and closes it exactly once.
    let file = unsafe { std::fs::File::from_raw_fd(raw) };
    let mut bytes = Vec::with_capacity(st.st_size.max(0) as usize);
    use std::io::Read;
    // `take`, because `st_size` is a snapshot: a file being appended to while
    // this reads must not get past the cap it was measured against.
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ProjectError::Unreadable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    if bytes.len() as u64 > limit {
        return Err(ProjectError::TooBig {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(bytes)
}

/// A project's `apex.toml`, parsed.
///
/// Provider-neutral: it knows about tables, strings and lists of strings, and
/// nothing about what any of them mean. §13.1's account, zone and environment
/// are read out of one of these by the Cloudflare provider, and P2-013's
/// `[identity.github]` and `[identity.ssh]` will be read out of the same one.
#[derive(Debug, Clone)]
pub struct ProjectConfig {
    path: PathBuf,
    table: toml::Table,
}

impl ProjectConfig {
    /// Read and parse `<root>/apex.toml`.
    pub fn read(root: &Path, owner_uid: u32, owner_name: &str) -> Result<ProjectConfig, ProjectError> {
        let bytes = read_file(root, PROJECT_FILE, owner_uid, owner_name, MAX_PROJECT_FILE)?;
        let path = root.join(PROJECT_FILE);
        let text = String::from_utf8(bytes).map_err(|_| ProjectError::Malformed {
            path: path.clone(),
            line: 0,
            column: 0,
        })?;
        ProjectConfig::parse(&path, &text)
    }

    /// Parse text that is already in hand.
    ///
    /// Separate from [`ProjectConfig::read`] so the parser can be tested
    /// without a filesystem, and so the position-only error rule has one
    /// implementation rather than two.
    pub fn parse(path: &Path, text: &str) -> Result<ProjectConfig, ProjectError> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            // Position, never `e`'s own Display: that quotes the offending
            // line, and this message ends up in the audit trail.
            let (line, column) = e
                .span()
                .map(|s| position(text, s.start))
                .unwrap_or((0, 0));
            ProjectError::Malformed {
                path: path.to_path_buf(),
                line,
                column,
            }
        })?;
        Ok(ProjectConfig {
            path: path.to_path_buf(),
            table,
        })
    }

    /// Where it came from. Every refusal that mentions a binding names this, so
    /// a person is told which file to edit.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn at(&self, keys: &[&str]) -> Option<&toml::Value> {
        let (last, parents) = keys.split_last()?;
        let mut table = &self.table;
        for key in parents {
            table = table.get(*key)?.as_table()?;
        }
        table.get(*last)
    }

    /// A string at a dotted key, or nothing.
    ///
    /// A key that is present but is not a string is an error rather than a
    /// miss: `account_id = 12345` is somebody trying to configure this and
    /// getting it wrong, and reporting it as absent would send them looking for
    /// a line that is right in front of them.
    pub fn string(&self, keys: &[&str]) -> Result<Option<&str>, ProjectError> {
        match self.at(keys) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(self.bad(keys, "a string in quotes")),
        }
    }

    /// A `true`/`false` at a dotted key, or nothing.
    ///
    /// **Only a TOML boolean.** `unattended = "true"` is refused rather than
    /// read as true, and that is the whole point of the method existing: this
    /// is the reader P1-014's environment protection is switched off with, so
    /// the failure that matters is a file the owner believes says one thing
    /// while the daemon reads another. A string that happens to spell `true`
    /// is somebody configuring this and getting it wrong; reporting it as
    /// absent would silently leave production protected while the file says it
    /// is not, and — far worse in the other direction — a lenient reader that
    /// accepted `"false"` as a boolean would have to decide what `"no"` means.
    pub fn boolean(&self, keys: &[&str]) -> Result<Option<bool>, ProjectError> {
        match self.at(keys) {
            None => Ok(None),
            Some(toml::Value::Boolean(b)) => Ok(Some(*b)),
            Some(_) => Err(self.bad(keys, "true or false, unquoted")),
        }
    }

    /// A non-negative whole number at a dotted key, or nothing.
    ///
    /// TOML integers only, and never a float that happens to be whole: §13.14's
    /// budgets put counts and money in the same table, and a reader that
    /// accepted `3.0` as a count would accept `3.5` as one too and have to
    /// decide what it meant. A negative is refused here rather than clamped to
    /// zero — a cap of minus one is somebody getting it wrong, and zero is a
    /// cap that refuses everything.
    pub fn count(&self, keys: &[&str]) -> Result<Option<u64>, ProjectError> {
        match self.at(keys) {
            None => Ok(None),
            Some(toml::Value::Integer(n)) if *n >= 0 => Ok(Some(*n as u64)),
            Some(_) => Err(self.bad(keys, "a whole number that is not negative")),
        }
    }

    /// An amount of money at a dotted key, in millionths of a unit.
    ///
    /// §13.14 writes budgets as `cloudflare_daily = 5.00`, so the file carries
    /// a TOML float — and a float is exactly what a running total of money must
    /// not be kept in. So it is converted once, here, to an integer number of
    /// millionths and never touched as a float again: a thousand additions of
    /// `0.011` in `f64` does not equal `11.0`, and a budget that drifts is a
    /// budget that stops one operation early or lets one too many through.
    ///
    /// A whole number is accepted too — `cloudflare_daily = 5` is what somebody
    /// writes when the amount is round, and refusing it would be pedantry with
    /// a refusal attached.
    ///
    /// Millionths and not hundredths because Workers AI is priced per neuron,
    /// in fractions of a cent. A cap expressed in cents could not distinguish
    /// any two AI operations.
    pub fn money(&self, keys: &[&str]) -> Result<Option<u64>, ProjectError> {
        let want = "an amount of money, like 5.00, that is not negative";
        let raw = match self.at(keys) {
            None => return Ok(None),
            Some(toml::Value::Float(f)) => *f,
            Some(toml::Value::Integer(n)) if *n >= 0 => *n as f64,
            Some(_) => return Err(self.bad(keys, want)),
        };
        // Rejected rather than saturated. An amount this build cannot represent
        // is somebody who meant something, and the largest number it can hold
        // is not it.
        if !raw.is_finite() || raw < 0.0 || raw > (u64::MAX / MICROS) as f64 {
            return Err(self.bad(keys, want));
        }
        Ok(Some((raw * MICROS as f64).round() as u64))
    }

    /// A list of strings at a dotted key, or nothing.
    pub fn strings(&self, keys: &[&str]) -> Result<Vec<String>, ProjectError> {
        match self.at(keys) {
            None => Ok(Vec::new()),
            Some(toml::Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let toml::Value::String(s) = item else {
                        return Err(self.bad(keys, "a list of strings in quotes"));
                    };
                    out.push(s.clone());
                }
                Ok(out)
            }
            Some(_) => Err(self.bad(keys, "a list of strings in quotes")),
        }
    }

    /// A table of `name = "value"` at a dotted key, or nothing.
    ///
    /// §13.1 binds an account and a zone, and the two of them fit in scalar
    /// keys. P1-006's resources do not: a project has several D1 databases and
    /// several KV namespaces, each with a name its own code uses and an id the
    /// REST API is addressed by, and neither a list of strings nor a pair of
    /// scalars can carry that. `[cloudflare.kv]` with one line per namespace
    /// can.
    ///
    /// Ordered, so a refusal that lists what is bound reads the same way
    /// twice. A sub-table is skipped rather than refused — `[cloudflare]`
    /// holds both `zone = "…"` and `[cloudflare.production]`, so a table
    /// containing a table is ordinary — but a key whose value is a number or a
    /// list is an error, for the reason [`ProjectConfig::string`] gives: it is
    /// somebody configuring this and getting it wrong, and calling it absent
    /// sends them looking for a line that is right in front of them.
    pub fn pairs(&self, keys: &[&str]) -> Result<BTreeMap<String, String>, ProjectError> {
        let Some(toml::Value::Table(table)) = self.at(keys) else {
            return Ok(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        for (name, value) in table {
            match value {
                toml::Value::String(s) => {
                    out.insert(name.clone(), s.clone());
                }
                toml::Value::Table(_) => {}
                _ => {
                    let mut key = keys.to_vec();
                    key.push(name);
                    return Err(self.bad(&key, "a string in quotes"));
                }
            }
        }
        Ok(out)
    }

    /// The names of the keys of a table that are NOT sub-tables.
    ///
    /// The counterpart to [`ProjectConfig::sections`], and it exists because
    /// §13.14's budget is the first table here whose key *names* are open —
    /// `cloudflare_daily`, `github_daily`, and whatever a provider written
    /// later calls itself. Everything before it read keys it already knew the
    /// names of. A reader that could only ask about known names could not tell
    /// an unknown budget key from an absent one, and §13.14's whole argument is
    /// that an unknown cap has to be refused rather than ignored.
    ///
    /// Sorted, so a refusal names the same key twice running.
    pub fn scalar_keys(&self, keys: &[&str]) -> Vec<String> {
        let Some(toml::Value::Table(t)) = self.at(keys) else {
            return Vec::new();
        };
        let mut names: Vec<String> = t
            .iter()
            .filter(|(_, v)| !v.is_table())
            .map(|(k, _)| k.clone())
            .collect();
        names.sort();
        names
    }

    /// The names of the sub-tables of a table — `preview` and `production` for
    /// §13.1's `[cloudflare.preview]` and `[cloudflare.production]`.
    ///
    /// Sorted, so a refusal that lists them reads the same way twice.
    pub fn sections(&self, keys: &[&str]) -> Vec<String> {
        let Some(toml::Value::Table(t)) = self.at(keys) else {
            return Vec::new();
        };
        let mut names: Vec<String> = t
            .iter()
            .filter(|(_, v)| v.is_table())
            .map(|(k, _)| k.clone())
            .collect();
        names.sort();
        names
    }

    fn bad(&self, keys: &[&str], want: &str) -> ProjectError {
        ProjectError::BadValue {
            path: self.path.clone(),
            key: keys.join("."),
            want: want.to_string(),
        }
    }
}

/// One-based line and column of a byte offset.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map(str::len).unwrap_or(0) + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::symlink;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn scratch(name: &str) -> Dir {
        let path = std::env::temp_dir().join(format!(
            "apex-project-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).expect("scratch");
        Dir(path)
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(text.as_bytes()).expect("write");
    }

    fn me() -> (u32, String) {
        // Safe: getuid cannot fail.
        (unsafe { libc::getuid() }, "the test account".to_string())
    }

    #[test]
    fn a_project_file_the_owner_wrote_is_read() {
        let dir = scratch("plain");
        write(&dir.0, "apex.toml", "[identity.cloudflare]\naccount = \"acme\"\n");
        let (uid, name) = me();
        let cfg = ProjectConfig::read(&dir.0, uid, &name).expect("read");
        assert_eq!(
            cfg.string(&["identity", "cloudflare", "account"]).unwrap(),
            Some("acme")
        );
        assert_eq!(cfg.path(), dir.0.join("apex.toml"));
    }

    #[test]
    fn a_symlinked_project_file_is_refused_rather_than_followed() {
        // The one this module exists for. Root reading `apex.toml` in a
        // directory the caller controls must not end up reading whatever the
        // caller pointed it at.
        let dir = scratch("symlink");
        write(&dir.0, "real.toml", "[cloudflare]\nzone = \"example.com\"\n");
        symlink("real.toml", dir.0.join("apex.toml")).expect("symlink");
        let (uid, name) = me();
        assert!(matches!(
            ProjectConfig::read(&dir.0, uid, &name),
            Err(ProjectError::Symlink { .. })
        ));
    }

    #[test]
    fn a_symlinked_directory_on_the_way_is_refused_too() {
        // Guarding only the last component is not enough, and this is the test
        // that says so: `deploy` is the symlink, `deploy/worker.js` is not.
        let dir = scratch("symlinkdir");
        write(&dir.0, "elsewhere/worker.js", "export default {};\n");
        symlink("elsewhere", dir.0.join("deploy")).expect("symlink");
        let (uid, name) = me();
        assert!(matches!(
            read_file(&dir.0, "deploy/worker.js", uid, &name, MAX_PAYLOAD),
            Err(ProjectError::Symlink { .. })
        ));
        // ...and the same file by its real path is fine, so the refusal is
        // about the link and not about the file.
        assert!(read_file(&dir.0, "elsewhere/worker.js", uid, &name, MAX_PAYLOAD).is_ok());
    }

    #[test]
    fn a_non_directory_in_the_middle_of_a_path_is_told_apart_from_a_link() {
        // The distinction `O_DIRECTORY | O_NOFOLLOW` cannot make: both a
        // symlinked directory and a plain file where a directory belongs give
        // ENOTDIR. Here they are three different answers, each of which sends
        // the reader somewhere useful.
        let dir = scratch("classify");
        write(&dir.0, "afile", "not a directory\n");
        write(&dir.0, "real/worker.js", "export default {};\n");
        symlink("real", dir.0.join("link")).expect("symlink");
        let (uid, name) = me();
        assert!(matches!(
            read_file(&dir.0, "afile/worker.js", uid, &name, MAX_PAYLOAD),
            Err(ProjectError::NotADirectory { .. })
        ));
        assert!(matches!(
            read_file(&dir.0, "link/worker.js", uid, &name, MAX_PAYLOAD),
            Err(ProjectError::Symlink { .. })
        ));
        assert!(matches!(
            read_file(&dir.0, "gone/worker.js", uid, &name, MAX_PAYLOAD),
            Err(ProjectError::Absent { .. })
        ));
    }

    #[test]
    fn a_file_the_owner_does_not_own_is_refused() {
        // What makes a symlinked project ROOT harmless: it lands on somebody
        // else's files. Root owns /etc/hostname on every machine this runs on.
        let (uid, name) = me();
        assert!(
            uid != 0,
            "this test needs a non-root uid to be meaningful; it is asserting \
             that a root-owned file is refused"
        );
        let err = read_file(Path::new("/etc"), "hostname", uid, &name, MAX_PROJECT_FILE);
        assert!(matches!(err, Err(ProjectError::NotOwned { .. })), "{err:?}");
    }

    #[test]
    fn a_path_that_climbs_out_of_the_project_is_refused_before_anything_opens() {
        let dir = scratch("climb");
        let (uid, name) = me();
        for evil in [
            "../etc/passwd",
            "/etc/passwd",
            "a/../../etc/passwd",
            "",
            "a//b",
            "a/",
        ] {
            assert!(
                matches!(
                    read_file(&dir.0, evil, uid, &name, MAX_PAYLOAD),
                    Err(ProjectError::BadPath(_))
                ),
                "'{evil}' was walked"
            );
        }
    }

    #[test]
    fn a_directory_where_a_file_belongs_is_not_read_as_one() {
        let dir = scratch("isdir");
        std::fs::create_dir_all(dir.0.join("apex.toml")).expect("dir");
        let (uid, name) = me();
        assert!(matches!(
            ProjectConfig::read(&dir.0, uid, &name),
            Err(ProjectError::NotAFile { .. })
        ));
    }

    #[test]
    fn a_file_over_the_cap_is_refused_without_being_read() {
        let dir = scratch("big");
        write(&dir.0, "big.js", &"x".repeat(2048));
        let (uid, name) = me();
        assert!(matches!(
            read_file(&dir.0, "big.js", uid, &name, 1024),
            Err(ProjectError::TooBig { limit: 1024, .. })
        ));
    }

    #[test]
    fn a_missing_file_says_so_and_names_itself() {
        let dir = scratch("absent");
        let (uid, name) = me();
        let err = ProjectConfig::read(&dir.0, uid, &name).unwrap_err();
        assert!(matches!(err, ProjectError::Absent { .. }));
        assert!(err.to_string().contains("apex.toml"), "{err}");
    }

    #[test]
    fn a_parse_failure_gives_a_position_and_never_the_line_it_failed_on() {
        // This message reaches the reply and the audit trail. `toml`'s own
        // Display quotes the offending line, which would put the contents of a
        // file the daemon can be pointed at into a log an administrator greps.
        let secret = "hunter2-should-never-appear-in-a-message";
        let text = format!("[cloudflare]\nzone = \"example.com\"\nbroken {secret}\n");
        let err = ProjectConfig::parse(Path::new("/p/apex.toml"), &text).unwrap_err();
        let ProjectError::Malformed { line, column, .. } = err else {
            panic!("expected a parse failure, got {err:?}");
        };
        assert_eq!(line, 3);
        assert!(column >= 1);
        let rendered = ProjectError::Malformed {
            path: PathBuf::from("/p/apex.toml"),
            line,
            column,
        }
        .to_string();
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(rendered.contains("line 3"), "{rendered}");
    }

    #[test]
    fn a_key_of_the_wrong_type_is_an_error_and_not_a_miss() {
        let cfg = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[identity.cloudflare]\naccount_id = 12345\n",
        )
        .expect("parses");
        let err = cfg
            .string(&["identity", "cloudflare", "account_id"])
            .unwrap_err();
        assert!(err.to_string().contains("account_id"), "{err}");
        // ...and a key that really is absent is still absent.
        assert_eq!(cfg.string(&["identity", "cloudflare", "nothing"]).unwrap(), None);
    }

    #[test]
    fn sub_tables_are_listed_in_a_stable_order() {
        // A refusal that lists a project's environments must read the same way
        // every time, or two runs of the same command disagree.
        let cfg = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[cloudflare]\nzone = \"example.com\"\n\
             [cloudflare.production]\nworker = \"p\"\n\
             [cloudflare.preview]\nworker = \"pv\"\n",
        )
        .expect("parses");
        assert_eq!(cfg.sections(&["cloudflare"]), vec!["preview", "production"]);
        // `zone` is a string, not a table, so it is not an environment.
        assert_eq!(cfg.string(&["cloudflare", "zone"]).unwrap(), Some("example.com"));
    }

    #[test]
    fn a_list_of_strings_is_read_and_a_list_of_anything_else_is_refused() {
        let cfg = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[cloudflare]\nbuckets = [\"a\", \"b\"]\nbad = [1, 2]\n",
        )
        .expect("parses");
        assert_eq!(cfg.strings(&["cloudflare", "buckets"]).unwrap(), vec!["a", "b"]);
        assert!(cfg.strings(&["cloudflare", "bad"]).is_err());
        assert!(cfg.strings(&["cloudflare", "absent"]).unwrap().is_empty());
    }

    #[test]
    fn a_table_of_names_is_read_as_pairs_and_a_sub_table_in_it_is_not_one() {
        // What P1-006's resources need: a name the project's own code uses and
        // an id the API is addressed by, several times over.
        let cfg = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[cloudflare]\nzone = \"example.com\"\n\
             [cloudflare.kv]\ncache = \"00112233445566778899aabbccddeeff\"\n\
             sessions = \"ffeeddccbbaa99887766554433221100\"\n\
             [cloudflare.production]\nworker = \"project\"\n",
        )
        .expect("parses");
        let kv = cfg.pairs(&["cloudflare", "kv"]).expect("pairs");
        assert_eq!(kv.len(), 2);
        assert_eq!(kv["cache"], "00112233445566778899aabbccddeeff");
        // `[cloudflare]` holds a scalar and two sub-tables. The scalar is a
        // pair; the sub-tables are skipped rather than refused, or every
        // §13.1 file would fail to read.
        let cloudflare = cfg.pairs(&["cloudflare"]).expect("pairs");
        assert_eq!(cloudflare.keys().collect::<Vec<_>>(), vec!["zone"]);
        // Absent is empty, not an error: a project that binds no namespace is
        // an ordinary project.
        assert!(cfg.pairs(&["cloudflare", "d1"]).expect("absent").is_empty());

        // ...and a value that is not a string is somebody getting it wrong.
        let wrong = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[cloudflare.kv]\ncache = 12345\n",
        )
        .expect("parses");
        let err = wrong.pairs(&["cloudflare", "kv"]).unwrap_err();
        assert!(err.to_string().contains("cloudflare.kv.cache"), "{err}");
    }

    #[test]
    fn a_boolean_is_only_a_boolean_and_a_quoted_one_is_refused() {
        // §13.8's opt-out is read with this, and the failure that matters is a
        // file whose owner believes it says one thing while the daemon reads
        // another. `unattended = "true"` is somebody switching production
        // protection off; reading it as absent would leave the environment
        // protected while the file says it is not, and the person would go
        // looking for the reason in the wrong place.
        let cfg = ProjectConfig::parse(
            Path::new("/p/apex.toml"),
            "[cloudflare.production]\nunattended = true\n\n\
             [cloudflare.staging]\nunattended = false\n\n\
             [cloudflare.preview]\nworker = \"w\"\n",
        )
        .expect("parses");
        assert_eq!(cfg.boolean(&["cloudflare", "production", "unattended"]), Ok(Some(true)));
        assert_eq!(cfg.boolean(&["cloudflare", "staging", "unattended"]), Ok(Some(false)));
        // Absent is `None` and not `Some(false)`: one is a default the owner
        // may not know about, the other is a sentence they wrote.
        assert_eq!(cfg.boolean(&["cloudflare", "preview", "unattended"]), Ok(None));

        for quoted in [
            "[cloudflare.production]\nunattended = \"true\"\n",
            "[cloudflare.production]\nunattended = 1\n",
            "[cloudflare.production]\nunattended = [\"true\"]\n",
        ] {
            let cfg = ProjectConfig::parse(Path::new("/p/apex.toml"), quoted).expect("parses");
            let err = cfg
                .boolean(&["cloudflare", "production", "unattended"])
                .expect_err(quoted);
            assert!(err.to_string().contains("true or false"), "{err}");
            assert!(
                err.to_string().contains("cloudflare.production.unattended"),
                "the message has to name the key somebody has to fix: {err}"
            );
        }
    }
}
