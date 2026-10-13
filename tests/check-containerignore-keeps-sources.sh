#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-containerignore-keeps-sources.sh — does .containerignore exclude a
#  file that is TRACKED SOURCE?
#
#  WHY THIS FILE EXISTS. `.containerignore` carried `**/target/` to keep a
#  developer's ~440 MB of `cargo build` output out of the build context. That
#  pattern matches a directory named `target` at ANY depth — including
#  `apexd/apex-backup-core/src/target/`, a source module holding the backup
#  backends. The source was excluded from the context of every image build.
#
#  It does not fail the COPY. It fails the compile, minutes later, with a
#  message that names neither the build context nor .containerignore:
#
#      error[E0583]: file not found for module `target`
#        --> apex-backup-core/src/lib.rs:45:1
#
#  Run 35582968952's `base` job died that way with exit 101, taking `image` and
#  `qcow2` with it. `.gitignore` had made the identical mistake with a bare
#  `target/` and carries its own comment about it — so this is the second time
#  one wildcard hid real source in this repository, in two different files.
#
#  The rule: a build-product pattern must be ANCHORED to the directory that
#  actually holds build products. If you want to exclude `apexd/target`, say
#  `apexd/target/`, not `**/target/`, because only you know that `target` is
#  also a legitimate module name.
#
#  Static, offline, milliseconds: it compares .containerignore's patterns
#  against `git ls-files`, and builds nothing.
#
#      ./tests/check-containerignore-keeps-sources.sh
# ─────────────────────────────────────────────────────────────────────────────
# NOT `set -e`: report every offending pattern, not just the first.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

CI_FILE=.containerignore
[ -f "$CI_FILE" ] || { echo "ok: no $CI_FILE, nothing to check"; exit 0; }

command -v git >/dev/null || { echo "FATAL: git is required"; exit 1; }
# A tarball checkout has no .git and cannot answer "is this tracked?".
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "SKIP: not a git checkout, cannot enumerate tracked files"
    exit 0
fi

fail=0
err()  { echo "FAIL: $*" >&2; fail=$((fail + 1)); }
hint() { echo "      $*" >&2; }

# Every tracked file, once.
tracked=$(git ls-files)
[ -n "$tracked" ] || { echo "FATAL: git ls-files returned nothing"; exit 1; }

checked=0
while IFS= read -r raw; do
    # Strip comments and blanks. A trailing comment is not a thing in
    # dockerignore syntax, so only a whole-line `#` counts.
    line=${raw%%$'\r'}
    case "$line" in ''|'#'*) continue ;; esac
    # Negations re-include; they cannot hide source.
    case "$line" in '!'*) continue ;; esac

    # The dangerous shape is an UNANCHORED directory name: `**/x/`, `**/x`, or
    # a bare `x/` with no slash before it — all of which match at any depth.
    case "$line" in
        '**/'*)  name=${line#'**/'} ;;
        */*)     continue ;;          # anchored (contains a path separator)
        *)       name=$line ;;        # bare name, matches at any depth
    esac
    name=${name%/}
    [ -n "$name" ] || continue
    # A bare name with a glob is a file-extension rule (*.tmp), not a dir trap.
    case "$name" in *'*'*|*'?'*|*'['*) continue ;; esac

    checked=$((checked + 1))

    # Any tracked path with that name as an interior component is source this
    # pattern would remove from the build context.
    hits=$(printf '%s\n' "$tracked" | grep -E "(^|/)${name}/" | head -5)
    if [ -n "$hits" ]; then
        n=$(printf '%s\n' "$tracked" | grep -cE "(^|/)${name}/")
        err "$CI_FILE pattern '$line' excludes $n TRACKED file(s) from the build context:"
        printf '%s\n' "$hits" | sed 's/^/        /' >&2
        hint "That pattern matches a directory named '$name' at ANY depth. If it is"
        hint "meant to drop build products, anchor it to the directory that holds"
        hint "them (e.g. 'apexd/$name/'), because '$name' is also a real source"
        hint "directory here. An excluded source file does not fail the COPY — it"
        hint "fails the compile later with a message naming neither this file nor"
        hint "the build context."
    fi
done < "$CI_FILE"

echo
if [ "$fail" -ne 0 ]; then
    echo "check-containerignore-keeps-sources: $fail unanchored pattern(s) hide tracked source" >&2
    exit 1
fi
echo "check-containerignore-keeps-sources: $checked unanchored pattern(s) checked, none hide tracked source"
