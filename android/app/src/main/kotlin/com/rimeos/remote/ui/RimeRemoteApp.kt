package com.rimeos.remote.ui

import com.rimeos.remote.ui.mirror.MediaScreen
import com.rimeos.remote.ui.mirror.CallsScreen
import com.rimeos.remote.ui.mirror.MessagesScreen
import com.rimeos.remote.ui.mirror.MirrorScreen
import com.rimeos.remote.ui.mirror.PhotosScreen
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.rimeos.remote.core.agent.AlertTap
import com.rimeos.remote.share.Shared
import com.rimeos.remote.ui.send.SendScreen
import com.rimeos.remote.ui.send.SendViewModel
import com.rimeos.remote.update.AppUpdater
import com.rimeos.remote.update.UpdateBanner
import com.rimeos.remote.update.UpdateUi
import com.rimeos.remote.ui.agent.AgentCenterScreen
import com.rimeos.remote.ui.agent.HelpScreen
import com.rimeos.remote.ui.agent.ApprovalsScreen
import com.rimeos.remote.ui.agent.SessionScreen
import com.rimeos.remote.ui.agent.WorktreesScreen
import com.rimeos.remote.ui.agent.StartAgentScreen
import com.rimeos.remote.ui.agent.knownDirectories
import com.rimeos.remote.ui.nex.NexNewSessionScreen
import com.rimeos.remote.ui.nex.NexScreen
import com.rimeos.remote.ui.nex.NexSessionScreen
import com.rimeos.remote.ui.nex.NexViewModel
import com.rimeos.remote.core.nex.Nex
import com.rimeos.remote.core.machine.Machine
import com.rimeos.remote.core.input.Input
import com.rimeos.remote.ui.touchpad.TouchpadScreen
import com.rimeos.remote.ui.touchpad.TouchpadViewModel
import kotlinx.coroutines.awaitCancellation
import com.rimeos.remote.ui.machine.MachineScreen
import com.rimeos.remote.ui.machine.MachineViewModel
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.ui.term.TerminalScreen
import com.rimeos.remote.ui.theme.RimeRemoteTheme
import android.content.Intent
import androidx.compose.ui.platform.LocalContext

/**
 * The whole app, above the screens.
 *
 * Navigation is two destinations and a gate, which is the entire shape of
 * P1-053: a list of machines, the screen that adds one, and a lock in front of
 * both. P1-054 and P1-055 add destinations under the machine row rather than
 * changing this.
 */
object Destinations {
    const val MACHINES = "machines"
    const val PAIRING = "pairing"

    /** Every agent on the connected machine. */
    const val AGENTS = "agents"

    /** One agent, in full. */
    const val SESSION = "session"

    /** The attached PTY. */
    const val TERMINAL = "terminal"

    /** Starting a new one. */
    const val START = "start"

    /** Projects, their worktrees and the work in them. */
    const val WORKTREES = "worktrees"

    /** Privileged operations waiting at the machine, and standing grants. */
    const val APPROVALS = "approvals"

    /** The guide (P1-060), whose words live in `:core`. */
    const val HELP = "help"

    /** nex on the connected machine: approvals, sessions, missions, usage. */
    const val NEX = "nex"

    /** One nex session's transcript and composer. */
    const val NEX_SESSION = "nex_session"

    /** Starting a nex session from the phone. */
    const val NEX_NEW = "nex_new"

    /** The computer itself: status, lock, media and volume. */
    const val MACHINE = "machine"

    /** "Send to Rime": what another app shared, before it is sent. */
    const val SEND = "send"

    /** "Notifications on your computer": access, and which apps. */
    const val MIRROR = "mirror"

    /** "Media on your computer": the switch, and which apps. */
    const val MEDIA = "media"

    /** "Messages on your computer": the permissions and the switch. */
    const val MESSAGES = "messages"

    /** "Calls on your computer": the permissions and the switch. */
    const val CALLS = "calls"

    /** "Photos on your computer": the access and the switch. */
    const val PHOTOS = "photos"

    /** The phone as the computer's touchpad and keyboard. */
    const val TOUCHPAD = "touchpad"
}

