//! A phone's recent photos, listed and saved from this machine (contract
//! §2.13).
//!
//! A listing is held in memory only until the person who asked has their
//! answer, like a messages read (§2.11), with one exception: each photo's
//! thumbnail is written as a small JPEG under the runtime directory, so the
//! Shell can show a grid by path instead of carrying 2 MiB of base64 through a
//! JSON reply. Those files are the device's and the listing connection's: they
//! go when that connection ends, the device is revoked, the phone stops
//! offering photos, or this process starts.
//!
//! A saved photo is the one thing that lands on disk for good, and this
//! process never chooses where. The person's own `rime remote photos save`
//! opens the file and passes the descriptor; this process only fills it with
//! the bytes the phone uploads on a `photos_upload` channel.
//!
//! The phone is trusted with nothing: a reply counts only from the device and
//! connection it was asked on, every field is cleaned again, a thumbnail's
//! metadata segments are dropped again here whatever the app did, and a file
//! name is built from a whole number, never from the phone's text. No photo's
//! name is ever written to a log or an error.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::phone::Events;

/// The control lines a phone sends about photos.
pub const VERBS: [&str; 2] = ["photos_reply", "photos_fetch_reply"];

/// What `remote_hello` offers: this machine understands the verbs above and
/// the upload channel.
pub const FEATURES: [&str; 1] = ["phone_photos"];

/// The event that asks the phone for its newest photos.
pub const REQUEST_EVENT: &str = "photos_request";

/// The event that asks the phone to upload one original.
pub const FETCH_EVENT: &str = "photos_fetch";

/// The channel an original is uploaded on.
pub const UPLOAD: &str = "photos_upload";

/// The most photos one listing asks for, and what it asks for unnamed.
pub const MAX_LIMIT: usize = 60;
pub const DEFAULT_LIMIT: usize = 30;

/// The largest thumbnail accepted, decoded. The phone aims for 24 KiB.
pub const MAX_THUMB: usize = 32 * 1024;

/// The longest name kept, in characters, and the longest mime type.
pub const MAX_NAME: usize = 120;
pub const MAX_MIME: usize = 40;

/// The most reply lines one listing accepts, and the most bytes in all.
pub const MAX_CHUNKS: usize = 64;
pub const MAX_TOTAL_BYTES: usize = 5 * 512 * 1024;

/// How long a listing waits for the whole answer: one deadline. Sixty
/// thumbnails are sixty decodes on the phone.
pub const LIST_WAIT: Duration = Duration::from_secs(30);

/// How long a save waits for the phone to start the upload, or refuse.
pub const START_WAIT: Duration = Duration::from_secs(20);

/// Nothing arriving on an upload for this long ends it.
pub const STALL: Duration = Duration::from_secs(30);

/// The largest original saved.
pub const MAX_PHOTO_BYTES: u64 = 64 * 1024 * 1024;

/// The most thumbnail files kept for one device, pages included.
pub const MAX_FILES: usize = 600;

/// The reason words a phone may refuse with. Anything else reads as `failed`.
pub const PHONE_REFUSALS: [&str; 7] = ["off", "no_permission", "locked", "busy", "not_found", "too_large", "failed"];

/// The whole upload's budget: 300 s plus one second per 128 KiB, as §1.8.
/// `$XDG_RUNTIME_DIR/rime-remote/photos`: the root the thumbnails go under,
/// one folder per device.
pub fn dir_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("rime-remote").join("photos")
}

pub fn upload_budget(len: u64) -> Duration {
    Duration::from_secs(300 + len / (128 * 1024))
}

/// One photo, as the phone described it, with its thumbnail's bytes once
/// cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub id: u64,
    pub name: String,
    pub date_ms: u64,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub mime: String,
    pub thumb: Option<Vec<u8>>,
}

/// One photo, as the control socket gives it: the thumbnail is a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub id: u64,
    pub name: String,
    pub date_ms: u64,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
}

/// A complete listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub photos: Vec<Listed>,
    /// The phone shows only the photos the person picked.
    pub partial: bool,
}

/// What a finished save wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The phone's name for it, reduced as §1.8 reduces a name.
    pub name: String,
    pub len: u64,
}

/// Why a request got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No events channel is open for the phone.
    NotConnected,
    /// The phone's channel belongs to a companion (locked) session.
    Locked,
    /// Another request of the same kind for this phone is waiting.
    Busy,
    /// The phone's connection ended before the answer was complete.
    Disconnected,
    /// The device was revoked while the request waited.
    Revoked,
    /// No complete answer within the deadline.
    Timeout,
    /// The phone answered with something that is not an answer to this.
    BadReply(String),
    /// The phone refused, with one of [`PHONE_REFUSALS`].
    Phone(String),
    /// The upload stopped before its last byte.
    Upload(String),
    /// The file the caller passed could not be written.
    Write(String),
}

impl Refusal {
    /// The sentence the person reads. Never carries a photo's name.
    pub fn sentence(&self, name: &str) -> String {
        match self {
            Refusal::NotConnected => format!("{name} is not connected. Open Rime Remote on it, then try again"),
            Refusal::Locked => format!("{name} is locked: Rime Remote shows photos only while it is unlocked on the phone"),
            Refusal::Busy => format!("{name} is already answering another photos request; try again in a moment"),
            Refusal::Disconnected => format!("{name} disconnected before it finished"),
            Refusal::Revoked => format!("{name} was revoked"),
            Refusal::Timeout => format!("{name} did not answer in time"),
            Refusal::BadReply(why) => format!("{name} answered with something this computer does not accept: {why}"),
            Refusal::Upload(why) => format!("the photo from {name} did not arrive whole: {why}"),
            Refusal::Write(why) => format!("the photo from {name} could not be written: {why}"),
            Refusal::Phone(word) => match word.as_str() {
                "off" => format!("Photos on your computer is turned off on {name} (Rime Remote → Photos)"),
                "no_permission" => {
                    format!("Rime Remote on {name} is not allowed to see photos. Allow it in Rime Remote → Photos")
                }
                "locked" => format!("{name} is locked: unlock Rime Remote on it to see its photos"),
                "busy" => format!("{name} is busy with another photo; try again in a moment"),
                "not_found" => format!("{name} has no such photo, or does not share it with this computer"),
                "too_large" => format!("that photo is larger than {} MiB", MAX_PHOTO_BYTES / (1024 * 1024)),
                _ => format!("{name} could not read the photo"),
            },
        }
    }
}

