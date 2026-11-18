package com.rimeos.remote.phone

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.CallLog
import android.telecom.TelecomManager
import android.telephony.PhoneNumberUtils
import android.telephony.TelephonyManager
import android.util.Log
import androidx.core.content.ContextCompat
import com.rimeos.remote.core.phone.Calls

/**
 * This phone's calls, for a paired computer (contract §2.12): what its call
 * is doing, its call log, and the phone's own buttons (answer, decline, end,
 * place a call) through Android's telephony service. There is no audio.
 * Every function here needs its permission, and does nothing without it.
 */
object PhoneCalls {
    private const val TAG = "RimeRemote"

    fun canState(context: Context): Boolean = granted(context, Manifest.permission.READ_PHONE_STATE)

    fun canHistory(context: Context): Boolean = granted(context, Manifest.permission.READ_CALL_LOG)

    fun canAnswer(context: Context): Boolean = granted(context, Manifest.permission.ANSWER_PHONE_CALLS)

    fun canDial(context: Context): Boolean = granted(context, Manifest.permission.CALL_PHONE)

    fun canNames(context: Context): Boolean = granted(context, Manifest.permission.READ_CONTACTS)

    private fun granted(context: Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    /** The switch and every grant, as they are now. */
    fun grants(context: Context, switchOn: Boolean): Calls.Grants = Calls.Grants(
        switchOn = switchOn,
        state = canState(context),
        history = canHistory(context),
        answer = canAnswer(context),
        dial = canDial(context),
    )

    /** Android's call state now ([Calls.CALL_STATE_IDLE] when it cannot be read). */
    fun telephonyState(context: Context): Int {
        if (!canState(context)) return Calls.CALL_STATE_IDLE
        val tm = context.getSystemService(TelephonyManager::class.java) ?: return Calls.CALL_STATE_IDLE
        @Suppress("DEPRECATION")
        return runCatching { tm.callState }.getOrDefault(Calls.CALL_STATE_IDLE)
    }

    /** A contact's name for [number], or null without the permission or a match. */
    fun name(context: Context, number: String): String? =
        if (canNames(context)) Sms.Names(context).of(number) else null

    /** The newest [limit] calls older than [before] (see [Calls.history]). */
    fun history(context: Context, before: Long?, limit: Int): List<Calls.Entry> {
        val names = if (canNames(context)) Sms.Names(context) else null
        val projection = arrayOf(
            CallLog.Calls.NUMBER,
            CallLog.Calls.NUMBER_PRESENTATION,
            CallLog.Calls.CACHED_NAME,
            CallLog.Calls.TYPE,
            CallLog.Calls.DATE,
            CallLog.Calls.DURATION,
        )
        val selection = before?.let { "${CallLog.Calls.DATE} < ?" }
        val args = before?.let { arrayOf(it.toString()) }
        val cursor = context.contentResolver.query(
            CallLog.Calls.CONTENT_URI,
            projection,
            selection,
            args,
            "${CallLog.Calls.DATE} DESC",
        ) ?: return emptyList()
        return cursor.use { c ->
            val rows = generateSequence { if (c.moveToNext()) c else null }.map {
                Calls.Row(
                    number = it.getString(0),
                    presentation = it.getInt(1),
                    cachedName = it.getString(2),
                    type = it.getInt(3),
                    dateMs = it.getLong(4),
                    durationS = it.getLong(5),
                )
            }
            Calls.history(rows, before, limit) { n -> names?.of(n) }
        }
    }

    /** Whether [number] is an emergency number here, which a computer never dials. */
    fun emergency(context: Context, number: String): Boolean {
        if (Build.VERSION.SDK_INT >= 29) {
            val tm = context.getSystemService(TelephonyManager::class.java)
            val known = runCatching { tm?.isEmergencyNumber(number) }.getOrNull()
            if (known != null) return known
        }
        @Suppress("DEPRECATION")
        return PhoneNumberUtils.isEmergencyNumber(number)
    }

    /**
     * Press one of the phone's buttons for a computer: [Calls.ANSWER],
     * [Calls.DECLINE], [Calls.HANGUP], or [Calls.DIAL] to [number]. The
     * caller has checked the switch, the grants and the call state. Null
     * when Android took it, or the word saying why not.
     */
    @Suppress("DEPRECATION")
    fun act(context: Context, action: String, number: String?): String? {
        val telecom = context.getSystemService(TelecomManager::class.java) ?: return Calls.FAILED
        return runCatching {
            when (action) {
                Calls.ANSWER -> {
                    telecom.acceptRingingCall()
                    null
                }
                // Android's one button for both: a ringing call is declined,
                // a call in progress is ended.
                Calls.DECLINE, Calls.HANGUP -> {
                    if (telecom.endCall()) null else Calls.NO_CALL
                }
                Calls.DIAL -> {
                    val n = number ?: return@runCatching Calls.FAILED
                    telecom.placeCall(Uri.fromParts("tel", n, null), Bundle())
                    null
                }
                else -> Calls.FAILED
            }
        }.getOrElse {
            Log.i(TAG, "a call action was not taken: ${it.javaClass.simpleName}")
            if (it is SecurityException) Calls.NO_PERMISSION else Calls.FAILED
        }
    }
}

/**
 * Follows this phone's call state from Android's PHONE_STATE broadcast while
 * [sync] says so, and calls [onChange] with each new [Calls.Now]. The number
 * comes with the broadcast only when READ_CALL_LOG is granted; Android then
 * says each change twice, once without it, which [Calls.next] folds into one.
 */
class CallWatcher(private val app: Context, private val onChange: (Calls.Now) -> Unit) {
    @Volatile
    var current: Calls.Now = Calls.Now.IDLE_NOW
        private set

