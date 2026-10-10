#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  boot-v2 shared helpers — firmware discovery, ESP authoring, guest launch.
#
#  Sourced by the scripts beside it. Everything here operates on files inside a
#  work directory the caller names; nothing in this directory may read or write
#  the host's ESP, `/boot`, `/boot/efi`, `/efi` or EFI variables. See
#  AGENTS.md "Touching a machine's boot path" — the katana is a real Rime
#  machine and the build box at the same time, so a guest ESP is always an
#  image file and `--esp-path`-style arguments are always explicit.
#
#  Two things in here look like over-engineering and are not:
#
#  * Firmware paths are DISCOVERED and the discovery hard-fails. Fedora has
#    moved OVMF between /usr/share/edk2/ovmf and /usr/share/edk2/x64 and
#    between 2 MB `.fd` and 4 MB `.qcow2` layouts across releases;
#    signing/spike-d/boot-sb-vm.sh still hardcodes
#    /usr/share/edk2/x64/OVMF_CODE.secure.4m.fd, which does not exist in the
#    boot lab at all. A wrong path must say so, not produce a confusing qemu
#    error twenty lines later.
#
#  * A guest run is only "successful" when qemu exited 0 under -no-reboot AND
#    the expected marker is in the serial log. Timeout-kill plus a marker means
#    the guest printed and then hung, which §22 explicitly refuses to count:
#    "Do not mark an update successful merely because the kernel started."
# ─────────────────────────────────────────────────────────────────────────────

# Callers set -euo pipefail themselves; this file must be safe to source under it.

BOOTV2_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export BOOTV2_LIB_DIR

log()  { printf '\n>>> %s\n' "$*" >&2; }
info() { printf '    %s\n' "$*" >&2; }
die()  { printf '!!! %s\n' "$*" >&2; exit 1; }

# ── firmware ────────────────────────────────────────────────────────────────
# Two OVMF variants matter and they are not interchangeable:
#   secboot  — built with SECURE_BOOT_ENABLE and SMM_REQUIRE. Verifies every
#              LoadImage against db. This is the only one that can prove a
#              signing chain, and the only one worth using here.
#   plain    — no verification. Kept out of these scripts deliberately: a run
#              that silently fell back to it would "pass" the signed-UKI
#              scenario while proving nothing.
# virt-fw-vars edits raw `.fd` varstores, so the raw pair is what we need;
# Fedora 43 ships only qcow2 for the 4 MB layout.
# ── the 4 MB firmware, and why the 2 MB pair is not good enough ────────────
#
# MEASURED, 2026-09-03, edk2-ovmf-20260508-7.fc43:
#
#   /usr/share/edk2/ovmf/OVMF_CODE.secboot.fd        (2 MB, raw)
#       Secure Boot works. TCG2 does NOT: the OVMF debug log contains no Tcg2
#       activity, sd-stub sets no StubPcr* EFI variables, and PCR 11 in the
#       guest reads as 64 zeros. systemd-cryptsetup then says "No signature
#       for current PCR policy in TPM2 signature JSON" and every TPM unlock
#       fails — including the ones that should succeed.
#
#   /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2  (4 MB, qcow2)
#       367 Tcg2 lines in the debug log, PCR 11 extended to a real value, the
#       .pcrsig and .pcrpkey sections delivered to
#       /run/systemd/tpm2-pcr-{signature.json,public-key.pem}.
#
# So measured boot needs the 4 MB build, and Fedora 43 ships only qcow2 for
# it — while virt-fw-vars edits raw varstores. Both are therefore converted to
# raw ONCE into a cache directory and reused. This is the whole reason the lab
# does not simply use the raw 2 MB pair, and finding it cost a full LUKS
# scenario reporting six failures that were all one missing firmware feature.
BOOTV2_FW_CACHE="${RIME_BOOTLAB_FW:-${TMPDIR:-/tmp}/rime-bootlab-fw}"

