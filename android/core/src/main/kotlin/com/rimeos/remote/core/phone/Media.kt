package com.rimeos.remote.core.phone

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * What this phone is playing, shown on a paired computer
 * (`docs/remote-live-contract.md` §2.8), and, where the person allows it
 * ("Control from your computer"), played, paused, skipped and sought from
 * there: each report says what the computer may do ([MediaState.can]) and
 * [Media.transport] decides what a `media_control` from it does here.
 *
 * Policy without Android in it, so it is tested here: which session is shown,
 * what of it crosses, and when. The app side reads each media session into
 * [SessionFacts] and hands them to a [MediaBoard]; one [MediaOutbox] per
 * connection decides what is sent.
 *
 * Every time here is `SystemClock.elapsedRealtime()` milliseconds, the clock
 * Android's `PlaybackState.lastPositionUpdateTime` is on. Wall time never
 * mixes in: a clock change would read as a seek.
 *
 * What leaves the phone is the app's name, title, artist, album, length,
 * position, speed and whether it is playing (and, with control on, which
 * controls the app offers), from apps the person switched on one by one, over
 * a connection that is already open. Never artwork, URIs, the queue or a
 * session's extras.
 */
object Media {
    /** The feature a machine's `remote_hello` lists; nothing is sent to one without it. */
    const val FEATURE: String = "phone_media"

    /** The feature a machine lists when it can use `can`; nobody else is sent it. */
    const val CONTROL_FEATURE: String = "phone_media_control"

    /** The `media_control` actions a computer sends. */
    val CONTROL_ACTIONS: Set<String> = setOf("play", "pause", "play_pause", "next", "previous", "seek")

    /** The caps the computer applies, applied here too so nothing larger crosses. */
    const val APP_CAP: Int = 60
    const val TITLE_CAP: Int = 120
    const val ARTIST_CAP: Int = 120
    const val ALBUM_CAP: Int = 120

    /** The longest length or position the computer takes: 7 days. */
    const val MAX_MS: Long = 7L * 24 * 60 * 60 * 1000

    /** The fastest rate the computer takes. */
    const val MAX_RATE: Double = 8.0

    /**
     * How far a position may stray from where the last one sent says playback
     * is now and still be the clock alone. The computer's own seek slack.
     */
    const val SEEK_MS: Long = 1_500

    /** At most one report per this long, per computer. */
    const val GAP_MS: Long = 500

    /**
     * The line for [state] as of [nowMs]: where playback is at that moment,
     * so it is worked out when the line is written, not when the state was
     * read. Null is "nothing to show".
     */
    fun encode(state: MediaState?, nowMs: Long): String =
        buildJsonObject {
            put("cmd", "phone_media")
            if (state == null) {
                put("playing", JsonNull)
            } else {
                put(
                    "playing",
                    buildJsonObject {
                        put("app", state.app)
                        put("state", if (state.playing) "playing" else "paused")
                        put("title", state.title)
                        if (state.artist.isNotEmpty()) put("artist", state.artist)
                        if (state.album.isNotEmpty()) put("album", state.album)
                        if (state.durationMs > 0) put("duration_ms", state.durationMs)
                        put("position_ms", state.positionAt(nowMs))
                        put("rate", state.rate)
                        if (state.can.isNotEmpty()) {
                            put("can", buildJsonArray { for (a in MediaAction.entries) if (a in state.can) add(JsonPrimitive(a.word)) })
                        }
                    },
                )
            }
        }.toString()

    /**
     * Whether the computer, shown [sent], already shows [newest] at [nowMs]:
     * everything but the position is the same, and the position is where
     * [sent] says the clock has carried it, give or take [SEEK_MS]. A seek, a
     * new track, a pause or a speed change is not the clock.
     */
    fun onlyClockMoved(sent: MediaState, newest: MediaState, nowMs: Long): Boolean =
        sent.pkg == newest.pkg &&
            sent.session == newest.session &&
            sent.can == newest.can &&
            sent.app == newest.app &&
            sent.playing == newest.playing &&
            sent.title == newest.title &&
            sent.artist == newest.artist &&
            sent.album == newest.album &&
            sent.durationMs == newest.durationMs &&
            sent.rate == newest.rate &&
            kotlin.math.abs(sent.positionAt(nowMs) - newest.positionAt(nowMs)) <= SEEK_MS

