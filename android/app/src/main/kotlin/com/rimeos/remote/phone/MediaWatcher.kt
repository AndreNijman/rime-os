package com.rimeos.remote.phone

import android.content.Context
import android.media.MediaMetadata
import android.media.session.MediaController
import android.media.session.MediaSession
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import com.rimeos.remote.core.phone.MediaBoard
import com.rimeos.remote.core.phone.MediaState
import com.rimeos.remote.core.phone.PlayState
import com.rimeos.remote.core.phone.SessionFacts

/**
 * Reads this phone's media sessions for the paired computers
 * (`docs/remote-live-contract.md` §2.8).
 *
 * `MediaSessionManager.getActiveSessions` is open to the app holding
 * notification access, named by its listener component ([MirrorService]), so
 * this needs no permission of its own. It is started only while something
 * could be sent (the app unlocked, "Media on your computer" on, a connected
 * computer that takes it: [com.rimeos.remote.link.LinkHub] decides) and
 * stopped otherwise, so it holds no callbacks the rest of the time.
 *
 * Everything runs on one thread of its own. Any change (a session added or
 * gone, a state or a track) re-reads every session into a [MediaBoard], and
 * what the board shows goes to [onChange]; the per-computer outboxes decide
 * whether that is worth sending. Only the title, artist, album, length,
 * position, speed and state are read: never artwork, URIs, the queue or
 * extras. Nothing is logged or written down.
 */
class MediaWatcher(private val context: Context, private val onChange: (MediaState?) -> Unit) {
    private val thread = HandlerThread("rime-media").apply { start() }
    private val handler = Handler(thread.looper)
    private val manager = context.getSystemService(MediaSessionManager::class.java)
    private val board = MediaBoard()

    // Owned by [thread].
    private var running = false
    private var allowed: Set<String> = emptySet()
    private var sessions: List<MediaController> = emptyList()
    private val callbacks = HashMap<MediaSession.Token, Pair<MediaController, MediaController.Callback>>()
    private val labels = HashMap<String, String>()
    /** A board id per session, stable while it lives: tokens are equal across lists, their objects are not. */
    private val ids = HashMap<MediaSession.Token, String>()
    private var nextId = 0L

    private val sessionsChanged = MediaSessionManager.OnActiveSessionsChangedListener { list ->
        if (running) reconcile(list.orEmpty())
    }

    /** Start watching, or, if already watching, only take the new [apps]. */
    fun start(apps: Set<String>) {
        handler.post {
            allowed = apps
            if (running) {
                publish()
                return@post
            }
            val listed = try {
                manager.addOnActiveSessionsChangedListener(sessionsChanged, MirrorService.component(context), handler)
                manager.getActiveSessions(MirrorService.component(context))
            } catch (e: SecurityException) {
                // Notification access is not (or no longer) granted.
                Log.i(TAG, "media sessions are not readable: ${e.message}")
                runCatching { manager.removeOnActiveSessionsChangedListener(sessionsChanged) }
                onChange(null)
                return@post
            }
            running = true
            reconcile(listed)
        }
    }

    /** Stop watching, let go of every session, and say nothing plays. */
    fun stop() {
        handler.post {
            if (!running) return@post
            running = false
            runCatching { manager.removeOnActiveSessionsChangedListener(sessionsChanged) }
            for ((controller, callback) in callbacks.values) runCatching { controller.unregisterCallback(callback) }
            callbacks.clear()
            ids.clear()
            labels.clear()
            sessions = emptyList()
            board.clear()
            // The last word, on this thread: a publish queued before this
            // stop has already run, so nothing can show a player after it.
            onChange(null)
        }
    }

    /** The person changed which apps may show; what is shown is worked out again at once. */
    fun setAllowed(apps: Set<String>) {
        handler.post {
            allowed = apps
            if (running) publish()
        }
    }