# _ovmf_raw CODE|VARS — echo a raw firmware image path, converting if needed.
_ovmf_raw() {
    local kind="$1" out src
    mkdir -p "$BOOTV2_FW_CACHE"
    case "$kind" in
        CODE) out="$BOOTV2_FW_CACHE/OVMF_CODE_4M.secboot.fd"
              src=/usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 ;;
        # The PRISTINE (key-free) 4 MB varstore, not the .secboot one — see the
        # comment on ovmf_vars_template below.
        VARS) out="$BOOTV2_FW_CACHE/OVMF_VARS_4M.fd"
              src=/usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2 ;;
        *) die "_ovmf_raw: unknown kind $kind" ;;
    esac
    if [[ ! -s "$out" ]]; then
        [[ -f "$src" ]] || return 1
        qemu-img convert -O raw "$src" "$out" >&2 || return 1
    fi
    printf '%s\n' "$out"
}

# The Secure Boot firmware. Fails hard rather than silently falling back to a
# build without TCG2: a run that quietly lost measured boot would report the
# LUKS scenarios as broken policy rather than as missing firmware, which is
# exactly the misdiagnosis this comment exists to prevent.
ovmf_code_secboot() {
    local c
    if c="$(_ovmf_raw CODE)"; then printf '%s\n' "$c"; return 0; fi
    # shellcheck disable=SC2043  # a one-entry search path, written as a list
    # because that is what it is: the next distribution that moves the 4 MB
    # build adds a line here. Collapsing it to an `if` would make adding one a
    # rewrite instead of a line.
    for c in /usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd; do
        [[ -f "$c" ]] && { printf '%s\n' "$c"; return 0; }
    done
    die "no 4 MB Secure Boot OVMF found. The 2 MB OVMF_CODE.secboot.fd is
    deliberately NOT used as a fallback: it has no TCG2 protocol, so sd-stub
    cannot measure and every TPM-bound unlock fails for a reason that looks
    like a broken PCR policy."
}

# ovmf_build_id FILE — the edk2 revision this firmware binary was built from.
#
# PROVENANCE IS READ OUT OF THE BINARY, NEVER OFF THE FILENAME. The
# alternate-firmware path below takes an operator-supplied file, and this
# unit's own scratch directory already holds the trap it exists to catch: a
# build with Secure Boot and SMM compiled OUT, saved under the name
# `OVMF_CODE_4M.secboot.fd`. A scenario that trusted that name would report
# "the firmware code changed" for a run in which Secure Boot had been switched
# off instead, which is two variables moving and a conclusion about neither.
#
# The revision appears in the uncompressed SEC firmware volume, inside the
# build path the rpm was compiled under
# (/builddir/.../edk2-<12 hex>/Build/OvmfX64/...), so `grep -a` finds it with
# no binutils — which the lab image does not install and must not start to.
# Prints nothing and returns 0 when there is no such string: the caller
# decides what an unidentifiable firmware means.
ovmf_build_id() {
    grep -aoE 'edk2-[0-9a-f]{12}' "$1" 2>/dev/null | head -1 || true
}

# A SECOND Secure Boot firmware build, supplied by whoever runs the lab, for
# the one experiment the shipped lab image cannot perform on its own: changing
# PCR 0.
#
# PCR 0 is the firmware CODE measurement. Moving it needs two firmware images
# that differ in code and in nothing else, and the lab image ships exactly one
# edk2 version — its `OVMF_CODE_4M.fd` and `OVMF_CODE_4M.secboot.qcow2` are the
# same revision built with and without Secure Boot enforcement, so swapping
# those two changes the enforcement as well as the code. A dbx update, which
# the firmware-change scenario does use, moves PCR 7 and never PCR 0.
#
# So the second build is an INPUT, named by $RIME_BOOTLAB_FW_ALT: a raw `.fd`
# or a `.qcow2` (converted here, once, like the discovered pair). It must be a
# Secure Boot build of a different edk2 revision; the scenario that uses it
# asserts both rather than trusting this comment.
#
# Return codes are distinct because the three ways this fails mean different
# things to a reader: 1 nobody asked for one, 2 the path is wrong, 3 the
# conversion failed.
ovmf_code_alt() {
    local p="${RIME_BOOTLAB_FW_ALT:-}"
    [[ -n "$p" ]] || return 1
    [[ -f "$p" ]] || return 2
    case "$p" in
        *.qcow2)
            mkdir -p "$BOOTV2_FW_CACHE"
            local out
            out="$BOOTV2_FW_CACHE/alt-$(basename "${p%.qcow2}").fd"
            if [[ ! -s "$out" ]]; then
                qemu-img convert -O raw "$p" "$out" >&2 || return 3
            fi
            printf '%s\n' "$out" ;;
        *)  printf '%s\n' "$p" ;;
    esac
}

