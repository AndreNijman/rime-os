package com.rimeos.remote.core.agent

/**
 * Voice, clipboard and file handoff from the phone to an agent (P1-059).
 *
 * Three of P1-059's four criteria are about pushing something the person is
 * holding — a sentence they said, something they copied, a photo they took —
 * into an agent that is already running. One verb carries all of it that this
 * protocol can carry, and one of the three has no transport at all. This file
 * is where each of those is decided, and where the one that cannot be done
 * says so rather than being approximated.
 *
 * ## The verb, and the fact that it appends nothing
 *
 * `Request::Input { id, data }` writes `data` to the session's PTY master with
 * `session::write_input`, which the protocol documents as writing it "verbatim
 * and no byte is added". Two consequences run through everything below.
 *
 * **Bytes on a PTY are keystrokes.** There is no field in a terminal for "this
 * came from a phone". A `\n` in the middle of pasted text is not text; it is
 * the return key, pressed at that point, and everything after it is typed into
 * whatever the agent asked next. An `ESC` is not a character; it is the first
 * byte of whatever sequence follows it. `Agentd.escape` carries control bytes
 * through the JSON faithfully, so an escaped 0x1b arrives as one 0x1b byte — the
 * escaping is correct and is precisely why the hazard survives to the terminal.
 *
 * **A machine's guess is read by a human before it becomes an instruction —
 * and on a phone that happens BEFORE the send, not after.** The desktop stages:
 * `rime agent input <id> <text>` takes a `--submit` flag and
 * `PushToTalkService.qml` does not pass it, so the words wait on the agent's
 * input line for the person sitting at that machine to press Enter. The rule is
 * right and the placement is right *there*, because the reviewer is at the
 * keyboard.
 *
 * The phone's user is not. Words staged on a machine nobody is standing at are
 * words in a terminal nobody will look at until they walk back to it, and a
 * push-to-talk whose result appears twenty minutes later at a desk is not the
 * feature anybody asked for. So the review happens where the person is: a
 * transcript and a clipboard are put into the reply box, on the phone, where
 * they can be read and edited, and the Send button is the same one a typed
 * reply uses.
 *
 * That is why [Voice.forReview] and [Clipboard.forReview] produce text for a
 * text field and not bytes for a socket, and why there is exactly one path to
 * the wire: [com.rimeos.remote.core.agent.Reply.bytes], through
 * `Reply.check`'s recycled-id guard. A second send path would be a second
 * place for that guard to be forgotten, and forgetting it types somebody's
 * sentence into a stranger's terminal.
 */
object Handoff {

    // ── Push to talk ────────────────────────────────────────────────────────

    /**
     * Speaking to a named agent.
     *
     * The rules are the desktop's, from `rime-shell/src/services/pushtotalk.js`,
     * and they are reproduced rather than reinvented so that the same feature
     * does not mean two things on two clients. Two of them are the whole design:
     *
     * 1. **The target is resolved before the microphone opens.** A refusal keeps
     *    the phase at [Phase.IDLE]. There is no reachable path on which the
     *    phase is [Phase.RECORDING] and the target is null — "a recording with
     *    nowhere to go is a hot microphone with a reason to feel fine about
     *    itself".
     * 2. **The target is frozen when recording starts.** A person talking to a
     *    computer looks away from it; a route re-resolved at delivery would
     *    send the words wherever the phone had drifted to.
     *
     * ## This app does not hold the microphone, and that is the design
     *
     * Speech reaches it through `RecognizerIntent.ACTION_RECOGNIZE_SPEECH`:
     * the system's own recogniser opens, shows the system's own microphone
     * indicator, and hands back text. **`RECORD_AUDIO` is not in the
     * manifest** and no audio ever enters this process. That is the same rule
     * P1-059's fourth criterion applies to the photo library, applied to the
     * microphone — take the Android-mediated selection, never the blanket
     * permission — and it is worth more here than convenience, because this is
     * the app that shows which machine is about to run something as root.
     *
     * The desktop's third rule is therefore inherited with its reason changed
     * rather than copied. `pushtotalk.js` needs [MAX_MS] because push-to-talk
     * there is a *toggle*: niri fires nothing on key release, so hold-to-talk
     * is unimplementable on one of its three compositors and one semantic
     * everywhere had to be the toggle, and a toggle's failure is a microphone
     * left open. This app cannot leave a microphone open — it never had one.
     *
     * The cap is kept at the same ninety seconds anyway, because the failure it
     * prevents arrives here by a different road: a result that never comes.
     * The recogniser is another process, and another process can be killed,
     * swiped away, or replaced while this app is in the background. Without the
     * cap that leaves push-to-talk in [Phase.RECORDING] with a destination held
     * and no event that will ever leave it, which reads to the user as a mic
     * button that has stopped working. [expired] is what unsticks it.
     *
     * This reducer is agnostic about what produced [stop] — a returning
     * activity result, a second tap, the cap — and is not agnostic about
     * something having produced it.
     */
    object Voice {

