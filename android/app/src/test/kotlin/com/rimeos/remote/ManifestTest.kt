package com.rimeos.remote

import com.rimeos.remote.core.agent.Handoff
import com.rimeos.remote.core.agent.Push
import com.rimeos.remote.core.agent.UnifiedPush
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.File

/**
 * What this app is allowed to ask the phone for — asserted, not trusted.
 *
 * P1-059's fourth criterion is "no automatic access to the phone photo
 * library/filesystem beyond Android-granted selections", and a criterion about
 * something that is **absent** is only ever met by a check, because absence is
 * exactly what nobody notices changing. The moment a file-handoff transport
 * exists, the obvious way to read the chosen file is a media permission — and
 * the picker that makes it unnecessary is one line longer to write. This test
 * is what makes the cheaper right answer the one that compiles.
 *
 * The same shape covers the microphone: push-to-talk goes through the system's
 * own recogniser, so `RECORD_AUDIO` must never appear either.
 *
 * ## This test refuses to pass by failing to look
 *
 * The manifest is found by walking up from the working directory rather than
 * by a fixed relative path, and a manifest that cannot be found is a
 * **failure**, never a skip. "Permission denied is not absence" has cost this
 * project real days; a green test that silently checked nothing would be the
 * same mistake with a nicer colour.
 */
class ManifestTest {

    private val manifest: String by lazy {
        var dir: File? = File("").absoluteFile
        var found: File? = null
        while (dir != null && found == null) {
            val candidate = File(dir, "app/src/main/AndroidManifest.xml")
            val here = File(dir, "src/main/AndroidManifest.xml")
            found = when {
                candidate.isFile -> candidate
                here.isFile -> here
                else -> null
            }
            dir = dir.parentFile
        }
        requireNotNull(found) {
            "AndroidManifest.xml was not found from ${File("").absolutePath}. This test asserts " +
                "that permissions are ABSENT, so a run that could not read the manifest must " +
                "fail rather than report that it found nothing."
        }.readText()
    }

    private fun declares(permission: String): Boolean =
        manifest.contains("\"$permission\"")

    @Test
    fun `no storage or media permission is declared`() {
        // The picker route needs none of these. `ACTION_PICK_IMAGES`
        // (androidx `PickVisualMedia`) returns a URI for the one item the user
        // chose and requires no permission at all; asking for one of these
        // instead would hand the app the whole library for the life of the
        // grant, which is the "automatic access beyond Android-granted
        // selections" the criterion forbids.
        for (p in Handoff.Files.FORBIDDEN_PERMISSIONS) {
            assertFalse(
                declares(p),
                "$p is declared. The photo picker needs no permission, and this one grants " +
                    "access beyond what the user selected.",
            )
        }
    }

    @Test
    fun `RECORD_AUDIO is not declared, because this app never holds the microphone`() {
        assertFalse(
            declares("android.permission.RECORD_AUDIO"),
            "RECORD_AUDIO is declared. Push-to-talk goes through the system recogniser " +
                "(ACTION_RECOGNIZE_SPEECH), which holds the microphone itself and hands back " +
                "text — no audio enters this process and none should be able to.",
        )
        assertFalse(declares("android.permission.CAPTURE_AUDIO_OUTPUT"))
    }

    @Test
    fun `the recogniser is visible under package visibility`() {
        // Without this, `resolveActivity` answers null on API 30+ for a phone
        // that HAS a recogniser: refusal and absence confused in the direction
        // that makes a working feature look broken.
        assertTrue(
            manifest.contains("<queries>"),
            "no <queries> block, so nothing this app looks up by intent is visible to it",
        )
        assertTrue(
            manifest.contains("android.speech.action.RECOGNIZE_SPEECH"),
            "the speech recogniser is not declared in <queries>",
        )
    }

