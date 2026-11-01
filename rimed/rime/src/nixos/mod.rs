//! What `rime` does differently on Rime on NixOS, selected at runtime by [`is_nixos`]; the bootc paths are untouched
//! when it says no.

pub mod pkg;

use std::fs;
use std::path::Path;

/// NixOS when `/etc/NIXOS` exists and `/run/current-system` points into the store (§8.1).
pub fn is_nixos() -> bool {
    Path::new("/etc/NIXOS").exists()
        && fs::read_link("/run/current-system").map(|p| p.starts_with("/nix/store")).unwrap_or(false)
}
