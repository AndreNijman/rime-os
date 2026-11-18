package com.rimeos.remote.core.agent

/**
 * The in-app guide (P1-060), and its agreement with the desktop's.
 *
 * ## Why the words live in `:core`
 *
 * For the reason `AgentHelpContent.qml` exists on the desktop: prose in one
 * file can be read start to finish, and a test can hold it to the stop-slop
 * rules without scanning UI code around it. It also means `HelpParityTest` can
 * compare this against a vendored copy of the desktop's file, which is the only
 * way "equivalent to desktop Agents/Workspaces help" is a claim anybody can
 * check rather than a claim somebody made.
 *
 * ## Equivalent is not identical, and the difference is written down
 *
 * The two clients do not have the same powers. A phone cannot approve a root
 * operation, cannot restore a checkpoint, and cannot turn the agent runtime on.
 * Help that repeated the desktop's instructions would be telling somebody
 * holding a phone to do things they cannot do from it, which is worse than
 * saying less.
 *
 * So the SECTIONS match one for one — that is what the parity test asserts —
 * and where a section's content differs it says what this device can do and
 * names what it cannot. [Section.desktopId] is the link between the two, and
 * a desktop section with no counterpart has to be listed in the parity test's
 * allowlist with a reason, so dropping one is a decision somebody writes down
 * rather than a gap nobody notices.
 */
object Help {

    /**
     * One block of the guide.
     *
     * The kinds are the desktop's, deliberately: `h` a sub-heading, `p` a
     * paragraph, `cmd` a command printed verbatim, `kv` a term and its meaning,
     * `note` a paragraph with a rule down its edge, `todo` something the
     * roadmap asks for that this build does not have. Two vocabularies for one
     * document is how the two ends stop looking like one product.
     */
    data class Block(val kind: Kind, val text: String, val term: String = "")

    enum class Kind { H, P, CMD, KV, NOTE, TODO }

    data class Section(
        /** The desktop section this answers to. */
        val desktopId: String,
        val title: String,
        val blocks: List<Block>,
    )

    private fun h(t: String) = Block(Kind.H, t)
    private fun p(t: String) = Block(Kind.P, t)
    private fun cmd(t: String) = Block(Kind.CMD, t)
    private fun kv(term: String, t: String) = Block(Kind.KV, t, term)
    private fun note(t: String) = Block(Kind.NOTE, t)
    private fun todo(t: String) = Block(Kind.TODO, t)

    /** What the help entry is called where it is offered. */
    const val ENTRY_LABEL: String = "How agents and workspaces work"