    /**
     * What a `media_control` [action] from the computer does to [live], the
     * session as it is now, given [sent], what this phone last showed that
     * computer: null (ignored) unless [live] is that very session (package
     * and session id) and both the report and the session allow it.
     * `play_pause` is resolved here, against [live]'s state; a seek is
     * clamped to the length when it is known.
     */
    fun transport(sent: MediaState, liveSession: String, live: SessionFacts, action: String, positionMs: Long?): Transport? {
        if (sent.session.isEmpty() || liveSession != sent.session || live.pkg != sent.pkg) return null
        val liveCan = MediaAction.of(live.actions)
        fun allowed(a: MediaAction) = a in sent.can && a in liveCan
        return when (action) {
            "play" -> Transport.Play.takeIf { allowed(MediaAction.PLAY) }
            "pause" -> Transport.Pause.takeIf { allowed(MediaAction.PAUSE) }
            "play_pause" -> when (live.state) {
                PlayState.PLAYING, PlayState.WAITING -> Transport.Pause.takeIf { allowed(MediaAction.PAUSE) }
                else -> Transport.Play.takeIf { allowed(MediaAction.PLAY) }
            }
            "next" -> Transport.Next.takeIf { allowed(MediaAction.NEXT) }
            "previous" -> Transport.Previous.takeIf { allowed(MediaAction.PREVIOUS) }
            "seek" -> {
                val to = positionMs?.takeIf { it >= 0 } ?: return null
                val end = live.durationMs.takeIf { it in 1..MAX_MS } ?: MAX_MS
                Transport.SeekTo(to.coerceAtMost(end)).takeIf { allowed(MediaAction.SEEK) }
            }
            else -> null
        }
    }

    /** [raw] on one line, control characters as spaces, so a title that is only those counts as empty. */
    internal fun visible(raw: CharSequence?): String =
        raw?.toString()?.map { if (it.isISOControl()) ' ' else it }?.joinToString("").orEmpty()
}

/** Where a session's playback stands, as Android's `PlaybackState` puts it, in the few cases that matter. */
enum class PlayState {
    PLAYING,

    /** Buffering, connecting, skipping: on its way to playing, the position held. */
    WAITING,
    PAUSED,

    /** Stopped, errored or none: nothing to show. */
    IDLE,
}

/**
 * What a computer may do to a session, as `can` names it (§2.8), from the
 * session's `PlaybackState` actions.
 */
enum class MediaAction(val word: String) {
    PLAY("play"),
    PAUSE("pause"),
    NEXT("next"),
    PREVIOUS("previous"),
    SEEK("seek"),
    ;

    companion object {
        // android.media.session.PlaybackState.ACTION_*, copied: core has no Android.
        const val ACTION_PAUSE: Long = 1L shl 1
        const val ACTION_PLAY: Long = 1L shl 2
        const val ACTION_SKIP_TO_PREVIOUS: Long = 1L shl 4
        const val ACTION_SKIP_TO_NEXT: Long = 1L shl 5
        const val ACTION_SEEK_TO: Long = 1L shl 8
        const val ACTION_PLAY_PAUSE: Long = 1L shl 9

        /** What [actions] (`PlaybackState.actions`) allows. A play/pause toggle allows both. */
        fun of(actions: Long): Set<MediaAction> = buildSet {
            fun has(bit: Long) = actions and bit != 0L
            if (has(ACTION_PLAY) || has(ACTION_PLAY_PAUSE)) add(PLAY)
            if (has(ACTION_PAUSE) || has(ACTION_PLAY_PAUSE)) add(PAUSE)
            if (has(ACTION_SKIP_TO_NEXT)) add(NEXT)
            if (has(ACTION_SKIP_TO_PREVIOUS)) add(PREVIOUS)
            if (has(ACTION_SEEK_TO)) add(SEEK)
        }
    }
}

/** What a `media_control` does to the session, through its `TransportControls`. */
sealed interface Transport {
    data object Play : Transport
    data object Pause : Transport
    data object Next : Transport
    data object Previous : Transport
    data class SeekTo(val ms: Long) : Transport
}

/** What the app reads off one media session, before anything is decided. */
data class SessionFacts(
    val pkg: String,
    /** The app's label, or its package name when the label cannot be read. */
    val app: String,
    val state: PlayState,
    val title: CharSequence?,
    val artist: CharSequence?,
    val album: CharSequence?,
    /** `METADATA_KEY_DURATION`; 0 or less is unknown. */
    val durationMs: Long,
    /** `PlaybackState.position`; negative is unknown. */
    val positionMs: Long,
    /** `PlaybackState.lastPositionUpdateTime`, elapsed-realtime ms; 0 is unknown. */
    val updatedAtMs: Long,
    /** `PlaybackState.playbackSpeed`. */
    val speed: Float,
    /** `PlaybackState.actions`: what the app lets a controller do. */
    val actions: Long = 0,
)