        /**
         * Longest single recording, in milliseconds.
         *
         * Ninety seconds, agreeing with `pushtotalk.js`'s `MAX_MS`. Longer than
         * anything a person says in one breath, short enough that a microphone
         * left open is an embarrassment and not an incident.
         */
        const val MAX_MS: Long = 90_000L

        /** Where push-to-talk is in its cycle. */
        enum class Phase {
            IDLE,

            /** The microphone is open. The target is already decided. */
            RECORDING,

            /** Audio captured, words not yet out. Not interruptible. */
            TRANSCRIBING,

            /** Words in hand, `input` in flight. Not interruptible. */
            DELIVERING,

            /** Something refused, and [State.error] says what. */
            ERROR,
        }

        /**
         * The whole of what push-to-talk knows.
         *
         * [target] is null in exactly two phases — [Phase.IDLE] and
         * [Phase.ERROR] — and `targetIsHeldWhileTheMicrophoneIsOpen` asserts
         * it, because that invariant is the feature.
         */
        data class State(
            val phase: Phase = Phase.IDLE,
            val target: Reply.Target? = null,
            /** What the user is told they are talking to. Display only. */
            val label: String = "",
            /** The transcript, once there is one. */
            val text: String = "",
            /** When [Phase.RECORDING] began, for [remainingMs]. */
            val startedAt: Long = 0L,
            val error: String = "",
        )

        /**
         * The one refusal that is about this phone rather than about the
         * session.
         *
         * Everything else push-to-talk can refuse is something
         * [com.rimeos.remote.core.agent.Reply.check] already decides, and
         * [route] defers to it rather than keeping a second vocabulary for the
         * same facts. Two lists of reasons a message cannot be delivered is how
         * one of them ends up saying something the other does not.
         */
        enum class Refusal(val message: String) {
            /**
             * Nothing on this phone answers `ACTION_RECOGNIZE_SPEECH`.
             *
             * The only device-side refusal there is, because this app asks for
             * no microphone permission and so has none to be denied. A phone
             * with no recogniser installed — or one whose recogniser has been
             * disabled — is told that, not sent to a permission screen where it
             * would find nothing.
             *
             * **Raised from a thrown `ActivityNotFoundException`, never from a
             * `resolveActivity` that answered null.** On API 30 and later,
             * package visibility makes `resolveActivity` return null for an
             * intent this app has not declared a `<queries>` entry for — so a
             * pre-check would report "no recogniser" on a phone that has one,
             * which is refusal and absence confused in the direction that makes
             * a working feature look broken. The manifest declares the
             * `<queries>` entry as well, and `ManifestTest` asserts it.
             */
            NO_RECOGNISER(
                "This phone has no speech recognition installed, so there is nothing to turn " +
                    "speech into text. Type instead.",
            ),
        }