/// A listing on its way in.
struct List {
    device: String,
    connection: u64,
    limit: usize,
    photos: Vec<Photo>,
    chunks: usize,
    bytes: usize,
    partial: bool,
    outcome: Option<Result<(Vec<Photo>, bool), Refusal>>,
}

impl List {
    fn finish(&mut self, outcome: Result<(Vec<Photo>, bool), Refusal>) {
        if self.outcome.is_none() {
            // What was gathered goes now, not when the waiter wakes.
            if outcome.is_err() {
                self.photos = Vec::new();
            }
            self.outcome = Some(outcome);
        }
    }
}

/// Where a save stands.
enum Stage {
    /// The fetch is written; no upload yet. Holds the caller's file.
    Waiting(File),
    /// An upload channel took it; its thread holds the file.
    Uploading,
}

/// A save on its way in.
struct Fetch {
    device: String,
    connection: u64,
    stage: Stage,
    outcome: Option<Result<Saved, Refusal>>,
}

impl Fetch {
    fn finish(&mut self, outcome: Result<Saved, Refusal>) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }
}

#[derive(Default)]
struct Book {
    lists: HashMap<u64, List>,
    fetches: HashMap<u64, Fetch>,
    /// Which connection's listing wrote each device's thumbnails.
    thumbs: HashMap<String, u64>,
}

/// The listings and saves waiting on phones, and the thumbnails of the last
/// listing of each.
#[derive(Default)]
pub struct Photos {
    next: AtomicU64,
    book: Mutex<Book>,
    wake: Condvar,
    /// `$XDG_RUNTIME_DIR/rime-remote/photos`. Only `main.rs` sets it, as for
    /// the focus file: unset, no thumbnail is written anywhere, so a unit test
    /// in a developer's session never writes beside the real Shell's.
    dir: Mutex<Option<PathBuf>>,
}

