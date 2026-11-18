package com.rimeos.remote.link

import android.content.Context
import android.os.SystemClock
import android.util.Log
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Session
import com.rimeos.remote.core.StaticKey
import com.rimeos.remote.core.agent.Hello
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.agent.RemoteHello
import com.rimeos.remote.core.phone.CompanionKeys
import com.rimeos.remote.core.phone.CompanionLink
import com.rimeos.remote.core.phone.Content
import com.rimeos.remote.core.phone.DeviceEvent
import com.rimeos.remote.core.phone.Focus
import com.rimeos.remote.core.phone.Media
import com.rimeos.remote.core.phone.Calls
import com.rimeos.remote.core.phone.Messages
import com.rimeos.remote.core.phone.MediaOutbox
import com.rimeos.remote.core.phone.MediaState
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.core.phone.MirrorOutbox
import com.rimeos.remote.core.phone.OwnDnd
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.core.phone.PhoneLink
import com.rimeos.remote.core.phone.Photos
import com.rimeos.remote.core.phone.ReportThrottle
import com.rimeos.remote.core.phone.Scope
import com.rimeos.remote.phone.CallWatcher
import com.rimeos.remote.phone.ComputerClipboard
import com.rimeos.remote.phone.MediaWatcher
import com.rimeos.remote.phone.MirrorService
import com.rimeos.remote.phone.PhoneCalls
import com.rimeos.remote.phone.PhoneStatus
import com.rimeos.remote.phone.PhotoStore
import com.rimeos.remote.phone.Receiver
import com.rimeos.remote.phone.Ringer
import com.rimeos.remote.phone.Sms
import com.rimeos.remote.phone.SmsSender
import com.rimeos.remote.phone.SmsWatcher
import com.rimeos.remote.core.link.Route
import com.rimeos.remote.core.link.Upload
import com.rimeos.remote.data.MachineRepository
import com.rimeos.remote.pairing.PairingService
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking

/**
 * The connections this phone holds open, and the keys they were opened with.
 *
 * ## Why this is not the view model's any more
 *
 * "I want it just always connected." A view model lives as long as a screen
 * does; the connection has to live as long as the app is UNLOCKED, which is
 * longer — through the screen going off, through backing out to the launcher
 * — and a foreground service ([LinkService]) is what keeps the process around
 * for that. The service and the screens both need the same links and the same
 * unwrapped keys, so they live here, once per process, and each of the two is
 * a view onto this.
 *
 * ## The key rule has not moved
 *
 * [identities] is IN MEMORY and nowhere else, exactly as it was in the view
 * model: unwrapping a device key needs a biometric prompt, and the unwrapped
 * key never reaches `AppStorage`. What changed is only how long the memory
 * lasts — the process instead of the screen — and the process ends with the
 * app lock ([lock]) or with Android killing it. After either, the next
 * connection needs the prompt again, by design. The service is
 * `START_NOT_STICKY` for the same reason: a process Android restarted on its
 * own has no keys, and a notification saying "connected" over no connection
 * would be a lie.
 *
 * ## Reconnecting
 *
 * A held machine whose link drops is reconnected on [BACKOFF_MS]'s schedule;
 * a network change skips the wait ([onNetworkChanged]). Nothing here prompts:
 * the identity was unwrapped once, on unlock or on the first tap, and every
 * reconnect reuses it.
 *
 * ## Companion mode (contract §2.10)
 *
 * With "Keep working after the phone restarts" on, locking does not end
 * everything: the full links close and their keys are forgotten as before,
 * and then each machine is dialled again with its COMPANION key, which opens
 * with nobody present. Those links live in [companionLinks], never in
 * [links], so nothing that asks for a machine's link ([link], [dialSession])
 * can be handed one; and they run only [CompanionLink.features]: the report,
 * the notification mirror, what is playing, and the events channel. A boot
 * receiver starts the same mode after a restart ([startCompanion]).
 * Unlocking closes them and connects with the full key again.
 */
class LinkHub private constructor(private val app: Context) {
    private val repository = MachineRepository(app)
    private val pairing = PairingService()
    private val keys = CompanionKeys(repository.companionVault())
    /**
     * Where every connection attempt runs. The handler is not decoration: an
     * exception escaping a `launch` here would reach the thread's uncaught
     * handler and end the process — a phone with no network would crash the
     * app it is trying to reconnect. Each launch catches its own failures as
     * well; this is the belt.
     */
    private val scope = CoroutineScope(
        SupervisorJob() + Dispatchers.IO + CoroutineExceptionHandler { _, e ->
            Log.w(TAG, "a connection task failed: ${e.message}")
        },
    )

    /** Device keys unwrapped this process, by device id. Memory only; see the class note. */
    private val identities = ConcurrentHashMap<String, StaticKey>()

    /** One control link per machine. */
    private val links = ConcurrentHashMap<String, MachineLink>()

    /** Machines the app wants connected while it is unlocked. */
    private val held = ConcurrentHashMap.newKeySet<String>()

    /** Companion keys opened this process, by device id. Memory only, like [identities]. */
    private val companionIdentities = ConcurrentHashMap<String, StaticKey>()

    /** One companion link per machine, while in companion mode. Never in [links]. */
    private val companionLinks = ConcurrentHashMap<String, MachineLink>()

    /** Machines held with the companion key while the app is locked. */
    private val companionHeld = ConcurrentHashMap.newKeySet<String>()

    @Volatile
    private var companionRunning = false

    private val _companion = MutableStateFlow(false)

    /**
     * Whether companion mode is running: the app is locked and the device
     * features keep reaching the machines with the companion key.
     */
    val companion: StateFlow<Boolean> = _companion.asStateFlow()

    /**
     * Whether the device features may run now: unlocked, or in companion
     * mode. Agent, terminal, nex and machine control still need [unlocked].
     */
    val live: Boolean get() = _unlocked.value || companionRunning

    /** The reconnect waiting to run for each machine, if any. */
    private val reconnects = ConcurrentHashMap<String, Job>()
    private val attemptsSinceUp = ConcurrentHashMap<String, Int>()

    /** How each link's current connection was made, set by the connect lambda. */
    private val lastDial = ConcurrentHashMap<String, Dial>()

    // The phone's own side of each connection (contract §2.5): its report,
    // and the event channel a machine rings it on. One of each per machine;
    // a new connection replaces both. Each machine's reports go out one at a
    // time from one sender, read at the moment each is sent (§2.9): two
    // launches on the IO pool could land Do Not Disturb's on and off in the
    // wrong order and leave the computer with the old one.
    private class ReportLane(
        val remote: RemoteHello,
        val wake: Channel<Unit>,
        var sender: Job? = null,
        /** A companion link's lane: its `can` never offers replies. */
        val companion: Boolean = false,
        /** Its connection's phone side, which `messages_changed` goes out on (§2.11). */
        val phone: PhoneLink? = null,
    ) {
        val throttle = ReportThrottle()

        /** The call this connection was last told of (§2.12); only its sender changes it. */
        var toldCall: Calls.Now = Calls.Now.IDLE_NOW
    }

    private val reportLanes = ConcurrentHashMap<String, ReportLane>()
    private val eventChannels = ConcurrentHashMap<String, java.io.Closeable>()
    // This phone's notifications on each machine (contract §2.7): what waits
    // for it and what it shows, and the one sender that writes them in order.
    // A launch per notification on the IO pool could let a removal overtake
    // its own post, which would leave it on the computer for good.
    // `reply`: the machine's hello lists the reply feature, so it may be sent
    // a reply label; one that does not gets the same post without it.
    private class MirrorLane(
        val outbox: MirrorOutbox,
        val wake: Channel<Unit>,
        var sender: Job? = null,
        val reply: Boolean = false,
    )

    private val mirrorLanes = ConcurrentHashMap<String, MirrorLane>()

    /** The apps the person chose, read from the store on a connection and set by the screen. */
    @Volatile
    private var mirrorApps: Set<String> = emptySet()

