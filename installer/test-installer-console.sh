#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-installer-console.sh — the text consoles, after Fedora 45 made kmscon
#  the default one.
#
#  ── What changed under us ──────────────────────────────────────────────────
#
#  Fedora 45's preset enables kmsconvt@.service as the implementation of
#  autovt@ (the name logind starts when someone switches to VT 2..6) and
#  disables getty@. Two consequences for the installer, both checked here:
#
#   1. Its live environment has no kmscon and runs no preset, and Fedora 45's
#      systemd no longer ships autovt@.service as a static link to getty@
#      (Fedora 43's did). So autovt@tty2 resolved to nothing, and the rescue
#      path every failure screen prints ("Ctrl+Alt+F2, login root/rime") was
#      a dead screen. Containerfile.installer now enables getty@.service, which
#      creates the alias, and keeps every name for a console on tty1 masked:
#      getty@tty1, autovt@tty1 and kmsconvt@tty1. The masks are checked where
#      they matter, in a fedora:45 container BOOTED with systemd: systemd 262
#      loads a unit referenced as autovt@tty1 through the alias, as the live
#      getty@ template, and a mask on getty@tty1 alone is then ignored. A
#      symlink check cannot see that; `systemctl show ... -p LoadState` can.
#      kmscon in the live env is refused by the build; that refusal is run
#      against a container that has kmscon installed.
#
#   2. kmscon does not read /etc/vconsole.conf. At start it takes its layout
#      from XKB_DEFAULT_* in its environment if any is set, and only otherwise
#      asks systemd-localed (locale1) for X11Layout/X11Variant/...; with no
#      xkb-* layout option in kmscon.conf it then follows locale1 changes live
#      (kmscon 10, src/main.c + src/misc/dbus.c). On a Rime image the
#      environment comes first: the 50-rime-keymap system-environment
#      generator exports XKB_DEFAULT_* from /etc/X11/xorg.conf.d/00-keyboard.conf
#      into the manager environment, which kmsconvt@ inherits. localed reads
#      that same file. This suite checks the two halves that Rime owns: the
#      engine's own set_locale_keymap_in() writes 00-keyboard.conf into a fake
#      deploy tree, and for each case a real Fedora 45 systemd-localed (in a
#      container, on a private system bus) and the shipped generator, run on
#      that file, must report the same layout and variant. It does NOT start
#      kmscon; the kmscon side is the source reading above plus a manual
#      QEMU run (see the PR).
#
#  ── What it will not do ─────────────────────────────────────────────────────
#
#  No root, no block device, nothing on this machine's bus. The podman legs
#  run against registry.fedoraproject.org/fedora:45 with the fixtures mounted
#  read-only (one `podman run --rm`, and one throwaway image booted with
#  --systemd=always, removed afterwards); they need the Fedora mirrors for dnf.
#  Without podman (or a reachable registry/mirror) they print SKIP, and CI
#  fails a run that skipped them.
#
#  Run from anywhere: ./installer/test-installer-console.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# +e like every suite here: CI runs `bash -e {0}`, and under -e a failing
# assignment would end the run silently, mid-section.
set +e

cd "$(dirname "$0")" || exit 2
ENGINE="$PWD/rime-install"
UNIT="$PWD/rime-installer.service"
CF="$PWD/Containerfile.installer"
ISO="$PWD/build-live-iso.sh"
GEN="$PWD/../files/system/systemd/rime-keymap-env"
for f in "$ENGINE" "$UNIT" "$CF" "$ISO" "$GEN"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done
IMAGE="${RIME_TEST_FEDORA_IMAGE:-registry.fedoraproject.org/fedora:45}"

pass=0; fail=0; skip=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
die() { printf 'FATAL: %s\n' "$1" >&2; exit 2; }
is() {
    local name=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then ok "$name"
    else bad "$name" "want [$want] got [$got]"; fi
}
finish() {
    printf '\ninstaller-console: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
}