/**
 * What a computer is shown: clipped and clamped to what it accepts, so a
 * report is never refused for a value Android allows (an unknown position, a
 * live stream's length, a rewind's negative speed).
 *
 * The position is [positionMs] at [atMs] and moves from there at [rate] while
 * [playing], as the computer moves it; [positionAt] is where it is at any
 * moment.
 */
data class MediaState(
    val app: String,
    val playing: Boolean,
    val title: String,
    val artist: String = "",
    val album: String = "",
    /** 0 is unknown. */
    val durationMs: Long = 0,
    val positionMs: Long = 0,
    val atMs: Long = 0,
    val rate: Double = 1.0,
    /** What the computer may do to it; empty is display only. Stripped per computer unless control is on. */
    val can: Set<MediaAction> = emptySet(),
    /** The app and the board's session id it came from: where a control goes. Never sent. */
    val pkg: String = "",
    val session: String = "",
) {
    /** Where playback is at [nowMs], in whole milliseconds, never past the end. */
    fun positionAt(nowMs: Long): Long {
        var ms = positionMs.toDouble()
        if (playing && rate > 0 && nowMs > atMs) ms += (nowMs - atMs) * rate
        val end = if (durationMs > 0) durationMs else Media.MAX_MS
        return ms.toLong().coerceIn(0, end)
    }

    companion object {
        /**
         * What [f] shows as of [nowMs], or null when it shows nothing: idle,
         * or no title (a report with an empty title is refused, so none is
         * sent).
         */
        fun of(f: SessionFacts, nowMs: Long): MediaState? {
            if (f.state == PlayState.IDLE) return null
            val title = Mirror.clip(Media.visible(f.title), Media.TITLE_CAP)
            if (title.isBlank()) return null
            val duration = f.durationMs.takeIf { it in 1..Media.MAX_MS } ?: 0
            val speed = f.speed.toDouble().takeIf { it.isFinite() } ?: 1.0
            val rate = when (f.state) {
                PlayState.WAITING -> 0.0
                // A paused session's speed moves nothing; 0 would read as "stopped" to a player.
                PlayState.PAUSED -> if (speed > 0) speed.coerceAtMost(Media.MAX_RATE) else 1.0
                else -> speed.coerceIn(0.0, Media.MAX_RATE)
            }
            val at = f.updatedAtMs.takeIf { it in 1..nowMs } ?: nowMs
            val base = MediaState(
                app = Mirror.clip(Media.visible(f.app), Media.APP_CAP),
                playing = f.state == PlayState.PLAYING || f.state == PlayState.WAITING,
                title = title,
                artist = Mirror.clip(Media.visible(f.artist), Media.ARTIST_CAP),
                album = Mirror.clip(Media.visible(f.album), Media.ALBUM_CAP),
                durationMs = duration,
                positionMs = f.positionMs.coerceAtLeast(0),
                atMs = at,
                rate = rate,
                can = MediaAction.of(f.actions),
                pkg = f.pkg,
            )
            // Re-anchored at now and clamped, so a stale anchor or a position
            // past the end is not carried along.
            return base.copy(positionMs = base.positionAt(nowMs), atMs = nowMs)
        }
    }
}

/**
 * Every media session on the phone, and which one is shown.
 *
 * The rule: among sessions of the apps the person switched on that have a
 * title, the first playing one in the system's order (most recently active
 * first, as `MediaSessionManager` lists them); if none is playing, the one
 * that paused most recently; if none is paused either, nothing.
 *
 * "Paused most recently" is when this board saw it pause. A session already
 * paused when the board first sees it is dated by its last position update,
 * which Android stamps when the state was set, so a session paused
 * yesterday does not beat one paused a minute ago.
 *
 * Not thread-safe: the watcher's one thread owns it.
 */
class MediaBoard {
    private class Entry(var facts: SessionFacts, var pausedAt: Long)

    /** By session id, in the system's order. */
    private var entries = LinkedHashMap<String, Entry>()