impl Photos {
    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Where thumbnails go; everything already there is removed, since no
    /// listing outlives the process that made it.
    pub fn set_dir(&self, dir: PathBuf) {
        let _ = std::fs::remove_dir_all(&dir);
        *self.dir.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir);
    }

    fn dir(&self) -> Option<PathBuf> {
        self.dir.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The folder a device's thumbnails are in, when a folder is set.
    pub fn device_dir(&self, device: &str) -> Option<PathBuf> {
        self.dir().map(|d| d.join(dir_name(device)))
    }

    /// Ask `device` for its newest photos and wait up to `wait` for the whole
    /// answer; write the thumbnails and give their paths.
    pub fn list(
        &self,
        events: &Events,
        device: &str,
        before: Option<u64>,
        limit: usize,
        wait: Duration,
    ) -> Result<Listing, Refusal> {
        let Some((connection, full)) = events.holder(device) else {
            return Err(Refusal::NotConnected);
        };
        if !full {
            // Refused here and at once: the phone does not offer photos on a
            // companion session, and may not answer on one.
            return Err(Refusal::Locked);
        }
        let limit = limit.clamp(1, MAX_LIMIT);
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut line = json!({"event": REQUEST_EVENT, "id": id, "limit": limit});
        if let Some(b) = before {
            line["before"] = json!(b);
        }
        {
            let mut b = self.book();
            if b.lists.values().any(|l| l.device == device) {
                return Err(Refusal::Busy);
            }
            b.lists.insert(
                id,
                List {
                    device: device.to_string(),
                    connection,
                    limit,
                    photos: Vec::new(),
                    chunks: 0,
                    bytes: 0,
                    partial: false,
                    outcome: None,
                },
            );
        }
        if !events.send(device, &line) {
            self.book().lists.remove(&id);
            return Err(Refusal::NotConnected);
        }
        let deadline = Instant::now() + wait;
        let mut b = self.book();
        let (photos, partial) = loop {
            if b.lists.get(&id).is_some_and(|l| l.outcome.is_some()) {
                let l = b.lists.remove(&id).expect("present");
                break l.outcome.expect("set")?;
            }
            if !b.lists.contains_key(&id) {
                return Err(Refusal::Revoked);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                b.lists.remove(&id);
                return Err(Refusal::Timeout);
            }
            b = self.wake.wait_timeout(b, left).unwrap_or_else(|p| p.into_inner()).0;
        };
        // Written under the book's lock, so `ended` and `forget` either run
        // first, and this sees the connection gone, or after, and remove what
        // this wrote. A listing never leaves files behind its connection.
        if events.holder(device).map(|(c, _)| c) != Some(connection) {
            return Err(Refusal::Disconnected);
        }
        let listed = self.write_thumbs(&mut b, device, connection, before.is_some(), photos);
        Ok(Listing { photos: listed, partial })
    }

    /// Write each photo's thumbnail (when a folder is set) and give the
    /// photos with paths. The caller holds the book.
    fn write_thumbs(&self, b: &mut Book, device: &str, connection: u64, page: bool, photos: Vec<Photo>) -> Vec<Listed> {
        let dir = self.device_dir(device);
        if let Some(dir) = &dir {
            let kept = std::fs::read_dir(dir).map(|r| r.count()).unwrap_or(0);
            // A first page replaces, and so does a page past the cap.
            if !page || kept + photos.len() > MAX_FILES || b.thumbs.get(device) != Some(&connection) {
                let _ = std::fs::remove_dir_all(dir);
            }
            if let Err(e) = make_private_dir(dir) {
                eprintln!("rime-remoted: could not make the thumbnail folder: {e}");
            }
            b.thumbs.insert(device.to_string(), connection);
        }
        photos
            .into_iter()
            .map(|p| {
                let thumb = match (&dir, &p.thumb) {
                    (Some(dir), Some(bytes)) => {
                        let path = dir.join(format!("{}.jpg", p.id));
                        match write_private(&path, bytes) {
                            Ok(()) => Some(path.to_string_lossy().into_owned()),
                            Err(e) => {
                                eprintln!("rime-remoted: could not write a thumbnail: {e}");
                                None
                            }
                        }
                    }
                    _ => None,
                };
                Listed {
                    id: p.id,
                    name: p.name,
                    date_ms: p.date_ms,
                    width: p.width,
                    height: p.height,
                    size: p.size,
                    mime: p.mime,
                    thumb,
                }
            })
            .collect()
    }

    /// Ask `device` to upload photo `photo` into `sink`, and wait for it.
    pub fn fetch(&self, events: &Events, device: &str, photo: u64, sink: File, start: Duration) -> Result<Saved, Refusal> {
        let Some((connection, full)) = events.holder(device) else {
            return Err(Refusal::NotConnected);
        };
        if !full {
            return Err(Refusal::Locked);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut b = self.book();
            if b.fetches.values().any(|f| f.device == device) {
                return Err(Refusal::Busy);
            }
            b.fetches.insert(
                id,
                Fetch {
                    device: device.to_string(),
                    connection,
                    stage: Stage::Waiting(sink),
                    outcome: None,
                },
            );
        }
        if !events.send(device, &json!({"event": FETCH_EVENT, "id": id, "photo": photo})) {
            self.book().fetches.remove(&id);
            return Err(Refusal::NotConnected);
        }
        // Until the upload begins, `start`; then its own deadlines, enforced
        // by its thread, with a margin here so a wedged thread cannot hold
        // the caller forever.
        let mut deadline = Instant::now() + start;
        let mut uploading = false;
        let mut b = self.book();
        loop {
            match b.fetches.get(&id) {
                None => return Err(Refusal::Revoked),
                Some(f) if f.outcome.is_some() => {
                    return b.fetches.remove(&id).expect("present").outcome.expect("set");
                }
                Some(f) if !uploading && matches!(f.stage, Stage::Uploading) => {
                    uploading = true;
                    deadline = Instant::now() + upload_budget(MAX_PHOTO_BYTES) + STALL;
                }
                Some(_) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Removed, so an upload that begins now is an unknown id, and
                // the caller's file is closed with the entry.
                b.fetches.remove(&id);
                return Err(Refusal::Timeout);
            }
            b = self.wake.wait_timeout(b, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Answer one of [`VERBS`] from `device` on `connection`. `None` for a
    /// line this module does not own.
    pub fn control(&self, device: &str, connection: u64, line: &[u8]) -> Option<Vec<u8>> {
        let v: Value = serde_json::from_slice(line).ok()?;
        let cmd = v.get("cmd")?.as_str()?;
        if !VERBS.contains(&cmd) {
            return None;
        }
        let Some(id) = v["id"].as_u64() else {
            return Some(error("bad_request", "id must be a whole number"));
        };
        let mut b = self.book();
        if cmd == "photos_fetch_reply" {
            let Some(f) = b
                .fetches
                .get_mut(&id)
                .filter(|f| f.device == device && f.connection == connection && f.outcome.is_none())
                .filter(|f| matches!(f.stage, Stage::Waiting(_)))
            else {
                return Some(error("unknown_id", "no photo request with that id is waiting"));
            };
            f.finish(Err(Refusal::Phone(refusal_word(&v["error"]))));
            // The caller's file is closed now; the CLI removes it.
            f.stage = Stage::Uploading;
            drop(b);
            self.wake.notify_all();
            return Some(ok());
        }
        let Some(l) = b
            .lists
            .get_mut(&id)
            .filter(|l| l.device == device && l.connection == connection && l.outcome.is_none())
        else {
            return Some(error("unknown_id", "no photo request with that id is waiting"));
        };
        let answer = take_reply(l, &v, line.len());
        let reply = match &answer {
            Err(Refusal::BadReply(why)) => error("bad_request", why),
            _ => ok(),
        };
        if let Some(outcome) = answer.transpose() {
            l.finish(outcome);
            drop(b);
            self.wake.notify_all();
        }
        Some(reply)
    }

    /// `connection` ended: a listing on it fails, a save written on it that
    /// has not begun uploading fails, and the thumbnails its listing wrote go.
    pub fn ended(&self, device: &str, connection: u64) {
        let mut b = self.book();
        for l in b.lists.values_mut() {
            if l.device == device && l.connection == connection {
                l.finish(Err(Refusal::Disconnected));
            }
        }
        for f in b.fetches.values_mut() {
            if f.device == device && f.connection == connection && matches!(f.stage, Stage::Waiting(_)) {
                f.finish(Err(Refusal::Disconnected));
            }
        }
        if b.thumbs.get(device) == Some(&connection) {
            b.thumbs.remove(device);
            self.remove_thumbs(device);
        }
        drop(b);
        self.wake.notify_all();
    }

    /// The phone stopped offering photos: its thumbnails go.
    pub fn off(&self, device: &str) {
        let mut b = self.book();
        if b.thumbs.remove(device).is_some() {
            self.remove_thumbs(device);
        }
    }

    /// Revoke: everything about the phone goes.
    pub fn forget(&self, device: &str) {
        let mut b = self.book();
        for l in b.lists.values_mut() {
            if l.device == device {
                l.finish(Err(Refusal::Revoked));
            }
        }
        for f in b.fetches.values_mut() {
            if f.device == device {
                f.finish(Err(Refusal::Revoked));
            }
        }
        b.thumbs.remove(device);
        self.remove_thumbs(device);
        drop(b);
        self.wake.notify_all();
    }

    fn remove_thumbs(&self, device: &str) {
        if let Some(dir) = self.device_dir(device) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Open a `photos_upload` channel from `device` (a session of scope
    /// `full`): the end the pump reads, and the reply. The bytes the phone
    /// sends are written into the waiting save's file by a thread of their
    /// own. `Err` is the reply that refuses it, sent before any byte.
    pub fn open_upload(self: &Arc<Self>, device: &str, full: bool, request: &[u8]) -> Result<(UnixStream, Vec<u8>), Vec<u8>> {
        let v: Value = serde_json::from_slice(request).map_err(|_| error("bad_request", "not a JSON object"))?;
        if !full {
            return Err(error("not_allowed", "photos are uploaded from an unlocked phone"));
        }
        let Some(id) = v["id"].as_u64() else {
            return Err(error("bad_request", "id must be a whole number"));
        };
        let len = v["len"].as_u64().unwrap_or(0);
        let mut b = self.book();
        let Some(f) = b
            .fetches
            .get_mut(&id)
            .filter(|f| f.device == device && f.outcome.is_none() && matches!(f.stage, Stage::Waiting(_)))
        else {
            return Err(error("unknown_id", "no photo request with that id is waiting"));
        };
        if len == 0 || len > MAX_PHOTO_BYTES {
            let why = format!("len must be from 1 to {MAX_PHOTO_BYTES} bytes");
            f.finish(Err(Refusal::Phone("too_large".into())));
            f.stage = Stage::Uploading;
            drop(b);
            self.wake.notify_all();
            return Err(error("too_large", &why));
        }
        let name = reduce_name(v["name"].as_str().unwrap_or(""));
        let (theirs, ours) = UnixStream::pair().map_err(|e| error("unavailable", &e.to_string()))?;
        let Stage::Waiting(sink) = std::mem::replace(&mut f.stage, Stage::Uploading) else {
            unreachable!("filtered above");
        };
        drop(b);
        self.wake.notify_all();
        let photos = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome = receive(ours, sink, len, STALL, upload_budget(len)).map(|()| Saved { name, len });
            let mut b = photos.book();
            if let Some(f) = b.fetches.get_mut(&id) {
                f.finish(outcome);
            }
            drop(b);
            photos.wake.notify_all();
        });
        Ok((theirs, json!({"reply": "uploading", "len": len}).to_string().into_bytes()))
    }

    /// How many photos are held for `device` right now. For the tests that
    /// prove nothing outlives a disconnect.
    #[cfg(test)]
    pub fn held(&self, device: &str) -> usize {
        self.book().lists.values().filter(|l| l.device == device).map(|l| l.photos.len()).sum()
    }
}

/// Read exactly `len` bytes from `channel` into `sink`, then say so on the
/// channel. Any stop before the last byte is an error, and the caller removes
/// what was written.
fn receive(mut channel: UnixStream, mut sink: File, len: u64, stall: Duration, budget: Duration) -> Result<(), Refusal> {
    let deadline = Instant::now() + budget;
    let said = |channel: &mut UnixStream, line: Value| {
        let _ = channel.write_all(format!("{line}\n").as_bytes());
    };
    let result = (|| {
        sink.rewind().map_err(|e| Refusal::Write(e.to_string()))?;
        sink.set_len(0).map_err(|e| Refusal::Write(e.to_string()))?;
        let mut got = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        while got < len {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Refusal::Upload("it took too long".into()));
            }
            channel
                .set_read_timeout(Some(stall.min(left)))
                .map_err(|e| Refusal::Upload(e.to_string()))?;
            let want = buf.len().min((len - got) as usize);
            let n = match channel.read(&mut buf[..want]) {
                Ok(0) => return Err(Refusal::Upload(format!("the phone stopped after {got} of {len} bytes"))),
                Ok(n) => n,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Err(Refusal::Upload("nothing arrived for too long".into()))
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Refusal::Upload(e.to_string())),
            };
            sink.write_all(&buf[..n]).map_err(|e| Refusal::Write(e.to_string()))?;
            got += n as u64;
        }
        sink.sync_all().map_err(|e| Refusal::Write(e.to_string()))?;
        Ok(())
    })();
    match &result {
        Ok(()) => said(&mut channel, json!({"reply": "uploaded", "len": len})),
        Err(r) => said(&mut channel, json!({"reply": "error", "code": "failed", "message": r.sentence("the phone")})),
    }
    let _ = channel.shutdown(std::net::Shutdown::Both);
    result
}