    @Test
    fun `the photo picker's fallback is visible under package visibility too`() {
        // The same defect, one feature later. `PickVisualMedia` looks for a
        // system fallback picker with `PackageManager.resolveActivity` on this
        // action — read out of activity-1.12.4's bytecode rather than assumed
        // — and package visibility filters that call on API 30 and later. A
        // null answer is not a crash; it sends the contract down
        // `ACTION_OPEN_DOCUMENT`, so the user presses "Photo" and gets a file
        // browser. Quiet, wrong, and exactly what a phone-less round cannot
        // see.
        assertTrue(
            manifest.contains("androidx.activity.result.contract.action.PICK_IMAGES"),
            "the photo picker's system fallback is not declared in <queries>, so on a phone " +
                "without the platform picker the Photo button opens a file browser",
        )
        // And the thing that must NOT have appeared alongside it. A picker is
        // the alternative to a permission, not a companion to one.
        for (p in Handoff.Files.FORBIDDEN_PERMISSIONS) {
            assertFalse(declares(p), "$p was added alongside the picker")
        }
    }

    @Test
    fun `the permissions this app does declare are the seven it can justify`() {
        // A fixed set rather than a floor. A permission added without a reason
        // fails here and has to be argued for in a diff, which is the only
        // moment anybody reads the list.
        val declared = Regex("""uses-permission android:name="([^"]+)"""")
            .findAll(manifest)
            .map { it.groupValues[1] }
            .toSet()
        assertTrue(
            declared == setOf(
                // Pairing reads a QR off the desktop's screen, and works
                // without it: the payload can be pasted.
                "android.permission.CAMERA",
                // The LAN leg is a TCP connection; the relay leg a WebSocket.
                "android.permission.INTERNET",
                "android.permission.ACCESS_NETWORK_STATE",
                // An agent waiting for you is the reason to carry this app.
                "android.permission.POST_NOTIFICATIONS",
                // The app updates itself, because nothing else can: the phone
                // is not running Rime OS and `rime update` cannot reach an
                // APK. It earns the right to ASK and nothing more -- Android
                // still draws the install prompt, and so does GrapheneOS.
                // Argued at length in the manifest and in docs/android-app.md,
                // including why the DESKTOP AI apps are ruled the other way.
                "android.permission.REQUEST_INSTALL_PACKAGES",
                // Added deliberately with the remote-live change set: "I want
                // it just always connected." A foreground service holds the
                // connection to the computer while the app is unlocked, so a
                // machine or a terminal opens without a fresh handshake. The
                // first is what any foreground service needs, the second what
                // its `specialUse` type needs on Android 14 and later; neither
                // is a runtime prompt, and the cost is the ongoing
                // notification, which the next test pins.
                "android.permission.FOREGROUND_SERVICE",
                "android.permission.FOREGROUND_SERVICE_SPECIAL_USE",
            ),
            "the declared permissions changed: $declared",
        )
    }

    @Test
    fun `the connection keeper is a private specialUse service that says what it is for`() {
        // Android 14 refuses to start a foreground service whose type the
        // manifest does not declare, and a `specialUse` one without the
        // subtype property is the kind a reviewer — or a person reading the
        // app's info page — cannot account for. Exported, it could be started
        // or stopped by any app on the phone.
        val service = manifest.substringAfter("\".link.LinkService\"").substringBefore("</service>")
        assertTrue(manifest.contains("\".link.LinkService\""), "the connection service is not declared")
        assertTrue(service.contains("android:exported=\"false\""), "the connection service is exported")
        assertTrue(
            service.contains("android:foregroundServiceType=\"specialUse\""),
            "the connection service does not declare its foreground type",
        )
        assertTrue(
            service.contains("android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE"),
            "a specialUse service with no stated reason",
        )
        // And nothing else that a service could have been used as an excuse
        // for: it runs while the app is unlocked, not from boot, and it does
        // not ask to be exempt from the battery rules everything else obeys.
        for (p in listOf(
            "android.permission.RECEIVE_BOOT_COMPLETED",
            "android.permission.WAKE_LOCK",
            "android.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
        )) {
            assertFalse(declares(p), "$p came in with the connection service")
        }
    }

    @Test
    fun `the activity is not locked to one orientation and survives a rotation`() {
        // P1-060 asks for rotation and tablets/foldables. Neither is testable
        // without a device, but the two declarations that decide the outcome
        // are readable here: a locked orientation makes the criterion
        // impossible, and `configChanges` is what stops a rotation tearing down
        // the activity and taking a live terminal attachment with it.
        assertFalse(
            manifest.contains("android:screenOrientation"),
            "the activity pins an orientation, which fails P1-060's rotation and " +
                "tablet/foldable criterion by construction",
        )
        for (change in listOf("orientation", "screenSize", "screenLayout")) {
            assertTrue(
                Regex("""android:configChanges="[^"]*\b$change\b""").containsMatchIn(manifest),
                "configChanges does not handle $change, so that change recreates the activity " +
                    "and drops whatever the terminal was attached to",
            )
        }
    }

    @Test
    fun `the install-status receiver exists and is not exported`() {
        // `PackageInstaller.commit()` draws nothing by itself. It broadcasts
        // STATUS_PENDING_USER_ACTION carrying the intent that raises the
        // user's install prompt, and a build that committed sessions with no
        // receiver to read that broadcast would have an update that silently
        // never happened — which is the shape of failure this whole unit was
        // asked to eliminate, one layer down.
        //
        // A manifest assertion and not a behavioural one, said plainly: no
        // test in this repository installs anything, because there is no
        // device. What is checked here is that the declaration the flow
        // depends on is present and is closed to every sender but the system.
        val name = ".update.UpdateInstallReceiver"
        assertTrue(
            manifest.contains("\"" + name + "\""),
            "the install-status receiver is not declared, so committing an update session " +
                "would never raise the prompt that installs it",
        )
        val receiver = manifest.substringAfter(name).substringBefore("</receiver>")
        assertTrue(
            receiver.contains("android:exported=\"false\""),
            "the install-status receiver is exported, so any app on the phone could send it " +
                "an install status",
        )
        assertTrue(
            receiver.contains("com.rimeos.remote.UPDATE_STATUS"),
            "the receiver does not listen for the action the updater sends",
        )
    }

    @Test
    fun `the ring stop receiver is not exported and adds no permission`() {
        // Stopping a ring is harmless, but an exported receiver is one any app
        // can poke; the Stop button reaches it by explicit intent.
        val name = "\".phone.RingStopReceiver\""
        assertTrue(manifest.contains(name), "the ring stop receiver is not declared, so Stop does nothing")
        val receiver = manifest.substringAfter(name).substringBefore("/>")
        assertTrue(receiver.contains("android:exported=\"false\""), "the ring stop receiver is exported")
    }

    @Test
    fun `send to rime takes shares on the one activity, with no permission added for them`() {
        // The read grant arrives with the share (FLAG_GRANT_READ_URI_PERMISSION),
        // which is the whole reason "Send to Rime" needs no storage permission;
        // the permission tests above keep that true.
        val filter = manifest.substringAfter("@string/send_to_rime").substringBefore("</intent-filter>")
        assertTrue(filter.contains("android.intent.action.SEND\""), "no SEND action in the share filter")
        assertTrue(filter.contains("android.intent.action.SEND_MULTIPLE"), "no SEND_MULTIPLE in the share filter")
        assertTrue(filter.contains("android.intent.category.DEFAULT"), "a share filter without DEFAULT is never offered")
        assertTrue(filter.contains("android:mimeType=\"*/*\""), "the share filter takes only some types")
        assertFalse(manifest.contains("<provider"), "a provider would be an authority Shared.acceptable must refuse")
    }

    @Test
    fun `backup stays off, so the sealed device key is never copied anywhere`() {
        assertTrue(manifest.contains("""android:allowBackup="false""""))
        assertTrue(manifest.contains("""android:fullBackupContent="false""""))
    }
}

/**
 * The manifest half of push (P1-058), which nothing else here can check.
 *
 * A distributor reaches this app through the manifest and through nothing
 * else. Every failure in this file is **silent on a phone**: a missing action
 * means that message is never delivered, a missing `<queries>` entry means the
 * app reports that no distributor is installed on a phone that has one, and an
 * unexported receiver means the distributor's broadcast is dropped by the
 * platform without a word to either side.
 *
 * None of that is visible to `assembleDebug`, to a JVM test of the rules, or
 * to a person reading the code — which is why it is asserted against the
 * manifest text, in the same shape and for the same reason as the permission
 * assertions above.
 */
class PushManifestTest {

    private val manifest: String by lazy {
        var dir: File? = File("").absoluteFile
        var found: File? = null
        while (dir != null && found == null) {
            val candidate = File(dir, "app/src/main/AndroidManifest.xml")
            val here = File(dir, "src/main/AndroidManifest.xml")
            found = when {
                candidate.isFile -> candidate
                here.isFile -> here
                else -> null
            }
            dir = dir.parentFile
        }
        requireNotNull(found) {
            "AndroidManifest.xml was not found from ${File("").absolutePath}; this test must " +
                "fail rather than report that it found nothing."
        }.readText()
    }

    @Test
    fun `the receiver declares every connector action the specification defines`() {
        // Missing one is silent: the distributor simply never delivers that
        // message, and nothing on the phone says so. The list comes from
        // `UnifiedPush.CONNECTOR_ACTIONS` rather than being retyped, so an
        // action added there without a manifest entry fails here.
        for (action in UnifiedPush.CONNECTOR_ACTIONS) {
            assertTrue(
                manifest.contains("\"$action\""),
                "the manifest does not declare $action, so that message would never arrive",
            )
        }
        assertTrue(
            manifest.contains("\".push.PushReceiver\""),
            "the receiver class is not declared",
        )
    }

    @Test
    fun `the receiver is exported, because a distributor is another app`() {
        val receiver = manifest.substringAfter("\".push.PushReceiver\"").substringBefore("</receiver>")
        assertTrue(
            receiver.contains("android:exported=\"true\""),
            "an unexported receiver is one the platform drops the distributor's broadcast for, " +
                "silently",
        )
        // And what makes that safe is not the manifest: a message is acted on
        // only if it decrypts under a key one paired machine holds. Asserted
        // here as a comment would be worthless, so it is asserted as a fact
        // about the code that exists — `Push.open` returns null for anything
        // that does not authenticate.
        assertNull(
            Push.open(ByteArray(Push.KEY_LEN) { 1 }, ByteArray(Push.ENVELOPE_LEN) { 1 }),
            "a broadcast from a hostile app must reach no notification",
        )
    }

    @Test
    fun `distributors can be found at all`() {
        // Package visibility on API 30+ filters `queryBroadcastReceivers`.
        // Without this entry the app reports "no distributor installed" on a
        // phone that has one — refusal and absence confused in the direction
        // that makes a working feature look broken.
        assertTrue(
            manifest.contains("\"${UnifiedPush.ACTION_REGISTER}\""),
            "no <queries> entry for the distributor register action",
        )
        val queries = manifest.substringAfter("<queries>").substringBefore("</queries>")
        assertTrue(
            queries.contains(UnifiedPush.ACTION_REGISTER),
            "the register action is named outside <queries>, where it does not grant visibility",
        )
    }

    @Test
    fun `push adds no permission to this app`() {
        // The whole design's cost, stated as a check. A UnifiedPush client
        // needs no permission at all — no WAKE_LOCK, no RECEIVE_BOOT_COMPLETED,
        // and nothing from Google Play services. The distributor is the app
        // that holds a connection open for push, and it is the one the user
        // chose to install for that.
        //
        // FOREGROUND_SERVICE used to be on this list and is not any more, and
        // the reason is not push: the remote-live change set holds the
        // connection to the COMPUTER open while the app is unlocked (the
        // `.link.LinkService` that `ManifestTest` pins). Push still works with
        // the app closed and that service stopped, which is the point of it.
        for (permission in listOf(
            "android.permission.WAKE_LOCK",
            "android.permission.RECEIVE_BOOT_COMPLETED",
            "android.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
            "com.google.android.c2dm.permission.RECEIVE",
        )) {
            assertFalse(
                manifest.contains("\"$permission\""),
                "push was implemented in a way that needs $permission",
            )
        }
        // No Firebase or Play services component either. The app is built to
        // run on a phone that has neither.
        for (word in listOf("firebase", "com.google.android.gms", "FirebaseMessaging")) {
            assertFalse(
                manifest.lowercase().contains(word.lowercase()),
                "the manifest names $word",
            )
        }
    }
}
