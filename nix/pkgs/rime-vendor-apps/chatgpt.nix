# ChatGPT as Rime ships it (the catalogue's `chatgpt`, D5): OpenAI's own rpm, pinned and checked against OpenAI's
# signing key (./sources.nix), packaged with nixpkgs' chatgpt recipe (its asar fix, resource layout, launcher and
# wrapper), which unpacks OpenAI's deb of the same build. Only the source changes: the rpm whose signature rime-pkg
# checks, instead of the deb nixpkgs takes on its sha256 alone.
#
# Shipped from the rpm: usr/ only. Its /etc/yum.repos.d/chatgpt.repo (OpenAI's update channel) and AppArmor profile
# are left out, as on bootc: ChatGPT changes only with a Rime release.
{
  chatgpt,
  rpm,
  cpio,
  sources,
}:

let
  pin = sources.pins.chatgpt;
in
chatgpt.overrideAttrs (old: {
  pname = "rime-chatgpt";
  inherit (pin) version;
  src = "${sources.chatgpt}/chatgpt.rpm";

  nativeBuildInputs = old.nativeBuildInputs ++ [ rpm cpio ];

  # The rpm has the deb's tree (usr/lib/chatgpt, usr/bin, usr/share) plus etc/; nixpkgs' recipe works in ./root
  # (its sourceRoot: stdenv changes into it after this phase).
  unpackPhase = ''
    runHook preUnpack
    mkdir root
    # Through a file, not a pipe: cpio may exit at the archive trailer while rpm2cpio still writes padding,
    # and under pipefail rpm2cpio's "Error writing archive: Write error" then fails the build (seen on a hosted run).
    rpm2cpio "$src" > payload.cpio
    (cd root && cpio -idm --quiet --no-absolute-filenames < ../payload.cpio)
    rm payload.cpio
    test -x root/usr/lib/chatgpt/ChatGPT
    chmod -R u+w root
    runHook postUnpack
  '';

  passthru = old.passthru // {
    verified = sources.chatgpt;
    inherit pin;
  };

  meta = old.meta // {
    description = "ChatGPT desktop app from OpenAI's signed rpm, as Rime ships it";
    mainProgram = "chatgpt";
  };
})