/// Whether an `Open` request is a photos upload.
pub fn is_upload(request_line: &[u8]) -> bool {
    serde_json::from_slice::<Value>(request_line)
        .ok()
        .and_then(|v| v["cmd"].as_str().map(|c| c == UPLOAD))
        .unwrap_or(false)
}

/// One `photos_reply` line into `l`. `Ok(None)` when more is coming.
fn take_reply(l: &mut List, v: &Value, len: usize) -> Result<Option<(Vec<Photo>, bool)>, Refusal> {
    if let Some(word) = v.get("error") {
        return Err(Refusal::Phone(refusal_word(word)));
    }
    l.chunks += 1;
    l.bytes += len;
    if l.chunks > MAX_CHUNKS || l.bytes > MAX_TOTAL_BYTES {
        return Err(bad("the answer is larger than this computer accepts"));
    }
    let list = v["photos"].as_array().ok_or_else(|| bad("photos must be a list"))?;
    for p in list {
        l.photos.push(Photo::parse(p)?);
    }
    if l.photos.len() > l.limit {
        return Err(bad("more photos than were asked for"));
    }
    if v["partial"].as_bool() == Some(true) {
        l.partial = true;
    }
    if v["more"].as_bool() == Some(true) {
        return Ok(None);
    }
    Ok(Some((std::mem::take(&mut l.photos), l.partial)))
}

impl Photo {
    fn parse(v: &Value) -> Result<Photo, Refusal> {
        let mime = v["mime"].as_str().ok_or_else(|| bad("mime must be text"))?;
        let mime_ok = mime.len() <= MAX_MIME
            && mime.starts_with("image/")
            && mime.len() > "image/".len()
            && mime.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/.+-".contains(&c));
        if !mime_ok {
            return Err(bad("mime must be an image type"));
        }
        let thumb = match &v["thumb"] {
            Value::Null => None,
            t => {
                let text = t.as_str().ok_or_else(|| bad("thumb must be text"))?;
                let bytes = rime_remote_core::b64_decode(text).ok_or_else(|| bad("thumb must be base64url"))?;
                if bytes.len() > MAX_THUMB {
                    return Err(bad("a thumbnail is at most 32 KiB"));
                }
                // Not a JPEG: the photo stays, without a picture.
                strip_jpeg(&bytes)
            }
        };
        Ok(Photo {
            id: whole(v, "id")?,
            name: reduce_name(v["name"].as_str().ok_or_else(|| bad("name must be text"))?),
            date_ms: whole(v, "date_ms")?,
            width: small(v, "width")?,
            height: small(v, "height")?,
            size: whole(v, "size")?,
            mime: mime.to_string(),
            thumb,
        })
    }
}