# The PRISTINE template — the one with no keys in it at all.
#
# This is not interchangeable with OVMF_VARS.secboot.fd, and the difference
# cost a debugging round: Fedora's `secboot` variable store ships with Red Hat
# and Microsoft certificates already enrolled as PK/KEK/db. virt-fw-vars'
# --add-db APPENDS, and --no-microsoft only means "do not add more", so
# building on that template leaves a firmware that trusts Microsoft's UEFI CA.
# Every "only Rime-signed images load" assertion would then be false while
# still passing, because a Fedora-signed shim would satisfy db too.
ovmf_vars_template() {
    local v
    if v="$(_ovmf_raw VARS)"; then
        if virt-fw-vars --input "$v" --print 2>/dev/null | grep -E '^name=(PK|db)\b' >/dev/null; then
            die "$v is not pristine — it already has PK/db enrolled"
        fi
        printf '%s\n' "$v"; return 0
    fi
    # shellcheck disable=SC2043  # a one-entry search path; see ovmf_code_secboot.
    for v in /usr/share/edk2/x64/OVMF_VARS.4m.fd; do
        [[ -f "$v" ]] || continue
        virt-fw-vars --input "$v" --print 2>/dev/null | grep -E '^name=(PK|db)\b' >/dev/null && continue
        printf '%s\n' "$v"; return 0
    done
    die "no PRISTINE (key-free) 4 MB OVMF variable-store template found"
}

sd_boot_efi() {
    local p=/usr/lib/systemd/boot/efi/systemd-bootx64.efi
    [[ -f "$p" ]] || die "systemd-bootx64.efi not found at $p (systemd-boot-unsigned missing)"
    printf '%s\n' "$p"
}

sd_stub_efi() {
    local p=/usr/lib/systemd/boot/efi/linuxx64.efi.stub
    [[ -f "$p" ]] || die "linuxx64.efi.stub not found at $p (systemd-boot-unsigned missing)"
    printf '%s\n' "$p"
}

# ── ESP image authoring (no loop devices, no privileges) ────────────────────
#
# The ESP is a real GPT partition inside a disk image rather than a whole-disk
# FAT "superfloppy", and that is load-bearing rather than tidiness:
# systemd-bless-boot resolves the ESP through find_esp_and_warn(), which
# insists on the EFI System Partition GPT type GUID. On a superfloppy it fails
# with "Failed to find ESP" and the whole boot-counting deliverable becomes
# untestable. `parted` writes the GPT into a plain file and the FAT filesystem
# is built separately and dd'd into place, so nothing here needs losetup or
# root inside the container.
esp_disk_create() {
    local disk="$1" esp_mib="${2:-256}"
    local total_mib=$(( esp_mib + 2 ))
    rm -f "$disk" "$disk.esp"
    truncate -s "${total_mib}M" "$disk"
    parted -s "$disk" mklabel gpt \
        mkpart RIMEESP fat32 1MiB "$(( esp_mib + 1 ))MiB" \
        set 1 esp on
    truncate -s "${esp_mib}M" "$disk.esp"
    mkfs.vfat -F 32 -n RIMEESP "$disk.esp" >/dev/null
    # Verify the partition really is an ESP: `set 1 esp on` silently doing
    # nothing would produce a disk that boots but on which bless-boot cannot
    # find the ESP, which is exactly the failure this layout exists to avoid.
    parted -s "$disk" print 2>/dev/null | grep 'esp' >/dev/null \
        || die "GPT partition 1 in $disk is not flagged esp"
}

