package com.rimeos.remote.ui.touchpad

import android.app.Application
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.Choreographer
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.input.Gesture
import com.rimeos.remote.core.input.Input
import com.rimeos.remote.core.input.Mod
import com.rimeos.remote.core.input.MouseButton
import com.rimeos.remote.core.input.Notice
import com.rimeos.remote.core.input.Opened
import com.rimeos.remote.core.input.TouchpadFacts
import com.rimeos.remote.core.input.TouchpadFlow
import com.rimeos.remote.core.input.TouchpadInput
import com.rimeos.remote.core.input.TouchpadPhase
import com.rimeos.remote.core.input.TouchpadStep
import com.rimeos.remote.core.input.TypedText
import com.rimeos.remote.core.link.Mux
import com.rimeos.remote.link.LinkHub
import java.util.concurrent.Executors
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** What the touchpad screen draws. */
data class TouchpadUiState(
    val machine: String? = null,
    val phase: TouchpadPhase = TouchpadPhase.Away,
    /** Modifiers armed on the key strip for the next key or click. */
    val armed: Set<Mod> = emptySet(),
    /** Modifiers locked on (tapped twice). */
    val locked: Set<Mod> = emptySet(),
    /** When the computer last let go of everything by itself (its lease), for a short note. */
    val releasedAt: Long? = null,
)

/**
 * The phone as this computer's touchpad and keyboard (contract §2.16).
 *
 * One channel on the machine's shared connection, opened while the screen
 * is in front and closed when it leaves, after a frame that lets go of
 * everything held. Every decision about states and retries is
 * [TouchpadFlow]'s and every frame is [TouchpadInput]'s; this class is the
 * plumbing between them, the screen and the connection.
 *
 * Threads: everything here runs on the main thread except the network,
 * which runs on one thread of its own in order (open, the frames, close),
 * so a close never overtakes the frame that lets go. One frame is in flight
 * at a time; what happens meanwhile waits in the batcher and goes in the
 * next. Frames are sent at most once per display frame.
 *
 * Nothing typed is logged or kept: text goes from the field into a frame
 * and is gone.
 */
class TouchpadViewModel(application: Application) : AndroidViewModel(application) {
    private val hub = LinkHub.get(application)
    private val main = Handler(Looper.getMainLooper())
    private val net = Executors.newSingleThreadExecutor { r -> Thread(r, "touchpad-net").apply { isDaemon = true } }

    private val _state = MutableStateFlow(TouchpadUiState())
    val state: StateFlow<TouchpadUiState> = _state.asStateFlow()

    private var machine: PairedMachine? = null
    private var phoneName: String = "this phone"
    private val input = TouchpadInput()
    private var flow = TouchpadFlow(TouchpadFacts(false, null, null, phoneName))
    private var watching: Job? = null

    /** The open channel, or null. */
    private var channel: Pair<Mux, UInt>? = null

    /** Bumped for every open and every leave: a callback for an older one is ignored. */
    private var token = 0
    private var inFlight = false
    private var frameAsked = false
    private val lines = StringBuilder()

    /** Told on every click, for the haptic tick. Set by the screen. */
    var onClick: (() -> Unit)? = null

    private fun now(): Long = SystemClock.uptimeMillis()

    fun open(machine: PairedMachine, phoneName: String) {
        if (this.machine?.deviceId != machine.deviceId) {
            leave()
            this.machine = machine
            this.phoneName = phoneName
            flow = TouchpadFlow(facts())
            _state.value = TouchpadUiState(machine = machine.machine)
        }
    }

    fun configure(natural: Boolean, speed: Float) {
        input.natural = natural
        input.speed(speed.toDouble())
    }

    private fun facts(): TouchpadFacts {
        val m = machine ?: return TouchpadFacts(false, null, null, phoneName)
        val s = hub.state(m.deviceId)
        val connected = s?.status == LinkHub.Status.CONNECTED
        val known = s != null && s.hellos > 0
        return TouchpadFacts(
            connected = connected,
            supported = if (known) s.remote.has(Input.FEATURE) else null,
            inputOn = when (s?.remote?.input) {
                "on" -> true
                "off" -> false
                else -> null
            },
            phoneName = phoneName,
        )
    }

