package com.rimeos.remote.core.agent

import kotlinx.serialization.Serializable

/**
 * The strings a machine advertises, and the only way this app decides to use
 * something newer than the oldest machine it talks to.
 *
 * Two lists, from two processes, and they are not interchangeable:
 *
 * * `rime-agentd`'s `hello` carries [Hello.features] — what the RUNTIME can do
 *   with a session: submit a typed reply, rename, peek.
 * * `rime-remoted`'s own `remote_hello` carries [RemoteHello.features] — what
 *   the CONNECTION can do: whether a terminal may share the control
 *   connection, whether it expects pongs.
 *
 * A phone and a desktop are updated by different people at different times —
 * the OS through `rime update`, this app through its own release — so every
 * one of these is feature-detected and every one has a fallback that is
 * today's behaviour. Nothing here is inferred from a version number: a
 * version cannot say that a fix was backported, and trying a verb to see
 * whether it is refused is not an option for `input`, where the refusal would
 * arrive after the sentence was already typed.
 */
object Features {
    // ---- rime-agentd, in `hello` ------------------------------------------

    /** `input` takes `submit: true`: the daemon writes the Return itself, 80 ms later. */
    const val INPUT_SUBMIT: String = "input_submit"

    /** `rename` exists, and `SessionInfo.name` is a person's name for the session. */
    const val RENAME: String = "rename"

    /** `peek` exists: the tail of a session's output without attaching. */
    const val PEEK: String = "peek"

    /** `SessionInfo.name` is populated. */
    const val SESSION_NAME: String = "session_name"

    /** `SessionInfo.title` is populated from the agent's OSC title. */
    const val SESSION_TITLE: String = "session_title"

    /** A phone's attach no longer leaves the desktop's terminal at the phone's size. */
    const val SIZE_RESTORE: String = "size_restore"

    /** `clipboard_write` exists: the phone can set the computer's clipboard. */
    const val CLIPBOARD_WRITE: String = "clipboard_write"

    /** `save_file` exists: the phone can save a shared file into the computer's Downloads. */
    const val SAVE_FILE: String = "save_file"

    // ---- rime-remoted, in `remote_hello` ----------------------------------

    /** `TCP_NODELAY` on every socket, one write per frame. */
    const val NODELAY: String = "nodelay"

    /**
     * Control requests run on a worker, in order, so frames keep flowing while
     * one is slow. The precondition for [MUX_ATTACH] being safe.
     */
    const val CONTROL_WORKER: String = "control_worker"

    /**
     * A terminal may be opened as a channel on the control connection.
     *
     * Without it this app dials a connection per terminal, as it always did,
     * and the reason is the one `MachineLink`'s class note gives: an older
     * `rime-remoted` answers a control frame from a loop that blocks while the
     * daemon thinks, and a terminal behind that loop would freeze for as long
     * as a slow request took.
     */
    const val MUX_ATTACH: String = "mux_attach"

    /** The machine expects pongs and closes a connection that stops answering. */
    const val LIVENESS: String = "liveness"
}

/**
 * `rime-remoted`'s answer to `remote_hello` (contract §2.1).
 *
 * [lan] is the list of addresses the machine is listening on right now,
 * through the same filter pairing uses. It is how a phone paired on one
 * network keeps finding its machine after DHCP moves it: the addresses in the
 * pairing offer are a snapshot, and a snapshot is wrong by next week.
 *
 * Unauthenticated in the sense that matters least: it arrived over the Noise
 * channel, so it came from the machine, but an address is only ever a hint —
 * reaching the wrong host produces a handshake that does not complete, never
 * a session with the wrong machine.
 */