# Flush the staged FAT image into the partition. Idempotent, and every caller
# that mutates $disk.esp must call this before booting.
esp_disk_flush() {
    local disk="$1"
    [[ -f "$disk.esp" ]] || die "no staged ESP image at $disk.esp"
    dd if="$disk.esp" of="$disk" bs=1M seek=1 conv=notrunc status=none
}

# Pull the partition back out after a boot, so the bootloader's own writes
# (the boot-counting rename) are what we inspect — not our pre-boot staging
# copy. Reading $disk.esp after a run would report the tally we wrote, which is
# the single most likely way this whole deliverable goes green while proving
# nothing.
esp_disk_readback() {
    local disk="$1" out="$2"
    local esp_mib
    esp_mib=$(( ($(stat -c %s "$disk") / 1048576) - 2 ))
    dd if="$disk" of="$out" bs=1M skip=1 count="$esp_mib" conv=notrunc status=none
}

esp_mkdir_p() {
    local esp="$1" path="$2" acc="" part
    # mmd fails on an existing directory, so create each level tolerantly and
    # then assert the leaf exists. This is the narrow exception AGENTS.md
    # allows: a tolerated "already exists" followed by a hard postcondition.
    IFS=/ read -r -a parts <<<"${path#/}"
    for part in "${parts[@]}"; do
        acc="$acc/$part"
        mmd -i "$esp" "::$acc" >/dev/null 2>&1 || true
    done
    mdir -i "$esp" "::$path" >/dev/null 2>&1 \
        || die "could not create $path in $esp"
}

# ── a software TPM the HOST can issue commands to ───────────────────────────
#
# vm_boot's swtpm speaks qemu's unix control protocol, so nothing outside the
# guest can send it a command. The two operations L-001 has to qualify are both
# host-side:
#
#   * a TPM CLEAR. On a real machine TPM2_Clear is issued by the FIRMWARE, from
#     its setup menu, while the platform hierarchy is still enabled — the OS
#     never gets to do it, because firmware disables the platform hierarchy
#     before handing over. An emulator started with `not-need-init` leaves the
#     platform hierarchy enabled with empty auth, so `tpm2_clear -c p` here is
#     the same command the firmware issues, not a stand-in for it.
#   * a RE-ENROLMENT after that clear, which is `systemd-cryptenroll` and is
#     what the user's recovery procedure actually consists of.
#
# Both tools speak the mssim protocol over TCP rather than qemu's unix control
# socket. Same emulator, same state directory — which is the only reason an
# object sealed through one is reachable through the other, and the reason this
# is a session over a directory rather than a second TPM.
#
#   swtpm_session STATE_DIR TAG COMMAND...
#
# COMMAND runs with $SWTPM_TCTI exported. Both `tpm2_*` (via -T "$SWTPM_TCTI")
# and `systemd-cryptenroll --tpm2-device=` accept that exact string.
#
# The clean shutdown at the end is not hygiene. swtpm writes its NV state when
# it exits, and starting qemu's swtpm on a half-written state file loses the
# sealed object — which reaches the guest as an unlock failure indistinguishable
# from a broken PCR policy. rime-luks-enroll learned this the same way.
swtpm_session() {
    local state="$1" tag="$2"; shift 2
    # Not 2321: rime-luks-enroll owns that port and a scenario may run while its
    # emulator is still shutting down. A collision would present as a sealed
    # object that cannot be unsealed, which is the failure this whole file is
    # trying to make legible.
    local port="${RIME_SWTPM_SESSION_PORT:-2381}"
    [[ -d "$state" ]] || die "swtpm_session: no TPM state directory at $state"

    swtpm socket --tpm2 --tpmstate "dir=$state" \
        --server "type=tcp,port=$port,bindaddr=127.0.0.1" \
        --ctrl "type=tcp,port=$((port + 1)),bindaddr=127.0.0.1" \
        --flags not-need-init,startup-clear \
        --log "file=$state/swtpm-$tag.log,level=1" \
        --pid "file=$state/swtpm-$tag.pid" --daemon \
        || die "swtpm failed to start for '$tag' (see $state/swtpm-$tag.log)"

    local rc=0 i
    export SWTPM_TCTI="swtpm:host=127.0.0.1,port=$port"
    set +e
    "$@"
    rc=$?
    set -e

    if [[ -f "$state/swtpm-$tag.pid" ]]; then
        kill "$(cat "$state/swtpm-$tag.pid")" 2>/dev/null || true
        # shellcheck disable=SC2034  # a bounded wait; nothing reads the counter.
        for i in $(seq 1 200); do
            kill -0 "$(cat "$state/swtpm-$tag.pid" 2>/dev/null || echo 0)" 2>/dev/null || break
            sleep 0.05
        done
        rm -f "$state/swtpm-$tag.pid"
    fi
    unset SWTPM_TCTI
    return "$rc"
}

