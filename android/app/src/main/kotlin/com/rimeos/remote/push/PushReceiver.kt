package com.rimeos.remote.push

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import com.rimeos.remote.core.Base64Url
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.PushRegistration
import com.rimeos.remote.core.agent.Alert
import com.rimeos.remote.core.agent.Push
import com.rimeos.remote.phone.Ringer
import com.rimeos.remote.core.agent.UnifiedPush
import com.rimeos.remote.ui.Notifier
import com.rimeos.remote.data.MachineRepository

/**
 * Where a UnifiedPush distributor delivers (P1-058).
 *
 * ## What this class is allowed to decide
 *
 * Almost nothing. Every rule — the action and extra names, whether an endpoint
 * is usable, whether a message is a replay, what an envelope's bytes mean,
 * whether a failure is worth retrying — lives in `:core`, in [Push] and
 * [UnifiedPush], where `PushTest` can reach it. Nothing in this project can
 * test a `BroadcastReceiver`: there is no device, no emulator and no
 * Robolectric, and `assembleDebug` compiles this file without running it. A
 * rule written here would be a rule with no gate in front of it, which is the
 * same reason `Notifier` decides nothing about notification content.
 *
 * ## The one thing this file must get right on its own
 *
 * **It runs with the app not running and the phone probably locked.** So it
 * reads storage itself rather than through a `ViewModel` that does not exist
 * yet, it creates the notification channel itself rather than assuming
 * `MainActivity` ever ran, and it must not throw: an exception here is a
 * process death the user sees as the app crashing whenever a notification
 * arrives. Every parse returns null instead.
 *
 * `onReceive` is on the main thread with roughly ten seconds before the
 * process may be killed. Everything here is a small file read, one AEAD open
 * and one `notify`, so it stays well inside that — and deliberately does NOT
 * open a connection to the machine, which would not.
 */
class PushReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        // Wrapped whole. A distributor is another app sending broadcasts into
        // this process; a malformed one must not be able to crash it.
        runCatching { handle(context, intent) }
    }

    private fun handle(context: Context, intent: Intent) {
        val token = intent.getStringExtra(UnifiedPush.EXTRA_TOKEN)
        if (!UnifiedPush.isToken(token)) return
        when (intent.action) {
            UnifiedPush.ACTION_MESSAGE -> message(context, token!!, intent)
            UnifiedPush.ACTION_NEW_ENDPOINT -> endpoint(context, token!!, intent)
            UnifiedPush.ACTION_REGISTRATION_FAILED -> failed(context, token!!, intent)
            UnifiedPush.ACTION_UNREGISTERED -> unregistered(context, token!!)
            else -> Unit
        }
    }

    /**
     * One envelope.
     *
     * The acknowledgement goes first, before the notification is posted and
     * before anything that could fail. A distributor may retry a message it
     * was not acknowledged for, and — the part that matters more — may drop
     * the *endpoint* whose acknowledgement does not arrive within thirty
     * seconds. So an ack skipped because a later step threw costs future
     * notifications, and nobody would ever connect that to its cause.
     */
    private fun message(context: Context, token: String, intent: Intent) {
        UnifiedPush.ackFor(intent.getStringExtra(UnifiedPush.EXTRA_MESSAGE_ID))?.let {
            acknowledge(context, token, it)
        }
        val bytes = intent.getByteArrayExtra(UnifiedPush.EXTRA_BYTES_MESSAGE) ?: return
        val storage = MachineRepository(context)
        val store = storage.loadBlocking()
        val machine = store.machines.firstOrNull { it.push?.token == token } ?: return
        val registration = machine.push ?: return
        val key = Base64Url.decode(registration.key) ?: return

        // A successful open is the authentication: the key is this
        // registration's, so only the machine it was handed to can have sealed
        // this. Anything else is dropped without a word — a notification from
        // an envelope that did not authenticate is a notification anybody
        // could have caused.
        val body = Push.open(key, bytes) ?: return

        // A replay, or one that arrived after a newer envelope. The sequence
        // is persisted before the notification is posted, so a process killed
        // between the two shows the notification at most twice rather than
        // going on accepting the same envelope for ever.
        if (body.seq <= registration.lastSeq) return
        storage.updateBlocking { current ->
            val m = current.find(machine.deviceId) ?: return@updateBlocking current
            val seen = m.push ?: return@updateBlocking current
            current.with(m.copy(push = seen.copy(lastSeq = maxOf(seen.lastSeq, body.seq))))
        }

        if (body.kind == Alert.Kind.RING) {
            // `rime remote ring`, by push because the link was not open or the
            // ack was lost (contract §2.5). A late push does not ring, and the
            // same ring already rung over the link does not ring twice.
            Ringer(context).ringFromPush(machine.deviceId, machine.machine, body.started, System.currentTimeMillis() / 1000)
            return
        }

        if (body.kind == Alert.Kind.SENT) {
            // `rime remote send` while the link was down (contract §2.6). The
            // push carries nothing of what was sent; taking it needs the link,
            // which needs the person to unlock the app.
            com.rimeos.remote.phone.Receiver(context).waiting(machine.machine)
            return
        }

        val notifier = Notifier(context)
        // The channel may never have been created: this process may not have
        // run since the app was installed, and `MainActivity` is where it is
        // otherwise made. Idempotent, so calling it every time is free.
        notifier.ensureChannel()
        notifier.post(
            Alert(machine.deviceId, body.session, body.kind, body.agent, System.currentTimeMillis()),
            machine.machine,
        )
    }

    /**
     * A new endpoint for a machine.
     *
     * Only stored here. Telling the machine needs the Noise channel, which
     * needs the device key, which is behind a biometric gate this receiver
     * cannot satisfy — so the send happens on the next connection, from
     * [PushRegistrar.registerIfNeeded]. An endpoint stored but not yet sent is
     * exactly the state a phone is in between a distributor rotating its
     * endpoint and the user next opening the app, and it is why the stored
     * record is compared against what was sent rather than assumed current.
     */
    private fun endpoint(context: Context, token: String, intent: Intent) {
        val endpoint = UnifiedPush.checkEndpoint(
            intent.getStringExtra(UnifiedPush.EXTRA_ENDPOINT),
        ) ?: return
        UnifiedPush.ackFor(intent.getStringExtra(UnifiedPush.EXTRA_MESSAGE_ID))?.let {
            acknowledge(context, token, it)
        }
        MachineRepository(context).updateBlocking { store ->
            val machine = store.machines.firstOrNull { it.push?.token == token } ?: return@updateBlocking store
            val was = machine.push ?: return@updateBlocking store
            if (was.endpoint == endpoint) return@updateBlocking store
            // A new endpoint means a new place; the sequence belongs to the
            // old one and keeping it would make the machine's first envelope
            // to the new endpoint look like a replay if the machine's own
            // counter had been reset. It has not been — the desktop carries it
            // across a re-registration — so this only ever moves forward.
            store.with(machine.copy(push = was.copy(endpoint = endpoint)))
        }
    }

    /**
     * The distributor refused.
     *
     * The registration is left in place for a transient failure and cleared
     * for the three that need the user, so the app stops believing it has a
     * push path it does not have. Nothing is notified: a person who has not
     * asked for anything should not get a notification saying a component they
     * have never heard of is unhappy.
     *
     * Clearing it is also what makes the app recover on its own. The next
     * connection finds `push == null`, [PushRegistrar.registerIfNeeded] asks
     * the distributor again, and a user who has since signed in gets a working
     * endpoint without touching anything. Leaving the dead record in place
     * would make the failure permanent until the machine was forgotten.
     */
    private fun failed(context: Context, token: String, intent: Intent) {
        val failure = UnifiedPush.Failure(intent.getStringExtra(UnifiedPush.EXTRA_REASON))
        if (failure.isTransient) return
        clear(context, token)
    }

    private fun unregistered(context: Context, token: String) = clear(context, token)

    private fun clear(context: Context, token: String) {
        MachineRepository(context).updateBlocking { store ->
            val machine = store.machines.firstOrNull { it.push?.token == token } ?: return@updateBlocking store
            store.with(machine.copy(push = null))
        }
    }

    /**
     * Acknowledge one message to the distributor that sent it.
     *
     * Sent to the distributor's package explicitly. An implicit broadcast
     * would be delivered to every app that declared the action — and on API 26
     * and later would not be delivered at all.
     */
    private fun acknowledge(context: Context, token: String, id: String) {
        val distributor = Distributors.forToken(context, token) ?: return
        val ack = Intent(UnifiedPush.ACTION_MESSAGE_ACK).apply {
            `package` = distributor
            putExtra(UnifiedPush.EXTRA_TOKEN, token)
            putExtra(UnifiedPush.EXTRA_MESSAGE_ID, id)
        }
        runCatching { context.sendBroadcast(ack) }
    }
}