    private var running = false

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val state = when (intent.getStringExtra(TelephonyManager.EXTRA_STATE)) {
                TelephonyManager.EXTRA_STATE_RINGING -> Calls.CALL_STATE_RINGING
                TelephonyManager.EXTRA_STATE_OFFHOOK -> Calls.CALL_STATE_OFFHOOK
                TelephonyManager.EXTRA_STATE_IDLE -> Calls.CALL_STATE_IDLE
                else -> return
            }
            @Suppress("DEPRECATION")
            val number = intent.getStringExtra(TelephonyManager.EXTRA_INCOMING_NUMBER)
            see(state, number)
        }
    }

    @Synchronized
    private fun see(telephony: Int, number: String?) {
        val before = current
        var next = Calls.next(before, telephony, number, System.currentTimeMillis())
        // A name, looked up once per call, when the person allowed contacts.
        if (next.number != null && next.name == null && next.number != before.number) {
            next = next.copy(name = runCatching { PhoneCalls.name(app, next.number!!) }.getOrNull())
        }
        if (next == before) return
        current = next
        onChange(next)
    }

    @Synchronized
    fun sync(want: Boolean) {
        if (want == running) return
        if (want) {
            // System broadcasts reach a receiver that is not exported.
            running = runCatching {
                ContextCompat.registerReceiver(
                    app,
                    receiver,
                    IntentFilter(TelephonyManager.ACTION_PHONE_STATE_CHANGED),
                    ContextCompat.RECEIVER_NOT_EXPORTED,
                )
            }.onFailure { Log.i(TAG, "calls are not followed: ${it.javaClass.simpleName}") }.isSuccess
            // What is happening now, in case a call is already going.
            if (running) see(PhoneCalls.telephonyState(app), null)
        } else {
            runCatching { app.unregisterReceiver(receiver) }
            running = false
            if (current.state != Calls.IDLE) {
                current = Calls.Now(Calls.IDLE, System.currentTimeMillis())
                onChange(current)
            }
        }
    }

    private companion object {
        const val TAG = "RimeRemote"
    }
}