W="$(mktemp -d "${TMPDIR:-/tmp}/rime-inst-console.XXXXXX")" || exit 2
BOOT_IMG="localhost/rime-inst-console-boot:$$"
cleanup() {
    [ -n "${BOOT_CID:-}" ] && podman stop -t 2 "$BOOT_CID" >/dev/null 2>&1
    podman image rm -f "$BOOT_IMG" >/dev/null 2>&1
    rm -rf "$W"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# ─────────────────────────────────────────────────────────────────────────────
section "cage owns tty1 against every console that could take it"
# ─────────────────────────────────────────────────────────────────────────────
# One systemd property can be split over several lines; collect them all.
unit_list() {  # $1 = key — every value of that key, one unit per line
    sed -n "s/^$1=//p" "$UNIT" | tr ' ' '\n' | sed '/^$/d'
}
for u in getty@tty1.service kmsconvt@tty1.service; do
    # grep >/dev/null, not -q: under pipefail an early-exiting grep can make
    # the producer die of SIGPIPE and turn a match into a failure.
    if unit_list Conflicts | grep -x "$u" >/dev/null && unit_list After | grep -x "$u" >/dev/null; then
        ok "rime-installer.service conflicts with and orders after $u"
    else
        bad "rime-installer.service conflicts with and orders after $u" \
            "Conflicts: $(unit_list Conflicts | tr '\n' ' ')"
    fi
done
# Naming autovt@tty1 here makes systemd load getty@tty1 through the alias,
# unmasked (the booted leg below shows it). It must not come back.
if unit_list Conflicts | grep -x 'autovt@tty1.service' >/dev/null \
   || unit_list After | grep -x 'autovt@tty1.service' >/dev/null; then
    bad "rime-installer.service does not name autovt@tty1 (it would unmask getty@tty1)"
else
    ok "rime-installer.service does not name autovt@tty1 (it would unmask getty@tty1)"
fi

# The live env's enable/mask block, exactly as the image runs it: the whole
# RUN instruction (first line to the first line without a continuation) that
# sets the default target.
RUNBLOCK="$W/units.sh"
awk '/^RUN / { blk = ""; on = 1 }
     on { blk = blk $0 "\n"; if ($0 !~ /\\$/) { on = 0
          if (blk ~ /systemctl set-default multi-user\.target/) { printf "%s", blk; exit } } }' \
    "$CF" | sed '1s/^RUN //' > "$RUNBLOCK"
n=$(wc -l < "$RUNBLOCK")
if [ "$n" -lt 5 ]; then
    bad "the live env's unit block is extracted from Containerfile.installer" "only $n lines"
    finish; exit 1
fi
ok "the live env's unit block is extracted from Containerfile.installer ($n lines)"
for want in 'systemctl set-default multi-user.target' \
            'systemctl mask getty@tty1.service' 'systemctl mask autovt@tty1.service' \
            'systemctl mask kmsconvt@tty1.service' 'systemctl enable getty@.service' \
            'rm -f /etc/systemd/system/getty.target.wants/getty@tty1.service'; do
    if grep -qF "$want" "$RUNBLOCK"; then ok "the live env runs: $want"
    else bad "the live env runs: $want"; fi
done
# Order matters: `systemctl enable getty@.service` refuses once getty@tty1 is
# masked (measured: "Unit /etc/systemd/system/getty@tty1.service is masked").
l_en=$(grep -nF 'systemctl enable getty@.service' "$RUNBLOCK" | cut -d: -f1 | head -1)
l_mk=$(grep -nF 'systemctl mask getty@tty1.service' "$RUNBLOCK" | cut -d: -f1 | head -1)
if [ -n "$l_en" ] && [ -n "$l_mk" ] && [ "$l_en" -lt "$l_mk" ]; then
    ok "getty@.service is enabled BEFORE getty@tty1 is masked"
else
    bad "getty@.service is enabled BEFORE getty@tty1 is masked" "enable line ${l_en:-none}, mask line ${l_mk:-none}"
fi
if grep -qF 'autovt@tty1 is not masked' "$ISO"; then
    ok "build-live-iso.sh refuses an ISO whose autovt@tty1 is not masked"
else bad "build-live-iso.sh refuses an ISO whose autovt@tty1 is not masked"; fi
if grep -qF 'kmsconvt@tty1 is not masked' "$ISO"; then
    ok "build-live-iso.sh refuses an ISO whose kmsconvt@tty1 is not masked"
else bad "build-live-iso.sh refuses an ISO whose kmsconvt@tty1 is not masked"; fi
if grep -qF 'Ctrl+Alt+F2 would have no login' "$ISO"; then
    ok "build-live-iso.sh refuses an ISO with no autovt@ alias"
else bad "build-live-iso.sh refuses an ISO with no autovt@ alias"; fi
# kmscon is unsupported in the live env, so the ISO accepts only agetty.
if sed -n '/^_autovt=/,/^esac/p' "$ISO" | grep -F 'kmsconvt@.service' >/dev/null; then
    bad "build-live-iso.sh accepts only getty@ as autovt@ (kmscon is refused in the live env)"
else
    ok "build-live-iso.sh accepts only getty@ as autovt@ (kmscon is refused in the live env)"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "the engine writes the operator's layout where localed and 50-rime-keymap read it"
# ─────────────────────────────────────────────────────────────────────────────
# Lifted out of the shipped engine with its dependencies, the same way
# test-installer-locale.sh does (see its notes on why each piece is needed).
FN="$W/fn.sh"
sed -n '/^set_locale_keymap_in()/,/^}/p' "$ENGINE" > "$FN"
sed -n '/^console_keymap_for()/,/^}/p'   "$ENGINE" >> "$FN"
sed -n '/^kbd_has()/,/^}/p'              "$ENGINE" >> "$FN"
for _dep in set_locale_keymap_in console_keymap_for kbd_has; do
    grep -q "^${_dep}()" "$FN" || die "the engine no longer defines ${_dep}(); this suite would measure nothing"
done
FAKE_ETC="$W/fake-etc"; mkdir -p "$FAKE_ETC"
sed -i -e "s|-L /etc/localtime|-L $FAKE_ETC/localtime|g" \
       -e "s|readlink /etc/localtime|readlink $FAKE_ETC/localtime|g" "$FN"
mkdir -p "$W/bin"
for t in localectl timedatectl setfiles; do
    printf '#!/usr/bin/env bash\nexit 1\n' > "$W/bin/$t"; chmod +x "$W/bin/$t"
done
mk_deploy() {  # $1 = case name — a fake deploy root under $W/cases/<name>
    local d="$W/cases/$1"
    mkdir -p "$d/etc" "$d/usr/lib/kbd/keymaps/xkb" "$d/usr/share/zoneinfo/Australia"
    for k in us de uk fr; do : > "$d/usr/lib/kbd/keymaps/xkb/$k.map.gz"; done
    : > "$d/usr/share/zoneinfo/Australia/Perth"
    printf '%s' "$d"
}
run_fn() {  # $1 = deploy  $2 = layout  $3 = variant
    PATH="$W/bin:$PATH" KEYMAP="$2" KEYVARIANT="$3" TIMEZONE="" \
    LOG="$W/engine.log" DEPLOY="$1" \
    bash -c '
        set +u
        have() { command -v "$1" >/dev/null 2>&1; }
        log()  { printf "%s\n" "$*" >> "$LOG"; }
        . '"$FN"'
        set_locale_keymap_in "$DEPLOY"
    ' >/dev/null 2>&1
}
# name | layout | variant — what an operator picks on the keyboard page.
CASES='gb|gb|extd
de|de|nodeadkeys
fr|fr|
us|us|'
while IFS='|' read -r name layout variant; do
    d="$(mk_deploy "$name")"
    run_fn "$d" "$layout" "$variant"
    if [ -s "$d/etc/X11/xorg.conf.d/00-keyboard.conf" ] && [ -s "$d/etc/vconsole.conf" ]; then
        ok "case $name: the engine wrote 00-keyboard.conf and vconsole.conf"
    else
        bad "case $name: the engine wrote 00-keyboard.conf and vconsole.conf" "$(find "$d/etc" 2>&1 | tr '\n' ' ')"
    fi
done <<< "$CASES"

# ─────────────────────────────────────────────────────────────────────────────
section "Fedora 45, for real: autovt@ in the live env, and what kmscon would start from"
# ─────────────────────────────────────────────────────────────────────────────
if ! command -v podman >/dev/null 2>&1; then
    skp "the Fedora 45 container legs" "podman is not installed"
    finish; exit $?
fi

cp "$UNIT" "$W/rime-installer.service"
cp "$GEN" "$W/rime-keymap-env"
cat > "$W/inside.sh" <<'INSIDE'
#!/bin/bash
# Runs inside fedora:45 as root. Prints KEY=VALUE lines the host asserts on.
set -u
dnf -q -y install --setopt=install_weak_deps=False systemd util-linux >/dev/null 2>&1 \
    || { echo "SETUP=dnf-failed"; exit 0; }
# Leg 1: the live env's unit block. Its units must exist to be enabled; the
# installer's own unit is the real file, seatd/NetworkManager are stand-ins.
cp /w/rime-installer.service /usr/lib/systemd/system/rime-installer.service
for u in seatd NetworkManager; do
    printf '[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' \
        > "/usr/lib/systemd/system/$u.service"
done
echo "BEFORE_AUTOVT=$(readlink /etc/systemd/system/autovt@.service /usr/lib/systemd/system/autovt@.service | head -1)"
if sh -e /w/units.sh > /tmp/units.log 2>&1; then echo "UNITS_RC=0"; else echo "UNITS_RC=$?"; sed 's/^/UNITS_LOG=/' /tmp/units.log; fi
echo "AUTOVT=$(readlink /etc/systemd/system/autovt@.service)"
echo "GETTY1=$(readlink /etc/systemd/system/getty@tty1.service)"
echo "AUTOVT1=$(readlink /etc/systemd/system/autovt@tty1.service)"
echo "KMSCON1=$(readlink /etc/systemd/system/kmsconvt@tty1.service)"
if [ -L /etc/systemd/system/getty.target.wants/getty@tty1.service ]; then echo "WANTS1=yes"; else echo "WANTS1=no"; fi
# Leg 2: the installed system's localed, kmscon's own package, and Rime's
# 50-rime-keymap generator, all over the same 00-keyboard.conf.
dnf -q -y install --setopt=install_weak_deps=False dbus-daemon kmscon >/dev/null 2>&1 \
    || { echo "SETUP=dnf2-failed"; exit 0; }
# The live env refuses kmscon: with it installed (its %post preset made
# autovt@ an alias of kmsconvt@), the same block must stop with its own FATAL.
rm -f /etc/systemd/system/autovt@.service
systemctl preset kmsconvt@.service >/dev/null 2>&1
echo "KMSCON_AUTOVT=$(readlink /etc/systemd/system/autovt@.service)"
if sh -e /w/units.sh > /tmp/units2.log 2>&1; then echo "REFUSE_RC=0"; else echo "REFUSE_RC=$?"; fi
if grep -F 'FATAL: kmscon entered the live env' /tmp/units2.log >/dev/null; then echo "REFUSE_MSG=yes"; else echo "REFUSE_MSG=no"; sed 's/^/REFUSE_LOG=/' /tmp/units2.log; fi
# Premise (Fedora's binary, not Rime code): this kmscon has locale1 support.
grep -aq 'org.freedesktop.locale1' /usr/bin/kmscon && echo "KMSCON_LOCALE1=yes" || echo "KMSCON_LOCALE1=no"
xkb=no; for f in /etc/kmscon/*.conf; do [ -f "$f" ] && grep -qE '^[[:space:]]*xkb-(model|layout|variant|options|keymap)[[:space:]]*=' "$f" && xkb="$f"; done
echo "KMSCON_XKB_CONF=$xkb"
mkdir -p /run/dbus
dbus-daemon --system --fork --nopidfile || { echo "SETUP=dbus-failed"; exit 0; }
for c in /cases/*; do
    n="${c##*/}"
    rm -rf /etc/X11/xorg.conf.d /etc/vconsole.conf
    mkdir -p /etc/X11
    [ -d "$c/etc/X11/xorg.conf.d" ] && cp -r "$c/etc/X11/xorg.conf.d" /etc/X11/
    [ -f "$c/etc/vconsole.conf" ] && cp "$c/etc/vconsole.conf" /etc/vconsole.conf
    /usr/lib/systemd/systemd-localed & pid=$!
    v=""
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        v="$(busctl --system get-property org.freedesktop.locale1 /org/freedesktop/locale1 \
               org.freedesktop.locale1 X11Layout X11Variant X11Model X11Options 2>/dev/null | tr '\n' ' ')"
        [ -n "$v" ] && break; sleep 0.5
    done
    echo "LOCALED_$n=$v"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    # What the manager environment (and so kmsconvt@) gets on a Rime image.
    echo "GEN_$n=$(bash /w/rime-keymap-env | tr '\n' ' ')"
