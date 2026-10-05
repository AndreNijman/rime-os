#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-pkg-alternatives.sh — extract a REAL package set with the shipped
#  engine and assert that every binary the set publishes through `alternatives`
#  actually exists in the payload.
#
#  ── The defect ──────────────────────────────────────────────────────────────
#  ROADMAP/evidence/katana-p1038-apps-20260922.md and
#  ROADMAP/evidence/pkg-alternatives-20260922.md. rime-pkg builds the system
#  extension by extracting rpms with --noscripts. A package that publishes its
#  binaries through `alternatives` creates them IN ITS SCRIPTLETS, so they are
#  never created. Measured on katana: `rime install wine` left /usr/bin/wine and
#  /usr/bin/wineserver absent and twelve /usr/bin entries — msidb, msiexec,
#  notepad, regedit, regsvr32, wineboot, winecfg, wineconsole, winedbg,
#  winefile, winemine, winepath — as dangling symlinks pointing at `wine`. The
#  install reported success. `command -v` answered, `wine64 --version` printed
#  a version, and only actually running a program failed.
#
#  ── What this suite asserts, and why it is not "zero dangling symlinks" ─────
#  A dangling symlink in a payload is not by itself a defect. gcc's extraction
#  leaves eight, all of them links into subpackages the set did not carry
#  (/usr/lib/gcc/.../32/libasan.a -> ../../../i686-redhat-linux/15/libasan.a);
#  measured here on 2026-09-22. A suite that demanded zero would fail on an
#  incomplete set for a reason that has nothing to do with alternatives and
#  could never be made green.
#
#  The property asserted is the upstream one, and it is strictly stronger than
#  the symptom:
#
#     every link a package in the set DECLARES through `alternatives --install`
#     — and every --slave/--follower of one — exists in the payload and
#     resolves once the extension is merged.
#
#  It catches the case the symptom misses. wine-core declares twelve links
#  under /usr/lib64/wine-wow64/wine/*/  that nothing else points at, so no
#  symlink dangles on their account and they are simply absent.
#
#  The katana finding is then reproduced as a CONSEQUENCE, in its own leg:
#  dangling symlinks whose missing target is one of the declared links. Twelve
#  of those is what the machine had. Nothing in the engine names wine, and
#  nothing here asserts on the number twelve — it is printed.
#
#  ── Why the package set is what it is ───────────────────────────────────────
#  Four packages, chosen for four different shapes of the same declaration:
#
#    wine-core     declares /usr/bin/wine and /usr/bin/wineserver in %POSTTRANS
#                  — not %post — by ABSOLUTE path (/usr/bin/alternatives), plus
#                  twelve .dll links, two of them with --slave followers. It is
#                  the measured package. 221 MB, and it is here because the
#                  katana reproduction is worth it.
#    wine-common   103 KB, noarch, ships eleven of the twelve dangling symlinks
#                  and declares nothing. It is what makes the consequence leg
#                  possible without installing the whole of wine.
#    iptables-nft  builds all three of its invocations out of SHELL VARIABLES
#                  (pfx=/usr/bin/iptables … --install $pfx iptables $pfx-nft 10
#                  --follower $pfx6 …) and calls `update-alternatives` as a BARE
#                  word. No parser can read it; it is the reason the engine runs
#                  the scriptlet instead of parsing it.
#    nmap-ncat     one --install with one --slave, in %post, absolute path. The
#                  smallest complete example, and the refusal control below
#                  installs it for real.
#
#  ── What a container can and cannot prove ───────────────────────────────────
#  Everything here except the merge. The payload is built, not mounted, so
#  "resolves once the extension is merged" is computed by the engine's own
#  payload_link_resolves() — the same function production warns from, so this
#  suite cannot pass against a checker the engine does not use. That checker is
#  itself put on trial in the negative-control leg.
#
#  Not provable here: that wine then launches a Windows program. That needs the
#  machine, and it is P1-038's row, not this suite's.
#
#  Run from anywhere: ./tests/test-rime-pkg-alternatives.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

ENGINE=files/system/libexec/rime-pkg
[ -f "$ENGINE" ] || { echo "cannot find $ENGINE"; exit 2; }

