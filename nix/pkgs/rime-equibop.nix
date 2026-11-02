# Equibop as Rime ships it (the catalogue's `equibop`, D5): nixpkgs' build, with Equicord taken from nixpkgs' own
# `equicord` instead of downloaded at run time.
#
# nixpkgs' Equibop turns off its own updater, but not Equicord's: on first start it downloads
# github.com/Equicord/Equicord/releases/latest/download/equibop.asar into sessionData/equicord.asar and does nothing
# else (arRPC included) until it has it ("Failed to fetch ... Gonna retry with backoff"). That is an app updating
# itself outside `sudo rime update`, which the apps-update-only-through-Rime decision forbids, and offline it hangs
# with no message. So:
#
#   - the default Equicord location (vencordDir.ts) is nixpkgs' build of the same file, in the store, built with
#     --disable-updater. A user who points Equibop at their own Equicord directory (equicordDir) keeps it.
#   - downloadVencordAsar, behind "Force Update Equicord" and "Repair Equicord", refuses with a message instead of
#     downloading: Equicord moves with Rime's release (a nixpkgs bump), like every catalogue app.
#
# This is nixpkgs' vesktop `withSystemVencord` pattern (use_system_vencord.patch), done with substituteInPlace so a
# changed upstream line fails the build (--replace-fail) instead of shipping the download again.
# Checked by checks.equibop: offline, no seeded asar, arRPC answers and nothing is written to sessionData.
{ equibop, equicord }:

equibop.overrideAttrs (old: {
  pname = "rime-equibop";

  postPatch = (old.postPatch or "") + ''
    substituteInPlace src/main/vencordDir.ts \
      --replace-fail ': join(SESSION_DATA_DIR, "equicord.asar");' ': "${equicord}/equibop.asar";'
    substituteInPlace src/main/utils/vencordLoader.ts \
      --replace-fail 'export async function downloadVencordAsar() {' 'export async function downloadVencordAsar() {
        throw new Error("Equicord comes with Rime and updates with sudo rime update; Equibop does not download it");'
  '';

  passthru = (old.passthru or { }) // {
    inherit equicord;
  };

  meta = old.meta // {
    description = "Equibop with nixpkgs' Equicord (no run-time download), as Rime ships it";
  };
})