done
INSIDE

out="$(podman run --rm \
        -v "$W/inside.sh:/w/inside.sh:ro,Z" -v "$RUNBLOCK:/w/units.sh:ro,Z" \
        -v "$W/rime-installer.service:/w/rime-installer.service:ro,Z" \
        -v "$W/rime-keymap-env:/w/rime-keymap-env:ro,Z" \
        -v "$W/cases:/cases:ro,Z" \
        "$IMAGE" bash /w/inside.sh 2>"$W/podman.err")"
get() { printf '%s\n' "$out" | sed -n "s/^$1=//p" | head -1; }
if [ -z "$out" ] || [ -n "$(get SETUP)" ]; then
    skp "the Fedora 45 container legs" "could not set up ($(get SETUP)$(tail -1 "$W/podman.err"))"
    finish; exit $?
fi

# Leg 1
if [ -z "$(get BEFORE_AUTOVT)" ]; then
    ok "premise: Fedora 45 ships no autovt@ link of its own (the reason for the enable)"
else
    # Not a failure and not a skip: the enable is harmless if Fedora brings
    # the link back, and CI treats any SKIP as "the legs did not run".
    printf 'note  Fedora 45 ships an autovt@ link again (%s); the enable is now belt and braces\n' "$(get BEFORE_AUTOVT)"
