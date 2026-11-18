package com.rimeos.remote.phone

import android.Manifest
import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.database.ContentObserver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.ContactsContract
import android.provider.Telephony
import android.telephony.SmsManager
import android.util.Log
import androidx.core.content.ContextCompat
import com.rimeos.remote.core.phone.Messages
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.withTimeoutOrNull

/**
 * This phone's SMS store, read for a paired computer when it asks
 * (contract §2.11). Nothing read is kept: each answer is a fresh query.
 * Every function here needs its permission, and gives nothing without it.
 */
object Sms {
    private const val TAG = "RimeRemote"

    fun canRead(context: Context): Boolean = granted(context, Manifest.permission.READ_SMS)

    fun canSend(context: Context): Boolean = granted(context, Manifest.permission.SEND_SMS)

    fun canNames(context: Context): Boolean = granted(context, Manifest.permission.READ_CONTACTS)

    private fun granted(context: Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    private val PROJECTION = arrayOf(
        Telephony.Sms._ID,
        Telephony.Sms.THREAD_ID,
        Telephony.Sms.ADDRESS,
        Telephony.Sms.BODY,
        Telephony.Sms.DATE,
        Telephony.Sms.TYPE,
        Telephony.Sms.READ,
        Telephony.Sms.STATUS,
    )

    /** The conversation list, newest first (see [Messages.threads]). */
    fun threads(context: Context, before: Long?, limit: Int): List<Messages.Thread> {
        val names = if (canNames(context)) Names(context) else null
        return rows(context, null, null) { Messages.threads(it, before, limit) { n -> names?.of(n) } }
    }

    /** One conversation's messages, newest first, or null when it has none (see [Messages.thread]). */
    fun thread(context: Context, thread: Long, before: Long?, limit: Int): List<Messages.Message>? =
        rows(context, "${Telephony.Sms.THREAD_ID} = ?", arrayOf(thread.toString())) { Messages.thread(it, before, limit) }

    /** The addresses of one conversation, for a send into it; null when it has none. */
    fun addresses(context: Context, thread: Long): List<String>? =
        Messages.threads(
            rowsList(context, "${Telephony.Sms.THREAD_ID} = ?", arrayOf(thread.toString())).asSequence(),
            before = null,
            limit = 1,
        ).firstOrNull()?.addresses

    private fun rowsList(context: Context, selection: String?, args: Array<String>?): List<Messages.Row> =
        rows(context, selection, args) { it.take(Messages.SCAN_CAP).toList() }

    private fun <T> rows(
        context: Context,
        selection: String?,
        args: Array<String>?,
        use: (Sequence<Messages.Row>) -> T,
    ): T {
        val cursor = context.contentResolver.query(
            Telephony.Sms.CONTENT_URI,
            PROJECTION,
            selection,
            args,
            "${Telephony.Sms.DATE} DESC",
        ) ?: return use(emptySequence())
        return cursor.use { c ->
            val seq = sequence {
                while (c.moveToNext()) {
                    yield(
                        Messages.Row(
                            id = c.getLong(0),
                            thread = c.getLong(1),
                            address = c.getString(2),
                            body = c.getString(3),
                            dateMs = c.getLong(4),
                            type = c.getInt(5),
                            read = c.getInt(6) != 0,
                            status = if (c.isNull(7)) -1 else c.getInt(7),
                        ),
                    )
                }
            }
            use(seq)
        }
    }

    /** Names from the person's contacts, looked up once per number per answer. */
    private class Names(private val context: Context) {
        private val seen = HashMap<String, String?>()

        fun of(number: String): String? = seen.getOrPut(number) {
            runCatching {
                val uri = Uri.withAppendedPath(ContactsContract.PhoneLookup.CONTENT_FILTER_URI, Uri.encode(number))
                context.contentResolver.query(uri, arrayOf(ContactsContract.PhoneLookup.DISPLAY_NAME), null, null, null)
                    ?.use { if (it.moveToFirst()) it.getString(0) else null }
            }.onFailure { Log.i(TAG, "a contact name was not read: ${it.javaClass.simpleName}") }.getOrNull()
        }
    }
}

/**
 * Sends one SMS for a paired computer through Android's SmsManager, and
 * says whether every part went. One at a time: a second send while one is
 * going is [Messages.BUSY]. A long text goes as several parts, each with
 * its own result intent, and counts as sent only when all of them say so.
 */
class SmsSender(private val app: Context) {
    private val lock = Mutex()
    private val codes = AtomicInteger(0)

