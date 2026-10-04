# The Android app: how it ships and how it updates

Rime Remote is the phone half of Rime OS. This page covers its
*distribution*: where the file comes from, what signs it, how it updates itself,
and what happens when the phone and the machine stop agreeing. What the app does
once installed is in `docs/remote.md`.

## Where a user gets it

The [Releases page](https://github.com/AndreNijman/apex-os/releases), which is <!-- rime-rename: keep: GitHub redirects the old repository name -->
already where `README.md` sends people for the netinstall ISO. The APK is the
same kind of artefact as the ISO, the thing you fetch *before* you have a Rime
machine to fetch it from, so it goes to the same place instead of to a registry
a phone cannot read.

A release carries five files:

| file | what it is |
|---|---|
| `rime-remote-<version>.apk` | the app |
| `rime-remote-<version>.apk.sha256` | what you check it against |
| `rime-remote-<version>.json` | what the in-app updater reads |
| `rime-remote-<version>.apk.sig` | a Sigstore signature over the APK |
| `rime-remote-<version>.apk.pem` | the certificate for that signature |

There is no Play Store listing and no F-Droid repository. The release build
produces an AAB as well as an APK, and `verifyReleaseSigning` checks both,
because a bundle is a jar and needs `jarsigner` where the APK needs
`apksigner`. The AAB is not published, because there is nowhere to publish it
to.

You cut a release with `.github/workflows/release-android.yml`, by hand
(`workflow_dispatch`), on `main` only. `dry_run` defaults to **true**: a
dispatch builds, signs, verifies and writes the page, and creates nothing. It
is the only workflow in this repository with `contents: write`, and it touches
no registry and no channel tag, so it cannot move what a booted machine tracks.

## Signing: two different signatures, and only one of them matters to a phone

**The APK signing key** is what Android cares about. Android refuses an update
signed by a different key than the installed app, so this key is the identity
of the app on every phone that has it. It lives in four repository secrets
(`APEX_KEYSTORE_BASE64`, `APEX_KEYSTORE_PASSWORD`, `APEX_KEY_ALIAS`, <!-- rime-rename: keep -->
`APEX_KEY_PASSWORD`: they kept their names through the rebrand, because a <!-- rime-rename: keep -->
secret's value cannot be read back to copy it), and nothing about it is in this
repository. The workflow
materialises it into a `0700` directory under `$RUNNER_TEMP`, `0600` on the
file, validates it with `keytool -list` before starting the build, and `shred`s
it afterwards in an `if: always()` step. That is the ritual `build-image.yml`
uses for the Secure Boot key, chosen on purpose over `pr-validation.yml`'s
looser one: a keystore in the workspace is a keystore an `upload-artifact`
step could publish.

v1 signing is **off**; v2 and v3 are on. `minSdk` is 28, so every phone this app
supports verifies v2, and v1 is the JAR-signature scheme whose Janus and Master
Key bug families are the reason v2 exists.

**The cosign signature** is the `.sig`/`.pem` pair, made keylessly under the
workflow's own identity, the same way this project signs its OS images. It does
not protect a phone. The `.sha256` and Android's own refusal to install an
update signed by a different key protect a phone. The cosign material
exists so the APK has the same provenance as everything else here, and so a
desktop can verify it without trusting the browser that downloaded it.

### If the key is lost

Every installed app is permanently unable to upgrade. The only escape is to
uninstall and reinstall, and uninstalling **destroys the paired device key**,
so every user has to walk back to their machine and scan a new QR code. There
is no server-side fix, and no amount of care afterwards undoes it.

Rotation differs from loss, and the difference was checked, not assumed.
`apksigner rotate` in build-tools 36.0.0 produces a
`SigningCertificateLineage` linking an old key to a new one under APK Signature
Scheme v3, which this app already enables. So:

* **While the old key still exists**, you can rotate: sign with the new key
  plus the lineage, and phones accept the update.
* **Once the old key is gone**, nothing can be rotated, because the key being
  replaced has to sign the lineage.

That makes the backup of this key the whole story, and it is why no agent took
the decision recorded in [android-signing.md](android-signing.md).

## versionCode: derived, and ratcheted twice

`versionCode` is `git rev-list --count` on the release commit. It is
deterministic (the same commit always yields the same number, so a re-run
cannot drift), and it needs no state file that could be lost or edited.

`android/tools/release-version.sh` then refuses to emit a bad one, which is its
real job:

1. **The release commit must be an ancestor of `main`.** A topic branch can
   have a lower count than `main`, and releasing from one would hand out a code
   that goes backwards. `--is-ancestor` answers 0 for yes, 1 for no and 128 for
   "could not look", and the script refuses on the third in its own right
   instead of folding it into the second.
2. **The new code must be strictly greater than every `android-v<N>` tag** that
   exists, locally *and* on the remote. Tags are the durable record: git refuses
   to move one, so the tag namespace is a ratchet that survives the script being
   rewritten. A `git ls-remote` that fails is a refusal, never an empty answer.
   "The network is down" and "there are no releases yet" look identical, and
   treating them the same is how a code lands below one already published.

The script refuses an equal code as well as a lower one. Re-releasing a commit
would put a second, different APK behind a code already installed, and Android
refuses that as surely as it refuses a lower one.

The local default is `1`, the lowest legal code, so a real release always
installs over a developer build and never the reverse. An `RIME_VERSION_CODE`
that is *present and malformed* is a hard failure, never a fallback: a fallback
is how a release ships as versionCode 1 and makes every later release
uninstallable.

`tests/test-android-release.sh` exercises all of this against a real fixture
repository with real tags and a real remote.

## Why THIS app self-updates when the desktop AI apps must not

On 2026-09-11 Andre ruled that the ChatGPT and Claude **desktop** apps must not
self-update: `sudo rime update` already moves them (they were baked into the
image then; since 2026-10-04 `rime install` puts them in the system extension
and every update fetches them again), so a second updater would compete with a
working one and the fleet would have two answers to "what version am I on".

**None of that reasoning reaches a phone, and this app breaks the rule on
purpose.** The phone is not running Rime OS. The APK is in no image. `rime
update` has no path to it, and there is no store in this picture either. The
choice here is between an updater and an app that moves only when its owner
remembers to go and look. A client that drifts away from the OS it talks to is
the failure this work exists to prevent.

So the app checks the Releases page and offers. It does **not** replace itself
quietly: `REQUEST_INSTALL_PACKAGES` earns the right to *ask*, and Android still
draws its own install prompt, as does GrapheneOS. The user always presses the
last button.

It also fails safe. No signal, a captive portal, a rate limit, a GitHub outage:
each of them leaves the app as it was, with no banner and no warning. The one
thing it volunteers uninvited is a download whose SHA-256 did not match the one
the release published, with *"your installed app has not changed"* next to it,
because the message otherwise reads as though something has already happened.

The APK never becomes a file the app owns. The app streams it from the release
straight into a `PackageInstaller` session while it computes the digest, and
calls `commit` only once the digest agrees: verify, then commit, never the
reverse. That is also why the app has no download directory, no `FileProvider`
and no `content://` URI: an APK that never lands in app storage cannot be a
second write path, cannot be left behind, and cannot be swapped between the
check and the install.

One deliberate omission: **the app stores no "last checked" timestamp**. It
would be a second write path into app storage, which `InsecureStorageTest`
walks and accounts for byte by byte. The cost is one small HTTP request per
launch.

### Not `/releases/latest`

Measured on 2026-09-20: this repository's Releases page already carried `v1.0.0`
and `v0.1.0`, both OS netinstall ISOs, and `/releases/latest` answered `v1.0.0`.
On 2026-09-28 it answers `v2.1.0`, another ISO release. An updater built on
that endpoint would find a release with no APK in it and conclude, every time
and in silence, that there was nothing to update to. The app picks Android
releases out of the full listing by their `android-v<code>` tag instead, and
`UpdatesTest` drives that against a listing shaped like the real one.

## When the app and the machine disagree about the protocol

`REMOTE_PROTOCOL_VERSION` versions the Android↔`rime-remoted` wire, and it is
**1**. (It is not the `PROTOCOL_VERSION = 11` in
`rimed/rime-agent-core/src/protocol.rs`. That one travels *inside* the Noise
tunnel, the app already parses it leniently and never compares it, and it needs
nothing.)

The version is hashed into the Noise prologue and **never transmitted**
(`rime-remote-core/src/noise.rs`). The daemon never inspects a version: it
builds the responder with its own constant, the AEAD tag fails, and it drops the
socket with one journald line. It treats a device that has been revoked
byte-for-byte the same way.

**This shipped as a real defect.** `Client.openSession` reported every version
mismatch as *"this machine does not accept this device: it is not paired, or it
has been revoked"*. Bumping `REMOTE_PROTOCOL_VERSION` would therefore have told
every installed phone it had been thrown out, which is the "half the time
people's things won't work" this work was asked to fix.

Two things fix it, and tests assert both:

* **A window instead of an equality test.** `SUPPORTED_REMOTE_PROTOCOL_VERSIONS`
  lists every revision the build can speak, newest first. Because the prologue
  is never sent, a client cannot *ask* which revision a desktop speaks, but it
  can try one, and a wrong guess costs a single connection and leaks nothing.
  So `openSessionAcrossVersions` dials once per revision until one completes.
  Pairing is the one moment the version *is* in the clear, in the QR offer, so
  `Client.pair` accepts any revision inside the window and handshakes at **the
  desktop's**, not at its own preferred one.
* **A message that names both causes.** When every revision is refused, the app
  says the device may have been unpaired *or* the two may no longer speak the
  same protocol, prints its own window, and points at `rime remote status` for
  the machine's. It never asserts the revocation, because from the phone the two
  are indistinguishable.

Only a refusal moves to the next revision. The app raises an unreachable
address, or a desktop whose static key is not the pinned one, as itself:
retrying it once per revision would turn one honest error into several
misleading ones and multiply the wait the user sits through.

The window today is `{1}`, and the Releases page says so in those words instead
of claiming a tolerance this build does not have. `release-artifacts.sh` reads
the window out of `Client.kt`, refuses anything that is not a literal list of
integers, and carries it into the release metadata; `release-notes.sh` reads it
back. If the window cannot be read, the release does not happen.

**Adding an entry to that window is a promise**: the build must hold a whole
session at that revision, not merely complete its handshake. Removing the oldest
entry drops support for machines that have not updated, and belongs in the
release notes, because to the owner of such a machine it is indistinguishable
from the app breaking.

## The terminal: when it repaints, and what a screen reader hears

Two things about the terminal screen were true until 2026-09-22. Both read as
limits of the tooling, neither was, and that is why they are written down here.

**It repainted for ever.** The screen ran `withFrameNanos { }` in a loop for the
life of the composition and compared `Terminal.revision` on each pass. That
coalesced a fast `cat` into one repaint per displayed frame, which is right, but
`withFrameNanos` is a *request* for the next frame: with nothing happening, the
loop asked the Choreographer for sixty frames a second over a terminal where
nothing had changed. It also meant Compose's `Recomposer` was never idle, and
every harness built on `ComposeTestRule` waits for idleness before `setContent`
returns, so "this screen cannot be tested by the standard harness" was a true
observation with a false cause. The loop now waits for a change before it asks
for a frame; `Terminal.onChanged` is the signal for bytes, and
`TerminalController.invalidate()` for everything the user does to the view. The
rules are in `TerminalFrames`, and JVM tests assert them against
`BroadcastFrameClock`, which asks the same question the `Recomposer` asks
itself instead of a proxy for it.

`foundation`'s bytecode holds a second perpetual animation nobody had noticed,
and reading the bytecode beats remembering the documentation: the hidden input
field set `cursorBrush = SolidColor(Color.Transparent)`, and
`TextFieldCursorKt.cursor` skips the blink only for
`SolidColor(Color.Unspecified)`. A transparent cursor is a *specified* one, so
that field blinked an invisible cursor for as long as it held focus.

**Nothing on it reached a screen reader.** `TerminalView` paints the grid with
`drawText`, so not one glyph is composed and the semantics tree under that
screen was empty: TalkBack had nothing to announce on a terminal full of
output. The view now carries two nodes, and there are two because the
requirements conflict:

* the grid's node holds the visible buffer as `text`, so TalkBack can move
  through it line by line at the granularity the user chose, with a
  `stateDescription` naming the cursor position and whether the view is live or
  scrolled back, a labelled click action, and custom actions for "back to the
  live output" and "copy what is on screen", the two things that are otherwise
  a target on a grid nobody can see;
* a separate one-dp node is the polite live region. `TerminalAnnouncer` decides
  the announcements: only lines the cursor has moved past, only while the view
  is following, one utterance per burst with a count of what it left out, never
  the same words twice running, and nothing at all for the replay a reconnect
  starts with. A live region on the grid node would re-read the whole screen on
  every byte, and the user would hear the first two words of forty
  interruptions.

A full-screen TUI repainting its own grid gets one announcement and then
silence. That limit is a choice: the buffer stays there to read, and an
announcer that read out each repaint of Claude Code's interface would make the
app unusable with TalkBack on.

## What has NOT been proved here

No device is attached to this project, so no test executes anything below the
download:

* The `PackageInstaller` session, the permission, and the install prompt are
  Android API glue. The half that can be quietly wrong (which release to read,
  whether a build is newer, whether the bytes are the published ones) is in
  `:core` and is tested on a JVM. The glue is deliberately thin so the untested
  part stays small.
* No release has ever been cut, and nobody has dispatched `release-android.yml`,
  even as a dry run: its run list is empty (checked 2026-09-28). Local tests
  exercise the workflow as far as a machine without the signing secrets can go;
  `dry_run` exists so the rest can be proved without publishing.
* JVM tests assert the terminal's semantics on the values handed to Android (a
  `SemanticsPropertyReceiver` is one method, so a JVM test implements it and
  reads back what was set), and not on what TalkBack says with them.
  `TerminalHarnessOnDeviceTest` asserts that the standard Compose harness can
  host the screen and that the grid reaches the tree on a real phone. Nobody has
  run it: no device, and the emulator on this machine has never booted.

## The signing key

**The key exists.** Checked, not assumed, on 2026-09-28: `gh secret list -R
AndreNijman/apex-os` lists `APEX_KEYSTORE_BASE64`, `APEX_KEYSTORE_PASSWORD`, <!-- rime-rename: keep -->
`APEX_KEY_ALIAS` and `APEX_KEY_PASSWORD`, all set on 2026-09-20, beside <!-- rime-rename: keep -->
`APEX_SB_CRT_B64` and `APEX_SB_KEY_B64`. <!-- rime-rename: keep --> Its certificate fingerprint is
published in `android/signing-certificate.sha256`, in `README.md` and in
android-signing.md.

**[android-signing.md](android-signing.md)** settles where that key is
generated, where its only authoritative copy lives, and how to rotate it. The
short version: the key is
generated offline with `android/tools/generate-signing-key.sh`, the
authoritative copy is Andre's and offline, GitHub holds a working copy that
*cannot be read back*, and a release with no key, or with a key this repository
has not published, refuses instead of shipping something one person could
install and then never update.

`pr-validation.yml` does not sign at all. It builds the release target on every
Android change because `lintVitalRelease` only runs there, and asserts the APK
is named `unsigned`; the signing key appears in exactly one workflow.
