#!/usr/bin/env bash
# gens.sh: build the lab's six generations (systems.nix) in the rime-nix container and record their toplevels in
# $LAB/gens.json. Needs $LAB/in/kernel.sig (the detached TEST-MOK signature over the unsigned bzImage, made by run.sh).
set -euo pipefail
LAB=${LAB:-/var/lab/rime-nix/scratch/boothook}
WT=${WT:-/var/lab/rime-nix/wt-boothook}
to_work() { echo "/work/${1#/var/lab/rime-nix/}"; }
CLAB=$(to_work "$LAB") CWT=$(to_work "$WT")
expr() { echo "(import $CWT/nix/probe/ovmf/boothook/systems.nix { flake = builtins.getFlake \"path:$CWT\"; signature = $CLAB/in/kernel.sig; certificate = $CLAB/keys/test-mok.crt; }).$1"; }
json='{'
for l in bls custom; do
  json+="\"$l\":{"
  for g in gen1 gen2 gen3; do
    p=$(sudo -n podman exec rime-nix nix build --impure --no-link --print-out-paths --option max-jobs auto --option cores 0 \
      --expr "$(expr "$l.$g")" 2>>"$LAB/gens.log")
    echo "$l.$g $p"
    json+="\"$g\":\"$p\","
  done
  json="${json%,}},"
done
echo "${json%,}}" > "$LAB/gens.json"
