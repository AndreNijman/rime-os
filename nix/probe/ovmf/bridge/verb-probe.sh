#!/bin/bash
# verb-probe.sh LABEL WANT: runs as root INSIDE a bridge lab VM (KVM guest), never on a real machine. WANT is
# `allowed` or `refused`: what the readiness release's `rime` must answer for EVERY verb it holds once
# `rime migrate start` has run (bridged_verb in rimed/rime/src/main.rs on feat/nixos-readiness), given the start record
# this guest has now (/var/lib/rime/nixos-bridge/state.json), and what rime-update-check must answer with it.
#
# Each verb runs as the real /usr/bin/rime, as root, in a private mount namespace whose bootc, ostree, rpm-ostree,
# skopeo, cosign, flatpak, fwupdmgr, dnf/dnf5, systemd-sysext and rime-boot-migrate are STUBS that log their call and
# exit 1. The guard reads the real record before anything else, so:
#   refused = exit 3 AND the literal `REFUSED [nixos-migration-started]` AND no stub was reached (nothing ran);
#   allowed = the output never names nixos-migration-started (the verb went on past the guard; the stubs it reached
#             are listed: the evidence it went on to drive bootc/ostree, which the stubs then stopped).
# The stubs keep an ALLOWED verb from changing this disk (a pin, a boot-path migration, a staged update), so the probe
# can run on the disk the lab goes on to migrate. rime-update-check runs the same way, with a stub bootc that says an
# update is available: allowed = it writes state=available, refused = it logs the refusal and writes state=none.
# Prints one `verb=[...] rc= stubs= guard=` line per verb and `PROBE-VERDICT=ok|bad` (computed here, so the driver's
# echoed command line can never match it).
set -uo pipefail
LABEL=$1 WANT=$2
[[ $WANT == allowed || $WANT == refused ]] || { echo "verb-probe: WANT is allowed or refused"; exit 2; }
[[ $(systemd-detect-virt) == kvm ]] || { echo "verb-probe: not a KVM guest; refusing"; exit 2; }
[[ $(id -u) == 0 ]] || { echo "verb-probe: needs root"; exit 2; }
REFUSED='REFUSED [nixos-migration-started]'
REC=/var/lib/rime/nixos-bridge/state.json
S=/run/lab-stub
OUT=/run/lab-probe/$LABEL
rm -rf "$S" "$OUT"; mkdir -p "$S/bin" "$OUT"
cat > "$S/bin/stub" <<'STUB'
#!/bin/sh
echo "LAB-STUB-REACHED $(basename "$0") $*" >> /run/lab-stub/calls
echo "LAB-STUB-REACHED $(basename "$0") $*" >&2
exit 1
STUB
chmod 755 "$S/bin/stub"
targets=()
for b in bootc ostree rpm-ostree skopeo cosign flatpak fwupdmgr dnf dnf5 systemd-sysext; do
  p=$(command -v "$b" 2>/dev/null) && targets+=("$(readlink -f "$p")")
done
for p in /usr/libexec/rime-boot-migrate /usr/libexec/rime-pkg; do [[ -e $p ]] && targets+=("$p"); done
printf 'stubbed: %s\n' "${targets[@]}"
echo "== record ($REC)"
if [[ -e $REC ]]; then cat "$REC"; echo; else echo "absent"; fi

# The verbs bridged_verb names (the readiness test `the_nixos_move_holds_every_bootc_verb_and_nothing_else`).
# `channel set` with --dry-run: the guard matches every form of it, and the dry run changes nothing even if allowed.
VERBS=("update" "update --check" "update --firmware-only" "update --migrate-boot" "rollback" "pin"
       "channel set stable --dry-run" "boot migrate" "live apply" "live rollback" "live soft-reboot --yes")