@Composable
fun RimeRemoteApp(
    activity: FragmentActivity,
    deviceName: String,
    /** A payload the app was launched with, from an `rime-remote:` link. */
    launchPayload: String? = null,
    /** Called once the payload has been handed to a screen, so it fires once. */
    onPayloadConsumed: () -> Unit = {},
    /** Where a tapped notification asks to go, if this is one. */
    alertTarget: AlertTap? = null,
    /** Called once the tap has been acted on, so it fires once. */
    onAlertConsumed: () -> Unit = {},
    /** What another app shared, if this launch is a share. */
    share: Shared? = null,
    /** Called once the Send screen has the share, so it is offered once. */
    onShareConsumed: () -> Unit = {},
    /** Ask Android for `POST_NOTIFICATIONS`. Owned by the activity. */
    onAskNotifications: () -> Unit = {},
    /** Bumped each time the permission dialog is answered, whichever way. */
    notificationsAnswered: Int = 0,
    viewModel: RemoteViewModel = viewModel(),
    nexModel: NexViewModel = viewModel(),
    machineModel: MachineViewModel = viewModel(),
    sendModel: SendViewModel = viewModel(),
    touchpadModel: TouchpadViewModel = viewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val nex by nexModel.state.collectAsStateWithLifecycle()
    val computer by machineModel.state.collectAsStateWithLifecycle()
    val sending by sendModel.state.collectAsStateWithLifecycle()
    val appContext = LocalContext.current.applicationContext
    // The APPLICATION context, not the activity's. This object outlives a
    // rotation and an install prompt, and holding an activity across either is
    // how a leak becomes a crash.
    val updater = remember(appContext) { AppUpdater(appContext) }
    // Pressing Update is a state change, not a suspend call: a composable
    // cannot start one, and a `rememberCoroutineScope` launched from a button
    // would keep running after the screen that owned it had gone.
    var installing by remember { mutableStateOf(false) }

    RimeRemoteTheme(useDynamicColour = state.settings.dynamicColour) {
        val navigation = rememberNavController()

        if (!state.unlocked) {
            LockScreen(
                failure = state.failure,
                onUnlock = { viewModel.unlockApp(activity, prefer = alertTarget?.machine) },
                companion = state.companion,
            )
            // Asked once the state is known, rather than behind a button nobody
            // would choose to press. Keyed on `loading` because the view model
            // refuses to prompt before it has decided whether this phone can
            // authenticate at all — so the effect has to run again once it has.
            // A notification's machine first, so its tap needs no second prompt.
            LaunchedEffect(state.loading) { viewModel.unlockApp(activity, prefer = alertTarget?.machine) }
            // And nothing below this line runs while locked, which is why the
            // deep-link effect is inside the graph rather than out here:
            // `NavController.navigate` before a `NavHost` has composed throws
            // "Navigation graph has not been set", and a cold launch from an
            // `rime-remote:` link is exactly that moment.
            return@RimeRemoteTheme
        }

        // While the app is on screen and unlocked, an offer to join another
        // computer can be sealed: the prompt needs the screen (§2.15).
        val joinLifecycle = LocalLifecycleOwner.current.lifecycle
        LaunchedEffect(joinLifecycle) {
            joinLifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { viewModel.joinWhileShown(activity, deviceName) }
        }

        NavHost(navigation, startDestination = Destinations.MACHINES) {
            composable(Destinations.MACHINES) {
                val context = LocalContext.current
                // Read on arrival. A report written by the handler during the
                // PREVIOUS process is on disk before this one starts, so
                // nothing else would ever notice it.
                LaunchedEffect(state.settings.crashReports) {
                    if (state.settings.crashReports) viewModel.refreshCrash()
                }
                MachinesScreen(
                    state = state,
                    onPair = { navigation.navigate(Destinations.PAIRING) },
                    onConnect = {
                        viewModel.connect(activity, it)
                        navigation.navigate(Destinations.AGENTS)
                    },
                    onPing = { viewModel.ping(activity, it) },
                    onForget = { viewModel.forget(it) },
                    onDynamicColour = { viewModel.setDynamicColour(it) },
                    onMirror = { navigation.navigate(Destinations.MIRROR) },
                    onMedia = { navigation.navigate(Destinations.MEDIA) },
                    onMessages = { navigation.navigate(Destinations.MESSAGES) },
                    onCalls = { navigation.navigate(Destinations.CALLS) },
                    onPhotos = { navigation.navigate(Destinations.PHOTOS) },
                    onCompanion = { viewModel.setCompanionOn(it) },
                    onClipboard = { viewModel.setClipboardOn(it) },
                    onAutoJoin = { viewModel.setAutoJoin(it) },
                    onCrashReports = { viewModel.setCrashReports(it) },
                    onShareCrash = { report ->
                        // ACTION_SEND with the text in the intent, so the user
                        // picks where it goes. No upload, no reporting SDK,
                        // and no destination this app chose for them.
                        val send = Intent(Intent.ACTION_SEND).apply {
                            type = "text/plain"
                            putExtra(Intent.EXTRA_SUBJECT, "Rime Remote crash report")
                            putExtra(Intent.EXTRA_TEXT, report)
                        }
                        runCatching { context.startActivity(Intent.createChooser(send, null)) }
                    },
                    onClearCrash = { viewModel.clearCrash() },
                    onLock = { viewModel.lock() },
                    onDismiss = { viewModel.dismiss() },
                    updateBanner = {
                        UpdateBanner(
                            state = updater.state,
                            // Re-read on every recomposition rather than
                            // remembered. The user may have just come back
                            // from the Settings page that grants it, and a
                            // cached `false` would leave them looking at the
                            // Allow button they had already pressed.
                            mayInstall = updater.mayInstall(),
                            onInstall = { installing = true },
                            onAllowSources = {
                                runCatching { context.startActivity(updater.sourcesSettings()) }
                            },
                            onDismiss = { updater.dismiss() },
                        )
                    },
                )
                // The check, and the install, both here rather than in the
                // view model. `AppUpdater` is about this app's own APK and has
                // nothing to do with the machines the view model holds; the
                // one thing it needs is a Context, which a view model is the
                // wrong place to keep.
                LaunchedEffect(Unit) { updater.check() }
                LaunchedEffect(installing) {
                    val offer = (updater.state as? UpdateUi.Available)?.offer
                    if (installing && offer != null) {
                        // The flag is cleared in a `finally`, AFTER the install
                        // returns, and that ordering is the whole of this
                        // comment. Clearing it first changes this effect's own
                        // key, which schedules a recomposition, which CANCELS
                        // the coroutine it was cleared in — so the blocking
                        // download would run to completion on its IO
                        // dispatcher while `withContext` returned a
                        // CancellationException here, and neither the "waiting
                        // for Android's prompt" state nor a checksum failure
                        // would ever reach the banner. The user would be left
                        // watching "Downloading…" forever, which is the
                        // spinning pairing screen in a new place.
                        try {
                            updater.install(offer)
                        } finally {
                            installing = false
                        }
                    }
                }
            }
            composable(Destinations.PAIRING) {
                PairingScreen(
                    state = state,
                    // Handed to the screen, not acted on. It arrives in the
                    // paste box with the explanation beside it, and pairing
                    // still takes a deliberate press.
                    initialPayload = launchPayload,
                    // Cleared by the screen once it has captured the code, and
                    // NOT here. `navigate` does not compose anything; it writes
                    // state the `NavHost` reads on the next recomposition. A
                    // consume beside the navigate would therefore run first,
                    // and `PairingScreen` would `remember` an empty string —
                    // which is the exact bug this parameter was added to fix.
                    onPayloadConsumed = onPayloadConsumed,
                    onPayload = { payload ->
                        viewModel.pair(activity, payload, deviceName)
                    },
                    onBack = { navigation.popBackStack() },
                    onDismiss = { viewModel.dismiss() },
                )
            }

            composable(Destinations.AGENTS) {
                // Re-read on arrival AND after the dialog is answered. The
                // second key is what makes this correct: the permission dialog
                // is asynchronous, so a refresh on the line after `launch()`
                // would read the state before the user had answered and
                // conclude they had said no. It also covers the user revoking
                // the permission in Android's settings and coming back.
                LaunchedEffect(notificationsAnswered) { viewModel.refreshNotificationState() }

                // Asked when the user first looks at a machine's agents —
                // the first moment a notification would have anything to say —
                // and not at launch, where it is a dialog in front of somebody
                // who has not seen the app yet. Keyed on the flag rather than
                // on `Unit`, so it fires once the refresh above has actually
                // landed rather than against a stale snapshot.
                LaunchedEffect(state.notificationsUnasked) {
                    if (state.notificationsUnasked) onAskNotifications()
                }
                AgentCenterScreen(
                    machine = state.agents.machine?.machine ?: "",
                    sessions = state.agents.sessions,
                    nowSeconds = state.agents.nowSeconds,
                    busy = state.busy ?: state.agents.busy,
                    failure = state.failure ?: state.agents.failure,
                    notice = state.agents.notice,
                    onRefresh = { viewModel.refreshAgents() },
                    onOpen = {
                        viewModel.selectSession(it)
                        navigation.navigate(Destinations.SESSION)
                    },
                    onStart = { navigation.navigate(Destinations.START) },
                    onProjects = { navigation.navigate(Destinations.WORKTREES) },
                    onApprovals = { navigation.navigate(Destinations.APPROVALS) },
                    onHelp = { navigation.navigate(Destinations.HELP) },
                    // Offered only when the machine's bridge says it reaches a
                    // nexd: a button that always answered "not running" would
                    // be noise on every Rime without nex.
                    onNex = state.agents.machine
                        ?.takeIf { state.links[it.deviceId]?.remote?.has(Nex.FEATURE) == true }
                        ?.let { machine ->
                            {
                                nexModel.open(machine)
                                navigation.navigate(Destinations.NEX)
                            }
                        },
                    // Offered when the machine's rime-remoted answers the
                    // machine verbs, which every build since they shipped does.
                    onComputer = state.agents.machine
                        ?.takeIf { state.links[it.deviceId]?.remote?.has(Machine.STATUS) == true }
                        ?.let { machine ->
                            {
                                machineModel.open(machine)
                                navigation.navigate(Destinations.MACHINE)
                            }
                        },
                    // Offered when the computer's rime-remoted has the
                    // touchpad (§2.16), whether or not this phone is allowed
                    // yet: the screen says how to allow it.
                    onTouchpad = state.agents.machine
                        ?.takeIf { state.links[it.deviceId]?.remote?.has(Input.FEATURE) == true }
                        ?.let { machine ->
                            {
                                touchpadModel.open(machine, deviceName)
                                navigation.navigate(Destinations.TOUCHPAD)
                            }
                        },
                    onCopyFromMachine = { viewModel.pullMachineClipboard() },
                    clipboardPull = state.agents.clipboardPull,
                    onClipboardCopied = { viewModel.clipboardPullConsumed() },
                    pendingApprovals = state.agents.approvals.pending.size,
                    notificationsEnabled = state.notificationsEnabled,
                    notificationsUnasked = state.notificationsUnasked,
                    lastLines = state.agents.lastLines,
                    onWatchList = { viewModel.watchList(it) },
                    onBack = {
                        // Stop the four-second poll. Leaving it running would
                        // keep a control round trip going to a machine nobody
                        // is looking at, for as long as the app is open.
                        viewModel.leaveAgents()
                        navigation.popBackStack()
                    },
                    onDismiss = { viewModel.dismiss() },
                )
            }

            composable(Destinations.SESSION) {
                // Read from the state rather than passed as an argument, so a
                // poll that arrives while this screen is open updates it. A
                // detail screen holding a copy from four seconds ago is one
                // that shows `working` for a session that has since failed.
                val session = state.agents.selected
                if (session == null) {
                    LaunchedEffect(Unit) { navigation.popBackStack() }
                } else {
                    SessionScreen(
                        session = session,
                        machine = state.agents.machine?.machine ?: "",
                        liveSessions = state.agents.sessions,
                        nowSeconds = state.agents.nowSeconds,
                        busy = state.agents.busy,
                        failure = state.agents.failure,
                        onAttach = {
                            if (viewModel.attach(session) != null) {
                                navigation.navigate(Destinations.TERMINAL)
                            }
                        },
                        onPause = { viewModel.signal(session, "stop") },
                        onResume = { viewModel.signal(session, "cont") },
                        onInterrupt = { viewModel.signal(session, "int") },
                        onStop = { viewModel.signal(session, "term") },
                        onRefresh = { viewModel.refreshAgents() },
                        onReply = { viewModel.replyToSession(session, it) },
                        onSendFile = { viewModel.sendFileToSession(session, it) },
                        notice = state.agents.notice,
                        peek = state.agents.peek,
                        onWatch = { viewModel.watchSession(session) },
                        onUnwatch = { viewModel.stopWatchingSession() },
                        canRename = state.agents.hello?.has(com.rimeos.remote.core.agent.Features.RENAME) == true ||
                            viewModel.canRename(),
                        onRename = { viewModel.renameSession(session, it) },
                        onBack = { navigation.popBackStack() },
                        onDismiss = { viewModel.dismiss() },
                    )
                }
            }

            composable(Destinations.TERMINAL) {
                val controller = viewModel.terminal
                if (controller == null) {
                    LaunchedEffect(Unit) { navigation.popBackStack() }
                } else {
                    TerminalScreen(
                        controller = controller,
                        title = state.agents.selected?.displayName
                            ?: (state.agents.machine?.machine ?: "Terminal"),
                        settings = state.settings,
                        onBack = {
                            // Detaching leaves the session running on the
                            // machine, which is the entire point of a viewport.
                            viewModel.detach()
                            navigation.popBackStack()
                        },
                    )
                }
            }

            composable(Destinations.WORKTREES) {
                WorktreesScreen(
                    machine = state.agents.machine?.deviceId ?: "",
                    state = state.agents.worktrees,
                    // Which sessions this phone actually has, so a worktree
                    // row knows whether "open agent" can do anything. The
                    // daemon's worktree rows carry session ids it knows about,
                    // and the two lists can disagree.
                    liveSessions = state.agents.sessions.map { it.id }.toSet(),
                    onLoad = { viewModel.loadWorktrees() },
                    onOpenSession = {
                        if (viewModel.openWorktreeSession(it) != null) {
                            navigation.navigate(Destinations.SESSION)
                        }
                    },
                    onBack = { navigation.popBackStack() },
                )
            }

            composable(Destinations.APPROVALS) {
                ApprovalsScreen(
                    state = state.agents.approvals,
                    nowSeconds = state.agents.nowSeconds,
                    onLoad = { viewModel.loadApprovals() },
                    onRevokeGrant = { project, key -> viewModel.revokeGrant(project, key) },
                    onRevokeSystemGrant = { viewModel.revokeSystemGrant(it) },
                    onBack = { navigation.popBackStack() },
                )
            }

            composable(Destinations.MIRROR) {
                MirrorScreen(
                    chosen = state.settings.mirrorApps,
                    onChoose = { pkg, on -> viewModel.setMirrorApp(pkg, on) },
                    reply = state.settings.mirrorReply,
                    onReply = { viewModel.setMirrorReply(it) },
                    focus = state.settings.focusOn,
                    onFocus = { viewModel.setFocusOn(it) },
                    onBack = { navigation.popBackStack() },
                )
            }
            composable(Destinations.MEDIA) {
                MediaScreen(
                    on = state.settings.mediaOn,
                    onOn = { viewModel.setMediaOn(it) },
                    control = state.settings.mediaControl,
                    onControl = { viewModel.setMediaControl(it) },
                    chosen = state.settings.mediaApps,
                    onChoose = { pkg, on -> viewModel.setMediaApp(pkg, on) },
                    onBack = { navigation.popBackStack() },
                )
            }
            composable(Destinations.MESSAGES) {
                MessagesScreen(
                    on = state.settings.messagesOn,
                    onOn = { viewModel.setMessagesOn(it) },
                    onPermissions = { viewModel.onSmsPermissionChanged() },
                    onBack = { navigation.popBackStack() },
                )
            }
            composable(Destinations.CALLS) {
                CallsScreen(
                    on = state.settings.callsOn,
                    onOn = { viewModel.setCallsOn(it) },
                    onPermissions = { viewModel.onCallsPermissionChanged() },
                    onBack = { navigation.popBackStack() },
                )
            }
            composable(Destinations.PHOTOS) {
                PhotosScreen(
                    on = state.settings.photosOn,
                    onOn = { viewModel.setPhotosOn(it) },
                    onPermissions = { viewModel.onPhotosPermissionChanged() },
                    onBack = { navigation.popBackStack() },
                )
            }
            composable(Destinations.HELP) {
                HelpScreen(onBack = { navigation.popBackStack() })
            }

            composable(Destinations.NEX) {
                NexScreen(
                    machine = state.agents.machine?.machine ?: "",
                    state = nex,
                    onOpen = {
                        nexModel.select(it)
                        navigation.navigate(Destinations.NEX_SESSION)
                    },
                    onNew = { navigation.navigate(Destinations.NEX_NEW) },
                    onApprove = { nexModel.approve(it) },
                    onDeny = { nexModel.deny(it) },
                    onMission = { verb, mission -> nexModel.mission(verb, mission) },
                    onToggleMission = { nexModel.toggleMission(it) },
                    onRefresh = { nexModel.refresh() },
                    onDismiss = { nexModel.dismiss() },
                    onBack = {
                        // The feed is held only while nex is on screen.
                        nexModel.leave()
                        navigation.popBackStack()
                    },
                )
            }

            composable(Destinations.MACHINE) {
                // Read only while this screen is started: nothing is asked of
                // the computer while the phone is in a pocket.
                val lifecycle = LocalLifecycleOwner.current.lifecycle
                LaunchedEffect(lifecycle) {
                    lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { machineModel.watch() }
                }
                MachineScreen(
                    state = computer,
                    onLock = { machineModel.lock() },
                    onMedia = { machineModel.control(it) },
                    onVolume = { machineModel.volume(it) },
                    onMute = { machineModel.mute(it) },
                    onRefresh = { machineModel.refresh() },
                    onDismiss = { machineModel.dismiss() },
                    onBack = { navigation.popBackStack() },
                    onSendClipboard = { machineModel.sendClipboard(it) },
                )
            }

            composable(Destinations.TOUCHPAD) {
                // Open only while the screen is started: leaving it, or the
                // app going to the background, lets go of everything held on
                // the computer and closes the channel.
                val lifecycle = LocalLifecycleOwner.current.lifecycle
                LaunchedEffect(lifecycle) {
                    lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                        touchpadModel.enter()
                        try {
                            awaitCancellation()
                        } finally {
                            touchpadModel.leave()
                        }
                    }
                }
                val touchpad by touchpadModel.state.collectAsStateWithLifecycle()
                TouchpadScreen(
                    model = touchpadModel,
                    state = touchpad,
                    settings = state.settings,
                    onNatural = { viewModel.setTouchpadNatural(it) },
                    onSpeed = { viewModel.setTouchpadSpeed(it) },
                    onBack = { navigation.popBackStack() },
                )
            }

            composable(Destinations.SEND) {
                val leave = {
                    sendModel.clear()
                    if (!navigation.popBackStack()) navigation.navigate(Destinations.MACHINES)
                }
                SendScreen(
                    state = sending,
                    onChoose = { sendModel.choose(it) },
                    onSend = {
                        state.machines.firstOrNull { it.deviceId == sending.chosen }?.let { m ->
                            viewModel.withKey(activity, m, onFailure = { sendModel.failed(it) }) { sendModel.send(m) }
                        }
                    },
                    onStop = { sendModel.stop() },
                    onDismiss = { sendModel.dismiss() },
                    onDone = { leave() },
                )
            }

            composable(Destinations.NEX_SESSION) {
                val session = nex.selected
                if (session == null) {
                    LaunchedEffect(Unit) { navigation.popBackStack() }
                } else {
                    NexSessionScreen(
                        session = session,
                        state = nex,
                        onSend = { text, sent -> nexModel.send(session, text, sent) },
                        onStop = { nexModel.stop(session) },
                        onApprove = { nexModel.approve(it) },
                        onDeny = { nexModel.deny(it) },
                        onRefresh = { nexModel.refresh() },
                        onDismiss = { nexModel.dismiss() },
                        onBack = {
                            nexModel.unselect()
                            navigation.popBackStack()
                        },
                    )
                }
            }

            composable(Destinations.NEX_NEW) {
                NexNewSessionScreen(
                    state = nex,
                    onCreate = { root, provider, title, message -> nexModel.create(root, provider, title, message) },
                    onCreated = {
                        nexModel.createdConsumed()
                        navigation.navigate(Destinations.NEX_SESSION) {
                            popUpTo(Destinations.NEX)
                        }
                    },
                    onDismiss = { nexModel.dismiss() },
                    onBack = { navigation.popBackStack() },
                )
            }

            composable(Destinations.START) {
                StartAgentScreen(
                    machine = state.agents.machine?.machine ?: "",
                    hello = state.agents.hello,
                    knownDirectories = knownDirectories(state.agents.sessions),
                    picker = state.agents.picker,
                    busy = state.agents.busy,
                    failure = state.agents.failure,
                    onLoad = { viewModel.loadPickers() },
                    onProject = { slug -> viewModel.loadPickerWorktrees(slug) },
                    onStart = { agent, cwd, worktree, prompt, checkpoint, args ->
                        viewModel.startAgent(
                            cwd = cwd,
                            agent = agent,
                            worktree = worktree,
                            prompt = prompt,
                            checkpoint = checkpoint,
                            args = args,
                        )
                    },
                    onBack = { navigation.popBackStack() },
                    onDismiss = { viewModel.dismiss() },
                )
            }
        }

        // A start that worked lands on the new agent's page, which is where
        // the next thing somebody wants to do is. Keyed on the id so it fires
        // once per started session rather than once per recomposition.
        LaunchedEffect(state.agents.selected?.id) {
            if (state.agents.selected != null &&
                navigation.currentDestination?.route == Destinations.START
            ) {
                navigation.popBackStack()
                navigation.navigate(Destinations.SESSION)
            }
        }

        // A share opens the Send screen and sends nothing: only a tap there
        // does. Inside the graph for the same reason as the effects below.
        LaunchedEffect(share) {
            val shared = share ?: return@LaunchedEffect
            onShareConsumed()
            sendModel.offer(shared, state.machines)
            navigation.navigate(Destinations.SEND) { launchSingleTop = true }
        }

        // A tapped notification opens the exact session it named, when this
        // phone still has it. Inside the graph for the same reason as the
        // pairing effect below: `navigate` before the `NavHost` has composed
        // throws, and a cold launch from a notification is exactly that
        // moment.
        // A nex tap that had to connect first: the machine it is waiting for.
        var pendingNex by remember { mutableStateOf<AlertTap.Pending?>(null) }
        LaunchedEffect(alertTarget) {
            val target = alertTarget ?: return@LaunchedEffect
            // Consumed whatever happens. A tap that could not be honoured must
            // not be retried on the next recomposition: the session is not
            // coming back, and the user would be unable to navigate away.
            onAlertConsumed()
            val connected = state.agents.machine
            val goes = target.goesTo(
                connected = connected?.deviceId,
                nexOffered = state.links[target.machine]?.remote?.has(Nex.FEATURE) == true,
                paired = state.machines.any { it.deviceId == target.machine },
            )
            val tapped = state.machines.firstOrNull { it.deviceId == target.machine }
            if (goes == AlertTap.GoesTo.CONNECT_FOR_NEX && tapped != null) {
                val link = state.links[tapped.deviceId]
                pendingNex = AlertTap.Pending(
                    machine = tapped.deviceId,
                    hellosAtTap = link?.hellos ?: 0,
                    upAtTap = link?.status == LinkHub.Status.CONNECTED && link.remote.features.isNotEmpty(),
                )
                viewModel.connect(activity, tapped)
                navigation.navigate(Destinations.AGENTS) { launchSingleTop = true }
            } else if (goes == AlertTap.GoesTo.NEX && connected != null) {
                // nex names no session in a push: its screen lists what waits.
                nexModel.open(connected)
                navigation.navigate(Destinations.NEX) { launchSingleTop = true }
            } else if (goes == AlertTap.GoesTo.SESSION && viewModel.openAlerted(target.machine, target.session)) {
                navigation.navigate(Destinations.SESSION) { launchSingleTop = true }
            } else {
                // The session is gone, or it belongs to a machine this phone
                // is not connected to. `SessionInfo.id` is reused after a
                // prune, so landing on whatever now holds that number would be
                // opening a stranger's agent — the Agent Center is the honest
                // destination, and it is where the user can see what IS there.
                navigation.navigate(
                    if (state.agents.machine != null) Destinations.AGENTS else Destinations.MACHINES,
                ) { launchSingleTop = true }
            }
        }

        // The connection a nex tap asked for (AlertTap.afterConnect says what
        // happens once it lands), forgotten after AlertTap.CONNECT_WAIT_MS.
        val pendingLink = pendingNex?.let { state.links[it.machine] }
        val pendingFeatures = pendingLink?.remote?.features?.takeIf { pendingNex?.fresh(pendingLink.hellos) == true }
        LaunchedEffect(pendingNex, state.agents.machine?.deviceId, pendingFeatures) {
            val want = pendingNex ?: return@LaunchedEffect
            val connected = state.agents.machine
            when (AlertTap.afterConnect(want.machine, connected?.deviceId, pendingFeatures)) {
                AlertTap.Then.WAIT -> Unit
                AlertTap.Then.GIVE_UP, AlertTap.Then.STAY -> pendingNex = null
                AlertTap.Then.OPEN_NEX -> {
                    pendingNex = null
                    nexModel.open(connected!!)
                    navigation.navigate(Destinations.NEX) { launchSingleTop = true }
                }
            }
        }
        LaunchedEffect(pendingNex) {
            if (pendingNex == null) return@LaunchedEffect
            kotlinx.coroutines.delay(AlertTap.CONNECT_WAIT_MS)
            pendingNex = null
        }

        // A link that carries a pairing code opens the screen that can explain
        // what accepting it would mean. It is deliberately NOT paired on
        // arrival: a URL that paired a phone by being opened would be a URL
        // worth sending somebody. Inside the graph, because navigating before
        // the `NavHost` has composed throws.
        LaunchedEffect(launchPayload) {
            if (launchPayload != null) {
                // `launchSingleTop`, because `onNewIntent` delivers a second
                // link while this screen is already open — the activity is
                // `singleTask` — and without it the second one stacks a
                // duplicate pairing screen on top of the first.
                navigation.navigate(Destinations.PAIRING) { launchSingleTop = true }
            }
        }

        // A pairing that worked takes the user back to the list it was added
        // to, which is where the next thing they want to do is.
        LaunchedEffect(state.machines.size) {
            if (state.message != null && navigation.currentDestination?.route == Destinations.PAIRING) {
                navigation.popBackStack()
            }
        }
    }
}

