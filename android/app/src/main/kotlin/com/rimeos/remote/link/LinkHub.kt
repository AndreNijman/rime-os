package com.rimeos.remote.link

import android.content.Context
import android.util.Log
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.Session
import com.rimeos.remote.core.StaticKey
import com.rimeos.remote.core.agent.Hello
import com.rimeos.remote.core.agent.MachineLink
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.agent.RemoteHello
import com.rimeos.remote.core.phone.BatteryThrottle
import com.rimeos.remote.core.phone.Content
import com.rimeos.remote.core.phone.DeviceEvent
import com.rimeos.remote.core.phone.Mirror
import com.rimeos.remote.core.phone.MirrorOutbox
import com.rimeos.remote.core.phone.Phone
import com.rimeos.remote.core.phone.PhoneLink
import com.rimeos.remote.phone.MirrorService
import com.rimeos.remote.phone.Receiver
import com.rimeos.remote.phone.Ringer
import com.rimeos.remote.core.link.Route
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
 */
class LinkHub private constructor(private val app: Context) {
    private val repository = MachineRepository(app)
    private val pairing = PairingService()
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

    /** The reconnect waiting to run for each machine, if any. */
    private val reconnects = ConcurrentHashMap<String, Job>()
    private val attemptsSinceUp = ConcurrentHashMap<String, Int>()

    /** How each link's current connection was made, set by the connect lambda. */
    private val lastDial = ConcurrentHashMap<String, Dial>()

    // The phone's own side of each connection (contract §2.5): its battery
    // report, and the event channel a machine rings it on. One of each per
    // machine; a new connection replaces both.
    private val batteryThrottles = ConcurrentHashMap<String, BatteryThrottle>()
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

    fun markUnlocked() {
        _unlocked.value = true
    }

    fun identity(deviceId: String): StaticKey? = identities[deviceId]

    fun remember(deviceId: String, identity: StaticKey) {
        identities[deviceId] = identity
    }

    fun state(deviceId: String): LinkState? = _states.value[deviceId]

