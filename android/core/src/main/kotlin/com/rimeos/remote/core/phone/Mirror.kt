package com.rimeos.remote.core.phone

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.security.MessageDigest

/**
 * This phone's notifications, shown on a paired computer
 * (`docs/remote-live-contract.md` §2.7).
 *
 * Everything here is policy without Android in it, so it is tested here: what
 * is sent, what is never sent, and how fast. The listener service on the app
 * side only reads a notification into [Facts] and [Content] and hands them on.
 *
 * What leaves the phone is an app's name, a title and a text (and, with
 * replying on, the label of the app's reply button), from apps the
 * person switched on one by one ([Facts.allowed], empty by default), over a
 * connection that is already open. Nothing is queued for a computer that is
 * not connected, nothing is replayed when one connects, and nothing is
 * written to this phone's storage.
 */
object Mirror {
    /** The feature a machine's `remote_hello` lists, and nothing is sent to one without it. */
    const val FEATURE: String = "phone_notifications"

    /** The feature that says the machine takes `reply` on a post and draws a reply field. */
    const val REPLY_FEATURE: String = "phone_notification_reply"

    /** The caps the computer applies, applied here too so nothing larger crosses. */
    const val APP_CAP: Int = 60
    const val TITLE_CAP: Int = 120
    const val TEXT_CAP: Int = 1000

    /** The longest reply label sent, and the longest reply text taken from a computer. */
    const val REPLY_LABEL_CAP: Int = 40
    const val REPLY_TEXT_CAP: Int = 2000

    /** The label when the app's reply action has no title of its own. */
    const val REPLY_LABEL: String = "Reply"

    /**
     * How long after a reply sent from a computer the app's own update of
     * that notification is taken as the answer to it, and not sent.
     */
    const val ABSORB_MS: Long = 5_000

    fun post(key: String, content: Content): String =
        buildJsonObject {
            put("cmd", "phone_notification")
            put("key", key)
            put("app", content.app)
            put("title", content.title)
            put("text", content.text)
            content.reply?.let { put("reply", it) }
        }.toString()

    fun removed(key: String): String =
        buildJsonObject {
            put("cmd", "phone_notification_removed")
            put("key", key)
        }.toString()

    /**
     * The key a notification is known by on the computer: a hash of Android's
     * key, which itself names the app, its user and its tag. 32 hex digits,
     * inside the computer's 64.
     */
    fun key(androidKey: String): String =
        MessageDigest.getInstance("SHA-256")
            .digest(androidKey.toByteArray(Charsets.UTF_8))
            .take(16)
            .joinToString("") { "%02x".format(it) }

    /** Whether [key] has the shape [key] gives: what a computer names back must. */
    fun validKey(key: String): Boolean = key.length == 32 && key.all { it in '0'..'9' || it in 'a'..'f' }

    /** At most [cap] characters, cut on a character, never inside a surrogate pair. */
    fun clip(raw: CharSequence?, cap: Int): String {
        val s = raw?.toString()?.trim() ?: return ""
        if (s.codePointCount(0, s.length) <= cap) return s
        return s.substring(0, s.offsetByCodePoints(0, cap)).trimEnd() + "…"
    }

    /**
     * The reply field's label for a notification with these [actions], or
     * null when none of them takes free-form text: a reply that only picks
     * from the app's canned choices is not one a computer can type.
     */
    fun replyLabel(actions: List<ReplyAction>): String? {
        val action = actions.firstOrNull { it.freeForm } ?: return null
        return clip(action.title, REPLY_LABEL_CAP).ifBlank { REPLY_LABEL }
    }

    /** Why a notification is not sent, or null when it is. */
    fun skip(f: Facts): Skip? = when {
        f.pkg == f.ownPkg -> Skip.OWN
        f.pkg !in f.allowed -> Skip.NOT_CHOSEN
        f.otherUser -> Skip.OTHER_PROFILE
        f.localOnly -> Skip.LOCAL_ONLY
        f.secret -> Skip.SECRET
        f.groupSummary -> Skip.GROUP_SUMMARY
        f.ongoing || f.foregroundService -> Skip.ONGOING
        f.media -> Skip.MEDIA
        else -> null
    }
}