    /** The screen came to the front. */
    fun enter() {
        val m = machine ?: return
        // Connect if nothing has yet; the link's own reconnects do the rest.
        if (hub.state(m.deviceId)?.status != LinkHub.Status.CONNECTED) {
            net.execute { runCatching { hub.connectNow(m) } }
        }
        flow.facts(now(), facts())
        act(flow.enter(now()))
        watching?.cancel()
        watching = viewModelScope.launch {
            hub.states
                .map { facts() }
                .distinctUntilChanged()
                .collect { act(flow.facts(now(), it)) }
        }
    }

    /** The screen left: let go of everything on the computer, then close. */
    fun leave() {
        watching?.cancel()
        watching = null
        val letGo = input.leave(now())
        // An open still waiting for its reply is closed when the reply comes: its token is old by then.
        flow.leave()
        val ch = channel
        channel = null
        token++
        inFlight = false
        lines.clear()
        main.removeCallbacks(wake)
        if (ch != null) {
            val (mux, id) = ch
            net.execute {
                letGo?.let { runCatching { mux.send(id, it.toByteArray(Charsets.UTF_8)) } }
                runCatching { mux.closeChannel(id) }
            }
        }
        publish()
    }

    /** "Check again", "Connect", "Try now". */
    fun retry() = act(flow.retry(now()))

    private fun act(step: TouchpadStep) {
        if (step.open) openChannel()
        publish()
        schedule()
    }

    private fun openChannel() {
        val m = machine ?: return
        val t = ++token
        val inbox = Inbox(t)
        net.execute {
            try {
                val mux = hub.link(m).shared()
                val id = mux.nextChannelId()
                val reply = mux.openChannel(
                    id,
                    Input.OPEN.toByteArray(Charsets.UTF_8),
                    timeoutMs = OPEN_TIMEOUT_MS,
                    channelListener = inbox.listener(),
                    accepted = Input::accepted,
                )
                val opened = Input.readOpen(reply)
                main.post {
                    if (t != token) {
                        if (opened is Opened.Ready) net.execute { runCatching { mux.closeChannel(id) } }
                        return@post
                    }
                    if (opened is Opened.Ready) {
                        channel = mux to id
                        input.fresh()
                    }
                    act(flow.opened(now(), opened))
                    inbox.release()
                }
            } catch (_: Exception) {
                main.post {
                    if (t == token) act(flow.openFailed(now()))
                }
            }
        }
    }

    /**
     * A channel's events, held back until the reply to `Open` has been
     * handled: the computer's first line can arrive on the connection's
     * thread before that reply reaches the main thread.
     */
    private inner class Inbox(val t: Int) {
        private val lock = Any()
        private var holding = true
        private val held = mutableListOf<() -> Unit>()

        private fun deliver(r: () -> Unit) {
            synchronized(lock) {
                if (holding) {
                    held += r
                    return
                }
            }
            main.post(r)
        }

        fun release() {
            val now = synchronized(lock) {
                holding = false
                held.toList().also { held.clear() }
            }
            for (r in now) r()
        }

        fun listener() = object : Mux.Listener {
            override fun onData(channel: UInt, bytes: ByteArray) {
                val text = bytes.toString(Charsets.UTF_8)
                deliver { if (t == token) lines(text) }
            }

            override fun onClose(channel: UInt, reason: String) = deliver { if (t == token) ended() }

            override fun onDisconnect(cause: Throwable?) = deliver { if (t == token) ended() }
        }
    }

    private fun lines(text: String) {
        lines.append(text)
        while (true) {
            val nl = lines.indexOf("\n")
            if (nl < 0) break
            val line = lines.substring(0, nl)
            lines.delete(0, nl + 1)
            Input.readEvent(line)?.let(::notice)
        }
        // A computer that never ends a line is not allowed to grow this without bound.
        if (lines.length > MAX_LINE) lines.clear()
    }