in_ns() {   # in_ns CMD...: CMD in a private mount namespace with the stubs bound over the real programs
  # shellcheck disable=SC2016 # the inner script expands its own positional parameters
  unshare --mount --propagation private -- bash -c '
    for t in "${@:2:$1}"; do mount --bind /run/lab-stub/bin/stub "$t" || exit 97; done
    shift $(( $1 + 1 )); exec "$@"' _ "${#targets[@]}" "${targets[@]}" "$@"
}
nr=0 na=0 nx=0 i=0
for v in "${VERBS[@]}"; do
  i=$((i + 1))
  : > "$S/calls"
  # shellcheck disable=SC2086
  o=$(in_ns timeout 300 /usr/bin/rime $v </dev/null 2>&1); r=$?
  printf '%s\n' "$o" > "$OUT/verb-$i.txt"
  stubs=$(wc -l < "$S/calls")
  if [[ $r == 3 ]] && grep -F "$REFUSED" "$OUT/verb-$i.txt" >/dev/null && [[ $stubs == 0 ]]; then
    g=refused; nr=$((nr + 1))
  elif ! grep -F 'nixos-migration-started' "$OUT/verb-$i.txt" >/dev/null && [[ $r != 97 ]]; then
    g=allowed; na=$((na + 1))
  else
    g=INCONSISTENT; nx=$((nx + 1))
  fi
  echo "=== rime $v"
  sed 's/^/  | /' "$OUT/verb-$i.txt"
  sed 's/^/  stub: /' "$S/calls"
  echo "verb=[rime $v] rc=$r stubs=$stubs guard=$g"
done

# rime-update-check: the real script, the real record, a stub bootc that reports an update.
cat > "$S/bin/bootc-update" <<'STUB'
#!/bin/sh
echo "LAB-STUB-REACHED bootc $*" >> /run/lab-stub/calls
case "$*" in
  "status --format=json") echo '{"status":{"staged":null}}' ;;
  "upgrade --check") printf 'Update available for: lab\n  Digest: sha256:%s\n' "$(printf 'a%.0s' $(seq 64))" ;;
  *) exit 1 ;;
esac
STUB
chmod 755 "$S/bin/bootc-update"
: > "$S/calls"
rm -rf /run/lab-ruc
o=$(RIME_BOOTC_BIN=$S/bin/bootc-update RIME_UPDATE_DIR=/run/lab-ruc /usr/libexec/rime-update-check 2>&1); r=$?
st=$(cat /run/lab-ruc/state 2>/dev/null | tr '\n' ' ')
echo "=== /usr/libexec/rime-update-check (stub bootc reports an update)"
printf '%s\n' "$o" | sed 's/^/  | /'
sed 's/^/  stub: /' "$S/calls"
echo "  state: $st"
if grep -F "$REFUSED" <<<"$o" >/dev/null && [[ $st == "state=none digest= "* ]] && ! grep 'upgrade --check' "$S/calls" >/dev/null; then
  ruc=refused
elif ! grep -F 'nixos-migration-started' <<<"$o" >/dev/null && [[ $st == state=available* ]]; then
  ruc=allowed
else
  ruc=INCONSISTENT
fi
echo "update-check rc=$r guard=$ruc"
echo "=== units the bridge masks (is-enabled)"
for u in rime-update-check.timer rime-update-check.service rime-store-compact.timer rime-store-compact.service \
         bootloader-update.service rime-boot-migrate-confirm.service; do   # MASK_UNITS in rime-migrate
  echo "  $u: $(systemctl is-enabled "$u" 2>&1)"
done
echo "=== rime migrate check (what the bridge itself says about the record)"
rime migrate check 2>&1 | grep -E 'REFUSED|start-|already-moved|record' | sed 's/^/  | /'
n=${#VERBS[@]}
echo "VERBS=$n REFUSED-N=$nr ALLOWED-N=$na INCONSISTENT-N=$nx UPDATE-CHECK=$ruc WANT=$WANT"
if [[ $WANT == refused && $nr == "$n" && $ruc == refused ]] || [[ $WANT == allowed && $na == "$n" && $ruc == allowed ]]; then
  echo "PROBE-VERDICT=ok"
else
  echo "PROBE-VERDICT=bad"
fi
