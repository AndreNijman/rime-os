#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-apex-shared-machine.sh — guest disposability and kiosk policy
#  (roadmap P2-016, criteria 2 and 3).
#
#  ── What it asserts, and why in this shape ──────────────────────────────────
#
#  Two halves, and they are tested differently on purpose.
#
#  **Criterion 3 is an assertion about what is ABSENT.** "Kiosk/shared-device
#  policy is possible without weakening ordinary desktop use" is a claim about
#  the ORDINARY config, so the test is on the shipped
#  files/desktop/apex-greet/greetd-config.toml: it must carry no auto-login
#  stanza. A kiosk recipe that had leaked into the login path would be the
#  criterion failed, and it would fail silently — the machine would still boot,
#  it would just stop asking who you are. The same grep is then applied to the
#  kiosk recipe, where the stanza must be present in the right section, and to
#  a deliberately mutated copy, which must be caught. An assertion about an
#  absence that never sees the presence is an assertion that cannot fail.
#
#  **Criterion 2 is an assertion about BEHAVIOUR**, and it runs the real
#  engine. /usr/libexec/apex-guest-wipe is executed against a fixture tree —
#  a fixture passwd file, a fixture secretd store, a fixture greeter state dir
#  — as an ordinary user, through the overrides its header documents. So the
#  fences are exercised as shipped rather than re-described here.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#
#  * It creates no account and modifies no account. `getent passwd` on the
#    machine this was written on lists exactly one account at or above uid
#    1000, and the suite must be as true there as on a machine with twenty.
#    Every account it reasons about is a line in a temp file.
#  * It touches nothing outside its temp directory. The engine is pointed at
#    fixture roots for all four of the paths it can remove, and the suite
#    asserts that redirection worked before it asserts anything about wiping —
#    a suite that silently ran the real thing against /var/lib would be the
#    worst possible outcome of a test for a file that deletes directories.
#  * It starts no compositor, no greeter and no session, and opens no window.
#
#      ./tests/test-apex-shared-machine.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# +e deliberately, like every other suite here: CI invokes a suite as
# `bash -e {0}`, and under -e an assignment from a command that exits non-zero
# ends the run silently, mid-section. This suite COUNTS failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }
# There is deliberately no skip helper. This repository's own notes record
# three occasions where a skip became a green tick over nothing asserted.

ENGINE="$ROOT/files/system/libexec/apex-guest-wipe"
GREET_CONF="$ROOT/files/desktop/apex-greet/greetd-config.toml"
KIOSK_CONF="$ROOT/files/system/shared-machine/greetd-kiosk.toml"
KIOSK_SWAY="$ROOT/files/system/shared-machine/sway-kiosk.conf"
ALLOW_SHIPPED="$ROOT/files/system/shared-machine/guest-accounts"
WIPE_UNIT="$ROOT/files/system/units/apex-guest-wipe@.service"

for f in "$ENGINE" "$GREET_CONF" "$KIOSK_CONF" "$KIOSK_SWAY" "$ALLOW_SHIPPED" "$WIPE_UNIT"; do
    [ -f "$f" ] || { echo "FATAL: cannot find $f" >&2; exit 2; }
done
command -v bash >/dev/null 2>&1 || { echo "FATAL: bash is required" >&2; exit 2; }

# ─────────────────────────────────────────────────────────────────────────────
section "the shipped files parse"
# ─────────────────────────────────────────────────────────────────────────────
bash -n "$ENGINE" 2>"$WORK/syntax" \
    && ok "apex-guest-wipe is syntactically valid bash" \
    || bad "apex-guest-wipe is syntactically valid bash" "$(cat "$WORK/syntax")"

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 3 — the ordinary login path is not weakened"
# ─────────────────────────────────────────────────────────────────────────────
#
# greetd's auto-login is the `[initial_session]` section (greetd(5)). Its
# presence in the shipped config would mean APEX had stopped asking who you
# are. The match is anchored at the start of a line and allows leading
# whitespace, so a commented mention — which greetd-kiosk.toml's own prose
# contains — is not read as configuration.
auto_login() { grep -Eq '^[[:space:]]*\[initial_session\]' "$1"; }

if auto_login "$GREET_CONF"; then
    bad "the shipped greeter config has no auto-login stanza" \
        "greetd-config.toml contains [initial_session]"
else
    ok "the shipped greeter config has no auto-login stanza"
fi