/// A JPEG with every APP1 to APP15 and COM segment taken out: no EXIF, no
/// location, no XMP, no comment. APP0 (JFIF) and the segments that make the
/// picture stay byte for byte. `None` for anything that is not a JPEG with a
/// scan in it.
pub fn strip_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    loop {
        if bytes.get(i) != Some(&0xFF) {
            return None;
        }
        while bytes.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *bytes.get(i)?;
        i += 1;
        match marker {
            // A picture ends before it had a scan: not one.
            0xD9 | 0xD8 | 0x00 => return None,
            0x01 | 0xD0..=0xD7 => out.extend([0xFF, marker]),
            _ => {
                let seg_len = u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]) as usize;
                if seg_len < 2 || i + seg_len > bytes.len() {
                    return None;
                }
                if marker == 0xDA {
                    // The scan and everything after it: entropy-coded data,
                    // which carries no segments of its own to strip.
                    out.extend([0xFF, 0xDA]);
                    out.extend_from_slice(&bytes[i..]);
                    return Some(out);
                }
                if !((0xE1..=0xEF).contains(&marker) || marker == 0xFE) {
                    out.extend([0xFF, marker]);
                    out.extend_from_slice(&bytes[i..i + seg_len]);
                }
                i += seg_len;
            }
        }
    }
}

/// A photo's name as the phone gave it, made a name and nothing else, as
/// §1.8 reduces one: up to the last `/` or `\` dropped, control and bidi
/// characters out, leading dots and spaces dropped, and cut to [`MAX_NAME`]
/// characters keeping the extension. Empty after that is `photo`.
pub fn reduce_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let clean = crate::messages::clean(base, usize::MAX, 0);
    let clean = clean.trim_start_matches(['.', ' ']).trim_end().to_string();
    if clean.is_empty() {
        return "photo".to_string();
    }
    if clean.chars().count() <= MAX_NAME {
        return clean;
    }
    let (stem, ext) = match clean.rfind('.') {
        Some(dot) if clean.len() - dot <= 10 && dot > 0 => (&clean[..dot], &clean[dot..]),
        _ => (clean.as_str(), ""),
    };
    let keep = MAX_NAME.saturating_sub(ext.chars().count());
    let mut out: String = stem.chars().take(keep).collect();
    out.push_str(ext);
    out
}

/// A device id as a folder name: itself when it is plain letters, digits,
/// `-` and `_`, otherwise its bytes in hex, so no id can name a path.
fn dir_name(device: &str) -> String {
    if !device.is_empty()
        && device.len() <= 128
        && device.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        device.to_string()
    } else {
        device.bytes().map(|b| format!("{b:02x}")).collect()
    }
}

/// `dir` and its parents up to it, each new one 0700; refused when `dir` is
/// not a directory of its own (a link put there).
fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other("the thumbnail folder is not a folder"));
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Write `bytes` to `path`, 0600, never following a link there.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)
}

fn refusal_word(v: &Value) -> String {
    let w = v.as_str().unwrap_or("");
    if PHONE_REFUSALS.contains(&w) {
        w.to_string()
    } else {
        "failed".to_string()
    }
}

fn bad(why: &str) -> Refusal {
    Refusal::BadReply(why.to_string())
}

fn whole(v: &Value, field: &str) -> Result<u64, Refusal> {
    v[field].as_u64().ok_or_else(|| bad(&format!("{field} must be a whole number")))
}

fn small(v: &Value, field: &str) -> Result<u32, Refusal> {
    v[field]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| bad(&format!("{field} must be a whole number")))
}

fn ok() -> Vec<u8> {
    json!({"reply": "ok"}).to_string().into_bytes()
}

