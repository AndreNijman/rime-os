#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-boot-migrate.sh — executable assertions for the in-place move from
#  ostree + GRUB to composefs + systemd-boot.
#
#  What it can and cannot check, said plainly. The migration's real proof is a
#  guest with its power cut: ROADMAP/evidence/sdboot-migrate-20260921-lab.md
#  has eleven boots' worth, and nothing here replaces it. What a CI runner can
#  check is the part that is most likely to rot silently:
#
#    * the ORDER of the writes — nothing that changes what the firmware boots
#      may happen before the single BootNext commit;
#    * that every refusal exists and fires, because a precheck that stopped
#      refusing would migrate a machine that must not be migrated;
#    * that the state machine cannot skip a step or re-arm a failed migration;
#    * that the unit's conditions and ordering are what the design says.
#
#  Every assertion here is written to fail if the property is removed. The
#  suite was mutation-tested: each block below was checked to go red when the
#  line it guards is changed.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MIG="$REPO/files/system/libexec/rime-boot-migrate"
UNIT="$REPO/files/system/units/rime-boot-migrate-confirm.service"
BASECF="$REPO/Containerfile.base"
OPS="$REPO/rimed/rime/src/ops.rs"

PASS=0 FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }

for f in "$MIG" "$UNIT" "$BASECF" "$OPS"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done
[[ -x "$MIG" ]] || { echo "FATAL: $MIG is not executable in the repo" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A body without its comments. Every grep for "does the code do X" runs against
# this, because this file argues with itself at length in comments and a naive
# grep would match the argument instead of the code. (The same trap the
# Containerfile tripwire hit: a comment naming a command it refuses to run.)
CODE="$TMP/code.sh"
sed 's/[[:space:]]*#.*$//' "$MIG" > "$CODE"

run_mig() {   # run the engine with a fixture state dir; never touches this machine
    RIME_MIGRATE_STATE="$TMP/state" \
    RIME_MIGRATE_ROOT="$TMP/sysroot" \
    RIME_MIGRATE_ESP="$TMP/esp" \
    RIME_MIGRATE_DRYRUN=1 \
    RIME_MIGRATE_STORE="${STORE:-ostreeContainer}" \
        bash "$MIG" "$@" 2>&1
}

# ═════════════════════════════════════════════════════════════════════════════
sec "the commit is BootNext, and nothing before it changes what boots"
# This is the property the whole design rests on. If a stage ever writes
# BootOrder or BootNext, a power cut in the middle of the ESP write leaves a
# machine pointed at a half-written loader.
if grep -n 'efibootmgr --bootnext' "$CODE" >/dev/null; then
    ok "the commit writes BootNext"
else
    bad "nothing writes BootNext — where is the commit point?"
fi
if grep -n 'create-only' "$CODE" >/dev/null; then
    ok "the boot entry is created with --create-only (not put into BootOrder)"
else
    bad "the entry is created without --create-only, so creating it reorders the boot"
fi

# The stage function must contain no efibootmgr call at all. Extracted by
# brace-free line range: from `cmd_stage() {` to the next line that is a
# function definition at column 0.
stage_body() {
    awk '/^cmd_stage\(\) \{/{inside=1} inside{print} inside && /^\}/{exit}' "$CODE"
}
if stage_body | pipe_has 'efibootmgr'; then
    bad "cmd_stage calls efibootmgr — the stage must change no boot variable"
else
    ok "cmd_stage calls efibootmgr nowhere"
fi
if stage_body | pipe_has -E 'bootctl[[:space:]]+(install|update)|bootupctl|grub2-install'; then
    bad "cmd_stage installs a bootloader with a tool that writes a live boot path"
else
    ok "cmd_stage installs no bootloader of its own"
fi

# And BootOrder is written only by confirm.
confirm_body() {
    awk '/^cmd_confirm\(\) \{/{inside=1} inside{print} inside && /^\}/{exit}' "$CODE"
}
if confirm_body | pipe_has 'efibootmgr --bootorder'; then
    ok "BootOrder is written by cmd_confirm"
else
    bad "cmd_confirm does not write BootOrder"
fi
if grep -c 'efibootmgr --bootorder' "$CODE" | pipe_has -x 1; then
    ok "BootOrder is written in exactly one place"
else
    bad "BootOrder is written in more than one place"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the install cannot reach the real /boot"
# Measured: run bare, `bootc install to-existing-root` deletes /EFI/fedora,
# overwrites /EFI/BOOT/BOOTX64.EFI and wipes the root filesystem's /boot. The
# bind over /target/boot is the only thing that stops the third one.
if grep -qE '\-v "\$stagedir:/target/boot"' "$CODE"; then
    ok "the install binds a staging filesystem over /target/boot"
else
    bad "nothing binds /target/boot — bootc's wipe would hit the real one"
fi
if grep -q 'mkfs.vfat' "$CODE"; then
    ok "the staging is a real filesystem (bootc reads a UUID off it)"
else
    bad "the staging is not a filesystem; bootc fails with 'No UUID found for /boot'"
fi
# The staging must carry the ESP's own volume id, or the migrated machine is
# told to mount a filesystem that was deleted minutes earlier. Measured: the
# guest booted and dropped to emergency mode on boot.mount.
if grep -qE 'mkfs.vfat .*-i "\$volid"' "$CODE"; then
    ok "the staging filesystem is given the ESP's volume id"
else
    bad "the staging gets a fresh volume id, which leaks into boot=UUID= on the cmdline"
fi
if grep -q 'entry-names-wrong-boot' "$CODE"; then
    ok "the staged entry's boot=UUID= is checked against the real ESP"
else
    bad "nothing checks the boot=UUID= the machine will boot with"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the old path is preserved, not destroyed"
for want in 'BOOTX64.EFI.orig' 'grub-esp-moved' 'grub-boot-wiped'; do
    if grep -q "$want" "$CODE"; then
        ok "the stage guards: $want"
    else
        bad "missing guard: $want"
    fi
done
# GRUB is demoted, never removed: nothing may delete /EFI/fedora or the ostree
# deployment.
if grep -qE 'rm -rf .*(EFI/fedora|ostree/deploy)|ostree admin undeploy' "$CODE"; then
    bad "the engine deletes part of the old boot path"
else
    ok "nothing deletes /EFI/fedora or the ostree deployment"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "every refusal exists"
# Each of these is a machine that must NOT be migrated. A precheck that stops
# refusing is how a laptop with a 600 MiB ESP gets a migration that cannot fit.
for token in not-root not-uefi already-migrated update-staged bootc-too-old \
             secure-boot-unsigned-loader no-esp esp-too-small no-dosfstools \
             no-rsync no-podman no-repo-size root-too-small; do
    if grep -q "refuse \"$token\"" "$CODE"; then
        ok "refuses: $token"
    else
        bad "no refusal for: $token"
    fi
done
# A refusal must exit 10 and say nothing was changed: `rime update` reads that
# code to mean "carry on with the normal update", and anything else would make
# a machine that cannot migrate also fail to update.
if grep -qE '^refuse\(\).*exit 10|exit 10; \}' "$CODE"; then
    ok "a refusal exits 10"
else
    bad "a refusal does not exit 10 — rime update would treat it as a failure"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the esp-too-small refusal names the XBOOTLDR partition it cannot use"
# Everyone who reads esp-too-small on a machine that has a spare XBOOTLDR
# partition has the same idea within a minute, because the Boot Loader
# Specification is written for exactly that layout and systemd-boot reads it
# fine. bootc's composefs backend does not write it -- the BLSCompatible arm of
# setup_composefs_bls_boot mounts the ESP unconditionally -- so the refusal has
# to say so. docs/boot-v2.md, "XBOOTLDR: the Boot Loader Specification allows
# it, bootc does not implement it".
if grep -q 'XBOOTLDR_TYPE_GUID=bc13c2ff-59e6-4262-a352-b275fd6f7172' "$CODE"; then
    ok "the engine knows the XBOOTLDR type GUID"
else
    bad "XBOOTLDR_TYPE_GUID is missing or not bc13c2ff-59e6-4262-a352-b275fd6f7172"
fi
# The note must be attached to the esp-too-small refusal and nowhere else: a
# note printed on a machine that is migrating fine would be noise.
esp_refusal_block="$(awk '/refuse "esp-too-small"/{inside=1} inside{print} inside && /docs\/boot-v2.md/{exit}' "$CODE")"
if grep -q 'xbnote' <<<"$esp_refusal_block"; then
    ok "the esp-too-small refusal carries the XBOOTLDR note"
else
    bad "esp-too-small says nothing about XBOOTLDR — the next reader will spend an evening on it"
fi

# And the finder itself, run for real against a fake block layout, both ways.
# A grep alone would pass on a find_xbootldr that never matches anything.
FAKEBIN="$TMP/fakebin"; mkdir -p "$FAKEBIN"
cat > "$FAKEBIN/findmnt" <<'FAKE'
#!/usr/bin/env bash
echo /dev/faked1p3
FAKE
cat > "$FAKEBIN/lsblk" <<'FAKE'
#!/usr/bin/env bash
dev="${*: -1}"; flags="$*"
case "$flags" in
    *-ndo\ TYPE*|*-dno\ TYPE*)
        case "$dev" in */faked1) echo disk ;; *) echo part ;; esac ;;
    *PKNAME*)   echo faked1 ;;
    *-lno\ NAME*) printf 'faked1\nfaked1p1\nfaked1p2\nfaked1p3\n' ;;
    *PARTTYPE*)
        case "$dev" in
            */faked1p1) echo c12a7328-f81f-11d2-ba4b-00a0c93ec93b ;;
            */faked1p2) echo "${FAKE_P2_TYPE:-bc13c2ff-59e6-4262-a352-b275fd6f7172}" ;;
            *)          echo 4f68bce3-e8cd-4db1-96e7-fbcaf984b709 ;;
        esac ;;