    /** Whether the person turned on replying from a computer; read and set as [mirrorApps] is. */
    @Volatile
    private var mirrorReply: Boolean = false

    /** "Do Not Disturb follows your computer" (§2.9); read and set as [mirrorApps] is. */
    @Volatile
    private var focusOn: Boolean = false

    /** "Get the computer's clipboard" (§2.14); read and set as [mirrorApps] is. */
    @Volatile
    private var clipboardOn: Boolean = false

    /** A Do Not Disturb this app turned on for a computer, to take back when the switch goes off or the app locks. */
    private val ownDnd = OwnDnd()

    // What this phone is playing, on each machine (contract §2.8): one
    // outbox and one sender per CONNECTION, because the machine removes the
    // phone's player when the connection that described it ends, so a new
    // connection starts from "nothing shown".
    // `control`: the machine's hello offers phone_media_control, so it is
    // sent `can` (while the switch is on) and may send media_control.
    private class MediaLane(
        val outbox: MediaOutbox,
        val wake: Channel<Unit>,
        val control: Boolean,
        var sender: Job? = null,
    )

    private val mediaLanes = ConcurrentHashMap<String, MediaLane>()

    /** "Media on your computer", and its apps: read from the store on a connection and set by the screen. */
    @Volatile
    private var mediaOn: Boolean = false

    @Volatile
    private var mediaApps: Set<String> = emptySet()

    /** "Control from your computer": read from the store on a connection and set by the screen. */
    @Volatile
    private var mediaControl: Boolean = true

    /** What the watcher last said is playing, so a machine that connects is told at once. */
    @Volatile
    private var media: MediaState? = null

    private val watcher by lazy { MediaWatcher(app) { onPhoneMedia(it) } }

    /** "Messages on your computer" (§2.11): read from the store on a connection and set by the screen. */
    @Volatile
    private var messagesOn: Boolean = false

    /** "Calls on your computer" (§2.12): read from the store on a connection and set by the screen. */
    @Volatile
    private var callsOn: Boolean = false

    private val callWatcher by lazy { CallWatcher(app) { onCallChanged() } }

    /** "Photos on your computer" (§2.13): read from the store on a connection and set by the screen. */
    @Volatile
    private var photosOn: Boolean = false

    /**
     * The connections answering a photo listing now, and those sending an
     * original: one of each at a time per connection, so a computer asking
     * again and again cannot keep the phone reading its whole library.
     * A listing does not wait for a send, nor a send for a listing.
     */
    private val photosListing = ConcurrentHashMap.newKeySet<PhoneLink>()
    private val photosSending = ConcurrentHashMap.newKeySet<PhoneLink>()

    private val smsSender by lazy { SmsSender(app) }
    private val smsWatcher by lazy { SmsWatcher(app) { onSmsChanged() } }

    /** Whether the watcher was last told to run; only [syncMediaWatcher] changes it. */
    private var watching = false
    private val ringer by lazy { Ringer(app) }
    private val receiver by lazy { Receiver(app) }

    private class Dial(val route: Route?, val ms: Long, val relay: Boolean)

    /** Where a machine's link stands, for the machines list and the notification. */
    enum class Status { IDLE, CONNECTING, CONNECTED, RECONNECTING }

    data class LinkState(
        val machine: String,
        val status: Status = Status.IDLE,
        /** `lan` or `relay`, once connected. */
        val via: String? = null,
        /** How long the connection took to make, once connected. */
        val connectMs: Long? = null,
        /** What the daemon can do, from its `hello` on this connection. */
        val hello: Hello? = null,
        /** What `rime-remoted` can do, from `remote_hello` on this connection. */
        val remote: RemoteHello = RemoteHello.NONE,
        /**
         * How many times [remote] and [hello] have been asked on a new
         * connection in this process. [remote] keeps the last connection's
         * answer through a drop and until the next answer lands, so a screen
         * waiting for THIS connection's hello waits for this to move.
         */
        val hellos: Int = 0,
        /** Why the last attempt failed, while reconnecting. */
        val failure: String? = null,
    )

    private val _states = MutableStateFlow<Map<String, LinkState>>(emptyMap())
    val states: StateFlow<Map<String, LinkState>> = _states.asStateFlow()

    private val _unlocked = MutableStateFlow(false)

    /**
     * Whether the app is unlocked, for this process.
     *
     * Process-wide rather than per screen, because the connection is: a
     * person who backed out of the app with the link still held and came
     * back is not asked to authenticate again for a connection that never
     * closed. Locking — the button, or the notification's action — ends it.
     */
    val unlocked: StateFlow<Boolean> = _unlocked.asStateFlow()

    /**
     * The app was unlocked. Companion mode, if it was running, ends here: its
     * links close and the full key takes over on the next [hold].
     */
    fun markUnlocked() {
        _unlocked.value = true
        stopCompanion()
    }

    fun identity(deviceId: String): StaticKey? = identities[deviceId]

    fun remember(deviceId: String, identity: StaticKey) {
        identities[deviceId] = identity
    }

    fun state(deviceId: String): LinkState? = _states.value[deviceId]

    /** The link to [machine], made on first use. Opens nothing by itself. Always the full key's. */
    fun link(machine: PairedMachine): MachineLink = links.computeIfAbsent(machine.deviceId) { id ->
        // `computeIfAbsent`, not Kotlin's `getOrPut`: that one is a read and
        // then a write, and `hold` on the IO dispatcher and a tap on the main
        // thread could each make a link — two connections to one machine,
        // one of them orphaned.
        makeLink(machine, Scope.FULL) { identities[id] }
    }

    /** The companion link to [machine], dialled with its companion key. Never handed out. */
    private fun companionLink(machine: PairedMachine): MachineLink =
        companionLinks.computeIfAbsent(machine.deviceId) { id -> makeLink(machine, Scope.COMPANION) { companionIdentities[id] } }

    private fun makeLink(machine: PairedMachine, dialled: Scope, key: () -> StaticKey?): MachineLink {
        val id = machine.deviceId
        lateinit var self: MachineLink
        self = MachineLink(
            connect = {
                val identity = key()
                    ?: throw IllegalStateException("${machine.machine} is locked")
                setState(id, machine.machine) {
                    it.copy(status = if (it.status == Status.RECONNECTING) Status.RECONNECTING else Status.CONNECTING)
                }
                // The CURRENT record, not the one this link was made with:
                // the LAN hints and the last good path are rewritten after
                // every connection, and a reconnect should use what the last
                // one learnt.
                val current = repository.loadBlocking().find(id) ?: machine
                val connected = runBlocking { pairing.open(current, identity) }
                lastDial[id] = Dial(connected.route, connected.elapsedMs, connected.route == Route.Relay)
                connected.session
            },
            onEvent = { event -> onLinkEvent(machine, self, event) },
            dialled = dialled,
        )
        return self
    }

    /**
     * A connection of its own for a terminal, on a machine that does not
     * allow terminals on the control connection.
     *
     * Cannot prompt, and must not: `PtyAttachment` calls this on every
     * reconnect. A locked machine is an exception, which the attachment
     * reports as a lost connection.
     */
    fun dialSession(machine: PairedMachine): Session {
        val identity = identities[machine.deviceId]
            ?: throw IllegalStateException("${machine.machine} is locked")
        val current = repository.loadBlocking().find(machine.deviceId) ?: machine
        return runBlocking { pairing.open(current, identity) }.session
    }

    /**
     * Keep [machine] connected while the app is unlocked, and connect it now.
     *
     * Starts the foreground service if it is not running. Returns at once;
     * the connection is made on the IO dispatcher and reported through
     * [states].
     */
    fun hold(machine: PairedMachine) {
        if (identities[machine.deviceId] == null) return
        held.add(machine.deviceId)
        LinkService.start(app)
        // A failure is already recorded in [states] and a reconnect scheduled
        // by [connectNow]; nothing is left to do with the exception here.
        scope.launch { runCatching { connectNow(machine) } }
    }

