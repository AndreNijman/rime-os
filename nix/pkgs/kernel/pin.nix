# kernel/kernel.pin, read as data. The pin file stays the ONE place that says what Rime's kernel is, on bootc
# and on NixOS alike: every URL and sha256 below comes from it, nothing is restated here. Bumping the kernel
# is still "edit kernel.pin" (then regenerate ./config, see ./README.md).
#
# Format (kernel.pin's own header): KEY=VALUE, `#` comments, no shell expansion.
{ lib }:

let
  text = builtins.readFile ../../../kernel/kernel.pin;
  lines = lib.filter (l: l != "" && !(lib.hasPrefix "#" l)) (map lib.trim (lib.splitString "\n" text));
  parse =
    l:
    let
      m = builtins.match "([A-Z0-9_]+)=(.*)" l;
    in
    if m == null then throw "kernel.pin: cannot parse line '${l}'" else lib.nameValuePair (lib.elemAt m 0) (lib.elemAt m 1);
  pin = lib.listToAttrs (map parse lines);
  need = k: pin.${k} or (throw "kernel.pin: ${k} is missing");
in
{
  version = need "KERNEL_VERSION"; # 7.2.9
  tag = need "KERNEL_TAG"; # cachyos-7.2.9-2
  src = {
    url = need "KERNEL_SRC_URL";
    sha256 = need "KERNEL_SRC_SHA256";
  };
  config = {
    url = need "KERNEL_CONFIG_URL";
    sha256 = need "KERNEL_CONFIG_SHA256";
  };
  bore = {
    url = need "KERNEL_PATCH_BORE_URL";
    sha256 = need "KERNEL_PATCH_BORE_SHA256";
  };
  # The pahole the kernel's Kconfig must record (CONFIG_PAHOLE_VERSION). On bootc this was enforced by pinning
  # Fedora's dwarves by koji NVR; on NixOS the flake-locked nixpkgs provides pahole and the gate checks it.
  paholeVersionExpected = need "PAHOLE_VERSION_EXPECTED"; # 132
  # Bumped when a rebuild's inputs change but the kernel version does not (bootc: `apex<N>` in the RPM release).
  build = need "APEX_KERNEL_BUILD";
}
