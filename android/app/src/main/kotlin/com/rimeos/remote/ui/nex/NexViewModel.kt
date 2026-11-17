package com.rimeos.remote.ui.nex

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexCall
import com.rimeos.remote.core.nex.NexCalls
import com.rimeos.remote.core.nex.NexClient
import com.rimeos.remote.core.nex.NexConnection
import com.rimeos.remote.core.nex.NexEvent
import com.rimeos.remote.core.nex.NexMission
import com.rimeos.remote.core.nex.NexMissionDetail
import com.rimeos.remote.core.nex.NexSession
import com.rimeos.remote.core.nex.NexTranscript
import com.rimeos.remote.core.nex.NexUsage
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.link.LinkHub
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement

/**
 * nex on one paired computer: its sessions, approvals and missions, kept
 * current by the live feed.
 *
 * Every nexd call goes through [NexClient] on `Dispatchers.IO`: the link is
 * blocking, and a call on the main thread is the StrictMode crash the rest of
 * the app documents at each of its own call sites.
 *
 * ## Staying current
 *
 * Against a machine with `mux_attach` the feed is opened **before** the first
 * load, from now (`tail`), so an event that lands between the two is either
 * in the load or in the feed and never in neither. Events ask for a reload;
 * reloads are folded together for [DEBOUNCE_MS], so a turn that emits forty
 * events costs one round of reads, not forty. When the feed ends it is opened
 * again from the last gseq it delivered, and the lists are read once, because
 * events from the gap may be past the backlog nexd keeps.
 *
 * Without `mux_attach`, or while the feed cannot be opened, the lists are
 * read every [POLL_MS]: the same cadence as the Agent Center.
 *
 * ## Writes
 *
 * Each tap builds its [NexCall] once, and that one line is what is sent, and
 * re-sent by the link once after a dropped connection. nexd answers a repeated
 * `request_id` with the first answer, so the resend cannot approve twice or
 * post a message twice.
 */
class NexViewModel(application: Application) : AndroidViewModel(application) {
    private val hub = LinkHub.get(application)

    private val _state = MutableStateFlow(NexUiState())
    val state: StateFlow<NexUiState> = _state.asStateFlow()

    private var machine: PairedMachine? = null
    private var follow: Job? = null
    private var subscription: NexClient.Subscription? = null
    private var reloads = Channel<NexUiState.Reload>(Channel.UNLIMITED)

    /** Start showing [machine]'s nex. Opening the same machine again only reloads. */
    fun open(machine: PairedMachine) {
        if (this.machine?.deviceId == machine.deviceId && follow?.isActive == true) {
            reloads.trySend(EVERYTHING)
            return
        }
        leave()
        this.machine = machine
        reloads = Channel(Channel.UNLIMITED)
        _state.value = NexUiState(machine = machine.deviceId)
        follow = viewModelScope.launch { follow(machine) }
    }

    /** Stop following. Called when the user leaves nex, and on clear. */
    fun leave() {
        follow?.cancel()
        follow = null
        subscription?.close()
        subscription = null
        machine = null
    }

    override fun onCleared() {
        super.onCleared()
        leave()
    }

    fun refresh() {
        reloads.trySend(EVERYTHING)
    }

    fun dismiss() = _state.update { it.dismissed() }

    private fun client(): NexClient? {
        val m = machine ?: return null
        // The key is memory-only and gone after a lock; a screen that outlived
        // the lock has nothing to talk with.
        if (hub.identity(m.deviceId) == null) return null
        val shared = hub.state(m.deviceId)?.remote?.has(Features.MUX_ATTACH) == true
        return NexClient(hub.link(m), shared)
    }

    private suspend fun follow(machine: PairedMachine) = coroutineScope {
        launch { reloadLoop() }
        var lastGseq: Long? = null
        var backoff = FIRST_BACKOFF_MS
        while (isActive && this@NexViewModel.machine?.deviceId == machine.deviceId) {
            val c = client()
            val sub = if (c != null && c.shared && _state.value.away == null) {
                val ended = CompletableDeferred<Throwable?>()
                runCatching { withContext(Dispatchers.IO) { c.subscribe(lastGseq, listener(ended)) } }
                    .getOrNull()?.let { it to ended }
            } else {
                null
            }
            if (sub == null) {
                // No feed: read now, and again after a wait. Against a machine
                // that can follow, the wait grows to the poll interval while the
                // feed keeps failing; against one that cannot, it is the poll.
                reloads.trySend(EVERYTHING)
                delay(if (c?.shared == true && _state.value.away == null) backoff else POLL_MS)
                backoff = (backoff * 2).coerceAtMost(POLL_MS)
                continue
            }
            val (subscription, ended) = sub
            this@NexViewModel.subscription = subscription
            _state.update { it.copy(live = true) }
            // Read after the feed is open, never before: see the class note.
            reloads.trySend(EVERYTHING)
            val opened = System.currentTimeMillis()
            ended.await()
            this@NexViewModel.subscription = null
            _state.update { it.copy(live = false) }
            lastGseq = subscription.lastGseq.takeIf { it > 0 } ?: lastGseq
            // A feed that lasted was healthy, and the next one starts quickly;
            // one that ended at once is a refusal, and is not hammered.
            backoff = if (System.currentTimeMillis() - opened > HEALTHY_MS) FIRST_BACKOFF_MS else backoff
            delay(backoff)
            backoff = (backoff * 2).coerceAtMost(POLL_MS)
        }
    }