    /**
     * Connect now, if not already connected, and learn what the machine can
     * do. Blocking callers use this from the IO dispatcher.
     */
    fun connectNow(machine: PairedMachine) {
        // Locked in companion mode: the companion key's link. Otherwise the
        // full key's, which needs the identity an unlock put in memory.
        val link = if (!_unlocked.value && companionRunning) companionLink(machine) else link(machine)
        try {
            link.connectNow()
        } catch (e: Exception) {
            val keep = wanted(machine.deviceId)
            setState(machine.deviceId, machine.machine) {
                it.copy(
                    status = if (keep) Status.RECONNECTING else Status.IDLE,
                    failure = e.message ?: e::class.java.simpleName,
                )
            }
            if (keep) scheduleReconnect(machine)
            throw e
        }
    }

    /** Whether [id] should be kept connected right now, by either key. */
    private fun wanted(id: String): Boolean =
        (_unlocked.value && id in held) || (companionRunning && id in companionHeld)

    /** The link currently carrying [id], by either key. */
    private fun current(id: String): MachineLink? = links[id] ?: companionLinks[id]

    /**
     * The phone's network changed. Every held link is asked whether it
     * survived — a ping and a few seconds, not the forty-second watchdog —
     * and anything waiting to reconnect goes now instead of after its
     * backoff.
     */
    fun onNetworkChanged() {
        if (!live) return
        val ids = if (_unlocked.value) held.toList() else companionHeld.toList()
        for (id in ids) {
            // The store read inside the launch too: this is called on the
            // ConnectivityManager's binder thread, which is not the place for
            // a disk read either.
            scope.launch {
                val machine = machineFor(id) ?: return@launch
                val link = current(id)
                val alive = link != null && link.connected && link.probe()
                if (!alive) {
                    reconnects.remove(id)?.cancel()
                    attemptsSinceUp[id] = 0
                    runCatching { connectNow(machine) }
                }
            }
        }
    }

    /**
     * Lock: every link closes, every key is forgotten, the service stops.
     *
     * The sockets close on the IO dispatcher — this may be called from the
     * main thread, and Android kills a process that touches a socket there.
     */
    fun lock() {
        _unlocked.value = false
        held.clear()
        reconnects.values.forEach { it.cancel() }
        reconnects.clear()
        val closing = links.values.toList()
        links.clear()
        identities.clear()
        lastDial.clear()
        reportLanes.values.forEach { it.sender?.cancel() }
        reportLanes.clear()
        eventChannels.clear()
        mirrorLanes.values.forEach { it.sender?.cancel() }
        mirrorLanes.clear()
        mediaLanes.values.forEach { it.sender?.cancel() }
        mediaLanes.clear()
        media = null
        syncMediaWatcher()
        _states.value = emptyMap()
        scope.launch { closing.forEach { runCatching { it.close() } } }
        // With the switch on, the device features carry on with the companion
        // keys, and Do Not Disturb keeps following the computer. Otherwise a
        // locked app follows no computer: what it turned on for one ends here.
        if (!startCompanion()) {
            releaseOwnDnd("locked")
            LinkService.stop(app)
        }
    }

    /**
     * Start companion mode: dial every machine that holds a registered
     * companion key, with that key, and run the device features over it.
     *
     * From the lock above and from the boot receiver (through [LinkService]).
     * Decides synchronously, so the service can tell at once whether it has
     * anything to keep alive; the keys are opened and the machines dialled on
     * the IO dispatcher. False, and nothing started, when the app is
     * unlocked, the switch is off, or no machine has a registered key.
     */
    fun startCompanion(): Boolean {
        if (_unlocked.value) return false
        if (companionRunning) return true
        val store = runCatching { repository.loadBlocking() }.getOrNull() ?: return false
        if (!BootStart.shouldStart(store)) return false
        val machines = store.companions()
        readSettings(store.settings)
        companionRunning = true
        _companion.value = true
        companionHeld.addAll(machines.map { it.deviceId })
        LinkService.start(app)
        scope.launch {
            for (m in machines) {
                val identity = runCatching { keys.identity(m) }
                    .onFailure { Log.w(TAG, "${m.machine}: the companion key did not open: ${it.message}") }
                    .getOrNull()
                if (identity == null || !companionRunning) continue
                companionIdentities[m.deviceId] = identity
                launch { runCatching { connectNow(m) } }
            }
            if (companionIdentities.isEmpty() && companionRunning && !_unlocked.value) {
                stopCompanion()
                LinkService.stop(app)
            }
        }
        return true
    }

    /** End companion mode: its links close and its keys leave memory. */
    private fun stopCompanion() {
        if (!companionRunning && companionLinks.isEmpty()) return
        companionRunning = false
        _companion.value = false
        val closing = companionLinks.values.toList()
        companionLinks.clear()
        companionIdentities.clear()
        for (id in companionHeld) {
            reconnects.remove(id)?.cancel()
            reportLanes.remove(id)?.sender?.cancel()
            eventChannels.remove(id)?.let { runCatching { it.close() } }
            mirrorLanes[id]?.let { lane ->
                lane.sender?.cancel()
                lane.sender = null
            }
            mediaLanes.remove(id)?.sender?.cancel()
            if (id !in held) _states.update { it - id }
        }
        companionHeld.clear()
        syncMediaWatcher()
        scope.launch { closing.forEach { runCatching { it.close() } } }
    }

    /**
     * Tear everything down without starting companion mode, as a process
     * that has just started is: nothing held, no keys in memory. For the
     * device suite, to stand in for a restart before the boot broadcast.
     */
    fun shutdown() {
        stopCompanion()
        if (_unlocked.value) {
            _unlocked.value = false
        }
        held.clear()
        reconnects.values.forEach { it.cancel() }
        reconnects.clear()
        val closing = links.values.toList()
        links.clear()
        identities.clear()
        lastDial.clear()
        reportLanes.values.forEach { it.sender?.cancel() }
        reportLanes.clear()
        eventChannels.values.forEach { runCatching { it.close() } }
        eventChannels.clear()
        mirrorLanes.values.forEach { it.sender?.cancel() }
        mirrorLanes.clear()
        mediaLanes.values.forEach { it.sender?.cancel() }
        mediaLanes.clear()
        media = null
        syncMediaWatcher()
        _states.value = emptyMap()
        LinkService.stop(app)
        scope.launch { closing.forEach { runCatching { it.close() } } }
    }

    /**
     * The person turned "Keep working after the phone restarts" on or off.
     * Blocking: call from the IO dispatcher. On mints and registers a key with
     * every machine connected now (and the rest on their next connect); off
     * forgets where it can, owes the rest, and deletes every key at once.
     */
    fun setCompanionOn(on: Boolean) {
        val connected = links.mapNotNull { (id, link) ->
            val remote = _states.value[id]?.remote
            if (link.connected && remote != null) id to (link to remote) else null
        }.toMap()
        if (on) keys.turnOn(connected) else keys.turnOff(connected)
    }

    /** How many machines hold a registered companion key. For the screen and the device suite. */
    fun companionKeys(): Int = runCatching { repository.loadBlocking().companions().size }.getOrDefault(0)

    /**
     * [deviceId] is being unpaired: it is told to forget the companion key if
     * its full link is up, and the key is deleted here. Blocking.
     */
    fun unpairCompanion(deviceId: String) {
        keys.unpair(deviceId, links[deviceId]?.takeIf { it.connected })
    }

