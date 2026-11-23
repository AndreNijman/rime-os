#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  release-artifacts.sh — build the APK a person downloads, and refuse to call
#  anything else a release.
#
#  This is deliberately stricter than the same build in `pr-validation.yml`.
#  There, a missing signing key is a NOTICE: a pull request from a fork gets no
#  secrets, the artefacts are built unsigned to prove the release target still
#  compiles, and the job says so. Here, a missing key is a hard failure —
#  because the output of this script is the file somebody installs on their
#  phone, and an unsigned APK is not a release, it is a file that cannot be
#  installed at all.
#
#  WHAT IT EMITS, next to each other in --out:
#
#      rime-remote-<versionName>.apk          the thing you install
#      rime-remote-<versionName>.apk.sha256   what you check it against
#      rime-remote-<versionName>.json         what the app's updater reads
#
#  The `.json` is a contract, not a convenience: the in-app updater fetches it
#  to decide whether a newer build exists, and verifies the APK it then
#  downloads against the `sha256` in it. It also carries the protocol window
#  this build speaks, read out of `Client.kt` rather than repeated here, so the
#  Releases page can state the compatibility promise instead of implying one.
#
#  THE AAB IS BUILT AND NOT PUBLISHED. `verifyReleaseSigning` checks the bundle
#  as well as the APK — a bundle is a jar and needs jarsigner rather than
#  apksigner — and weakening that check to save a minute of build time would
#  leave the artefact a store actually takes unchecked. There is no Play
#  listing today, so it simply is not copied to --out.
#
#  Usage:
#      android/tools/release-artifacts.sh --out DIR --code N --name STRING
#                                         [--tag android-vN]
#
#  Signing comes from the environment, exactly as `app/build.gradle.kts`
#  expects: RIME_KEYSTORE, RIME_KEYSTORE_PASSWORD, RIME_KEY_ALIAS,
#  RIME_KEY_PASSWORD. Nothing here prints any of them.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

out=""; code=""; name=""; tag=""
while [ $# -gt 0 ]; do
    case "$1" in
        --out)  out="${2:?--out needs a value}"; shift 2 ;;
        --code) code="${2:?--code needs a value}"; shift 2 ;;
        --name) name="${2:?--name needs a value}"; shift 2 ;;
        --tag)  tag="${2:?--tag needs a value}"; shift 2 ;;
        -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
        *) echo "FATAL: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

fatal() { echo "FATAL: $*" >&2; exit 1; }

[ -n "$out" ]  || fatal "--out is required"
[ -n "$code" ] || fatal "--code is required"
[ -n "$name" ] || fatal "--name is required"
case "$code" in ""|*[!0-9]*) fatal "--code '$code' is not a number" ;; esac
[ -n "$tag" ] || tag="android-v$code"

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
android=$(dirname "$here")
repo=$(dirname "$android")

# ── The protocol revision this build prefers, and the whole window ───────────
#
# Read FIRST, before the keystore and long before Gradle. These two values go
# into the metadata the updater reads and onto the page a person reads, and a
# source file they cannot be read from must stop the release in a second
# rather than after a ten-minute build.
#
# Read out of the source rather than repeated here, and a refusal if the line
# has moved — a metadata file that quietly claims the wrong protocol revision
# would send a user to update the wrong side.
client="$android/core/src/main/kotlin/com/rimeos/remote/core/Client.kt"
[ -f "$client" ] || fatal "$client is missing, so the protocol revision cannot be read"
protocol=$(sed -n 's/^const val REMOTE_PROTOCOL_VERSION: Int = \([0-9]*\).*/\1/p' "$client" | head -1)
[ -n "$protocol" ] || fatal "no REMOTE_PROTOCOL_VERSION line in $client; it was renamed or removed"