# The absence above only means something if this grep can see a presence.
# Without this, deleting the pattern from auto_login() would leave the
# assertion green.
printf '\n[initial_session]\ncommand = "sway"\nuser = "somebody"\n' \
    > "$WORK/leaked.toml"
cat "$GREET_CONF" >> "$WORK/leaked.toml"
if auto_login "$WORK/leaked.toml"; then
    ok "and the check that says so would notice one if it appeared"
else
    bad "and the check that says so would notice one if it appeared" \
        "a config with [initial_session] was not detected"
fi

# A commented mention must NOT count, or the kiosk recipe's own explanation of
# why it avoids the stanza would read as the stanza.
printf '# [initial_session] is deliberately not used here\n' > "$WORK/commented.toml"
if auto_login "$WORK/commented.toml"; then
    bad "a commented mention is not configuration"
else
    ok "a commented mention is not configuration"
fi

# The shipped greeter still authenticates: greetd runs a greeter as the greetd
# user in default_session, which is the whole of "ordinary desktop use".
grep -Eq '^[[:space:]]*\[default_session\]' "$GREET_CONF" \
    && ok "the shipped greeter config still has a default_session (a greeter runs)" \
    || bad "the shipped greeter config still has a default_session (a greeter runs)"

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 3 — the kiosk recipe is a kiosk, and stays a recipe"
# ─────────────────────────────────────────────────────────────────────────────
#
# greetd(5), on the shipped greetd 0.10.3: the initial session "will only be
# executed during the first run of greetd since boot ... checked through the
# presence of the runfile". So a kiosk on [initial_session] drops to a greeter
# the first time its app exits and stays there. The kiosk session belongs in
# [default_session], which greetd restarts whenever no session is running.
if auto_login "$KIOSK_CONF"; then
    bad "the kiosk recipe does not use [initial_session]" \
        "it runs once per boot, so the kiosk would not come back after its app exits"
else
    ok "the kiosk recipe does not use [initial_session] (it runs once per boot)"
fi

grep -Eq '^[[:space:]]*\[default_session\]' "$KIOSK_CONF" \
    && ok "the kiosk session is a default_session, which greetd restarts" \
    || bad "the kiosk session is a default_session, which greetd restarts"

grep -Eq '^[[:space:]]*user[[:space:]]*=' "$KIOSK_CONF" \
    && ok "the kiosk recipe names the account it runs as" \
    || bad "the kiosk recipe names the account it runs as"

# The kiosk boundary is absent configuration, not a lockdown. If the host
# config ever included sway's defaults, every keybinding — terminal, launcher,
# exec — would come back and the kiosk would have a way out of itself.
if grep -Eq '^[[:space:]]*include[[:space:]]' "$KIOSK_SWAY"; then
    bad "the kiosk compositor host includes no system sway config" \
        "an include would restore sway's default keybindings"
else
    ok "the kiosk compositor host includes no system sway config"
fi

# And it must not be installed over the login path by a Containerfile.
#
# COPY lines only, with backslash continuations joined first — a COPY source and
# destination are routinely on two lines. Restricting to COPY matters: a RUN
# line legitimately names both paths to assert they DIFFER, and an earlier draft
# of this check read that assertion as the thing it was asserting against.
copy_lines="$(sed -e ':a' -e '/\\$/{N;s/\\\n//;ba' -e '}' "$ROOT"/Containerfile* 2>/dev/null \
    | grep -E '^[[:space:]]*COPY[[:space:]]')"
if printf '%s\n' "$copy_lines" \
        | grep -E 'shared-machine/greetd-kiosk\.toml' \
        | grep -q '/etc/greetd/config\.toml'; then
    bad "no Containerfile installs the kiosk recipe as the live greetd config"
else
    ok "no Containerfile installs the kiosk recipe as the live greetd config"
fi

# ...and the check can see one, so its silence means something. A COPY line of
# exactly the shape that would turn the login screen off.
probe="$WORK/probe-Containerfile"
printf 'COPY files/system/shared-machine/greetd-kiosk.toml /etc/greetd/config.toml\n' > "$probe"
if grep -E '^[[:space:]]*COPY[[:space:]]' "$probe" \
        | grep -E 'shared-machine/greetd-kiosk\.toml' \
        | grep -q '/etc/greetd/config\.toml'; then
    ok "and that check would notice such a COPY if one appeared"
else
    bad "and that check would notice such a COPY if one appeared"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 2 — the allowlist ships empty, so nothing is wipeable"
