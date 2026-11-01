# Every Rime unit's Documentation= must lead somewhere on the system it ships in (nix/tests/unit-docs.nix).
#   $1 = a toplevel (or a tree shaped like one: etc/systemd/{system,user}, sw/share/man)
# A unit is Rime's when its name starts with "rime". Accepted:
#   man:NAME(SECTION)   only when sw/share/man/manSECTION/NAME.SECTION[.gz|.xz|.bz2] exists
#   file:PATH           only a /nix/store path that exists, or /run/current-system/... that exists in the toplevel
#                       (a bootc path such as file:///usr/share/doc/rime/boot-v2.md exists on no NixOS machine)
#   http:// https://    accepted as they are (nothing to resolve offline)
# Anything else fails. Prints one line per link, and refuses a run that found fewer than MIN_UNITS Rime units.
set -u
top=$1
min=${MIN_UNITS:-10}
units=0 links=0 bad=0

resolves() { # $1 = one Documentation URI
    local u=$1 name sec p
    case "$u" in
        http://* | https://*) return 0 ;;
        man:*\(*\))
            name=${u#man:}; sec=${name##*\(}; sec=${sec%\)}; name=${name%\(*}
            for p in "$top/sw/share/man/man$sec/$name.$sec"{,.gz,.xz,.bz2}; do [ -f "$p" ] && return 0; done
            return 1 ;;
        file:*)
            p=${u#file:}; p=/${p##/}  # file:/x and file:///x
            case "$p" in
                /nix/store/*) [ -e "$p" ] ;;
                /run/current-system/*) [ -e "$top/${p#/run/current-system/}" ] ;;
                *) return 1 ;;
            esac ;;
        *) return 1 ;;
    esac
}

for kind in system user; do
    for f in "$top/etc/systemd/$kind"/rime*; do
        [ -f "$f" ] || continue # drop-in directories
        units=$((units + 1))
        while IFS= read -r line; do
            for u in ${line#Documentation=}; do
                links=$((links + 1))
                if resolves "$u"; then
                    echo "ok   $kind/${f##*/}: $u"
                else
                    echo "BAD  $kind/${f##*/}: $u leads nowhere on this system"
                    bad=$((bad + 1))
                fi
            done
        done < <(grep '^Documentation=' "$f")
    done
done

echo "$units Rime units, $links Documentation links, $bad leading nowhere"
if [ "$units" -lt "$min" ]; then
    echo "unit-docs: found only $units Rime units (expected at least $min): the toplevel is not the one meant" >&2
    exit 1
fi
[ "$bad" = 0 ]