/**
 * Which app on this phone is the push distributor.
 *
 * ## Why this is a query and not a setting with a default
 *
 * UnifiedPush's whole point is that the user chooses who carries their
 * notifications. An app that silently picked the first distributor it found
 * would be choosing on their behalf which server sees the timing of every
 * alert — which is the one thing this design is careful about. So: none
 * installed is a state the UI explains, exactly one is offered, and more than
 * one is a choice the user makes.
 */
object Distributors {
    /**
     * The package to ask for a NEW registration, or `null` when there is none.
     *
     * Deliberately not remembered: the answer to "who should carry the next
     * registration" is about what is installed right now, and a remembered
     * package name goes stale the moment the user uninstalls one.
     *
     * With more than one installed this takes the first in a stable order
     * rather than asking. That is a choice and it is the small one — every
     * distributor sees the same thing, which is the timing of an alert and
     * nothing else, and the user can uninstall the one they do not want.
     */
    fun chosen(context: Context): String? = installed(context).firstOrNull()

    /**
     * The distributor that carries one registration, for acknowledging to.
     *
     * **This is not [chosen] and must not be.** A message is acknowledged to
     * the app that delivered it, and Android does not say which app sent a
     * broadcast — so the only correct answer is the one that was asked, which
     * is why [com.rimeos.remote.core.PushRegistration.distributor] is stored.
     * On a phone with two distributors installed, acknowledging to the wrong
     * one is silent: the message is redelivered, and the specification lets
     * the distributor drop the ENDPOINT whose acknowledgement does not arrive
     * within thirty seconds. Notifications then stop, days later, for a reason
     * nobody could connect to its cause.
     *
     * Falls back to [chosen] for a registration made before the field existed,
     * and for one whose distributor has since been uninstalled — in both cases
     * a guess is better than not acknowledging at all.
     */
    fun forToken(context: Context, token: String): String? {
        val stored = MachineRepository(context).loadBlocking().machines
            .firstOrNull { it.push?.token == token }
            ?.push
            ?.distributor
            .orEmpty()
        val available = installed(context)
        return when {
            stored.isNotEmpty() && available.contains(stored) -> stored
            else -> available.firstOrNull()
        }
    }

    /**
     * Every installed app that can be a distributor.
     *
     * Found by the receiver they must declare. Package visibility on API 30
     * and later filters this, which is why the manifest carries a `<queries>`
     * entry for the register action — without it this answers empty on a phone
     * that HAS a distributor, and refusal and absence are confused in the
     * direction that makes a working feature look broken.
     */
    fun installed(context: Context): List<String> {
        val intent = Intent(UnifiedPush.ACTION_REGISTER)
        val pm = context.packageManager
        val found = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.queryBroadcastReceivers(
                intent,
                PackageManager.ResolveInfoFlags.of(0L),
            )
        } else {
            @Suppress("DEPRECATION")
            pm.queryBroadcastReceivers(intent, 0)
        }
        return found.mapNotNull { it.activityInfo?.packageName }.distinct().sorted()
    }
}

/**
 * Asking a distributor for an endpoint, and telling a machine about it.
 *
 * Separate from the receiver because the two halves happen at different times
 * and in different processes: the ask is a broadcast this app sends while it
 * is open, and the answer arrives later at [PushReceiver].
 */
object PushRegistrar {