    /** The link to [machine], made on first use. Opens nothing by itself. */
    fun link(machine: PairedMachine): MachineLink = links.computeIfAbsent(machine.deviceId) { id ->
        // `computeIfAbsent`, not Kotlin's `getOrPut`: that one is a read and
        // then a write, and `hold` on the IO dispatcher and a tap on the main
        // thread could each make a link — two connections to one machine,
        // one of them orphaned.
        MachineLink(
            connect = {
                val identity = identities[id]
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
            onEvent = { event -> onLinkEvent(machine, event) },
        )
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
        val link = link(machine)
        try {
            link.connectNow()
        } catch (e: Exception) {
            setState(machine.deviceId, machine.machine) {
                it.copy(
                    status = if (machine.deviceId in held) Status.RECONNECTING else Status.IDLE,
                    failure = e.message ?: e::class.java.simpleName,
                )
            }
            if (machine.deviceId in held) scheduleReconnect(machine)
            throw e
        }
    }

    /**
     * The phone's network changed. Every held link is asked whether it
     * survived — a ping and a few seconds, not the forty-second watchdog —
     * and anything waiting to reconnect goes now instead of after its
     * backoff.
     */
    fun onNetworkChanged() {
        if (!_unlocked.value) return
        for (id in held.toList()) {
            // The store read inside the launch too: this is called on the
            // ConnectivityManager's binder thread, which is not the place for
            // a disk read either.
            scope.launch {
                val machine = machineFor(id) ?: return@launch
                val link = links[id]
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
        batteryThrottles.clear()
        eventChannels.clear()
        mirrorLanes.values.forEach { it.sender?.cancel() }
        mirrorLanes.clear()
        _states.value = emptyMap()
        LinkService.stop(app)
        scope.launch { closing.forEach { runCatching { it.close() } } }
    }

    /** Forget one machine: its link, its key, and holding it. */
    fun forget(deviceId: String) {
        held.remove(deviceId)
        reconnects.remove(deviceId)?.cancel()
        identities.remove(deviceId)
        lastDial.remove(deviceId)
        batteryThrottles.remove(deviceId)
        eventChannels.remove(deviceId)?.let { runCatching { it.close() } }
        mirrorLanes.remove(deviceId)?.sender?.cancel()
        _states.update { it - deviceId }
        val link = links.remove(deviceId)
        if (held.isEmpty()) LinkService.stop(app)
        scope.launch { runCatching { link?.close() } }
    }

    /** The machines currently held, with their names, for the notification. */
    fun heldNames(): List<String> = held.mapNotNull { _states.value[it]?.machine }

    private fun machineFor(deviceId: String): PairedMachine? =
        runCatching { repository.loadBlocking().find(deviceId) }.getOrNull()

    private fun onLinkEvent(machine: PairedMachine, event: MachineLink.LinkEvent) {
        val id = machine.deviceId
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
                scope.launch { afterConnect(machine, dial) }
            }
            is MachineLink.LinkEvent.Silent ->
                Log.i(TAG, "${machine.machine}: nothing arrived for ${event.forMs} ms, reconnecting")
            is MachineLink.LinkEvent.Dropped -> {
                val keep = id in held && _unlocked.value
                setState(id, machine.machine) {
                    it.copy(status = if (keep) Status.RECONNECTING else Status.IDLE, via = null, connectMs = null)
                }
                mirrorLanes[id]?.let { lane ->
                    lane.sender?.cancel()
                    lane.sender = null
                    synchronized(lane.outbox) { lane.outbox.drop() }
                }
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
    private suspend fun afterConnect(machine: PairedMachine, dial: Dial?) {
        val link = links[machine.deviceId] ?: return
        val remote = scope.async { runCatching { link.remoteHello() }.getOrNull() }
        val hello = scope.async { runCatching { link.hello() }.getOrNull() }
        val r = remote.await()
        val h = hello.await()
        setState(machine.deviceId, machine.machine) {
            it.copy(hello = h ?: it.hello, remote = r ?: it.remote, hellos = it.hellos + 1)
        }
        r?.let { phoneSide(machine, link, it) }
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
        // Before the report, which says whether replies are taken.
        runCatching {
            val settings = repository.loadBlocking().settings
            mirrorApps = settings.mirrorApps
            mirrorReply = settings.mirrorReply
        }
        if (remote.has(Phone.REPORT)) {
            val throttle = batteryThrottles.computeIfAbsent(id) { BatteryThrottle() }
            throttle.reset()
            report(phone, throttle, Ringer.battery(app))
        }
        if (remote.has(Mirror.FEATURE)) {
            startMirror(machine, phone, remote.has(Mirror.REPLY_FEATURE))
        } else {
            mirrorLanes.remove(id)?.sender?.cancel()
        }
        eventChannels.remove(id)?.let { runCatching { it.close() } }
        if (remote.has(Phone.EVENTS) && remote.has(Features.MUX_ATTACH)) {
            runCatching {
                phone.subscribe(
                    listener = { event -> onDeviceEvent(machine, phone, event) },
                    onEnd = { cause -> Log.i(TAG, "${machine.machine}: event channel ended: ${cause?.message}") },
                )
            }.onSuccess { eventChannels[id] = it }
                .onFailure { Log.w(TAG, "${machine.machine}: event channel did not open: ${it.message}") }
        }
    }

    private fun report(phone: PhoneLink, throttle: BatteryThrottle, reading: Pair<Int?, Boolean?>) {
        val (level, charging) = reading
        val now = System.currentTimeMillis()
        if (!throttle.due(level, charging, now)) return
        val can = buildList {
            if (ringer.canRing) add(Phone.CAN_RING)
            if (receiver.canReceive) add(Phone.CAN_RECEIVE)
            val granted = MirrorService.granted(app)
            if (granted) add(Phone.CAN_DISMISS)
            if (granted && mirrorReply) add(Phone.CAN_REPLY)
        }
        runCatching { phone.report(level, charging, can) }
            .onSuccess { throttle.sent(level, charging, now) }
            .onFailure { Log.i(TAG, "battery report not taken: ${it.message}") }
    }

    /** The phone's battery changed; tell each connected machine that wants to know, when it is worth it. */
    fun onBattery(level: Int?, charging: Boolean?) {
        if (!_unlocked.value) return
        for ((id, throttle) in batteryThrottles) {
            val link = links[id] ?: continue
            if (!link.connected) continue
            if (state(id)?.remote?.has(Phone.REPORT) != true) continue
            if (!throttle.due(level, charging, System.currentTimeMillis())) continue
            scope.launch { report(PhoneLink(link), throttle, level to charging) }
        }
    }

    /**
     * Whether a notification could go anywhere now: unlocked, and at least one
     * machine connected that takes them. The listener asks this before it
     * reads anything off a notification.
     */
    val mirroring: Boolean get() = _unlocked.value && mirrorLanes.values.any { it.sender?.isActive == true }

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
        if (!_unlocked.value) return
        for ((id, throttle) in batteryThrottles) {
            val link = links[id] ?: continue
            if (!link.connected) continue
            if (state(id)?.remote?.has(Phone.REPORT) != true) continue
            throttle.reset()
            scope.launch { report(PhoneLink(link), throttle, Ringer.battery(app)) }
        }
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
        if (!_unlocked.value) return
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

    private fun onDeviceEvent(machine: PairedMachine, phone: PhoneLink, event: DeviceEvent) {
        when (event) {
            is DeviceEvent.Ring -> scope.launch {
                // A repeat of a ring already rung is acknowledged again, not
                // rung again: the machine only learns it worked from the ack.
                val rang = if (Ringer.rings.first(machine.deviceId, event.at)) ringer.ring(machine.machine) else true
                if (rang) runCatching { phone.ack(event.id) }
            }
            is DeviceEvent.Offer -> scope.launch { onOffer(machine, phone, event) }
            is DeviceEvent.Dismissed -> {
                // Only what this machine was sent, and only while unlocked:
                // a locked app sends nothing and takes nothing back.
                val lane = mirrorLanes[machine.deviceId] ?: return
                if (!_unlocked.value) return
                if (synchronized(lane.outbox) { lane.outbox.dismissedThere(event.key) }) MirrorService.dismiss(event.key)
            }
            is DeviceEvent.Replied -> {
                // As for a dismissal, and only while the person has replying
                // on: a report the machine had before they turned it off
                // does not let it speak for them. The outbox is told first,
                // so the app's own update after the reply finds it ready.
                val lane = mirrorLanes[machine.deviceId] ?: return
                if (!_unlocked.value || !mirrorReply) return
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
            if (id !in held || !_unlocked.value) return@launch
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