esac
FAKE
chmod +x "$FAKEBIN/findmnt" "$FAKEBIN/lsblk"

# The engine's own definitions, lifted verbatim: from `set -uo pipefail` down to
# the line that ends the block of finders. Testing a copy would test the copy.
FINDERS="$TMP/finders.sh"
sed -n '/^set -uo pipefail/,/^esp_mounted_at=/p' "$MIG" | head -n -1 > "$FINDERS"
grep -q 'find_xbootldr()' "$FINDERS" || { echo "FATAL: find_xbootldr not in the extracted range" >&2; exit 1; }

probe_xbootldr() {   # $1 = the GPT type to give the fake p2
    FAKE_P2_TYPE="$1" PATH="$FAKEBIN:$PATH" bash -c \
        'source "$1"; if out="$(find_xbootldr)"; then printf "FOUND:%s\n" "$(tr "\n" "," <<<"$out")"; else printf "NONE\n"; fi' \
        _ "$FINDERS" 2>&1
}
got="$(probe_xbootldr bc13c2ff-59e6-4262-a352-b275fd6f7172)"
if [[ "$got" == FOUND:/dev/faked1p2* ]]; then
    ok "find_xbootldr finds an EA00 partition on the root disk"
else
    bad "find_xbootldr missed an XBOOTLDR partition: $got"
