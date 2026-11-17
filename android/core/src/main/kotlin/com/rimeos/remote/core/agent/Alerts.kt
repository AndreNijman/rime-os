package com.rimeos.remote.core.agent

/**
 * What is worth telling the user about, and once.
 *
 * ## There are now TWO ways an alert arrives, and this file is one of them
 *
 * This class turns **polls of a connection the phone opened** into alerts, and
 * that path works only while the app is running. The other is push: since
 * P1-058 the desktop runs the same rules itself, in
 * `rimed/rime-remoted/src/push.rs`, and posts an opaque envelope to a
 * UnifiedPush distributor — which reaches this phone with the app closed, the
 * screen off and the network suspended in Doze. See [Push] for the envelope
 * and `docs/remote.md` for what the user has to install.
 *
 * The two paths raise the *same* alerts, and that is deliberate rather than
 * redundant: push needs a distributor the user may not have, and the poll
 * needs an app that may not be running. What stops the overlap showing up as
 * two notifications is [NotificationContent.idFor] — the id is derived from
 * (machine, session, kind), so a pushed alert and a polled alert for one
 * moment REPLACE each other in the shade instead of stacking.
 *
 * **The earlier text here said there was no push transport anywhere in Rime,
 * and that the relay had never been deployed. Both were true when written and
 * neither is now**: the relay is serving at `apex-relay.andrenijman.com`, and (rime-rename: keep)
 * the push path above exists. The relay is still not what carries a push, and
 * the reason is worth keeping — it is a rendezvous that both ends dial into at
 * the same time, which is precisely the situation push exists to handle the
 * absence of.
 *
 * ## What must never be in an alert
 *
 * `SessionInfo.detail` is built by `hook::detail_for` and copies **verbatim**,
 * clipped to 120 characters: Bash command lines, file paths, grep patterns,
 * fetched URLs, task descriptions, and Claude's own notification text. It is
 * the single most sensitive field on the record and it is never carried here.
 * Nor are `cwd`, `project`, `worktree`, `args` or `telemetry.branch`, which
 * name what the user is working on.
 *
 * An [Alert] is `(machine, session id, kind, agent)`. The words the user reads
 * come from [AgentStates.label] and this file, both of which are fixed strings
 * in this build.
 */