        /** A destination, or the sentence explaining why there is none. */
        data class Route(
            val target: Reply.Target?,
            /** What the user is told they are talking to. Display only. */
            val label: String,
            /** Why not, ready to show. Null when [target] is set. */
            val why: String?,
        )

        /**
         * Which agent the words would go to, resolved **before** anything opens
         * the recogniser.
         *
         * The destination is the session on screen, which is what "target the
         * selected agent" means in an app whose Session screen *is* the
         * selection. There is deliberately no picker and no fallback: the
         * desktop routes to "the active project or the focused agent session"
         * because a compositor knows what is focused, and a phone showing one
         * session already knows, so a second rule guessing between several
         * would be a rule with nothing to decide.
         *
         * Every refusal comes from [Reply.check], not from a vocabulary of its
         * own — the session pruned, the id recycled, the agent exited or
         * paused. That is the same guard a typed reply passes, applied one step
         * earlier so the recogniser never opens for a session that has gone.
         */
        fun route(
            machine: String,
            session: AgentSession,
            live: List<AgentSession>,
        ): Route {
            // `started` is on the identity and the id alone is not: the daemon
            // reuses a session number after a prune (`registry.rs:441`), so a
            // route that carried only the number could deliver a dictated
            // sentence into a stranger's terminal. Same three values
            // [Reply.Target] carries for a typed reply, for the same reason.
            val target = Reply.Target(machine, session.id, session.started)
            Reply.check(target, machine, live)?.let { return Route(null, "", it.message) }
            return Route(target, session.routeLabel, null)
        }

        /**
         * Open the microphone, or refuse and stay shut.
         *
         * [route] is called by the caller and its answer passed in, so that a
         * screen can show the destination before a finger goes down; this
         * function re-checks it rather than trusting that it was checked,
         * because the invariant is worth more than the duplicated branch.
         */
        fun start(state: State, route: Route, now: Long): State {
            // Not interruptible, and a second press while words are in flight
            // must not start a new recording on top of them.
            if (state.phase == Phase.TRANSCRIBING || state.phase == Phase.DELIVERING) return state
            val target = route.target ?: return State(
                phase = Phase.ERROR,
                error = route.why ?: "There is nothing on screen to talk to.",
            )
            return State(
                phase = Phase.RECORDING,
                target = target,
                label = route.label,
                startedAt = now,
            )
        }

        /**
         * The finger came up, or the second tap arrived, or [MAX_MS] ran out.
         *
         * All three are one event. The target is carried forward untouched —
         * that is what "frozen" means.
         */
        fun stop(state: State): State =
            if (state.phase != Phase.RECORDING) state
            else state.copy(phase = Phase.TRANSCRIBING)

        /**
         * Whether an open microphone has outstayed [MAX_MS].
         *
         * Checked by the caller on a timer; a reducer cannot see a clock.
         */
        fun expired(state: State, now: Long): Boolean =
            state.phase == Phase.RECORDING && now - state.startedAt >= MAX_MS

        /** How long an open microphone has left, for the indicator. */
        fun remainingMs(state: State, now: Long): Long =
            if (state.phase != Phase.RECORDING) 0L
            else (MAX_MS - (now - state.startedAt)).coerceAtLeast(0L)

        /**
         * The recogniser answered.
         *
         * An empty or blank transcript is not a delivery. Sending it would put
         * nothing in the agent's input line while the phone said it had spoken,
         * and the user would never learn that the room was too loud.
         */
        fun transcribed(state: State, text: String): State {
            if (state.phase != Phase.TRANSCRIBING) return state
            if (text.isBlank()) {
                return State(
                    phase = Phase.ERROR,
                    error = "Nothing was heard, so nothing was sent.",
                )
            }
            return state.copy(phase = Phase.DELIVERING, text = text)
        }

        /** `input` was accepted. */
        fun delivered(state: State): State =
            if (state.phase != Phase.DELIVERING) state else State()