fi
got="$(probe_xbootldr 0fc63daf-8483-4772-8e79-3d69d8477de4)"
if [[ "$got" == NONE ]]; then
    ok "find_xbootldr reports nothing when no partition is XBOOTLDR-typed"
else
    bad "find_xbootldr matched a plain Linux partition: $got"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "a machine that boots from another disk's ESP is told, not refused"
# katana boots Rime off the 200 MiB ESP on the WINDOWS disk while its own
# 512 MiB EFI-SYSTEM sits unused on the Rime disk. find_esp resolves the
# machine's OWN disk, so migrating moves the boot onto it -- the right answer,
# and a change of disk the user should hear about. It must stay a note: a
# refusal would keep katana depending on another operating system's disk
# forever, and a firmware that cannot see the new ESP is already covered by
# the trial boot coming back to GRUB.
if grep -q 'booted_esp_partuuid' "$CODE"; then
    ok "the engine can read which ESP the firmware is booting from"
else
    bad "nothing reads BootCurrent's ESP — a cross-disk migration would be silent"
fi
if grep -qE 'refuse "(different-esp|foreign-esp|cross-disk[a-z-]*)"' "$CODE"; then
    bad "booting from another disk's ESP is a REFUSAL — that would strand katana on the Windows disk"
else
    ok "booting from another disk's ESP is not a refusal"
fi
precheck_body="$(awk '/^cmd_precheck\(\) \{/{inside=1} inside{print} inside && /^\}/{exit}' "$CODE")"
if grep -q 'booted_esp_partuuid' <<<"$precheck_body"; then
    ok "the precheck is where the cross-disk case is noticed"
else
    bad "booted_esp_partuuid is never called from the precheck"
fi

# The parser, run for real against katana's actual efibootmgr shape.
cat > "$FAKEBIN/efibootmgr" <<'FAKE'
#!/usr/bin/env bash
cat <<'OUT'
BootCurrent: 0000
Timeout: 1 seconds
BootOrder: 0000,0001,0002
Boot0000* APEX-OS Primary	HD(1,GPT,2ba9a2ea-5f0c-4c2b-9d31-7a1e6b0c8d44,0x800,0x64000)/File(\EFI\APEX\SHIMX64.EFI)
Boot0001* Windows Boot Manager	HD(1,GPT,2ba9a2ea-5f0c-4c2b-9d31-7a1e6b0c8d44,0x800,0x64000)/File(\EFI\Microsoft\Boot\bootmgfw.efi)
Boot0002* UEFI: Generic Flash Disk	PciRoot(0x0)/Pci(0x14,0x0)/USB(0,0)
OUT
FAKE
chmod +x "$FAKEBIN/efibootmgr"
probe_booted_esp() {   # $1 = optional override of the whole efibootmgr output
    PATH="$FAKEBIN:$PATH" bash -c 'source "$1"; booted_esp_partuuid || echo NONE' _ "$FINDERS" 2>&1
}
got="$(probe_booted_esp)"
if [[ "$got" == "2ba9a2ea-5f0c-4c2b-9d31-7a1e6b0c8d44" ]]; then
    ok "booted_esp_partuuid reads BootCurrent's GPT PARTUUID out of efibootmgr -v"