fi
is "the live env's unit block runs clean on Fedora 45" "0" "$(get UNITS_RC)"
a="$(get AUTOVT)"
is "autovt@ is agetty in the live env, so Ctrl+Alt+F2 has a login" "getty@.service" "${a##*/}"
is "getty@tty1 is masked on disk in the live env" "/dev/null" "$(get GETTY1)"
is "autovt@tty1 is masked on disk in the live env" "/dev/null" "$(get AUTOVT1)"
is "kmsconvt@tty1 is masked on disk in the live env" "/dev/null" "$(get KMSCON1)"
is "getty.target does not want a console on tty1 in the live env" "no" "$(get WANTS1)"
a="$(get KMSCON_AUTOVT)"
is "premise: kmscon's preset makes autovt@ an alias of kmsconvt@" "kmsconvt@.service" "${a##*/}"
if [ "$(get REFUSE_RC)" != 0 ] && [ "$(get REFUSE_MSG)" = yes ]; then
    ok "with kmscon installed, the live env's block stops with its own FATAL"
else
    bad "with kmscon installed, the live env's block stops with its own FATAL" \
        "rc $(get REFUSE_RC), message $(get REFUSE_MSG): $(get REFUSE_LOG)"
fi

# Leg 1, booted: what systemd actually LOADS for each name of a console on
# tty1 and tty2. This is the check the symlinks above cannot make.
mkdir -p "$W/boot"
cp "$RUNBLOCK" "$W/boot/units.sh"; cp "$UNIT" "$W/boot/rime-installer.service"
cat > "$W/boot/Containerfile" <<BOOTEOF
FROM $IMAGE
RUN dnf -q -y install --setopt=install_weak_deps=False systemd util-linux >/dev/null 2>&1
RUN for u in seatd NetworkManager; do printf '[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n' > /usr/lib/systemd/system/\$u.service; done
COPY rime-installer.service /usr/lib/systemd/system/rime-installer.service
COPY units.sh /units.sh
RUN sh -e /units.sh
BOOTEOF
boot_ok=0
if podman build -q -t "$BOOT_IMG" "$W/boot" >"$W/boot.log" 2>&1; then
    BOOT_CID="$(podman run -d --rm --systemd=always "$BOOT_IMG" /sbin/init 2>>"$W/boot.log")"
    if [ -n "$BOOT_CID" ]; then
        # degraded is fine (no consoles in a container); wait for the boot job.
        timeout 60 podman exec "$BOOT_CID" systemctl is-system-running --wait >/dev/null 2>&1
        boot_ok=1
    fi
