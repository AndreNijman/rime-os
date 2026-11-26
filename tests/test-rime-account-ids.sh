#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-account-ids.sh — a system account whose ID an image update gave
#  to a different account gets a free ID back, and nothing else moves.
#
#  Drives files/system/libexec/rime-account-ids against a fake root
#  (RIME_ACCOUNT_IDS_ROOT) shaped like the katana on 2026-11-26: rtkit 976 in
#  /etc/passwd vs systemd-oom 976 in /usr/lib/passwd, geoclue 977 vs
#  systemd-coredump 977, the shipped defaults (/usr/etc) numbering them 974 and
#  973, and a hand-made account already on 960. No root needed: re-owning
#  files is reported (RIME_ACCOUNT_IDS_NO_CHOWN), not performed.
#
#  The failures it exists to catch:
#    1. a colliding account left colliding (rtkit hangs: no portal);
#    2. a new ID that is itself taken, or an account split from its group;
#    3. a person, root, or an account that does NOT collide being renumbered;
#    4. the files rewritten when nothing collides, or on a second run;
#    5. a primary group left pointing at the old number;
#    6. files re-owned outside the account's own /var/lib home.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="$REPO/files/system/libexec/rime-account-ids"
UNIT="$REPO/files/system/units/rime-account-ids.service"
BASE="$REPO/Containerfile.base"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }

for f in "$TOOL" "$UNIT" "$BASE"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fixture() {
    R="$WORK/root"; rm -rf "$R"
    mkdir -p "$R/etc" "$R/usr/lib" "$R/usr/etc" "$R/var/lib/geoclue/sub" "$R/var/lib/other"
    cat > "$R/etc/passwd" <<'EOF'
root:x:0:0:Super User:/root:/bin/bash
rtkit:x:976:976:RealtimeKit:/:/usr/sbin/nologin
geoclue:x:977:977::/var/lib/geoclue:/usr/sbin/nologin
ghrunner:x:960:960:GitHub Actions self-hosted runner:/var/lab/runner:/usr/sbin/nologin
greetd:x:973:973:greetd daemon:/var/lib/greetd:/usr/sbin/nologin
andre:x:1000:1000:Andre:/var/home/andre:/bin/zsh
EOF
    cat > "$R/etc/group" <<'EOF'
root:x:0:
wheel:x:10:andre
rtkit:x:976:
geoclue:x:977:
ghrunner:x:960:
greetd:x:973:
andre:x:1000:
EOF
    cat > "$R/usr/lib/passwd" <<'EOF'
root:x:0:0:Super User:/root:/bin/bash
systemd-oom:x:976:976:systemd Userspace OOM Killer:/:/usr/sbin/nologin
systemd-coredump:x:977:977:systemd Core Dumper:/:/usr/sbin/nologin
polkitd:x:999:999:User for polkitd:/:/sbin/nologin
EOF
    cat > "$R/usr/lib/group" <<'EOF'
root:x:0:
wheel:x:10:
systemd-oom:x:976:
systemd-coredump:x:977:
polkitd:x:999:
EOF
    cat > "$R/usr/etc/passwd" <<'EOF'
rtkit:x:974:974:RealtimeKit:/:/usr/sbin/nologin
geoclue:x:973:973::/var/lib/geoclue:/usr/sbin/nologin
EOF
    cat > "$R/usr/etc/group" <<'EOF'
rtkit:x:974:
geoclue:x:973:
EOF
    touch "$R/var/lib/geoclue/sub/data" "$R/var/lib/other/file"
}
run() { RIME_ACCOUNT_IDS_ROOT="$R" RIME_ACCOUNT_IDS_NO_CHOWN=1 python3 "$TOOL" "$@" > "$WORK/out" 2>&1; }
field() { awk -F: -v n="$2" -v k="$3" '$1==n{print $k}' "$R/$1"; }

sec "--check on a drifted machine"
fixture
cp -a "$R/etc" "$WORK/etc.before"
run --check; rc=$?
[[ $rc -eq 1 ]] && ok "exit 1 when there is drift" || bad "exit 1 when there is drift" "rc=$rc"
diff -r -x .pwd.lock "$WORK/etc.before" "$R/etc" >/dev/null && ok "--check writes nothing" || bad "--check writes nothing"
grep -q 'user rtkit: 976 -> 974' "$WORK/out" && ok "plan names rtkit" || bad "plan names rtkit" "$(cat "$WORK/out")"

