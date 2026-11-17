package com.rimeos.remote.share

import android.content.Intent
import android.net.Uri
import androidx.core.content.IntentCompat

/**
 * What another app handed this one through the share sheet ("Send to Rime").
 *
 * Nothing here sends anything. The SEND filter is exported, so any app on the
 * phone can fire it: arriving only fills the Send screen, which shows what
 * would go where, and only a tap there sends.
 *
 * Only `content://` streams from another app are kept. A `file://` URI names
 * a path this app would read with its OWN permissions, and a URI of this
 * app's own authority would be another app asking this one to send its own
 * files to a computer; both are counted in [refused] and never read.
 */
data class Shared(
    /** Text to put on the computer's clipboard. Null when files were shared. */
    val text: String?,
    /** Files to save in the computer's Downloads folder, in the order given. */
    val streams: List<Uri>,
    /** Items that were shared but will not be read (see the class comment). */
    val refused: Int,
) {
    companion object {
        /**
         * The share in [intent], or null when it is not one.
         *
         * Streams win over text: an app that shares a photo usually adds a
         * caption, and the photo is what was meant.
         */
        fun of(intent: Intent?, ownPackage: String): Shared? {
            val action = intent?.action ?: return null
            if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return null
            val raw = streams(intent, action)
            if (raw.isNotEmpty()) {
                val (ok, bad) = raw.partition { acceptable(it, ownPackage) }
                return Shared(text = null, streams = ok, refused = bad.size)
            }
            val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
            if (text.isNullOrEmpty()) return null
            return Shared(text = text, streams = emptyList(), refused = 0)
        }

        /** Another app's content, and nothing else. */
        fun acceptable(uri: Uri, ownPackage: String): Boolean {
            if (uri.scheme != "content") return false
            val authority = uri.authority ?: return false
            return authority != ownPackage && !authority.startsWith("$ownPackage.")
        }

        private fun streams(intent: Intent, action: String): List<Uri> {
            val extra = if (action == Intent.ACTION_SEND) {
                listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
            } else {
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
            }
            if (extra.isNotEmpty()) return extra.distinct()
            // Some apps put the files only in the ClipData, which is also what
            // carries the read grant.
            val clip = intent.clipData ?: return emptyList()
            return (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }.distinct()
        }
    }
}
