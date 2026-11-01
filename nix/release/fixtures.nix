# Stand-in release inputs: two small toplevels shaped like NixOS ones (boot.json bootspec, kernel-modules link),
# so the stand-in and the VM test run the real pipeline without building a 10 GiB desktop system.
#
# Rime-built (not on any upstream cache): the kernel, initrd, modules, a "shell" and the toplevels themselves.
# Upstream: GNU hello and its closure from the pinned nixpkgs (cache.nixos.org has them; the VM test serves them
# from its own fake upstream cache). `tag` changes every Rime-built path, for a second release with new content.
{ pkgs, tag ? "a" }:
let
  lib = pkgs.lib;
  rimeFile = name: content: pkgs.runCommand "rime-standin-${name}-${tag}" { } ''
    mkdir -p $out
    printf '%s\n' ${lib.escapeShellArg content} > $out/${name}
  '';
  kernel = rimeFile "bzImage" "TEST stand-in kernel image ${tag}";
  initrd = rimeFile "initrd" "TEST stand-in initrd ${tag}";
  modules = pkgs.runCommand "rime-standin-kernel-modules-${tag}" { } ''
    mkdir -p $out/lib/modules/7.2.9-standin
    printf 'TEST module %s\n' ${tag} > $out/lib/modules/7.2.9-standin/rime.ko
  '';
  shell = pkgs.runCommand "rime-standin-shell-${tag}" { } ''
    mkdir -p $out/share/rime-shell
    printf 'TEST shell %s, runs %s\n' ${tag} ${pkgs.hello}/bin/hello > $out/share/rime-shell/shell.qml
  '';
  toplevel = variant: params: pkgs.runCommand "rime-standin-toplevel-${variant}-${tag}" { } ''
    mkdir -p $out
    ln -s ${kernel}/bzImage $out/kernel
    ln -s ${initrd}/initrd $out/initrd
    ln -s ${modules}/lib/modules $out/kernel-modules
    ln -s ${shell} $out/sw-shell
    ln -s ${pkgs.hello} $out/sw-hello
    cat > $out/boot.json <<JSON
    {"org.nixos.bootspec.v1":{"init":"$out/init","initrd":"${initrd}/initrd","kernel":"${kernel}/bzImage",
     "kernelParams":${builtins.toJSON params},"label":"Rime TEST stand-in (${variant})","system":"x86_64-linux",
     "toplevel":"$out"}}
    JSON
  '';
  plain = toplevel "plain" [ "quiet" "rime.standin=plain" ];
  luks = toplevel "luks" [ "quiet" "rd.luks=1" "rime.standin=luks" ];
  str = p: builtins.unsafeDiscardStringContext (toString p);
  components = {
    kernel = [ (str kernel) (str modules) ];
    shell = [ (str shell) ];
    libraries = [ (str pkgs.stdenv.cc.libc) ];
  };
in
{
  inherit plain luks;
  # The plan names paths WITHOUT depending on them, as the real one does: building it builds nothing else.
  plan = pkgs.writeText "rime-standin-plan.json" (builtins.toJSON {
    variants = {
      plain = { toplevel = str plain; publishable = false; note = "TEST stand-in"; inherit components; };
      luks = { toplevel = str luks; publishable = false; note = "TEST stand-in"; inherit components; };
    };
    catalogue = [ ];
  });
}