    /**
     * Ask the distributor to register this machine, if it is not already.
     *
     * ## Idempotent, and only because it ignores its own argument
     *
     * `machine` says WHICH machine and nothing else. Whether one is already
     * registered is read from storage every time, and the reason is that a
     * `PairedMachine` held by a screen is a snapshot: [sent] and
     * `PushReceiver`'s `NEW_ENDPOINT` handler both write the record directly,
     * and nothing refreshes the view model's list afterwards. Trusting the
     * argument means every reconnection with a stale copy in hand mints a new
     * token and a NEW KEY over the live one — after which the machine's
     * envelopes no longer decrypt, silently, and the distributor is left
     * holding an orphaned instance per attempt.
     *
     * The re-check inside `update` closes the same hole against the receiver
     * writing between the read and the write.
     *
     * @return the token in use for this machine, or `null` when there is no
     *   distributor and push is therefore not available on this phone.
     */
    fun registerIfNeeded(context: Context, machine: PairedMachine): String? {
        val storage = MachineRepository(context)
        storage.loadBlocking().find(machine.deviceId)?.push?.let { return it.token }
        val distributor = Distributors.chosen(context) ?: return null
        val token = UnifiedPush.newToken()
        var created = false
        storage.updateBlocking { store ->
            val current = store.find(machine.deviceId) ?: return@updateBlocking store
            // Somebody registered between the read above and this write. Their
            // token is the live one; ours has been broadcast to nobody.
            if (current.push != null) return@updateBlocking store
            created = true
            store.with(
                current.copy(
                    push = PushRegistration(
                        token = token,
                        // Empty until the distributor answers. A record with no
                        // endpoint is what "asked and waiting" looks like, and
                        // it is distinguishable from "never asked", which is
                        // `null`.
                        endpoint = "",
                        key = Base64Url.encode(Push.newKey()),
                        // Recorded BEFORE the broadcast goes out, so a message
                        // that arrives before this returns still finds the
                        // package to acknowledge to.
                        distributor = distributor,
                    ),
                ),
            )
        }
        if (!created) return storage.loadBlocking().find(machine.deviceId)?.push?.token
        val register = Intent(UnifiedPush.ACTION_REGISTER).apply {
            `package` = distributor
            putExtra(UnifiedPush.EXTRA_TOKEN, token)
            putExtra(UnifiedPush.EXTRA_APPLICATION, context.packageName)
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                // Below API 34 the specification requires a PendingIntent so
                // the distributor can identify the caller. From 34 the
                // broadcast's own FLAG_SHARE_IDENTITY does it.
                putExtra(
                    UnifiedPush.EXTRA_PI,
                    PendingIntent.getBroadcast(
                        context,
                        0,
                        Intent(context, PushReceiver::class.java),
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                    ),
                )
            }
        }
        runCatching { context.sendBroadcast(register) }
        return token
    }

    /**
     * Record that a machine has been told an endpoint.
     *
     * Written only after the machine answered `ok`. Writing it optimistically
     * would make the one failure that matters permanent: a send that did not
     * land would look sent, and [Push.registerLine] would answer `null` for
     * ever afterwards — a phone that believes it has a push path and never
     * rings, with nothing anywhere saying so.
     */
    fun sent(context: Context, machine: PairedMachine, endpoint: String) {
        MachineRepository(context).updateBlocking { store ->
            val current = store.find(machine.deviceId) ?: return@updateBlocking store
            val push = current.push ?: return@updateBlocking store
            store.with(current.copy(push = push.copy(sentEndpoint = endpoint)))
        }
    }

    /**
     * Forget this phone's push registration for one machine.
     *
     * The distributor is told as well as the store being cleared, and that
     * second half is the one worth stating: without it the distributor keeps
     * the instance, the push server keeps the subscription, and a machine that
     * was never told to stop — one that was forgotten while unreachable, say —
     * goes on posting to an endpoint nobody reads. Unregistering is the only
     * thing on this phone that can end that.
     */
    fun forget(context: Context, machine: PairedMachine) {
        val storage = MachineRepository(context)
        // From storage and not from the argument, for the reason
        // [registerIfNeeded] gives: a screen's `PairedMachine` predates every
        // write the receiver has made, so a phone that registered since the
        // list was loaded would have its token read as null and the
        // distributor would never be told to stop.
        val token = storage.loadBlocking().find(machine.deviceId)?.push?.token
        val distributor = token?.let { Distributors.forToken(context, it) }
        storage.updateBlocking { store ->
            val current = store.find(machine.deviceId) ?: return@updateBlocking store
            store.with(current.copy(push = null))
        }
        if (token == null || distributor == null) return
        val unregister = Intent(UnifiedPush.ACTION_UNREGISTER).apply {
            `package` = distributor
            putExtra(UnifiedPush.EXTRA_TOKEN, token)
        }
        runCatching { context.sendBroadcast(unregister) }
    }
}