data class Alert(
    /** Which machine. A paired device id, opaque and already known to the phone. */
    val machine: String,
    /** The session, within that machine. See [Key] for why it is not enough alone. */
    val session: Int,
    val kind: Kind,
    /** The adapter: `claude`, `codex`. Not the program, not the prompt. */
    val agent: String,
    /** Unix **milliseconds** the alert was raised on the phone. */
    val atMs: Long,
) {
    /**
     * What happened, in the vocabulary P1-058 asks for.
     *
     * Three of the six criteria map onto a session state the daemon publishes;
     * the other three do not, and are named here as what they actually are.
     */
    enum class Kind(val title: String, val detail: String) {
        /**
         * The agent is waiting for the user.
         *
         * **This is also what "working complete" arrives as**, and that is a
         * property of the runtime rather than a simplification here.
         * `AgentState::Complete` comes only from the agent process exiting 0
         * (`session.rs:493`), and the `claude` adapter runs claude
         * interactively — the prompt is a trailing argument, not `-p` — so a
         * managed session reaches `complete` only when a human quits claude.
         * The end of a turn publishes `waiting_for_user`, via the `stop` hook.
         * A separate "finished" alert would therefore either never fire or
         * fire for every turn under a name that means something else.
         */
        WAITING("Waiting for you", "The agent finished its turn and wants input."),

        /** The agent is asking for permission for something it wants to do. */
        PERMISSION("Needs permission", "The agent is asking before it acts."),

        /** The session's process exited non-zero. */
        FAILED("Failed", "The session exited with an error."),

        /** The session's process exited 0, or was ended. */
        FINISHED("Finished", "The session ended."),

        /**
         * A test run Rime observed in one of this machine's worktrees failed.
         *
         * Not a session state: it reaches the daemon as a `TestNote` on a hook
         * event and is stored per worktree path, surfacing on
         * `WorktreeStatus.tests` rather than on any session. So this kind is
         * raised from a `worktrees` poll, not from `list`, and its `session`
         * is the first session the daemon attributes to that worktree — or
         * [NO_SESSION] when it attributes none.
         */
        TEST_FAILED("Tests failed", "The last test run in a worktree failed."),

        /**
         * A privileged operation is waiting for a decision.
         *
         * The phone cannot make that decision: see
         * [PrivilegeRequest.DECIDE_IS_LOCAL_ONLY]. The alert exists so the user
         * knows to go to the machine, which is the only thing it can usefully
         * cause.
         */
        APPROVAL("Approval needed", "A privileged operation is waiting at the machine."),

        /**
         * A deployment finished, and it worked.
         *
         * P1-058's first criterion asks for deployment notifications, and the
         * previous round recorded — correctly at the time — that nothing on
         * this socket could raise one. The source now named is real and was
         * already on the wire: a `PrivilegeRequest` for one of the five verbs
         * that change what the machine boots or what is installed on it —
         * `update`, `rollback`, `pin`, `pkg_rebuild`, `pkg_rollback` — acquires
         * an `executed_ms` when the operation ran, and an `exit_code` saying
         * how it went.
         *
         * `install` and `remove` deliberately do NOT raise one. Their
         * notification is the [APPROVAL], and a second on completion would
         * double every package operation.
         *
         * What this does not cover, stated so nobody reads more into it: an
         * agent deploying somebody else's software by running a command in a
         * terminal. Nothing on this socket can see that — it is a Bash tool
         * call, and the only record of it is `SessionInfo.detail`, which is
         * the field this whole design exists to keep off a push server.
         */
        DEPLOYED("Deployed", "A deployment on the machine finished."),

        /** A deployment finished with a non-zero exit code. */
        DEPLOY_FAILED("Deployment failed", "A deployment on the machine did not complete."),

        /**
         * nex is waiting for somebody to approve or deny what it wants to do.
         *
         * Raised only by the machine's push watcher: the in-app watcher has
         * no need of it, because the nex screen follows nexd live while it is
         * open. A nex session id is a string the push body has no room for,
         * so this alert names no session; tapping it opens the nex screen,
         * which reads what is waiting over the paired link. What the tool
         * would do never reaches the shade.
         */
        NEX_APPROVAL("nex needs a decision", "Open nex to approve or deny what it wants to do."),
    }

    /**
     * What two alerts must share to be the same alert.
     *
     * **The duplicate this deduplicates is internal to Rime, not "Claude
     * versus Rime".** Four independent paths publish `waiting_for_user` for
     * one moment: the `notification` hook, the `stop` hook, the PTY scanner
     * seeing a BEL or an OSC 9/777, and the ten-second idle rule
     * (`session.rs:484`). `Session::set_state` (`registry.rs:163`) has **no
     * same-state early return** — it assigns unconditionally and bumps
     * `last_activity` — so a poller keying on "saw waiting_for_user" fires up
     * to four times for one turn, seconds apart. And `SessionInfo` carries no
     * field naming the hook event that caused the transition, so the four are
     * not distinguishable at the client even in principle.
     *
     * Dedup is therefore on the **transition edge**: an alert is raised when a
     * session ENTERS a state, and not again while it stays there. That is the
     * only rule available, and it happens to be the right one.
     *
     * The machine is part of the key because `SessionInfo.id` is a per-daemon
     * counter: two paired machines both have a session 1.
     */
    data class Key(val machine: String, val session: Int, val kind: Kind)

    val key: Key get() = Key(machine, session, kind)

    companion object {
        /** A test-failure alert for a worktree the daemon attributes no session to. */
        const val NO_SESSION: Int = -1
    }
}

