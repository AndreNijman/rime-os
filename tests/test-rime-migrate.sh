#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-migrate.sh — the bootc side of the move to Rime OS on NixOS.
#
#  What the readiness release adds, checked without root, a registry or a VM:
#
#    1. /usr/libexec/rime-migrate (files/system/libexec/rime-migrate): the
#       bridge program from the NixOS branch, one Python file, standard library
#       only, run by Fedora's own python. Its unit suite (tests/rime-migrate/)
#       runs against the shipped file.
#    2. rime-update-check: once `rime migrate start` has run, it never asks
#       the registry and reports "none", unless the start rolled itself back.
#    3. With --with-binary (or RIME_BIN set): the `rime` binary against fixture
#       records (RIME_BRIDGE_ROOT). Every bootc verb is REFUSED by name after a
#       start, before the root gate, and allowed again after a rollback; `rime
#       migrate …` hands every argument to the bridge unchanged.
#
#  The payload signature is the bridge's own check (verify_payload, at the
#  point it reads the payload; its unit suite covers unsigned, untrusted-key,
#  tampered, missing-key-file and good payloads). This suite pins where that
#  trust comes from: the image's key file
#  /usr/share/rime-os/trust/nixos-bridge-payload.pub, nothing compiled in, and
#  the signature file the NixOS side's payload-sign.sh writes
#  (manifest.json.sig, over the manifest's exact bytes).
#
#  Failures it exists to catch: a finished move whose old side still runs
#  `bootc upgrade` (the bridge's VM run saw exactly that); a refusal that a
#  rolled-back start leaves in place for ever; an unreadable record read as
#  "never started"; a bridge refresh that quietly changes whom it trusts.
#
#  Run: ./tests/test-rime-migrate.sh [--with-binary]
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail the producer then fails.
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROG="$REPO/files/system/libexec/rime-migrate"
CHECK="$REPO/files/system/libexec/rime-update-check"
WITH_BINARY=0
[[ "${1:-}" == "--with-binary" || -n "${RIME_BIN:-}" ]] && WITH_BINARY=1
RIME_BIN="${RIME_BIN:-$REPO/rimed/target/debug/rime}"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
export PYTHONDONTWRITEBYTECODE=1

# ── 1. the program ──────────────────────────────────────────────────────────
sec "rime-migrate, the program"
[[ -f "$PROG" && -x "$PROG" ]] && ok "shipped and executable" || bad "missing or not executable: $PROG"
# Fedora's python, isolated: nothing from /nix, no user site, no PYTHONPATH.
[[ "$(head -n1 "$PROG")" == '#!/usr/bin/python3 -I' ]] && ok "shebang is #!/usr/bin/python3 -I" \
    || bad "shebang" "$(head -n1 "$PROG")"
python3 -I -c 'import ast,sys; ast.parse(open(sys.argv[1]).read())' "$PROG" && ok "parses" || bad "does not parse"
# Standard library only: the old side has no pip and no /nix.
nonstd="$(python3 -I - "$PROG" <<'PY'
import ast, sys
tree = ast.parse(open(sys.argv[1]).read())
mods = set()
for n in ast.walk(tree):
    if isinstance(n, ast.Import):
        mods |= {a.name.split(".")[0] for a in n.names}
    elif isinstance(n, ast.ImportFrom) and n.module and not n.level:
        mods.add(n.module.split(".")[0])
print(" ".join(sorted(m for m in mods if m not in sys.stdlib_module_names)))
PY
)"
[[ -z "$nonstd" ]] && ok "imports the standard library only" || bad "non-stdlib imports" "$nonstd"
"$PROG" --help >/dev/null 2>&1 && ok "--help answers without root" || bad "--help failed"
# docs/nixos-move.md names these as `rime migrate <sub>`; check-doc-verbs.sh
# cannot ask them of the Rust binary (it passes them through to this program),
# so tests/doc-verbs-allow lists them and THIS is where they are proved to parse.
for sub in check repair-e key-export start; do
    if "$PROG" "$sub" --help >/dev/null 2>&1; then
        ok "rime migrate $sub is a real subcommand of the program"
    else
        bad "rime migrate $sub --help failed: docs/nixos-move.md names a verb that does not parse"
    fi
