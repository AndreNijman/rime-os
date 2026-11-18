package com.rimeos.remote.testhelper

import android.app.Activity
import android.content.Context
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Bundle
import android.os.SystemClock
import android.util.Log

private const val MEDIA_TAG = "RimeMediaHelper"

/**
 * One media session, driven by the device suite (contract §2.8):
 *
 * `am start -n com.rimeos.remote.testhelper/.MediaActivity --es do play --el position 1000`
 *
 * `do` is `play` (at `position` ms), `pause`, `seek` (to `position`, keeping
 * the state) or `stop` (stopped, then released). The track is always the
 * same, so the shell passes no spaces: "Helper Song" by "Helper Band" from
 * "Helper LP", 3 minutes.
 */
class MediaActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val position = if (intent.hasExtra("position")) intent.getLongExtra("position", 0) else null
        when (intent.getStringExtra("do")) {
            "play" -> Player.play(this, position ?: Player.now())
            "pause" -> Player.set(PlaybackState.STATE_PAUSED, position ?: Player.now())
            "seek" -> Player.set(Player.state, position ?: Player.now())
            "stop" -> Player.stop()
            else -> Log.w(MEDIA_TAG, "unknown do=${intent.getStringExtra("do")}")
        }
        finish()
    }
}

/** The session, for the life of this process. */
private object Player {
    private var session: MediaSession? = null
    var state = PlaybackState.STATE_NONE
        private set
    private var position = 0L
    private var at = 0L

    /** Where playback is now. */
    fun now(): Long =
        if (state == PlaybackState.STATE_PLAYING) position + (SystemClock.elapsedRealtime() - at) else position

    fun play(context: Context, from: Long) {
        val s = session ?: MediaSession(context.applicationContext, "rime-helper").also { session = it }
        s.setMetadata(
            MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, "Helper Song")
                .putString(MediaMetadata.METADATA_KEY_ARTIST, "Helper Band")
                .putString(MediaMetadata.METADATA_KEY_ALBUM, "Helper LP")
                .putLong(MediaMetadata.METADATA_KEY_DURATION, 180_000)
                .build(),
        )
        set(PlaybackState.STATE_PLAYING, from)
        s.isActive = true
    }

    fun set(next: Int, from: Long) {
        val s = session ?: return Unit.also { Log.w(MEDIA_TAG, "no session: play first") }
        state = next
        position = from
        at = SystemClock.elapsedRealtime()
        s.setPlaybackState(
            PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_SEEK_TO)
                .setState(next, from, if (next == PlaybackState.STATE_PLAYING) 1f else 0f, at)
                .build(),
        )
        Log.i(MEDIA_TAG, "state=$next position=$from")
    }

    fun stop() {
        val s = session ?: return
        set(PlaybackState.STATE_STOPPED, now())
        s.isActive = false
        s.release()
        session = null
        state = PlaybackState.STATE_NONE
        Log.i(MEDIA_TAG, "released")
    }
}
