package com.rimeos.remote.ui.send

import android.app.Application
import android.content.res.AssetFileDescriptor
import android.net.Uri
import android.provider.OpenableColumns
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.Features
import com.rimeos.remote.core.link.Upload
import com.rimeos.remote.link.LinkHub
import com.rimeos.remote.share.Shared
import java.io.FilterInputStream
import java.io.InputStream
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * "Send to Rime": what another app shared, onto a paired computer.
 *
 * Text goes to the computer's clipboard (`clipboard_write`), files to its
 * Downloads folder (`save_file`), contract §1.8. Nothing is sent by [offer];
 * only [send], which the screen calls on a tap.
 *
 * Files go one at a time, in the order shared, and the send stops at the
 * first failure: the screen then says which were saved. A file is never sent
 * twice by this class, because after a dropped connection nobody knows
 * whether the computer kept it, and a second send would save a second copy.
 *
 * Each file dials a connection of its own ([LinkHub.dialSession]), so the
 * control link the rest of the app watches is not touched while it goes over.
 */
class SendViewModel(application: Application) : AndroidViewModel(application) {
    private val hub = LinkHub.get(application)
    private val resolver get() = getApplication<Application>().contentResolver

    private val _state = MutableStateFlow(SendUiState())
    val state: StateFlow<SendUiState> = _state.asStateFlow()

    private var uris: List<Uri> = emptyList()

    /** Set by Stop and by the app locking; checked between chunks. */
    private val stop = AtomicBoolean(false)

    init {
        viewModelScope.launch { hub.unlocked.collect { if (!it) stop.set(true) } }
    }

    /**
     * Show [shared] and the computers it could go to. Sends nothing.
     *
     * A share that arrives while another is going over is ignored: replacing
     * the screen under a send would hide how the first one ended.
     */
    fun offer(shared: Shared, machines: List<PairedMachine>) {
        if (_state.value.sending) return
        uris = shared.streams
        val choices = machines.map { SendChoice(it.deviceId, it.machine) }
        // The computer whose key is already open needs no second prompt.
        val preferred = machines.firstOrNull { hub.identity(it.deviceId) != null }?.deviceId
            ?: machines.singleOrNull()?.deviceId
        _state.value = SendUiState(
            text = shared.text,
            files = shared.streams.map { SendFile(null, null, null) },
            refused = shared.refused,
            describing = shared.streams.isNotEmpty(),
            machines = choices,
            chosen = preferred,
        )
        if (uris.isEmpty()) return
        val asked = uris
        viewModelScope.launch {
            // A provider's `query` is a binder call into another process.
            val described = withContext(Dispatchers.IO) { asked.map { describe(it) } }
            if (uris === asked) _state.update { it.copy(files = described, describing = false) }
        }
    }

    fun choose(deviceId: String) = _state.update { if (it.sending || it.done) it else it.copy(chosen = deviceId) }

    fun stop() = stop.set(true)

    /** Forget the share, once the person is done with the screen. */
    fun clear() {
        if (_state.value.sending) return
        uris = emptyList()
        _state.value = SendUiState()
    }

    /** A failure from before the send started: the key could not be opened. */
    fun failed(message: String) = _state.update { it.copy(failure = message) }

    fun dismiss() = _state.update { it.copy(failure = null, notice = null) }

    /** Send to [machine], whose key the caller has already opened. */
    fun send(machine: PairedMachine) {
        val start = _state.value
        if (!start.canSend || start.chosen != machine.deviceId) return
        stop.set(false)
        _state.update { it.copy(sending = true, failure = null, notice = null) }
        viewModelScope.launch {
            val name = machine.machine
            try {
                val link = hub.link(machine)
                val hello = withContext(Dispatchers.IO) { hub.state(machine.deviceId)?.hello ?: link.hello() }
                val text = start.text
                if (text != null) {
                    if (!hello.has(Features.CLIPBOARD_WRITE)) {
                        _state.update { it.copy(sending = false, failure = SendUiState.tooOld(name, files = false)) }
                        return@launch
                    }
                    withContext(Dispatchers.IO) { link.clipboardWrite(text) }
                    _state.update { it.copy(sending = false, done = true, notice = "Copied to $name's clipboard.") }
                    return@launch
                }
                if (!hello.has(Features.SAVE_FILE)) {
                    _state.update { it.copy(sending = false, failure = SendUiState.tooOld(name, files = true)) }
                    return@launch
                }
                sendFiles(machine, start)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                val failure = SendUiState.failureOf(e, null, name, lastByte = false, opened = false)
                _state.update { it.copy(sending = false, failure = failure) }
            }
        }
    }