done
# The program refuses to run without root and names the command to type.
if [[ $EUID -ne 0 ]]; then
    out="$("$PROG" check 2>&1)"; rc=$?
    [[ $rc -eq 2 ]] && pipe_has -F 'sudo rime migrate check' <<<"$out" \
        && ok "a non-root run is refused with the sudo line" || bad "non-root run" "rc=$rc $out"
fi
# Its own unit suite: topologies A-F, every named refusal, the NVRAM parser, the
# forbidden-tool scan, the CLI guards.
if RIME_MIGRATE="$PROG" python3 -I -B -W error::ResourceWarning -m unittest discover \
        -s "$REPO/tests/rime-migrate" -p 'test_*.py' >"$W/unit.log" 2>&1; then
    ok "unit suite: $(grep -E '^Ran [0-9]+ tests' "$W/unit.log")"
else
    bad "unit suite" "$(tail -n 20 "$W/unit.log")"
fi

# Where the bridge's payload trust comes from. The NixOS side signs the
# payload with its payload-sign.sh (nix/pkgs/rime-bridge on the NixOS branch):
# manifest.json.sig, "<name>:<base64 ed25519 signature>" over the exact bytes
# of manifest.json. The bridge trusts exactly one thing: the key file this
# image carries, PAYLOAD_KEY_FILE; TRUSTED_PAYLOAD_KEYS is None in the product
# (only the OVMF lab's build sets it). A refresh of the bridge that changes any
# of the three must change this line on purpose, together with the producer.
trust="$(python3 -I - "$PROG" <<'PY'
import ast, sys
want = {"TRUSTED_PAYLOAD_KEYS", "PAYLOAD_KEY_FILE", "PAYLOAD_SIG"}
got = {}
for n in ast.parse(open(sys.argv[1]).read()).body:
    if isinstance(n, ast.Assign) and len(n.targets) == 1 and getattr(n.targets[0], "id", None) in want:
        got[n.targets[0].id] = ast.literal_eval(n.value)
print(repr(got.get("TRUSTED_PAYLOAD_KEYS", "unset")), repr(got.get("PAYLOAD_KEY_FILE")), repr(got.get("PAYLOAD_SIG")))
PY
)"
if [[ "$trust" == "None '/usr/share/rime-os/trust/nixos-bridge-payload.pub' 'manifest.json.sig'" ]]; then
    ok "the payload is trusted only by the image's key file, in the format payload-sign.sh writes"
else
    bad "payload trust root changed" "$trust"
fi
# The key file (rime-bridge-payload-1, docs/nixos-move.md): one
# "<name>:<base64 32-byte key>" line, installed where the bridge reads it. The
# branch below for an absent file covers an image released before the key.
KEYSRC="$REPO/files/system/trust/nixos-bridge-payload.pub"
if [[ -e "$KEYSRC" ]]; then
    if python3 -I -c 'import base64,sys; t=open(sys.argv[1]).read().strip(); n,_,b=t.partition(":"); assert n and len(t.splitlines())==1 and len(base64.b64decode(b, validate=True))==32' "$KEYSRC" 2>/dev/null \
       && pipe_has -F 'files/system/trust/nixos-bridge-payload.pub /usr/share/rime-os/trust/nixos-bridge-payload.pub' "$REPO/Containerfile.base"; then
        ok "the payload key file is one valid line and the image installs it"
    else
        bad "files/system/trust/nixos-bridge-payload.pub is not one valid key line, or Containerfile.base does not install it"
    fi
else
    # The shipped program itself, loaded as a module, with its key file absent.
    # -B: -I ignores PYTHONDONTWRITEBYTECODE, and loading the program as a
    # module would otherwise leave a __pycache__ beside it in files/.
    if (cd "$W" && python3 -I -B - "$PROG" <<'PY'
import importlib.machinery, importlib.util, sys
loader = importlib.machinery.SourceFileLoader("bridge", sys.argv[1])
B = importlib.util.module_from_spec(importlib.util.spec_from_loader("bridge", loader))
loader.exec_module(B)
B.PAYLOAD_KEY_FILE = "/nonexistent/nixos-bridge-payload.pub"
try:
    B.payload_keys()
except B.Refused as e:
    sys.exit(0 if e.name == "payload-key-missing" else 1)
sys.exit(1)
PY
    ); then
        ok "no key file shipped yet: the bridge refuses every payload (payload-key-missing)"
    else
        bad "with no key file the bridge does not refuse payload-key-missing"
    fi