# ── And the whole window, not just the preferred revision ────────────────────
#
# The Releases page tells people what happens when their machine and their app
# are at different ages, and that sentence is only true if it is written from
# the window this build actually ships. So the list is read here, refused if it
# is not a plain list of integers, and carried into the metadata — where
# `release-notes.sh` reads it back rather than guessing.
#
# A strict match on purpose. `listOf(REMOTE_PROTOCOL_VERSION)` or a list built
# at runtime would parse to nothing here, and a window that parsed to nothing
# must STOP the release rather than publish a page that quietly says "v" and a
# blank. That is why `Client.kt` writes the entry as a literal and a test holds
# it to the constant.
window_line=$(sed -n 's/^val SUPPORTED_REMOTE_PROTOCOL_VERSIONS: List<Int> = listOf(\([^)]*\)).*/\1/p' \
    "$client" | head -1)
[ -n "$window_line" ] \
    || fatal "no SUPPORTED_REMOTE_PROTOCOL_VERSIONS = listOf(...) line in $client; it was renamed, or it is no longer a literal list the release metadata can read"
case "$window_line" in
    *[!0-9,\ ]*) fatal "SUPPORTED_REMOTE_PROTOCOL_VERSIONS is '$window_line', which is not a plain list of integers. The release metadata cannot state a window it cannot read, and a page that claims compatibility it cannot name is worse than one that claims none." ;;
esac
window=$(printf '%s' "$window_line" | tr -d ' ')
case "$window" in
    ""|*,,*|,*|*,) fatal "SUPPORTED_REMOTE_PROTOCOL_VERSIONS is '$window_line', which is malformed" ;;
esac
# The preferred revision must be IN the window. The Kotlin test asserts this
# too; it is asserted again here because the two files can be edited apart and
# this is the last point before the number reaches a user's phone.
case ",$window," in
    *",$protocol,"*) : ;;
    *) fatal "REMOTE_PROTOCOL_VERSION is $protocol but SUPPORTED_REMOTE_PROTOCOL_VERSIONS is ($window_line); a build cannot prefer a revision it does not list" ;;
esac

# ── The signing key, or nothing ──────────────────────────────────────────────
#
# Checked HERE, before a build that takes minutes, and named one at a time so
# the message says which one is missing rather than "signing is not
# configured". The same reasoning as the Secure Boot step in build-image.yml:
# "Fail now rather than ship an unsigned image because a secret was pasted in
# mangled."
for var in RIME_KEYSTORE RIME_KEYSTORE_PASSWORD RIME_KEY_ALIAS RIME_KEY_PASSWORD; do
    eval "value=\${$var:-}"
    [ -n "$value" ] || fatal "$var is not set. A release APK must be signed: an unsigned one cannot be installed, and falling back to the debug key would ship an artefact signed by a key whose password is the word 'android'."
done
[ -f "$RIME_KEYSTORE" ] || fatal "RIME_KEYSTORE points at '$RIME_KEYSTORE', which is not a file"

# Prove the keystore really holds the alias before building. `keytool -list`
# with the wrong password fails here in two seconds instead of failing inside
# Gradle after the whole app has compiled.
if ! keytool -list -keystore "$RIME_KEYSTORE" -alias "$RIME_KEY_ALIAS" \
        -storepass:env RIME_KEYSTORE_PASSWORD >/dev/null 2>&1; then
    fatal "the keystore did not open with the supplied password, or it has no key called '$RIME_KEY_ALIAS'. Nothing about the key material is printed here on purpose."
fi

# ── And it must be the key we PUBLISHED ──────────────────────────────────────
#
# "It opened" only says a keystore and a password agree with each other. It
# says nothing about WHICH key is in there, and a secret replaced by a
# different key passes every other gate in this script: the build signs, the
# signature verifies, the release publishes, and every phone that already holds
# a Rime Remote build refuses the update for the rest of time. Android accepts
# an update only from the key that signed what is installed, and the owner's
# only escape is uninstalling — which destroys their pairing.
#
# So the keystore is compared against the fingerprint this repository
# publishes, here in two seconds, and the APK is compared again after the build
# against the same value. See android/signing-certificate.sha256.
"$here/verify-signing-identity.sh" --keystore "$RIME_KEYSTORE" --alias "$RIME_KEY_ALIAS" \
    || fatal "the signing key is not the one this repository publishes (above). Refusing before the build."