        /**
         * Something refused.
         *
         * Accepted from any phase: an error can arrive from the recorder, the
         * recogniser or the socket, and a guard that only fired on one of them
         * would strand the microphone open in a phase nothing could leave.
         */
        fun failed(state: State, why: String): State =
            State(phase = Phase.ERROR, error = why.ifBlank { "Push-to-talk failed." })

        /** Back to idle from [Phase.ERROR], when the user has read it. */
        fun dismiss(state: State): State =
            if (state.phase == Phase.ERROR) State() else state

        /** Whether the indicator should show an open microphone. */
        fun micOpen(state: State): Boolean = state.phase == Phase.RECORDING

        /**
         * Whether a returning transcript belongs to what is on screen now.
         *
         * The frozen target, surviving the move of the review step onto the
         * phone. The recogniser is **another activity**: this one is stopped
         * while it runs, and what comes back arrives at whatever the user has
         * since navigated to. Filling the reply box without this check would
         * put one agent's dictated answer into another agent's box, where the
         * person would read it as their own words and press Send.
         *
         * Compares all three values, not the id: ids are reused after a prune,
         * so a transcript that returns to the same NUMBER on a different
         * session must be refused too.
         *
         * @param onScreen the target the reply box would send to now, or null
         *   when nothing is on screen to send to.
         */
        fun landsOn(state: State, onScreen: Reply.Target?): Boolean =
            state.target != null && state.target == onScreen

        /**
         * Shown when it does not.
         *
         * Says the words were kept out rather than that something failed: the
         * transcript is discarded, and a person who has just spoken a sentence
         * needs to know it is gone rather than wonder where it went.
         */
        const val LANDED_ELSEWHERE: String =
            "That was dictated for a different agent, so it has not been put in this box. " +
                "Go back to that session and say it again."

        /**
         * What a finished transcript becomes in the reply box.
         *
         * Text for a text field, never bytes for a socket. A transcript is a
         * machine's guess at what a person said, and this is the step where a
         * person reads the guess — so it carries **no terminator**, because a
         * string that arrived in an editable box already submits nothing.
         * Pressing Send is what submits, and Send goes through [Reply.bytes]
         * and `Reply.check` like every other reply.
         *
         * The trailing space is not padding. The words land where somebody will
         * keep typing, and a caret jammed against the last word is the
         * difference between adding a clause and editing one.
         */
        fun forReview(text: String): String = text.trim() + " "
    }

    // ── Clipboard ───────────────────────────────────────────────────────────

