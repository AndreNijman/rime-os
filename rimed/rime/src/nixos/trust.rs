//! `rime trust --gate` on NixOS (§5.5): verify the release manifest of the system that is RUNNING, re-fetched by the
//! digest recorded when it was installed, and confirm that manifest names this toplevel. Exit 0 only when it does.

use rimed_core::nixrel::manifest;
use rimed_core::nixrel::NIX_RELEASE_MEDIA_TYPE;

use crate::trust::Roots;
use crate::verify::{self, Verdict};

use super::{resolve_link, Applied, Config};

pub fn gate(json_out: bool) -> i32 {
    let (ok, lines) = check();
    if json_out {
        println!("{}", serde_json::json!({"product": "nixos", "verified": ok, "detail": lines}));
    } else {
        for l in &lines {
            println!("{l}");
        }
    }
    i32::from(!ok)
}

fn check() -> (bool, Vec<String>) {
    let mut out = Vec::new();
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => return (false, vec![format!("rime: {e}")]),
    };
    let Some(running) = resolve_link("/run/current-system") else {
        return (false, vec!["rime: /run/current-system does not resolve".into()]);
    };
    let Some(rec) = Applied::load().by_toplevel(&running).cloned() else {
        return (false, vec![format!("rime: the running system ({running}) was not installed by rime update, so there is no signed release to check it against")]);
    };
    out.push(format!("release:   {}", rec.release));
    out.push(format!("manifest:  {}@{}", cfg.repository, rec.digest));
    let roots = Roots::from_env();
    match verify::verify_signature_as(&roots, &cfg.repository, &rec.digest, &cfg.expect()) {
        Verdict::Verified { signer, at } => out.push(format!("signature: verified, signed by {signer} ({at})")),
        Verdict::Absent(w) => return (false, [out, vec![format!("signature: none ({w})")]].concat()),
        Verdict::Failed(w) => return (false, [out, vec![format!("signature: does NOT verify: {w}")]].concat()),
        Verdict::CouldNotRun(w) => return (false, [out, vec![format!("signature: could not be checked: {w}")]].concat()),
    }
    let bytes = match verify::artifact_layer_at(&roots, &cfg.repository, &rec.digest, &rec.digest, NIX_RELEASE_MEDIA_TYPE) {
        Ok(b) => b,
        Err(e) => return (false, [out, vec![format!("manifest:  could not be read: {e}")]].concat()),
    };
    let m = match manifest::parse(&bytes) {
        Ok(m) => m,
        Err(r) => return (false, [out, vec![r.message()]].concat()),
    };
    match m.variant(&cfg.variant).map(|v| m.path(v.toplevel).to_string()) {
        Some(t) if t == running => {
            out.push(format!("system:    {running} is the signed {} system of {}", cfg.variant, m.release));
            (true, out)
        }
        Some(t) => (false, [out, vec![format!("system:    the signed release names {t}, and this machine runs {running}")]].concat()),
        None => (false, [out, vec![format!("system:    the signed release has no {} system", cfg.variant)]].concat()),
    }
}