    private suspend fun sendFiles(machine: PairedMachine, start: SendUiState) {
        val name = machine.machine
        var failure: String? = null
        for (i in start.sendable) {
            val file = start.files[i]
            val size = file.size ?: continue
            val uri = uris.getOrNull(i) ?: continue
            if (stop.get()) {
                failure = SendUiState.failureOf(Stopped(), file.label, name, lastByte = false, opened = false)
                break
            }
            _state.update { it.copy(current = i, sent = 0) }
            var lastByte = false
            var opened = false
            try {
                val saved = withContext(Dispatchers.IO) {
                    Upload.save(
                        connect = { hub.dialSession(machine) },
                        // No name: the computer picks one (contract §1.8).
                        name = file.name ?: "",
                        len = size,
                        source = {
                            opened = true
                            open(uri)
                        },
                        onProgress = { n ->
                            if (n >= size) {
                                lastByte = true
                            } else if (stop.get()) {
                                // Between chunks, before the last: the
                                // computer is left an early end and keeps
                                // nothing.
                                throw Stopped()
                            }
                            _state.update { it.copy(sent = n) }
                        },
                    )
                }
                _state.update { it.copy(saved = it.saved + saved) }
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                failure = SendUiState.failureOf(e, file.label, name, lastByte, opened)
                break
            }
        }
        _state.update {
            it.copy(
                sending = false,
                done = true,
                current = null,
                failure = failure,
                notice = SendUiState.summary(it.saved, name),
            )
        }
    }

    /** The shared file, with read failures told apart from connection failures. */
    private fun open(uri: Uri): InputStream {
        val stream = try {
            resolver.openInputStream(uri)
        } catch (e: SecurityException) {
            throw Unreadable("this app is no longer allowed to read it", e)
        } catch (e: java.io.IOException) {
            throw Unreadable(e.message ?: "it could not be opened", e)
        } ?: throw Unreadable("the app that shared it returned nothing")
        return Guarded(stream)
    }

    private class Guarded(inner: InputStream) : FilterInputStream(inner) {
        override fun read(): Int = guard { super.read() }
        override fun read(b: ByteArray, off: Int, len: Int): Int = guard { super.read(b, off, len) }
        private inline fun guard(block: () -> Int): Int = try {
            block()
        } catch (e: java.io.IOException) {
            throw Unreadable(e.message ?: "reading it failed", e)
        } catch (e: SecurityException) {
            throw Unreadable("this app is no longer allowed to read it", e)
        }
    }

    /**
     * A shared file's name and size, or why it will not be sent.
     *
     * The size comes from `OpenableColumns`, then from the file descriptor.
     * A file whose size nobody will say is not sent: the computer commits to
     * exactly `len` bytes before the first one crosses, and this app keeps no
     * spool to measure it in.
     */
    private fun describe(uri: Uri): SendFile {
        var name: String? = null
        var size: Long? = null
        try {
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    val n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val s = c.getColumnIndex(OpenableColumns.SIZE)
                    if (n >= 0 && !c.isNull(n)) name = c.getString(n)?.takeIf { it.isNotBlank() }
                    if (s >= 0 && !c.isNull(s)) size = c.getLong(s).takeIf { it >= 0 }
                }
            }
            if (size == null) {
                size = resolver.openAssetFileDescriptor(uri, "r")?.use { fd ->
                    fd.length.takeIf { it != AssetFileDescriptor.UNKNOWN_LENGTH && it >= 0 }
                        ?: fd.parcelFileDescriptor.statSize.takeIf { it >= 0 }
                }
            }
        } catch (e: SecurityException) {
            return SendFile(name, size, "This app was not allowed to read it.")
        } catch (e: Exception) {
            return SendFile(name, size, "It could not be read (${e.message ?: e.javaClass.simpleName}).")
        }
        val known = size
        val problem = when {
            known == null -> "The app that shared it did not say how big it is, so it cannot be sent."
            known == 0L -> "It is empty."
            known > Upload.SAVE_MAX_BYTES -> SendUiState.tooBig(known)
            else -> null
        }
        return SendFile(name, size, problem)
    }
}