    val sections: List<Section> = listOf(
        Section(
            desktopId = "start",
            title = "Start here",
            blocks = listOf(
                h("A Rime agent is an ordinary agent in a terminal Rime owns"),
                p(
                    "An agent is a coding assistant that runs in a terminal: Claude Code, " +
                        "OpenCode, Codex or Gemini. Rime does not replace it. Rime opens the " +
                        "terminal and runs the same binary you would have typed yourself, so " +
                        "the agent behaves the way it does outside Rime.",
                ),
                p(
                    "The shell on the machine does not own that terminal. The agent runtime " +
                        "does, which is why closing a window there does not stop the work and " +
                        "why this phone can pick the session up from anywhere at all.",
                ),
                h("What this app is"),
                p(
                    "A second window onto agents running on a computer you have paired with. " +
                        "The phone read all of this from that computer, over a connection it " +
                        "opened. Nothing runs on the phone.",
                ),
                h("Starting an agent from here"),
                p(
                    "Agent Center lists what is running. New starts one. It asks for a " +
                        "directory and an agent, and takes an opening instruction when you " +
                        "have one. The directory is a path on the computer, not on the phone.",
                ),
                note(
                    "The agent runtime has to be switched on at the computer before any of " +
                        "this answers. It is a per-user service that stays off until asked, " +
                        "and it cannot be switched on from a phone, because turning on the " +
                        "thing that runs commands as you is a decision Rime keeps at the " +
                        "keyboard. Run this there once:",
                ),
                cmd("rime agent enable"),
            ),
        ),
        Section(
            desktopId = "projects",
            title = "Projects and worktrees",
            blocks = listOf(
                h("A worktree is a second checkout of the same repository"),
                p(
                    "Git can have several working directories sharing one history. Rime uses " +
                        "that to let an agent work on a branch without touching the files you " +
                        "have open, so a review and a rewrite can happen at the same time.",
                ),
                h("What the Projects screen shows"),
                p(
                    "Each project the computer remembers, with its worktrees underneath. A row " +
                        "that arrived before Rime knew which project owned it goes in a " +
                        "bucket of its own rather than under the wrong heading.",
                ),
                kv("Changed", "Files that differ from the base, plus the lines added and removed."),
                kv("Ahead / behind", "Commits this worktree has that the base does not, and the reverse."),
                kv("Merge state", "The result Rime got when it tried the merge. Unknown stands until it has tried, and unknown is not clean."),
                kv("Tests", "The last run Rime saw. Not observed, running, passed and failed are four answers, and no two of them mean the same thing."),
                kv("Ready to propose", "A judgement the computer made from local facts. Nothing here asked GitHub anything."),
                note(
                    "This list stays off the four-second timer the session list uses. To " +
                        "answer it the computer runs git in each remembered project, and the " +
                        "trial merge writes objects. The phone therefore asks for this list " +
                        "when you open the screen, not behind your back.",
                ),
            ),
        ),
        Section(
            desktopId = "review",
            title = "Review, checkpoints and undo",
            blocks = listOf(
                h("The summary, not the diff"),
                p(
                    "Per worktree: how many files changed, how many lines, what the last test " +
                        "run did, whether a merge would conflict, and whether the work looks " +
                        "finished. The computer sends counts and no file names, so a list of " +
                        "changed files on this screen would be a list the app invented.",
                ),
                h("Checkpoints"),
                p(
                    "A checkpoint is a snapshot of the tree taken before an agent starts, so " +
                        "there is something to go back to. The toggle on the New screen asks for " +
                        "one. A session that has one shows its id.",
                ),
                note(
                    "A checkpoint cannot be restored from this phone, and caution is not the " +
                        "reason. No verb on this connection reads a checkpoint or reverses " +
                        "one. The computer works out what a restore would change from the " +
                        "snapshot's own tree, and none of that crosses the wire. A list of " +
                        "consequences on this screen would therefore describe a destructive " +
                        "operation from the wrong data. Run it there:",
                ),
                cmd("rime agent undo --checkpoint <id>"),
                todo(
                    "Showing what an undo would change, before it runs, is on the roadmap. " +
                        "It needs a checkpoint verb on the connection this app uses, which " +
                        "this build does not have.",
                ),
            ),
        ),
        Section(
            desktopId = "sandbox",
            title = "Sandbox and permissions",
            blocks = listOf(
                h("Five questions, kept apart"),
                p(
                    "A session carries what Rime decided about it, and each line answers a " +
                        "different question. One word covering all five would hide the one " +
                        "that matters on the day it matters.",
                ),
                kv("Sandbox", "The parts of the filesystem the agent can see."),
                kv("Network", "Whether it can reach the network, and how much of it."),
                kv("System", "Whether it holds a grant to do anything as root, and for how long."),
                kv("Secrets", "Whether the secret broker will hand it credentials."),
                kv("Agent", "The agent's own account of its permission mode. Its report, not Rime's decision."),
                h("Approving things"),
                p(
                    "Approvals lists what agents asked for and how it went, the standing " +
                        "grants each project holds, and any live system access.",
                ),
                note(
                    "This app cannot approve anything, and no setting changes that. Each " +
                        "operation in the vocabulary is a root operation, and Rime reserves " +
                        "approving one for a person at that computer. A paired device cannot " +
                        "even deny: the refusal lands before the daemon looks the request up. " +
                        "Hence no Approve button here, rather than a button that refuses " +
                        "whenever you press it.",
                ),
                p(
                    "Revoking is different and it works from here. Taking authority away only " +
                        "ever removes it, so a device that cannot grant anything can still " +
                        "take it back.",
                ),
            ),
        ),
        Section(
            desktopId = "remote",
            title = "This phone and the computer",
            blocks = listOf(
                h("Pairing"),
                p(
                    "Scan the code the computer shows. Both ends work out a shared key from " +
                        "it and remember each other's public key. Later connections answer to the " +
                        "key from that first meeting, not to a name.",
                ),
                h("Where the connection goes"),
                kv("On the same network", "A direct connection to the computer. Nothing in the middle."),
                kv("Away from it", "A relay, which forwards bytes it cannot read. The key stays at the two ends."),
                note(
                    "This app tries the direct connection first and falls back to the relay " +
                        "only if that fails, so on your own network the relay learns nothing " +
                        "about the connection. The app tells you which one it got.",
                ),
                kv(
                    "What the relay sees",
                    "Encrypted bytes it cannot read, both addresses, when you connect and " +
                        "how much data moves. Not what any agent said, typed or ran.",
                ),
                h("The computer's clipboard"),
                p(
                    "Clipboard, on the agent list, fetches whatever was last copied on the " +
                        "computer and puts it on this phone, ready to paste anywhere. It does " +
                        "not belong to any session, so it works whether or not an agent is " +
                        "waiting for you.",
                ),
                note(
                    "This is the opposite direction from Paste in the reply box, which sends " +
                        "what is on THIS phone to an agent. If the computer's clipboard is " +
                        "empty you are told so, and what you had copied here is left alone.",
                ),
                p(
                    "The computer refuses long text rather than cutting it short: past " +
                        "eight kilobytes it says so instead of handing over part of what " +
                        "you copied.",
                ),
                h("The computer itself"),
                p(
                    "Computer, on the agent list, shows how the computer is: its battery, " +
                        "its power mode, whether its lock screen is up, and whether an update " +
                        "is waiting. It also shows what is playing there, with play, pause, " +
                        "skip and volume.",
                ),
                p(
                    "Lock the screen locks it the way the keyboard shortcut would, and the " +
                        "button says so only once the computer confirms it. Unlocking is " +
                        "only ever done at the computer.",
                ),
                note(
                    "The screen checks on the computer a few times a minute while it is " +
                        "open, and not at all while the app is in the background. A line the computer " +
                        "could not read is left out rather than guessed.",
                ),
                h("Sending to the computer"),
                p(
                    "Share a photo, a file or some text from any app and pick Rime Remote. " +
                        "Nothing is sent until you choose the computer and tap Send. Files go " +
                        "to its Downloads folder, and text goes to its clipboard, ready to paste.",
                ),
                p(
                    "A file with the same name as one already there does not replace it: the computer " +
                        "keeps both and tells you the name it used. Send this phone's " +
                        "clipboard, on the Computer screen, does the same for what you copied here.",
                ),
                note(
                    "If the connection drops after the whole file went over, the app says the " +
                        "file may or may not be there. Check before sending it again. The " +
                        "computer deletes any file you stop part way.",
                ),
                h("Messages on the computer"),
                p(
                    "Messages on your computer, in Display, lets your computers read this " +
                        "phone's SMS conversations and send an SMS from its number. It is off " +
                        "until you turn it on, and Android asks you to allow SMS first. On the " +
                        "computer, rime remote messages lists them, and rime remote messages " +
                        "send sends one.",
                ),
                p(
                    "The computer asks the phone each time and keeps nothing. Sending needs " +
                        "the app unlocked, and a person at the computer: a program running " +
                        "there in the background cannot send. Group conversations and picture " +
                        "messages are left out.",
                ),
                note(
                    "If this app came from a file rather than a store, Android may block the " +
                        "SMS permission without asking. Open App info, use the menu at the top " +
                        "right to allow restricted settings, then allow SMS there.",
                ),
                h("Calls on the computer"),
                p(
                    "Calls on your computer, in Display, shows a call on your computers while " +
                        "it rings on this phone, with Answer and Decline. It is off until you " +
                        "turn it on, and Android asks you to allow calls first. On the " +
                        "computer, rime remote calls lists recent calls, and rime remote calls " +
                        "answer, decline, hangup and dial do what they say.",
                ),
                p(
                    "The sound stays on the phone: talk on it or on a headset paired with it. " +
                        "You can answer and end a call with the app locked, as you can on the " +
                        "phone's lock screen. Calling a number needs the app unlocked, and " +
                        "a person at the computer. The phone refuses to dial an emergency " +
                        "number for the computer: make that call on the phone.",
                ),
                note(
                    "If this app came from a file rather than a store, Android may block the " +
                        "call log permission without asking. Allow restricted settings in App " +
                        "info, as for SMS.",
                ),
                h("Photos on the computer"),
                p(
                    "Photos on your computer, in Display, lets your computers see this phone's " +
                        "photos, newest first, with a small preview of each, and save the ones " +
                        "you choose. It is off until you turn it on, and Android asks you to " +
                        "allow photos first. On the computer, rime remote photos lists them, and " +
                        "rime remote photos save copies one to the computer.",
                ),
                p(
                    "The computer asks the phone each time, and only while you have the app " +
                        "unlocked. The previews carry no location or camera details. From " +
                        "Android 10 on, Android also removes the location from a saved photo. " +
                        "Videos are left out.",
                ),
                note(
                    "If you let Android show only some photos, the computer sees those and no " +
                        "others, including photos you take later. To add more, or allow all, " +
                        "open Photos on your computer again.",
                ),
                h("Finding this phone"),
                p(
                    "On the computer, rime remote ring makes this phone play its alarm " +
                        "sound so you can find it. Tap Stop on the notification, or it stops " +
                        "by itself after 30 seconds. It rings in silent mode and through Do " +
                        "Not Disturb, unless Do Not Disturb is set to total silence.",
                ),
                note(
                    "While connected, this app also tells the computer its battery level and " +
                        "whether it is charging, so rime remote devices can show it. Nothing " +
                        "else about the phone is sent.",
                ),
                h("Taking this phone's access away"),
                p(
                    "Forget the computer here, or remove this device at the computer. Either " +
                        "way the key is gone and the other end stops answering it.",
                ),
                h("After the phone restarts"),
                p(
                    "The key this app uses with a computer sits behind the phone's lock, so " +
                        "after a restart the computer hears nothing until you unlock the app. " +
                        "Keep working after the phone restarts, in Display, changes that. Each " +
                        "computer gets a second key from this phone that opens without you, and " +
                        "it can only carry the phone's own features: its battery, its " +
                        "notifications, what is playing, its messages, its calls, ringing it " +
                        "and sending it files.",
                ),
                p(
                    "With it on, locking the app keeps those running instead of ending the " +
                        "link, and after a restart they come back at the phone's first unlock. " +
                        "Replying to a notification, sending an SMS, calling a number, controlling what " +
                        "plays, photos, agents, terminals " +
                        "and the computer's controls still wait until you unlock.",
                ),
                note(
                    "Turning it on asks once to leave this app out of battery optimisation, " +
                        "so Android does not hold the link off while the phone is idle. You can " +
                        "say no; the link still works, later. Turning it off, or forgetting the " +
                        "computer, deletes the second key here and tells the computer to forget it.",
                ),
                h("App lock"),
                p(
                    "The lock on this app is the phone's own biometric or device credential, " +
                        "and it guards the key stored on this phone. It is not an " +
                        "authenticator the computer knows about, so unlocking here does not " +
                        "authorise anything there.",
                ),
            ),
        ),
        Section(
            desktopId = "keys",
            title = "The terminal, gestures and the key row",
            blocks = listOf(
                h("The terminal is the real one"),
                p(
                    "Attach and you get the session's actual terminal, the same bytes the " +
                        "computer's screen would show. Detaching leaves the agent running.",
                ),
                kv("Scroll", "Drag. Scrollback is kept, and the buffer is searchable."),
                kv("Select", "Long-press and drag, then copy."),
                kv("Paste", "Long-press in the terminal or the reply box."),
                h("The key row"),
                p(
                    "A soft keyboard has no Escape, no Control and no arrows. A terminal " +
                        "leans on all three. The row above the keyboard supplies them, and " +
                        "settings decides which keys it holds.",
                ),
                h("Text size"),
                p(
                    "Terminal text size is a setting because it decides how many columns fit, " +
                        "and eighty columns is what these programs lay out for. It is the " +
                        "difference between reading the output and reading a smear of it.",
                ),
                h("Answering without attaching"),
                p(
                    "A session that is waiting for you has a reply box. Speak fills it from " +
                        "the phone's own speech recognition, Paste fills it from the " +
                        "clipboard, and Send types it into the terminal on the computer.",
                ),
                note(
                    "Text with a line break in it gets a warning before you send it. On a " +
                        "terminal a line break is the return key, so three lines run the " +
                        "first and type the other two into whatever the agent asks next.",
                ),
                h("Sending a photo or a file"),
                p(
                    "Photo opens the phone's picker and File opens the document picker. " +
                        "Neither asks for a storage permission, because neither needs one: " +
                        "the picker hands this app the one item you chose and nothing else.",
                ),
                p(
                    "The computer drops your file into that session's own inbox, gives it a " +
                        "name of its own choosing, and types the path into the agent's " +
                        "terminal without pressing return. You see the same path. It touches " +
                        "nothing else on the computer, and no other session can reach it.",
                ),
                note(
                    "The name changes on the way. Spaces, accents and anything a terminal " +
                        "would act on become underscores, so a screenshot called `Screenshot " +
                        "2026-09-13 at 14.02.11.png` arrives as " +
                        "`Screenshot_2026-09-13_at_14.02.11.png`. The screen shows you the " +
                        "new name before you send it.",
                ),
                todo(
                    "The 32 MB limit belongs to the computer, not to this app. This app " +
                        "checks it first so that you spend no data finding out.",
                ),
            ),
        ),
        Section(
            desktopId = "stuck",
            title = "When a session is stuck",
            blocks = listOf(
                h("Waiting for you is not stuck"),
                p(
                    "An agent that stops and asks something is working as intended. It shows " +
                        "as waiting, and the reply box on the session is how to answer it " +
                        "without attaching a terminal.",
                ),
                h("The four controls"),
                kv("Pause", "Stops the process where it is. It stays paused until resumed, and anything sent to it waits unread."),
                kv("Resume", "Starts it again from where it stopped."),
                kv("Interrupt", "The same as pressing Ctrl+C at the terminal. Ends what it is doing, not the session."),
                kv("Stop", "Asks it to shut down, and lets it clean up first."),
                h("Nothing answers at all"),
                p(
                    "Check that the connection is up on Machines. If it is, the agent runtime " +
                        "on the computer may have stopped; it is a service there and this " +
                        "phone cannot restart it.",
                ),
                h("It says this computer's Rime is too old"),
                p(
                    "That is not a refusal and not an empty answer. The computer is running a " +
                        "build from before the thing you asked for existed, and this app tells " +
                        "the two apart rather than showing you an empty list. Update it there:",
                ),
                cmd("sudo rime update"),
            ),
        ),
    )

    /** Every desktop section this guide answers to. */
    val covered: Set<String> get() = sections.map { it.desktopId }.toSet()
}
