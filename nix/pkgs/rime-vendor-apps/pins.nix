# The vendor apps' pins (D5): ONE version of each, the bytes fixed by sha256, the vendor's own signed metadata kept
# beside this file. The vendor-key check (rime-pkg's vendor_fetch_*, via nix/pkgs/rime-vendor.nix) runs in the build
# against exactly these files, so a pin that does not verify against the key Rime pins fails the build, never a
# machine.
#
# Why this shape (decided 2026-11-02, nix/apps):
#
#   - The per-version download URLs persist (checked: OpenAI's chatgpt-26.1023.50603-1.x86_64.rpm still answers 200
#     after 26.1027.51404 replaced it in the index; Anthropic's apt pool lists 63 versions). Only the INDEXES move:
#     OpenAI's repomd/primary name one build, Anthropic's InRelease/Packages are re-signed every release. So the
#     package is a fixed-output fetch of its own URL, and the moving index is pinned by keeping the copy that named it.
#   - chatgpt: the rpm carries OpenAI's signature (rpmkeys checks it against the key the rpm ships, whose fingerprint
#     rime-pkg pins). OpenAI's repository metadata is not signed, so the copy here (repomd.xml + its primary index)
#     only tells the verifier which file to take; the build checks that the copy's primary is the one repomd names
#     and that it names the pinned rpm and sha256.
#   - claude-desktop: the deb itself is not signed; Anthropic signs InRelease, which names Packages by sha256, which
#     names the deb by sha256. InRelease is kept here (it is the signed root and its URL moves), and so is the
#     Packages it names (68 KB of text): its by-hash URL answers today, but an apt archive drops old by-hash entries
#     after a few releases, and a machine that builds the app itself (see Redistribution below) must still find it.
#     The build checks the kept Packages' sha256 against packagesSha256 before the verifier reads it, and the
#     verifier checks it again against the signed InRelease (decided 2026-11-02 by the nix/apps verification). The
#     deb is fetched by its pool path, which Anthropic keeps (63 versions listed). gpgv does not enforce Valid-Until, so the kept InRelease
#     keeps verifying. The verifier takes the NEWEST claude-desktop in Packages (`sort -V | tail -1`), so the pin is
#     always the newest version the kept Packages lists.
#
# Bumping a pin (a pull request, never automatic; the release then builds and caches the new closure):
#   chatgpt         curl $REPO/repodata/repomd.xml > chatgpt/repomd.xml; fetch the primary it names into chatgpt/
#                   (keep the hash-prefixed name, remove the old one); take <location href> and the sha256 from it.
#   claude-desktop  curl $REPO/dists/stable/InRelease > claude-desktop/InRelease; packagesSha256 = its
#                   main/binary-amd64/Packages SHA256 line; curl $REPO/dists/stable/main/binary-amd64/Packages >
#                   claude-desktop/Packages (its sha256 must be packagesSha256); file/sha256 = the newest
#                   claude-desktop stanza of that Packages.
#   then: nix build path:.#rime-chatgpt path:.#rime-claude-desktop (the verifier and the package checks run in it)
#   and checks.vendor-apps. A version the nixpkgs-derived packaging no longer fits fails there, not on a machine.
#
# Redistribution (plan risk R14, docs/nixos/updates-and-release.md 6.3): the release builds both apps like any class 2
# app, so Rime's cache would serve OpenAI's and Anthropic's binaries, as bootc's image did from 2026-09-11 to
# 2026-10-04. Neither licence is verified. If the upload allowlist (nix/redistributable.nix, not written yet) leaves
# them out, nothing here changes: the vendor files are fixed-output fetches, so a machine that cannot substitute an
# app builds it itself from the vendor's own URLs, and the vendor-key check runs there in the same build.
{
  chatgpt = {
    version = "26.1027.51404";
    # rime-pkg's CHATGPT_REPO default.
    repo = "https://persistent.oaistatic.com/codex-app-prod/linux/rpm/x86_64";
    file = "chatgpt-26.1027.51404-1.x86_64.rpm";
    sha256 = "7b7cebf685e6ed70c35895a00d78ebc49cbd14dd900c159a636203b8ee0fa4a1";
    repomd = ./chatgpt/repomd.xml;
    primary = "repodata/505da0d609c5c57da508b34955cec72730699ff2aa7d0f13e8cfa2620ed5df72-primary.xml.gz";
    primaryFile = ./chatgpt/505da0d609c5c57da508b34955cec72730699ff2aa7d0f13e8cfa2620ed5df72-primary.xml.gz;
  };

  claude-desktop = {
    version = "4.4212.0";
    # rime-pkg's CLAUDE_REPO default.
    repo = "https://downloads.claude.ai/claude-desktop/apt/stable";
    inRelease = ./claude-desktop/InRelease;
    packages = ./claude-desktop/Packages;
    packagesSha256 = "a8d00f835869869d2c0dd35da06a9c15faf8fca90ee0b6f666b7727650a3cebd";
    file = "pool/main/c/claude-desktop/claude-desktop_4.4212.0_amd64.deb";
    sha256 = "4fcf658e0fc895390ab82593287281dfbce261b0ec3ea90060e82d8bfbfd7a84";
  };
}