    /** Forget one machine: its link, its key, and holding it. */
    fun forget(deviceId: String) {
        held.remove(deviceId)
        reconnects.remove(deviceId)?.cancel()
        identities.remove(deviceId)
        lastDial.remove(deviceId)
        reportLanes.remove(deviceId)?.sender?.cancel()
        eventChannels.remove(deviceId)?.let { runCatching { it.close() } }
        mirrorLanes.remove(deviceId)?.sender?.cancel()
        mediaLanes.remove(deviceId)?.sender?.cancel()
        syncMediaWatcher()
        _states.update { it - deviceId }
        val link = links.remove(deviceId)
        companionHeld.remove(deviceId)
        companionIdentities.remove(deviceId)
        val companionLink = companionLinks.remove(deviceId)
        if (held.isEmpty() && !companionRunning) LinkService.stop(app)
        scope.launch {
            runCatching { link?.close() }
            runCatching { companionLink?.close() }
        }
    }

    /** The machines currently held, with their names, for the notification. */
    fun heldNames(): List<String> =
        (if (_unlocked.value) held else companionHeld).mapNotNull { _states.value[it]?.machine }

    /** The companion link to [deviceId], for the device suite's evidence. Never for sending. */
    fun companionLinkOf(deviceId: String): MachineLink? = companionLinks[deviceId]

    private fun machineFor(deviceId: String): PairedMachine? =
        runCatching { repository.loadBlocking().find(deviceId) }.getOrNull()

    private fun onLinkEvent(machine: PairedMachine, link: MachineLink, event: MachineLink.LinkEvent) {
        val id = machine.deviceId
        // A link that was replaced or closed (locking, unlocking, forgetting)
        // says nothing about the one carrying this machine now: a late Dropped
        // from the companion link must not tear down the full link's lanes.
        if (links[id] !== link && companionLinks[id] !== link) return
        when (event) {
            is MachineLink.LinkEvent.Connected -> {
                reconnects.remove(id)?.cancel()
                attemptsSinceUp[id] = 0
                val dial = lastDial[id]
                setState(id, machine.machine) {
                    it.copy(
                        status = Status.CONNECTED,
                        via = dial?.route?.kind,
                        connectMs = dial?.ms,
                        failure = null,
                    )
                }
                // Off this thread: the event arrives from inside the link's
                // own connect, and asking a question from there would be a
                // request made from inside a request.
                scope.launch { afterConnect(machine, link, dial) }
            }
            is MachineLink.LinkEvent.Silent ->
                Log.i(TAG, "${machine.machine}: nothing arrived for ${event.forMs} ms, reconnecting")
            is MachineLink.LinkEvent.Dropped -> {
                val keep = wanted(id)
                setState(id, machine.machine) {
                    it.copy(status = if (keep) Status.RECONNECTING else Status.IDLE, via = null, connectMs = null)
                }
                mirrorLanes[id]?.let { lane ->
                    lane.sender?.cancel()
                    lane.sender = null
                    synchronized(lane.outbox) { lane.outbox.drop() }
                }
                mediaLanes.remove(id)?.sender?.cancel()
                reportLanes.remove(id)?.sender?.cancel()
                syncMediaWatcher()
                if (keep) scheduleReconnect(machine)
            }
        }
    }

    /**
     * What every new connection asks first, in one round trip's time.
     *
     * `remote_hello` and `hello` are sent together rather than one after the
     * other, because each is a round trip and over the relay a round trip is
     * a noticeable fraction of a second. Safe because `Mux` writes requests
     * in the order it queues their replies (its `sendOrder`), and the daemon
     * answers in wire order. Then the store learns the path that worked and the addresses
     * the machine is listening on now, so the next connection starts from
     * both.
     */
    private suspend fun afterConnect(machine: PairedMachine, link: MachineLink, dial: Dial?) {
        // `hello` is the agent daemon's, so only a scope that runs the agent
        // screens asks it; a companion link sends `remote_hello` alone.
        val agent = CompanionLink.Feature.AGENT in CompanionLink.features(link.scope)
        val remote = scope.async { runCatching { link.remoteHello() }.getOrNull() }
        val hello = scope.async { if (agent) runCatching { link.hello() }.getOrNull() else null }
        val r = remote.await()
        val h = hello.await()
        setState(machine.deviceId, machine.machine) {
            it.copy(hello = h ?: it.hello, remote = r ?: it.remote, hellos = it.hellos + 1)
        }
        r?.let { phoneSide(machine, link, it) }
        // On a full link, bring the companion key in line with the switch:
        // register (again, idempotently) while it is on, or pay a forget owed.
        if (r != null && link.scope == Scope.FULL) {
            runCatching { keys.sync(machine.deviceId, link, r) }
                .onFailure { Log.i(TAG, "${machine.machine}: companion key not synced: ${it.message}") }
        }
        runCatching {
            repository.updateBlocking { store ->
                var next = store
                dial?.route?.let { next = next.connected(machine.deviceId, it.key, System.currentTimeMillis()) }
                r?.let { next = next.withLan(machine.deviceId, it.lanHints()) }
                next
            }
        }
    }

    /**
     * The phone's side of a new connection: report the battery, and open the
     * channel the machine rings this phone on. Only what the machine's hello
     * offers is sent; an older machine gets nothing it does not know.
     */
    private fun phoneSide(machine: PairedMachine, link: MachineLink, remote: RemoteHello) {
        val id = machine.deviceId
        val phone = PhoneLink(link)
        // A companion link runs the device features only, and of those, never
        // the two that let a computer act for the person: replying to a
        // notification and controlling playback. Those wait for an unlock.
        val features = CompanionLink.features(link.scope)
        val companion = link.scope == Scope.COMPANION
        // Before the report, which says whether replies are taken.
        runCatching { readSettings(repository.loadBlocking().settings) }
        if (remote.has(Phone.REPORT) && CompanionLink.Feature.REPORT in features) {
            startReports(machine, phone, remote, companion)
        } else {
            reportLanes.remove(id)?.sender?.cancel()
        }
        if (remote.has(Mirror.FEATURE) && CompanionLink.Feature.MIRROR in features) {
            startMirror(machine, phone, remote.has(Mirror.REPLY_FEATURE) && !companion)
        } else {
            mirrorLanes.remove(id)?.sender?.cancel()
        }
        if (remote.has(Media.FEATURE) && CompanionLink.Feature.MEDIA in features) {
            startMedia(machine, phone, remote.has(Media.CONTROL_FEATURE) && !companion)
        } else {
            mediaLanes.remove(id)?.sender?.cancel()
            syncMediaWatcher()
        }
        eventChannels.remove(id)?.let { runCatching { it.close() } }
        if (remote.has(Phone.EVENTS) && remote.has(Features.MUX_ATTACH) && CompanionLink.Feature.EVENTS in features) {
            runCatching {
                phone.subscribe(
                    listener = { event -> onDeviceEvent(machine, phone, event, companion) },
                    onEnd = { cause -> Log.i(TAG, "${machine.machine}: event channel ended: ${cause?.message}") },
                )
            }.onSuccess { eventChannels[id] = it }
                .onFailure { Log.w(TAG, "${machine.machine}: event channel did not open: ${it.message}") }
        }
    }

    /**
     * The sender of one machine's reports, for its new connection: the first
     * report goes at once, then one whenever [ReportThrottle] says a change
     * is worth it.
     */
    private fun readSettings(settings: com.rimeos.remote.core.Settings) {
        mirrorApps = settings.mirrorApps
        mirrorReply = settings.mirrorReply
        focusOn = settings.focusOn
        clipboardOn = settings.clipboardOn
        mediaOn = settings.mediaOn
        mediaApps = settings.mediaApps
        mediaControl = settings.mediaControl
        messagesOn = settings.messagesOn
        syncSmsWatcher()
        callsOn = settings.callsOn
        syncCallWatcher()
        photosOn = settings.photosOn
    }

    private fun startReports(machine: PairedMachine, phone: PhoneLink, remote: RemoteHello, companion: Boolean) {
        val lane = reportLanes.compute(machine.deviceId) { _, old ->
            old?.sender?.cancel()
            ReportLane(remote, Channel(Channel.CONFLATED), companion = companion, phone = phone)
        }!!
        lane.sender = scope.launch {
            while (isActive) {
                lane.wake.receive()
                report(machine, phone, lane)
                // After the report, which is what lets the machine take it.
                tellCall(machine, phone, lane)
            }
        }
        lane.wake.trySend(Unit)
    }