    /**
     * Sending what is on the phone's clipboard to an agent.
     *
     * ## Both directions are real now, and they are different code paths
     *
     * **Receive** — what the COMPUTER copied, onto this phone — is the
     * `clipboard` verb: [Agentd.clipboard] builds it, [Agentd.readClipboard]
     * reads the answer, [MachineLink.clipboard] carries it, and
     * `RemoteViewModel.pullMachineClipboard` puts the result on the phone's
     * clipboard from the Agent Center. It is **not in this object**, because
     * it is not a handoff into a session: it carries no session id, one
     * Wayland seat having one clipboard, and it is offered as a machine
     * action beside Projects rather than from the reply box.
     *
     * Everything below is the **send** direction: this phone's clipboard into
     * a waiting agent, through `input`. The two are one criterion pointing
     * opposite ways, and the earlier version of this comment — which stated
     * that no verb on this socket read the machine's clipboard, and that
     * P1-059's third criterion was "half met" — was true when it was written
     * and is not true now. Both sentences are kept here, struck, because the
     * reason they were wrong is the trap: `rime send --clipboard` is a
     * genuinely different feature wearing a similar name
     * (`rimed/rime/src/dispatch.rs` shells out to `wl-paste`, ssh's the bytes
     * to another **Linux host in the §20 registry** and runs `wl-copy`
     * there), and three rounds of work read the similar name as the same
     * feature and concluded the receive half was impossible.
     *
     * The terminal's own select-and-copy still exists and still covers "get
     * that command off the machine". What it never covered was "get what the
     * machine copied", and that is the gap the verb closes.
     *
     * ## Why a paste is not a reply
     *
     * `Reply.bytes` trims the ends and appends CR, which is right for a
     * sentence someone typed into a box. A clipboard is not that. It holds
     * whatever was last copied — a stack trace, a URL, a chunk of a file — and
     * `Input` writes it to a PTY **verbatim**. Every interior newline is a
     * press of the return key at that point, so a three-line paste submits the
     * first line and types the other two into whatever the agent asked next. An
     * `ESC` is the start of a control sequence, and TUIs act on those.
     *
     * The daemon does not help here and could not: `inject` wraps its path in
     * bracketed-paste markers when `OutputScanner` has seen the application set
     * DECSET 2004, because the daemon *is* the terminal and knows. `input`
     * brackets nothing, and a phone has no way to learn whether the mode is on.
     *
     * ## [inspect] runs on the BOX, not on the clipboard
     *
     * This is the placement that matters and the first attempt got it wrong.
     * A rule that only ran when the user tapped a *Send clipboard* button
     * would be bypassed by the ordinary way anybody pastes: long-press the
     * reply field, tap Paste. The IME writes straight into the text field,
     * no code of this app's runs, and the three-line stack trace goes to
     * [Reply.bytes] — which trims the ends, adds CR, and submits at the first
     * interior newline with the remaining two lines typed into whatever the
     * agent asks next.
     *
     * So [inspect] is a gate on **the send**, applied to whatever is in the box
     * however it got there — typed, dictated, pasted by the keyboard, or put
     * there by this app's own button. The button is a convenience over a rule
     * the send applies anyway, which is the only arrangement in which the rule
     * cannot be walked around.
     *
     * Nothing is silently stripped. Text somebody chose to paste is text they
     * chose; a send that quietly delivered something else would be worse than
     * one that stopped and asked.
     */
    object Clipboard {

        /**
         * What typing this clipboard into a live PTY would do.
         *
         * @param lines how many lines the content would be typed as. One means
         *   the whole paste lands on the input line.
         * @param submits true when an interior CR or LF would press return
         *   before the end of the content.
         * @param controlBytes the C0/DEL bytes present other than the line
         *   breaks already counted — the ones a TUI acts on rather than shows.
         *   Reported as code points so a message can name them.
         * @param truncated whether [MAX_CHARS] would clip it.
         */
        data class Shape(
            val lines: Int,
            val submits: Boolean,
            val controlBytes: List<Int>,
            val truncated: Boolean,
        ) {
            /** Nothing here needs explaining before it is sent. */
            val isPlain: Boolean get() = !submits && controlBytes.isEmpty() && !truncated
        }

        /**
         * Longest clipboard this app will send in one `input`.
         *
         * A control frame on the remote wire carries at most
         * `rime_remote_core::wire::MAX_PAYLOAD` = 65514 bytes and a request is
         * one frame; JSON escaping turns one control byte into six characters.
         * 16384 characters is comfortably inside that for any encoding, and it is far
         * more than anybody pastes into a prompt on purpose. Above it the user
         * is told rather than having the tail silently dropped — or, worse, the
         * frame refused with a wire error they cannot act on.
         */
        const val MAX_CHARS: Int = 16_384

        /**
         * Classify what is about to be typed, without typing it.
         *
         * Called on the reply box's contents before every send — see the note
         * above on why it is not called on the clipboard.
         */
        fun inspect(text: String): Shape {
            val submits = text.dropLast(1).any { it == '\n' || it == '\r' }
            val control = text
                .filter { (it.code < 0x20 && it != '\n' && it != '\r' && it != '\t') || it.code == 0x7F }
                .map { it.code }
                .distinct()
                .sorted()
            // Counted the way a terminal counts them: CRLF is one press of
            // return, not two, and a trailing break does not open a line that
            // has nothing on it.
            val normalised = text.replace("\r\n", "\n").replace('\r', '\n').trimEnd('\n')
            val lines = if (normalised.isEmpty()) 0 else normalised.count { it == '\n' } + 1
            return Shape(
                lines = lines,
                submits = submits,
                controlBytes = control,
                truncated = text.length > MAX_CHARS,
            )
        }