else
    bad "booted_esp_partuuid returned '$got', not katana's Boot0000 PARTUUID"
fi
# BootCurrent pointing at an entry with no GPT device path (a USB stick, a
# network boot) must yield nothing rather than a wrong partition.
cat > "$FAKEBIN/efibootmgr" <<'FAKE'
#!/usr/bin/env bash
cat <<'OUT'
BootCurrent: 0002
BootOrder: 0002,0000
Boot0000* APEX-OS Primary	HD(1,GPT,2ba9a2ea-5f0c-4c2b-9d31-7a1e6b0c8d44,0x800,0x64000)/File(\EFI\APEX\SHIMX64.EFI)
Boot0002* UEFI: Generic Flash Disk	PciRoot(0x0)/Pci(0x14,0x0)/USB(0,0)
OUT
FAKE
got="$(probe_booted_esp)"
if [[ -z "$got" || "$got" == NONE ]]; then
    ok "a BootCurrent with no GPT device path yields nothing, not another entry's UUID"
else
    bad "booted_esp_partuuid invented '$got' for a USB boot"
fi
# Firmware prints the loader two ways and the L16 uses the one WITHOUT the
# File() wrapper, so both shapes are fixtures rather than one being assumed.
# Measured on the L16, read-only, 2026-09-21:
#   Boot0000* Rime OS	HD(1,GPT,1c417de2-…,0x800,0x12c000)/\EFI\fedora\shimx64.efi
cat > "$FAKEBIN/efibootmgr" <<'FAKE'
#!/usr/bin/env bash
cat <<'OUT'
BootCurrent: 0000
Timeout: 0 seconds
BootOrder: 0000,0020,0004
Boot0000* Rime OS	HD(1,GPT,1c417de2-5766-455f-9318-198610885424,0x800,0x12c000)/\EFI\fedora\shimx64.efi
Boot0004* UEFI: IP4 Realtek PCIe GBE	PciRoot(0x0)/Pci(0x1c,0x4)/MAC(001122334455,0)
OUT
FAKE
got="$(probe_booted_esp)"
if [[ "$got" == "1c417de2-5766-455f-9318-198610885424" ]]; then
    ok "the device-path form without File() parses too (the L16's own shape)"
else
    bad "booted_esp_partuuid returned '$got' for the L16's real Boot0000 line"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the state machine"
rm -rf "$TMP/state"; mkdir -p "$TMP/state" "$TMP/esp" "$TMP/sysroot"

out="$(run_mig commit || true)"
if grep -q 'REFUSED \[nothing-staged\]' <<<"$out"; then
    ok "commit without a stage is refused"
else
    bad "commit without a stage was not refused: $out"
fi

echo failed > "$TMP/state/phase"
out="$(run_mig auto || true)"
if grep -q 'REFUSED \[last-attempt-failed\]' <<<"$out"; then
    ok "auto refuses to re-arm a migration whose trial boot failed"
else
    bad "auto re-arms a failed migration — every update would spend a reboot on it"
fi

out="$(run_mig retry || true)"
if [[ "$(cat "$TMP/state/phase")" == staged ]]; then
    ok "retry puts a failed migration back to staged"
else
    bad "retry did not re-arm: phase is $(cat "$TMP/state/phase")"
fi

echo confirmed > "$TMP/state/phase"
out="$(run_mig retry || true)"
if grep -q 'REFUSED \[nothing-failed\]' <<<"$out"; then
    ok "retry refuses when nothing failed"
else
    bad "retry fired on a machine with nothing to retry"
fi

echo committed > "$TMP/state/phase"
out="$(run_mig abort || true)"
if grep -q 'REFUSED \[already-committed\]' <<<"$out"; then
    ok "abort refuses past the commit point"
else
    bad "abort discarded a committed migration"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the machine's data comes with it"
# A migration that loses /var/home is a reinstall wearing a migration's name.
if grep -q 'join_state' "$CODE"; then ok "the stage joins /var and /etc"
else bad "nothing joins the machine's state"; fi
# The rename has to happen in a private mount namespace: the ostree stateroot's
# var is a mountpoint on a running machine and a plain rename is EBUSY.
if grep -q 'unshare -m --propagation private' "$CODE"; then
    ok "/var is moved inside a private mount namespace"
else
    bad "/var is moved in the host namespace, where the rename returns EBUSY"
fi
# And the direction matters: the composefs path needs a real directory, or its
# /var comes up read-only. Measured both ways in the same guest.
if grep -qE 'mv -f "\$1" "\$2"' "$CODE"; then
    ok "the machine's var is moved INTO the composefs stateroot"
else
    bad "the var join does not move the machine's var into the stateroot"