sec "repair"
run; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc: $(cat "$WORK/out")"
[[ "$(field etc/passwd rtkit 3)" == 974 ]] && ok "rtkit takes its shipped default, 974" || bad "rtkit takes its shipped default, 974" "$(field etc/passwd rtkit 3)"
[[ "$(field etc/passwd rtkit 4)" == 974 && "$(field etc/group rtkit 3)" == 974 ]] \
    && ok "rtkit's group moves with it" || bad "rtkit's group moves with it"
g=$(field etc/passwd geoclue 3)
[[ "$g" != 977 && "$g" != 973 ]] && ok "geoclue leaves 977, and does not take greetd's 973" || bad "geoclue leaves 977, and does not take greetd's 973" "$g"
[[ "$g" == 201 ]] && ok "geoclue takes the lowest free dynamic ID (201), far from where sysusers allocates" || bad "geoclue takes the lowest free dynamic ID (201), far from where sysusers allocates" "$g"
[[ "$(field etc/group geoclue 3)" == "$g" && "$(field etc/passwd geoclue 4)" == "$g" ]] \
    && ok "geoclue's group and primary group match" || bad "geoclue's group and primary group match"
for n in root ghrunner greetd andre; do
    [[ "$(grep "^$n:" "$R/etc/passwd")" == "$(grep "^$n:" "$WORK/etc.before/passwd")" ]] \
        && ok "$n untouched" || bad "$n untouched"