    private fun reconcile(list: List<MediaController>) {
        val own = context.packageName
        val next = list.filter { it.packageName != own }
        val tokens = next.map { it.sessionToken }.toSet()
        // Gone: let go of its callback.
        val gone = callbacks.keys.filter { it !in tokens }
        for (token in gone) {
            callbacks.remove(token)?.let { (c, cb) -> runCatching { c.unregisterCallback(cb) } }
            ids.remove(token)
        }
        // New: hear its changes.
        for (controller in next) {
            if (controller.sessionToken in callbacks) continue
            val callback = object : MediaController.Callback() {
                override fun onPlaybackStateChanged(state: PlaybackState?) = changed()
                override fun onMetadataChanged(metadata: MediaMetadata?) = changed()
                override fun onSessionDestroyed() = changed()
            }
            controller.registerCallback(callback, handler)
            callbacks[controller.sessionToken] = controller to callback
            ids[controller.sessionToken] = "s${nextId++}"
        }
        sessions = next
        read()
    }

    private fun changed() {
        if (running) read()
    }

    private fun read() {
        val now = SystemClock.elapsedRealtime()
        board.sync(sessions.mapNotNull { c -> ids[c.sessionToken]?.let { it to facts(c) } }, now)
        publish()
    }

    private fun publish() {
        if (!running) return
        onChange(board.current(allowed, SystemClock.elapsedRealtime()))
    }

    private fun facts(c: MediaController): SessionFacts {
        val ps = c.playbackState
        val md = c.metadata
        return SessionFacts(
            pkg = c.packageName,
            app = label(c.packageName),
            state = stateOf(ps?.state),
            title = md?.getText(MediaMetadata.METADATA_KEY_TITLE) ?: md?.getText(MediaMetadata.METADATA_KEY_DISPLAY_TITLE),
            artist = md?.getText(MediaMetadata.METADATA_KEY_ARTIST) ?: md?.getText(MediaMetadata.METADATA_KEY_ALBUM_ARTIST),
            album = md?.getText(MediaMetadata.METADATA_KEY_ALBUM),
            durationMs = md?.getLong(MediaMetadata.METADATA_KEY_DURATION) ?: 0,
            positionMs = ps?.position ?: -1,
            updatedAtMs = ps?.lastPositionUpdateTime ?: 0,
            speed = ps?.playbackSpeed ?: 1f,
        )
    }

    private fun label(pkg: String): String = labels.getOrPut(pkg) { labelOf(context, pkg) }

    companion object {
        private const val TAG = "RimeRemote"

        private fun stateOf(state: Int?): PlayState = when (state) {
            PlaybackState.STATE_PLAYING,
            PlaybackState.STATE_FAST_FORWARDING,
            PlaybackState.STATE_REWINDING,
            -> PlayState.PLAYING
            PlaybackState.STATE_BUFFERING,
            PlaybackState.STATE_CONNECTING,
            PlaybackState.STATE_SKIPPING_TO_NEXT,
            PlaybackState.STATE_SKIPPING_TO_PREVIOUS,
            PlaybackState.STATE_SKIPPING_TO_QUEUE_ITEM,
            -> PlayState.WAITING
            PlaybackState.STATE_PAUSED -> PlayState.PAUSED
            else -> PlayState.IDLE
        }

        /** An app's label, or its package name when this app may not see it. */
        fun labelOf(context: Context, pkg: String): String {
            val pm = context.packageManager
            return runCatching { pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString() }.getOrDefault(pkg)
        }

        /**
         * The packages with a media session now, for the settings screen,
         * read directly: the screen must list them whether or not a computer
         * is connected. Empty without notification access.
         */
        fun sessionPackages(context: Context): List<String> = runCatching {
            context.getSystemService(MediaSessionManager::class.java)
                .getActiveSessions(MirrorService.component(context))
                .map { it.packageName }
                .filter { it != context.packageName }
                .distinct()
        }.getOrDefault(emptyList())
    }
}
