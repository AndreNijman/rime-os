//! The one installer image this build will install from, fixed at compile
//! time.
//!
//! The Windows program and the installer ISO are released together: the
//! workflow that publishes an ISO builds this program with that ISO's URL,
//! size and SHA-256 baked in (RIME_ISO_URL, RIME_ISO_BYTES, RIME_ISO_SHA256).
//! A file that does not hash to the pinned value is never read for anything
//! but its hash, whether it came from the network or from the user's own
//! Downloads folder. That is what makes downloading over the network safe:
//! the network is not trusted, the hash is, and the hash arrived with the
//! program the user chose to run.
//!
//! A build without the three variables has no image and refuses to install.

pub const ISO_URL: &str = match option_env!("RIME_ISO_URL") { Some(v) => v, None => "" };
pub const ISO_SHA256: &str = match option_env!("RIME_ISO_SHA256") { Some(v) => v, None => "" };
const ISO_BYTES_TEXT: &str = match option_env!("RIME_ISO_BYTES") { Some(v) => v, None => "" };
pub const ISO_FILE_NAME: &str = "rime-os-netinstall-x86_64.iso";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The compiled-in pin, or why there is none.
pub fn pin() -> Result<Pin, String> {
    let bytes: u64 = ISO_BYTES_TEXT.parse().map_err(|_| "this build has no installer image pinned (RIME_ISO_BYTES)".to_string())?;
    let ok_hash = ISO_SHA256.len() == 64 && ISO_SHA256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if !ok_hash {
        return Err("this build has no installer image pinned (RIME_ISO_SHA256)".to_string());
    }
    if !ISO_URL.starts_with("https://") {
        return Err("this build has no installer image pinned (RIME_ISO_URL)".to_string());
    }
    Ok(Pin { url: ISO_URL.to_string(), sha256: ISO_SHA256.to_string(), bytes })
}