# ── the call site, asserted before anything else ────────────────────────────
# Every leg below calls apply_alternatives directly, exactly as the multilib and
# etc-label suites call extract_rpms and install_etc. That proves the pass
# works and proves nothing about whether an `rime install` reaches it — delete
# the one line in rebuild_extension and all fourteen assertions still pass. So
# the order of the three calls in rebuild_extension is read out of the shipped
# engine here, and this runs even on a machine with no podman, where everything
# else abstains.
callsite=$(grep -n -E '^\s+(extract_rpms|apply_alternatives|fix_caches) ' "$ENGINE" \
           | sed -E 's/^[0-9]+:\s*([a-z_]+).*/\1/' | paste -sd, -)
precall=0
case "$callsite" in
    *extract_rpms,apply_alternatives,fix_caches*) precall=1 ;;
esac

if ! command -v podman >/dev/null 2>&1; then
    if [ "$precall" = 1 ]; then
        echo "PASS  rebuild_extension calls apply_alternatives between extract_rpms and fix_caches"
    else
        echo "FAIL  rebuild_extension does not call apply_alternatives between extract_rpms and fix_caches; the call order is '$callsite'"
        exit 1
    fi
    echo "SKIP  podman is absent; the rest of this suite needs a real dnf, a real repository and real rpm scriptlets"
    exit 0
fi

IMAGE=${RIME_ALT_IMAGE:-registry.fedoraproject.org/fedora:45}
SET=${RIME_ALT_SET:-wine-core wine-common iptables-nft nmap-ncat}

PROBE=$(mktemp) || exit 2
trap 'rm -f "$PROBE"' EXIT

cat > "$PROBE" <<'PROBE_EOF'
set -uo pipefail
PKG=/repo/files/system/libexec/rime-pkg

# setpriv comes from util-linux, which the Fedora base image does NOT carry —
# measured 2026-09-22. The engine refuses to run a scriptlet it cannot drop
# privileges for, so without this the whole pass no-ops and every leg below
# would go green for the wrong reason. That refusal is asserted too, at the end.
dnf -y install --setopt=install_weak_deps=False util-linux >/dev/null 2>&1 \
    || { echo "PROBE_SKIP util-linux would not install in this container"; exit 0; }
command -v setpriv >/dev/null 2>&1 \
    || { echo "PROBE_SKIP util-linux installed but there is no setpriv"; exit 0; }

W=/tmp/w; rm -rf "$W"; mkdir -p "$W/dl" "$W/dl2"
# shellcheck disable=SC2086
dnf5 -y download --arch=x86_64 --arch=noarch --destdir "$W/dl" $ALT_SET >/dev/null 2>&1 \
    || { echo "PROBE_SKIP no repository reachable"; exit 0; }
echo "PROBE_SET $(find "$W/dl" -name '*.rpm' | wc -l)"

# shellcheck disable=SC1090
source "$PKG" >/dev/null 2>&1
set +e

# ── the population ──────────────────────────────────────────────────────────
# extract_rpms is called DIRECTLY, as the multilib and etc-label suites call it:
# the install path would need this container's versions to match the repository,
# and guard_rpms correctly refuses the transaction when they do not. What is on
# trial here is what the payload contains, not how it was decided.
extract_rpms "$W/dl" "$W/root" >/dev/null 2>&1
echo "PROBE_EXTRACT_RC $?"
echo "PROBE_SYMLINKS $(find "$W/root" -type l 2>/dev/null | wc -l)"

# ── what the packages DECLARE ───────────────────────────────────────────────
alternatives_record "$W/dl" "$W/rec" 2>/dev/null
: > "$W/declared"
argv=()
while IFS= read -r -d '' tok; do
    if [ -z "$tok" ]; then alternatives_candidates "$W/declared" "${argv[@]}"; argv=(); else argv+=("$tok"); fi
done < "$W/rec"
# One row per LINK, carrying the target of the highest-priority declaration for
# it — which is the one alternatives itself would choose, and the one the engine
# applies first.
LC_ALL=C sort -t"$(printf '\t')" -k1,1nr -s "$W/declared" \
    | awk -F'\t' '!seen[$2]++ { print $2 "\t" $3 }' > "$W/declared.best"
cut -f1 "$W/declared.best" | LC_ALL=C sort -u > "$W/declared.links"
echo "PROBE_DECLARED $(wc -l < "$W/declared")"
echo "PROBE_DECLARED_UNIQUE $(wc -l < "$W/declared.links")"
while IFS= read -r l; do echo "PROBE_DECL $l"; done < "$W/declared.links"

