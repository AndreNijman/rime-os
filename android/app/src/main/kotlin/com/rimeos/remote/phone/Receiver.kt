package com.rimeos.remote.phone

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.rimeos.remote.MainActivity
import com.rimeos.remote.core.agent.AgentError
import com.rimeos.remote.core.phone.DeviceEvent
import com.rimeos.remote.core.phone.OfferKind
import com.rimeos.remote.core.phone.OfferMemory
import com.rimeos.remote.core.phone.PhoneLink
import com.rimeos.remote.ui.Notifier
import java.io.ByteArrayOutputStream
import java.io.OutputStream

/**
 * Takes what a paired computer sends this phone (`rime remote send`,
 * contract §2.6) and puts it where it belongs, without asking: the computer
 * only lets a person at it send, so the phone does not ask again.
 *
 * Text goes onto the clipboard, and a notification says so without showing
 * it. A file goes into Downloads through MediaStore, under a name nothing
 * else has: nothing is ever replaced. A file this phone has no room for is
 * refused, leaving [RESERVE_BYTES] free. Android 9 has no MediaStore
 * Downloads, and the only folder it offers without a storage permission is
 * app storage that other apps can read, so there a file is refused with a
 * notification and text still works.
 */
class Receiver(private val context: Context) {

    fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(CHANNEL, "Sent from your computer", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "Files and text your computer sends this phone."
            setShowBadge(false)
        }
        context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
    }

    /**
     * Whether this phone tells the computer it can receive: only when the
     * person would hear about what arrived, so nothing lands unannounced.
     */
    val canReceive: Boolean
        get() {
            if (!Notifier(context).enabled) return false
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return true
            val channel = context.getSystemService(NotificationManager::class.java)?.getNotificationChannel(CHANNEL)
            return channel == null || channel.importance != NotificationManager.IMPORTANCE_NONE
        }

    /** Why a take was given up for good, rather than left for the next try. */
    class Refused(message: String) : Exception(message)

    /**
     * Take [offer] over [phone], put it away and tell the person. Returns
     * normally when the offer is finished with (taken, or refused for a
     * reason that would not change on a retry), after which the caller sends
     * `offer_done`; throws when the link failed and a replay should try again.
     */
    fun receive(machineName: String, phone: PhoneLink, offer: DeviceEvent.Offer) {
        ensureChannel()
        val from = machineName.ifEmpty { offer.machine.ifEmpty { "Your computer" } }
        when (offer.kind) {
            OfferKind.TEXT -> {
                val out = ByteArrayOutputStream()
                phone.take(offer.id, { out.write(it) })
                val text = out.toString(Charsets.UTF_8.name())
                val clipboard = context.getSystemService(ClipboardManager::class.java)
                    ?: throw Refused("this phone has no clipboard")
                clipboard.setPrimaryClip(ClipData.newPlainText("From $from", text))
                notify(offer, "Text from $from", "It is on your clipboard.", null)
            }
            OfferKind.FILE -> {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
                    notify(offer, "${offer.name} was not saved", "Files from $from need Android 10 or later. Text still works.", null)
                    throw Refused("files need Android 10")
                }
                val free = freeBytes()
                if (free in 0 until offer.len + RESERVE_BYTES) {
                    notify(offer, "Not enough room for ${offer.name}", "$from sent a file this phone has no room for.", null)
                    throw Refused("no room")
                }
                val saved = save(offer, phone)
                notify(offer, offer.name, "From $from, in Downloads.", saved)
            }
        }
    }

    /**
     * Free space where files are saved, or -1 when Android will not say.
     * Shared storage is emulated on top of the data volume, so its free space
     * is the data volume's.
     */
    private fun freeBytes(): Long = runCatching { Environment.getDataDirectory().usableSpace }.getOrDefault(-1L)

    /** Saves the file into Downloads and returns a uri that opens it. */
    @RequiresApi(Build.VERSION_CODES.Q)
    private fun save(offer: DeviceEvent.Offer, phone: PhoneLink): Uri {
        val mime = mimeOf(offer.name)
        val resolver = context.contentResolver
        // MediaStore picks a free name itself ("a (1).jpg"), so an
        // existing file is never replaced.
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, offer.name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw Refused("Downloads would not take a new file")
        try {
            (resolver.openOutputStream(uri) ?: throw Refused("Downloads would not open the new file")).use { out ->
                takeInto(offer, phone, out)
            }
            resolver.update(uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
            return uri
        } catch (e: Throwable) {
            runCatching { resolver.delete(uri, null, null) }
            throw e
        }
    }

    private fun takeInto(offer: DeviceEvent.Offer, phone: PhoneLink, out: OutputStream) {
        val taken = phone.take(offer.id, { out.write(it) })
        if (taken.kind != offer.kind) throw AgentError("internal", "the computer sent a different kind than it offered")
    }

    private fun notify(offer: DeviceEvent.Offer, title: String, text: String, open: Uri?) {
        if (!Notifier(context).enabled) return
        val intent = if (open != null) {
            Intent(Intent.ACTION_VIEW).setDataAndType(open, mimeOf(offer.name))
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        } else {
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        val pending = PendingIntent.getActivity(
            context,
            idFor(offer),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(title)
            .setContentText(text)
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            // A file's name is the person's own; on a locked screen only "Sent
            // from your computer" shows.
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setContentIntent(pending)
            .build()
        runCatching { NotificationManagerCompat.from(context).notify(idFor(offer), n) }
    }

    /** "Open Rime Remote to take it": a push said something was sent while the link was down. */
    fun waiting(machineName: String) {
        ensureChannel()
        if (!Notifier(context).enabled) return
        val pending = PendingIntent.getActivity(
            context,
            WAITING_ID,
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("${machineName.ifEmpty { "Your computer" }} sent you something")
            .setContentText("Open Rime Remote to take it. It waits for 15 minutes.")
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_STATUS)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setContentIntent(pending)
            .build()
        runCatching { NotificationManagerCompat.from(context).notify(WAITING_ID, n) }
    }

    fun clearWaiting() = runCatching { NotificationManagerCompat.from(context).cancel(WAITING_ID) }

    companion object {
        const val CHANNEL: String = "received"
        const val RESERVE_BYTES: Long = 64L * 1024 * 1024
        const val WAITING_ID: Int = 0x53454E54 // "SENT"

        /** Offers taken by this process, shared by every connection (see [OfferMemory]). */
        val offers = OfferMemory()

        fun idFor(offer: DeviceEvent.Offer): Int = (offer.at * 31 + offer.id).toInt() or 0x01000000

        fun mimeOf(name: String): String =
            MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
                ?: "application/octet-stream"
    }
}