fi
if [ "$boot_ok" != 1 ]; then
    skp "the booted Fedora 45 live-env leg" "could not build or boot ($(tail -1 "$W/boot.log"))"
else
    show() {  # $1 = unit $2 = property
        podman exec "$BOOT_CID" systemctl show "$1" -p "$2" --value 2>/dev/null
    }
    for u in getty@tty1 autovt@tty1 kmsconvt@tty1; do
        is "booted: $u.service is LoadState=masked (cage owns tty1)" "masked" "$(show "$u.service" LoadState)"
    done
    is "booted: autovt@tty2 is agetty (Ctrl+Alt+F2 has a login)" "getty@tty2.service" "$(show autovt@tty2.service Id)"
    is "booted: autovt@tty2 loads" "loaded" "$(show autovt@tty2.service LoadState)"
    deps="$(podman exec "$BOOT_CID" systemctl list-dependencies getty.target --plain 2>/dev/null | tr -d ' ' | tr '\n' ' ')"
    case " $deps " in
        *" getty@tty1.service "*|*" autovt@tty1.service "*|*" kmsconvt@tty1.service "*)
            bad "booted: getty.target wants no console on tty1" "$deps" ;;
        *) ok "booted: getty.target wants no console on tty1" ;;
    esac
    podman stop -t 2 "$BOOT_CID" >/dev/null 2>&1; BOOT_CID=""
