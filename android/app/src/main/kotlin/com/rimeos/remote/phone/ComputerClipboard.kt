package com.rimeos.remote.phone

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.PersistableBundle
import com.rimeos.remote.core.phone.DeviceEvent

/**
 * The computer's clipboard on this phone (contract §2.14): a `clipboard` line
 * put on the phone's own clipboard, as plain text.
 *
 * No permission is needed: any app may set the clipboard, and Android shows
 * its own "copied" notice when one does. A clip the computer marked
 * sensitive carries [EXTRA_IS_SENSITIVE], which keeps the text out of that
 * notice and out of keyboard suggestions.
 */
object ComputerClipboard {
    /**
     * `ClipDescription.EXTRA_IS_SENSITIVE`, by its value: the constant is
     * Android 13's, the key is honoured from there on and harmless before.
     */
    private const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"

    /** Put [event]'s text on the clipboard, labelled as coming from [machine]. False when there is no clipboard. */
    fun put(context: Context, machine: String, event: DeviceEvent.Clipboard): Boolean {
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return false
        val clip = ClipData.newPlainText("From ${machine.ifEmpty { "your computer" }}", event.text)
        if (event.sensitive) {
            clip.description.extras = PersistableBundle().apply { putBoolean(EXTRA_IS_SENSITIVE, true) }
        }
        return runCatching { clipboard.setPrimaryClip(clip) }.isSuccess
    }
}