        /**
         * The two things a user may choose to do with a paste that is not
         * plain.
         *
         * Named rather than left to a screen, because the safe one has to be
         * the default and a screen that got that backwards would be typing
         * somebody's stack trace into an agent as a series of commands.
         */
        enum class Choice {
            /**
             * The first line only, staged and not submitted.
             *
             * The default for anything [Shape.isPlain] is false about. It is
             * the answer that cannot do something the user did not ask for.
             */
            FIRST_LINE,

            /**
             * All of it, exactly as copied, control bytes included.
             *
             * Only after the user has been told what it will do. This is not
             * hidden behind a flag because it is wrong — a deliberate
             * multi-line paste into an agent that is asking for a here-doc is a
             * real thing to want — but it is never what a tap lands on by
             * accident.
             */
            EVERYTHING,
        }

        /**
         * Reduce what is in the box to what the chosen [Choice] would send.
         *
         * Applied so that what the person reads in the box is exactly what Send
         * will deliver. A reduction applied at send time instead would show
         * them one thing and send another, which is the failure mode this whole
         * section exists to avoid.
         */
        fun forReview(text: String, choice: Choice): String {
            val clipped = text.take(MAX_CHARS)
            return when (choice) {
                Choice.EVERYTHING -> clipped
                Choice.FIRST_LINE ->
                    clipped.replace("\r\n", "\n").replace('\r', '\n').substringBefore('\n')
            }
        }
    }

    // ── Photo, screenshot and file ──────────────────────────────────────────

    /**
     * P1-059's second criterion, and why no code here meets it.
     *
     * > Photo/screenshot/file upload lands in an explicit permitted
     * > project/temp path and inserts a safe path into the agent session.
     *
     * The destination half of that is **built and working on the desktop**.
     * `rime_agent_core::inject` copies a file into the session's own scratch
     * `inbox/`, names the copy from an allowlist of 64 characters so no byte of
     * the user's filename reaches the PTY unreduced, types the path, submits
     * nothing, and mirrors every handover to `systemd-journald` where the
     * session cannot rewrite it. P1-035 landed all of it. A phone does not need
     * a new destination; it needs a way to get the bytes there.
     *
     * **There is no such way, and the arithmetic says how far off it is.** The
     * wire vocabulary has no verb that carries file content. The nearest
     * relative is `Request::Inject { id, source }`, whose `source` is a path on
     * the **host** read with the daemon's own access — which is why this round
     * gated it to local origins after finding a paired phone could have named
     * `~/.ssh/id_ed25519` with it.
     *
     * Carrying bytes in a request instead would mean base64 in JSON inside one
     * `Frame::Control`, and `rime_remote_core::wire::MAX_PAYLOAD` is 65514
     * bytes. Base64 costs four bytes for three, the JSON envelope takes its
     * share, and what is left is about **48 KB of file**. A phone screenshot is
     * 100 KB to 2 MB. A verb built to that shape would carry a log tail and
     * refuse a screenshot — the feature's own name — which is the mistake this
     * app has already made once, building a request nothing could parse.
     *
     * Doing it honestly is a chunked transfer: begin, chunk, end, with staging
     * state in the daemon, a size cap, a timeout, cleanup for an upload that is
     * abandoned halfway, and the origin gate above applied to the destination
     * rather than to the source. That is a protocol unit and it is named as one
     * rather than started at the end of an Android round.
     *
     * ## What this app does instead
     *
     * It does not offer the button. A picker that opened, took a photo and then
     * said the machine could not receive it would be worse than no picker: the
     * user has already granted access to that image by the time they learn.
     * [WHY] is what the Session screen shows where the control would be, in the
     * app's own words, so the absence is explained where somebody looks for it.
     */
    object Files {

