package com.rimeos.remote.testhelper

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log

private const val TAG = "RimeReplyHelper"
private const val CHANNEL = "chat"
private const val ID = 1
private const val RESULT = "reply"

/**
 * `am start -n com.rimeos.remote.testhelper/.PostActivity --es tag T --es title A --es text B`
 * posts a chat notification with a Reply action; `--ez cancel true` takes
 * tag T's away instead.
 */
class PostActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val tag = intent.getStringExtra("tag").orEmpty()
        val manager = getSystemService(NotificationManager::class.java)
        if (intent.getBooleanExtra("cancel", false)) {
            manager.cancel(tag, ID)
        } else {
            post(this, tag, intent.getStringExtra("title").orEmpty(), intent.getStringExtra("text").orEmpty())
        }
        finish()
    }
}

/** What a messaging app does with an inline reply: take it, then show it in the notification. */
class ReplyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val tag = intent.getStringExtra("tag").orEmpty()
        val title = intent.getStringExtra("title").orEmpty()
        val text = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(RESULT)?.toString()
        Log.i(TAG, "replied tag=$tag text=$text")
        if (text == null) return
        post(context, tag, title, "You: $text")
        Log.i(TAG, "reposted tag=$tag")
    }
}

private fun post(context: Context, tag: String, title: String, text: String) {
    val manager = context.getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(NotificationChannel(CHANNEL, "Chat", NotificationManager.IMPORTANCE_DEFAULT))
    // One PendingIntent per tag: extras alone do not tell two apart, so the
    // data URI does, or a second tag's reply would carry the first one's.
    val target = Intent(context, ReplyReceiver::class.java)
        .setData(Uri.fromParts("rime-reply", tag, null))
        .putExtra("tag", tag)
        .putExtra("title", title)
    // Mutable: Android writes the typed text into it when it is sent.
    val pending = PendingIntent.getBroadcast(
        context, 0, target, PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    val reply = Notification.Action.Builder(null, "Reply", pending)
        .addRemoteInput(RemoteInput.Builder(RESULT).setLabel("Reply").build())
        .build()
    manager.notify(
        tag,
        ID,
        Notification.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle(title)
            .setContentText(text)
            .addAction(reply)
            .build(),
    )
}