fi
if grep -q 'ln -s "\$3" "\$1"' "$CODE"; then
    ok "a symlink is left where ostree looks, so the GRUB path still has /var"
else
    bad "nothing is left behind for the GRUB path — the fallback would boot empty"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the confirm unit"
grep -q '^ConditionPathExists=/var/lib/rime/boot-migrate/phase$' "$UNIT" \
    && ok "inert on a machine that never started a migration" \
    || bad "the unit runs on machines with no migration in flight"
grep -q '^Wants=boot-complete.target$' "$UNIT" \
    && ok "pulls in boot-complete.target where it can be reached" \
    || bad "the unit does not pull in the health target"
if grep -q '^Requires=boot-complete.target' "$UNIT"; then
    bad "Requires= on a target a migrated machine may never reach: the confirm would never run"
else
    ok "does not Require= a target the first migrated boot may not reach"
fi
grep -q '^WantedBy=multi-user.target$' "$UNIT" \
    && ok "WantedBy, so a machine that cannot run it still boots" \
    || bad "the unit is not WantedBy=multi-user.target"
grep -q 'ExecStart=/usr/libexec/rime-boot-migrate confirm' "$UNIT" \
    && ok "runs the engine's confirm verb" \
    || bad "the unit runs something other than 'rime-boot-migrate confirm'"

# ═════════════════════════════════════════════════════════════════════════════
sec "rime update runs it, and a refusal does not stop the update"
grep -q 'fn migrate_boot_path' "$OPS" \
    && ok "ops.rs has the migration step" \
    || bad "rime update does not call the migration"
grep -q 'Ok(10) =>' "$OPS" \
    && ok "ops.rs treats exit 10 (refused) as 'carry on'" \
    || bad "a refusal is not distinguished from a failure"
# Instead of, not as well as: an ostree deployment staged in the same
# invocation would give one shutdown two finalize paths.
if grep -q 'return finish_update(started, worst, &opts);' "$OPS"; then
    ok "a machine that migrated does not also stage an image update"
else
    bad "the update carries on into bootc upgrade after migrating"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the image ships it"
grep -q 'COPY --chmod=0755 files/system/libexec/rime-boot-migrate' "$BASECF" \
    && ok "Containerfile.base ships the engine" \
    || bad "the engine is not in the image"
grep -q 'systemctl enable rime-boot-migrate-confirm.service' "$BASECF" \
    && ok "the confirm unit is enabled at build time" \
    || bad "the confirm unit is shipped but never enabled"
for dep in podman mkfs.vfat rsync efibootmgr unshare; do
    grep -q "command -v $dep" "$BASECF" \
        && ok "the image asserts $dep is present" \
        || bad "$dep is a runtime dependency of the migration and is not asserted"
done

# ═════════════════════════════════════════════════════════════════════════════
sec "the five holes the review found, each with the assertion that would catch it"

# 1. A migrated machine must not be told on every update that it stays on GRUB.
rm -rf "$TMP/state"; mkdir -p "$TMP/state"
echo confirmed > "$TMP/state/phase"
# `set -e` would kill the suite on the non-zero exit these cases are ABOUT,
# so every one of them captures the code instead of letting it propagate.
rc=0; run_mig auto >/dev/null 2>&1 || rc=$?
if [[ "$rc" == 3 ]]; then
    ok "auto on a confirmed machine exits 3 (nothing to do), not 10 (refused)"
else
    bad "auto on a confirmed machine exits $rc — rime update would print a refusal forever"
fi
grep -q 'Ok(3) => false' "$OPS" \
    && ok "rime update treats exit 3 as silence" \
    || bad "rime update does not handle the 'nothing to do' code"

# 2. Two updates in one boot must not re-run the install.
rm -rf "$TMP/state"; mkdir -p "$TMP/state"
echo committed > "$TMP/state/phase"
cat /proc/sys/kernel/random/boot_id > "$TMP/state/committed-boot"
rc=0; out="$(run_mig auto 2>&1)" || rc=$?
if [[ "$rc" == 0 ]] && grep -q 'reboot to finish' <<<"$out"; then
    ok "committed in this boot: auto says reboot, and runs no install"
else
    bad "auto re-ran on a machine committed in this same boot (rc=$rc): $out"
fi
# Specifically: the COMMIT writes it. Checking that the file is merely
# mentioned passes while nothing creates it — the fixture writes one itself,
# so that weaker assertion survived the mutation that removed the write.
commit_body() {
    awk '/^cmd_commit\(\) \{/{inside=1} inside{print} inside && /^\}/{exit}' "$CODE"
}
if commit_body | pipe_has 'committed-boot'; then
    ok "the commit records which boot it happened in"
else
    bad "nothing records the committing boot, so a second update cannot tell"
