# checks.x86_64-linux.delta: the G2 delta tool (nix/release/delta/rime_delta.py, docs/nixos/delta.md) without a
# network or a VM.
#
# Two releases of a small multi-output package (out + dev) and one package only the new release has. The check
# pairs them, measures the deltas, rebuilds the new NAR on the client side from the old path and the delta, checks
# it against a signed narinfo, and imports it into a chroot store with `nix copy` (Nix checks the signature and the
# hash again). Then the refusals: a corrupted delta, a narinfo whose NarHash names another path, a delta applied to
# the wrong base, a narinfo signed by an untrusted key. Last, mutants of the tool: each must make this check fail.
{ pkgs, self }:

let
  py = pkgs.python3.withPackages (ps: [ ps.cryptography ]);

  # A deterministic 3 MiB blob with a few edits per version, a versioned text file, and a dev output.
  fixture =
    name: ver: edits:
    pkgs.runCommand "${name}-${ver}"
      {
        outputs = [ "out" "dev" ];
        nativeBuildInputs = [ pkgs.python3 ];
      }
      ''
        mkdir -p $out/share $out/bin $dev/include
        python3 - <<'EOF'
        import os, random
        blob = bytearray(random.Random(1).randbytes(3 << 20))
        for off in [${edits}]:
            blob[off:off + 64] = random.Random(off).randbytes(64)
        open(os.environ["out"] + "/share/blob", "wb").write(blob)
        EOF
        printf '#!/bin/sh\necho ${name} ${ver}\n' > $out/bin/${name}
        chmod +x $out/bin/${name}
        printf '#define VERSION "${ver}"\n' > $dev/include/${name}.h
      '';

  old = fixture "rime-delta-fixture" "1.0" "";
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
        tsv $oldOut $oldDev > old.tsv
        tsv $newOut $newDev $onlyNew > new.tsv

        echo "== pair"
        $tool pair old.tsv new.tsv pairs.tsv
        cand() { grep -F "$1"$'\t' pairs.tsv | cut -f5,6; }
        [ "$(cand "$newOut")" = "same-package"$'\t'"$oldOut=$(nix-store --dump $oldOut | wc -c)" ] ||
          fail "out paired with: $(cand "$newOut")"
        [ "$(cand "$newDev")" = "same-package"$'\t'"$oldDev=$(nix-store --dump $oldDev | wc -c)" ] ||
          fail "dev paired with: $(cand "$newDev") (outputs must pair with the same output)"
        [ "$(cand "$onlyNew")" = "none"$'\t' ] || fail "a new package got a base: $(cand "$onlyNew")"

        echo "== measure (every delta is decoded again and compared with the NAR)"
        $tool measure pairs.tsv res.tsv -j 2 --keep deltas
        $tool summary res.tsv
        col() { grep -F "$1"$'\t' res.tsv | cut -f"$2"; }
        full=$(col "$newOut" 8) delta=$(col "$newOut" 10)
        [ "$delta" -lt $((full / 20)) ] || fail "delta $delta B is not much smaller than the full $full B"
        [ "$(col "$onlyNew" 6)" = "" ] && [ "$(col "$onlyNew" 10)" = "$(col "$onlyNew" 8)" ] ||
          fail "a path with no base must cost its full size"
        hp=$(basename $newOut | cut -c1-32)
        [ -s deltas/$hp.delta ] || fail "measure --keep wrote no delta for $newOut"

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
  };
in
run tool
// {
  # Each must FAIL: nix build path:.#checks.x86_64-linux.delta.mutants.<name>
  mutants = builtins.mapAttrs (_: run) mutants;
}