# The independent observation that a TPM CLEAR really happened.
#
# `tpm2_clear` exiting 0 proves a command was accepted, not that the seeds
# rotated — and "the tool ran" standing in for "the state changed" is the defect
# family this repository keeps meeting. Every primary key under the owner
# hierarchy is derived from the Storage Primary Seed, so its NAME is a function
# of the seed. Create one before the clear and one after: a different name is
# the seed having changed, observed through the TPM's own key derivation rather
# than through the exit status of the tool that asked for it.
#
# This is also exactly why the sealed LUKS object stops working: systemd seals
# to an SRK under this same hierarchy, so a new seed means a parent that no
# longer exists.
# The name comes from tpm2_readpublic and NOT from tpm2_createprimary's own
# output. MEASURED 2026-09-14 on tpm2-tools in the lab image: createprimary's
# YAML ends at `sym-mode:` and carries no `name:` line at all, so the obvious
# `createprimary | sed -n 's/^name: *//p'` yields the empty string on a TPM that
# is working perfectly. The first run of luks-tpm-clear reported
# "could not read an owner primary name, so the clear cannot be confirmed"
# against a TPM that had in fact been cleared correctly — a control that fails
# for a reason unrelated to its subject, which is worse than no control,
# because it spends a real failure on a parsing bug.
swtpm_owner_primary_name() {
    local ctx; ctx="$(mktemp)"
    local name=""
    if tpm2_createprimary -T "$SWTPM_TCTI" -C o -g sha256 -G ecc -c "$ctx" >/dev/null 2>&1; then
        name="$(tpm2_readpublic -T "$SWTPM_TCTI" -c "$ctx" 2>/dev/null \
                | sed -n 's/^name: *//p' | tr -d '[:space:]')"
    fi
    rm -f "$ctx"
    # A name is 2 bytes of algorithm plus a sha256 digest: 68 hex characters.
    # Length-checked rather than merely non-empty, because the failure this
    # replaces was a parse that produced "" from a healthy TPM, and a parse that
    # produced a fragment would be the same defect wearing a different mask.
    [[ "$name" =~ ^[[:xdigit:]]{68}$ ]] || name="<unreadable>"
    printf '%s\n' "$name"
}