fi
# committed in an EARLIER boot, still on GRUB, is the failed case
echo committed > "$TMP/state/phase"
echo "not-this-boot" > "$TMP/state/committed-boot"
out="$(run_mig auto 2>&1)" || true
if grep -q 'REFUSED \[last-attempt-failed\]' <<<"$out"; then
    ok "committed in an earlier boot and still on GRUB is recorded as failed"
else
    bad "a trial boot that never happened is retried: $out"
fi

# 3. The saved fallback must never be overwritten by a re-run.
if grep -q 'fallback-already-systemd-boot' "$CODE"; then
    ok "refuses when the fallback is already systemd-boot with nothing saved"
else
    bad "a re-run could save systemd-boot as 'the original' fallback"
fi
if grep -qE '\[ ! -f "\$STATE/BOOTX64.EFI.orig" \]' "$CODE"; then
    ok "the fallback is saved once and never over an existing copy"
else
    bad "the fallback save is unconditional — a re-run destroys the good copy"
fi

# 4. The ESP has to hold three deployments, which is what the message says.
if grep -q 'need=$(( per \* 3 / 1024' "$CODE"; then
    ok "the ESP check sizes for three deployments"
else
    bad "the ESP check sizes for fewer deployments than an update needs"
fi

# 4b. The ROOT filesystem is checked too, and separately from the ESP — a
# migration writes a second full image copy into /composefs, and the ESP
# being big enough says nothing about that. This is item 2's whole point:
# before this, only the ESP was checked.
if grep -q 'root_need=$(( repo_kib \* 2' "$CODE"; then
    ok "the root check sizes for two image copies (the temporary + the permanent one)"
else
    bad "the root check does not size for both the temporary and permanent copy"
fi
# 4c. The stateroot's var must be the DIRECTORY, never the symlink beside it.
#
# bootc's composefs layout puts TWO things called `var` exactly three levels
# under $SYSROOT/state:
#
#     state/os/<stateroot>/var            the real directory
#     state/deploy/<digest>/var  ->  ../../os/<stateroot>/var
#
# `find` walks in readdir order, not sorted, so a lookup without `-type d`
# returns whichever the filesystem hands back first. Measured on the real Rime
# image on btrfs, 2026-09-22: it returned the SYMLINK, join_state's
# `[ ! -L "$newvar" ]` was false, and the migration failed `state-join` AFTER a
# completely successful install. The predecessor's ext4 fedora-bootc guest got
# the directory first, so the bug was invisible for two rounds.
# ROADMAP/evidence/sdboot-migrate-3-20260922-lab.md, section 4.
#
# This is behavioural, and the expression under test is EXTRACTED FROM THE
# SOURCE rather than copied here — a copy would keep passing after the source
# drifted, which is the failure mode this suite exists to catch.
jt="$(mktemp -d)"
mkdir -p "$jt/state/os/default/var" "$jt/state/deploy/abc123"
ln -s ../../os/default/var "$jt/state/deploy/abc123/var"
jt_expr="$(sed -n 's/^[[:space:]]*newvar="\$(\(.*\))"$/\1/p' "$CODE" | head -1)"
if [[ -z "$jt_expr" ]]; then
    bad "could not extract join_state's newvar lookup from the source — this test is inspecting nothing"
else
    jt_got="$(SYSROOT="$jt" bash -c "$jt_expr" 2>/dev/null)"
    if [[ -n "$jt_got" && -d "$jt_got" && ! -L "$jt_got" ]]; then
        ok "join_state's var lookup returns a real directory, not a symlink"
    else
        bad "join_state's var lookup returned '$jt_got' — a symlink or nothing, so the /var join fails after a successful install"
    fi
    if [[ "$jt_got" == "$jt/state/os/default/var" ]]; then
        ok "join_state's var lookup picks the stateroot's own var, not the deployment's link to it"
    else
        bad "join_state's var lookup picked '$jt_got', not \$SYSROOT/state/os/default/var"
    fi
fi
# And prove the hazard is real rather than hypothetical: with the type filter
# removed, the candidate set genuinely does contain a symlink, so `head -1`
# has something wrong to pick. If this ever stops holding, the assertions
# above are guarding a layout that no longer exists and should be revisited.
if [[ -n "$(find "$jt/state" -mindepth 3 -maxdepth 3 -name var -type l 2>/dev/null)" ]]; then
    ok "the layout really does offer a symlink candidate at the same depth"
else
    bad "the fixture has no symlink candidate — this test would pass without -type d"
fi
rm -rf "$jt"

if grep -qE 'df -Pk "\$SYSROOT"' "$CODE"; then
    ok "the root check reads free space on \$SYSROOT, not the ESP"
else
    bad "the root check does not measure \$SYSROOT — it may be checking the wrong filesystem"
fi
if grep -qE 'du -sk "\$SYSROOT/ostree/repo"' "$CODE"; then
    ok "the root check estimates size from the ostree repo, offline and without copying anything"