fi
# No way around the check: no environment variable or option skips it.
skip_re='RIME_[A-Z_]*(UNSIGNED|INSECURE|SKIP_SIG|TRUST)|allow-unsigned|--insecure|--no-verify'
if ! pipe_has -E "$skip_re" "$PROG"; then
    ok "nothing in the program switches the payload signature off"
else
    bad "the program names a way to skip the payload signature" "$(grep -n -E "$skip_re" "$PROG")"
fi

# ── fixture records ─────────────────────────────────────────────────────────
# The shapes rime-migrate's cmd_start writes (json.dump of its state dict).
record() {   # record NAME -> $W/rec/NAME/var/lib/rime/nixos-bridge/state.json
    local d="$W/rec/$1/var/lib/rime/nixos-bridge"
    mkdir -p "$d"
    case "$1" in
        moved)      printf '{"done": "2026-11-02T03:00:00+00:00", "started": "2026-11-02T02:00:00+00:00", "steps": ["backup", "old side", "install closure", "done"]}\n' ;;
        running)    printf '{"started": "2026-11-02T02:00:00+00:00", "steps": ["backup", "old side", "install closure"]}\n' ;;
        rolledback) printf '{"failed": "chroot exited 1", "started": "2026-11-02T02:00:00+00:00", "steps": ["backup", "old side", "rolled back"]}\n' ;;
        halfrolled) printf '{"failed": "chroot exited 1", "started": "2026-11-02T02:00:00+00:00", "steps": ["backup", "old side"]}\n' ;;
        # "rolled back" as the last step with no "failed": not a rollback the
        # bridge wrote (it sets "failed" first), so it is not trusted as one.
        rbnofail)   printf '{"started": "2026-11-02T02:00:00+00:00", "steps": ["backup", "old side", "rolled back"]}\n' ;;
        garbage)    printf '{"started": "2026-11-02T0' ;;
        absent)     rm -rf "$W/rec/absent"; mkdir -p "$W/rec/absent" ;;
    esac > "$d/state.json"
    [[ "$1" == absent ]] && rm -rf "$W/rec/absent/var"
    true
}
for r in moved running rolledback halfrolled rbnofail garbage absent; do record "$r"; done
statefile() { echo "$W/rec/$1/var/lib/rime/nixos-bridge/state.json"; }

# ── 2. the update timer ─────────────────────────────────────────────────────
sec "rime-update-check after a start"
cat > "$W/bootc" <<STUB
#!/usr/bin/env bash
echo "\$*" >> "$W/bootc.calls"
case "\$1" in
  status) echo '{"status":{"staged":null}}' ;;
  upgrade) printf 'Update available for: docker://ghcr.io/x/rime-os:daily\n  Digest: sha256:%064d\n' 0 ;;
esac
STUB
chmod +x "$W/bootc"
run_check() {   # run_check RECORD -> rc; $W/run/state, $W/check.log
    rm -rf "$W/run" "$W/bootc.calls"; mkdir -p "$W/run"
    RIME_BOOTC_BIN="$W/bootc" RIME_UPDATE_DIR="$W/run" RIME_BRIDGE_STATE="$(statefile "$1")" \
        bash "$CHECK" >"$W/check.log" 2>&1
}
for r in moved running halfrolled rbnofail garbage; do
    run_check "$r"
    if [[ ! -e "$W/bootc.calls" ]] && pipe_has -x 'state=none' "$W/run/state" \
       && pipe_has -F 'REFUSED [nixos-migration-started]' "$W/check.log"; then
        ok "record '$r': no bootc call, state=none, refusal logged by name"
    else
        bad "record '$r'" "calls=$(cat "$W/bootc.calls" 2>/dev/null) state=$(cat "$W/run/state" 2>/dev/null) log=$(cat "$W/check.log")"
    fi
done
for r in rolledback absent; do
    run_check "$r"
    if pipe_has '^upgrade --check' "$W/bootc.calls" && pipe_has -x 'state=available' "$W/run/state"; then
        ok "record '$r': the check runs as before"
    else
        bad "record '$r' should check" "calls=$(cat "$W/bootc.calls" 2>/dev/null) log=$(cat "$W/check.log")"
    fi
done