/** What the listener reads off one notification, before anything is decided. */
data class Facts(
    val pkg: String,
    val ownPkg: String,
    /** The packages the person switched on. */
    val allowed: Set<String>,
    /** From a work profile or another user: never this person's to forward. */
    val otherUser: Boolean = false,
    /** The app asked for it to stay on this device (`FLAG_LOCAL_ONLY`). */
    val localOnly: Boolean = false,
    /** `VISIBILITY_SECRET`: the app hides even its existence on the lock screen. */
    val secret: Boolean = false,
    /** A group's summary, which repeats the group's children. */
    val groupSummary: Boolean = false,
    /** Ongoing (a download, navigation, a call in progress): a status, not news. */
    val ongoing: Boolean = false,
    val foregroundService: Boolean = false,
    /** A media player's controls, which the computer has its own of. */
    val media: Boolean = false,
)

/** What the listener reads off one of a notification's actions. */
data class ReplyAction(val title: CharSequence?, val freeForm: Boolean)

enum class Skip { OWN, NOT_CHOSEN, OTHER_PROFILE, LOCAL_ONLY, SECRET, GROUP_SUMMARY, ONGOING, MEDIA }

/**
 * What is shown. Built clipped, so an equal pair is an unchanged notification.
 * [reply] is the reply field's label, when the computer may offer one: a
 * change to it alone is a change too.
 */
data class Content(val app: String, val title: String, val text: String, val reply: String? = null) {
    val empty: Boolean get() = title.isBlank() && text.isBlank()

    companion object {
        fun of(app: CharSequence?, title: CharSequence?, text: CharSequence?, reply: String? = null): Content =
            Content(Mirror.clip(app, Mirror.APP_CAP), Mirror.clip(title, Mirror.TITLE_CAP), Mirror.clip(text, Mirror.TEXT_CAP), reply)
    }
}

/** One thing for the computer: a post or a removal. */
sealed interface MirrorItem {
    val key: String

    data class Post(override val key: String, val content: Content) : MirrorItem
    data class Removed(override val key: String) : MirrorItem
}

/**
 * What is waiting to go to ONE computer, and when it may.
 *
 * - The newest state per notification wins: a chat that updates ten times
 *   while the allowance is spent sends its last text, not its first, and
 *   never ten.
 * - An update identical to what the computer already shows is not sent.
 * - A removal goes only for a notification this computer was sent, and costs
 *   nothing; one for a post still waiting cancels the post instead.
 * - Posts are allowed [burst] at once and one more every [refillMs], stricter
 *   than the computer's own limit (10, then one every 3 s), so the computer
 *   never drops what the phone sends.
 * - At most [capacity] notifications wait; past that the oldest is dropped.
 * - After the person replied from the computer, the app's first update of
 *   that notification within [Mirror.ABSORB_MS] is the app showing the reply
 *   ("You: …"): it is taken as shown and not sent, because the computer
 *   already closed its copy and would pop it up again as news.
 * - A reply from ANOTHER computer closes this computer's copy too (the
 *   conversation was answered), and that same update of the app's within
 *   [Mirror.ABSORB_MS] is not sent here either.
 *
 * Not thread-safe: one sender per computer owns one of these.
 */