# ── guest launch ────────────────────────────────────────────────────────────
#
# Returns 0 only on a clean guest-initiated poweroff. Everything else — timeout
# kill, qemu error, firmware refusing to boot anything — is a non-zero return
# and the caller decides whether that was the expected outcome. The serial log
# is the single source of truth for what the guest said; assertions must read
# it and nothing else, because a marker found anywhere in the work directory
# could have come from the host that wrote the artifacts.
#
# `swtpm` is started per run with fresh state. A software TPM is what makes
# measured boot and TPM-bound LUKS2 testable at all; an untested TPM policy is
# worse than none, because it fails on the user's machine at the moment they
# cannot get a shell.
vm_boot() {
    local disk="" vars="" name="run" timeout=120 mem=2048 smp=2
    local tpm=0 serial="" accel="kvm:tcg" tpm_state="" s3=0 code_override=""
    # An ARRAY, because the firmware-change scenario needs two: the volume under
    # test on /dev/vdb and the by-value control volume on /dev/vdc. Attachment
    # order is the guest's device order, so the array order is load-bearing and
    # the guest probe names the devices rather than guessing.
    local -a extra_disks=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --disk)       disk="$2"; shift 2;;
            # S3 (suspend-to-RAM) and a QMP monitor, together, because neither
            # is any use alone: qemu's q35 disables S3 by default, and a guest
            # that suspends with nothing able to wake it is a guest that hangs
            # until the timeout kills it. See qmp-wake.py for the waker.
            --s3)         s3=1; shift;;
            # A firmware image other than the discovered secboot one. Only the
            # firmware-update scenario passes this, and it says in its own text
            # what it loses by doing so.
            --code)       code_override="$2"; shift 2;;
            # A swtpm state directory to KEEP across runs. Without it every
            # --tpm boot gets a fresh TPM, which is right for the bootloader
            # scenarios and fatal for the LUKS one: a key sealed to one TPM's
            # SRK cannot be unsealed by a different TPM, and the failure looks
            # exactly like a broken PCR policy.
            --tpm-state)  tpm_state="$2"; tpm=1; shift 2;;
            --vars)       vars="$2"; shift 2;;
            --name)       name="$2"; shift 2;;
            --timeout)    timeout="$2"; shift 2;;
            --mem)        mem="$2"; shift 2;;
            --smp)        smp="$2"; shift 2;;
            --tpm)        tpm=1; shift;;
            --serial)     serial="$2"; shift 2;;
            --extra-disk) extra_disks+=("$2"); shift 2;;
            --accel)      accel="$2"; shift 2;;
            *) die "vm_boot: unknown argument $1";;
        esac
    done
    [[ -f "$disk" ]] || die "vm_boot: no disk image at $disk"
    [[ -f "$vars" ]] || die "vm_boot: no varstore at $vars"
    [[ -n "$serial" ]] || die "vm_boot: --serial is required"

    local code
    if [[ -n "$code_override" ]]; then
        [[ -f "$code_override" ]] || die "vm_boot: no firmware image at $code_override"
        code="$code_override"
    else
        code="$(ovmf_code_secboot)"
    fi
    local tpmdir="" tpm_args=()
    if (( tpm )); then
        if [[ -n "$tpm_state" ]]; then
            tpmdir="$tpm_state"
            mkdir -p "$tpmdir"
        else
            tpmdir="$(dirname "$serial")/tpm-$name"
            rm -rf "$tpmdir"; mkdir -p "$tpmdir"
        fi
        swtpm socket --tpm2 --tpmstate "dir=$tpmdir" \
            --ctrl "type=unixio,path=$tpmdir/sock-$name" \
            --log "file=$tpmdir/swtpm-$name.log,level=1" \
            --pid "file=$tpmdir/swtpm-$name.pid" --daemon \
            || die "swtpm failed to start (see $tpmdir/swtpm-$name.log)"
        # Wait for the control socket rather than sleeping: a fixed sleep is
        # either flaky or slow, and on a 20-core box it is always the wrong
        # number.
        local i
        for i in $(seq 1 100); do
            [[ -S "$tpmdir/sock-$name" ]] && break
            sleep 0.05
        done
        [[ -S "$tpmdir/sock-$name" ]] || die "swtpm control socket never appeared"
        tpm_args=(
            -chardev "socket,id=chrtpm,path=$tpmdir/sock-$name"
            -tpmdev "emulator,id=tpm0,chardev=chrtpm"
            -device "tpm-tis,tpmdev=tpm0"
        )
    fi

    local extra_args=() d
    for d in "${extra_disks[@]}"; do
        [[ -f "$d" ]] || die "vm_boot: no extra disk image at $d"
        extra_args+=(-drive "if=virtio,format=raw,file=$d,media=disk")
    done

    : > "$serial"
    local dbg="${serial%.log}.ovmf.log"
    : > "$dbg"

    # ── S3 and the thing that wakes the guest up ────────────────────────────
    #
    # `-global ICH9-LPC.disable_s3=1` is qemu's q35 default and is what every
    # other scenario wants: a guest that suspends when nobody can wake it is a
    # guest that hangs until the timeout kills it, which reads as a kernel
    # panic. The suspend/resume scenario flips it and supplies a waker.
    #
    # The waker runs on the HOST and talks QMP, so "the machine suspended" is
    # observed by qemu rather than claimed by the guest. That matters: the guest
    # also reports it, and the two observers are independent. A guest that
    # printed "resumed" without ever suspending would be contradicted here.
    local s3_args=(-global ICH9-LPC.disable_s3=1)
    local qmp_sock="" waker_pid="" waker_rec=""
    if (( s3 )); then
        qmp_sock="$(dirname "$serial")/qmp-$name.sock"
        waker_rec="${serial%.log}.wake.json"
        rm -f "$qmp_sock" "$waker_rec"
        s3_args=(
            -global ICH9-LPC.disable_s3=0
            -qmp "unix:$qmp_sock,server=on,wait=off"
        )
        python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/qmp-wake.py" \
            --socket "$qmp_sock" --record "$waker_rec" --timeout "$timeout" \
            >>"$dbg" 2>&1 &
        waker_pid=$!
    fi

    info "booting '$name' (SB enforcing, tpm=$tpm, s3=$s3, timeout=${timeout}s)"
    info "  firmware $code"
    info "  serial   $serial"

    local rc=0
    set +e
    timeout --foreground --signal=KILL "$timeout" \
    qemu-system-x86_64 \
        -machine "q35,smm=on,accel=$accel" \
        -cpu max -m "$mem" -smp "$smp" \
        -global driver=cfi.pflash01,property=secure,value=on \
        "${s3_args[@]}" \
        -drive "if=pflash,unit=0,format=raw,readonly=on,file=$code" \
        -drive "if=pflash,unit=1,format=raw,file=$vars" \
        -drive "if=virtio,format=raw,file=$disk,media=disk" \
        "${extra_args[@]}" \
        "${tpm_args[@]}" \
        -debugcon "file:$dbg" -global isa-debugcon.iobase=0x402 \
        -serial "file:$serial" \
        -display none -nodefaults -no-reboot \
        2>>"$dbg"
    rc=$?
    set -e

    if [[ -n "$waker_pid" ]]; then
        wait "$waker_pid" 2>/dev/null || true
        rm -f "$qmp_sock"
    fi

    if [[ -n "$tpmdir" && -f "$tpmdir/swtpm-$name.pid" ]]; then
        kill "$(cat "$tpmdir/swtpm-$name.pid")" 2>/dev/null || true
        # Wait for it to actually exit before the caller reuses the state
        # directory: swtpm writes its NV state on shutdown, and starting a
        # second instance on a half-written state file is how a sealed object
        # disappears between two boots.
        # shellcheck disable=SC2034  # a bounded wait; nothing reads the counter.
        for i in $(seq 1 100); do
            kill -0 "$(cat "$tpmdir/swtpm-$name.pid" 2>/dev/null || echo 0)" 2>/dev/null || break
            sleep 0.05
        done
        rm -f "$tpmdir/swtpm-$name.pid" "$tpmdir/sock-$name"
    fi
    info "qemu exited rc=$rc (0 = guest powered off; 137 = timeout kill)"
    return "$rc"
}