# The first missing path in a link's chain, resolved the way the merged system
# will see it. The engine answers yes/no; this answers WHICH, which is what
# attributes a dangling link to a missing alternatives target rather than to a
# subpackage the set did not carry.
missing_target() {
    local root="$1" cur="$2" hops=0 t
    while [ -L "$cur" ] && [ "$hops" -lt 16 ]; do
        t="$(readlink "$cur")"
        case "$t" in
            /*) if [ -e "${root}${t}" ] || [ -L "${root}${t}" ]; then cur="${root}${t}"
                elif [ -e "$t" ] || [ -L "$t" ]; then cur="$t"
                else printf '%s\n' "$t"; return 0; fi ;;
            *)  cur="${cur%/*}/$t" ;;
        esac
        hops=$((hops + 1))
    done
    [ -e "$cur" ] || printf '%s\n' "${cur#"$root"}"
    return 0
}

# Three numbers per leg.
#
#   ABSENT is counted over the declared links the engine is ALLOWED to create,
#   not over all of them. A declaration whose target is in neither the payload
#   nor the image, or whose link path the image already owns, is one the engine
#   must refuse — counting those as failures would demand the opposite of the
#   refusals leg below. NONPLACEABLE is printed so that "absent 0" can never be
#   reached by declaring everything unplaceable.
#
#   ATTRIB is the katana symptom: payload symlinks that dangle because the path
#   they lead to is a declared link that was never created. gcc's extraction
#   leaves eight dangling links that are nothing to do with alternatives, and
#   this is what tells the two apart.
#
#   DANGLING is every dangling symlink in the payload, read through the
#   engine's own dangling_links(), and exists to catch a fix that trades one
#   broken link for another.
measure() {
    local tag="$1" root="$2"
    local link target absent=0 placeable=0 nonplaceable=0 attrib=0 l m
    while IFS="$(printf '\t')" read -r link target; do
        if image_owns "$link" \
           || { [ ! -e "${root}${target}" ] && [ ! -L "${root}${target}" ] && ! image_owns "$target"; } \
           || { [ -e "${root}${link}" ] && [ ! -L "${root}${link}" ]; }; then
            nonplaceable=$((nonplaceable + 1))
            echo "PROBE_${tag}_NONPLACEABLE_LINK $link"
            continue
        fi
        placeable=$((placeable + 1))
        if [ ! -e "${root}${link}" ] && [ ! -L "${root}${link}" ]; then
            absent=$((absent + 1))
            echo "PROBE_${tag}_ABSENT_LINK $link"
        fi
    done < "$W/declared.best"
    while IFS= read -r l; do
        m="$(missing_target "$root" "$l")"
        [ -n "$m" ] || continue
        if LC_ALL=C grep -qxF -- "$m" "$W/declared.links"; then
            attrib=$((attrib + 1))
            echo "PROBE_${tag}_ATTRIB_LINK ${l#"$root"} -> $m"
        fi
    done < <(find "$root" -type l 2>/dev/null)
    echo "PROBE_${tag}_PLACEABLE $placeable"
    echo "PROBE_${tag}_NONPLACEABLE $nonplaceable"
    echo "PROBE_${tag}_ABSENT $absent"
    echo "PROBE_${tag}_ATTRIB $attrib"
    echo "PROBE_${tag}_DANGLING $(dangling_links "$root" | wc -l)"
}

# ── leg 1, RED: the engine exactly as it shipped ────────────────────────────
# No stub and no mutation: rebuild_extension used to go straight from
# extract_rpms to fix_caches, so a tree on which apply_alternatives has not yet
# run IS the old engine's output.
measure RED "$W/root"

# ── leg 2, GREEN: the pass, on the same tree ────────────────────────────────
# The red leg changed nothing, so re-using the tree costs one 1.4 GB extraction
# instead of two and compares like with like.
apply_alternatives "$W/dl" "$W/root" > "$W/apply.out" 2>&1
echo "PROBE_APPLY_RC $?"
LC_ALL=C grep -c 'alternatives: not creating' "$W/apply.out" | sed 's/^/PROBE_REFUSALS /'
LC_ALL=C grep '^rime-pkg: alternatives:' "$W/apply.out" | sed 's/^/PROBE_APPLY_MSG /'
measure GREEN "$W/root"

# Every declared link that exists must also RESOLVE — placing a link that
# points at nothing would satisfy "absent 0" and still ship the defect.
unresolved=0
while IFS= read -r l; do
    [ -e "${W}/root${l}" ] || [ -L "${W}/root${l}" ] || continue
    payload_link_resolves "$W/root" "${W}/root${l}" || { unresolved=$((unresolved + 1)); echo "PROBE_GREEN_UNRESOLVED_LINK $l"; }
done < "$W/declared.links"
echo "PROBE_GREEN_UNRESOLVED $unresolved"

# ── leg 3, the katana finding, reproduced rather than asserted ──────────────
# The twelve /usr/bin entries that were dangling on the machine, before and
# after, by name. This is the evidence file's list, not an assertion: the
# property above is what fails the suite.
for b in msidb msiexec notepad regedit regsvr32 wineboot winecfg wineconsole winedbg winefile winemine winepath; do
    if [ -L "${W}/root/usr/bin/$b" ]; then
        if payload_link_resolves "$W/root" "${W}/root/usr/bin/$b"; then s=resolves; else s=DANGLES; fi
        echo "PROBE_KATANA $b $s $(stat -c %s "${W}/root/usr/bin/$b" 2>/dev/null)"
    fi
done
echo "PROBE_KATANA_WINE $( [ -L "${W}/root/usr/bin/wine" ] && readlink "${W}/root/usr/bin/wine" || echo ABSENT )"
echo "PROBE_KATANA_WINESERVER $( [ -L "${W}/root/usr/bin/wineserver" ] && readlink "${W}/root/usr/bin/wineserver" || echo ABSENT )"

# ── leg 4, negative control: the checker must SEE a broken link ─────────────
# Everything above is read through dangling_links()/payload_link_resolves(). A
# checker that answered "fine" unconditionally would make every leg pass, so
# one link that the pass just created is broken by hand and the count must move
# by exactly one.
victim="$(LC_ALL=C head -1 "$W/declared.links")"
before=$(dangling_links "$W/root" | wc -l)
ln -sfn /usr/bin/rime-no-such-target-2026 "${W}/root${victim}"
after=$(dangling_links "$W/root" | wc -l)
echo "PROBE_CONTROL $victim $before $after"
ln -sfn "$(LC_ALL=C grep -P "^\d+\t\Q${victim}\E\t" "$W/declared" | head -1 | cut -f3)" "${W}/root${victim}"

# ── leg 5, refusal control: a link the IMAGE owns is never created ──────────
# nmap-ncat is installed for real, which runs its scriptlet and makes
# /usr/bin/nc an rpm-owned path in this container. The pass must then refuse to
# put a second copy in the overlay, and must decide that from the rpmdb rather
# than from the filesystem.
cp -f "$W"/dl/nmap-ncat-*.rpm "$W/dl2/" 2>/dev/null
dnf -y install --setopt=install_weak_deps=False nmap-ncat >/dev/null 2>&1
echo "PROBE_NC_OWNER $(rpm -qf --qf '%{NAME}' /usr/bin/nc 2>/dev/null || echo NONE)"
extract_rpms "$W/dl2" "$W/root2" >/dev/null 2>&1
apply_alternatives "$W/dl2" "$W/root2" > "$W/apply2.out" 2>&1
echo "PROBE_REFUSE_NC $( [ -e "${W}/root2/usr/bin/nc" ] || [ -L "${W}/root2/usr/bin/nc" ] && echo CREATED || echo REFUSED )"
LC_ALL=C grep -c 'Rime OS already owns it' "$W/apply2.out" | sed 's/^/PROBE_REFUSE_SAID /'

# ── leg 6, the confinement is real ──────────────────────────────────────────
# A scriptlet must never run as root and must never reach a command. Both are
# measured by handing the recorder a package whose scriptlet tries: the id it
# runs as is written to a file only nobody could create, and `rm` is called on a
# file only root could delete.
mkdir -p "$W/dl3"
: > /usr/bin/rime-alt-canary
cat > "$W/canary.spec" <<'SPEC'
Name: rime-alt-canary
Version: 1
Release: 1
Summary: s
License: MIT
BuildArch: noarch
%description
s
%post
/usr/bin/id -u > /tmp/rime-alt-whoami 2>/dev/null || echo NO_ID_COMMAND > /tmp/rime-alt-whoami
rm -f /usr/bin/rime-alt-canary
/usr/bin/rm -f /usr/bin/rime-alt-canary
/usr/bin/alternatives --install /usr/bin/rime-alt-link rime-alt-link /usr/bin/sh 10
%files
SPEC
dnf -y install --setopt=install_weak_deps=False rpm-build >/dev/null 2>&1
if rpmbuild -bb --define "_rpmdir $W/dl3" "$W/canary.spec" >/dev/null 2>&1; then
    find "$W/dl3" -name 'rime-alt-canary*.rpm' -exec mv -t "$W/dl3" {} + 2>/dev/null
    rm -f /tmp/rime-alt-whoami
    alternatives_record "$W/dl3" "$W/rec3" 2>/dev/null
    echo "PROBE_CANARY_RAN $( [ -s "$W/rec3" ] && echo yes || echo no )"
    echo "PROBE_CANARY_WHOAMI $(cat /tmp/rime-alt-whoami 2>/dev/null || echo NOFILE)"
    echo "PROBE_CANARY_CANARY $( [ -e /usr/bin/rime-alt-canary ] && echo INTACT || echo DELETED )"
else
    echo "PROBE_CANARY_RAN skip"
fi
PROBE_EOF

out=$(podman run --rm --env ALT_SET="$SET" \
        -v "$PWD":/repo:ro,Z -v "$PROBE":/probe.sh:ro,Z \
        "$IMAGE" bash /probe.sh 2>&1)

# NOT `printf … | grep -q`: a match makes grep exit before printf finishes, the
# writer dies of SIGPIPE, and pipefail hands the `if` a 141.
if [[ "$out" == *PROBE_SKIP* ]]; then
    echo "SKIP  $(printf '%s\n' "$out" | grep PROBE_SKIP | sed 's/PROBE_SKIP //')"
    exit 0
fi

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail+1)); }
field() { printf '%s\n' "$out" | grep -m1 "^PROBE_$1 " | cut -d' ' -f2- ; }

nset=$(field SET);            extract_rc=$(field EXTRACT_RC)
declared=$(field DECLARED);   uniq=$(field DECLARED_UNIQUE)
red_absent=$(field RED_ABSENT);     red_attrib=$(field RED_ATTRIB);     red_dangling=$(field RED_DANGLING)
grn_absent=$(field GREEN_ABSENT);   grn_attrib=$(field GREEN_ATTRIB);   grn_dangling=$(field GREEN_DANGLING)
red_placeable=$(field RED_PLACEABLE); grn_nonplaceable=$(field GREEN_NONPLACEABLE)
grn_unres=$(field GREEN_UNRESOLVED)
apply_rc=$(field APPLY_RC)

printf '\n── what the container did ──────────────────────────────────────────\n'
printf '      %s rpm(s) downloaded; extract_rpms rc=%s; %s symlink(s) in the payload\n' \
       "$nset" "$extract_rc" "$(field SYMLINKS)"
printf '      %s declaration(s) recorded from the scriptlets, %s distinct link(s)\n' "$declared" "$uniq"
printf '%s\n' "$out" | grep '^PROBE_APPLY_MSG' | sed 's/^PROBE_APPLY_MSG rime-pkg: /      engine: /'
printf '      RED   placeable=%s absent=%s  dangling-because-of-it=%s  dangling-total=%s\n' \
       "$red_placeable" "$red_absent" "$red_attrib" "$red_dangling"
printf '      GREEN placeable=%s absent=%s  dangling-because-of-it=%s  dangling-total=%s  refused=%s\n' \
       "$(field GREEN_PLACEABLE)" "$grn_absent" "$grn_attrib" "$grn_dangling" "$grn_nonplaceable"
printf '\n'

[ "$precall" = 1 ] && ok "rebuild_extension calls apply_alternatives between extract_rpms and fix_caches — every other assertion here calls the pass directly, so without this one the call site could be deleted and the suite would stay green" \
                    || bad "rebuild_extension does not call apply_alternatives between extract_rpms and fix_caches; the call order is '$callsite'"
[ "$extract_rc" = 0 ] && ok "extract_rpms built a payload from the real package set" \
                      || bad "extract_rpms exited $extract_rc — nothing below means anything"
[ "$apply_rc" = 0 ] && ok "apply_alternatives exited 0" || bad "apply_alternatives exited $apply_rc"

# The suite is worthless if the packages declare nothing, and that is exactly
# what a recorder that silently failed would look like.
if [ "${uniq:-0}" -ge 10 ]; then
    ok "the scriptlets declare $uniq distinct alternatives link(s) — recorded by running them, which is the only way iptables-nft's \$pfx-built arguments can be read"
else
    bad "only ${uniq:-0} declared link(s) were recorded; the recorder is not reading the scriptlets and every leg below is trivial"
fi

# ── the property, both ways ─────────────────────────────────────────────────
if [ "${red_placeable:-0}" -ge 10 ]; then
    ok "$red_placeable of the declared links are ones the engine is allowed to create, so 'absent 0' cannot be reached by refusing everything"
else
    bad "only ${red_placeable:-0} declared link(s) were placeable; 'absent 0' below would be nearly free"
fi
if [ "${red_absent:-0}" -gt 0 ]; then
    ok "RED: $red_absent placeable declared link(s) absent from a payload built the way the engine shipped"
else
    bad "RED: every declared link was already present before the pass ran — this suite cannot be shown red, so it proves nothing"
fi
if [ "${grn_absent:-0}" = 0 ]; then
    ok "GREEN: every declared link exists after apply_alternatives"
else
    bad "GREEN: $grn_absent declared link(s) are still absent"
    printf '%s\n' "$out" | grep '^PROBE_DECL ' | sed 's/^PROBE_DECL /        declared: /' | head -40
fi
if [ "${grn_unres:-1}" = 0 ]; then
    ok "GREEN: every declared link that exists also RESOLVES once the extension is merged"
else
    bad "GREEN: $grn_unres declared link(s) exist but point at nothing"
    printf '%s\n' "$out" | grep '^PROBE_GREEN_UNRESOLVED_LINK' | sed 's/^/        /'
fi

# ── the symptom katana had ──────────────────────────────────────────────────
if [ "${red_attrib:-0}" -gt 0 ]; then
    ok "RED: $red_attrib symlink(s) in the payload dangle because a declared link is absent — the katana symptom, reproduced"
else
    bad "RED: no payload symlink dangled on account of a missing declared link; the set no longer reproduces the measured finding"
fi
if [ "${grn_attrib:-0}" = 0 ]; then
    ok "GREEN: no payload symlink dangles on account of a declared link any more"
else
    bad "GREEN: $grn_attrib still do"
fi
if [ "${grn_dangling:-0}" -le "${red_dangling:-0}" ]; then
    ok "GREEN: the pass created no new dangling symlink (total went $red_dangling -> $grn_dangling)"
else
    bad "GREEN: dangling symlinks went UP, $red_dangling -> $grn_dangling"
fi

printf '      the twelve katana names, after the pass:\n'
printf '%s\n' "$out" | grep '^PROBE_KATANA ' | sed 's/^PROBE_KATANA /        /'
printf '        /usr/bin/wine       -> %s\n' "$(field KATANA_WINE)"
printf '        /usr/bin/wineserver -> %s\n' "$(field KATANA_WINESERVER)"

# ── the checker itself ──────────────────────────────────────────────────────
read -r victim cbefore cafter <<<"$(field CONTROL)"
if [ "${cafter:-0}" = "$((${cbefore:-0} + 1))" ]; then
    ok "negative control: repointing $victim at a path that does not exist moved the checker's count $cbefore -> $cafter"
else
    bad "negative control: breaking $victim moved the count $cbefore -> $cafter; dangling_links() does not see a broken link and every GREEN leg above is worthless"
fi

# ── the refusals ────────────────────────────────────────────────────────────
if [ "$(field REFUSE_NC)" = REFUSED ] && [ "$(field REFUSE_SAID)" -ge 1 ]; then
    ok "refusal: /usr/bin/nc is owned by $(field NC_OWNER) in the rpmdb, and the pass refused to shadow it and said why"
else
    bad "refusal: the pass created /usr/bin/nc over a path the image owns (owner=$(field NC_OWNER), result=$(field REFUSE_NC))"
fi

# ── the confinement ─────────────────────────────────────────────────────────
canary=$(field CANARY_RAN)
if [ "$canary" = skip ]; then
    printf 'SKIP  the canary rpm would not build here; confinement not measured this run\n'
elif [ "$canary" = yes ]; then
    who=$(field CANARY_WHOAMI); intact=$(field CANARY_CANARY)
    if [ "$who" != 0 ]; then
        ok "confinement: the scriptlet did not run as root (id -u said '$who')"
    else
        bad "confinement: the scriptlet ran as ROOT"
    fi
    if [ "$intact" = INTACT ]; then
        ok "confinement: a scriptlet's 'rm -f /usr/bin/rime-alt-canary', by bare name AND by absolute path, deleted nothing"
    else
        bad "confinement: a scriptlet deleted a file in /usr/bin"
    fi
else
    bad "confinement: the canary scriptlet recorded nothing, so the recorder did not run it and the two checks above did not happen"
fi

printf '\nrime-pkg-alternatives: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
