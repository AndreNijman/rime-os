# The bridge payload's producer side (nix/pkgs/rime-bridge payload-build.sh + payload-release.sh, the release
# pipeline's `payload` job; docs/nixos/bridge.md "Where a payload comes from"), without a network or a VM:
#
#   bridge-payload   a payload built from a tiny toplevel-shaped fixture (sw/bin/nix-store, sw/bin/rime-hostlayer, an
#                    etc/ with a link, a plain file and a link to nothing) has exactly the layout the bridge reads; a
#                    second build is byte-identical; the builder refuses a closure that is not the toplevel's and a
#                    non-empty OUTDIR. payload-release.sh signs it with a generated TEST key and the bridge's own
#                    verify_payload accepts it; a byte changed in store.tar.zst or manifest.json is refused, a secret
#                    whose public half is not the trusted line is refused and leaves no signature, an empty secret
#                    is refused. Then MUTANTS of both scripts: each must make the cases fail.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  bridge = pkgs.rime.rime-bridge;
  tools = pkgs.rime.rime-bridge-payload;
  src = ../pkgs/rime-bridge;

  fakeNix = pkgs.runCommand "nix-2.99-fixture" { } ''
    mkdir -p $out/bin
    printf '#!/bin/sh\nexit 0\n' > $out/bin/nix-store
    chmod +x $out/bin/nix-store
  '';
  fakeHostlayer = pkgs.runCommand "rime-hostlayer-fixture" { } ''
    mkdir -p $out/bin $out/share/rime-hostlayer
    echo 'print("fixture")' > $out/bin/rime-hostlayer
    printf '[default_session]\ncommand = "fixture"\n' > $out/share/rime-hostlayer/greetd-config.toml
  '';
  osRelease = pkgs.writeText "os-release" "ID=rime\n";
  toplevel = pkgs.runCommand "nixos-system-rime-fixture" { } ''
    mkdir -p $out/sw/bin $out/etc/sub
    ln -s ${fakeNix}/bin/nix-store $out/sw/bin/nix-store
    ln -s ${fakeHostlayer}/bin/rime-hostlayer $out/sw/bin/rime-hostlayer
    ln -s ${osRelease} $out/etc/os-release
    echo plain > $out/etc/sub/plain
    # a link to a file only the running system makes: left out of preview.tar (tar -h exits 1)
    ln -s /run/rime-fixture-never-there $out/etc/dangling
  '';
  closure = pkgs.closureInfo { rootPaths = [ toplevel ]; };
  otherClosure = pkgs.closureInfo { rootPaths = [ fakeNix ]; };

  # name, script, sed expression, what it breaks. Each must make the cases FAIL.
  mutants = [
    [ "release-any-key" "payload-release.sh" ''s/^\[\[ \$got == "\$want" \]\] || die/true || die/'' "a payload signed by a key bootc does not trust is released" ]
    [ "release-unverified" "payload-release.sh" ''s/^verify$/true/'' "a payload the bridge refuses is released" ]
    [ "release-keeps-signature" "payload-release.sh" ''s/^  \[\[ \$signed == 1 \]\] || rm -f -- "\$dir\/manifest.json.sig"$/  true/'' "a refused payload left looking signed" ]
    [ "build-etc-links" "payload-build.sh" ''s/tar -C "\$TOP" -h /tar -C "$TOP" /'' "preview.tar's etc/ holds links into a store bootc does not have" ]
    [ "build-any-closure" "payload-build.sh" ''s/^grep -qxF -- "\$TOP" "\$CI\/store-paths" || die/true || die/'' "a closure that is not the toplevel's packed" ]
  ];

  # The cases, against the scripts in directory $1 (payload-build.sh, payload-release.sh, payload-sign.sh and the
  # bridge as rime-migrate, side by side as in share/rime-bridge). Exit status 0 only if every case holds.
  cases = pkgs.writeText "payload-cases.sh" ''
    set -euo pipefail
    S=$1
    W=$(mktemp -d)
    fail() { echo "FAIL: $*"; exit 1; }
    flip() { python3 -c 'import sys; p = sys.argv[1]; b = bytearray(open(p, "rb").read()); b[100] ^= 0xff; open(p, "wb").write(b)' "$1"; }
    B() { bash "$S/payload-build.sh" "$@"; }
    R() { bash "$S/payload-release.sh" "$@"; }

    B ${toplevel} ${closure} "$W/p1" > /dev/null
    [ "$(cd "$W/p1" && ls | sort | tr '\n' ' ')" = "manifest.json preview.tar registration store.tar.zst " ] \
      || fail "payload files: $(ls "$W/p1")"
    python3 - "$W/p1/manifest.json" <<'PY' || fail "manifest.json"
    import json, sys
    m = json.load(open(sys.argv[1]))
    assert list(m) == ["toplevel", "nix", "store", "registration", "preview"], list(m)
    assert m["toplevel"] == "${toplevel}" and m["nix"] == "${fakeNix}", m
    for k, f in (("store", "store.tar.zst"), ("registration", "registration"), ("preview", "preview.tar")):
        assert list(m[k]) == ["file", "sha256"] and m[k]["file"] == f, m[k]
    PY
    cmp -s "$W/p1/registration" ${closure}/registration || fail "registration is not the closureInfo's"
    zstd -dc "$W/p1/store.tar.zst" | tar -t | grep -E '^nix/store/[^/]+/?$' | sed 's|/$||; s|^|/|' | sort -u > "$W/in-tar"
    sort ${closure}/store-paths | cmp -s - "$W/in-tar" || fail "store.tar.zst does not hold exactly the closure"
    mkdir "$W/pv"; tar -C "$W/pv" -xf "$W/p1/preview.tar"
    [ -f "$W/pv/rime-hostlayer" ] && [ ! -L "$W/pv/rime-hostlayer" ] || fail "preview: rime-hostlayer"
    grep -qx '\[default_session\]' "$W/pv/greetd-config.toml" || fail "preview: greetd-config.toml"
    [ -f "$W/pv/system/etc/os-release" ] && [ ! -L "$W/pv/system/etc/os-release" ] || fail "preview: etc/ not dereferenced"
    grep -qx ID=rime "$W/pv/system/etc/os-release" || fail "preview: etc/os-release contents"
    grep -qx plain "$W/pv/system/etc/sub/plain" || fail "preview: etc/sub/plain"
    [ ! -e "$W/pv/system/etc/dangling" ] && [ ! -L "$W/pv/system/etc/dangling" ] || fail "preview: a dangling link kept"
    [ -f "$W/pv/system/sw/bin/nix-store" ] && [ ! -s "$W/pv/system/sw/bin/nix-store" ] || fail "preview: sw/bin names"
    sleep 1.1   # a second build in another second: nothing in the payload may carry the time it was made
    B ${toplevel} ${closure} "$W/p2" > /dev/null
    cmp -s "$W/p1/manifest.json" "$W/p2/manifest.json" || fail "a second build differs"
    if B ${toplevel} ${otherClosure} "$W/p3" 2> "$W/err" > /dev/null; then fail "another closure accepted"; fi
    grep -q 'is not the closure of' "$W/err" || fail "another closure: $(cat "$W/err")"
    if B ${toplevel} ${closure} "$W/p1" 2> "$W/err" > /dev/null; then fail "a non-empty OUTDIR accepted"; fi
    grep -q 'is not empty' "$W/err" || fail "non-empty OUTDIR: $(cat "$W/err")"

    key() { # $1 = name, $2 = file stem: a Nix-format secret line and the public line
      openssl genpkey -algorithm ed25519 -out "$2.pem"
      { openssl pkey -in "$2.pem" -outform DER | tail -c 32; openssl pkey -in "$2.pem" -pubout -outform DER | tail -c 32; } > "$2.bin"
      echo "$1:$(base64 -w0 "$2.bin")" > "$2.sec"
      echo "$1:$(tail -c 32 "$2.bin" | base64 -w0)" > "$2.pub"
    }
    key rime-bridge-payload-TEST-1 "$W/k1"
    key rime-bridge-payload-TEST-1 "$W/k2"
    cp -r "$W/p1" "$W/bad-store"; cp -r "$W/p1" "$W/wrong"; cp -r "$W/p1" "$W/empty"
    RIME_BRIDGE_PAYLOAD_KEY=$(cat "$W/k1.sec") R "$W/k1.pub" "$W/p1" > /dev/null
    [ "$(cut -d: -f1 "$W/p1/manifest.json.sig")" = rime-bridge-payload-TEST-1 ] || fail "no signature by the TEST key"
    R --verify "$W/k1.pub" "$W/p1" || fail "--verify refused the released payload"

    cp -r "$W/p1" "$W/t"
    flip "$W/t/store.tar.zst"
    if R --verify "$W/k1.pub" "$W/t" 2> "$W/err"; then fail "a changed store.tar.zst accepted"; fi
    grep -q 'payload-corrupt' "$W/err" || fail "changed store.tar.zst: $(cat "$W/err")"
    cp "$W/p1/store.tar.zst" "$W/t/store.tar.zst"; printf ' ' >> "$W/t/manifest.json"
    if R --verify "$W/k1.pub" "$W/t" 2> "$W/err"; then fail "a changed manifest.json accepted"; fi
    grep -q 'payload-signature-bad' "$W/err" || fail "changed manifest: $(cat "$W/err")"
    if R --verify "$W/k2.pub" "$W/p1" 2> "$W/err"; then fail "a payload accepted against another key"; fi

    # release refusals: each leaves no manifest.json.sig behind
    flip "$W/bad-store/store.tar.zst"
    if RIME_BRIDGE_PAYLOAD_KEY=$(cat "$W/k1.sec") R "$W/k1.pub" "$W/bad-store" 2> "$W/err" > /dev/null; then
      fail "a payload the bridge refuses was released"
    fi
    grep -q 'payload-corrupt' "$W/err" || fail "corrupt release: $(cat "$W/err")"
    [ ! -e "$W/bad-store/manifest.json.sig" ] || fail "a refused payload kept its signature"
    if RIME_BRIDGE_PAYLOAD_KEY=$(cat "$W/k2.sec") R "$W/k1.pub" "$W/wrong" 2> "$W/err" > /dev/null; then
      fail "a secret that is not the trusted key's was used"
    fi
    grep -q 'is not the trusted key' "$W/err" || fail "wrong key: $(cat "$W/err")"
    [ ! -e "$W/wrong/manifest.json.sig" ] || fail "the wrong key's signature was left behind"
    if RIME_BRIDGE_PAYLOAD_KEY= R "$W/k1.pub" "$W/empty" 2> "$W/err" > /dev/null; then fail "an empty secret accepted"; fi
    grep -q 'RIME_BRIDGE_PAYLOAD_KEY is empty' "$W/err" || fail "empty secret: $(cat "$W/err")"
    chmod -R u+w "$W"; rm -rf "$W"
  '';
