package com.rimeos.remote.link

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import com.rimeos.remote.core.MachineStore
import com.rimeos.remote.core.phone.CompanionKeys
import com.rimeos.remote.data.MachineRepository

/**
 * After a restart, or after this app was updated, start the companion link
 * (contract §2.10), and only when the person asked for it.
 *
 * `BOOT_COMPLETED`, not `LOCKED_BOOT_COMPLETED`: the store lives in
 * credential-encrypted storage, so there is nothing to read until the phone's
 * first unlock after the restart. The link comes back then, without the app
 * being opened.
 *
 * Starting a foreground service from here is allowed on Android 12 and later:
 * `BOOT_COMPLETED` and `MY_PACKAGE_REPLACED` are both on Android's list of
 * exemptions from the background-start restriction. Android 15 forbids a
 * `BOOT_COMPLETED` receiver from launching six service types (dataSync,
 * camera, mediaPlayback, phoneCall, mediaProjection, microphone);
 * [LinkService] is `specialUse`, which is not one of them.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED && intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        // A small file read, here rather than in a coroutine: the exemption
        // that lets this start a foreground service lasts while the broadcast
        // is being handled.
        val store = runCatching { MachineRepository(context).loadBlocking() }.getOrNull() ?: return
        if (!BootStart.shouldStart(store)) return
        Log.i(TAG, "starting the companion link after ${intent.action}")
        LinkService.startCompanion(context)
    }

    private companion object {
        const val TAG = "RimeRemote"
    }
}

/**
 * The boot receiver's decision, and companion mode's: the switch is on AND a
 * machine holds a registered companion key. Either missing, nothing starts.
 * Free of Android so a JVM test can hold it to that.
 */
object BootStart {
    fun shouldStart(store: MachineStore): Boolean = CompanionKeys.shouldStart(store)
}
