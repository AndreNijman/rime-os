package com.rimeos.remote

import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.fragment.app.FragmentActivity
import com.rimeos.remote.core.Device
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.AlertTap
import com.rimeos.remote.ui.RimeRemoteApp
import com.rimeos.remote.ui.Notifier
import com.rimeos.remote.core.CrashReport
import com.rimeos.remote.data.MachineRepository

/**
 * The one activity.
 *
 * A [FragmentActivity] and not a `ComponentActivity`, which is not a style
 * choice: `androidx.biometric`'s `BiometricPrompt` requires one, and every
 * unlock in this app goes through it.
 */
class MainActivity : FragmentActivity() {
    private var payload by mutableStateOf<String?>(null)

    /**
     * The (machine, session) a tapped notification named, if this launch came
     * from one.
     *
     * Both halves, and not just the session: `SessionInfo.id` is a per-daemon
     * counter that is REUSED after a prune (`registry.rs:441`) and is not
     * unique across machines either — two paired laptops both have a session
     * 1. A tap carrying only a number could open a stranger's agent.
     */
    private var alertTap by mutableStateOf<AlertTap?>(null)

    /**
     * Asks for `POST_NOTIFICATIONS`, which API 33 made a runtime permission.
     *
     * Registered here rather than inside a composable because
     * `registerForActivityResult` must be called before the activity is
     * STARTED — it throws otherwise, and a `rememberLauncherForActivityResult`
     * inside a screen that a notification cold-launches into is exactly that
     * race.
     *
     * The boolean the callback carries is deliberately ignored, and
     * [notificationsAnswered] is bumped instead. The dialog is asynchronous:
     * anything that read the permission state on the line after `launch()`
     * would read it before the user had answered, and conclude that they had
     * said no. What the bump does is make the answer a value the UI is
     * recomposed by, so the state is re-read from Android itself — which is
     * also right when the user changes their mind later in Settings.
     */
    private var notificationsAnswered by mutableStateOf(0)

    private val askNotifications =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {
            notificationsAnswered += 1
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        installCrashHandler()
        enableEdgeToEdge()
        payload = pairingPayload(intent)
        alertTap = alertTarget(intent)
        setContent {
            RimeRemoteApp(
                activity = this,
                deviceName = defaultDeviceName(),
                launchPayload = payload,
                alertTarget = alertTap,
                onAlertConsumed = { alertTap = null },
                onAskNotifications = {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        askNotifications.launch(Notifier.PERMISSION)
                    }
                },
                notificationsAnswered = notificationsAnswered,
                // Cleared once a screen has it. Otherwise locking and
                // unlocking again would navigate back to a code that has since
                // expired, and the user would be looking at a failure they did
                // nothing to cause.
                onPayloadConsumed = { payload = null },
            )
        }
    }

    /**
     * A second `rime-remote:` link while the app is already open.
     *
     * The activity is `singleTask`, so this arrives here instead of stacking a
     * second copy of the app on top of the first — which would mean two
     * view models, two stores and a race over the same file.
     */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        payload = pairingPayload(intent)
        // A notification tapped while the app is already open arrives here.
        // Written unconditionally, including as null: an ordinary relaunch
        // must clear a tap that has already been acted on, or going back to
        // the machines list would bounce straight into the old session again.
        alertTap = alertTarget(intent)
    }

    /**
     * The payload out of a VIEW intent, if it is one.
     *
     * Checked for the scheme rather than trusted: this filter is exported, so
     * anything on the phone can send an intent to it. Nothing here acts on the
     * payload — it opens the screen that explains what accepting it would mean
     * — and that ordering is the point. A link that paired a phone by being
     * opened would be a link worth sending somebody.
     */
    /**
     * The session a tapped notification names.
     *
     * Matched on this app's own action rather than on the extras being
     * present, because the launcher intent and an `rime-remote:` VIEW intent
     * both reach here too and neither is a tap. Nothing is trusted from it
     * beyond being a hint: the view model checks the machine is still the
     * connected one and that the session still exists before it opens
     * anything.
     */
    private fun alertTarget(intent: Intent?): AlertTap? {
        if (intent?.action != Notifier.ACTION_OPEN_ALERT) return null
        val machine = intent.getStringExtra(Notifier.EXTRA_MACHINE) ?: return null
        val session = intent.getIntExtra(Notifier.EXTRA_SESSION, Alert.NO_SESSION)
        return AlertTap(machine, session, nex = intent.getBooleanExtra(Notifier.EXTRA_NEX, false))
    }

    private fun pairingPayload(intent: Intent?): String? {
        val data = intent?.data?.toString() ?: return null
        return data.takeIf { it.startsWith(Pairing.SCHEME) }
    }

    /**
     * What this phone calls itself to the desktop.
     *
     * `Build.MODEL` because it is what the owner will recognise in
     * `rime remote devices`, run through the protocol's own name check so a
     * device whose model string is hostile — a newline would put a second
     * request into `rime-agentd`'s control protocol — never reaches the wire.
     * A model that fails the check falls back rather than refusing to pair.
     */
    private fun defaultDeviceName(): String {
        val candidate = listOf(Build.MODEL, Build.DEVICE, "android")
        for (name in candidate) {
            if (name.isNullOrBlank()) continue
            runCatching { Device.checkName(name) }.onSuccess { return it }
        }
        return "android"
    }
}

/**
 * Keep a redacted report of an uncaught exception, if the user has said so.
 *
 * ## Where the consent lives, and why it is not read here
 *
 * `AppStorage.saveCrash` refuses when consent is off, so this function can be
 * wrong about everything else and still not write a report nobody asked for.
 * That placement is deliberate: this code runs while the process is being torn
 * down, it is the least exercised path in any app, and a consent check sitting
 * here would be one edit away from being skipped.
 *
 * ## Reading the setting on a dying thread
 *
 * The consent flag is in the store, and the store is on disk. Reading it here
 * is a blocking read on the crashing thread, which is the only option: the
 * process will not outlive this call, so anything handed to a coroutine would
 * never be scheduled. `AppStorage.load` returns an empty store — and therefore
 * `crashReports = false` — when the file is missing or will not parse, so the
 * failure mode of that read is "write nothing", which is the right one.
 *
 * ## The previous handler is always called
 *
 * Whatever Android installed shows the "app has stopped" dialog and ends the
 * process. Swallowing the exception instead would leave a phone sitting in a
 * broken activity with no way out, so the report is a side effect and the
 * crash still happens.
 */
private fun MainActivity.installCrashHandler() {
    val previous = Thread.getDefaultUncaughtExceptionHandler()
    val repository = MachineRepository(applicationContext)
    val version = runCatching {
        packageManager.getPackageInfo(packageName, 0).versionName
    }.getOrNull() ?: "unknown"

    Thread.setDefaultUncaughtExceptionHandler { thread, error ->
        // Everything in here is wrapped. A crash inside crash handling would
        // replace a report about the real fault with one about this code, and
        // the real one is the one somebody needs.
        runCatching {
            val consented = repository.crashConsentBlocking()
            if (consented) {
                repository.saveCrashBlocking(
                    CrashReport.of(
                        error,
                        CrashReport.Environment(
                            appVersion = version,
                            androidRelease = Build.VERSION.RELEASE ?: "unknown",
                            sdkInt = Build.VERSION.SDK_INT,
                            model = Build.MODEL ?: "unknown",
                        ),
                        thread = thread.name,
                    ),
                    consented = true,
                )
            }
        }
        previous?.uncaughtException(thread, error)
    }
}