# ── assertions ──────────────────────────────────────────────────────────────
BOOTV2_PASS=0
BOOTV2_FAIL=0
BOOTV2_CANNOT=0
BOOTV2_CANNOT_WHY=()

ok()   { BOOTV2_PASS=$((BOOTV2_PASS + 1)); printf '  ok   %s\n' "$*" >&2; }
bad()  { BOOTV2_FAIL=$((BOOTV2_FAIL + 1)); printf '  FAIL %s\n' "$*" >&2; }

# The third word, and it is not a convenience.
#
# Some of what L-001 has to qualify depends on the machine the lab runs on —
# whether the guest kernel offers S3, whether a firmware build with the right
# properties exists. A check that could not be PERFORMED is neither a pass nor
# a failure, and this program's standing rule is that COULD-NOT-RUN must never
# be recorded as either. `ok` would buy a green square by redefining the word;
# `bad` would report a laptop's kernel configuration as a Rime defect.
#
# It does not fail the run — a CI machine without S3 has disproved nothing —
# but the summary line says COULD-NOT-RUN in capitals and lists every reason,
# so it cannot be read as green by anyone who reads the line they were given.
#
# UNLESS IT IS ALL THE RUN DID. `cannot` not failing the run is a statement
# about a run that also proved something; a run whose every check was a
# could-not-run proved nothing at all, and bootv2_summary fails that, below.
cannot() {
    BOOTV2_CANNOT=$((BOOTV2_CANNOT + 1))
    BOOTV2_CANNOT_WHY+=("$*")
    printf '  ---- COULD-NOT-RUN  %s\n' "$*" >&2
}