    /**
     * One report, if one is due: the battery, the network, Do Not Disturb
     * and `can` as they are now. `dnd` and "focus" go only to a machine that
     * offers [Phone.FOCUS]; an older one has no use for either.
     */
    private fun report(machine: PairedMachine, phone: PhoneLink, lane: ReportLane) {
        val (level, charging) = Ringer.battery(app)
        val network = PhoneStatus.network(app)
        val focus = lane.remote.has(Phone.FOCUS)
        val dnd = if (focus) PhoneStatus.dnd(app) else null
        val now = System.currentTimeMillis()
        val can = buildList {
            if (ringer.canRing) add(Phone.CAN_RING)
            if (receiver.canReceive) add(Phone.CAN_RECEIVE)
            val granted = MirrorService.granted(app)
            if (granted) add(Phone.CAN_DISMISS)
            if (granted && mirrorReply && !lane.companion) add(Phone.CAN_REPLY)
            // The listener is what applies a computer's Do Not Disturb.
            if (focus && granted && focusOn) add(Phone.CAN_FOCUS)
            // Reading runs over a companion link too; whether a send may run
            // is decided when one arrives, by the link it arrives on.
            if (lane.remote.has(Messages.FEATURE) && Messages.offered(messagesOn, Sms.canRead(app))) add(Phone.CAN_MESSAGES)
            // Never "calls_dial" on a companion link: a dial waits for an unlock.
            if (lane.remote.has(Calls.FEATURE)) addAll(Calls.can(PhoneCalls.grants(app, callsOn), lane.companion))
            // Never over a companion link and never while locked: a photo
            // leaving the phone is the person's to allow each time.
            if (lane.remote.has(Photos.FEATURE) && Photos.offered(photosOn, PhotoStore.access(app), lane.companion, _unlocked.value)) {
                add(Phone.CAN_PHOTOS)
            }
            // Only to a machine that sends its clipboard, and only over a full link on an
            // unlocked app: the switch promises the text lands "while this app is unlocked".
            if (lane.remote.has(Phone.CLIPBOARD) && clipboardOn && !lane.companion && _unlocked.value) add(Phone.CAN_CLIPBOARD)
        }
        if (!lane.throttle.due(level, charging, now, network, dnd, can)) return
        runCatching { phone.report(level, charging, can, network, dnd) }
            .onSuccess { lane.throttle.sent(level, charging, now, network, dnd, can) }
            .onFailure { Log.i(TAG, "${machine.machine}: report not taken: ${it.message}") }
    }

    /**
     * Something a report carries may have changed: the battery, the network
     * or its signal, Do Not Disturb. Each connected machine's sender looks,
     * and sends only what [ReportThrottle] finds worth it.
     */
    fun onPhoneChanged() {
        if (!live) return
        for (lane in reportLanes.values) lane.wake.trySend(Unit)
    }

    /** Something `can` says changed: every connected machine is told at once. */
    private fun reportAgain() {
        if (!live) return
        for (lane in reportLanes.values) {
            lane.throttle.reset()
            lane.wake.trySend(Unit)
        }
    }

    /**
     * Whether a notification could go anywhere now: unlocked, and at least one
     * machine connected that takes them. The listener asks this before it
     * reads anything off a notification.
     */
    val mirroring: Boolean get() = live && mirrorLanes.values.any { it.sender?.isActive == true }

    fun mirrorApps(): Set<String> = mirrorApps

    /** The person changed which apps are shown; the store was already written. */
    fun setMirrorApps(apps: Set<String>) {
        mirrorApps = apps
    }

    fun mirrorReply(): Boolean = mirrorReply

    /**
     * The person turned replying from a computer on or off; the store was
     * already written. Each connected machine is told at once, through a
     * fresh report: its `can` is what lets it send a reply at all.
     */
    fun setMirrorReply(on: Boolean) {
        mirrorReply = on
        reportAgain()
    }

    fun focusOn(): Boolean = focusOn

    fun messagesOn(): Boolean = messagesOn

    /**
     * The person turned "Messages on your computer" on or off; the store was
     * already written. Each connected machine is told at once, as for
     * [setMirrorReply]: its `can` is what lets it ask at all, and off
     * refuses every request from here on whatever it was told before.
     */
    fun setMessagesOn(on: Boolean) {
        messagesOn = on
        syncSmsWatcher()
        reportAgain()
    }

    /** The person granted, or Android took back, reading or sending SMS: every machine hears the new `can`. */
    fun onSmsPermissionChanged() {
        syncSmsWatcher()
        reportAgain()
    }

    /** Watch the SMS store exactly while a change could be worth telling a machine. */
    private fun syncSmsWatcher() {
        smsWatcher.sync(messagesOn && Sms.canRead(app))
    }

    /**
     * The SMS store changed: every machine connected now that takes
     * messages, and was told it may ask, hears `messages_changed` and may
     * ask again. Nothing about the change goes with it.
     */
    private fun onSmsChanged() {
        if (!live || !messagesOn || !Sms.canRead(app)) return
        val phones = reportLanes.values.filter { it.remote.has(Messages.FEATURE) && it.sender?.isActive == true }.mapNotNull { it.phone }
        if (phones.isEmpty()) return
        scope.launch {
            for (phone in phones) runCatching { phone.messages(Messages.changed()) }
        }
    }

    fun callsOn(): Boolean = callsOn

    /**
     * The person turned "Calls on your computer" on or off; the store was
     * already written. Each connected machine is told at once, as for
     * [setMessagesOn]: off refuses every action from here on, whatever an
     * older report told the machine, and its ringing call closes there.
     */
    fun setCallsOn(on: Boolean) {
        callsOn = on
        syncCallWatcher()
        reportAgain()
    }

    /** The person granted, or Android took back, a calls permission: every machine hears the new `can`. */
    fun onCallsPermissionChanged() {
        syncCallWatcher()
        reportAgain()
    }

    fun photosOn(): Boolean = photosOn

    /**
     * The person turned "Photos on your computer" on or off; the store was
     * already written. Each connected machine is told at once, as for
     * [setMessagesOn]: its `can` is what lets it ask at all, and off
     * refuses every request from here on whatever it was told before.
     */
    fun setPhotosOn(on: Boolean) {
        photosOn = on
        reportAgain()
    }

    /**
     * The person allowed photos, picked others, or Android took access
     * back: every machine hears the new `can`, and an answer from here on
     * says whether only the picked photos are shown.
     */
    fun onPhotosPermissionChanged() {
        reportAgain()
    }

    /** Follow the phone's calls exactly while a machine may be told of them. */
    private fun syncCallWatcher() {
        callWatcher.sync(callsOn && PhoneCalls.canState(app))
    }

    /** The phone's call changed: each connected machine's sender tells it, after its report. */
    private fun onCallChanged() {
        if (!live) return
        for (lane in reportLanes.values) lane.wake.trySend(Unit)
    }

    /**
     * Tell this connection the phone's call, if it changed since it was last
     * told and it may know. A machine whose report no longer says "calls" has
     * already let go of what it was told; it hears nothing more.
     */
    private fun tellCall(machine: PairedMachine, phone: PhoneLink, lane: ReportLane) {
        if (!lane.remote.has(Calls.FEATURE)) return
        val offered = live && Calls.can(PhoneCalls.grants(app, callsOn), lane.companion).isNotEmpty()
        if (!offered) {
            lane.toldCall = Calls.Now.IDLE_NOW
            return
        }
        val now = callWatcher.current
        val told = lane.toldCall
        if (now == told || (now.state == Calls.IDLE && told.state == Calls.IDLE)) return
        // Never a number in a log: who calls the person is theirs.
        runCatching { phone.calls(Calls.stateLine(now)) }
            .onSuccess { lane.toldCall = now }
            .onFailure { Log.i(TAG, "${machine.machine}: the call was not shown: ${it.message}") }
    }