# ─────────────────────────────────────────────────────────────────────────────
names="$(grep -vE '^[[:space:]]*(#|$)' "$ALLOW_SHIPPED" | tr -d '[:space:]')"
if [ -z "$names" ]; then
    ok "the shipped /etc/apex/guest-accounts names no account"
else
    bad "the shipped /etc/apex/guest-accounts names no account" "found: $names"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "criterion 2 — the fences, against a fixture tree"
# ─────────────────────────────────────────────────────────────────────────────
#
# A fixture passwd, a fixture store, a fixture greeter state dir, and a stub
# that reports who is logged in. Nothing here is a real account.
FIX="$WORK/fix"
mkdir -p "$FIX"
GUEST_HOME="$FIX/home/apex-guest"
OWNER_HOME="$FIX/home/owner"
STORE="$FIX/var/lib/apex-secretd"
GREET_STATE="$FIX/var/lib/apex-greet"

cat > "$FIX/passwd" <<PASSWD
root:x:0:0:root:/root:/bin/bash
owner:x:1000:1000:The machine owner:${OWNER_HOME}:/bin/bash
apex-guest:x:1500:1500:Guest:${GUEST_HOME}:/bin/bash
badhome:x:1600:1600:Guest with no home:/:/bin/bash
PASSWD

printf 'apex-guest\n' > "$FIX/allow-guest"
printf '# only comments here\n\n' > "$FIX/allow-empty"
printf 'apex-guest\nowner\nroot\nbadhome\n' > "$FIX/allow-everything"

# The "who is logged in" stub. Two variants: nobody, and the guest.
printf '#!/bin/sh\nexit 0\n' > "$FIX/nobody"
printf '#!/bin/sh\necho apex-guest\n' > "$FIX/guest-in"
chmod +x "$FIX/nobody" "$FIX/guest-in"

# Rebuild the fixture state before each behavioural assertion, so one wipe
# never sets up the next one's result.
seed() {
    rm -rf "$FIX/home" "$STORE" "$GREET_STATE"
    mkdir -p "$GUEST_HOME/.config" "$GUEST_HOME/Documents" "$OWNER_HOME/.config"
    printf 'guest secret\n' > "$GUEST_HOME/.config/token"
    printf 'guest doc\n'    > "$GUEST_HOME/Documents/notes.txt"
    printf 'owner data\n'   > "$OWNER_HOME/.config/token"
    mkdir -p "$STORE/users/1500" "$STORE/users/1000"
    printf 'guest credential\n' > "$STORE/users/1500/github.secret"
    printf '{"projects":{"/srv/shared":["github:git.push"]}}\n' > "$STORE/users/1500/grants.json"
    printf 'owner credential\n' > "$STORE/users/1000/github.secret"
    mkdir -p "$GREET_STATE"
    printf 'apex-guest' > "$GREET_STATE/last-user"
}

# Run the engine with every path redirected at the fixture.
wipe() {
    APEX_GUEST_ALLOWLIST="$1" \
    APEX_GUEST_PASSWD="$FIX/passwd" \
    APEX_SECRETD_STORE="$STORE" \
    APEX_GREET_STATE="$GREET_STATE" \
    APEX_GUEST_SESSIONS="${3:-$FIX/nobody}" \
    bash "$ENGINE" "$2" >"$WORK/out" 2>"$WORK/err"
    echo $?
}

# ── the redirection itself, before anything is trusted ───────────────────────
# If the overrides did not take, every assertion below would be running the
# real engine against the real /var/lib on the developer's machine.
seed
rc="$(wipe "$FIX/allow-guest" apex-guest)"
if [ "$rc" = 0 ] && grep -q "$STORE" "$WORK/err"; then
    ok "the engine acted on the FIXTURE store, not the machine's ($STORE)"
else
    bad "the engine acted on the FIXTURE store, not the machine's" \
        "rc=$rc; stderr: $(cat "$WORK/err")"
fi
[ -d /var/lib/apex-secretd/users/1500 ] \
    && bad "nothing was created under the real /var/lib/apex-secretd" \
    || ok "nothing was created under the real /var/lib/apex-secretd"

# ── what a successful wipe actually removes ──────────────────────────────────
seed
rc="$(wipe "$FIX/allow-guest" apex-guest)"
[ "$rc" = 0 ] && ok "the wipe succeeded for an allowlisted, logged-out guest" \
              || bad "the wipe succeeded for an allowlisted, logged-out guest" "rc=$rc: $(cat "$WORK/err")"

[ -e "$GUEST_HOME/Documents/notes.txt" ] \
    && bad "the guest's files are gone" \
    || ok "the guest's files are gone"