else
    bad "the root check has no offline size estimate — it may need to copy data to know if there is room for the copy"
fi
# The root check has to run whether or not this machine even has room for it
# to mount an ESP — a machine that is refusing no-esp should still refuse
# root-too-small first if it is ALSO too small, so a fix to one refusal is not
# mistaken for a fix to both. Order it ahead of with_esp in the source.
if awk '/^cmd_precheck\(\) \{/{p=1} p && /root_need=\$\(\(/{print "root"; exit} p && /with_esp \|\| refuse "no-esp"/{print "esp"; exit}' "$CODE" | pipe_has -x root; then
    ok "the root-space check runs before the ESP is even mounted"
else
    bad "the root-space check runs after with_esp — reorder so it does not depend on ESP state"
fi

# 5. A LUKS root must still find its ESP.
if grep -q 'lsblk -ndo TYPE' "$CODE"; then
    ok "root_disk walks up to a real disk (LUKS roots have a partition parent)"
else
    bad "root_disk takes one PKNAME — every encrypted machine would refuse no-esp"
fi

# and the Secure Boot state is read, not inferred from mokutil being installed
if grep -q 'SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c' "$CODE"; then
    ok "Secure Boot is read out of efivarfs"
else
    bad "Secure Boot is inferred from a tool's presence — absent tool reads as 'off'"
fi
grep -q 'secure-boot-unknown' "$CODE" \
    && ok "an unreadable SecureBoot variable is a refusal, not a guess" \
    || bad "an unreadable SecureBoot variable is treated as 'off'"

# ═════════════════════════════════════════════════════════════════════════════
sec "Rime never writes Windows' ESP — behavioural, both ways"
# Not a grep. These RUN the engine against two fixture ESPs that differ by one
# file and compare what it decides, so the assertion fails if the refusal is
# deleted AND fails if it is made unconditional. A grep for the refusal's name
# would pass in the second case, which is the failure mode this repo keeps
# finding: a gate that runs and inspects nothing.
#
# `precheck --explain` is what makes this possible without root or loopback.
# It evaluates every check instead of stopping at the first refusal, so an
# unprivileged runner gets past `not-root` and reaches the ESP checks, and
# RIME_MIGRATE_ESP points the ESP content test at a directory. The runner is
# not root, has no /dev/loop-control and may not be on UEFI at all — none of
# which this needs.
explain_with_esp() {   # $1 = fixture ESP dir; prints the decision table
    RIME_MIGRATE_STATE="$TMP/state" \
    RIME_MIGRATE_ROOT="$TMP/sysroot" \
    RIME_MIGRATE_ESP="$1" \
    RIME_MIGRATE_FAKEROOT="$TMP/fakeroot" \
    RIME_MIGRATE_STORE=ostreeContainer \
        bash "$MIG" precheck --explain 2>&1
}

mkdir -p "$TMP/esp-windows/EFI/Microsoft/Boot" "$TMP/esp-own/EFI/BOOT" \
         "$TMP/fakeroot/usr/lib/modules"
: > "$TMP/esp-windows/EFI/Microsoft/Boot/bootmgfw.efi"
: > "$TMP/esp-own/EFI/BOOT/BOOTX64.EFI"

# `|| true`: a refusing precheck exits 10 by design, and under `set -e` the
# assignment itself would abort this suite before a single assertion ran.
win_table="$(explain_with_esp "$TMP/esp-windows" || true)"
own_table="$(explain_with_esp "$TMP/esp-own" || true)"

# The two fixtures differ by exactly one file, so if the verdict does not
# differ the check is not reading the ESP at all.
if grep -qE '^REFUSE +esp-is-windows' <<<"$win_table"; then
    ok "an ESP carrying bootmgfw.efi is REFUSED (esp-is-windows)"
else
    bad "an ESP carrying Windows' loader was allowed — Rime must never write it"
fi
if grep -qE 'esp-is-windows' <<<"$own_table"; then
    bad "esp-is-windows fires on an ESP with no Windows loader — it is unconditional, so it proves nothing"
else
    ok "an ESP with no Windows loader raises no esp-is-windows verdict"
fi

# The refusal must be a REFUSE and not a NOTE. A note would let `rime update`
# carry straight on into the write, which is the whole thing being prevented.
if grep -qE '^NOTE +esp-is-windows' <<<"$win_table"; then
    bad "esp-is-windows is a NOTE — the migration would proceed onto Windows' ESP anyway"
else
    ok "esp-is-windows is a refusal, not an advisory note"
fi

# A refusal has to be clearable, or it strands every dual-boot machine forever
# — the argument that made BitLocker a note. The remedy has to be stated.
if grep -q 'esp-is-windows' "$CODE" && \
   awk '/refuse "esp-is-windows"/{p=1} p{print} p && /^        fi/{exit}' "$CODE" \
     | pipe_has 'ESP of its OWN'; then
    ok "the esp-is-windows refusal names its remedy (an ESP of Rime's own)"