in
{
  bridge-payload = pkgs.runCommand "rime-bridge-payload"
    { nativeBuildInputs = with pkgs; [ python3 openssl zstd gnutar diffutils gnugrep gnused coreutils shellcheck ]; } ''
    set -euo pipefail
    shellcheck -s bash ${src}/payload-build.sh ${src}/payload-release.sh ${src}/payload-sign.sh
    mkdir good
    cp ${bridge}/share/rime-bridge/{payload-build.sh,payload-release.sh,payload-sign.sh,rime-migrate} good/
    bash ${cases} good
    echo "cases green on the package's scripts"
    # The wrappers the release job runs reach the same scripts, with their tools.
    ${tools}/bin/rime-bridge-payload-build ${toplevel} ${closure} wrapped > /dev/null
    k=$(mktemp -d)
    openssl genpkey -algorithm ed25519 -out $k/k.pem
    { openssl pkey -in $k/k.pem -outform DER | tail -c 32; openssl pkey -in $k/k.pem -pubout -outform DER | tail -c 32; } > $k/k.bin
    echo "rime-bridge-payload-TEST-1:$(tail -c 32 $k/k.bin | base64 -w0)" > $k/k.pub
    env -i PATH=/nonexistent RIME_BRIDGE_PAYLOAD_KEY="rime-bridge-payload-TEST-1:$(base64 -w0 $k/k.bin)" \
      ${tools}/bin/rime-bridge-payload-release $k/k.pub wrapped
    env -i PATH=/nonexistent ${tools}/bin/rime-bridge-payload-release --verify $k/k.pub wrapped
    echo "the wrappers build, sign and verify with no PATH of their own"
    ${lib.concatMapStrings (m:
      let name = builtins.elemAt m 0; file = builtins.elemAt m 1; expr = builtins.elemAt m 2; why = builtins.elemAt m 3; in ''
        mkdir m-${name}; cp good/* m-${name}/; chmod u+w m-${name}/*
        sed -i ${lib.escapeShellArg expr} m-${name}/${file}
        if cmp -s good/${file} m-${name}/${file}; then echo "mutant ${name}: sed changed nothing"; exit 1; fi
        if bash ${cases} m-${name} > m-${name}.log 2>&1; then echo "mutant ${name} SURVIVED (${why})"; cat m-${name}.log; exit 1; fi
        echo "mutant ${name} killed (${why}): $(grep '^FAIL' m-${name}.log | head -n1)"
      '') mutants}
    echo "bridge-payload: cases green, wrappers green, ${toString (builtins.length mutants)} mutants killed" | tee $out
  '';
}