# Dotfiles are the half a `rm -rf "$home"/*` silently misses, and they are
# where everything worth wiping lives.
[ -e "$GUEST_HOME/.config/token" ] \
    && bad "the guest's DOTFILES are gone too" \
    || ok "the guest's DOTFILES are gone too"
# The directory itself stays: it is a mount point or a tmpfiles.d line, and
# removing it would take the mount with it.
[ -d "$GUEST_HOME" ] \
    && ok "the home DIRECTORY survives (it is a mount point, not content)" \
    || bad "the home DIRECTORY survives (it is a mount point, not content)"

# THE assertion this whole file exists for. A tmpfs home is not disposability
# on APEX: P0-002 moved credentials out of the home into a root-owned store
# that the account cannot read and that a home wipe does not touch.
[ -e "$STORE/users/1500/github.secret" ] \
    && bad "the guest's PROTECTED CREDENTIAL namespace is gone" \
           "a tmpfs home would have left this for the next guest" \
    || ok "the guest's PROTECTED CREDENTIAL namespace is gone"
[ -e "$STORE/users/1500/grants.json" ] \
    && bad "the guest's per-project GRANTS are gone" \
    || ok "the guest's per-project GRANTS are gone"

# ...and the owner is untouched by all of it.
[ -e "$OWNER_HOME/.config/token" ] \
    && ok "the owner's home is untouched" \
    || bad "the owner's home is untouched"
[ -e "$STORE/users/1000/github.secret" ] \
    && ok "the owner's credential namespace is untouched" \
    || bad "the owner's credential namespace is untouched"

# The greeter forgets the guest, so the next login does not prefill their name.
if [ -s "$GREET_STATE/last-user" ]; then
    bad "the greeter's last-user no longer names the guest" \
        "still: $(cat "$GREET_STATE/last-user")"
else
    ok "the greeter's last-user no longer names the guest"
fi

# ...but a last-user naming somebody else is left alone.
seed
printf 'owner' > "$GREET_STATE/last-user"
wipe "$FIX/allow-guest" apex-guest >/dev/null
[ "$(cat "$GREET_STATE/last-user")" = owner ] \
    && ok "a last-user naming the OWNER is not cleared by a guest wipe" \
    || bad "a last-user naming the OWNER is not cleared by a guest wipe" \
           "became: $(cat "$GREET_STATE/last-user")"

# ── fence 1: the allowlist ───────────────────────────────────────────────────
seed
rc="$(wipe "$FIX/allow-empty" apex-guest)"
[ "$rc" = 2 ] && ok "fence 1: an empty allowlist refuses the guest (exit 2)" \
              || bad "fence 1: an empty allowlist refuses the guest (exit 2)" "rc=$rc"
[ -e "$GUEST_HOME/.config/token" ] \
    && ok "fence 1: and nothing was removed" \
    || bad "fence 1: and nothing was removed"
grep -q "not listed" "$WORK/err" \
    && ok "fence 1: the refusal says which rule refused" \
    || bad "fence 1: the refusal says which rule refused" "$(cat "$WORK/err")"

# A missing allowlist is the same answer as an empty one, not a free pass.
seed
rc="$(wipe "$FIX/no-such-file" apex-guest)"
[ "$rc" = 2 ] && ok "fence 1: a MISSING allowlist refuses too" \
              || bad "fence 1: a MISSING allowlist refuses too" "rc=$rc"

# An unanchored `grep -q "$account"` would let a short name match a longer
# line. The engine matches whole trimmed lines; this is what proves it.
seed
printf 'apex-guest-2\n' > "$FIX/allow-similar"
rc="$(wipe "$FIX/allow-similar" apex-guest)"
[ "$rc" = 2 ] && ok "fence 1: a name that is a SUBSTRING of a listed name is refused" \
              || bad "fence 1: a name that is a SUBSTRING of a listed name is refused" "rc=$rc"

# ── fence 2: the account, and root ───────────────────────────────────────────
seed
rc="$(wipe "$FIX/allow-everything" root)"
[ "$rc" = 2 ] && ok "fence 2: root is refused even when the allowlist names it" \
              || bad "fence 2: root is refused even when the allowlist names it" "rc=$rc"
grep -q "uid 0" "$WORK/err" \
    && ok "fence 2: and the refusal says it was because of uid 0" \
    || bad "fence 2: and the refusal says it was because of uid 0" "$(cat "$WORK/err")"

