package com.rimeos.remote.ui.machine

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.machine.Machine
import com.rimeos.remote.core.machine.MachineControls
import com.rimeos.remote.core.machine.MachineStatus
import com.rimeos.remote.core.machine.MediaAction
import com.rimeos.remote.core.machine.MediaState
import com.rimeos.remote.link.LinkHub
import java.util.concurrent.TimeoutException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * What the "This computer" screen draws. Pure, so its rules are tested
 * without a phone (`MachineUiStateTest`).
 */
data class MachineUiState(
    val machine: String? = null,
    val offersStatus: Boolean = false,
    val offersLock: Boolean = false,
    val offersMedia: Boolean = false,
    val status: MachineStatus? = null,
    val media: MediaState? = null,
    /** True once the first read came back, so "nothing playing" is not drawn before anything was asked. */
    val loaded: Boolean = false,
    /** Which action is waiting on the computer: [LOCK], [MEDIA] or [VOLUME]. */
    val acting: String? = null,
    /** Why the last read failed. Cleared by the next read that works. */
    val away: String? = null,
    /** An action that did not work, in the computer's words. Dismissed by the person. */
    val failure: String? = null,
    /** An action that worked and that the person has to be told about. */
    val notice: String? = null,
) {
    fun offering(features: Collection<String>): MachineUiState = copy(
        offersStatus = Machine.STATUS in features,
        offersLock = Machine.LOCK in features,
        offersMedia = Machine.MEDIA in features,
    )

    fun read(status: MachineStatus?, media: MediaState?): MachineUiState = copy(
        status = status ?: this.status,
        media = media ?: this.media,
        loaded = true,
        away = null,
    )

    fun locked(confirmed: Boolean): MachineUiState = copy(
        acting = null,
        status = if (confirmed) status?.copy(locked = true) else status,
        notice = if (confirmed) "Locked" else null,
        failure = if (confirmed) null else "The computer was asked to lock, but its screen did not confirm it",
    )

    fun dismissed(): MachineUiState = copy(failure = null, notice = null)

    companion object {
        const val LOCK = "lock"
        const val MEDIA = "media"
        const val VOLUME = "volume"

        /**
         * One failure in words. [moved] is whether the request was one that
         * changes something and is never sent twice: after a drop, nobody
         * knows whether the computer did it, and the sentence says so instead
         * of claiming either.
         */
        fun failureOf(e: Throwable, machine: String, moved: Boolean = false): String = when (e) {
            is AgentError -> e.message
            is Disconnected, is java.io.IOException ->
                if (moved) {
                    "The connection to $machine dropped, so it may or may not have done that"
                } else {
                    "The connection to $machine dropped"
                }
            is TimeoutException -> "$machine did not answer in time"
            else -> e.message ?: "Something went wrong talking to $machine"
        }
    }
}

/**
 * The computer's status, lock and media, for the phone paired with it.
 *
 * Reads are polled by [watch], which the screen runs only while it is started:
 * nothing is asked while the phone is in a pocket. Every call goes through
 * [MachineControls] on `Dispatchers.IO`, over the same link the rest of the
 * app uses ([LinkHub]).
 */
class MachineViewModel(application: Application) : AndroidViewModel(application) {
    private val hub = LinkHub.get(application)

    private val _state = MutableStateFlow(MachineUiState())
    val state: StateFlow<MachineUiState> = _state.asStateFlow()

    private var machine: PairedMachine? = null

    fun open(machine: PairedMachine) {
        if (this.machine?.deviceId != machine.deviceId) {
            _state.value = MachineUiState(machine = machine.machine)
        }
        this.machine = machine
    }

    private fun controls(): MachineControls? {
        val m = machine ?: return null
        // The key is memory-only and gone after a lock of the app.
        if (hub.identity(m.deviceId) == null) return null
        return MachineControls(hub.link(m))
    }

    private fun features(): List<String> =
        machine?.let { hub.state(it.deviceId)?.remote?.features } ?: emptyList()

    /** Read now, then every [POLL_MS] until cancelled. */
    suspend fun watch() {
        while (true) {
            readOnce()
            delay(POLL_MS)
        }
    }

    fun refresh() {
        viewModelScope.launch { readOnce() }
    }

    private suspend fun readOnce() {
        val name = machine?.machine ?: return
        _state.update { it.offering(features()) }
        val c = controls() ?: return
        val s = _state.value
        try {
            val status = if (s.offersStatus) withContext(Dispatchers.IO) { c.status() } else null
            val media = if (s.offersMedia) withContext(Dispatchers.IO) { c.media() } else null
            _state.update { it.read(status, media) }
        } catch (e: Exception) {
            if (e is kotlinx.coroutines.CancellationException) throw e
            _state.update { it.copy(away = MachineUiState.failureOf(e, name), loaded = true) }
        }
    }

    fun lock() = act(MachineUiState.LOCK, moved = false) { c ->
        val confirmed = c.lock()
        _state.update { it.locked(confirmed) }
    }

    fun control(action: MediaAction) = act(MachineUiState.MEDIA, moved = true) { c ->
        val media = c.control(action)
        _state.update { it.copy(media = media, acting = null) }
    }

    fun volume(percent: Int) = act(MachineUiState.VOLUME, moved = false) { c ->
        val media = c.volume(percent.coerceIn(0, 100))
        _state.update { it.copy(media = media, acting = null) }
    }

    fun mute(muted: Boolean) = act(MachineUiState.VOLUME, moved = false) { c ->
        val media = c.mute(muted)
        _state.update { it.copy(media = media, acting = null) }
    }

    fun dismiss() = _state.update { it.dismissed() }

    private fun act(what: String, moved: Boolean, block: (MachineControls) -> Unit) {
        val name = machine?.machine ?: return
        if (_state.value.acting != null) return
        val c = controls() ?: return
        _state.update { it.copy(acting = what, failure = null, notice = null) }
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) { block(c) }
            } catch (e: Exception) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                _state.update { it.copy(acting = null, failure = MachineUiState.failureOf(e, name, moved)) }
            }
        }
    }

    companion object {
        /** Often enough that a track change shows up while the screen is open. */
        const val POLL_MS: Long = 3_000
    }
}
