//! The NixOS release client's pure half (docs/nixos/updates-and-release.md, D6).
//!
//! Same division as the rest of this crate: `rime` fetches, runs `nix` and
//! measures the machine; this module decides. Nothing here reads a file, runs a
//! program or looks at the clock, so every rule is a table test.
//!
//! * [`manifest`]: the signed release manifest (§3): parse, validate, closure walk.
//! * [`hash`]: narHash spellings (SRI base64 and Nix base32) normalised to bytes.
//! * [`classify`]: store path → live-engine [`Component`](crate::live::Component),
//!   shared by the client and any publisher so there is one rule set.
//! * [`activation`]: the boot-or-switch rule (§9) and the dry-activate parser.
//! * [`gc`]: which system generations `rime gc` keeps (§11.3).
//! * [`release`]: release IDs (`YYYY.MM.DD[.N]`) and their order.

pub mod activation;
pub mod classify;
pub mod gc;
pub mod hash;
pub mod manifest;
pub mod release;

/// The tag the signed channel document is published under (§4).
pub const NIX_CHANNELS_TAG: &str = "nix-channels";
/// The release manifest's layer media type (§3.1).
pub const NIX_RELEASE_MEDIA_TYPE: &str = "application/vnd.rimeos.nix-release.v1+json";
/// The channel document's layer media type (§4).
pub const NIX_CHANNELS_MEDIA_TYPE: &str = "application/vnd.rimeos.nix-channels.v1+json";
/// The `kind` every release manifest carries.
pub const RELEASE_KIND: &str = "rime-nixos-release";
/// The manifest schema this client understands.
pub const MANIFEST_SCHEMA: u32 = 1;
/// The keyless identity that signs Nix release objects (§5.1). Byte-identical to the design document.
pub const NIX_SIGNER: &str =
    "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main";
/// The system profile every generation lives in.
pub const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";