    private fun listener(ended: CompletableDeferred<Throwable?>) = object : NexClient.FeedListener {
        override fun onEvents(events: List<NexEvent>, startedNow: Boolean) {
            var reload = NexUiState.Reload(lists = false, transcript = false)
            _state.update { s ->
                val (next, r) = s.withEvents(events)
                reload = r
                next
            }
            if (reload.any) reloads.trySend(reload)
        }

        override fun onEnd(cause: Throwable?) {
            ended.complete(cause)
        }
    }

    private suspend fun reloadLoop() {
        for (first in reloads) {
            var r = first
            delay(DEBOUNCE_MS)
            while (true) {
                val more = reloads.tryReceive().getOrNull() ?: break
                r = NexUiState.Reload(r.lists || more.lists, r.transcript || more.transcript)
            }
            if (r.lists) loadLists()
            if (r.transcript) loadTranscript()
        }
    }

    /**
     * One round of list reads. A part that fails keeps what it showed, so a
     * dropped usage read does not blank the sessions; nex being away replaces
     * the screen; "not built" removes that section.
     */
    private suspend fun loadLists() {
        val c = client() ?: return
        _state.update { it.copy(loading = true) }
        val unbuilt = mutableSetOf<String>()
        var trouble: NexTrouble? = null
        fun <T> read(call: NexCall, parse: (JsonElement) -> T): T? = try {
            parse(c.call(call))
        } catch (e: Exception) {
            when (val t = NexTrouble.of(e)) {
                NexTrouble.NotBuilt -> unbuilt += call.method
                else -> if (trouble == null) trouble = t
            }
            null
        }
        val got = withContext(Dispatchers.IO) {
            val sessions = read(NexCalls.sessions(), NexSession::list)
            if (trouble is NexTrouble.Away) return@withContext null
            Lists(
                sessions = sessions,
                pending = read(NexCalls.pending(), NexApproval::pending),
                missions = read(NexCalls.missions(), NexMission::list),
                usage = read(NexCalls.usage(), NexUsage::list),
                connections = read(NexCalls.connections(), NexConnection::list),
            )
        }
        val t = trouble
        _state.update { s ->
            if (t is NexTrouble.Away) {
                return@update s.copy(loading = false, away = t.why, live = false)
            }
            s.copy(
                loading = false,
                loaded = true,
                away = null,
                sessions = got?.sessions ?: s.sessions,
                pending = got?.pending ?: s.pending,
                missions = got?.missions ?: s.missions,
                usage = got?.usage ?: s.usage,
                connections = got?.connections ?: s.connections,
                unbuilt = s.unbuilt + unbuilt,
                failure = when (t) {
                    is NexTrouble.Refused -> t.why
                    is NexTrouble.Dropped -> "Could not reach the computer: ${t.why}"
                    else -> s.failure
                },
            )
        }
        // Open missions keep their tasks current too.
        for (id in _state.value.missionDetails.keys) loadMission(id)
    }

    private data class Lists(
        val sessions: List<NexSession>?,
        val pending: List<NexApproval>?,
        val missions: List<NexMission>?,
        val usage: List<NexUsage>?,
        val connections: List<NexConnection>?,
    )

    private suspend fun loadTranscript() {
        val id = _state.value.selected ?: return
        val c = client() ?: return
        try {
            val t = withContext(Dispatchers.IO) { NexTranscript.parse(c.large(NexCalls.transcript(id))) }
            _state.update { s ->
                if (s.selected != id) return@update s
                s.copy(transcript = t, typing = if (t.running) s.typing else "")
            }
        } catch (e: Exception) {
            fail(e)
        }
    }

    /** Open a session's transcript. */
    fun select(session: String) {
        _state.update {
            if (it.selected == session) it else it.copy(selected = session, transcript = null, typing = "")
        }
        reloads.trySend(NexUiState.Reload(lists = false, transcript = true))
    }

    fun unselect() = _state.update { it.copy(selected = null, transcript = null, typing = "") }

    fun approve(a: NexApproval) = decide(a, NexCalls.approve(a), "Approved")

    fun deny(a: NexApproval) = decide(a, NexCalls.deny(a), "Denied")

