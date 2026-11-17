package com.rimeos.remote.ui.agent

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.agent.AgentGraph
import com.rimeos.remote.core.agent.AgentSession
import com.rimeos.remote.core.agent.Elapsed
import com.rimeos.remote.core.agent.Gauge
import com.rimeos.remote.core.agent.Order
import com.rimeos.remote.core.agent.live
import com.rimeos.remote.ui.ClipboardPull
import com.rimeos.remote.ui.PeekView
import com.rimeos.remote.ui.theme.RimeTones
import com.rimeos.remote.ui.theme.MachineText

/**
 * Every agent on one machine, and what each of them is doing.
 *
 * The order is the desktop's and is decided in `:core` — the ones that need
 * you as their own group above everything, then live before finished, then
 * most recently active. Doing it there rather than here is what stops a badge
 * count, a notification and this list disagreeing about which session is
 * first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgentCenterScreen(
    machine: String,
    sessions: List<AgentSession>,
    nowSeconds: Long,
    busy: String?,
    failure: String?,
    /**
     * Something that worked and that the user still has to be told.
     *
     * Carries the clipboard outcomes: how much was copied, or that the
     * machine's clipboard was empty. Empty is deliberately here and not in
     * [failure] — see `RemoteViewModel.pullMachineClipboard`.
     */
    notice: String? = null,
    onRefresh: () -> Unit,
    onOpen: (AgentSession) -> Unit,
    onStart: () -> Unit,
    onProjects: () -> Unit,
    onApprovals: () -> Unit,
    /** Open the guide. */
    onHelp: () -> Unit,
    /** Open nex on this machine, or null when its bridge reaches no nexd. */
    onNex: (() -> Unit)? = null,
    /**
     * Ask the machine for its clipboard (P1-059 criterion 3, receive half).
     *
     * Here and not in `ReplyBox`, which is reachable only while some agent is
     * waiting for input. Wanting what the computer copied has nothing to do
     * with whether an agent is waiting, and the request itself carries no
     * session id, so this belongs with the other id-less machine action —
     * Projects — and not under a session.
     */
    onCopyFromMachine: () -> Unit = {},
    /**
     * The machine's clipboard, waiting to be put on this phone's.
     *
     * Handed over as state rather than written by the view model because a
     * `ClipboardManager` needs a `Context`, and this composable already has
     * one through `LocalClipboardManager`.
     */
    clipboardPull: ClipboardPull? = null,
    /** Called once [clipboardPull] has been put on the phone's clipboard. */
    onClipboardCopied: () -> Unit = {},
    /** How many privileged operations are waiting at the machine. */
    pendingApprovals: Int = 0,
    /** Whether this phone will show a notification at all. */
    notificationsEnabled: Boolean = true,
    /** True while `POST_NOTIFICATIONS` has never been asked for. */
    notificationsUnasked: Boolean = false,
    /**
     * The last line each live session printed, from `peek`, keyed by
     * `PeekView.keyOf` (id and start time). Empty against a machine that
     * cannot peek, and then each row shows the session's `detail`, as before.
     */
    lastLines: Map<String, String> = emptyMap(),
    /** Tell the view model the list is on screen, so it asks for last lines only then. */
    onWatchList: (Boolean) -> Unit = {},
    onBack: () -> Unit,
    onDismiss: () -> Unit,
) {
    val (needsYou, rest) = Order.groups(sessions)
    DisposableEffect(Unit) {
        onWatchList(true)
        onDispose { onWatchList(false) }
    }

    // Keyed on the TOKEN and not on the text: asking twice for the same
    // clipboard has to copy twice, and an effect keyed on the text would not
    // re-run for an identical second answer. See `ClipboardPull`.
    val clipboard = LocalClipboardManager.current
    LaunchedEffect(clipboardPull?.token) {
        val pull = clipboardPull ?: return@LaunchedEffect
        clipboard.setText(AnnotatedString(pull.text))
        onClipboardCopied()
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text(machine, style = MaterialTheme.typography.titleMedium, maxLines = 1) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                actions = { TextButton(onClick = onRefresh) { Text("Refresh") } },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            busy?.let { Strip(it, alarming = false) }
            failure?.let { Strip(it, alarming = true, onDismiss = onDismiss) }
            // Not alarming: everything that arrives here WORKED. "The
            // machine's clipboard is empty" is the case this exists for —
            // drawn in red it reads as a permission problem, and drawn
            // nowhere it reads as a button that does nothing.
            notice?.let { Strip(it, alarming = false, onDismiss = onDismiss) }

            // Said on screen rather than left to be discovered. Without the
            // permission the poll goes on raising alerts and `Notifier.post`
            // goes on declining to show them, and the user concludes the
            // feature is broken rather than unasked.
            if (!notificationsEnabled) {
                Strip(
                    if (notificationsUnasked) {
                        "Notifications have not been turned on for this app, so nothing will " +
                            "reach you when an agent needs you. Everything still shows here."
                    } else {
                        "Notifications are switched off for this app in Android's settings, so " +
                            "nothing will reach you when an agent needs you."
                    },
                    alarming = false,
                )
            }

            // The other things a machine has, above the list rather than in a
            // menu: a pending root operation is the one thing on this screen
            // the user cannot act on from here, so it has to be visible enough
            // to send them to the machine. The guide sits beside them for the
            // same reason it does on the desktop — somebody who does not know
            // what a worktree is will not go looking in settings for a page
            // that explains it.
            // A `FlowRow` and not a `Row`, and the difference is a defect a
            // device found: at the largest text size Android offers, four
            // `TextButton`s do not fit across a Pixel 7a, and a `Row` resolves
            // that by measuring the last child at a width of ZERO. "Guide"
            // was on the screen with nothing to occupy — not truncated, not
            // scrolled off, simply invisible, and unreachable with it. A flow
            // row wraps instead, so every action stays on the screen at every
            // font scale. Measured by `LayoutOnDeviceTest` at scale 2.0.
            FlowRow(Modifier.fillMaxWidth().padding(horizontal = 8.dp)) {
                onNex?.let { TextButton(onClick = it) { Text("nex") } }
                TextButton(onClick = onProjects) { Text("Projects") }
                TextButton(onClick = onApprovals) {
                    Text(
                        if (pendingApprovals > 0) "Approvals ($pendingApprovals)" else "Approvals",
                        color = if (pendingApprovals > 0) {
                            RimeTones.forState("permission_request")
                        } else {
                            MaterialTheme.colorScheme.primary
                        },
                    )
                }
                // "Clipboard" and not "Paste": the word names what is being
                // fetched, and the direction is the thing users get wrong
                // about this feature. Paste already means the opposite
                // journey in this app — the button in `ReplyBox` that puts
                // THIS phone's clipboard into a reply.
                TextButton(onClick = onCopyFromMachine) { Text("Clipboard") }
                TextButton(onClick = onHelp) { Text("Guide") }
            }

            if (sessions.isEmpty() && busy == null) {
                Empty(onStart)
            } else {
                LazyColumn(Modifier.weight(1f)) {
                    if (needsYou.isNotEmpty()) {
                        item {
                            // A heading, which is why the two are groups rather
                            // than a sort: "two agents are waiting for you" is
                            // legible without counting badges.
                            Heading(
                                if (needsYou.size == 1) {
                                    "1 agent needs you"
                                } else {
                                    "${needsYou.size} agents need you"
                                },
                            )
                        }
                        items(needsYou, key = { "n${it.id}" }) {
                            SessionRow(it, nowSeconds, lastLines[PeekView.keyOf(it)]) { onOpen(it) }
                        }
                    }
                    if (rest.isNotEmpty()) {
                        if (needsYou.isNotEmpty()) item { Heading("Everything else") }
                        items(rest, key = { "r${it.id}" }) {
                            SessionRow(it, nowSeconds, lastLines[PeekView.keyOf(it)]) { onOpen(it) }
                        }
                    }
                }
                Button(
                    onClick = onStart,
                    shape = RoundedCornerShape(6.dp),
                    modifier = Modifier.fillMaxWidth().padding(16.dp),
                ) { Text("Start an agent") }
            }
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

/**
 * One session.
 *
 * The stripe down the left is the reason the page reads as coloured at a
 * glance rather than as a list of white text with one small badge on each
 * line — the desktop's `StateBadge.qml` exposes `toneColor` for exactly this,
 * and says so.
 */
@Composable
fun SessionRow(
    session: AgentSession,
    nowSeconds: Long,
    /** The last line it printed, when the machine can peek and it is live. */
    lastLine: String? = null,
    onClick: () -> Unit,
) {
    val tone = RimeTones.forState(session.state)
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(end = 16.dp, top = 10.dp, bottom = 10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Box(Modifier.width(3.dp).height(52.dp).background(tone))
        Spacer(Modifier.width(13.dp))
        StateBadge(session.state)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                // Name, then the agent's own title, then the label — the same
                // order as the desktop's Agent Center and `rime agent list`.
                Text(
                    session.displayName,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (session.isNamed) {
                    // Which agent it is, when the name no longer says.
                    Spacer(Modifier.width(6.dp))
                    Text(
                        session.agentName,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                session.telemetry?.model?.let {
                    // The model, only when the agent has told us. A row that
                    // printed a default would be naming a model nobody chose.
                    Spacer(Modifier.width(6.dp))
                    Text(it, style = MaterialTheme.typography.labelSmall, color = tone)
                }
                Spacer(Modifier.weight(1f))
                Text(
                    Elapsed.of(session, nowSeconds),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                session.where,
                style = MachineText,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(StateLabel(session.state), style = MaterialTheme.typography.labelSmall, color = tone)
                // The runtime's own words about the state, when it attached
                // any: a step, an error, "paused". Never invented here.
                session.detail?.takeIf { it.isNotBlank() && lastLine == null }?.let {
                    Spacer(Modifier.width(6.dp))
                    Text(
                        it,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            // What it last printed, in its own words and its own font, for a
            // live session on a machine that can peek. It replaces the detail
            // above rather than joining it: one line of "what is it doing" is
            // what a row has room for, and the agent's screen is the more
            // current answer.
            lastLine?.takeIf { session.live && it.isNotBlank() }?.let {
                Text(
                    it,
                    style = MachineText,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            val branch = session.telemetry?.branch
            val graph = AgentGraph.summary(session.children)
            if (branch != null || graph != null) {
                Text(
                    listOfNotNull(branch, graph).joinToString(" · "),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // The context gauge, and ONLY when the agent has reported one.
            // `Telemetry` says it plainly: a gauge drawn at 0% for a session
            // that has never reported is a gauge that is lying.
            Gauge.fraction(session.telemetry?.contextPct)?.let { fraction ->
                Spacer(Modifier.height(4.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LinearProgressIndicator(
                        progress = { fraction },
                        color = tone,
                        modifier = Modifier.weight(1f).height(3.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        "context ${Gauge.label(session.telemetry?.contextPct)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.surfaceVariant)
}

@Composable
private fun Empty(onStart: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("Nothing is running", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "This machine has no agent sessions. Starting one here runs it on the " +
                "machine, not on this phone — it keeps going when you put the phone away.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(20.dp))
        Button(onClick = onStart, shape = RoundedCornerShape(6.dp)) { Text("Start an agent") }
    }
}

@Composable
internal fun Strip(text: String, alarming: Boolean, onDismiss: (() -> Unit)? = null) {
    Surface(
        color = if (alarming) {
            MaterialTheme.colorScheme.errorContainer
        } else {
            MaterialTheme.colorScheme.surfaceVariant
        },
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).clip(RoundedCornerShape(6.dp)),
    ) {
        Row(Modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(text, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
            onDismiss?.let { TextButton(onClick = it) { Text("OK") } }
        }
    }
}

/** Whether a session can still be signalled. A dead one cannot. */
val AgentSession.controllable: Boolean get() = live