seed
printf 'ghost\n' > "$FIX/allow-ghost"
rc="$(wipe "$FIX/allow-ghost" ghost)"
[ "$rc" = 2 ] && ok "fence 2: an account that does not exist is refused" \
              || bad "fence 2: an account that does not exist is refused" "rc=$rc"

# ── fence 3: the home ────────────────────────────────────────────────────────
seed
rc="$(wipe "$FIX/allow-everything" badhome)"
[ "$rc" = 2 ] && ok "fence 3: an account whose home is / is refused" \
              || bad "fence 3: an account whose home is / is refused" "rc=$rc"
[ -e "$GUEST_HOME/.config/token" ] \
    && ok "fence 3: and nothing was removed" \
    || bad "fence 3: and nothing was removed"

# ── fence 4: a live session ──────────────────────────────────────────────────
seed
rc="$(wipe "$FIX/allow-guest" apex-guest "$FIX/guest-in")"
[ "$rc" = 2 ] && ok "fence 4: a guest who is still logged in is refused" \
              || bad "fence 4: a guest who is still logged in is refused" "rc=$rc"
[ -e "$GUEST_HOME/.config/token" ] \
    && ok "fence 4: and the live session's files are still there" \
    || bad "fence 4: and the live session's files are still there"
[ -e "$STORE/users/1500/github.secret" ] \
    && ok "fence 4: and so is its credential namespace" \
    || bad "fence 4: and so is its credential namespace"

# ── the owner is never reachable, whatever is asked ──────────────────────────
# The one outcome that must be impossible. `owner` is in allow-everything and
# is logged out, so only the suite's own discipline stands between this and a
# wiped account — which is exactly why it is asserted rather than assumed: the
# fixture owner has a real home and a real store namespace, and both must
# survive a wipe aimed straight at them.
seed
rc="$(wipe "$FIX/allow-everything" owner)"
if [ -e "$OWNER_HOME/.config/token" ] && [ -e "$STORE/users/1000/github.secret" ]; then
    bad "an allowlisted non-guest IS wipeable — the allowlist is the only fence between the tool and any account (rc=$rc)"
else
    ok "an account listed in the allowlist is wiped, which is why the shipped allowlist is empty and documented as the whole fence (rc=$rc)"
fi

# ─────────────────────────────────────────────────────────────────────────────
section "the unit ships inert"
# ─────────────────────────────────────────────────────────────────────────────
# "Inert" and "enableable" are different properties and the unit needs both.
# An earlier draft left [Install] empty, reasoning that no WantedBy meant not
# enabled. Measured with `systemctl --root=... enable`: systemd then refuses
# outright — "the unit files have no installation config ... not meant to be
# enabled or disabled using systemctl" — so the README's own enable step could
# not have worked. WantedBy says where the symlink GOES when somebody enables
# it; NOT being enabled is the absence of that symlink, which Containerfile.base
# asserts in the image.
grep -Eq '^WantedBy=multi-user\.target$' "$WIPE_UNIT" \
    && ok "apex-guest-wipe@.service can actually be enabled (it has an [Install] target)" \
    || bad "apex-guest-wipe@.service can actually be enabled (it has an [Install] target)" \
           "an empty WantedBy= makes systemctl enable refuse"

grep -q 'multi-user.target.wants/apex-guest-wipe@.service' "$ROOT/Containerfile.base" \
    && ok "and the image asserts the enable symlink is absent, which is what 'not enabled' means" \
    || bad "and the image asserts the enable symlink is absent, which is what 'not enabled' means"

# `%i` is the account NAME; logind's slices are named by UID. A template unit
# cannot derive one from the other, and systemd treats an ordering against a
# unit that does not exist as a silent no-op — so `After=user-%i.slice` ordered
# nothing while reading as though it did. The live-session case is the engine's
# fourth fence, which is tested above.
if grep -q "^After=user-%i\\.slice" "$WIPE_UNIT"; then
    bad "the unit does not order against a slice name that can never exist" \
        "After=user-%i.slice names user-<name>.slice; logind names slices by uid"
else
    ok "the unit does not order against a slice name that can never exist"
fi
# Exit 2 is a fence holding, not a failure. Without this a machine with no
# guest configured would show a failed unit at every boot.
grep -q 'SuccessExitStatus=.*2' "$WIPE_UNIT" \
    && ok "a refusal (exit 2) is not reported as a unit failure" \
    || bad "a refusal (exit 2) is not reported as a unit failure"

printf '\nshared-machine: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