    /**
     * A computer presses one of the phone's buttons (§2.12). Answering,
     * declining and ending run on a locked phone, as on its lock screen;
     * dialling only over a full link on an unlocked app, because a dial is
     * the phone speaking for the person. The computer refuses a companion
     * link's dial before it asks; this side does not rely on that.
     */
    private fun onCallAction(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.CallAction, companion: Boolean) {
        val unlocked = !companion && _unlocked.value
        val state = Calls.stateOf(PhoneCalls.telephonyState(app))
        val dial = if (event.action == Calls.DIAL) Calls.dialNumber(event.number) { PhoneCalls.emergency(app, it) } else null
        val word = Calls.refusal(PhoneCalls.grants(app, callsOn), unlocked, event.action, state)
            ?: (if (!live) Calls.LOCKED else null)
            ?: dial?.exceptionOrNull()?.let { (it as Messages.Refused).word }
        val result = word ?: PhoneCalls.act(app, event.action, dial?.getOrNull())
        Log.i(TAG, "${machine.machine}: ${event.action} from the computer: ${result ?: "done"}")
        runCatching { phone.calls(Calls.done(event.id, result)) }
            .onFailure { Log.i(TAG, "${machine.machine}: a call action's result was not taken: ${it.message}") }
    }

    /** A computer asks for the call history (§2.12); it runs over a companion link too. */
    private fun onCallHistory(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.CallHistoryRequest) {
        val word = Calls.historyRefusal(PhoneCalls.grants(app, callsOn), live)
        val lines = if (word != null) {
            listOf(Calls.refused(event.id, word))
        } else {
            runCatching { Calls.historyReply(event.id, PhoneCalls.history(app, event.before, event.limit)) }.getOrElse {
                Log.i(TAG, "${machine.machine}: the call history was not read: ${it.javaClass.simpleName}")
                listOf(Calls.refused(event.id, Calls.FAILED))
            }
        }
        for (line in lines) {
            val ok = runCatching { phone.calls(line) }
                .onFailure { Log.i(TAG, "${machine.machine}: a history answer was not taken: ${it.message}") }
                .isSuccess
            if (!ok) return
        }
    }

    /**
     * A computer asks for this phone's conversations, or one of them
     * (§2.11). The switch and the permission are checked here, whatever an
     * older report told the machine; reading runs over a companion link as
     * the other device features do.
     */
    private fun onMessagesRequest(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.MessagesRequest) {
        val word = Messages.refusal(messagesOn, Sms.canRead(app), canSend = false, unlocked = false, send = false)
            ?: if (!live) Messages.LOCKED else null
        val lines = if (word != null) {
            listOf(Messages.refused(event.id, word))
        } else {
            runCatching {
                val thread = event.thread
                if (thread == null) {
                    Messages.threadsReply(event.id, Sms.threads(app, event.before, event.limit))
                } else {
                    Sms.thread(app, thread, event.before, event.limit)
                        ?.let { Messages.messagesReply(event.id, thread, it) }
                        ?: listOf(Messages.refused(event.id, Messages.NO_THREAD))
                }
            }.getOrElse {
                Log.i(TAG, "${machine.machine}: messages not read: ${it.javaClass.simpleName}")
                listOf(Messages.refused(event.id, Messages.FAILED))
            }
        }
        for (line in lines) {
            val ok = runCatching { phone.messages(line) }
                .onFailure { Log.i(TAG, "${machine.machine}: a messages answer was not taken: ${it.message}") }
                .isSuccess
            if (!ok) return
        }
    }

    /**
     * A computer asks this phone to send an SMS (§2.11). Only over a full
     * link on an unlocked app: a send is the phone speaking for the person.
     * The computer refuses a locked phone's send before it asks; this side
     * does not rely on that.
     */
    private suspend fun onMessagesSend(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.MessagesSend, companion: Boolean) {
        val unlocked = !companion && _unlocked.value
        val word = Messages.refusal(messagesOn, Sms.canRead(app), Sms.canSend(app), unlocked, send = true)
            ?: Messages.text(event.text).exceptionOrNull()?.let { (it as Messages.Refused).word }
        val result = if (word != null) {
            word
        } else {
            val to = Messages.destination(event.to, event.thread) { runCatching { Sms.addresses(app, it) }.getOrNull() }
            to.fold(
                onSuccess = { number -> smsSender.send(number, Messages.text(event.text).getOrThrow()) },
                onFailure = { (it as Messages.Refused).word },
            )
        }
        Log.i(TAG, "${machine.machine}: a send from the computer: ${result ?: "sent"}")
        runCatching { phone.messages(Messages.sent(event.id, result)) }
            .onFailure { Log.i(TAG, "${machine.machine}: a send's result was not taken: ${it.message}") }
    }

    /** Why a photo request must be refused now, or null when it may run (§2.13). */
    private fun photosRefusal(companion: Boolean): String? =
        Photos.refusal(photosOn, PhotoStore.access(app), companion, _unlocked.value)

    /**
     * A computer asks for a page of this phone's photos (§2.13). The switch,
     * the access, the link and the unlock are all checked here, whatever an
     * older report told the machine. Nothing is logged about any photo.
     */
    private fun onPhotosRequest(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.PhotosRequest, companion: Boolean) {
        val word = photosRefusal(companion)
        if (word != null) return sendPhotos(machine, phone, listOf(Photos.refused(event.id, word)))
        if (!photosListing.add(phone)) return sendPhotos(machine, phone, listOf(Photos.refused(event.id, Photos.BUSY)))
        try {
            val partial = PhotoStore.access(app) == Photos.Access.PARTIAL
            val lines = runCatching { Photos.reply(event.id, PhotoStore.listing(app, event.before, event.limit), partial) }.getOrElse {
                Log.i(TAG, "${machine.machine}: photos not read: ${it.javaClass.simpleName}")
                listOf(Photos.refused(event.id, Photos.FAILED))
            }
            sendPhotos(machine, phone, lines)
        } finally {
            photosListing.remove(phone)
        }
    }

    /** Send an answer's lines in order, stopping at the first the link does not take. */
    private fun sendPhotos(machine: PairedMachine, phone: PhoneLink, lines: List<String>) {
        for (line in lines) {
            // A companion link refuses the line itself; that is not worth more than a log.
            val ok = runCatching { phone.photos(line) }
                .onFailure { Log.i(TAG, "${machine.machine}: a photos answer was not taken: ${it.message}") }
                .isSuccess
            if (!ok) return
        }
    }

    /**
     * A computer asks for one photo's original (§2.13). It goes on a
     * connection of its own, as a file the person sends does, so a large
     * photo never holds up the device channel; a refusal goes back on the
     * device channel. The photo is looked up again here: a listing the
     * computer had is no promise it is still there, or still allowed.
     */
    private fun onPhotosFetch(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent.PhotosFetch, companion: Boolean) {
        fun refuse(word: String) {
            Log.i(TAG, "${machine.machine}: a photo was not sent: $word")
            runCatching { phone.photos(Photos.fetchRefused(event.id, word)) }
                .onFailure { Log.i(TAG, "${machine.machine}: a photo refusal was not taken: ${it.message}") }
        }
        photosRefusal(companion)?.let { return refuse(it) }
        if (!photosSending.add(phone)) return refuse(Photos.BUSY)
        try {
            val original = runCatching { PhotoStore.original(app, event.photo) }
                .getOrElse { return refuse(Photos.FAILED) }
                ?: return refuse(Photos.NOT_FOUND)
            if (original.len > Photos.MAX_PHOTO_BYTES) return refuse(Photos.TOO_LARGE)
            if (original.len <= 0) return refuse(Photos.FAILED)
            // Once more just before dialling: the new connection does not
            // know it was asked for over a companion link, and the person
            // may have locked the app or turned the switch off meanwhile.
            photosRefusal(companion)?.let { return refuse(it) }
            runCatching {
                Upload.photo(
                    connect = { dialSession(machine) },
                    // The fetch's own id, which is what the computer waits on; not the photo's.
                    id = event.id,
                    name = original.name,
                    len = original.len,
                    source = { PhotoStore.open(app, original) },
                )
            }.onFailure {
                Log.i(TAG, "${machine.machine}: a photo did not cross: ${it.javaClass.simpleName}")
                refuse(Photos.FAILED)
            }
        } finally {
            photosSending.remove(phone)
        }
    }

