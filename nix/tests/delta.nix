# checks.x86_64-linux.delta: the G2 delta tool (nix/release/delta/rime_delta.py, docs/nixos/delta.md) without a
# network or a VM.
#
# Two releases of a small multi-output package (out + dev) and one package only the new release has. The check
# pairs them, measures the deltas (and `measure` must refuse a delta that decodes to the wrong bytes), rebuilds the
# new NAR on the client side from the old path and the delta, checks it against a signed narinfo, and imports it
# into a chroot store with `nix copy` (Nix checks the signature and the hash again). Then the refusals: a corrupted
# delta, a narinfo whose NarHash names another path, a delta applied to the wrong base, a narinfo signed by an
# untrusted key. Last, mutants of the tool: each must make this check fail.
#
# Two candidates and references (verify:g2 3, wave 4 integration): the old release also has a decoy of the same
# package (same NAR size, other bytes), listed first, so `measure` tries two bases and must keep the smaller delta,
# not the first; and the server's narinfos say the new path that sorts first references the one that sorts last, so
# `apply-all` must import references first (in path order, `nix copy --no-recursive` refuses the referrer).
{ pkgs, self }:

let
  py = pkgs.python3.withPackages (ps: [ ps.cryptography ]);

  # A deterministic 3 MiB blob with a few edits per version, a versioned text file, and a dev output.
  fixtureSeed =
    name: ver: seed: edits:
    pkgs.runCommand "${name}-${ver}"
      {
        outputs = [ "out" "dev" ];
        nativeBuildInputs = [ pkgs.python3 ];
      }
      ''
        mkdir -p $out/share $out/bin $dev/include
        python3 - <<'EOF'
        import os, random
        blob = bytearray(random.Random(${toString seed}).randbytes(3 << 20))
        for off in [${edits}]:
            blob[off:off + 64] = random.Random(off).randbytes(64)
        open(os.environ["out"] + "/share/blob", "wb").write(blob)
        EOF
        printf '#!/bin/sh\necho ${name} ${ver}\n' > $out/bin/${name}
        chmod +x $out/bin/${name}
        printf '#define VERSION "${ver}"\n' > $dev/include/${name}.h
      '';

  fixture = name: ver: fixtureSeed name ver 1;
  old = fixture "rime-delta-fixture" "1.0" "";
  # Same package, same NAR size, every byte of the blob different: a base that gives no useful delta.
  decoy = fixtureSeed "rime-delta-fixture" "0.9" 2 "";
  new = fixture "rime-delta-fixture" "1.1" "4096, 1048576, 2097152";
  only-new = pkgs.runCommand "rime-delta-newpkg-2.0" { } ''
    mkdir -p $out; head -c 65536 /dev/zero | tr '\0' x > $out/data
  '';

  run =
    toolSrc:
    pkgs.runCommand "rime-delta-check"
      {
        nativeBuildInputs = [ py pkgs.nix pkgs.zstd pkgs.gnugrep pkgs.diffutils ];
        oldOut = old.out;
        oldDev = old.dev;
        newOut = new.out;
        newDev = new.dev;
        onlyNew = only-new;
        decoyOut = decoy.out;
      }
      ''
        set -euo pipefail
        export HOME=$TMPDIR NIX_CONFIG="experimental-features = nix-command"
        tool="python3 -I ${toolSrc}"
        fail() { echo "FAIL: $*"; exit 1; }

        tsv() {
          printf 'path\tname\tnar_size\tnar_hash\n'
          for p in "$@"; do
            printf '%s\t%s\t%s\tsha256:%s\n' "$p" "$(basename "$p" | cut -c34-)" \
              "$(nix-store --dump "$p" | wc -c)" "$(nix-hash --type sha256 --base32 "$p")"
          done
        }
        tsv $decoyOut $oldOut $oldDev > old.tsv
        tsv $newOut $newDev $onlyNew > new.tsv

        echo "== pair"
        $tool pair old.tsv new.tsv pairs.tsv
        cand() { grep -F "$1"$'\t' pairs.tsv | cut -f5,6; }
        oldSize=$(nix-store --dump $oldOut | wc -c) decoySize=$(nix-store --dump $decoyOut | wc -c)
        [ "$oldSize" = "$decoySize" ] || fail "the decoy's NAR ($decoySize B) must be as large as the old out's ($oldSize B)"
        # equal distance from the new size: the order of old.tsv stands, so the decoy is tried first
        [ "$(cand "$newOut")" = "same-package"$'\t'"$decoyOut=$decoySize,$oldOut=$oldSize" ] ||
          fail "out paired with: $(cand "$newOut") (expected the decoy, then the old out)"
        [ "$(cand "$newDev")" = "same-package"$'\t'"$oldDev=$(nix-store --dump $oldDev | wc -c)" ] ||
          fail "dev paired with: $(cand "$newDev") (outputs must pair with the same output)"
        [ "$(cand "$onlyNew")" = "none"$'\t' ] || fail "a new package got a base: $(cand "$onlyNew")"

        echo "== measure (every delta is decoded again and compared with the NAR)"
        $tool measure pairs.tsv res.tsv -j 2 --keep deltas
        $tool summary res.tsv
        col() { grep -F "$1"$'\t' res.tsv | cut -f"$2"; }
        full=$(col "$newOut" 8) delta=$(col "$newOut" 9)
        [ "$delta" -lt $((full / 20)) ] || fail "delta $delta B is not much smaller than the full $full B"
        [ "$(col "$newOut" 5)" = 2 ] || fail "measure tried $(col "$newOut" 5) bases for out, not 2"
        [ "$(col "$newOut" 6)" = "$oldOut" ] || fail "measure kept base $(col "$newOut" 6), not the smaller delta's $oldOut"
        [ "$(col "$onlyNew" 6)" = "" ] && [ "$(col "$onlyNew" 9)" = "$(col "$onlyNew" 8)" ] ||
          fail "a path with no base must cost its full size"
        hp=$(basename $newOut | cut -c1-32)
        [ -s deltas/$hp.delta ] || fail "measure --keep wrote no delta for $newOut"

        echo "== measure refuses a delta that does not rebuild the NAR (a zstd shim flips one byte of what it decodes)"
        mkdir -p shim
        cat > shim/zstd <<'EOF'
        #!${pkgs.runtimeShell}
        dst= dec= patch= prev=
        for a in "$@"; do
          case $a in -d) dec=1 ;; --patch-from=*) patch=1 ;; esac
          [ "$prev" = -o ] && dst=$a
          prev=$a
        done
        ${pkgs.lib.getBin pkgs.zstd}/bin/zstd "$@" || exit $?
        if [ -n "$dec" ] && [ -n "$patch" ] && [ -n "$dst" ]; then
          python3 -c 'import sys; f = open(sys.argv[1], "r+b"); f.seek(100); b = f.read(1); f.seek(100); f.write(bytes([b[0] ^ 0xFF]))' "$dst"
        fi
        EOF
        chmod +x shim/zstd
        if PATH=$PWD/shim:$PATH $tool measure pairs.tsv res-bad.tsv -j 1 > measure-bad.out 2>&1; then
          cat measure-bad.out; fail "measure accepted a delta that does not rebuild the NAR"
        fi
        grep -F "does not rebuild the NAR" measure-bad.out >/dev/null || { cat measure-bad.out; fail "measure: no 'does not rebuild the NAR'"; }
        echo "refused as expected: measure with a decoder that corrupts its output"

        echo "== a signed narinfo for the new path (the server side), a key the client trusts and one it does not"
        nix key generate-secret --key-name rime-delta-test-1 > sk
        nix key convert-secret-to-public < sk > pk
        nix key generate-secret --key-name rime-delta-other-1 > sk-other
        narinfo() { # narinfo PATH NARHASH_OF KEYFILE
          python3 -I - "$1" "$2" "$3" <<'EOF'
        import base64, subprocess, sys
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        path, hash_of, keyfile = sys.argv[1:]
        nar_hash = "sha256:" + subprocess.run(["nix-hash", "--type", "sha256", "--base32", hash_of], check=True,
                                              capture_output=True, text=True).stdout.strip()
        nar_size = len(subprocess.run(["nix-store", "--dump", path], check=True, capture_output=True).stdout)
        name, secret = open(keyfile).read().strip().split(":", 1)
        key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(secret)[:32])
        fp = f"1;{path};{nar_hash};{nar_size};"
        sig = base64.b64encode(key.sign(fp.encode())).decode()
        print(f"StorePath: {path}\nURL: nar/unused.nar.zst\nCompression: zstd\nNarHash: {nar_hash}\n"
              f"NarSize: {nar_size}\nReferences: \nSig: {name}:{sig}")
        EOF
        }
        narinfo $newOut $newOut sk > good.narinfo
        narinfo $newOut $newDev sk > wrong-hash.narinfo
        narinfo $newOut $newOut sk-other > other-key.narinfo
        head -c 100 deltas/$hp.delta > bad.delta
        python3 -c 'import sys; d = bytearray(open(sys.argv[1], "rb").read()); d[-9] ^= 0xFF; sys.stdout.buffer.write(d)' \
          deltas/$hp.delta > flipped.delta

        echo "== apply: rebuild, check against the narinfo, import into a chroot store (the client)"
        client=$TMPDIR/client
        $tool apply --narinfo good.narinfo --delta deltas/$hp.delta --base $oldOut --into w1 \
          --import-to $client --trusted-key "$(cat pk)" | tee apply.json
        diff -r $client$newOut $newOut || fail "the imported path differs from the release's"
        nix path-info --store $client $newOut >/dev/null || fail "$newOut is not valid in the client store"
        nix store verify --store $client --trusted-public-keys "$(cat pk)" $newOut || fail "nix store verify"

        echo "== refusals: each must exit non-zero, say why, and import nothing"
        refuse() { # refuse NAME EXPECTED-TEXT ARGS...
          local name=$1 want=$2; shift 2
          local c=$TMPDIR/client-$name
          if $tool apply --into w-$name --import-to $c --trusted-key "$(cat pk)" "$@" > out-$name 2>&1; then
            cat out-$name; fail "$name was accepted"
          fi
          grep -F -- "$want" out-$name >/dev/null || { cat out-$name; fail "$name: no '$want' in the refusal"; }
          [ ! -e $c$newOut ] || fail "$name: the path reached the store"
          echo "refused as expected: $name"
        }
        refuse flipped-delta "the delta does not decode" --narinfo good.narinfo --delta flipped.delta --base $oldOut
        refuse truncated-delta "the delta does not decode" --narinfo good.narinfo --delta bad.delta --base $oldOut
        refuse wrong-narhash "the signed narinfo says" --narinfo wrong-hash.narinfo --delta deltas/$hp.delta --base $oldOut
        refuse wrong-base "refused" --narinfo good.narinfo --delta deltas/$hp.delta --base $oldDev
        refuse untrusted-key "nix copy refused" --narinfo other-key.narinfo --delta deltas/$hp.delta --base $oldOut

        echo "== apply-all (the G3 benchmark): a machine on the old release moves to the new one"
        cachepath() { # cachepath DIR PATH...: a file binary cache entry per path (NAR + narinfo signed with sk);
          # $REFS = "PATH=REF ..." gives a path References (in the narinfo and in the signed fingerprint)
          python3 -I - "$@" <<'EOF'
        import base64, hashlib, os, subprocess, sys
        refs = {}
        for pair in os.environ.get("REFS", "").split():
            p, r = pair.split("=", 1)
            refs.setdefault(p, []).append(r)
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        d, paths = sys.argv[1], sys.argv[2:]
        os.makedirs(d + "/nar", exist_ok=True)
        open(d + "/nix-cache-info", "w").write("StoreDir: /nix/store\n")
        name, secret = open("sk").read().strip().split(":", 1)
        key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(secret)[:32])
        for path in paths:
            nar = subprocess.run(["nix-store", "--dump", path], check=True, capture_output=True).stdout
            nar_hash = "sha256:" + subprocess.run(["nix-hash", "--type", "sha256", "--base32", path], check=True,
                                                  capture_output=True, text=True).stdout.strip()
            fh = nar_hash.split(":")[1]
            open(f"{d}/nar/{fh}.nar", "wb").write(nar)
            rs = sorted(refs.get(path, []))
            fp = f"1;{path};{nar_hash};{len(nar)};{','.join(rs)}"
            sig = base64.b64encode(key.sign(fp.encode())).decode()
            open(f"{d}/{os.path.basename(path)[:32]}.narinfo", "w").write(
                f"StorePath: {path}\nURL: nar/{fh}.nar\nCompression: none\nNarHash: {nar_hash}\nNarSize: {len(nar)}\n"
                f"References: {' '.join(os.path.basename(r) for r in rs)}\nSig: {name}:{sig}\n")
        EOF
        }
        machine=$TMPDIR/machine
        cachepath old-cache $oldOut $oldDev
        nix copy --from file://$PWD/old-cache --to $machine --option trusted-public-keys "$(cat pk)" $oldOut $oldDev
        # the path that sorts first references the one that sorts last: path order would import the referrer first
        first=$(printf '%s\n' $newOut $newDev $onlyNew | sort | head -n 1)
        last=$(printf '%s\n' $newOut $newDev $onlyNew | sort | tail -n 1)
        echo "server: $first references $last"
        REFS="$first=$last" cachepath server $newOut $newDev $onlyNew
        # The server keeps NARs only for paths with no base: a path with a delta must come from its delta.
        for p in $newOut $newDev; do
          rm server/nar/$(nix-hash --type sha256 --base32 $p).nar
        done
        $tool apply-all res.tsv --server server --deltas deltas --store $machine --into w-all \
          --trusted-key "$(cat pk)" --log all.jsonl | tee all.json
        for p in $newOut $newDev $onlyNew; do
          diff -r $machine$p $p || fail "apply-all: $p differs from the release's"
        done
        nix store verify --store $machine --trusted-public-keys "$(cat pk)" $newOut $newDev $onlyNew ||
          fail "apply-all: nix store verify"
        grep -F '"delta_paths": 2, "full_paths": 1' all.json >/dev/null || fail "apply-all: expected 2 delta + 1 full path"
        nix path-info --store $machine -r $first | grep -Fx "$last" >/dev/null ||
          fail "apply-all: $first was imported without its reference $last"

        touch $out
      '';

  tool = ../release/delta/rime_delta.py;

  # A copy of the tool with one line replaced; `run` on it must fail.
  mutant =
    name: from: to:
    pkgs.runCommand "rime_delta-${name}.py" { } ''
      grep -F -- ${pkgs.lib.escapeShellArg from} ${tool} >/dev/null || { echo "mutant ${name}: pattern gone"; exit 1; }
      substitute ${tool} $out --replace-fail ${pkgs.lib.escapeShellArg from} ${pkgs.lib.escapeShellArg to}
    '';

  mutants = {
    # the client no longer compares the rebuilt NAR with the narinfo's NarHash/NarSize
    no-narhash-check = mutant "no-narhash-check" ''if size != int(info["NarSize"]) or not narinfo_hash_matches(info["NarHash"], digest):'' "if False:";
    # outputs no longer pair with the same output (dev may take out's NAR as its base)
    pair-ignores-output = mutant "pair-ignores-output" ''return f"{pname}|{out}"'' ''return pname'';
    # a wrong Nix base-32 (digits in the wrong order): every NAR hash check must then fail
    nix32-reversed = mutant "nix32-reversed" ''return "".join(out)'' ''return "".join(reversed(out))'';
    # measure keeps no delta for the client
    keep-nothing = mutant "keep-nothing" "if a.keep and best_file:" "if False:";
    # apply-all fetches every path whole instead of rebuilding it from its delta
    apply-all-no-delta = mutant "apply-all-no-delta" ''if r["base"]:'' "if False:";
    # measure no longer checks that a delta decodes back to the new NAR
    measure-trusts-delta = mutant "measure-trusts-delta" "ok = sha256_file(back) == digest" "ok = True";
    # measure keeps the first base it tries instead of the smallest delta
    first-candidate-wins = mutant "first-candidate-wins" ''if not res["base"] or dsize < res["delta"]:'' ''if not res["base"]:'';
    # apply-all imports in path order, not references first
    no-references-first = mutant "no-references-first" "if rp in rows and rp != p:" "if False:";
  };
in
run tool
// {
  # Each must FAIL: nix build path:.#checks.x86_64-linux.delta.mutants.<name>
  mutants = builtins.mapAttrs (_: run) mutants;
}