        /**
         * Largest file this phone will offer to hand over.
         *
         * `rime-agentd`'s `inject::MAX_BYTES`, to the byte. Not a guess and
         * not a policy of this app's: the daemon refuses anything larger
         * BEFORE it takes the channel over, so a phone with a different number
         * here would either refuse a file the machine would have taken, or
         * spend a round trip carrying a file it was always going to be told
         * was too big. It is checked on this side so the second cannot happen
         * and stated as the daemon's so the first cannot drift.
         */
        const val MAX_BYTES: Long = 32L * 1024 * 1024

        /** Shown where the size of the chosen file is. */
        fun tooBig(bytes: Long): String =
            "That file is ${bytes} bytes. A file handed to an agent has to be under " +
                "$MAX_BYTES — the machine refuses a larger one before any of it is sent, so " +
                "this is the same limit rather than a stricter one."

        /**
         * The characters `inject::safe_name` keeps, everything else replaced.
         *
         * Mirrored so the user can be shown what the file will be called
         * before they send it, and for no other purpose: what is SENT is the
         * name as the picker gave it. A phone that reduced the name itself
         * would be a second implementation of a rule the daemon applies
         * anyway, and the one case where the two differed is the case where
         * the user is told the wrong filename.
         */
        const val SAFE: String =
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"

        /** `inject::MAX_NAME`. Longer names keep their tail, so the extension survives. */
        const val MAX_NAME: Int = 48

        /**
         * What the machine will call this file, or `null` when it will refuse
         * the name outright.
         *
         * "As far as it can say" is exact: the daemon also prefixes a sequence
         * number it alone knows, so this is the name and never the path. Shown
         * because a photo called `Screenshot 2026-09-13 at 14.02.11.png`
         * arrives as `Screenshot_2026-09-13_at_14.02.11.png`, and a user who
         * was not told that has to go and look.
         *
         * ## It mirrors `inject::safe_name`, and writing it found three places
         * ## where the obvious mirror is not one
         *
         * 1. **`null` for a name the daemon refuses.** `safe_name` returns
         *    `Err(NoFileName)` for an empty name, `.` and `..`, and
         *    `Err(ControlByte)` for anything with a byte a terminal would act
         *    on. A preview that dressed those up as `file..` would promise a
         *    file the user is never going to find.
         * 2. **Code points, not UTF-16 units.** Rust iterates `chars()`, so an
         *    emoji is ONE replacement. A Kotlin loop over `Char` sees a
         *    surrogate pair and produces TWO, and the user is shown a name a
         *    character longer than the real one.
         * 3. **The all-dots repair applies to what the reduction PRODUCED**,
         *    not to what was typed. `...` survives the alphabet intact and
         *    would make the destination `001-...`, which is a traversal
         *    spelled as a filename — so the daemon prefixes it, and `..` never
         *    reaches that line because rule 1 already refused it.
         *
         * None of this changes what is SENT. The name goes over the wire as
         * the picker gave it; the reduction is the daemon's, and the only way
         * this function can be wrong is by telling somebody the wrong
         * filename.
         */
        fun preview(name: String): String? {
            if (name.isEmpty() || name == "." || name == "..") return null
            // `safe_name` refuses on UTF-8 bytes below 0x20 or 0x7f. Every
            // byte of a non-ASCII character is 0x80 or above, so checking
            // UTF-16 units for the same range is the same test — and a
            // surrogate is 0xD800 or above, so it cannot be caught here.
            if (name.any { it.code < 0x20 || it.code == 0x7f }) return null

            val out = StringBuilder(name.length)
            var i = 0
            while (i < name.length) {
                val cp = name.codePointAt(i)
                out.append(if (cp < 128 && SAFE.indexOf(cp.toChar()) >= 0) cp.toChar() else '_')
                i += Character.charCount(cp)
            }
            var reduced = out.toString()
            if (reduced.isNotEmpty() && reduced.all { it == '.' }) reduced = "file$reduced"
            // The tail, because the extension is the part that tells an agent
            // what it is looking at. The reduction is pure ASCII by
            // construction, so character length and byte length — which is
            // what the daemon measures — are the same number here.
            if (reduced.length > MAX_NAME) reduced = reduced.substring(reduced.length - MAX_NAME)
            return reduced
        }