    /**
     * The person turned "Do Not Disturb follows your computer" on or off;
     * the store was already written. Each connected machine is told at once,
     * as for [setMirrorReply]: its `can` is what lets it follow this phone
     * and send it a `focus` line at all.
     */
    fun setFocusOn(on: Boolean) {
        focusOn = on
        if (!on) releaseOwnDnd("switched off")
        reportAgain()
    }

    fun clipboardOn(): Boolean = clipboardOn

    /**
     * The person turned "Get the computer's clipboard" on or off; the store
     * was already written. Each connected machine is told at once, as for
     * [setMirrorReply]: its `can` is what makes it send a `clipboard` line.
     */
    fun setClipboardOn(on: Boolean) {
        clipboardOn = on
        reportAgain()
    }

    /**
     * Android says the phone's Do Not Disturb changed, by any path. Off means
     * nothing this app turned on is left to take back; then every machine
     * hears the new state.
     */
    fun onFocusChanged() {
        ownDnd.observed(PhoneStatus.dnd(app))
        onPhoneChanged()
    }

    /**
     * Take back a Do Not Disturb this app turned on for a computer, and only
     * that: never one the person turned on at the phone. It needs the
     * listener; without notification access it stays on, and is taken back
     * when access returns ([onListenerChanged]) or ended at the phone.
     */
    private fun releaseOwnDnd(why: String) {
        if (!ownDnd.owed(PhoneStatus.dnd(app))) return
        if (MirrorService.focus(false)) ownDnd.applied(false)
        else Log.i(TAG, "Do Not Disturb this app turned on stays on ($why): no listener")
    }

    /**
     * A notification from a chosen app was posted or updated: every machine
     * connected now gets its newest state, at its own pace. Called on the
     * listener's thread, in the order Android reports, which is the order
     * each outbox keeps.
     */
    fun onPhoneNotification(key: String, content: Content) {
        val now = System.currentTimeMillis()
        toLanes { lane -> lane.outbox.offer(key, if (lane.reply) content else content.copy(reply = null), now) }
    }

    /** A notification went away on this phone; a machine that was sent it closes it too. */
    fun onPhoneNotificationRemoved(key: String) = toLanes { it.outbox.remove(key) }

    private fun toLanes(block: (MirrorLane) -> Unit) {
        if (!live) return
        for (lane in mirrorLanes.values) {
            if (lane.sender?.isActive != true) continue
            synchronized(lane.outbox) { block(lane) }
            lane.wake.trySend(Unit)
        }
    }

    /**
     * The sender for one machine's new connection. The outbox outlives the
     * connection (so a removal after a reconnect still closes what was
     * shown); what waited when the old one dropped was already discarded.
     */
    private fun startMirror(machine: PairedMachine, phone: PhoneLink, reply: Boolean) {
        val lane = mirrorLanes.compute(machine.deviceId) { _, old ->
            old?.sender?.cancel()
            MirrorLane(old?.outbox ?: MirrorOutbox(System.currentTimeMillis()), Channel(Channel.CONFLATED), reply = reply)
        }!!
        lane.sender = scope.launch {
            val outbox = lane.outbox
            while (isActive) {
                lane.wake.receive()
                while (isActive) {
                    val now = System.currentTimeMillis()
                    val item = synchronized(outbox) { outbox.next(now) }
                    if (item == null) {
                        val wait = synchronized(outbox) { outbox.waitMs(now) }
                        if (wait == 0L) break
                        delay(wait)
                        continue
                    }
                    // Never the content in a log: it is somebody's message.
                    val ok = runCatching { phone.mirror(item) }
                        .onFailure { Log.i(TAG, "${machine.machine}: a notification was not shown: ${it.message}") }
                        .isSuccess
                    synchronized(outbox) { if (ok) outbox.sent(item) else outbox.failed(item) }
                }
            }
        }
    }

    /** Whether the media watcher is running: something could be sent now. For the device suite. */
    val mediaWatching: Boolean get() = synchronized(this) { watching }

    /**
     * The person turned "Media on your computer" on or off; the store was
     * already written. Off takes back what every machine shows at once.
     */
    fun setMediaOn(on: Boolean) {
        mediaOn = on
        if (!on) onPhoneMedia(null)
        syncMediaWatcher()
    }

    /**
     * The person turned "Control from your computer" on or off; the store was
     * already written. Every machine is told again at once, with or without
     * `can`, so its buttons follow the switch.
     */
    fun setMediaControl(on: Boolean) {
        mediaControl = on
        onPhoneMedia(media)
    }

    /** The person changed which apps' playback may show; what is shown is worked out again now. */
    fun setMediaApps(apps: Set<String>) {
        mediaApps = apps
        synchronized(this) { if (watching) watcher.setAllowed(apps) }
    }

    /**
     * Notification access was granted or taken away (the listener bound or
     * unbound): media sessions are readable only with it, and `can` says
     * what it allows (dismiss, reply, focus), so every machine is told again.
     */
    fun onListenerChanged() {
        if (!focusOn || !live) releaseOwnDnd("access back")
        syncMediaWatcher()
        reportAgain()
    }

    /**
     * Run the watcher exactly while something could be sent: unlocked, the
     * switch on, notification access granted, and a machine connected that
     * takes `phone_media`. When it stops, every machine is told nothing
     * plays (a machine shown nothing is sent nothing).
     */
    private fun syncMediaWatcher() {
        val want = live && mediaOn &&
            mediaLanes.values.any { it.sender?.isActive == true } &&
            runCatching { MirrorService.granted(app) }.getOrDefault(false)
        val stopped = synchronized(this) {
            if (want) {
                watcher.start(mediaApps)
                watching = true
                false
            } else {
                val was = watching
                if (was) watcher.stop()
                watching = false
                was
            }
        }
        if (stopped) onPhoneMedia(null)
    }

    /**
     * What is playing changed (or the watcher stopped: null). Every machine
     * connected now gets it at its own pace; called on the watcher's thread.
     * Locked or switched off, it is always nothing.
     */
    private fun onPhoneMedia(state: MediaState?) {
        val s = if (live && mediaOn) state else null
        media = s
        for (lane in mediaLanes.values) {
            if (lane.sender?.isActive != true) continue
            synchronized(lane.outbox) { lane.outbox.offer(forLane(s, lane.control)) }
            lane.wake.trySend(Unit)
        }
    }

    /** [s] as one machine is shown it: with `can` only if it takes it and the switch is on. */
    private fun forLane(s: MediaState?, control: Boolean): MediaState? =
        if (s == null || (control && mediaControl)) s else s.copy(can = emptySet())

    /**
     * [machine] asks for a control on what it shows (§2.8). Only while
     * unlocked, with both switches on, on a connection that offered control,
     * and only to the session this connection last showed it; the watcher
     * then checks that session still allows it. Anything else is ignored.
     */
    private fun onMediaControl(machine: PairedMachine, event: DeviceEvent.MediaControl) {
        if (!_unlocked.value || !mediaOn || !mediaControl) return
        val lane = mediaLanes[machine.deviceId]?.takeIf { it.control && it.sender?.isActive == true } ?: return
        val shown = synchronized(lane.outbox) { lane.outbox.showing } ?: return
        if (shown.can.isEmpty()) return
        synchronized(this) { if (watching) watcher.control(shown, event.action, event.positionMs) }
    }