assert_eq() {
    local want="$1" got="$2" what="$3"
    if [[ "$want" == "$got" ]]; then ok "$what == $want"
    else bad "$what: want '$want', got '$got'"; fi
}

# Reads ONLY the named serial log. Never the work directory: the UKI, the ESP
# staging copy and the guest init script all contain the marker string, so a
# grep over the directory would pass without a guest ever running.
assert_serial_has() {
    local serial="$1" marker="$2"
    [[ -f "$serial" ]] || { bad "serial log $serial does not exist"; return; }
    if grep -qF -- "$marker" "$serial"; then ok "serial '$(basename "$serial")' contains $marker"
    else bad "serial '$(basename "$serial")' is missing $marker"; fi
}

assert_serial_lacks() {
    local serial="$1" marker="$2"
    [[ -f "$serial" ]] || { bad "serial log $serial does not exist"; return; }
    if grep -qF -- "$marker" "$serial"; then bad "serial '$(basename "$serial")' unexpectedly contains $marker"
    else ok "serial '$(basename "$serial")' does not contain $marker"; fi
}

# Two files, byte for byte, WITHOUT diffutils.
#
# MEASURED 2026-09-14: the boot lab image ships no `cmp` and no `diff` — see
# bootlab/Containerfile, which installs neither and asserts neither — and the
# firmware-change scenario decided its "the varstore really changed" control
# with `if cmp -s A B; then bad ...; fi`. With no cmp that is exit 127, the
# `if` takes the branch it takes when the files DIFFER, and the control printed
# its ok line whatever the two files held. A control that cannot fail is the
# defect it was written to catch, one level up.
#
# sha256sum is coreutils, so it is present wherever bash is. It also says what
# it compared when it disagrees, which `cmp -s` does not.
files_same() {
    local a b
    a="$(sha256sum <"$1")" || return 2
    b="$(sha256sum <"$2")" || return 2
    [[ "$a" == "$b" ]]
}

bootv2_summary() {
    # A RUN THAT ASSERTED NOTHING IS NOT A PASS.
    #
    # `cannot` deliberately does not fail the run, and `ok`/`bad` are the only
    # things that count — so a run in which every single check was a
    # could-not-run ended 0 passed, 0 failed, exit 0, and every caller that
    # reads an exit status was told the qualification had passed. It had not
    # run. This is the same defect as the missing summary line and the same
    # defect as a control that cannot fail: a gate reporting green for work it
    # never did. An empty run is INCOMPLETE, and INCOMPLETE is a failure.
    if (( BOOTV2_PASS == 0 && BOOTV2_FAIL == 0 )); then
        bad "this run asserted nothing: no check passed and none failed, so there is no result here to read as green"
    fi
    if (( BOOTV2_CANNOT )); then
        printf '\n== %s: %d passed, %d failed, %d COULD-NOT-RUN ==\n' \
            "${1:-boot-v2}" "$BOOTV2_PASS" "$BOOTV2_FAIL" "$BOOTV2_CANNOT" >&2
        local why
        for why in "${BOOTV2_CANNOT_WHY[@]}"; do
            printf '   could-not-run: %s\n' "$why" >&2
        done
    else
        printf '\n== %s: %d passed, %d failed ==\n' "${1:-boot-v2}" "$BOOTV2_PASS" "$BOOTV2_FAIL" >&2
    fi
    (( BOOTV2_FAIL == 0 )) || return 1
}
