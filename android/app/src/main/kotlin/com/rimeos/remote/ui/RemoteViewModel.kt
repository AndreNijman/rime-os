package com.rimeos.remote.ui

import android.app.Application
import androidx.biometric.BiometricManager
import androidx.fragment.app.FragmentActivity
import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import com.rimeos.remote.core.agent.Handoff
import com.rimeos.remote.core.link.Upload
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rimeos.remote.core.Device
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.StaticKey
import com.rimeos.remote.core.agent.AgentSession
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.AlertWatcher
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.agent.Agentd
import com.rimeos.remote.core.agent.GrantState
import com.rimeos.remote.core.agent.Grants
import com.rimeos.remote.core.agent.PrivilegeRequest
import com.rimeos.remote.core.agent.AgentProfile
import com.rimeos.remote.core.agent.Project
import com.rimeos.remote.core.agent.ProjectRecord
import com.rimeos.remote.core.agent.Reply
import com.rimeos.remote.core.agent.SystemGrant
import com.rimeos.remote.core.agent.WorktreeStatus
import com.rimeos.remote.core.agent.Hello
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.agent.LiveTail
import com.rimeos.remote.core.agent.NotSubmitted
import com.rimeos.remote.core.agent.Push
import com.rimeos.remote.core.agent.SessionNames
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.push.PushRegistrar
import com.rimeos.remote.core.Pairing
import com.rimeos.remote.core.PairingException
import com.rimeos.remote.core.PairingOffer
import com.rimeos.remote.core.Settings
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.pairing.GatedBox
import com.rimeos.remote.core.Rendezvous
import com.rimeos.remote.pairing.PairingService
import com.rimeos.remote.security.AppLock
import com.rimeos.remote.security.AppLockRefused
import com.rimeos.remote.security.KeystoreSecretBox
import com.rimeos.remote.ui.term.TerminalController
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * What the screens read and what they can ask for.
 *
 * One state object and one flow rather than a graph of observable fields: every
 * screen here is a view of the same two things — the machines this phone knows
 * and what just happened — and splitting them would mean two sources that can
 * disagree about whether a pairing succeeded.
 */
data class UiState(
    val loading: Boolean = true,
    val machines: List<PairedMachine> = emptyList(),
    val settings: Settings = Settings(),
    /** `null` while the app lock has not yet been satisfied this launch. */
    val unlocked: Boolean = false,
    /** Why this device cannot gate a key, when it cannot. Shown, never hidden. */
    val gateWarning: String? = null,
    val busy: String? = null,
    val message: String? = null,
    val failure: String? = null,
    val connection: ConnectionReport? = null,
    /**
     * Whether this phone will actually show a notification.
     *
     * On screen rather than assumed, because the failure is silent: the poll
     * keeps raising alerts, `Notifier.post` keeps declining to show them, and
     * the user concludes the feature does not work.
     */
    val notificationsEnabled: Boolean = true,
    /** True while `POST_NOTIFICATIONS` has never been asked for (API 33+). */
    val notificationsUnasked: Boolean = false,
    /**
     * The last crash report, when consent allowed one to be kept (P1-060).
     *
     * Null both when there is none and when consent is off, which are the same
     * state on disk and deliberately the same state here: there is no path on
     * which a report exists and the user has not agreed to it existing.
     */
    val crash: String? = null,
    val agents: AgentUiState = AgentUiState(),
    /**
     * How each machine's held connection stands, by device id.
     *
     * The machines list draws it: which machine is connected, and whether
     * through the relay — the disclosure `docs/remote.md` requires a relay
     * path to make, at the moment it becomes true rather than in a settings
     * screen, now that the phone connects on unlock without being asked.
     */
    val links: Map<String, LinkHub.LinkState> = emptyMap(),
    /**
     * Companion mode is running: the app is locked and the device features
     * keep reaching the computers with the companion keys (contract §2.10).
     * The lock screen says so.
     */
    val companion: Boolean = false,
)

/**
 * What a successful connection proved.
 *
 * Kept from P1-053, and still shown when a connection is made and nothing else
 * is asked of it: the pinned key still matches, the device is still paired,
 * and the round trip came back on a frame that crossed the same path
 * everything else will.
 */
data class ConnectionReport(
    val machine: String,
    val roundTripMs: Long?,
    /**
     * Which way the connection went, so the user can be told.
     *
     * `docs/remote.md` requires a relay path to print its disclosure "rather
     * than leaving the reader to assume", and this is the phone's equivalent of
     * the `path` column `rime remote status` prints. It is here, on a report
     * that exists to be displayed, and not on the session — see
     * `PairingService.open`.
     */
    val path: Rendezvous.Path,
)

/** The Agent Center's state for one machine. */
data class AgentUiState(
    val machine: PairedMachine? = null,
    val sessions: List<AgentSession> = emptyList(),
    val hello: Hello? = null,
    /** The session whose detail screen is open. */
    val selected: AgentSession? = null,
    val busy: String? = null,
    val failure: String? = null,
    /**
     * Something that WORKED and that the user has to be told about anyway.
     *
     * Distinct from [failure], and distinct from nothing at all: a file handed
     * to an agent lands at a path the machine chose, and that path is also the
     * text typed into the agent's terminal. A person who sent a photo needs to
     * see it — it is what they will refer to when they type the rest of their
     * sentence — and showing it as a failure or not showing it would each be
     * wrong in a different direction.
     */
    val notice: String? = null,
    /** Refreshed on every poll, so elapsed times move without recomputing per row. */
    val nowSeconds: Long = System.currentTimeMillis() / 1000,
    val worktrees: WorktreesUiState = WorktreesUiState(),
    val approvals: ApprovalsUiState = ApprovalsUiState(),
    /** What the Start screen picks from (P1-054 criterion 2). */
    val picker: PickerUiState = PickerUiState(),
    /**
     * Text fetched from the MACHINE's clipboard and not yet put on the phone's.
     *
     * A one-shot: the screen copies it and calls back to clear it. It is here
     * rather than written to the clipboard from this class because a
     * `ClipboardManager` needs a `Context` and this is not an
     * `AndroidViewModel` — the alternative was giving the view model a context
     * for one feature, which is a worse trade than one nullable field.
     *
     * **Null also means "the machine's clipboard was empty"**, which is why
     * that case is reported through [notice] and never by leaving this null
     * and saying nothing. Writing an empty string to the phone's clipboard
     * would silently destroy whatever the person had copied on the phone,
     * which is the one outcome worse than telling them there was nothing.
     */
    val clipboardPull: ClipboardPull? = null,
    /**
     * Alerts raised by the last poll and not yet shown.
     *
     * Held here rather than delivered through a callback so that the mapping
     * from a poll to a notification is a value a test can read. `:app` drains
     * it; see `Notifier`.
     */
    val alerts: List<Alert> = emptyList(),
    /**
     * The bottom of the open session's screen, from `peek`, while the session
     * screen is showing it. Null when the machine cannot peek, or before the
     * first answer.
     */
    val peek: PeekView? = null,
    /**
     * The last line each live session printed, for the Agent Center's rows,
     * keyed by [PeekView.keyOf] — id AND start time, because ids are reused
     * after a prune and a line from a pruned session must not appear under
     * the session that took its number.
     */
    val lastLines: Map<String, String> = emptyMap(),
) {
    /**
     * Every transient banner on this machine's screens, cleared.
     *
     * Exists because [failure] and [notice] live HERE and `dismiss()` used to
     * clear only the fields on the state above this one. The Agent Center
     * draws `state.failure ?: state.agents.failure`, so a refusal raised by
     * `replyToSession` — which writes the second — was shown with a dismiss
     * button that cleared the first and left the banner on screen. Adding a
     * notice strip beside it would have added a second button that does
     * nothing, which is why this is fixed here rather than worked around at
     * the call site.
     *
     * [clipboardPull] is deliberately NOT cleared: it is not a banner, it is
     * work in flight, and dropping it would mean a user who dismissed a
     * message during the round trip never gets the text they asked for.
     */
    fun dismissed(): AgentUiState = copy(failure = null, notice = null)
}

/**
 * One answer to one `clipboard` request, waiting to be put on the phone.
 *
 * [token] exists so that asking twice for the SAME text still copies twice.
 * The screen consumes this from a `LaunchedEffect`, and a `LaunchedEffect`
 * keyed on the text alone would not re-run when the second answer equals the
 * first — so a user who tapped the button again, saw nothing happen and
 * concluded the feature was broken would be right about the symptom and wrong
 * about the cause. Keyed on the token, every answer is a new effect.
 */
data class ClipboardPull(val text: String, val token: Long)

/**
 * What a `peek` showed of one session: its bottom lines, already replayed
 * through the terminal emulator, and the width they were drawn at.
 *
 * Carries the session's id and start time so a screen can refuse to draw a
 * preview of a different session that has since taken the same number.
 */
data class PeekView(val session: Int, val started: Long, val lines: List<String>, val cols: Int) {
    fun isFor(s: AgentSession): Boolean = s.id == session && s.started == started

    companion object {
        fun keyOf(s: AgentSession): String = "${s.id}:${s.started}"
    }
}

/**
 * Projects, worktrees and the work in them (P1-056).
 *
 * [tooOld] is not a kind of failure: it is the answer a machine gives when its
 * Rime predates the `worktrees` verb, and it must not be drawn as an empty
 * list. It is also the ONLY answer obtainable from the image on this
 * developer's machine — 2026-09-05, against a verb that landed 2026-09-08.
 */