class MirrorOutbox(
    nowMs: Long,
    private val burst: Int = 8,
    private val refillMs: Long = 4_000,
    private val capacity: Int = 32,
    private val remembered: Int = 128,
) {
    private val waiting = LinkedHashMap<String, MirrorItem>()
    /** What the computer shows, by key, newest last. */
    private val shown = LinkedHashMap<String, Content>()
    /** The post being sent right now: a removal for it must still go. */
    private var inflight: String? = null
    /** Keys replied to from the computer, and until when their next update is the app's answer. */
    private val absorbing = LinkedHashMap<String, Long>()
    /** Keys replied to from another computer and closed here, and until when their next update is the app's answer. */
    private val answeredElsewhere = LinkedHashMap<String, Long>()
    private var tokens = burst
    private var at = nowMs

    val pending: Int get() = waiting.size

    fun offer(key: String, content: Content, nowMs: Long) {
        val closedUntil = answeredElsewhere.remove(key)
        if (closedUntil != null && nowMs <= closedUntil) return
        val until = absorbing.remove(key)
        if (until != null && nowMs <= until && key in shown) {
            waiting.remove(key)
            show(key, content)
            return
        }
        if (shown[key] == content) {
            waiting.remove(key)
            return
        }
        waiting.remove(key)
        waiting[key] = MirrorItem.Post(key, content)
        while (waiting.size > capacity) waiting.remove(waiting.keys.first())
    }

    fun remove(key: String) {
        waiting.remove(key)
        absorbing.remove(key)
        answeredElsewhere.remove(key)
        if (key in shown || key == inflight) waiting[key] = MirrorItem.Removed(key)
    }

    /** The connection dropped: nothing waits for the next one. What is shown is kept, so a later removal still lands. */
    fun drop() {
        waiting.clear()
        inflight = null
    }

    /**
     * The next item to send now, taken off the queue, or null when nothing may
     * go yet ([waitMs] says how long). Removals first: they are free.
     */
    fun next(nowMs: Long): MirrorItem? {
        refill(nowMs)
        waiting.values.firstOrNull { it is MirrorItem.Removed }?.let {
            waiting.remove(it.key)
            return it
        }
        if (tokens == 0) return null
        val post = waiting.values.firstOrNull() ?: return null
        waiting.remove(post.key)
        tokens -= 1
        inflight = post.key
        return post
    }

    /** How long until [next] can give a post, or 0 when nothing waits for one. */
    fun waitMs(nowMs: Long): Long {
        refill(nowMs)
        if (waiting.isEmpty() || tokens > 0) return 0
        return (refillMs - (nowMs - at)).coerceAtLeast(1)
    }

    /** [item] did not get there. Nothing is retried: the next update replaces it. */
    fun failed(item: MirrorItem) {
        if (inflight == item.key) inflight = null
    }

    /**
     * The person dismissed [key] on this computer. True when this computer
     * was sent it and shows it still; it then no longer does, so the phone's
     * own removal that follows is not sent back to it. A key it was never
     * sent is false: a computer dismisses only what it was given.
     */
    fun dismissedThere(key: String): Boolean {
        waiting.remove(key)
        absorbing.remove(key)
        answeredElsewhere.remove(key)
        return shown.remove(key) != null
    }

    /**
     * The person replied to [key] on this computer at [nowMs]. True when this
     * computer was sent it with a reply field and shows it still, so the
     * reply may go; the computer then closes its copy itself. The app's
     * update that follows is absorbed (see the class notes).
     */
    fun repliedThere(key: String, nowMs: Long): Boolean {
        if (shown[key]?.reply == null) return false
        absorbing.remove(key)
        absorbing[key] = nowMs + Mirror.ABSORB_MS
        while (absorbing.size > ABSORBING_KEPT) absorbing.remove(absorbing.keys.first())
        return true
    }

    /**
     * The person replied to [key] on another computer at [nowMs]. This one
     * closes its copy (a removal, when it was sent it or a post waits), and
     * the app's update that follows within [Mirror.ABSORB_MS] is not sent:
     * it shows the reply, which is not news here. False when this computer
     * has nothing of [key].
     */
    fun repliedElsewhere(key: String, nowMs: Long): Boolean {
        val had = key in shown || key == inflight || waiting[key] is MirrorItem.Post
        if (!had) return false
        remove(key)
        answeredElsewhere[key] = nowMs + Mirror.ABSORB_MS
        while (answeredElsewhere.size > ABSORBING_KEPT) answeredElsewhere.remove(answeredElsewhere.keys.first())
        return true
    }

    /** The reply to [key] found nothing to answer: no update of it is the app's answer, so none is held back. */
    fun replyFailed(key: String) {
        absorbing.remove(key)
        answeredElsewhere.remove(key)
    }

    /** The computer took [item]. */
    fun sent(item: MirrorItem) {
        if (inflight == item.key) inflight = null
        when (item) {
            is MirrorItem.Post -> show(item.key, item.content)
            is MirrorItem.Removed -> shown.remove(item.key)
        }
    }

    private fun show(key: String, content: Content) {
        shown.remove(key)
        shown[key] = content
        while (shown.size > remembered) shown.remove(shown.keys.first())
    }

    private fun refill(nowMs: Long) {
        val earned = ((nowMs - at) / refillMs).toInt()
        if (earned <= 0) return
        tokens = (tokens + earned).coerceAtMost(burst)
        at = if (tokens == burst) nowMs else at + earned * refillMs
    }

    private companion object {
        /** Replies outstanding at once; a person does not answer more than this in five seconds. */
        const val ABSORBING_KEPT = 16
    }
}