    /**
     * The sender for one machine's new connection: it is told what plays
     * now at once, then each change, at most every [Media.GAP_MS].
     */
    private fun startMedia(machine: PairedMachine, phone: PhoneLink, control: Boolean) {
        val lane = mediaLanes.compute(machine.deviceId) { _, old ->
            old?.sender?.cancel()
            MediaLane(MediaOutbox(), Channel(Channel.CONFLATED), control)
        }!!
        synchronized(lane.outbox) { lane.outbox.offer(forLane(if (live && mediaOn) media else null, control)) }
        lane.sender = scope.launch {
            val outbox = lane.outbox
            while (isActive) {
                lane.wake.receive()
                while (isActive) {
                    val now = SystemClock.elapsedRealtime()
                    val report = synchronized(outbox) { outbox.next(now) }
                    if (report == null) {
                        val wait = synchronized(outbox) { outbox.waitMs(now) }
                        if (wait == 0L) break
                        delay(wait)
                        continue
                    }
                    // The position as of the moment the line is written.
                    val at = SystemClock.elapsedRealtime()
                    // Never the title in a log: what somebody listens to is theirs.
                    val ok = runCatching { phone.media(Media.encode(report.state, at)) }
                        .onFailure { Log.i(TAG, "${machine.machine}: what is playing was not shown: ${it.message}") }
                        .isSuccess
                    synchronized(outbox) { if (ok) outbox.sent(report, at) else outbox.failed(report, at) }
                }
            }
        }
        lane.wake.trySend(Unit)
        syncMediaWatcher()
    }

    private fun onDeviceEvent(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent, companion: Boolean) {
        when (event) {
            is DeviceEvent.Ring -> scope.launch {
                // A repeat of a ring already rung is acknowledged again, not
                // rung again: the machine only learns it worked from the ack.
                val rang = if (Ringer.rings.first(machine.deviceId, event.at)) ringer.ring(machine.machine) else true
                if (rang) runCatching { phone.ack(event.id) }
            }
            is DeviceEvent.Offer -> scope.launch { onOffer(machine, phone, event) }
            is DeviceEvent.MessagesRequest -> scope.launch { onMessagesRequest(machine, phone, event) }
            is DeviceEvent.MessagesSend -> scope.launch { onMessagesSend(machine, phone, event, companion) }
            is DeviceEvent.CallAction -> scope.launch { onCallAction(machine, phone, event, companion) }
            is DeviceEvent.CallHistoryRequest -> scope.launch { onCallHistory(machine, phone, event) }
            is DeviceEvent.PhotosRequest -> scope.launch { onPhotosRequest(machine, phone, event, companion) }
            is DeviceEvent.PhotosFetch -> scope.launch { onPhotosFetch(machine, phone, event, companion) }
            // Never over a companion link: it acts for the person.
            is DeviceEvent.MediaControl -> if (!companion) onMediaControl(machine, event)
            is DeviceEvent.Focus -> {
                // Only with the person's switch on, whatever an older report
                // told the machine, and only when the phone is not there
                // already. The change Android then reports goes back to
                // every machine as `dnd`, which is how the others follow.
                val switchOn = focusOn && MirrorService.granted(app)
                if (!Focus.apply(switchOn, live, PhoneStatus.dnd(app), event.dnd)) return
                if (MirrorService.focus(event.dnd)) ownDnd.applied(event.dnd)
                else Log.i(TAG, "${machine.machine}: Do Not Disturb not changed: no listener")
            }
            is DeviceEvent.Clipboard -> {
                // Only with the person's switch on and the app unlocked,
                // whatever an older report told the machine. The text is
                // never logged: it may be a password.
                if (!clipboardOn || !_unlocked.value) return
                if (!ComputerClipboard.put(app, machine.machine, event)) {
                    Log.i(TAG, "${machine.machine}: the computer's clipboard was not put on this phone's")
                }
            }
            is DeviceEvent.Dismissed -> {
                // Only what this machine was sent, and only while the device
                // features run (unlocked, or companion mode): a locked app
                // with the switch off sends nothing and takes nothing back.
                val lane = mirrorLanes[machine.deviceId] ?: return
                if (!live) return
                if (synchronized(lane.outbox) { lane.outbox.dismissedThere(event.key) }) MirrorService.dismiss(event.key)
            }
            is DeviceEvent.Replied -> {
                // As for a dismissal, and only while the person has replying
                // on: a report the machine had before they turned it off
                // does not let it speak for them. The outbox is told first,
                // so the app's own update after the reply finds it ready.
                val lane = mirrorLanes[machine.deviceId] ?: return
                // Never over a companion link, whatever it sends: a reply is
                // the phone speaking for the person, and that needs an unlock.
                if (companion || !_unlocked.value || !mirrorReply) return
                val now = System.currentTimeMillis()
                if (synchronized(lane.outbox) { lane.outbox.repliedThere(event.key, now) }) {
                    if (!MirrorService.reply(event.key, event.text)) {
                        synchronized(lane.outbox) { lane.outbox.replyFailed(event.key) }
                        Log.i(TAG, "${machine.machine}: a reply found nothing to answer")
                    }
                }
            }
        }
    }

    /**
     * Something the computer sent (contract §2.6): say it arrived, take it,
     * say it is done. An offer already taken (a replay after a lost
     * `offer_done`) is only said done again. One whose take failed on the
     * link is left for the next replay; one refused for good is done.
     */
    private fun onOffer(machine: PairedMachine, phone: PhoneLink, offer: DeviceEvent.Offer) {
        val id = machine.deviceId
        runCatching { phone.offerAck(offer.id) }
        receiver.clearWaiting()
        if (Receiver.offers.wasTaken(id, offer.id, offer.at)) {
            runCatching { phone.offerDone(offer.id) }
            return
        }
        if (!Receiver.offers.claim(id, offer.id, offer.at)) return
        val finished = try {
            receiver.receive(machine.machine, phone, offer)
            true
        } catch (e: Receiver.Refused) {
            Log.i(TAG, "${machine.machine}: offer ${offer.id} refused: ${e.message}")
            true
        } catch (e: Throwable) {
            Log.w(TAG, "${machine.machine}: offer ${offer.id} not taken: ${e.message}")
            false
        }
        Receiver.offers.finish(id, offer.id, offer.at, finished)
        if (finished) runCatching { phone.offerDone(offer.id) }
    }

    private fun scheduleReconnect(machine: PairedMachine) {
        val id = machine.deviceId
        if (reconnects[id]?.isActive == true) return
        val n = attemptsSinceUp.merge(id, 1) { a, b -> a + b } ?: 1
        val wait = BACKOFF_MS[(n - 1).coerceAtMost(BACKOFF_MS.size - 1)]
        reconnects[id] = scope.launch {
            delay(wait)
            reconnects.remove(id)
            if (!wanted(id)) return@launch
            val current = machineFor(id) ?: machine
            runCatching { connectNow(current) }
        }
    }

    private fun setState(deviceId: String, name: String, block: (LinkState) -> LinkState) {
        _states.update { all -> all + (deviceId to block(all[deviceId] ?: LinkState(name))) }
    }

    companion object {
        private const val TAG = "RimeRemote"

        /**
         * How long a held link waits before each reconnect attempt.
         *
         * Short, because the point of holding a connection is that it is there
         * when a person looks: a quarter of a second first (most drops are a
         * network hand-over that is already over), then doubling to fifteen
         * seconds and staying there. A network change skips the wait
         * entirely, so the long end is only ever paid somewhere with no
         * network at all.
         */
        val BACKOFF_MS: List<Long> = listOf(250, 1_000, 2_000, 4_000, 8_000, 15_000)

        @Volatile
        private var instance: LinkHub? = null

        fun get(context: Context): LinkHub =
            instance ?: synchronized(this) {
                instance ?: LinkHub(context.applicationContext).also { instance = it }
            }
    }
}