command -v python3 >/dev/null 2>&1 || fatal "python3 is required to write the metadata"

mkdir -p "$out" || fatal "cannot create $out"
out=$(cd "$out" && pwd)

# ── Build ────────────────────────────────────────────────────────────────────
cd "$android" || fatal "cannot enter $android"

export RIME_VERSION_CODE="$code"
export RIME_VERSION_NAME="$name"

# gradle-retry.sh runs it again only when Gradle failed fetching a plugin or a
# dependency (one bad answer from a repository is not a failed release).
"$here/gradle-retry.sh" ./gradlew --no-daemon --stacktrace :app:assembleRelease :app:bundleRelease \
    || fatal "the release build failed"

# `verifyReleaseSigning` reads the signature out of the artefacts with
# apksigner and jarsigner. It is a check, not a claim: without it "we sign it
# in CI" is prose.
./gradlew --no-daemon --stacktrace :app:verifyReleaseSigning \
    || fatal "the built artefacts are not signed, so this is not a release"

built=$(ls app/build/outputs/apk/release/*.apk 2>/dev/null | head -1)
[ -n "$built" ] || fatal "assembleRelease produced no APK under app/build/outputs/apk/release"
case "$built" in
    *unsigned*) fatal "$built is named unsigned, so the signing config did not apply even though the keystore opened" ;;
esac

# ── The name a person sees in their downloads ────────────────────────────────
apk="$out/rime-remote-$name.apk"
cp "$built" "$apk" || fatal "cannot copy the APK to $out"

# ── Who signed the file that is about to be uploaded ─────────────────────────
#
# The check above was on the INPUT. This one is on the output, read back out of
# the APK itself with apksigner, because that is the file a person installs and
# the only one whose signature is evidence rather than inference. It is the
# same rule the release workflow follows when it re-reads a release it has just
# created instead of trusting `gh release create`'s exit code.
"$here/verify-signing-identity.sh" --apk "$apk" \
    || fatal "the built APK is not signed by the certificate this repository publishes (above). Refusing to call this a release."

sum=$(sha256sum "$apk") || fatal "sha256sum failed"
digest="${sum%% *}"
printf '%s  %s\n' "$digest" "$(basename "$apk")" > "$apk.sha256"

size=$(stat -c%s "$apk") || fatal "cannot stat the APK"

commit=$(git -C "$repo" rev-parse HEAD 2>/dev/null) || fatal "cannot read the release commit"

# Written by python rather than by hand, so a version name containing a
# character JSON cares about produces valid JSON instead of a file the
# updater cannot parse.
RIME_JSON_CODE="$code" RIME_JSON_NAME="$name" RIME_JSON_TAG="$tag" \
RIME_JSON_APK="$(basename "$apk")" RIME_JSON_SHA="$digest" RIME_JSON_SIZE="$size" \
RIME_JSON_PROTOCOL="$protocol" RIME_JSON_COMMIT="$commit" RIME_JSON_WINDOW="$window" \
python3 -c '
import json, os
print(json.dumps({
    "versionCode": int(os.environ["RIME_JSON_CODE"]),
    "versionName": os.environ["RIME_JSON_NAME"],
    "tag": os.environ["RIME_JSON_TAG"],
    "commit": os.environ["RIME_JSON_COMMIT"],
    "apk": os.environ["RIME_JSON_APK"],
    "sha256": os.environ["RIME_JSON_SHA"],
    "sizeBytes": int(os.environ["RIME_JSON_SIZE"]),
    "remoteProtocolPreferred": int(os.environ["RIME_JSON_PROTOCOL"]),
    "remoteProtocolSupported": [int(v) for v in os.environ["RIME_JSON_WINDOW"].split(",")],
}, indent=2))
' > "$out/rime-remote-$name.json" || fatal "could not write the metadata"

echo "apk=$apk"
echo "sha256=$digest"
echo "json=$out/rime-remote-$name.json"
echo "protocol=$protocol"
echo "window=$window"