    /**
     * The sessions Android lists now, in its order, by an id stable for each
     * session (its token). Ids no longer listed are forgotten.
     */
    fun sync(sessions: List<Pair<String, SessionFacts>>, nowMs: Long) {
        val next = LinkedHashMap<String, Entry>()
        for ((id, facts) in sessions) {
            val old = entries[id]
            val entry = if (old == null) {
                val since = facts.updatedAtMs.takeIf { it in 1..nowMs } ?: 0
                Entry(facts, if (facts.state == PlayState.PAUSED) since else 0)
            } else {
                if (facts.state == PlayState.PAUSED && old.facts.state != PlayState.PAUSED) old.pausedAt = nowMs
                old.facts = facts
                old
            }
            next[id] = entry
        }
        entries = next
    }

    fun clear() {
        entries = LinkedHashMap()
    }

    /** The packages with a session now, in the system's order. */
    val packages: List<String> get() = entries.values.map { it.facts.pkg }.distinct()

    /** What is shown as of [nowMs] from the apps in [allowed], or null for nothing. */
    fun current(allowed: Set<String>, nowMs: Long): MediaState? {
        val shown = entries.mapNotNull { (id, e) ->
            if (e.facts.pkg !in allowed) return@mapNotNull null
            MediaState.of(e.facts, nowMs)?.let { e to it.copy(session = id) }
        }
        shown.firstOrNull { (_, s) -> s.playing }?.let { return it.second }
        // maxBy keeps the first of equals: the system's order breaks a tie.
        return shown.filter { (e, _) -> e.facts.state == PlayState.PAUSED }.maxByOrNull { (e, _) -> e.pausedAt }?.second
    }
}

/**
 * What goes to ONE computer over ONE connection, and when.
 *
 * - Made with each connection, so it starts as "nothing shown": the computer
 *   removes a phone's player when the connection that described it ends, and
 *   a `null` is sent only to take back something this connection showed.
 * - Only the newest state counts ([offer]); nothing is queued.
 * - A state the computer already shows, moved on by the clock alone
 *   ([Media.onlyClockMoved]), is not sent: the computer moves the position
 *   itself.
 * - At most one report per [gapMs]; a change inside the gap is sent when it
 *   ends, as the newest state then, so the last change is never lost.
 * - A report that failed is not sent again until something new is offered.
 *
 * Not thread-safe: the connection's one sender owns it, under a lock shared
 * with whoever offers.
 */
class MediaOutbox(private val gapMs: Long = Media.GAP_MS) {
    private var newest: MediaState? = null
    private var shown: MediaState? = null
    private var lastSendAt: Long? = null
    private var blocked = false

    /** What the computer shows now, as far as this connection knows: the session a control from it acts on. */
    val showing: MediaState? get() = shown

    /** The state to show from now on; null is nothing. */
    fun offer(state: MediaState?) {
        newest = state
        blocked = false
    }

    /** Whether the newest state is not what the computer shows. */
    private fun wanted(nowMs: Long): Boolean {
        if (blocked) return false
        val n = newest
        val s = shown
        return when {
            n == null -> s != null
            s == null -> true
            else -> !Media.onlyClockMoved(s, n, nowMs)
        }
    }

    /**
     * What to send now, or null when nothing is due ([waitMs] says whether
     * something waits for the gap). The caller encodes it at the moment it
     * writes it ([Media.encode]) and then says [sent] or [failed].
     */
    fun next(nowMs: Long): Report? {
        if (!wanted(nowMs)) return null
        val last = lastSendAt
        if (last != null && nowMs - last < gapMs) return null
        return Report(newest)
    }

    /** How long until [next] has a report, or 0 when nothing waits for one. */
    fun waitMs(nowMs: Long): Long {
        if (!wanted(nowMs)) return 0
        val last = lastSendAt ?: return 0
        // At least 1 while something waits, so a caller that read the clock
        // twice never takes "due now" for "nothing waits" and sleeps on it.
        return (gapMs - (nowMs - last)).coerceAtLeast(1)
    }

    /** The computer took [report] at [nowMs]. */
    fun sent(report: Report, nowMs: Long) {
        shown = report.state
        lastSendAt = nowMs
    }

    /** [report] did not get there; it is not sent again until a new state is offered. */
    fun failed(report: Report, nowMs: Long) {
        lastSendAt = nowMs
        if (newest === report.state) blocked = true
    }

    /** One report: the state to show, or null for nothing. */
    class Report(val state: MediaState?)
}
