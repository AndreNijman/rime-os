package com.rimeos.remote.ui.send

import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.link.Disconnected
import com.rimeos.remote.core.link.Upload
import java.util.concurrent.TimeoutException

/** One shared file, as far as this phone could tell before sending it. */
data class SendFile(
    /** The name the sharing app gave it, or null when it gave none (the computer picks one). */
    val name: String?,
    /** Its length in bytes, or null when the sharing app would not say. */
    val size: Long?,
    /** Why it will not be sent, or null when it will. */
    val problem: String?,
) {
    val label: String get() = name ?: "Unnamed file"
}

/** A paired computer the Send screen offers. */
data class SendChoice(val deviceId: String, val name: String)

/**
 * What the Send screen draws. Pure, so its rules are tested without a phone
 * (`SendUiStateTest`).
 */
data class SendUiState(
    val text: String? = null,
    val files: List<SendFile> = emptyList(),
    /** Shared items this app will not read (another app's `file://`, or this app's own). */
    val refused: Int = 0,
    /** True while the files' names and sizes are being asked for. */
    val describing: Boolean = false,
    val machines: List<SendChoice> = emptyList(),
    val chosen: String? = null,
    val sending: Boolean = false,
    /** The index in [files] going over now. */
    val current: Int? = null,
    /** Bytes of [current] sent so far. */
    val sent: Long = 0,
    /** Every file the computer said it saved, in order. */
    val saved: List<Upload.Saved> = emptyList(),
    /** Set once a send has finished, whichever way; the screen then offers only Done. */
    val done: Boolean = false,
    val failure: String? = null,
    val notice: String? = null,
) {
    val empty: Boolean get() = text == null && files.isEmpty()

    /** Indices of the files that will be sent. */
    val sendable: List<Int> get() = files.indices.filter { files[it].problem == null }

    val textProblem: String? get() = text?.let { textRefusal(it) }

    val canSend: Boolean
        get() = !sending && !done && !describing && chosen != null &&
            if (text != null) textProblem == null else sendable.isNotEmpty()

    fun chosenName(): String? = machines.firstOrNull { it.deviceId == chosen }?.name

    companion object {
        /** `clipboard::MAX_BYTES` on the computer, to the byte. */
        const val MAX_TEXT_BYTES = 8192

        /** Refused here with the numbers, never cut to fit. */
        fun textRefusal(text: String): String? {
            val bytes = text.toByteArray(Charsets.UTF_8).size
            return when {
                text.isEmpty() -> "There is no text to send."
                bytes > MAX_TEXT_BYTES ->
                    "That text is $bytes bytes, and a computer's clipboard takes up to $MAX_TEXT_BYTES from a phone. Nothing was sent."
                else -> null
            }
        }

        fun tooBig(size: Long): String =
            "It is ${megabytes(size)}, and the most a phone can send is ${megabytes(Upload.SAVE_MAX_BYTES)}."

        fun megabytes(bytes: Long): String {
            val mb = bytes / (1024.0 * 1024.0)
            return if (mb >= 10) "${mb.toLong()} MB" else "%.1f MB".format(java.util.Locale.ROOT, mb)
        }

        fun tooOld(machine: String, files: Boolean): String =
            "$machine's Rime is too old to take ${if (files) "files" else "text"} from a phone. Update it, then share again."

        /**
         * One failure in words. [lastByte] is whether every byte had gone
         * over: after that, a dropped connection means nobody knows whether
         * the computer kept the file. [opened] is whether the computer had
         * taken the upload at all.
         */
        fun failureOf(e: Throwable, file: String?, machine: String, lastByte: Boolean, opened: Boolean): String {
            val what = file ?: "The text"
            return when (e) {
                is Stopped -> "Stopped. $what was not saved."
                is Unreadable -> "$what could not be read on this phone (${e.message}). It was not sent."
                is Upload.Refused, is AgentError -> "$what was not saved: ${e.message}"
                is Disconnected, is java.io.IOException, is TimeoutException -> when {
                    lastByte -> "The connection to $machine ended after all of ${file ?: "the text"} went over, so it may or may not be on $machine. Check before sending it again."
                    opened -> "The connection to $machine ended while ${file ?: "the text"} was going over. It was not saved."
                    else -> "Could not reach $machine (${e.message}). Nothing was sent."
                }
                else -> "Could not reach $machine (${e.message ?: e.javaClass.simpleName}). Nothing was sent."
            }
        }

        /** What was saved, for the line shown once a send has finished. */
        fun summary(saved: List<Upload.Saved>, machine: String): String? = when (saved.size) {
            0 -> null
            1 -> "Saved in $machine's Downloads as ${saved[0].name}."
            else -> "Saved ${saved.size} files in $machine's Downloads: ${saved.joinToString(", ") { it.name }}."
        }
    }
}

/** The person pressed Stop, or the app locked, between two chunks. */
class Stopped : Exception("stopped")

/** Reading the shared file on this phone failed: no grant any more, or the provider failed. */
class Unreadable(message: String, cause: Throwable? = null) : Exception(message, cause)
