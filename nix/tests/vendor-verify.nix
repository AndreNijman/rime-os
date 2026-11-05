# D5's vendor-key check on NixOS (nix/pkgs/rime-vendor.nix): rime-pkg's own vendor_fetch_chatgpt and
# vendor_fetch_claude_desktop, cut out of the script, run against file:// repositories built here with THROWAWAY keys
# (made in the build, never in the repository). The pins are rewritten in a copy of the verifier, as
# tests/test-rime-pkg-rpm6.sh does on bootc; the product verifier has no seam that could change them.
#
#   accepted   a repository whose files verify (both apps), and fetchVerified's layout of the same files
#   refused    each check that guards the route, one broken at a time, with rime-pkg's own reason:
#              claude-desktop: InRelease signed by another key, the deb's archive key is not the pinned one, the deb
#              does not match the sha256 Packages publishes, Packages is not named in the signed InRelease;
#              chatgpt: rpm signed by another key, the rpm no longer carries the pinned key.
{ pkgs, self }:

let
  vendor = pkgs.rime.rime-vendor;
  tools = with pkgs; [ gnupg rpm binutils gnutar gzip coreutils gnused gnugrep gawk ];
in
{
  vendor-verify = pkgs.runCommand "rime-vendor-verify" { nativeBuildInputs = tools; } ''
    set -eu
    export HOME=$TMPDIR
    K=$TMPDIR/keys; mkdir -p $K
    mkkey() {
      mkdir -p $K/$1; chmod 700 $K/$1
      gpg --homedir $K/$1 --batch --pinentry-mode loopback --passphrase "" \
        --quick-gen-key "Rime test $1 <$1@example.invalid>" ed25519 sign never 2>/dev/null
      gpg --homedir $K/$1 --armor --export > $K/$1.asc
    }
    fpr() { gpg --homedir $K/$1 --with-colons --fingerprint | awk -F: '/^fpr:/ && !d {print $10; d=1}'; }
    mkkey vendor; mkkey other; mkkey other2
    V=$(fpr vendor)

    # The verifier with BOTH pins pointed at the throwaway vendor key: a copy of rime-pkg's block and of the wrapper.
    sed -e "s/^readonly CHATGPT_KEY_FPR=.*/readonly CHATGPT_KEY_FPR=$V/" \
        -e "s/^readonly CLAUDE_KEY_FPR=.*/readonly CLAUDE_KEY_FPR=$V/" ${vendor.block} > $TMPDIR/block.sh
    grep -c "_KEY_FPR=$V" $TMPDIR/block.sh | grep -x 2
    sed "s|${vendor.block}|$TMPDIR/block.sh|" ${vendor.verifier}/bin/rime-vendor-verify > $TMPDIR/verify
    chmod +x $TMPDIR/verify
    grep -F "$TMPDIR/block.sh" $TMPDIR/verify > /dev/null

    # ── claude-desktop: an apt repository ──────────────────────────────────────────────────────────────────────────
    # $1 repo, $2 key carried in postinst, $3 key signing InRelease, $4 "tamper-deb" | "unnamed-packages" | ""
    mkapt() {
      local r=$1 b=$TMPDIR/deb-$RANDOM
      mkdir -p $b/ctl $b/data/usr/share/claude-desktop $r/dists/stable/main/binary-amd64 $r/pool
      printf 'Package: claude-desktop\nVersion: 1.2.3\nArchitecture: amd64\n' > $b/ctl/control
      { echo '#!/bin/sh'; echo 'cat > /dev/null <<KEY'; cat $K/$2.asc; echo KEY; } > $b/ctl/postinst
      echo marker > $b/data/usr/share/claude-desktop/marker
      echo 2.0 > $b/debian-binary
      tar -C $b/ctl -czf $b/control.tar.gz .
      tar -C $b/data -czf $b/data.tar.gz .
      (cd $b && ar rc $r/pool/claude-desktop_1.2.3_amd64.deb debian-binary control.tar.gz data.tar.gz)
      local sum; sum=$(sha256sum $r/pool/claude-desktop_1.2.3_amd64.deb | cut -d' ' -f1)
      printf 'Package: claude-desktop\nVersion: 1.2.3\nFilename: pool/claude-desktop_1.2.3_amd64.deb\nSHA256: %s\n\n' "$sum" \
        > $r/dists/stable/main/binary-amd64/Packages
      [ "$4" = tamper-deb ] && echo extra >> $r/pool/claude-desktop_1.2.3_amd64.deb
      local psum; psum=$(sha256sum $r/dists/stable/main/binary-amd64/Packages | cut -d' ' -f1)
      [ "$4" = unnamed-packages ] && psum=$(echo other | sha256sum | cut -d' ' -f1)
      printf 'Origin: test\nSuite: stable\nSHA256:\n %s 100 main/binary-amd64/Packages\n' "$psum" > $b/Release
      gpg --homedir $K/$3 --batch --pinentry-mode loopback --passphrase "" --clearsign -o $r/dists/stable/InRelease $b/Release
    }

    # ── chatgpt: an rpm-md repository ──────────────────────────────────────────────────────────────────────────────
    cat > $TMPDIR/p.spec <<'SPEC'
    Name: chatgpt
    Version: 1.0
    Release: 1
    Summary: vendor-verify fixture
    License: MIT
    BuildArch: noarch
    %description
    fixture
    %install
    install -Dm644 %{_sourcedir}/payload %{buildroot}%{ppath}
    %files
    %{ppath}
    SPEC
    sed -i 's/^    //' $TMPDIR/p.spec
    # $1 repo, $2 key the rpm carries (named by the PINNED fingerprint, or "none"), $3 signing key
    mkrpmmd() {
      local r=$1 b=$TMPDIR/rpm-$RANDOM
      mkdir -p $r/repodata $b/src $b/tmp
      if [ "$2" = none ]; then cp $K/$3.asc $b/src/payload; ppath=/usr/share/chatgpt/key.asc
      else cp $K/$2.asc $b/src/payload; ppath=/etc/pki/rpm-gpg/RPM-GPG-KEY-chatgpt-$V.asc; fi
      rpmbuild -bb --define "_topdir $b" --define "_sourcedir $b/src" --define "_tmppath $b/tmp" \
        --define "_rpmdir $r" --define '_build_name_fmt %%{NAME}-%%{VERSION}.rpm' \
        --define "ppath $ppath" --define '__os_install_post %{nil}' $TMPDIR/p.spec > $b/build.log 2>&1 \
        || { cat $b/build.log; exit 1; }
      rpmsign --addsign --define "_gpg_path $K/$3" --define "_gpg_name $3@example.invalid" \
        --define '_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback --passphrase ""' $r/chatgpt-1.0.rpm
      printf '<metadata><package><location href="chatgpt-1.0.rpm"/></package></metadata>\n' \
        | gzip > $r/repodata/0123abcd-primary.xml.gz
      printf '<repomd><data type="primary"><location href="repodata/0123abcd-primary.xml.gz"/></data></repomd>\n' \
        > $r/repodata/repomd.xml
    }

    pass=0
    accept() {  # $1 name, $2 repo, $3 case
      if $TMPDIR/verify $1 $2 $TMPDIR/out/$3 > $TMPDIR/log 2>&1; then
        echo "ok   $3: accepted"; cat $TMPDIR/log; pass=$((pass + 1))
      else echo "FAIL $3: refused"; cat $TMPDIR/log; exit 1; fi
    }
    refuse() {  # $1 name, $2 repo, $3 case, $4 reason rime-pkg must give
      if $TMPDIR/verify $1 $2 $TMPDIR/out/$3 > $TMPDIR/log 2>&1; then
        echo "FAIL $3: accepted"; cat $TMPDIR/log; exit 1
      fi
      grep -F -- "$4" $TMPDIR/log > /dev/null || { echo "FAIL $3: refused for another reason"; cat $TMPDIR/log; exit 1; }
      test ! -e $TMPDIR/out/$3 || { echo "FAIL $3: left a file behind"; exit 1; }
      echo "ok   $3: refused ($(tail -n1 $TMPDIR/log))"; pass=$((pass + 1))
    }

    mkapt $TMPDIR/apt-good vendor vendor ""
    mkapt $TMPDIR/apt-badsig vendor other ""
    mkapt $TMPDIR/apt-badkey other other ""
    mkapt $TMPDIR/apt-tamper vendor vendor tamper-deb
    mkapt $TMPDIR/apt-unnamed vendor vendor unnamed-packages
    mkrpmmd $TMPDIR/rpm-good vendor vendor
    mkrpmmd $TMPDIR/rpm-badsig vendor other
    mkrpmmd $TMPDIR/rpm-nokey none vendor

    accept claude-desktop $TMPDIR/apt-good claude-good
    refuse claude-desktop $TMPDIR/apt-badsig claude-badsig "InRelease is not signed by the pinned key"
    refuse claude-desktop $TMPDIR/apt-badkey claude-badkey "its archive key is $(fpr other), not the $V Rime pins"
    refuse claude-desktop $TMPDIR/apt-tamper claude-tamper "does not match the sha256 its index publishes"
    refuse claude-desktop $TMPDIR/apt-unnamed claude-unnamed "its package index is not named by the signed InRelease"
    accept chatgpt $TMPDIR/rpm-good chatgpt-good
    refuse chatgpt $TMPDIR/rpm-badsig chatgpt-badsig "does not verify against the pinned key"
    refuse chatgpt $TMPDIR/rpm-nokey chatgpt-nokey "no longer carries the key Rime pins"

    # The product verifier (real pins) refuses the throwaway vendor's files: the pins are not test keys.
    if ${vendor.verifier}/bin/rime-vendor-verify claude-desktop $TMPDIR/apt-good $TMPDIR/out/real > $TMPDIR/log 2>&1; then
      echo "FAIL: the shipped pins accepted a throwaway key"; exit 1
    fi
    grep -F "not the 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE Rime pins" $TMPDIR/log > /dev/null
    echo "ok   shipped pins: $(tail -n1 $TMPDIR/log)"; pass=$((pass + 1))

    echo "PASS: $pass vendor-verify cases"
    mkdir -p $out
    cp -r $TMPDIR/out $out/verified
  '';

  # fetchVerified lays its files out as a repository and fails the BUILD on a refusal. Shown with the shipped pins and
  # a throwaway repository: the derivation must fail, which `testers.testBuildFailure` captures.
  vendor-fetch-refuses =
    let
      repo = pkgs.runCommand "rime-vendor-fixture-apt" { nativeBuildInputs = with pkgs; [ gnupg gnutar gzip binutils ]; } ''
        export HOME=$TMPDIR
        mkdir -p k b/ctl b/data; chmod 700 k
        gpg --homedir k --batch --pinentry-mode loopback --passphrase "" --quick-gen-key "x <x@example.invalid>" ed25519 sign never 2>/dev/null
        { echo '#!/bin/sh'; gpg --homedir k --armor --export; } > b/ctl/postinst
        echo 'Package: claude-desktop' > b/ctl/control
        echo 2.0 > b/debian-binary
        tar -C b/ctl -czf b/control.tar.gz . && tar -C b/data -czf b/data.tar.gz .
        mkdir -p $out
        (cd b && ar rc $out/deb debian-binary control.tar.gz data.tar.gz)
        printf 'Package: claude-desktop\nVersion: 1\nFilename: pool/c.deb\nSHA256: %s\n\n' "$(sha256sum $out/deb | cut -d' ' -f1)" > $out/Packages
        printf 'SHA256:\n %s 1 main/binary-amd64/Packages\n' "$(sha256sum $out/Packages | cut -d' ' -f1)" > b/Release
        gpg --homedir k --batch --pinentry-mode loopback --passphrase "" --clearsign -o $out/InRelease b/Release
      '';
      failed = pkgs.testers.testBuildFailure (vendor.fetchVerified {
        name = "claude-desktop";
        files = {
          "dists/stable/InRelease" = "${repo}/InRelease";
          "dists/stable/main/binary-amd64/Packages" = "${repo}/Packages";
          "pool/c.deb" = "${repo}/deb";
        };
      });
    in
    pkgs.runCommand "rime-vendor-fetch-refuses" { } ''
      grep -F "refusing claude-desktop: its archive key is" ${failed}/testBuildFailure.log
      test "$(cat ${failed}/testBuildFailure.exit)" != 0
      touch $out
    '';
}