    /** Null when sent, or the word saying why not. */
    suspend fun send(number: String, text: String): String? {
        if (!lock.tryLock()) return Messages.BUSY
        try {
            return withTimeoutOrNull(Messages.SEND_WAIT_MS) { sendNow(number, text) } ?: Messages.TIMEOUT
        } finally {
            lock.unlock()
        }
    }

    private suspend fun sendNow(number: String, text: String): String? {
        @Suppress("DEPRECATION")
        val manager = if (Build.VERSION.SDK_INT >= 31) app.getSystemService(SmsManager::class.java) else SmsManager.getDefault()
        manager ?: return Messages.NO_SERVICE
        val parts = runCatching { manager.divideMessage(text) }.getOrNull()?.takeIf { it.isNotEmpty() } ?: return Messages.FAILED
        val token = "${System.nanoTime()}-${codes.incrementAndGet()}"
        val results = arrayOfNulls<Int>(parts.size)
        val done = CompletableDeferred<String?>()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.getStringExtra(EXTRA_TOKEN) != token) return
                val part = intent.getIntExtra(EXTRA_PART, -1)
                if (part !in results.indices) return
                synchronized(results) {
                    results[part] = resultCode
                    if (results.any { it == null }) return
                }
                done.complete(verdict(results.map { it!! }))
            }
        }
        // Not exported, and every intent names this package: only this
        // app's own result intents arrive here.
        ContextCompat.registerReceiver(app, receiver, IntentFilter(ACTION_SENT), ContextCompat.RECEIVER_NOT_EXPORTED)
        try {
            val sent = ArrayList<PendingIntent>(parts.size)
            for (i in parts.indices) {
                val intent = Intent(ACTION_SENT)
                    .setPackage(app.packageName)
                    .putExtra(EXTRA_TOKEN, token)
                    .putExtra(EXTRA_PART, i)
                // A distinct request code per part, so no part's intent
                // replaces another's.
                sent += PendingIntent.getBroadcast(
                    app,
                    codes.incrementAndGet(),
                    intent,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_ONE_SHOT,
                )
            }
            val started = runCatching {
                if (parts.size == 1) {
                    manager.sendTextMessage(number, null, parts[0], sent[0], null)
                } else {
                    manager.sendMultipartTextMessage(number, null, parts, sent, null)
                }
            }
            started.exceptionOrNull()?.let {
                Log.i(TAG, "an SMS was not sent: ${it.javaClass.simpleName}")
                return Messages.FAILED
            }
            return done.await()
        } finally {
            runCatching { app.unregisterReceiver(receiver) }
        }
    }

    companion object {
        private const val TAG = "RimeRemote"
        private const val ACTION_SENT = "com.rimeos.remote.SMS_SENT"
        private const val EXTRA_TOKEN = "token"
        private const val EXTRA_PART = "part"

        /** Every part's result: sent only when each says so; no signal is said as such. */
        fun verdict(codes: List<Int>): String? = when {
            codes.all { it == Activity.RESULT_OK } -> null
            codes.any { it == SmsManager.RESULT_ERROR_NO_SERVICE || it == SmsManager.RESULT_ERROR_RADIO_OFF } -> Messages.NO_SERVICE
            else -> Messages.FAILED
        }
    }
}

/**
 * Calls [onChange] when the SMS store changes, at most once per
 * [quietMs]: a burst (a long message arriving in parts, a thread marked
 * read) is one call, after it settles.
 */
class SmsWatcher(private val app: Context, private val quietMs: Long = 1_500, private val onChange: () -> Unit) {
    private val handler = Handler(Looper.getMainLooper())
    private val fire = Runnable { onChange() }
    private val observer = object : ContentObserver(handler) {
        override fun onChange(selfChange: Boolean) {
            handler.removeCallbacks(fire)
            handler.postDelayed(fire, quietMs)
        }
    }
    private var running = false

    @Synchronized
    fun sync(want: Boolean) {
        if (want == running) return
        if (want) {
            running = runCatching {
                app.contentResolver.registerContentObserver(Telephony.Sms.CONTENT_URI, true, observer)
            }.onFailure { Log.i(TAG, "the SMS store is not watched: ${it.javaClass.simpleName}") }.isSuccess
        } else {
            runCatching { app.contentResolver.unregisterContentObserver(observer) }
            handler.removeCallbacks(fire)
            running = false
        }
    }

    private companion object {
        const val TAG = "RimeRemote"
    }
}