    private fun notice(n: Notice) {
        when (n) {
            is Notice.Paused -> input.paused()
            is Notice.Released -> {
                input.released()
                _state.update { it.copy(releasedAt = now()) }
            }
            is Notice.Closed -> channel = null
            Notice.Resumed -> {}
        }
        act(flow.notice(now(), n))
    }

    private fun ended() {
        channel = null
        act(flow.channelClosed(now()))
    }

    // ── input from the screen ────────────────────────────────────────────

    private val ready: Boolean get() = flow.phase == TouchpadPhase.Ready && channel != null

    fun down(id: Long, x: Double, y: Double, at: Long) {
        if (!ready) return
        feedback(input.down(id, x, y, at))
    }

    fun move(id: Long, x: Double, y: Double, at: Long) {
        if (!ready) return
        input.move(id, x, y, at)
        frame()
    }

    fun up(id: Long, at: Long) {
        if (!ready) return
        feedback(input.up(id, at))
    }

    fun button(button: MouseButton, pressed: Boolean) {
        if (!ready) return
        input.button(button, pressed)
        if (pressed) onClick?.invoke()
        publish()
        frame()
    }

    /** A whole click, for a screen reader's action on the pad. */
    fun click(button: MouseButton) {
        button(button, true)
        button(button, false)
    }

    fun toggle(mod: Mod) {
        if (!ready) return
        input.toggle(mod)
        publish()
    }

    fun tap(code: Int) {
        if (!ready) return
        input.tap(code)
        publish()
        frame()
    }

    /** A change of the hidden field; returns what the field should hold next. */
    fun typed(text: String, composing: IntRange?): TypedText.Field {
        val read = TypedText.read(text, composing)
        if (ready) {
            input.typed(read)
            publish()
            frame()
        }
        return read.next
    }

    /** A key from a hardware keyboard; true when it was taken. */
    fun hardware(keyCode: Int, pressed: Boolean, repeat: Boolean): Boolean {
        if (!ready) return false
        val taken = input.hardware(keyCode, pressed, repeat)
        if (taken) frame()
        return taken
    }

    private fun feedback(gs: List<Gesture>) {
        if (gs.any { it is Gesture.Click || it is Gesture.Press }) onClick?.invoke()
        if (gs.isNotEmpty() || input.dueAt(now()) != null) frame()
        schedule()
    }

    // ── frames and timers ────────────────────────────────────────────────

    private fun frame() {
        if (frameAsked) return
        frameAsked = true
        Choreographer.getInstance().postFrameCallback {
            frameAsked = false
            pump()
        }
    }

    private fun pump() {
        if (inFlight || !ready) {
            schedule()
            return
        }
        val now = now()
        val f = input.next(now)
        if (f == null) {
            schedule()
            return
        }
        val (mux, id) = channel ?: return
        val t = token
        inFlight = true
        net.execute {
            val ok = runCatching { mux.send(id, f.toByteArray(Charsets.UTF_8)) }.isSuccess
            main.post {
                if (t != token) return@post
                inFlight = false
                if (!ok) {
                    ended()
                    return@post
                }
                pump()
            }
        }
    }

    private val wake = Runnable {
        val now = now()
        if (ready) feedbackTick(now)
        act(flow.tick(now))
        if (ready) pump()
    }

    private fun feedbackTick(now: Long) {
        val gs = input.tick(now)
        if (gs.any { it is Gesture.Click }) onClick?.invoke()
    }

    /** One wake-up at the earliest of: a waiting tap's click, a `hold`, a refusal's retry. */
    private fun schedule() {
        main.removeCallbacks(wake)
        val now = now()
        val at = listOfNotNull(if (ready) input.dueAt(now) else null, flow.dueAt()).minOrNull() ?: return
        main.postAtTime(wake, maxOf(at, now + 1))
    }

    private fun publish() {
        _state.update {
            it.copy(phase = flow.phase, armed = input.armed, locked = input.locked)
        }
    }

    override fun onCleared() {
        leave()
        net.shutdown()
    }

    companion object {
        /** The computer makes its keyboard and pointer before it answers; far less than this. */
        const val OPEN_TIMEOUT_MS: Long = 20_000
        private const val MAX_LINE = 4_096
    }
}