@Serializable
data class RemoteHello(
    val version: Int = 0,
    val features: List<String> = emptyList(),
    val lan: List<String> = emptyList(),
) {
    fun has(feature: String): Boolean = feature in features

    /**
     * The addresses worth storing: well-formed-looking, deduplicated, capped.
     *
     * A defensive filter rather than a parser. The machine is trusted to say
     * where it is, but a list that grew without bound or carried a newline
     * would end up in the store and in a log line, and neither is a place for
     * whatever a buggy build decided to send.
     */
    fun lanHints(): List<String> =
        lan.asSequence()
            .map { it.trim() }
            .filter { it.isNotEmpty() && it.length <= MAX_ADDRESS && it.none { c -> c.isWhitespace() || c.isISOControl() } }
            .distinct()
            .take(MAX_ADDRESSES)
            .toList()

    companion object {
        /** What an `rime-remoted` older than the verb is taken to have said. */
        val NONE: RemoteHello = RemoteHello()

        /** An IPv6 literal in brackets with a port is under 50 characters. */
        const val MAX_ADDRESS: Int = 64

        /** A laptop has a handful of interfaces, not dozens. */
        const val MAX_ADDRESSES: Int = 8
    }
}

/**
 * Session names: the rule for what a person may call one, and what a screen
 * may draw.
 *
 * ## The rule is the daemon's, repeated here for a better sentence
 *
 * `rime-agent-core` has one validation function and the daemon and the CLI
 * both use it: trim; empty means "clear the name"; at most [MAX_CHARS]
 * characters; no control characters; refused, never sanitised. This is the
 * same rule so that a phone can say "that name is 70 characters, the limit is
 * 64" while the dialog is still open, rather than sending it and showing a
 * refusal from a machine. The daemon checks again and its answer is the one
 * that counts.
 *
 * "Characters" means what Rust's `chars().count()` means — Unicode scalar
 * values — which is `codePointCount` here and NOT `String.length`: Kotlin
 * counts UTF-16 units, so a name of 64 emoji is 128 long in Kotlin and 64 to
 * the daemon. Counting the Kotlin way would refuse names the machine accepts.
 *
 * "Control" is Rust's `char::is_control`, the Unicode general category Cc:
 * U+0000–U+001F and U+007F–U+009F. That is `Character.getType == CONTROL`,
 * checked per code point.
 */
object SessionNames {
    /** The daemon's limit, in characters (code points). */
    const val MAX_CHARS: Int = 64

    /** What the daemon clips a terminal title to. */
    const val MAX_TITLE_CHARS: Int = 80

    /** The outcome of [check]. */
    sealed class Checked {
        /** Send this. `null` clears the name. */
        data class Ok(val name: String?) : Checked()

        /** Do not send anything; tell the person [why]. */
        data class Refused(val why: String) : Checked()
    }

    fun check(raw: String): Checked {
        val name = raw.trim()
        if (name.isEmpty()) return Checked.Ok(null)
        val count = name.codePointCount(0, name.length)
        if (count > MAX_CHARS) {
            return Checked.Refused(
                "That name is $count characters long. A session name can be at most $MAX_CHARS.",
            )
        }
        if (name.codePoints().anyMatch { Character.getType(it) == Character.CONTROL.toInt() }) {
            return Checked.Refused(
                "That name has a control character in it — a tab, a line break or something " +
                    "invisible. Type it on one line.",
            )
        }
        return Checked.Ok(name)
    }

    /**
     * A name or title, made safe to draw, or null when nothing drawable is
     * left.
     *
     * Applied to BOTH, and it matters most for the title, which is whatever
     * the agent printed: the daemon strips control characters already, and
     * this strips them again along with the bidirectional overrides
     * (U+202A–U+202E, U+2066–U+2069) that would let a title reorder the text
     * drawn after it. Then it clips to [max] characters. A phone that trusted
     * the daemon's cleaning completely would be trusting every daemon it will
     * ever talk to, including one with a bug.
     */
    fun forDisplay(raw: String?, max: Int): String? {
        if (raw == null) return null
        val out = StringBuilder(raw.length)
        var kept = 0
        var i = 0
        while (i < raw.length && kept < max) {
            val cp = raw.codePointAt(i)
            i += Character.charCount(cp)
            if (Character.getType(cp) == Character.CONTROL.toInt()) continue
            if (cp in 0x202A..0x202E || cp in 0x2066..0x2069) continue
            out.appendCodePoint(cp)
            kept++
        }
        return out.toString().trim().takeIf { it.isNotEmpty() }
    }
}