fi

# Leg 2
is "premise: Fedora 45's kmscon has locale1 support (Fedora's binary, not Rime code)" "yes" "$(get KMSCON_LOCALE1)"
is "Fedora 45's kmscon package ships no xkb-* layout config that would pin the console" "no" "$(get KMSCON_XKB_CONF)"
# localed: `s "gb" s "extd" s "" s "" ` (layout, variant, model, options).
# The generator: XKB_DEFAULT_LAYOUT=gb XKB_DEFAULT_VARIANT=extd (no empty keys).
genval() {  # $1 = generator line  $2 = key
    printf '%s\n' "$1" | tr ' ' '\n' | sed -n "s/^XKB_DEFAULT_$2=//p" | head -1
}
while IFS='|' read -r name layout variant; do
    l="$(get "LOCALED_$name")"
    is "case $name: localed reports layout '$layout' variant '${variant:-none}'" \
       "s \"$layout\" s \"$variant\" s \"\" s \"\" " "$l"
    g="$(get "GEN_$name")"
    gl="s \"$(genval "$g" LAYOUT)\" s \"$(genval "$g" VARIANT)\" s \"$(genval "$g" MODEL)\" s \"$(genval "$g" OPTIONS)\" "
    is "case $name: 50-rime-keymap exports exactly what localed reports (kmscon starts from it)" "$l" "$gl"
done <<< "$CASES"

finish