fn error(code: &str, message: &str) -> Vec<u8> {
    json!({"reply": "error", "code": code, "message": message})
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;

    /// A JPEG's segments: SOI, JFIF, an EXIF block with a GPS position in it,
    /// a comment, the tables, a scan and EOI. The stripper walks segments, so
    /// the tables need not decode.
    fn jpeg_with_gps() -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        let seg = |j: &mut Vec<u8>, marker: u8, body: &[u8]| {
            j.extend([0xFF, marker]);
            j.extend(((body.len() + 2) as u16).to_be_bytes());
            j.extend_from_slice(body);
        };
        seg(&mut j, 0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
        let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08".to_vec();
        exif.extend_from_slice(b"\0\x01\x88\x25\0\x04\0\0\0\x01\0\0\0\x1a\0\0\0\0GPSLatitude-31.95S");
        seg(&mut j, 0xE1, &exif);
        seg(&mut j, 0xEE, b"Adobe\0more");
        seg(&mut j, 0xFE, b"taken at home");
        seg(&mut j, 0xDB, &[0u8; 65]);
        seg(&mut j, 0xC0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]);
        seg(&mut j, 0xC4, &[0, 0, 0, 0]);
        seg(&mut j, 0xDA, &[1, 1, 0, 0, 0x3f, 0]);
        j.extend([0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF, 0xD0, 0x78]);
        j.extend([0xFF, 0xD9]);
        j
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn a_thumbnail_loses_its_exif_its_location_and_its_comment_and_keeps_its_picture() {
        let j = jpeg_with_gps();
        assert!(contains(&j, b"GPSLatitude") && contains(&j, b"Exif"));
        let out = strip_jpeg(&j).expect("a JPEG");
        for gone in [&b"Exif"[..], b"GPSLatitude", b"taken at home", b"Adobe", &[0xFF, 0xE1], &[0xFF, 0xFE]] {
            assert!(!contains(&out, gone), "{gone:?} survived");
        }
        assert!(contains(&out, b"JFIF"), "APP0 went");
        // The picture's segments, and the scan to the end, byte for byte.
        let tables = j.windows(2).position(|w| w == [0xFF, 0xDB]).unwrap();
        let kept = out.windows(2).position(|w| w == [0xFF, 0xDB]).unwrap();
        assert_eq!(&out[kept..], &j[tables..]);
        // Stripping again changes nothing.
        assert_eq!(strip_jpeg(&out).unwrap(), out);
    }

    #[test]
    fn anything_but_a_jpeg_with_a_scan_is_no_thumbnail() {
        assert_eq!(strip_jpeg(b"\x89PNG\r\n\x1a\n...."), None);
        assert_eq!(strip_jpeg(&[0xFF, 0xD8, 0xFF, 0xD9]), None);
        assert_eq!(strip_jpeg(&[0xFF, 0xD8, 0xFF, 0xE1, 0x00]), None);
        assert_eq!(strip_jpeg(&[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x40, 1, 2]), None, "a segment past the end");
        assert_eq!(strip_jpeg(&[]), None);
    }

    #[test]
    fn a_name_is_a_name_and_nothing_else() {
        assert_eq!(reduce_name("PXL_1.jpg"), "PXL_1.jpg");
        assert_eq!(reduce_name("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(reduce_name("C:\\x\\..hidden.png"), "hidden.png");
        assert_eq!(reduce_name("a\u{202E}gpj.exe\u{0007}"), "agpj.exe");
        assert_eq!(reduce_name("  ..  "), "photo");
        let long = format!("{}.jpeg", "é".repeat(300));
        let r = reduce_name(&long);
        assert_eq!(r.chars().count(), MAX_NAME);
        assert!(r.ends_with(".jpeg"), "{r}");
    }

    fn thumb_text() -> String {
        rime_remote_core::b64_encode(&jpeg_with_gps())
    }

    fn photo_json(id: u64) -> Value {
        json!({"id": id, "name": format!("IMG_{id}.jpg"), "date_ms": 1000 - id, "width": 4000, "height": 3000,
               "size": 2_000_000 + id, "mime": "image/jpeg", "thumb": thumb_text()})
    }

    fn phone(events: &Events, device: &str, connection: u64, full: bool) -> BufReader<UnixStream> {
        let (theirs, _) = events.open_for(device, connection, full).expect("open");
        BufReader::new(theirs)
    }

    fn next_event(r: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        r.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("json")
    }

    fn fresh(tag: &str) -> (Arc<Photos>, Arc<Events>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rime-photos-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = Arc::new(Photos::default());
        // set_dir removes what is there: this process starts clean.
        std::fs::create_dir_all(dir.join("photos/stale")).unwrap();
        p.set_dir(dir.join("photos"));
        assert!(!dir.join("photos/stale").exists(), "a start left an old listing");
        (p, Arc::new(Events::default()), dir)
    }

    fn listing(
        p: &Arc<Photos>,
        events: &Arc<Events>,
        before: Option<u64>,
        limit: usize,
        wait: Duration,
    ) -> std::thread::JoinHandle<Result<Listing, Refusal>> {
        let (p, events) = (Arc::clone(p), Arc::clone(events));
        std::thread::spawn(move || p.list(&events, "dev-a", before, limit, wait))
    }

    #[test]
    fn a_chunked_listing_writes_private_thumbnails_without_metadata_and_a_disconnect_removes_them() {
        let (p, events, dir) = fresh("list");
        let mut r = phone(&events, "dev-a", 7, true);
        let h = listing(&p, &events, None, 3, LIST_WAIT);
        let ev = next_event(&mut r);
        assert_eq!((ev["event"].as_str(), ev["limit"].as_u64()), (Some(REQUEST_EVENT), Some(3)), "{ev}");
        assert!(ev.get("before").is_none(), "{ev}");
        let id = ev["id"].as_u64().unwrap();
        let first = json!({"cmd": "photos_reply", "id": id, "photos": [photo_json(1), photo_json(2)], "more": true});
        assert_eq!(p.control("dev-a", 7, first.to_string().as_bytes()).unwrap(), ok());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!h.is_finished(), "a chunk with more:true finished the listing");
        let mut no_thumb = photo_json(3);
        no_thumb["thumb"] = json!(rime_remote_core::b64_encode(b"GIF89a not a jpeg"));
        let last = json!({"cmd": "photos_reply", "id": id, "photos": [no_thumb], "partial": true});
        p.control("dev-a", 7, last.to_string().as_bytes()).unwrap();
        let got = h.join().unwrap().expect("listing");
        assert!(got.partial);
        assert_eq!(got.photos.iter().map(|p| p.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(got.photos[2].thumb, None, "a thumbnail that is not a JPEG was kept");
        let folder = dir.join("photos/dev-a");
        assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        for listed in &got.photos[..2] {
            let path = PathBuf::from(listed.thumb.as_ref().expect("a path"));
            assert_eq!(path, folder.join(format!("{}.jpg", listed.id)));
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            let bytes = std::fs::read(&path).unwrap();
            assert!(!contains(&bytes, b"Exif") && !contains(&bytes, b"GPSLatitude"), "metadata reached the disk");
            assert_eq!(&bytes[..2], [0xFF, 0xD8]);
        }
        assert_eq!(p.held("dev-a"), 0);

        // Another connection of the phone ending leaves them; the listing's
        // own connection ending takes them.
        p.ended("dev-a", 8);
        assert!(folder.exists());
        p.ended("dev-a", 7);
        assert!(!folder.exists(), "thumbnails outlived the connection that asked for them");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_first_page_replaces_the_files_and_a_later_page_adds_to_them() {
        let (p, events, dir) = fresh("pages");
        let mut r = phone(&events, "dev-a", 7, true);
        let answer = |r: &mut BufReader<UnixStream>, ids: &[u64]| {
            let id = next_event(r)["id"].as_u64().unwrap();
            let photos: Vec<Value> = ids.iter().map(|i| photo_json(*i)).collect();
            p.control("dev-a", 7, json!({"cmd": "photos_reply", "id": id, "photos": photos}).to_string().as_bytes());
        };
        let files = || {
            let mut v: Vec<String> = std::fs::read_dir(dir.join("photos/dev-a"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let h = listing(&p, &events, None, 2, LIST_WAIT);
        answer(&mut r, &[1, 2]);
        h.join().unwrap().unwrap();
        let h = listing(&p, &events, Some(998), 2, LIST_WAIT);
        let ev = next_event(&mut r);
        assert_eq!(ev["before"], 998);
        let id = ev["id"].as_u64().unwrap();
        p.control("dev-a", 7, json!({"cmd": "photos_reply", "id": id, "photos": [photo_json(3)]}).to_string().as_bytes());
        h.join().unwrap().unwrap();
        assert_eq!(files(), ["1.jpg", "2.jpg", "3.jpg"]);
        let h = listing(&p, &events, None, 2, LIST_WAIT);
        answer(&mut r, &[4]);
        h.join().unwrap().unwrap();
        assert_eq!(files(), ["4.jpg"], "a first page kept an older listing's files");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn revoke_fails_the_listing_and_removes_the_thumbnails_and_so_does_switching_off() {
        let (p, events, dir) = fresh("revoke");
        let mut r = phone(&events, "dev-a", 7, true);
        let h = listing(&p, &events, None, 2, LIST_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        p.control("dev-a", 7, json!({"cmd": "photos_reply", "id": id, "photos": [photo_json(1)]}).to_string().as_bytes());
        h.join().unwrap().unwrap();
        assert!(dir.join("photos/dev-a/1.jpg").exists());
        p.off("dev-a");
        assert!(!dir.join("photos/dev-a").exists(), "switching photos off left the thumbnails");

        let h = listing(&p, &events, None, 2, LIST_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        p.control("dev-a", 7, json!({"cmd": "photos_reply", "id": id, "photos": [photo_json(1)]}).to_string().as_bytes());
        h.join().unwrap().unwrap();
        let h = listing(&p, &events, None, 2, LIST_WAIT);
        next_event(&mut r);
        p.forget("dev-a");
        assert_eq!(h.join().unwrap(), Err(Refusal::Revoked));
        assert!(!dir.join("photos/dev-a").exists(), "revoke left the thumbnails");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_disconnect_fails_the_listing_at_once_and_drops_what_came_in() {
        let (p, events, dir) = fresh("drop");
        let mut r = phone(&events, "dev-a", 7, true);
        let h = listing(&p, &events, None, 5, LIST_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let first = json!({"cmd": "photos_reply", "id": id, "photos": [photo_json(1)], "more": true});
        p.control("dev-a", 7, first.to_string().as_bytes());
        assert_eq!(p.held("dev-a"), 1);
        let started = Instant::now();
        p.ended("dev-a", 7);
        assert_eq!(p.held("dev-a"), 0);
        assert_eq!(h.join().unwrap(), Err(Refusal::Disconnected));
        assert!(started.elapsed() < Duration::from_secs(2));
        let late = json!({"cmd": "photos_reply", "id": id, "photos": []});
        assert!(String::from_utf8(p.control("dev-a", 7, late.to_string().as_bytes()).unwrap()).unwrap().contains("unknown_id"));
        assert!(!dir.join("photos/dev-a").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reply_counts_only_from_the_device_and_connection_it_was_asked_on() {
        let (p, events, dir) = fresh("who");
        let mut r = phone(&events, "dev-a", 7, true);
        let h = listing(&p, &events, None, 5, Duration::from_millis(400));
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let line = json!({"cmd": "photos_reply", "id": id, "photos": []}).to_string();
        for (dev, conn) in [("dev-b", 7), ("dev-a", 8)] {
            let out = String::from_utf8(p.control(dev, conn, line.as_bytes()).unwrap()).unwrap();
            assert!(out.contains("unknown_id"), "{dev}/{conn}: {out}");
        }
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_companion_session_is_asked_nothing_and_one_request_of_each_kind_waits() {
        let (p, events, dir) = fresh("companion");
        assert_eq!(p.list(&events, "dev-a", None, 5, LIST_WAIT), Err(Refusal::NotConnected));
        let (theirs, _) = events.open_for("dev-a", 7, false).expect("open");
        theirs.set_nonblocking(true).unwrap();
        assert_eq!(p.list(&events, "dev-a", None, 5, LIST_WAIT), Err(Refusal::Locked));
        let sink = tempfile(&dir, "c");
        assert_eq!(p.fetch(&events, "dev-a", 1, sink, START_WAIT), Err(Refusal::Locked));
        let mut buf = [0u8; 16];
        assert!(std::io::Read::read(&mut &theirs, &mut buf).is_err(), "something was written to a locked phone");

        let mut r = phone(&events, "dev-a", 8, true);
        let h = listing(&p, &events, None, 5000, Duration::from_millis(500));
        assert_eq!(next_event(&mut r)["limit"], MAX_LIMIT, "the limit was not clamped");
        assert_eq!(p.list(&events, "dev-a", None, 5, LIST_WAIT), Err(Refusal::Busy));
        assert_eq!(h.join().unwrap(), Err(Refusal::Timeout));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_phone_is_held_to_the_limit_the_size_and_the_rules() {
        let (p, events, dir) = fresh("rules");
        let mut r = phone(&events, "dev-a", 7, true);
        let mut bad_replies = vec![json!({"photos": [photo_json(1), photo_json(2), photo_json(3)]})];
        let mut video = photo_json(1);
        video["mime"] = json!("video/mp4");
        let mut huge = photo_json(1);
        huge["thumb"] = json!(rime_remote_core::b64_encode(&vec![0xFF; MAX_THUMB + 1]));
        let mut nameless = photo_json(1);
        nameless["name"] = json!(5);
        bad_replies.extend([json!({"photos": [video]}), json!({"photos": [huge]}), json!({"photos": [nameless]})]);
        for body in bad_replies {
            let h = listing(&p, &events, None, 2, LIST_WAIT);
            let id = next_event(&mut r)["id"].as_u64().unwrap();
            let mut line = body.clone();
            line["cmd"] = json!("photos_reply");
            line["id"] = json!(id);
            let out = String::from_utf8(p.control("dev-a", 7, line.to_string().as_bytes()).unwrap()).unwrap();
            assert!(out.contains("bad_request"), "{body}: {out}");
            assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))), "{body}");
        }
        // Too many lines.
        let h = listing(&p, &events, None, 60, LIST_WAIT);
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let chunk = json!({"cmd": "photos_reply", "id": id, "photos": [], "more": true}).to_string();
        for _ in 0..MAX_CHUNKS {
            p.control("dev-a", 7, chunk.as_bytes());
        }
        assert!(!h.is_finished());
        p.control("dev-a", 7, chunk.as_bytes());
        assert!(matches!(h.join().unwrap(), Err(Refusal::BadReply(_))));
        // A refusal word, and one this build does not know.
        for (word, read) in [("off", "off"), ("something-new", "failed")] {
            let h = listing(&p, &events, None, 2, LIST_WAIT);
            let id = next_event(&mut r)["id"].as_u64().unwrap();
            p.control("dev-a", 7, json!({"cmd": "photos_reply", "id": id, "error": word}).to_string().as_bytes());
            assert_eq!(h.join().unwrap(), Err(Refusal::Phone(read.into())));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempfile(dir: &Path, name: &str) -> File {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(name))
            .unwrap()
    }

    fn saving(p: &Arc<Photos>, events: &Arc<Events>, photo: u64, sink: File) -> std::thread::JoinHandle<Result<Saved, Refusal>> {
        let (p, events) = (Arc::clone(p), Arc::clone(events));
        std::thread::spawn(move || p.fetch(&events, "dev-a", photo, sink, Duration::from_secs(5)))
    }

    #[test]
    fn a_save_fills_the_callers_file_with_exactly_the_bytes_uploaded_once() {
        let (p, events, dir) = fresh("save");
        let mut r = phone(&events, "dev-a", 7, true);
        let h = saving(&p, &events, 42, tempfile(&dir, "out"));
        let ev = next_event(&mut r);
        assert_eq!((ev["event"].as_str(), ev["photo"].as_u64()), (Some(FETCH_EVENT), Some(42)), "{ev}");
        let id = ev["id"].as_u64().unwrap();
        let body: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let req = json!({"cmd": UPLOAD, "id": id, "name": "../PXL_1.jpg", "len": body.len()}).to_string();
        // Another device may not take it, and a companion session may not.
        let refused = String::from_utf8(p.open_upload("dev-b", true, req.as_bytes()).unwrap_err()).unwrap();
        assert!(refused.contains("unknown_id"), "{refused}");
        assert!(p.open_upload("dev-a", false, req.as_bytes()).is_err());
        // On another connection of the same phone: yes, once.
        let (mut ch, reply) = p.open_upload("dev-a", true, req.as_bytes()).expect("taken");
        assert_eq!(serde_json::from_slice::<Value>(&reply).unwrap(), json!({"reply": "uploading", "len": body.len()}));
        assert!(p.open_upload("dev-a", true, req.as_bytes()).is_err(), "an upload was taken twice");
        for part in body.chunks(32 * 1024) {
            ch.write_all(part).unwrap();
        }
        let mut said = String::new();
        BufReader::new(&ch).read_line(&mut said).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&said).unwrap()["reply"], "uploaded");
        let saved = h.join().unwrap().expect("saved");
        assert_eq!(saved, Saved { name: "PXL_1.jpg".into(), len: body.len() as u64 });
        assert_eq!(std::fs::read(dir.join("out")).unwrap(), body);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_is_refused_before_a_byte_when_too_large_and_fails_when_the_phone_stops_or_says_no() {
        let (p, events, dir) = fresh("save-no");
        let mut r = phone(&events, "dev-a", 7, true);
        // Too large: refused before `uploading`.
        let h = saving(&p, &events, 1, tempfile(&dir, "a"));
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let req = json!({"cmd": UPLOAD, "id": id, "name": "x.jpg", "len": MAX_PHOTO_BYTES + 1}).to_string();
        let out = String::from_utf8(p.open_upload("dev-a", true, req.as_bytes()).unwrap_err()).unwrap();
        assert!(out.contains("too_large"), "{out}");
        assert_eq!(h.join().unwrap(), Err(Refusal::Phone("too_large".into())));

        // The phone refuses.
        let h = saving(&p, &events, 2, tempfile(&dir, "b"));
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let line = json!({"cmd": "photos_fetch_reply", "id": id, "error": "not_found"}).to_string();
        assert!(String::from_utf8(p.control("dev-a", 8, line.as_bytes()).unwrap()).unwrap().contains("unknown_id"));
        assert_eq!(p.control("dev-a", 7, line.as_bytes()).unwrap(), ok());
        assert_eq!(h.join().unwrap(), Err(Refusal::Phone("not_found".into())));

        // The upload stops half-way.
        let h = saving(&p, &events, 3, tempfile(&dir, "c"));
        let id = next_event(&mut r)["id"].as_u64().unwrap();
        let req = json!({"cmd": UPLOAD, "id": id, "name": "x.jpg", "len": 1000}).to_string();
        let (mut ch, _) = p.open_upload("dev-a", true, req.as_bytes()).unwrap();
        ch.write_all(&[1u8; 400]).unwrap();
        ch.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(matches!(h.join().unwrap(), Err(Refusal::Upload(_))));

        // The connection it was asked on ends before an upload began.
        let h = saving(&p, &events, 4, tempfile(&dir, "d"));
        next_event(&mut r);
        p.ended("dev-a", 7);
        assert_eq!(h.join().unwrap(), Err(Refusal::Disconnected));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_upload_that_goes_quiet_ends_on_its_own() {
        let dir = std::env::temp_dir().join(format!("rime-photos-quiet-{}", std::process::id()));
        let (theirs, ours) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let got = receive(ours, tempfile(&dir, "q"), 10, Duration::from_millis(200), Duration::from_secs(5));
        assert!(matches!(got, Err(Refusal::Upload(_))), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(theirs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_sentence_carries_what_the_phone_sent() {
        for r in [Refusal::NotConnected, Refusal::Locked, Refusal::Busy, Refusal::Phone("off".into())] {
            assert!(r.sentence("Pixel").contains("Pixel"));
        }
        assert_eq!(dir_name("abc-_1"), "abc-_1");
        assert_eq!(dir_name("../x"), "2e2e2f78");
    }
}