/**
 * Turns successive polls into alerts, raising each transition once.
 *
 * Deliberately a plain class in `:core` with no Android type in it: everything
 * that can be a rule lives where a JVM test can reach it, because nothing in
 * this project can test a Compose callback or a `NotificationManager`.
 *
 * **Not thread-safe, and it does not need to be.** It is driven from one
 * polling loop. A caller that drives it from two would get interleaved state,
 * which is why [observe] takes the whole world each time rather than one
 * session at a time.
 */
class AlertWatcher {
    /** The state each session was last seen in, per machine. */
    private val lastState = mutableMapOf<Pair<String, Int>, String>()

    /** Worktree path -> the test state last seen there, per machine. */
    private val lastTests = mutableMapOf<Pair<String, String>, String>()

    /** Privilege request ids already announced, per machine. */
    private val announced = mutableSetOf<Pair<String, Int>>()

    /** Whether anything has been observed yet for a machine. */
    private val seen = mutableSetOf<String>()

    /**
     * Fold one poll of one machine into alerts.
     *
     * **The first poll of a machine raises nothing.** A phone opening the app
     * to six sessions that have been waiting since yesterday must not fire six
     * notifications for things that did not just happen; a notification means
     * "this changed", and on the first look nothing has. Recorded as a rule
     * rather than as an accident of an empty map, because an empty map would
     * also be the state after a process restart — which is exactly when the
     * six-notification burst would happen.
     *
     * @param nowMs the clock, passed in because a test cannot pin one it does
     *   not hold.
     */
    fun observe(
        machine: String,
        sessions: List<AgentSession>,
        worktrees: List<WorktreeStatus> = emptyList(),
        requests: List<PrivilegeRequest> = emptyList(),
        nowMs: Long = 0,
    ): List<Alert> {
        val first = seen.add(machine)
        val out = mutableListOf<Alert>()

        for (s in sessions) {
            val at = machine to s.id
            val was = lastState.put(at, s.state)
            if (first || was == s.state) continue
            val kind = kindOf(s) ?: continue
            out += Alert(machine, s.id, kind, s.agent, nowMs)
        }

        // A session the daemon has forgotten must not keep its old state in
        // this map: `Request::Remove` and `Prune` make the id reusable
        // (`registry.rs:441`), and a new session landing on a recycled id
        // would be compared against a stranger's state.
        val live = sessions.map { machine to it.id }.toSet()
        lastState.keys.retainAll { it.first != machine || it in live }

        for (w in worktrees) {
            val at = machine to w.path
            val was = lastTests.put(at, w.tests.state)
            if (first || was == w.tests.state || !w.tests.isFailed) continue
            out += Alert(
                machine,
                w.sessions.firstOrNull() ?: Alert.NO_SESSION,
                Alert.Kind.TEST_FAILED,
                sessions.firstOrNull { it.id in w.sessions }?.agent ?: "",
                nowMs,
            )
        }

        for (r in requests) {
            if (!r.isPending) {
                // Decided elsewhere. Forgotten so that a request re-opened by
                // a daemon restart announces itself again.
                announced.remove(machine to r.id)
                continue
            }
            if (!announced.add(machine to r.id)) continue
            if (first) continue
            out += Alert(machine, r.session ?: Alert.NO_SESSION, Alert.Kind.APPROVAL, r.agent ?: "", nowMs)
        }

        return out
    }

    /** Forget a machine, so the next poll of it is a first poll again. */
    fun forget(machine: String) {
        seen.remove(machine)
        lastState.keys.retainAll { it.first != machine }
        lastTests.keys.retainAll { it.first != machine }
        announced.retainAll { it.first != machine }
    }

    private fun kindOf(s: AgentSession): Alert.Kind? = when (s.state) {
        AgentStates.WAITING_FOR_USER -> Alert.Kind.WAITING
        AgentStates.PERMISSION_REQUEST -> Alert.Kind.PERMISSION
        AgentStates.FAILED -> Alert.Kind.FAILED
        AgentStates.COMPLETE, AgentStates.EXITED -> Alert.Kind.FINISHED
        // `starting` and `working` are not news.
        else -> null
    }
}
