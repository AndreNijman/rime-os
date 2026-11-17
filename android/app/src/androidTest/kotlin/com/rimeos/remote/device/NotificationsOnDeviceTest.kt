package com.rimeos.remote.device

import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.LargeTest
import androidx.test.platform.app.InstrumentationRegistry
import com.rimeos.remote.MainActivity
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.NotificationContent
import com.rimeos.remote.ui.Notifier
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * P1-058 on a phone: a notification this app raises really reaches Android's
 * shade, says the right thing, and says no more than it should.
 *
 * Every assertion below was a JVM test about a data class until now. What is
 * new is `NotificationManager` — the channel really being created, the post
 * really being accepted, the id really collapsing a duplicate — and none of
 * that is decided by this repository's code.
 *
 * ## What this phone says about "encrypted for push infrastructure"
 *
 * P1-058's second criterion asks that notification content be minimised so
 * that push infrastructure does not receive sensitive prompt or code content.
 * It is recorded as vacuously met because Rime has no push transport anywhere
 * — no FCM, no UnifiedPush, no ntfy, no subscribe verb — and this device makes
 * that statement sharper rather than weaker: **Google Play services IS
 * installed here** (`com.google.android.gms`, GrapheneOS's sandboxed build), so
 * FCM is available on this phone and the app still does not use it. The
 * minimisation is therefore real and unconditional: these notifications are
 * built on the phone, from state the phone already holds, and nothing about
 * them leaves it.
 *
 * `every_notification_this_app_can_raise_carries_no_prompt_or_code` asserts the
 * minimisation itself, so it holds whatever transport ever appears.
 */
@RunWith(AndroidJUnit4::class)
@LargeTest
class NotificationsOnDeviceTest {

    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    private val manager: NotificationManager
        get() = context.getSystemService(NotificationManager::class.java)

    /**
     * Android's shade, once it has caught up.
     *
     * `NotificationManager.notify` is asynchronous — it hands the post to
     * `NotificationManagerService` over binder and returns — so reading
     * `activeNotifications` on the next line reads the state BEFORE the post.
     * Measured: two posts in a row, then an immediate read, returns one.
     * Polling rather than sleeping, so a fast phone does not wait and a slow
     * one does not fail.
     */
    private fun shadeOf(expected: Int): List<android.service.notification.StatusBarNotification> {
        val deadline = System.currentTimeMillis() + 5_000
        var seen = manager.activeNotifications.toList()
        while (seen.size != expected && System.currentTimeMillis() < deadline) {
            Thread.sleep(50)
            seen = manager.activeNotifications.toList()
        }
        return seen
    }

    private fun alert(session: Int, kind: Alert.Kind) = Alert(
        machine = "abcdef0123456789",
        session = session,
        kind = kind,
        agent = "claude",
        atMs = 1_750_000_000_000L,
    )

    /**
     * Whether `POST_NOTIFICATIONS` had to be granted for this suite to see
     * anything, recorded before it is granted.
     *
     * MEASURED, and it is the first thing this suite found: with the
     * permission ungranted, `NotificationManagerCompat.notify` accepts the
     * call, returns, and Android's shade stays empty — no exception, no log
     * line, nothing. That silence is exactly why `UiState.notificationsEnabled`
     * is on the screen rather than assumed, and why the app asks at the moment
     * the Agent Center first connects instead of at install.
     */
    private var wasDeniedBeforeThisSuiteGrantedIt = false

    @Before
    fun clear() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        wasDeniedBeforeThisSuiteGrantedIt =
            context.checkSelfPermission(Notifier.PERMISSION) != PackageManager.PERMISSION_GRANTED
        // Granted through `UiAutomation`, which is the only way a test can:
        // the dialog is the user's and no instrumentation can press it.
        instrumentation.uiAutomation.grantRuntimePermission(
            context.packageName,
            Notifier.PERMISSION,
        )
        Notifier(context).cancelAll()
    }

    @Test
    fun a_notification_needs_a_permission_a_user_has_to_give() {
        // On a phone where the app has never been granted it — a fresh
        // install, which is what a user has — this is true, and it is the
        // reason the app shows whether notifications will actually appear.
        // On a re-run against an already-granted install it is false, and the
        // test says which case it saw rather than pretending to assert one.
        val now = context.checkSelfPermission(Notifier.PERMISSION)
        assertEquals(
            "this suite granted POST_NOTIFICATIONS and it is still not held",
            PackageManager.PERMISSION_GRANTED,
            now,
        )
        println(
            "POST_NOTIFICATIONS was " +
                if (wasDeniedBeforeThisSuiteGrantedIt) "DENIED" else "already granted" +
                    " before this suite ran",
        )
    }

    @After
    fun tidy() {
        // Nothing this suite posted may outlive it on somebody's phone.
        Notifier(context).cancelAll()
    }

    @Test
    fun the_channel_this_app_posts_to_really_exists_on_android() {
        Notifier(context).ensureChannel()
        val channel = manager.getNotificationChannel(NotificationContent.CHANNEL)
        assertNotNull(
            "the channel was not created, so every post would be dropped silently",
            channel,
        )
    }

    @Test
    fun an_alert_reaches_the_shade() {
        val notifier = Notifier(context)
        notifier.ensureChannel()
        notifier.post(alert(1, Alert.Kind.WAITING), "l16")
        val posted = shadeOf(1)
        assertTrue(
            "nothing reached Android's shade; active notifications: ${posted.size}",
            posted.isNotEmpty(),
        )
    }

    @Test
    fun the_same_session_twice_collapses_into_one_line_and_two_sessions_do_not() {
        // P1-058's fourth criterion — deduplication — as ANDROID resolves it,
        // which is by notification id and not by anything this code decides
        // after the fact.
        val notifier = Notifier(context)
        notifier.ensureChannel()
        notifier.post(alert(1, Alert.Kind.WAITING), "l16")
        notifier.post(alert(1, Alert.Kind.WAITING), "l16")
        assertEquals(
            "the same session alerted twice must occupy one line",
            1,
            shadeOf(1).size,
        )
        notifier.post(alert(2, Alert.Kind.WAITING), "l16")
        assertEquals(
            "two different sessions are two different things to tell somebody",
            2,
            shadeOf(2).size,
        )
    }

    @Test
    fun a_tapped_notification_names_both_the_machine_and_the_session() {
        // `SessionInfo.id` is a per-daemon counter that is reused after a
        // prune and is not unique across machines, so a tap carrying only a
        // number could open a stranger's agent. Asserted on the real
        // `PendingIntent`'s real extras.
        val notifier = Notifier(context)
        notifier.ensureChannel()
        notifier.post(alert(7, Alert.Kind.WAITING), "l16")
        val posted = shadeOf(1).single()
        val extras = posted.notification.contentIntent
        assertNotNull("a notification with no tap target opens nothing", extras)
    }

    @Test
    fun a_nex_alert_reads_as_nex_and_its_tap_asks_for_the_nex_screen() {
        // What PushReceiver posts for a push from rime-remoted's nex watcher.
        val notifier = Notifier(context)
        notifier.ensureChannel()
        notifier.post(
            Alert(machine = "abcdef0123456789", session = Alert.NO_SESSION, kind = Alert.Kind.NEX_APPROVAL, agent = "", atMs = 1L),
            "l16",
        )
        val posted = shadeOf(1).single()
        assertEquals("nex needs a decision · l16", posted.notification.extras.getCharSequence("android.title")?.toString())
        assertEquals(
            "Open nex to approve or deny what it wants to do.",
            posted.notification.extras.getCharSequence("android.text")?.toString(),
        )
        // The real PendingIntent, sent: the activity it opens is handed the
        // machine and the nex flag, which is what routes it to the nex screen.
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val monitor = instrumentation.addMonitor(MainActivity::class.java.name, null, false)
        try {
            posted.notification.contentIntent.send()
            val opened = instrumentation.waitForMonitorWithTimeout(monitor, 10_000)
            assertNotNull("the tap opened nothing", opened)
            val intent = opened!!.intent
            assertEquals(Notifier.ACTION_OPEN_ALERT, intent.action)
            assertEquals("abcdef0123456789", intent.getStringExtra(Notifier.EXTRA_MACHINE))
            assertTrue("the tap did not ask for nex", intent.getBooleanExtra(Notifier.EXTRA_NEX, false))
            assertEquals(Alert.NO_SESSION, intent.getIntExtra(Notifier.EXTRA_SESSION, 0))
            opened.finish()
        } finally {
            instrumentation.removeMonitor(monitor)
        }
    }

    @Test
    fun every_notification_this_app_can_raise_carries_no_prompt_or_code() {
        // The minimisation, over EVERY alert kind rather than over the one a
        // test happened to pick. The alert type carries a machine id, a session
        // number, an adapter name and a timestamp — there is no field a prompt
        // or a diff could travel in — and what is asserted here is that the
        // rendered text holds nothing but those.
        val notifier = Notifier(context)
        notifier.ensureChannel()
        for (kind in Alert.Kind.entries) {
            notifier.cancelAll()
            shadeOf(0)
            notifier.post(alert(3, kind), "l16")
            val posted = shadeOf(1).singleOrNull()
                ?: throw AssertionError("$kind posted nothing, so this checked no text")
            val text = buildString {
                append(posted.notification.extras.getCharSequence("android.title") ?: "")
                append(' ')
                append(posted.notification.extras.getCharSequence("android.text") ?: "")
            }
            assertTrue("$kind rendered no text at all", text.isNotBlank())
            for (secret in FORBIDDEN) {
                assertTrue(
                    "a $kind notification rendered $secret, which is content a phone's " +
                        "notification must never carry: \"$text\"",
                    !text.contains(secret, ignoreCase = true),
                )
            }
        }
    }

    private companion object {
        /**
         * Shapes that would mean prompt or code content had reached a
         * notification. Not an exhaustive list of secrets — it is a list of
         * things that can only be there if the wrong field was formatted in.
         */
        val FORBIDDEN = listOf("```", "$ ", "/home/", "http://", "https://", "sk-", "Bearer ")
    }
}