done
grep -q '^wheel:x:10:andre$' "$R/etc/group" && ok "wheel and its members untouched" || bad "wheel and its members untouched"
dups=$(cat "$R/etc/passwd" "$R/usr/lib/passwd" | cut -d: -f1,3 | sort -u | cut -d: -f2 | sort | uniq -d)
[[ -z "$dups" ]] && ok "no UID is shared by two names any more" || bad "no UID is shared by two names any more" "$dups"
dups=$(cat "$R/etc/group" "$R/usr/lib/group" | cut -d: -f1,3 | sort -u | cut -d: -f2 | sort | uniq -d)
[[ -z "$dups" ]] && ok "no GID is shared by two names any more" || bad "no GID is shared by two names any more" "$dups"
[[ $(wc -l < "$R/etc/passwd") -eq $(wc -l < "$WORK/etc.before/passwd") ]] && ok "passwd line count unchanged" || bad "passwd line count unchanged"
[[ $(wc -l < "$R/etc/group") -eq $(wc -l < "$WORK/etc.before/group") ]] && ok "group line count unchanged" || bad "group line count unchanged"
b=$(find "$R/var/lib/rime/account-ids" -name passwd 2>/dev/null | head -1)
[[ -n "$b" ]] && cmp -s "$b" "$WORK/etc.before/passwd" && ok "the original passwd is kept" || bad "the original passwd is kept"
tmp=("$R"/etc/*.rime-account-ids.*)
[[ -e "${tmp[0]}" ]] && bad "no temporary file left in /etc" "${tmp[*]}" || ok "no temporary file left in /etc"

sec "second run"
cp -a "$R/etc" "$WORK/etc.after"
run; rc=$?
[[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc"
diff -r -x .pwd.lock "$WORK/etc.after" "$R/etc" >/dev/null && ok "nothing rewritten" || bad "nothing rewritten"
[[ -s "$WORK/out" ]] && bad "and nothing said" "$(cat "$WORK/out")" || ok "and nothing said"
run --check; [[ $? -eq 0 ]] && ok "--check exits 0 once repaired" || bad "--check exits 0 once repaired"

# Re-owning needs real ownership: files owned by the OLD ID, made with sudo
# (GitHub's runners and the build hosts have it non-interactively).
sec "re-owning the home (as root)"
if sudo -n true 2>/dev/null; then
    fixture
    sudo chown -R 977:977 "$R/var/lib/geoclue"
    sudo chown 977:977 "$R/var/lib/other/file"
    # rtkit's home is /: a walk of it would re-own this too.
    touch "$R/var/lib/other/rtkit-owned"; sudo chown 976:976 "$R/var/lib/other/rtkit-owned"
    out=$(sudo env RIME_ACCOUNT_IDS_ROOT="$R" python3 "$TOOL" 2>&1); rc=$?
    printf '%s\n' "$out" > "$WORK/out"
    [[ $rc -eq 0 ]] && ok "exit 0" || bad "exit 0" "rc=$rc: $(cat "$WORK/out")"
    g=$(field etc/passwd geoclue 3)
    [[ "$(stat -c %u:%g "$R/var/lib/geoclue/sub/data")" == "$g:$g" ]] && ok "a file in geoclue's home follows it to $g"         || bad "a file in geoclue's home follows it to $g" "$(stat -c %u:%g "$R/var/lib/geoclue/sub/data")"
    [[ "$(stat -c %u:%g "$R/var/lib/geoclue")" == "$g:$g" ]] && ok "the home directory itself follows it" || bad "the home directory itself follows it"
    [[ "$(stat -c %u:%g "$R/var/lib/other/file")" == 977:977 ]] && ok "a file outside the home keeps its owner" || bad "a file outside the home keeps its owner"
    [[ "$(stat -c %u:%g "$R/var/lib/other/rtkit-owned")" == 976:976 ]] && ok "rtkit's home is /, so nothing is walked for it" \
        || bad "rtkit's home is /, so nothing is walked for it" "$(stat -c %u:%g "$R/var/lib/other/rtkit-owned")"
    [[ "$(stat -c %a "$R/etc/passwd")" == "$(stat -c %a "$WORK/etc.before/passwd")" ]] && ok "passwd keeps its mode" || bad "passwd keeps its mode"
    sudo rm -rf "$R"
else
    printf 'SKIP  re-owning needs non-interactive sudo\n'
fi

sec "an ID free as a UID but taken as a GID"
fixture
printf 'render:x:974:\n' >> "$R/usr/lib/group"
run; rc=$?
u=$(field etc/passwd rtkit 3); gg=$(field etc/group rtkit 3)
[[ "$u" != 974 && "$gg" != 974 ]] && ok "rtkit does not take 974 (render's GID)" || bad "rtkit does not take 974 (render's GID)" "$u/$gg"
[[ "$u" == "$gg" && "$(field etc/passwd rtkit 4)" == "$u" ]] && ok "rtkit keeps one number for user and group" || bad "rtkit keeps one number for user and group" "$u/$gg"

sec "a clean machine"
fixture
sed -i 's/^rtkit:x:976:976:/rtkit:x:974:974:/; s/^geoclue:x:977:977:/geoclue:x:972:972:/' "$R/etc/passwd"
sed -i 's/^rtkit:x:976:/rtkit:x:974:/; s/^geoclue:x:977:/geoclue:x:972:/' "$R/etc/group"
cp -a "$R/etc" "$WORK/etc.clean"
run; rc=$?
[[ $rc -eq 0 ]] && diff -r -x .pwd.lock "$WORK/etc.clean" "$R/etc" >/dev/null && ok "nothing to do: nothing written" || bad "nothing to do: nothing written"
[[ -d "$R/var/lib/rime/account-ids" ]] && bad "no backup directory made" || ok "no backup directory made"

sec "never root or a person"
fixture
printf 'nobody2:x:1000:1000::/:/bin/sh\n' >> "$R/usr/lib/passwd"
run; rc=$?
[[ "$(field etc/passwd andre 3)" == 1000 ]] && ok "a person colliding with the image is left alone" || bad "a person colliding with the image is left alone"
grep -q 'NOT repairing andre' "$WORK/out" && ok "and it says so" || bad "and it says so"

sec "not an altfiles image"
fixture; rm "$R/usr/lib/passwd"
cp -a "$R/etc" "$WORK/etc.noalt"
run; rc=$?
[[ $rc -eq 0 ]] && diff -r -x .pwd.lock "$WORK/etc.noalt" "$R/etc" >/dev/null && ok "no /usr/lib/passwd: nothing done" || bad "no /usr/lib/passwd: nothing done"

sec "wiring"
grep -q '^ExecStart=/usr/libexec/rime-account-ids$' "$UNIT" && ok "unit runs the helper" || bad "unit runs the helper"
grep -q '^Before=.*sysinit.target' "$UNIT" && ok "unit runs before sysinit.target" || bad "unit runs before sysinit.target"
grep -q '^Before=.*rtkit-daemon.service' "$UNIT" && ok "unit runs before rtkit-daemon" || bad "unit runs before rtkit-daemon"
grep -q 'COPY --chmod=0755 files/system/libexec/rime-account-ids /usr/libexec/rime-account-ids' "$BASE" \
    && ok "image installs the helper" || bad "image installs the helper"
grep -q 'systemctl enable rime-account-ids.service' "$BASE" && ok "image enables the unit" || bad "image enables the unit"
if command -v systemd-analyze >/dev/null; then
    # The helper is not installed on the host running this; that line only.
    out=$(systemd-analyze verify --man=no "$UNIT" 2>&1 | grep -v -F 'Command /usr/libexec/rime-account-ids is not executable')
    [[ -z "$out" ]] && ok "systemd-analyze verify" || bad "systemd-analyze verify" "$out"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail -eq 0 ]]