# ── 3. the binary ───────────────────────────────────────────────────────────
if (( WITH_BINARY )); then
    sec "rime: the bootc verbs after a start"
    [[ -x "$RIME_BIN" ]] || { bad "no rime binary at $RIME_BIN"; WITH_BINARY=0; }
    if [[ $EUID -eq 0 ]]; then
        # As root the gated verbs that are allowed would really run bootc.
        bad "the binary half must not run as root"; WITH_BINARY=0
    fi
fi
if (( WITH_BINARY )); then
    # PATH holds a bootc that records any call: none may happen in this half.
    mkdir -p "$W/bin"; cp "$W/bootc" "$W/bin/bootc"; rm -f "$W/bootc.calls"
    rime_at() { local r="$1"; shift; RIME_BRIDGE_ROOT="$W/rec/$r" PATH="$W/bin:$PATH" "$RIME_BIN" "$@"; }
    VERBS=("update" "update --check" "update --firmware-only" "rollback" "pin" "channel set stable"
           "boot migrate" "live apply" "live rollback" "live soft-reboot --yes")
    for v in "${VERBS[@]}"; do
        # shellcheck disable=SC2086  # the verb is words on purpose
        for r in moved running halfrolled rbnofail garbage; do
            out="$(rime_at "$r" $v 2>&1)"; rc=$?
            if [[ $rc -eq 3 ]] && pipe_has -F "REFUSED [nixos-migration-started]" <<<"$out" \
               && ! pipe_has 'must run as root' <<<"$out"; then
                ok "rime $v, record '$r': refused by name, before the root gate"
            else
                bad "rime $v, record '$r'" "rc=$rc $out"
            fi
        done
        # shellcheck disable=SC2086
        for r in rolledback absent; do
            out="$(rime_at "$r" $v 2>&1)"; rc=$?
            if ! pipe_has 'REFUSED' <<<"$out" && pipe_has 'must run as root' <<<"$out"; then
                ok "rime $v, record '$r': not refused (the root gate answers)"
            else
                bad "rime $v, record '$r' should not refuse" "rc=$rc $out"
            fi
        done
    done
    out="$(rime_at moved update 2>&1)"
    pipe_has -F 'Rime (generation' <<<"$out" && pipe_has -F 'sudo rime update' <<<"$out" \
        && ok "the refusal names the new boot entry and where updates are now" || bad "refusal text" "$out"
    out="$(rime_at running update 2>&1)"
    pipe_has -F 'install closure' <<<"$out" && pipe_has -F 'was cut off (a crash or a' <<<"$out" \
        && pipe_has -F 'record: ' <<<"$out" \
        && ok "an unfinished start names its last step and the record, and promises no rollback it lacks" \
        || bad "unfinished text" "$out"
    [[ ! -e "$W/bootc.calls" ]] && ok "no gated verb reached bootc, refused or not" \
        || bad "bootc was called" "$(cat "$W/bootc.calls")"
    # Reading verbs and the bridge itself stay open after a start.
    for v in "boot status" "channel status" "migrate --help"; do
        # shellcheck disable=SC2086
        out="$(rime_at moved $v 2>&1)"
        ! pipe_has 'REFUSED' <<<"$out" && ok "rime $v is not held by the move" || bad "rime $v refused" "$out"
    done

    sec "rime migrate: the bridge gets every argument"
    # No gate in front of the bridge: whatever start is given reaches it as is
    # (here the bridge is absent, so rime says so; on a Rime image it answers).
    out="$(RIME_BRIDGE_ROOT="$W/rec/absent" "$RIME_BIN" migrate start --backup "$W/backup" --payload "$W/nowhere" --yes 2>&1)"; rc=$?
    if [[ -x /usr/libexec/rime-migrate ]]; then
        ! pipe_has -F 'REFUSED [payload' <<<"$out" && ok "start went to the bridge (rc=$rc)" || bad "start" "rc=$rc $out"
    elif [[ $rc -eq 1 ]] && pipe_has -F 'cannot run the bridge (/usr/libexec/rime-migrate)' <<<"$out" \
         && ! pipe_has 'REFUSED' <<<"$out"; then
        ok "start is handed to /usr/libexec/rime-migrate with nothing in front of it"
    else
        bad "start should reach the bridge" "rc=$rc $out"
    fi
fi

printf '\nrime migrate: %d passed, %d failed\n' "$pass" "$fail"
[[ $fail -eq 0 ]]