/**
 * The front door.
 *
 * Nothing behind it is a secret — the store holds public keys and machine
 * names — and that is worth saying out loud on the screen rather than implying
 * a strength this does not have. What it stops is a phone picked up off a table
 * telling somebody what computers its owner has.
 */
@Composable
private fun LockScreen(
    failure: String?,
    onUnlock: () -> Unit,
    /**
     * Companion mode is running (contract §2.10). Said without machine names:
     * this screen exists so a phone picked up off a table does not tell
     * anybody what computers its owner has.
     */
    companion: Boolean = false,
) {
    Column(
        Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("Rime Remote", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(10.dp))
        Text(
            "Unlock to see your computers.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (companion) {
            Spacer(Modifier.height(16.dp))
            Text(
                COMPANION_LOCKED,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
        }
        failure?.let {
            Spacer(Modifier.height(16.dp))
            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        }
        Spacer(Modifier.height(24.dp))
        Button(
            onClick = onUnlock,
            shape = RoundedCornerShape(6.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
            ),
        ) { Text("Unlock") }
    }
}

/** What the lock screen says while companion mode runs. */
const val COMPANION_LOCKED: String =
    "Locked. This phone's battery, notifications and what is playing keep reaching your computers, " +
        "and they can still ring it and send it files. Unlock for agents, terminals and computer controls."