        /** Shown when [preview] is `null`. */
        const val UNUSABLE_NAME: String =
            "The machine will not take a file with that name. A name that is empty, that is " +
                "just a dot or two, or that contains a character a terminal would act on is " +
                "refused rather than repaired — a repaired name would be a different file " +
                "from the one you chose."

        /**
         * Shown beside the picker, and it is a statement about the machine
         * rather than about this app.
         *
         * The file does not go where the user chose; it goes where the DAEMON
         * chooses — an inbox inside the session's own scratch directory, under
         * a name it reduces — and the agent is told about it by having that
         * path typed into its terminal, unsubmitted. Both halves matter to
         * somebody deciding whether to send a photo: nothing of theirs is
         * written anywhere else on the machine, and nothing happens to it
         * until they or the agent's own operator act.
         */
        const val WHERE: String =
            "The file goes into this session's own inbox on the machine, under a name the " +
                "machine chooses, and its path is typed into the agent's terminal without " +
                "being submitted. Nothing else on the machine is written to, and no other " +
                "session can see it."

        /**
         * The rule this app follows about the phone's own storage, which is
         * P1-059's fourth criterion.
         *
         * `ACTION_PICK_IMAGES` (androidx's `PickVisualMedia`) returns a URI for
         * the one item the user chose and needs **no permission at all** — not
         * `READ_MEDIA_IMAGES`, not `READ_EXTERNAL_STORAGE`. Asking for either
         * would give the app the whole library for the life of the grant, which
         * is exactly the "automatic access beyond Android-granted selections"
         * the criterion forbids, and it would do so to no purpose since the
         * picker already returns what was chosen.
         *
         * So the manifest declares neither, and `ManifestPermissionsTest`
         * asserts it. It was written one round before the transport existed,
         * on the reasoning that the moment there IS one the obvious way to
         * read a file is a permission — and that is now the live case rather
         * than the anticipated one: [Upload] takes an `InputStream`, which is
         * what `ContentResolver.openInputStream` on a picker's URI returns,
         * and asking for any of these would buy nothing it does not already
         * have.
         *
         * One feature reverses this on purpose: "Photos on your computer"
         * (contract §2.13) lists the library for a paired computer, which no
         * picker can do, so the manifest declares `Photos.PERMISSIONS`. They
         * are asked for only when the person opens that screen, the switch is
         * off by default, and partial access is honoured. Sending a file to a
         * session still never uses them, and every other name here must
         * stay absent; `ManifestTest` checks both halves.
         */
        val FORBIDDEN_PERMISSIONS: List<String> = listOf(
            "android.permission.READ_EXTERNAL_STORAGE",
            "android.permission.WRITE_EXTERNAL_STORAGE",
            "android.permission.READ_MEDIA_IMAGES",
            "android.permission.READ_MEDIA_VIDEO",
            "android.permission.READ_MEDIA_VISUAL_USER_SELECTED",
            "android.permission.MANAGE_EXTERNAL_STORAGE",
            "android.permission.ACCESS_MEDIA_LOCATION",
        )
    }
}

/** What the user is told they are about to talk to. */
private val AgentSession.routeLabel: String
    get() = listOfNotNull(
        agentName.takeIf { it.isNotBlank() },
        where.takeIf { it.isNotBlank() }?.substringAfterLast('/'),
    ).joinToString(" in ").ifBlank { "session $id" }