else
    bad "the refusal states no remedy, so a user cannot act on it"
fi

# --explain must evaluate everything. If it ever short-circuits, every
# assertion above silently stops testing what it says it tests.
win_checks="$(grep -cE '^(OK|NOTE|REFUSE) ' <<<"$win_table" || true)"
if [[ "$win_checks" -ge 8 ]]; then
    ok "precheck --explain evaluates every check ($win_checks verdicts past the first refusal)"
else
    bad "precheck --explain stopped early ($win_checks verdicts) — it is short-circuiting again"
fi

# The decision doc's claim, pinned as an assertion so it cannot drift back:
# RIME_MIGRATE_ESP does NOT steer bootc, and this file must not say it does.
if grep -q 'overridden' "$CODE"; then
    bad "a verdict still calls RIME_MIGRATE_ESP an override of the write — it only moves the measurement"
else
    ok "nothing claims RIME_MIGRATE_ESP steers where bootc writes"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "A half-finished composefs deployment is refused — behavioural, both ways"
# `bootc install to-existing-root --composefs-backend` is not idempotent.
# Measured 2026-09-22 on the real image: an attempt whose install succeeded and
# whose join_state then failed left $SYSROOT/state and $SYSROOT/composefs
# behind, and every later attempt died INSIDE bootc with
#   "Setting up composefs boot: Writing composefs state:
#    Failed to create symlink for /var: File exists (os error 17)"
# while this engine's own message said "Re-running is safe".
# ROADMAP/evidence/sdboot-migrate-3-20260922-lab.md, section 5.
#
# Three fixtures, differing only in what is on the fake sysroot and what phase
# is recorded, so the refusal cannot pass by being unconditional and cannot
# pass by being deleted.
explain_with_root() {   # $1 = fixture sysroot, $2 = fixture state dir
    RIME_MIGRATE_STATE="$2" \
    RIME_MIGRATE_ROOT="$1" \
    RIME_MIGRATE_ESP="$TMP/esp-own" \
    RIME_MIGRATE_FAKEROOT="$TMP/fakeroot" \
    RIME_MIGRATE_STORE=ostreeContainer \
        bash "$MIG" precheck --explain 2>&1
}
mkdir -p "$TMP/sr-clean" "$TMP/st-clean" \
         "$TMP/sr-partial/composefs" "$TMP/sr-partial/state" "$TMP/st-partial" \
         "$TMP/st-staged"
echo staged > "$TMP/st-staged/phase"
pi_clean="$(explain_with_root "$TMP/sr-clean"   "$TMP/st-clean"  || true)"
pi_part="$( explain_with_root "$TMP/sr-partial" "$TMP/st-partial" || true)"
pi_staged="$(explain_with_root "$TMP/sr-partial" "$TMP/st-staged" || true)"

if grep -qE '^REFUSE +partial-install' <<<"$pi_part"; then
    ok "a leftover composefs deployment with no phase recorded is REFUSED (partial-install)"
else
    bad "a half-finished composefs deployment is allowed through — the next run dies inside bootc with 'File exists'"
fi
# `^REFUSE +partial-install`, anchored, not a bare substring: the passing
# verdict is named `no-partial-install` and a loose grep matches that too —
# which it did, on the first run of this assertion.
if grep -qE '^REFUSE +partial-install' <<<"$pi_clean"; then
    bad "partial-install fires on a sysroot with no leftovers — it is unconditional, so it proves nothing"
else
    ok "a sysroot with no leftovers raises no partial-install verdict"
fi
# The phase guard is the part that makes this safe to ship: a STAGED migration
# has a composefs deployment on purpose, and refusing there would break the
# one case the predecessor built the state machine for — a power cut between
# the stage and the commit.
if grep -qE '^REFUSE +partial-install' <<<"$pi_staged"; then
    bad "partial-install fires on a STAGED migration — it would refuse the machine its own completed stage"
else
    ok "partial-install ignores a staged migration's composefs deployment"
fi
# A refusal nobody can act on strands the machine, which is the whole complaint
# against the message this replaces.
if grep -A 20 'refuse "partial-install"' "$CODE" | pipe_has 'by hand'; then
    ok "the partial-install refusal says what to do about it"
else
    bad "partial-install states no remedy, so a user is stuck exactly as before"
fi
# And the message it replaces must be gone: "Re-running is safe" full stop was
# false, and a reader who believed it re-ran forever.
if grep -q 'machine still boots the way it did. Re-running is safe."' "$CODE"; then
    bad "install-failed still promises 'Re-running is safe' without qualification — measured false"
else
    ok "install-failed no longer promises an unqualified 'Re-running is safe'"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