    private fun decide(a: NexApproval, call: NexCall, done: String) {
        if (a.id in _state.value.deciding) return
        _state.update { it.copy(deciding = it.deciding + a.id, failure = null) }
        viewModelScope.launch {
            val c = client()
            try {
                if (c == null) error("this phone is locked; unlock it and try again")
                withContext(Dispatchers.IO) { c.call(call) }
                _state.update { s ->
                    s.copy(
                        deciding = s.deciding - a.id,
                        pending = s.pending.filterNot { it.id == a.id },
                        notice = "$done: ${a.summary}",
                    )
                }
            } catch (e: Exception) {
                _state.update { it.copy(deciding = it.deciding - a.id) }
                fail(e, write = true)
            }
            reloads.trySend(EVERYTHING)
        }
    }

    /** Send [text] to [session]; [onSent] runs once nexd has taken it, so the box clears then. */
    fun send(session: String, text: String, onSent: () -> Unit) = write(NexCalls.submit(session, text), "Sent") {
        onSent()
    }

    /** Stop [session]'s running turn. */
    fun stop(session: String) = write(NexCalls.cancel(session), "Stopped")

    /**
     * A new session in [root], then its first message if there is one. The
     * screen opens the session when [NexUiState.created] is set.
     */
    fun create(root: String, provider: String?, title: String?, firstMessage: String) {
        val create = NexCalls.create(root, provider, title)
        val message = firstMessage.trim()
        if (_state.value.sending) return
        _state.update { it.copy(sending = true, failure = null) }
        viewModelScope.launch {
            var id: String? = null
            try {
                val c = client() ?: error("this phone is locked; unlock it and try again")
                withContext(Dispatchers.IO) {
                    id = NexCalls.createdSession(c.call(create)) ?: error("nex made a session and did not say which")
                    if (message.isNotEmpty()) c.call(NexCalls.submit(id!!, message))
                }
            } catch (e: Exception) {
                fail(e, write = true)
            }
            // A session that was made is opened even when its first message
            // failed: it exists, and the message can be sent again from there.
            _state.update { s ->
                id?.let { s.copy(sending = false, created = it, selected = it, transcript = null, typing = "") }
                    ?: s.copy(sending = false)
            }
            reloads.trySend(EVERYTHING)
        }
    }

    fun createdConsumed() = _state.update { it.copy(created = null) }

    /** One of [NexMission.actions]. */
    fun mission(verb: String, mission: NexMission) = write(NexCalls.mission(verb, mission.id), null)

    /** Show or hide a mission's tasks. */
    fun toggleMission(id: String) {
        if (id in _state.value.missionDetails) {
            _state.update { it.copy(missionDetails = it.missionDetails - id) }
        } else {
            viewModelScope.launch { loadMission(id) }
        }
    }

    private suspend fun loadMission(id: String) {
        val c = client() ?: return
        try {
            val d = withContext(Dispatchers.IO) { NexMissionDetail.parse(c.call(NexCalls.mission(id))) }
            _state.update { it.copy(missionDetails = it.missionDetails + (id to d)) }
        } catch (e: Exception) {
            fail(e)
        }
    }

    private fun write(call: NexCall, done: String?, onDone: () -> Unit = {}) {
        if (_state.value.sending) return
        _state.update { it.copy(sending = true, failure = null) }
        viewModelScope.launch {
            try {
                val c = client() ?: error("this phone is locked; unlock it and try again")
                withContext(Dispatchers.IO) { c.call(call) }
                _state.update { it.copy(sending = false, notice = done ?: it.notice) }
                onDone()
            } catch (e: Exception) {
                _state.update { it.copy(sending = false) }
                fail(e, write = true)
            }
            reloads.trySend(EVERYTHING)
        }
    }

    private fun fail(e: Throwable, write: Boolean = false) {
        when (val t = NexTrouble.of(e)) {
            is NexTrouble.Away -> _state.update { it.copy(away = t.why, live = false) }
            is NexTrouble.Refused -> _state.update { it.copy(failure = t.why) }
            NexTrouble.NotBuilt -> _state.update { it.copy(failure = "This computer's nex cannot do that yet.") }
            is NexTrouble.Dropped -> _state.update {
                it.copy(
                    failure = if (write) {
                        "The connection dropped, so this may or may not have arrived. Trying again is " +
                            "safe: nex answers a repeat with its first answer."
                    } else {
                        "Could not reach the computer: ${t.why}"
                    },
                )
            }
        }
    }

    private companion object {
        val EVERYTHING = NexUiState.Reload(lists = true, transcript = true)

        /** Events closer together than this cost one round of reads. */
        const val DEBOUNCE_MS: Long = 250

        /** The Agent Center's cadence, for when there is no feed. */
        const val POLL_MS: Long = 4_000

        const val FIRST_BACKOFF_MS: Long = 500

        /** A feed open this long was working, not refused. */
        const val HEALTHY_MS: Long = 10_000
    }
}