data class WorktreesUiState(
    val projects: List<Project> = emptyList(),
    /**
     * The runtime's own project records, joined to the groups by slug.
     *
     * A SECOND, cheap request beside the git walk, and the thing that makes
     * P1-056's "workspaces" real: §8's capsule binding lives on this record
     * and on nothing in the `worktrees` reply. Empty against a machine that
     * has the `worktrees` verb and not the `projects` one — a narrow window,
     * three days wide, but a real one — and a heading with no record simply
     * omits the line rather than claiming the project has no workspace.
     */
    val records: List<ProjectRecord> = emptyList(),
    val loading: Boolean = false,
    /** The last time this was asked, unix seconds, or 0 for never. */
    val askedSeconds: Long = 0,
    val tooOld: Boolean = false,
    val failure: String? = null,
) {
    val rows: List<WorktreeStatus> get() = projects.flatMap { it.rows }
    val everAsked: Boolean get() = askedSeconds > 0 || loading

    /**
     * The runtime's record for a grouped project, or null.
     *
     * Joined on the SLUG, which the daemon states on every worktree row, and
     * never on the name: two checkouts of one repository share a name and are
     * different projects, and on this developer's machine that is the normal
     * case rather than the exception. A group with no slug — a listing from a
     * daemon older than that field — matches nothing, which is correct: there
     * is no evidence about which record it is.
     */
    fun recordFor(project: Project): ProjectRecord? =
        project.slug.takeIf { it.isNotEmpty() }?.let { slug -> records.firstOrNull { it.slug == slug } }
}

/**
 * What the Start screen picks from (P1-054 criterion 2).
 *
 * ## Three fetches, and why they are three
 *
 * `projects` and `profiles` are cheap and are asked together when the screen
 * opens. `worktrees` is not — it runs git in the named project, including
 * `merge-tree --write-tree` — so it is asked for ONE project, only once the
 * user has chosen it. That split is the whole reason these are separate verbs:
 * a screen that fetched worktree status for every remembered project on open
 * would put seconds of git in front of a text field, which is the documented
 * reason this screen had no picker at all for four rounds.
 *
 * [tooOld] is not a kind of failure, and it is not an empty list. It is what a
 * machine says when its Rime predates these verbs, and every machine in the
 * field does today: they landed after the published image. A screen that drew
 * it as "no projects" would report a version skew as a fact about the machine
 * — and, worse here than on the Worktrees screen, would make the free-text
 * directory field look like the only thing that ever existed.
 */
data class PickerUiState(
    val projects: List<ProjectRecord> = emptyList(),
    val profiles: List<AgentProfile> = emptyList(),
    /**
     * Worktree names in the chosen project, or empty when none is chosen.
     *
     * Names, not paths: `RunRequest.worktree` is "create or reuse this git
     * worktree under the project", so what goes on the wire is the name. The
     * main tree is deliberately excluded — starting "in the main tree" is
     * leaving this blank, and offering the project's own name as a worktree
     * would create `.rime/worktrees/<project>`, a second checkout nobody asked
     * for.
     */
    val worktrees: List<String> = emptyList(),
    /** The slug [worktrees] was fetched for, so a stale answer is not shown. */
    val worktreesFor: String? = null,
    val loading: Boolean = false,
    val loadingWorktrees: Boolean = false,
    val asked: Boolean = false,
    val tooOld: Boolean = false,
    val failure: String? = null,
)

/**
 * Privilege requests and grants (P1-057).
 *
 * There is no `deciding` field and no approve action, because
 * `privilege.rs:1176` refuses a decision from a paired device unconditionally.
 * See `Approvals.kt`.
 */
data class ApprovalsUiState(
    val requests: List<PrivilegeRequest> = emptyList(),
    val grants: Grants = emptyMap(),
    val systemGrants: List<Pair<SystemGrant, GrantState>> = emptyList(),
    val loading: Boolean = false,
    val busy: String? = null,
    val failure: String? = null,
) {
    val pending: List<PrivilegeRequest> get() = requests.filter { it.isPending }
}

class RemoteViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = MachineRepository(application)
    private val pairing = PairingService()

    /**
     * Which sessions were in which state at the last poll (P1-058).
     *
     * ONE watcher, driven from ONE loop. It is documented as not thread-safe
     * and it genuinely is not: calling [AlertWatcher.observe] from a second
     * coroutine — a slower worktrees poll, say — would interleave its maps and
     * build exactly the race the class warns about. `startPolling` is the only
     * call site, and the worktrees it passes are the last ones fetched rather
     * than a second fetch of its own.
     */
    private val alerts = AlertWatcher()

    /**
     * Posts what the watcher raises.
     *
     * Called **from the poll loop itself**, not from a collector of
     * [AgentUiState.alerts], and that is the whole reason it is a field here
     * rather than a `LaunchedEffect` in the UI. A lifecycle-aware collector
     * stops at STOPPED — so alerts would pile up in the state while the app
     * was in the background and appear the moment the user opened it, which is
     * a notification that only arrives when you are already looking. The poll
     * runs in `viewModelScope`, which outlives the UI going away, so posting
     * from there is the only placement that notifies a phone in a pocket.
     *
     * The queue in the state is kept anyway: it is what an in-app surface
     * reads, and it is a value a test can look at, where a
     * `NotificationManager` call is not.
     */
    private val notifier = Notifier(application)

    /** Poll iterations, so a slower cadence can ride the same loop. */
    private var ticks: Long = 0

    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    /**
     * The connections and the unwrapped keys, which outlive this view model.
     *
     * They used to be fields here. They moved to [LinkHub] because "always
     * connected" means the connection lives as long as the app is unlocked,
     * and a view model lives only as long as a screen: backing out to the
     * launcher used to close every socket and forget every key. The key rule
     * is the hub's now, and unchanged — memory only, dropped on [lock].
     *
     * The reason the keys are unwrapped ONCE and handed to something that
     * cannot prompt is also unchanged: `PtyAttachment` calls its `connect`
     * lambda on every backoff retry, and a lambda that went through
     * `AppLock.unlock` would put a biometric dialog on the screen once per
     * reconnect — a prompt storm for as long as a tunnel lasts.
     */
    private val hub = LinkHub.get(application)

    /** The session screen's live preview, while it is on screen. */
    private var peekJob: Job? = null

    /** Whether the Agent Center's rows want their last lines (it is on screen). */
    @Volatile
    private var listPeeking = false

    /** The attached terminal, when there is one. At most one at a time. */
    var terminal: TerminalController? = null
        private set

    private var poll: Job? = null

    init {
        refresh()
        viewModelScope.launch {
            hub.states.collect { links -> _state.update { it.copy(links = links) } }
        }
        viewModelScope.launch {
            hub.companion.collect { on -> _state.update { it.copy(companion = on) } }
        }
        viewModelScope.launch {
            // Locked from somewhere else — the notification's Lock action —
            // must lock this screen too, not leave it drawing a machine whose
            // keys are gone.
            hub.unlocked.collect { unlocked ->
                if (!unlocked && _state.value.unlocked && !_state.value.loading &&
                    AppLock.canAuthenticate(BiometricManager.from(getApplication()))
                ) {
                    lockScreenOnly()
                }
            }
        }
    }

    fun refresh() = viewModelScope.launch {
        val store = repository.load()
        val manager = BiometricManager.from(getApplication())
        val canGate = AppLock.canAuthenticate(manager)
        _state.update {
            it.copy(
                loading = false,
                machines = store.machines,
                settings = store.settings,
                gateWarning = KeystoreSecretBox.insecureFallbackReason(canGate),
                // A phone that cannot authenticate at all cannot be asked to.
                // Refusing to show the app on such a device would protect
                // nothing — the key gate is already absent, and the store holds
                // no secrets — so the lock stands down and says why.
                //
                // And a process whose hub is still unlocked — the person backed
                // out with the connection held, and came back — is not asked
                // again for a connection that never closed.
                unlocked = it.unlocked || !canGate || hub.unlocked.value,
            )
        }
        if (!canGate && !hub.unlocked.value) {
            // No prompt is possible, so none is shown; the last machine is
            // connected all the same, because a key with no gate unwraps
            // without one.
            hub.markUnlocked()
            val target = store.lastUsed()
            if (target != null) {
                runCatching {
                    val box = KeystoreSecretBox.forDevice(target.deviceId, requireAuthentication = false)
                    val sealed = requireNotNull(com.rimeos.remote.core.Base64Url.decode(target.sealed))
                    if (!box.authenticationIsRequired) {
                        hub.remember(target.deviceId, com.rimeos.remote.core.InMemoryStaticKey(box.open(sealed)))
                        hub.hold(target)
                    }
                }
            }
        }
    }

    /**
     * The lock on the front door.
     *
     * Worth being plain about what this is and is not. The *real* protection is
     * that the device key is wrapped by a keystore key gated per use, so no
     * session can be opened without a prompt whatever any flag in this class
     * says. This is the visible lock on top of it: it stops a picked-up phone
     * showing which machines exist and what they are called. A screen is a
     * curtain and a key is a lock, and Rime Remote has both — in that order of
     * importance.
     */
    fun unlockApp(activity: FragmentActivity, prefer: String? = null) = viewModelScope.launch {
        // Not before [refresh] has answered. A phone that cannot authenticate
        // at all sets `unlocked` there; prompting first would error instantly,
        // leave a failure notice on screen, and then be overruled a moment
        // later by a refresh that says no lock was needed.
        if (_state.value.loading || _state.value.unlocked) return@launch
        // ONE prompt, where there used to be two: the front door and the key
        // of the machine used last, together. See `AppLock.unlockApp` for why
        // that is no weaker, and why it is one machine and not all of them.
        // The machine a tapped notification names, when there is one: that
        // is the machine the person is about to open.
        val store = repository.load()
        val target = prefer?.let { id -> store.machines.firstOrNull { it.deviceId == id } } ?: store.lastUsed()
        val box = target?.let { runCatching { KeystoreSecretBox.forDevice(it.deviceId) }.getOrNull() }
        val sealed = target?.let { com.rimeos.remote.core.Base64Url.decode(it.sealed) }
        try {
            val result = AppLock.unlockApp(activity, box, sealed, target?.machine)
            hub.markUnlocked()
            _state.update { it.copy(unlocked = true, failure = null) }
            if (target != null && result.identity != null) {
                hub.remember(target.deviceId, result.identity)
                // Connect now, without waiting for a tap: by the time the
                // person taps the machine the handshake has happened.
                hub.hold(target)
            }
        } catch (e: AppLockRefused) {
            _state.update { it.copy(failure = e.message) }
        }
    }

    /**
     * Lock the app, and mean it.
     *
     * Everything derived from an unwrapped key goes: the terminal detaches
     * (the session keeps running on the machine, which is the point), the
     * control connections close, and the keys themselves are dropped so the
     * next connection needs the prompt again. A lock that only hid the screen
     * would leave a live socket and a usable key behind it.
     */
    fun lock() {
        // The hub drops the keys and the visible links at once, on this
        // thread, because those are what "locked" means, and closes the
        // sockets on its IO dispatcher — Android kills a process that touches
        // a socket on the main thread (`StrictMode.enableDeathOnNetwork()`),
        // and a debug build does not relax it. It stops the foreground
        // service too: a lock that left the app "always connected" would not
        // be a lock.
        hub.lock()
        lockScreenOnly()
    }

    /** Everything [lock] does to THIS screen, for a lock that came from elsewhere. */
    private fun lockScreenOnly() {
        poll?.cancel()
        poll = null
        peekJob?.cancel()
        peekJob = null
        listPeeking = false
        terminal?.close()
        terminal = null
        _state.update {
            it.copy(unlocked = false, connection = null, agents = AgentUiState())
        }
    }

    /** Stop polling. Called when the Agent Center is no longer on screen. */
    fun leaveAgents() {
        poll?.cancel()
        poll = null
        listPeeking = false
        // The watcher's history goes with it. Coming back to a machine after
        // an hour away, every session's state is news to the phone but none of
        // it is news that just happened — and `AlertWatcher` raises nothing on
        // a first poll, which is exactly the behaviour wanted here.
        _state.value.agents.machine?.let { alerts.forget(it.deviceId) }
    }

    override fun onCleared() {
        super.onCleared()
        // The terminal is this screen's; the control links are NOT. They are
        // the hub's, held by the foreground service for as long as the app is
        // unlocked, and closing them here is exactly what used to make every
        // return to the app a fresh connection. `lock()` is what closes them.
        terminal?.close()
        terminal = null
    }

    /**
     * Read a scanned or pasted payload, and pair with what it names.
     *
     * The offer is decoded first and on its own, because every failure here is
     * one the user can act on — the wrong code, an expired one, a code for a
     * newer Rime — and because a payload that does not decode must never reach
     * a socket.
     */
    fun pair(activity: FragmentActivity, payload: String, deviceName: String) =
        viewModelScope.launch {
            val offer = try {
                Pairing.decodeOffer(payload)
            } catch (e: PairingException) {
                _state.update { it.copy(failure = describe(e), busy = null) }
                return@launch
            }
            val now = System.currentTimeMillis()
            if (offer.expiresMs in 1..<now) {
                _state.update {
                    it.copy(
                        failure = "that pairing code has expired. Run `rime remote pair` again " +
                            "on ${offer.machine} for a fresh one.",
                        busy = null,
                    )
                }
                return@launch
            }
            _state.update { it.copy(busy = "Pairing with ${offer.machine}…", failure = null) }
            try {
                val machine = pairing.pair(
                    offer = offer,
                    deviceName = Device.checkName(deviceName),
                    // The prompt happens inside this, before a byte is sent.
                    // What comes back carries the gate the key actually has,
                    // which is what the desktop is told — a phone that promised
                    // a second factor it does not have would weaken the machine
                    // as well as itself.
                    boxFor = { deviceId -> sealingBox(activity, deviceId, offer.machine) },
                    nowMs = now,
                )
                val store = repository.remember(machine)
                _state.update {
                    it.copy(
                        machines = store.machines,
                        busy = null,
                        message = "Paired with ${machine.machine}.",
                    )
                }
            } catch (e: Exception) {
                _state.update { it.copy(busy = null, failure = describe(e)) }
            }
        }

    /**
     * Unwrap this machine's key once, open the Agent Center, and keep both.
     *
     * The prompt happens HERE and nowhere deeper, and that is not tidiness:
     * `PtyAttachment` calls its `connect` lambda on every backoff retry, so a
     * lambda that unwrapped through `AppLock` would put a biometric dialog on
     * the screen once per reconnect — a prompt storm for as long as a tunnel
     * lasts. See [identities].
     */
    fun connect(activity: FragmentActivity, machine: PairedMachine) = viewModelScope.launch {
        _state.update {
            it.copy(
                busy = "Connecting to ${machine.machine}…",
                failure = null,
                // Connecting to a DIFFERENT machine starts from an empty
                // block, not a copy of the old one. Sessions were already
                // replaced wholesale by the first poll, but worktrees, grants
                // and pending approvals are only ever fetched on request — so
                // carrying them over would leave one laptop's projects, its
                // per-project grants and its waiting root operations on the
                // screen under another laptop's name, with nothing to say they
                // were not this machine's. Undelivered alerts are the one
                // thing kept: each names the machine that raised it, so they
                // still reach the right session.
                agents = if (it.agents.machine?.deviceId == machine.deviceId) {
                    it.agents.copy(machine = machine, failure = null)
                } else {
                    AgentUiState(machine = machine, alerts = it.agents.alerts)
                },
            )
        }
        try {
            openIdentity(activity, machine)
            // Held from now on: connected while the app is unlocked, and
            // reconnected when it drops. Usually already true — the machine
            // used last was connected on unlock — and then this costs nothing.
            hub.hold(machine)
            val link = hub.link(machine)
            val (hello, sessions) = withContext(Dispatchers.IO) {
                // `hello` first: it is the only request a mismatched client
                // can rely on, and the adapter list it carries is what the
                // start screen offers. Already asked on this connection when
                // the hub made it, so usually not asked again.
                val h = hub.state(machine.deviceId)?.hello ?: runCatching { link.hello() }.getOrNull()
                h to link.sessions()
            }
            _state.update {
                it.copy(
                    busy = null,
                    connection = null,
                    agents = it.agents.copy(
                        machine = machine,
                        hello = hello,
                        sessions = sessions,
                        nowSeconds = System.currentTimeMillis() / 1000,
                        failure = null,
                    ),
                )
            }
            startPolling(machine)
            registerForPush(machine)
        } catch (e: Exception) {
            _state.update { it.copy(busy = null, failure = describeConnectFailure(machine, e)) }
        }
    }

    /**
     * Arrange for this machine to be able to wake this phone (P1-058).
     *
     * ## Why it is here, at the end of a successful connection
     *
     * It is the only moment that has all three things it needs at once: a
     * distributor can be asked for an endpoint from anywhere, but telling the
     * machine needs the Noise channel, and the Noise channel needs the device
     * key — which is behind a per-use biometric gate that was satisfied a few
     * lines above and cannot be satisfied by a `BroadcastReceiver` woken at
     * three in the morning.
     *
     * ## It must never turn a working connection into a failure
     *
     * Push is an improvement on the poll loop, not a replacement for it, and
     * every way this can go wrong leaves the app exactly as useful as it was
     * before push existed. So nothing here touches `failure`, and each outcome
     * is deliberate:
     *
     * * no distributor installed — the commonest case on a fresh phone, and
     *   not an error. The user is told in Settings, not here.
     * * a machine too old to know the verb — `rime-remoted` forwards it to
     *   `rime-agentd`, which answers `bad_request`, which [Agentd.isTooOld]
     *   recognises. Also not an error: that machine polls, as every machine
     *   did before.
     * * anything else — the endpoint is simply not recorded as sent, so the
     *   next connection tries again. Retrying here would spend a round trip
     *   per attempt on a screen nobody is waiting on.
     */
    private suspend fun registerForPush(machine: PairedMachine) {
        val context = getApplication<Application>()
        runCatching {
            withContext(Dispatchers.IO) {
                PushRegistrar.registerIfNeeded(context, machine)
                // Re-read: `registerIfNeeded` may have just written a record,
                // and a distributor may have delivered an endpoint since this
                // machine was loaded onto the screen. Acting on the stale copy
                // would send an endpoint the phone has already replaced.
                val current = repository.load().find(machine.deviceId) ?: return@withContext
                val endpoint = Push.endpointToSend(current) ?: return@withContext
                val key = current.push?.key ?: return@withContext
                val link = linkOf(machine) ?: return@withContext
                link.pushRegister(endpoint, key)
                // Only now, and only for the endpoint that was actually sent.
                PushRegistrar.sent(context, current, endpoint)
            }
        }
    }

    /**
     * Refresh the list every few seconds while it is on screen.
     *
     * Four seconds, and not faster: each poll is a control round trip over a
     * Noise session, and the thing it is watching — an agent's state — changes
     * on a human timescale. A phone that asked every second would spend
     * battery to redraw the same list.
     */
    private fun startPolling(machine: PairedMachine) {
        poll?.cancel()
        poll = viewModelScope.launch {
            while (true) {
                delay(POLL_MS)
                val link = linkOf(machine) ?: return@launch
                val sessions = runCatching { withContext(Dispatchers.IO) { link.sessions() } }.getOrNull()

                // Privilege requests ride the same loop at a slower cadence,
                // because `requests` is a cheap read of a small store while
                // `worktrees` runs git in every project and must never be on a
                // four-second timer. The worktrees handed to the watcher are
                // the LAST ONES FETCHED by an explicit refresh rather than a
                // fetch of this loop's own; see the note on `alerts`.
                ticks += 1
                val requests = if (ticks % REQUESTS_EVERY == 0L) {
                    runCatching { withContext(Dispatchers.IO) { link.requests() } }.getOrNull()
                } else {
                    null
                }

                // The Agent Center's last lines, while it is on screen and the
                // machine can peek. One small request per live session, on the
                // same four seconds as the list, and none at all when the list
                // is not being looked at.
                val lines = if (listPeeking && sessions != null && helloFor(machine)?.has(Features.PEEK) == true) {
                    runCatching { withContext(Dispatchers.IO) { lastLinesOf(link, sessions) } }.getOrNull()
                } else {
                    null
                }

                val raised = if (sessions == null) {
                    // A poll that failed is not evidence that anything
                    // changed. Feeding an empty list to the watcher would
                    // "forget" every session and then re-announce all of them
                    // on the next successful poll.
                    emptyList()
                } else {
                    alerts.observe(
                        machine = machine.deviceId,
                        sessions = sessions,
                        worktrees = _state.value.agents.worktrees.rows,
                        requests = requests ?: _state.value.agents.approvals.requests,
                        nowMs = System.currentTimeMillis(),
                    ).also { raised ->
                        // Posted here and not from a collector: see the note
                        // on `notifier`. `NotificationManagerCompat.notify` is
                        // a binder call, not a socket write, so it does not
                        // belong on `Dispatchers.IO` and StrictMode has no
                        // objection to it here.
                        for (alert in raised) notifier.post(alert, machine.machine)
                    }
                }

                // A distributor answers a registration ASYNCHRONOUSLY, by
                // broadcasting to `PushReceiver` some time after `connect()`
                // asked. So the endpoint does not exist yet when `connect()`
                // looks for one, and without this the machine would first be
                // told on the NEXT connection — which the user would try once
                // and read as "push does not work".
                //
                // Ridden on the slower sub-cadence, not every four seconds:
                // when there is nothing to send this is one storage read, and
                // `registerIfNeeded` returns on the first line once a record
                // exists. When there IS something it is one control round trip,
                // once, because `sent` then makes `endpointToSend` answer null.
                if (ticks % REQUESTS_EVERY == 0L) registerForPush(machine)

                _state.update {
                    if (it.agents.machine?.deviceId != machine.deviceId) return@update it
                    it.copy(
                        agents = it.agents.copy(
                            sessions = sessions ?: it.agents.sessions,
                            // Advanced every poll whether or not the list
                            // changed, so elapsed times move.
                            nowSeconds = System.currentTimeMillis() / 1000,
                            selected = it.agents.selected?.let { chosen ->
                                sessions?.firstOrNull { s -> s.id == chosen.id } ?: chosen
                            },
                            approvals = if (requests == null) {
                                it.agents.approvals
                            } else {
                                it.agents.approvals.copy(requests = requests, failure = null)
                            },
                            alerts = it.agents.alerts + raised,
                            lastLines = lines ?: it.agents.lastLines,
                        ),
                    )
                }
            }
        }
    }

    /**
     * The last line of each live session, for the Agent Center's rows.
     *
     * A 4 KiB tail each — enough to reconstruct the bottom of a screen, a
     * half of what the session screen asks for — and at most [LIST_PEEKS]
     * sessions a round, so a machine running thirty agents costs no more
     * than one running twelve. Finished sessions are skipped: their last
     * line is not news, and their detail says how they ended.
     */
    private fun lastLinesOf(
        link: com.rimeos.remote.core.agent.MachineLink,
        sessions: List<AgentSession>,
    ): Map<String, String> {
        val out = HashMap<String, String>()
        for (s in sessions.filter { !it.isTerminal }.take(LIST_PEEKS)) {
            val tail = runCatching { link.peek(s.id, LIST_PEEK_BYTES) }.getOrNull() ?: continue
            LiveTail.lastLine(tail)?.let { out[PeekView.keyOf(s)] = it }
        }
        return out
    }

    /** The daemon's `hello` for [machine]: this screen's copy, else the hub's. */
    private fun helloFor(machine: PairedMachine): Hello? =
        _state.value.agents.hello?.takeIf { _state.value.agents.machine?.deviceId == machine.deviceId }
            ?: hub.state(machine.deviceId)?.hello

    /**
     * The Agent Center is on screen, or has left it. Its rows ask for last
     * lines only while it is.
     */
    fun watchList(on: Boolean) {
        listPeeking = on
    }

    /**
     * Show the bottom of [session]'s screen, refreshed every 1.5 s, until
     * [stopWatchingSession] — which the session screen calls when it leaves.
     *
     * Only against a machine whose `hello` lists [Features.PEEK]; otherwise
     * nothing is asked and the screen shows the session's `detail`, as it
     * always did. A peek does not attach, does not resize, and does not show
     * up in `attached`, so looking at a session from the phone changes
     * nothing on the desktop.
     */
    fun watchSession(session: AgentSession) {
        peekJob?.cancel()
        val machine = _state.value.agents.machine ?: return
        if (helloFor(machine)?.has(Features.PEEK) != true) {
            updateAgents(machine) { it.copy(peek = null) }
            return
        }
        peekJob = viewModelScope.launch {
            while (isActive) {
                val current = _state.value.agents.sessions
                    .firstOrNull { it.id == session.id && it.started == session.started }
                    ?: _state.value.agents.selected?.takeIf { it.id == session.id && it.started == session.started }
                    ?: break
                val link = linkOf(machine) ?: break
                val view = runCatching {
                    withContext(Dispatchers.IO) {
                        val tail = link.peek(current.id)
                        PeekView(current.id, current.started, LiveTail.lines(tail), tail.cols)
                    }
                }.getOrNull()
                if (view != null) updateAgents(machine) { it.copy(peek = view) }
                // A finished session's screen does not change; one last look
                // is the whole of it.
                if (current.isTerminal) break
                delay(PEEK_MS)
            }
        }
    }

    fun stopWatchingSession() {
        peekJob?.cancel()
        peekJob = null
        updateAgents { it.copy(peek = null) }
    }

    fun refreshAgents() = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        try {
            val sessions = withContext(Dispatchers.IO) { link.sessions() }
            _state.update {
                it.copy(
                    agents = it.agents.copy(
                        sessions = sessions,
                        nowSeconds = System.currentTimeMillis() / 1000,
                        failure = null,
                    ),
                )
            }
        } catch (e: Exception) {
            _state.update { it.copy(agents = it.agents.copy(failure = describe(e))) }
        }
    }

    /**
     * Rewrite the Agent Center's block, optionally only while [machine] is
     * still the connected one.
     *
     * The guard is not decoration. Every loader below captures its machine,
     * hands a request to `Dispatchers.IO`, and can come back up to five
     * minutes later — `Mux.request`'s deadline. In that window the user can go
     * back, connect to a different laptop and be looking at it, and an
     * unguarded write would then put one machine's worktrees, grants and
     * pending approvals on the screen under another machine's name. The
     * four-second poll already carries this check; the slow paths need it
     * more, not less.
     *
     * Passing null means "this is not about a particular machine" — used only
     * by [drainAlerts], which empties a queue rather than describing anything.
     */
    private fun updateAgents(machine: PairedMachine? = null, block: (AgentUiState) -> AgentUiState) =
        _state.update { s ->
            if (machine != null && s.agents.machine?.deviceId != machine.deviceId) {
                s
            } else {
                s.copy(agents = block(s.agents))
            }
        }

    private fun updateWorktrees(
        machine: PairedMachine? = null,
        block: (WorktreesUiState) -> WorktreesUiState,
    ) = updateAgents(machine) { it.copy(worktrees = block(it.worktrees)) }

    private fun updateApprovals(
        machine: PairedMachine? = null,
        block: (ApprovalsUiState) -> ApprovalsUiState,
    ) = updateAgents(machine) { it.copy(approvals = block(it.approvals)) }

    private fun updatePicker(
        machine: PairedMachine? = null,
        block: (PickerUiState) -> PickerUiState,
    ) = updateAgents(machine) { it.copy(picker = block(it.picker)) }

    // ---- the machine's clipboard (P1-059 criterion 3, receive) ----------

    /**
     * Put what is on the COMPUTER's clipboard onto this phone's.
     *
     * The receive half of P1-059's third criterion. The send half is the
     * `Paste` button in `ReplyBox`, which carries the phone's clipboard the
     * other way; the two are one criterion pointing in opposite directions.
     *
     * **Machine-scoped, and modelled on [loadWorktrees] rather than on
     * `replyToSession` for that reason.** `Request::Clipboard` carries no
     * session id — one Wayland seat has one clipboard — so this is a property
     * of the link and the only precondition is that the link is up. It is
     * offered from the Agent Center beside Projects, not from `ReplyBox`,
     * which returns early unless some agent is waiting: a person copies
     * something on the computer and wants it here regardless of what any
     * agent is doing.
     *
     * Explicit, never on the four-second poll, and for a sharper reason than
     * `worktrees`' cost: a clipboard is whatever was last copied, and polling
     * one would stream a person's passwords and tokens to a phone they did
     * not ask to send them to.
     *
     * Three outcomes, three different channels, because collapsing any two of
     * them misinforms:
     * * text — onto the phone's clipboard, and [notice] says how much;
     * * **empty — [notice], not [failure] and not silence.** The daemon's
     *   `Response::Clipboard` documents an empty string as a real answer, so
     *   drawing it as an error sends the user hunting for a permission to
     *   grant when the machine simply had nothing on it;
     * * refused or unreachable — [failure], with the daemon's own sentence.
     *
     * `withContext(Dispatchers.IO)` is mandatory, not stylistic:
     * `StrictMode.enableDeathOnNetwork()` kills the process for a socket on
     * the main thread, and `Mux.request` blocks for up to five minutes.
     */
    fun pullMachineClipboard() = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updateAgents(machine) {
            it.copy(busy = "Reading ${machine.machine}'s clipboard…", failure = null, notice = null)
        }
        try {
            val text = withContext(Dispatchers.IO) { link.clipboard() }
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    // Nothing is handed to the screen when there is nothing to
                    // copy: writing "" to the phone's clipboard would destroy
                    // whatever the person had copied here, to tell them the
                    // computer had nothing.
                    clipboardPull = if (text.isEmpty()) {
                        null
                    } else {
                        ClipboardPull(text, System.currentTimeMillis())
                    },
                    notice = if (text.isEmpty()) {
                        "${machine.machine}'s clipboard is empty. Nothing was copied here, and " +
                            "what you had copied on this phone is untouched."
                    } else {
                        val what = if (text.length == 1) "1 character" else "${text.length} characters"
                        "Copied $what from ${machine.machine}."
                    },
                )
            }
        } catch (e: AgentError) {
            val message = if (Agentd.isTooOld(e)) {
                // Not a refusal. `Request::Clipboard` landed 2026-09-13; an
                // older runtime answers `bad_request` for the verb itself,
                // which is the same error kind a refusal uses, and reporting
                // it as "permission denied" would send the user looking for a
                // setting that does not exist.
                "This machine's Rime is too old to share its clipboard. " +
                    "Run `sudo rime update` on the machine."
            } else {
                "Could not read ${machine.machine}'s clipboard: ${describe(e)}"
            }
            updateAgents(machine) { it.copy(busy = null, failure = message) }
        } catch (e: Exception) {
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    failure = "Could not read ${machine.machine}'s clipboard: ${describe(e)}",
                )
            }
        }
    }

    /**
     * The screen has put [AgentUiState.clipboardPull] on the phone's clipboard.
     *
     * Cleared here rather than left to be overwritten by the next pull, so
     * that the text does not sit in the view model's state for the rest of the
     * session. A clipboard holds whatever was last copied, which on a
     * developer's machine is routinely a token.
     */
    fun clipboardPullConsumed() = updateAgents { it.copy(clipboardPull = null) }

    // ---- worktrees (P1-056) ---------------------------------------------

    /**
     * Ask the machine about its projects and worktrees.
     *
     * Explicit, never on the four-second poll. Answering this makes the daemon
     * run git in EVERY remembered project, `merge-tree --write-tree` included,
     * and putting that on a timer would mean a phone in a pocket driving git
     * across a laptop's disk all day.
     *
     * `withContext(Dispatchers.IO)` is not optional here and not merely good
     * manners: `Mux.request` blocks for up to five minutes, so on the main
     * thread this is an ANR before `StrictMode.enableDeathOnNetwork()` gets
     * the chance to kill the process for touching a socket.
     */
    fun loadWorktrees() = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updateWorktrees(machine) { it.copy(loading = true, failure = null) }
        try {
            val projects = withContext(Dispatchers.IO) { link.projects() }
            // The cheap listing beside the expensive one, so a heading can
            // name the project's workspace. Its failure is NOT this request's
            // failure: a machine that walked its worktrees and cannot answer
            // `projects` has given the user everything this screen is
            // primarily for, and turning that into a red banner would report a
            // three-day version window as a broken screen.
            val records = runCatching {
                withContext(Dispatchers.IO) { link.projectRecords() }
            }.getOrDefault(emptyList())
            updateWorktrees(machine) {
                it.copy(
                    projects = projects,
                    records = records,
                    loading = false,
                    askedSeconds = System.currentTimeMillis() / 1000,
                    tooOld = false,
                    failure = null,
                )
            }
        } catch (e: AgentError) {
            // "This runtime does not have the verb" is not a failure to show as
            // one, and it is certainly not an empty list — a screen that drew
            // it as "no projects" would be reporting a version skew as a fact
            // about the machine.
            val old = Agentd.isTooOld(e)
            updateWorktrees(machine) {
                it.copy(
                    loading = false,
                    askedSeconds = System.currentTimeMillis() / 1000,
                    tooOld = old,
                    failure = if (old) null else describe(e),
                )
            }
        } catch (e: Exception) {
            updateWorktrees(machine) {
                it.copy(
                    loading = false,
                    askedSeconds = System.currentTimeMillis() / 1000,
                    failure = describe(e),
                )
            }
        }
    }

    // ---- the Start screen's pickers (P1-054 criterion 2) ----------------

    /**
     * Fetch what the Start screen picks from: projects and profiles.
     *
     * Both in ONE coroutine and not two, so the screen has one loading state
     * and one failure rather than two that can disagree — a half-loaded picker
     * that shows real projects beside a guessed adapter list is worse than one
     * that is still loading.
     *
     * Deliberately NOT on the four-second poll. These change when a person
     * opens a project or installs an agent, which is not something a phone
     * needs to watch; polling them would spend a round trip every four seconds
     * on an answer that is the same all day.
     *
     * `withContext(Dispatchers.IO)` is mandatory, not stylistic:
     * `StrictMode.enableDeathOnNetwork()` kills the process for a socket on
     * the main thread.
     */
    fun loadPickers() = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updatePicker(machine) { it.copy(loading = true, failure = null) }
        try {
            val (projects, profiles) = withContext(Dispatchers.IO) {
                link.projectRecords() to link.profiles()
            }
            updatePicker(machine) {
                it.copy(
                    projects = projects,
                    profiles = profiles,
                    loading = false,
                    asked = true,
                    tooOld = false,
                    failure = null,
                )
            }
        } catch (e: AgentError) {
            // A machine older than these verbs. Not a failure to draw as one,
            // and above all not an empty list: the screen falls back to the
            // free-text directory it has always had and says why, rather than
            // showing a picker with nothing in it and letting the user
            // conclude they have no projects.
            val old = Agentd.isTooOld(e)
            updatePicker(machine) {
                it.copy(
                    loading = false,
                    asked = true,
                    tooOld = old,
                    failure = if (old) null else describe(e),
                )
            }
        } catch (e: Exception) {
            updatePicker(machine) {
                it.copy(loading = false, asked = true, failure = describe(e))
            }
        }
    }

    /**
     * Fetch the worktree names in ONE project, for the worktree picker.
     *
     * The expensive half, asked only for the project the user chose. It is the
     * same `worktrees` verb the Worktrees screen uses, narrowed by slug — and
     * a slug is what it takes, never a path: the daemon resolves a slug by
     * searching the remembered set, so the directories it can be made to run
     * git in are exactly the ones the user already chose to remember.
     *
     * The answer is stamped with the slug it was for. Without that, choosing
     * project A, then B before A's git finished, would show A's worktrees
     * under B — and the user would start an agent in a tree belonging to
     * another project.
     */
    fun loadPickerWorktrees(slug: String) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        if (slug.isEmpty()) {
            updatePicker(machine) { it.copy(worktrees = emptyList(), worktreesFor = null) }
            return@launch
        }
        updatePicker(machine) { it.copy(loadingWorktrees = true) }
        try {
            val rows = withContext(Dispatchers.IO) { link.worktrees(slug) }
            updatePicker(machine) {
                it.copy(
                    // The main tree is not a worktree to create: see
                    // `PickerUiState.worktrees`.
                    worktrees = rows.filter { w -> w.isAgent }.map { w -> w.name },
                    worktreesFor = slug,
                    loadingWorktrees = false,
                )
            }
        } catch (e: Exception) {
            // Silent, and that is the choice rather than an omission. The
            // worktree field is free text and always was; a project whose
            // worktrees could not be listed still starts an agent in its main
            // tree, and a banner about a picker the user may not have wanted
            // would be noise on the way to the Start button. A real failure
            // shows when Start is pressed, from the daemon's own words.
            updatePicker(machine) {
                it.copy(worktrees = emptyList(), worktreesFor = slug, loadingWorktrees = false)
            }
        }
    }

    /**
     * Open the agent working in a worktree.
     *
     * The join from a worktree to a session is the part a client running git
     * itself could not compute, so it is the daemon's `sessions` list that is
     * followed here and never a guess from a path.
     */
    fun openWorktreeSession(id: Int): AgentSession? {
        val session = _state.value.agents.sessions.firstOrNull { it.id == id } ?: return null
        selectSession(session)
        return session
    }

    // ---- approvals (P1-057) ---------------------------------------------

    /**
     * Load privilege requests and both kinds of grant.
     *
     * There is no companion that DECIDES one. `privilege.rs:1176` refuses a
     * decision from any non-local origin before it even looks at whether the
     * request is pending, so a phone can neither approve nor deny; the screen
     * says where to do it instead. See `Approvals.kt`.
     */
    fun loadApprovals() = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updateApprovals(machine) { it.copy(loading = true, failure = null) }
        try {
            val loaded = withContext(Dispatchers.IO) {
                Triple(link.requests(), link.grants(), link.systemGrants())
            }
            updateApprovals(machine) {
                it.copy(
                    requests = loaded.first,
                    grants = loaded.second,
                    systemGrants = loaded.third,
                    loading = false,
                    failure = null,
                )
            }
        } catch (e: Exception) {
            // Never an empty list: a refusal drawn as "nothing is waiting"
            // tells the user that nothing needs their attention.
            updateApprovals(machine) { it.copy(loading = false, failure = describe(e)) }
        }
    }

    /**
     * Withdraw a grant, or every grant for a project when [key] is null.
     *
     * Not retried by `MachineLink`, so a dropped connection is reported as
     * "this may not have happened" rather than silently repeated — and the
     * repeat would answer `no_such_request`, reporting failure for something
     * that succeeded.
     */
    fun revokeGrant(project: String, key: String? = null) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updateApprovals(machine) { it.copy(busy = "Revoking…", failure = null) }
        try {
            val grants = withContext(Dispatchers.IO) { link.revoke(project, key) }
            updateApprovals(machine) { it.copy(grants = grants, busy = null) }
        } catch (e: Exception) {
            updateApprovals(machine) {
                it.copy(
                    busy = null,
                    failure = "The revoke may not have arrived: ${describe(e)}. " +
                        "Reload before trying again.",
                )
            }
        }
    }

    /** End a live system-access grant now. Same non-retry reasoning. */
    fun revokeSystemGrant(id: Int) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        updateApprovals(machine) { it.copy(busy = "Revoking…", failure = null) }
        try {
            withContext(Dispatchers.IO) { link.revokeSystemGrant(id) }
            updateApprovals(machine) { it.copy(busy = null) }
            loadApprovals()
        } catch (e: Exception) {
            updateApprovals(machine) {
                it.copy(
                    busy = null,
                    failure = "The revoke may not have arrived: ${describe(e)}. " +
                        "Reload before trying again.",
                )
            }
        }
    }

    // ---- alerts (P1-058) -------------------------------------------------

    /** Take the alerts raised since the last drain. Called by the notifier. */
    fun drainAlerts(): List<Alert> {
        val taken = _state.value.agents.alerts
        if (taken.isNotEmpty()) updateAgents { it.copy(alerts = emptyList()) }
        return taken
    }

    /**
     * Open the session a notification names, if this phone still has it.
     *
     * Returns false when it does not — a session pruned since the notification
     * was posted, or one belonging to a machine this phone is no longer
     * connected to. `SessionInfo.id` is REUSED after a prune
     * (`registry.rs:441`), so the machine is checked as well as the id and the
     * caller is expected to land on the Agent Center rather than on whatever
     * now holds that number.
     */
    fun openAlerted(machine: String, session: Int): Boolean {
        val current = _state.value.agents.machine ?: return false
        if (current.deviceId != machine) return false
        val found = _state.value.agents.sessions.firstOrNull { it.id == session } ?: return false
        selectSession(found)
        return true
    }

    fun selectSession(session: AgentSession?) =
        // The notice goes with the session it was about. "The agent has
        // /tmp/.../001-shot.png" is true of ONE agent, and leaving it on screen
        // while a different one is open says the wrong thing about the wrong
        // machine — which is the same class of mistake `Handoff.Voice.landsOn`
        // exists to prevent, one screen further out.
        _state.update { it.copy(agents = it.agents.copy(selected = session, notice = null)) }

    /**
     * Signal a session.
     *
     * Not retried when the connection drops — `MachineLink` refuses to, and it
     * is right: a SIGTERM delivered twice because its reply was lost is a
     * second signal into whatever the agent was doing next. The failure comes
     * back to the screen so a person can decide.
     */
    fun signal(session: AgentSession, signal: String) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        _state.update { it.copy(agents = it.agents.copy(busy = "Sending…", failure = null)) }
        try {
            withContext(Dispatchers.IO) { link.signal(session.id, signal) }
            _state.update { it.copy(agents = it.agents.copy(busy = null)) }
            refreshAgents()
        } catch (e: Exception) {
            _state.update {
                it.copy(
                    agents = it.agents.copy(
                        busy = null,
                        failure = "The signal may not have arrived: ${describe(e)}. " +
                            "Refresh before sending it again.",
                    ),
                )
            }
        }
    }

    // ---- handing a file to a waiting agent (P1-059 criterion 2) ---------

    /**
     * Send a photo, a screenshot or a file the user picked to a session.
     *
     * The name and the size come from the `ContentResolver`, which is where a
     * picker's URI keeps them; the bytes come from `openInputStream`. NOTHING
     * here asks for a storage permission, and that is the fourth criterion of
     * P1-059 rather than a nicety — `PickVisualMedia` and `OpenDocument` both
     * return a URI this app may read and nothing else, where
     * `READ_MEDIA_IMAGES` would hand it the whole library for the life of the
     * grant. `Handoff.Files.FORBIDDEN_PERMISSIONS` and `ManifestTest` are what
     * keep that true when somebody reaches for the easier way. The media
     * permissions the manifest does declare belong to "Photos on your
     * computer" alone; this path never asks for or relies on them.
     *
     * The recycled-id guard is `Reply.check`'s, and it applies for the same
     * reason it applies to a typed reply, with a worse consequence:
     * `SessionInfo.id` is reused after a prune, so a file addressed from a
     * screen that has gone stale can land in a different agent's inbox — and
     * that agent is then handed a path to somebody else's photo and told to
     * look at it.
     *
     * `withContext(Dispatchers.IO)` is mandatory and not stylistic:
     * `StrictMode.enableDeathOnNetwork()` is installed for every app targeting
     * API 11 or later and a debug build does not relax it, so any of this on
     * the main thread kills the process on the first frame. The
     * `ContentResolver` query is inside it too — not for StrictMode's sake, but
     * because a document provider's `query` is a binder call into another
     * process that can be slow for reasons this app does not control.
     */
    fun sendFileToSession(session: AgentSession, uri: Uri) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        val target = Reply.Target(machine.deviceId, session.id, session.started)
        Reply.check(target, machine.deviceId, _state.value.agents.sessions)?.let { refusal ->
            updateAgents(machine) { it.copy(failure = refusal.message, notice = null) }
            return@launch
        }

        val resolver = getApplication<Application>().contentResolver
        val details = runCatching {
            withContext(Dispatchers.IO) { describe(resolver, uri) }
        }.getOrNull()
        if (details == null) {
            updateAgents(machine) {
                it.copy(notice = null, failure = "That file could not be read. Nothing was sent.")
            }
            return@launch
        }
        val (name, size) = details
        if (Handoff.Files.preview(name) == null) {
            updateAgents(machine) { it.copy(notice = null, failure = Handoff.Files.UNUSABLE_NAME) }
            return@launch
        }
        if (size <= 0L) {
            updateAgents(machine) {
                it.copy(notice = null, failure = "That file is empty, and an empty file is not a handover.")
            }
            return@launch
        }
        if (size > Handoff.Files.MAX_BYTES) {
            updateAgents(machine) { it.copy(notice = null, failure = Handoff.Files.tooBig(size)) }
            return@launch
        }

        updateAgents(machine) { it.copy(busy = "Sending $name…", failure = null, notice = null) }
        try {
            val landed = withContext(Dispatchers.IO) {
                link.upload(
                    id = session.id,
                    name = name,
                    len = size,
                    source = {
                        resolver.openInputStream(uri)
                            ?: throw java.io.IOException("$name could not be opened")
                    },
                )
            }
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    // The machine's own path, which is also the text typed into
                    // the agent's terminal. One string because the daemon sends
                    // one: telling the user a different path from the one the
                    // agent was given is the whole reason `Response::Injected`
                    // has a single field.
                    notice = "Sent. The agent has ${landed.path}, typed but not submitted.",
                )
            }
            refreshAgents()
        } catch (e: Upload.Refused) {
            // The machine ANSWERED, so nothing was handed over, and the message
            // is its own sentence — the session that exited, the limit with its
            // number in it, the permission it refused.
            updateAgents(machine) { it.copy(busy = null, failure = e.message) }
        } catch (e: AgentError) {
            val message = if (Agentd.isTooOld(e)) {
                "This machine's Rime is too old to take a file from a phone. Nothing was sent."
            } else {
                e.toString()
            }
            updateAgents(machine) { it.copy(busy = null, failure = message) }
        } catch (e: Exception) {
            // Everything else is a connection that went, and the difference
            // matters: a refusal above means nothing was written, and this
            // means nobody knows. Said plainly rather than reassuringly.
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    failure = "The connection ended while the file was going over " +
                        "(${e.message}). Check the agent before sending it again.",
                )
            }
        }
    }

    /**
     * A picked URI's display name and size.
     *
     * `OpenableColumns` is the contract every document provider and the photo
     * picker implement. A provider that answers neither is a file this app
     * cannot describe — and the daemon needs the length UP FRONT, because it
     * commits to reading exactly that many bytes before a single one is sent.
     * So an unknown size is a refusal here rather than a guess, and reading
     * the whole file into memory to measure it would be the phone paying for
     * the daemon's cap twice.
     */
    private fun describe(resolver: ContentResolver, uri: Uri): Pair<String, Long>? {
        resolver.query(uri, null, null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) return null
            val nameAt = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            val sizeAt = cursor.getColumnIndex(OpenableColumns.SIZE)
            if (nameAt < 0 || sizeAt < 0 || cursor.isNull(sizeAt)) return null
            val name = cursor.getString(nameAt) ?: return null
            return name to cursor.getLong(sizeAt)
        }
        return null
    }

    // ---- replying to a waiting agent (P1-058) ---------------------------

    /**
     * Type a reply into a session, without attaching a terminal.
     *
     * Checked against the live list BEFORE it is sent: `SessionInfo.id` is
     * reused after a prune (`registry.rs:441`), so a reply composed from a
     * notification can arrive at a number that now belongs to a different
     * agent. `Reply.check` is what refuses that, and it refuses rather than
     * guesses — see `Reply.kt`.
     *
     * `withContext(Dispatchers.IO)` is mandatory and not stylistic:
     * `StrictMode.enableDeathOnNetwork()` is installed for every app targeting
     * API 11 or later and a debug build does not relax it, so this on the main
     * thread kills the process on the first send.
     */
    fun replyToSession(session: AgentSession, text: String) = viewModelScope.launch {
        val machine = _state.value.agents.machine ?: return@launch
        val link = linkOf(machine) ?: return@launch
        val target = Reply.Target(machine.deviceId, session.id, session.started)
        Reply.check(target, machine.deviceId, _state.value.agents.sessions)?.let { refusal ->
            updateAgents(machine) { it.copy(failure = refusal.message) }
            return@launch
        }
        updateAgents(machine) { it.copy(busy = "Sending…", failure = null, notice = null) }
        // The daemon presses Return itself when it can (`input_submit`), and
        // otherwise the phone does, a hundred milliseconds after the text —
        // see `Reply.plan` for the measurement that made one write not enough.
        val submits = helloFor(machine)?.has(Features.INPUT_SUBMIT) == true
        try {
            withContext(Dispatchers.IO) { link.reply(session.id, text, submitSupported = submits) }
            updateAgents(machine) { it.copy(busy = null) }
            // Asked for immediately: the agent's state changes the moment it
            // reads the line, and waiting up to four seconds to see it makes a
            // reply feel as though it went nowhere.
            refreshAgents()
        } catch (e: NotSubmitted) {
            // The one outcome where "send it again" is wrong: the words ARE on
            // the agent's input line, and a second send would put them there
            // twice. What is missing is a Return.
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    failure = "Your reply is typed on the agent's input line, but the Return that " +
                        "submits it did not arrive (${e.cause?.message ?: "the machine did not answer"}). " +
                        "Send a bare return to submit it — do not send the reply again.",
                )
            }
        } catch (e: AgentError) {
            // The daemon ANSWERED, so nothing was typed, and saying otherwise
            // would be the worst possible error message here: told the reply
            // "may have been delivered", the user does not resend, and their
            // agent waits for ever. `protocol.rs` states this for the
            // version-skew case in its own words — "the client reports that it
            // could not deliver, and nothing has been typed" — and it holds
            // for every other kind too: `refuse_input`'s permission_denied,
            // `no_such_session`, `session_exited`, and the `Input::Failed`
            // that becomes `internal` all refuse BEFORE or instead of the
            // write.
            val message = if (Agentd.isTooOld(e)) {
                // Not a refusal. `Request::Input` landed 2026-09-08; a machine
                // older than that answers `bad_request` for the verb itself,
                // which is the same error kind a refusal uses.
                "This machine's Rime is too old to accept a typed reply. Nothing was sent. " +
                    "Run `sudo rime update` on the machine, or attach the terminal instead."
            } else {
                "Nothing was sent: ${describe(e)}"
            }
            updateAgents(machine) { it.copy(busy = null, failure = message) }
        } catch (e: Exception) {
            // No answer came back, so whether the bytes landed is genuinely
            // unknown — a socket that dropped after the write reaching the PTY
            // looks identical to one that dropped before it. Never retried: a
            // retry on the first case types the user's sentence a second time
            // into an agent that has already acted on the first.
            updateAgents(machine) {
                it.copy(
                    busy = null,
                    failure = "The connection went before the machine answered, so this reply " +
                        "may or may not have been delivered: ${describe(e)}. Check the session " +
                        "before sending it again.",
                )
            }
        }
    }

    // ---- naming a session (contract §1.5) --------------------------------

    /**
     * Name [session], or clear its name when [raw] is blank.
     *
     * Checked here with the daemon's own rule first ([SessionNames.check]),
     * so a name that is too long is refused while the dialog is still open,
     * and against the live list second — `SessionInfo.id` is reused after a
     * prune, and a rename composed against a stale screen must not name a
     * different agent. A paused session may be renamed; one that has exited
     * may not (the daemon renames live sessions only).
     *
     * The answer is the updated session, applied to the list and to the
     * open screen at once, so the new name shows without waiting a poll.
     *
     * @return the refusal to show in the dialog, or null when it was sent.
     */
    fun renameSession(session: AgentSession, raw: String): String? {
        val machine = _state.value.agents.machine ?: return "Not connected."
        val name = when (val checked = SessionNames.check(raw)) {
            is SessionNames.Checked.Refused -> return checked.why
            is SessionNames.Checked.Ok -> checked.name
        }
        val target = Reply.Target(machine.deviceId, session.id, session.started)
        Reply.check(target, machine.deviceId, _state.value.agents.sessions)
            ?.takeIf { it != Reply.Refusal.PAUSED }
            ?.let { refusal ->
                return when (refusal) {
                    Reply.Refusal.EXITED -> "That agent has exited, and only a live session can be renamed."
                    else -> refusal.message
                }
            }
        val link = linkOf(machine) ?: return "Not connected."
        viewModelScope.launch {
            updateAgents(machine) { it.copy(busy = "Renaming…", failure = null) }
            try {
                val updated = withContext(Dispatchers.IO) { link.rename(session.id, name) }
                updateAgents(machine) {
                    it.copy(
                        busy = null,
                        sessions = it.sessions.map { s -> if (s.id == updated.id && s.started == updated.started) updated else s },
                        selected = it.selected?.let { s ->
                            if (s.id == updated.id && s.started == updated.started) updated else s
                        },
                    )
                }
            } catch (e: AgentError) {
                updateAgents(machine) { it.copy(busy = null, failure = "Not renamed: ${describe(e)}") }
            } catch (e: Exception) {
                updateAgents(machine) {
                    it.copy(
                        busy = null,
                        failure = "The connection went before the machine answered, so the rename may " +
                            "or may not have happened: ${describe(e)}. Refresh to see.",
                    )
                }
            }
        }
        return null
    }

    /** Whether the connected machine can rename sessions at all. */
    fun canRename(): Boolean =
        _state.value.agents.machine?.let { helloFor(it)?.has(Features.RENAME) } == true

    // ---- notifications --------------------------------------------------

    /** Create the channel and record whether anything will be shown. */
    fun refreshNotificationState() {
        notifier.ensureChannel()
        _state.update {
            it.copy(
                notificationsEnabled = notifier.enabled,
                notificationsUnasked = notifier.permissionNeeded,
            )
        }
    }

    fun startAgent(
        cwd: String,
        agent: String?,
        worktree: String?,
        prompt: String?,
        checkpoint: Boolean = false,
        args: List<String> = emptyList(),
    ) =
        viewModelScope.launch {
            val machine = _state.value.agents.machine ?: return@launch
            val link = linkOf(machine) ?: return@launch
            _state.update { it.copy(agents = it.agents.copy(busy = "Starting…", failure = null)) }
            try {
                val session = withContext(Dispatchers.IO) {
                    // 80x24 because nothing has been laid out yet; the terminal
                    // screen sends a real resize the moment it measures itself.
                    link.run(
                        cwd = cwd,
                        cols = 80,
                        rows = 24,
                        agent = agent,
                        prompt = prompt,
                        worktree = worktree,
                        checkpoint = checkpoint,
                        args = args,
                        // So the "this adapter needs a program" refusal is the
                        // daemon's own rule rather than this app's guess at
                        // it. Empty when the machine predates `profiles`, and
                        // the id rule still applies there.
                        profiles = _state.value.agents.picker.profiles,
                    )
                }
                _state.update {
                    it.copy(
                        agents = it.agents.copy(
                            busy = null,
                            sessions = listOf(session) + it.agents.sessions.filterNot { s -> s.id == session.id },
                            selected = session,
                        ),
                    )
                }
            } catch (e: Exception) {
                _state.update { it.copy(agents = it.agents.copy(busy = null, failure = describe(e))) }
            }
        }

    /**
     * Attach to a session's PTY.
     *
     * A second connection, not this link's: `rime-remoted` answers a control
     * frame from a single-threaded loop that blocks while `rime-agentd`
     * thinks, and a privilege request waits as long as the person does.
     * Sharing would freeze the terminal behind a prompt somebody has not
     * answered.
     */
    fun attach(session: AgentSession): TerminalController? {
        val machine = _state.value.agents.machine ?: return null
        if (hub.identity(machine.deviceId) == null) return null
        terminal?.close()
        // A channel on the connection the app already holds, when the
        // machine's `rime-remoted` says that is safe (`mux_attach`): no
        // handshake, no second relay dial, and the terminal is there at once.
        // Otherwise a connection of its own, as before.
        val link = hub.link(machine)
        val shared = hub.state(machine.deviceId)?.remote?.has(Features.MUX_ATTACH) == true
        val controller = TerminalController(
            sessionId = session.id,
            // Cannot prompt, and must not: see [hub].
            connect = { hub.dialSession(machine) },
            shared = if (shared) ({ link.shared() }) else null,
        )
        terminal = controller
        return controller
    }

    /**
     * Leave the terminal. The session goes on running on the machine.
     *
     * `close()` is safe to call from here: `TerminalController` puts its own
     * socket work on its io thread, for the same reason [lock] does.
     */
    fun detach() {
        terminal?.close()
        terminal = null
    }

    /**
     * Open [machine]'s key (one prompt, unless it is already open), then run
     * [then]. For screens that dial the machine themselves, like "Send to
     * Rime"; a refusal goes to [onFailure] in words.
     */
    fun withKey(
        activity: FragmentActivity,
        machine: PairedMachine,
        onFailure: (String) -> Unit,
        then: () -> Unit,
    ) = viewModelScope.launch {
        try {
            openIdentity(activity, machine)
        } catch (e: AppLockRefused) {
            onFailure(e.message ?: "The key for ${machine.machine} was not opened.")
            return@launch
        } catch (e: Exception) {
            if (e is kotlinx.coroutines.CancellationException) throw e
            onFailure(describeConnectFailure(machine, e))
            return@launch
        }
        then()
    }

    private suspend fun openIdentity(activity: FragmentActivity, machine: PairedMachine): StaticKey {
        hub.identity(machine.deviceId)?.let { return it }
        val box = KeystoreSecretBox.forDevice(machine.deviceId)
        val sealed = requireNotNull(com.rimeos.remote.core.Base64Url.decode(machine.sealed)) {
            "the stored key for ${machine.machine} is not base64url"
        }
        val identity = AppLock.unlock(activity, box, sealed, machine.machine)
        hub.remember(machine.deviceId, identity)
        return identity
    }

    /**
     * The control link for [machine], or null when its key is not unwrapped
     * this process — which, since the hub outlives screens, means the app was
     * locked, and a screen asking for it has outlived that.
     */
    private fun linkOf(machine: PairedMachine): com.rimeos.remote.core.agent.MachineLink? =
        if (hub.identity(machine.deviceId) == null) null else hub.link(machine)

    /**
     * Open a session, prove it works, and hang up.
     *
     * Kept for the machines list, where "does this still work" is the question
     * being asked and nothing more is wanted.
     */
    fun ping(activity: FragmentActivity, machine: PairedMachine) = viewModelScope.launch {
        _state.update { it.copy(busy = "Connecting to ${machine.machine}…", failure = null) }
        try {
            val identity = openIdentity(activity, machine)
            val report = withContext(Dispatchers.IO) {
                val connected = pairing.open(machine, identity)
                connected.session.use { session ->
                    ConnectionReport(session.machine, session.measureRoundTrip(), connected.path)
                }
            }
            _state.update { it.copy(busy = null, connection = report, message = null) }
        } catch (e: Exception) {
            _state.update { it.copy(busy = null, failure = describeConnectFailure(machine, e)) }
        }
    }

    /**
     * A connection failure, in words that name the actual cause.
     *
     * The keystore's own exceptions do not. A key invalidated by a newly
     * enrolled fingerprint — which is deliberate, and is what
     * `setInvalidatedByBiometricEnrollment(true)` buys — surfaces as
     * `KeyPermanentlyInvalidatedException` at `Cipher.init`; a keystore that was
     * wiped surfaces as an AEAD tag mismatch, because [KeystoreSecretBox.forDevice]
     * helpfully created a fresh key that cannot open the old ciphertext. Both
     * mean the same thing to the person holding the phone, and neither says it.
     */
    private fun describeConnectFailure(machine: PairedMachine, e: Throwable): String = when {
        e is android.security.keystore.KeyPermanentlyInvalidatedException ||
            e is javax.crypto.AEADBadTagException ->
            "the key this phone used with ${machine.machine} is no longer usable — a new " +
                "fingerprint or a new screen lock invalidates it, which is the point. " +
                "Forget ${machine.machine} and pair again."
        else -> describe(e)
    }

    fun forget(machine: PairedMachine) = viewModelScope.launch {
        // Push first, while the connection and the stored record still exist.
        //
        // Three parties hold state about waking this phone and forgetting the
        // machine has to end all three. The desktop drops its registration on
        // revoke by itself (`control.rs`), but a machine forgotten from this
        // side may never be told — it may be off, or on another network — so
        // the distributor and the push server would go on holding a live
        // subscription to a phone that will never read it. `PushRegistrar`
        // unregisters with the distributor; the round trip below is
        // best-effort and its failure changes nothing here.
        runCatching {
            withContext(Dispatchers.IO) {
                linkOf(machine)?.pushUnregister()
            }
        }
        // The companion key too: the machine is told to forget it while the
        // link is up, and its keystore key goes either way.
        runCatching { withContext(Dispatchers.IO) { hub.unpairCompanion(machine.deviceId) } }
        hub.forget(machine.deviceId)
        PushRegistrar.forget(getApplication<Application>(), machine)
        val store = repository.forget(machine)
        _state.update {
            it.copy(
                machines = store.machines,
                message = "${machine.machine} forgotten, and so is the key this phone used with it.",
                connection = null,
            )
        }
    }

    /** Show [pkg]'s notifications on the paired computers, or stop (contract §2.7). */
    fun setMirrorApp(pkg: String, on: Boolean) = viewModelScope.launch {
        val apps = _state.value.settings.mirrorApps.let { if (on) it + pkg else it - pkg }
        val store = repository.settings(_state.value.settings.copy(mirrorApps = apps))
        hub.setMirrorApps(store.settings.mirrorApps)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Send replies typed on a computer from this phone, or stop (contract §2.7). */
    fun setMirrorReply(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(mirrorReply = on))
        hub.setMirrorReply(store.settings.mirrorReply)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let the paired computers turn Do Not Disturb on and off here and follow it, or stop (contract §2.9). */
    /**
     * "Keep working after the phone restarts" (contract §2.10). On mints a
     * companion key for each machine and registers it with every one
     * connected now; off forgets where it can and deletes every key here at
     * once. The store is read back afterwards, so a later settings write from
     * this screen does not put the old switch back.
     */
    fun setCompanionOn(on: Boolean) = viewModelScope.launch {
        withContext(Dispatchers.IO) { hub.setCompanionOn(on) }
        val store = repository.load()
        _state.update { it.copy(settings = store.settings, machines = store.machines) }
    }

    fun setFocusOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(focusOn = on))
        hub.setFocusOn(store.settings.focusOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let the paired computers read this phone's SMS, and send while unlocked, or stop (contract §2.11). */
    fun setMessagesOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(messagesOn = on))
        hub.setMessagesOn(store.settings.messagesOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let the paired computers see and handle this phone's calls, or stop (contract §2.12). */
    fun setCallsOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(callsOn = on))
        hub.setCallsOn(store.settings.callsOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let the paired computers see and fetch this phone's photos while it is unlocked, or stop (contract §2.13). */
    fun setPhotosOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(photosOn = on))
        hub.setPhotosOn(store.settings.photosOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Photo access may have changed, or the picked photos: the machines hear the new `can`. */
    fun onPhotosPermissionChanged() {
        hub.onPhotosPermissionChanged()
    }

    /** The call or contacts permissions may have changed: the machines hear the new `can`. */
    fun onCallsPermissionChanged() {
        hub.onCallsPermissionChanged()
    }

    /** The SMS or contacts permissions may have changed: the machines hear the new `can`. */
    fun onSmsPermissionChanged() {
        hub.onSmsPermissionChanged()
    }

    /** Put the paired computers' clipboard on this phone's, or stop (contract §2.14). */
    fun setClipboardOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(clipboardOn = on))
        hub.setClipboardOn(store.settings.clipboardOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Show what this phone is playing on the paired computers, or stop (contract §2.8). */
    fun setMediaOn(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(mediaOn = on))
        hub.setMediaOn(store.settings.mediaOn)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let the paired computers control what is shown there, or only show it (contract §2.8). */
    fun setMediaControl(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(mediaControl = on))
        hub.setMediaControl(store.settings.mediaControl)
        _state.update { it.copy(settings = store.settings) }
    }

    /** Let [pkg]'s playback show on the paired computers, or stop (contract §2.8). */
    fun setMediaApp(pkg: String, on: Boolean) = viewModelScope.launch {
        val apps = _state.value.settings.mediaApps.let { if (on) it + pkg else it - pkg }
        val store = repository.settings(_state.value.settings.copy(mediaApps = apps))
        hub.setMediaApps(store.settings.mediaApps)
        _state.update { it.copy(settings = store.settings) }
    }

    fun setDynamicColour(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(dynamicColour = on))
        _state.update { it.copy(settings = store.settings) }
    }

    // ---- crash reports (P1-060) -----------------------------------------

    /**
     * Turn keeping a crash report on or off.
     *
     * Turning it OFF also deletes whatever is already kept, and that is the
     * half worth writing down: consent withdrawn has to mean the file goes,
     * not that the next one is skipped. A report sitting on disk from before
     * somebody changed their mind is exactly the thing they changed their mind
     * about.
     */
    fun setCrashReports(on: Boolean) = viewModelScope.launch {
        val store = repository.settings(_state.value.settings.copy(crashReports = on))
        if (!on) repository.clearCrash()
        _state.update { it.copy(settings = store.settings, crash = if (on) it.crash else null) }
    }

    /** Read whatever the last crash left, for the settings dialog to offer. */
    fun refreshCrash() = viewModelScope.launch {
        _state.update { it.copy(crash = repository.crash()) }
    }

    /** Forget the kept report, once it has been read or sent. */
    fun clearCrash() = viewModelScope.launch {
        repository.clearCrash()
        _state.update { it.copy(crash = null) }
    }

    fun dismiss() = _state.update {
        it.copy(
            message = null,
            failure = null,
            connection = null,
            // The agent-scoped banners too. Without this the Agent Center's
            // dismiss button cleared whichever of the two `failure` fields it
            // was NOT showing. See `AgentUiState.dismissed`.
            agents = it.agents.dismissed(),
        )
    }

    private suspend fun sealingBox(
        activity: FragmentActivity,
        deviceId: String,
        machineName: String,
    ): GatedBox {
        val canGate = AppLock.canAuthenticate(BiometricManager.from(getApplication()))
        val box = KeystoreSecretBox.forDevice(deviceId, requireAuthentication = canGate)
        // `authenticationIsRequired` and not `canGate`: the first is read off
        // the key with `KeyInfo`, the second is a question asked of the system,
        // and the desktop is told the one that is true of the key protecting
        // this pairing.
        return GatedBox(
            AppLock.authoriseSeal(activity, box, machineName),
            box.authenticationIsRequired,
        )
    }

    /**
     * A sentence a person can act on.
     *
     * The protocol's own exceptions already carry one — `:core` writes its
     * messages for a reader rather than for a log — so this mostly gets out of
     * the way. What it does not do is print the exception class, which is how
     * "the machine refused this pairing code" becomes
     * "PairingException: Refused".
     */
    private fun describe(e: Throwable): String = when (e) {
        is PairingException -> e.error.message
        is AppLockRefused -> e.message ?: "the unlock was cancelled"
        else -> e.message ?: e::class.java.simpleName
    }

    private companion object {
        /**
         * Four seconds between list refreshes.
         *
         * Each one is a control round trip over a Noise session, and what it
         * watches — an agent's state — changes on a human timescale. A phone
         * polling every second would spend battery redrawing the same list.
         */
        const val POLL_MS: Long = 4_000

        /**
         * Ask for privilege requests every eighth poll — about half a minute.
         *
         * On its own loop it would be a second call site for
         * [AlertWatcher.observe], which is not thread-safe and says so. Riding
         * this one keeps that to one.
         */
        const val REQUESTS_EVERY: Long = 8

        /** How often the session screen's live preview is refreshed. */
        const val PEEK_MS: Long = 1_500

        /** The Agent Center's per-row tail: enough for the bottom of a screen. */
        const val LIST_PEEK_BYTES: Int = 4_096

        /** At most this many rows are previewed per poll. */
        const val LIST_PEEKS: Int = 12
    }
}
