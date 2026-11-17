package com.rimeos.remote.ui.nex

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.nex.NexMission
import com.rimeos.remote.core.nex.NexMissionDetail
import com.rimeos.remote.core.nex.NexSession
import com.rimeos.remote.core.nex.NexStates
import com.rimeos.remote.core.nex.NexUsage
import com.rimeos.remote.ui.agent.StateBadge
import com.rimeos.remote.ui.agent.Strip
import com.rimeos.remote.ui.theme.MachineText

/**
 * nex on one computer: what is waiting for you, the sessions, the missions,
 * and what they have used.
 *
 * Approvals come first because they are the one thing here that stops work
 * until somebody answers, and the reason a phone is worth having for this at
 * all: the agent asked while you were away from the desk.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NexScreen(
    machine: String,
    state: NexUiState,
    onOpen: (String) -> Unit,
    onNew: () -> Unit,
    onApprove: (com.rimeos.remote.core.nex.NexApproval) -> Unit,
    onDeny: (com.rimeos.remote.core.nex.NexApproval) -> Unit,
    onMission: (String, NexMission) -> Unit,
    onToggleMission: (String) -> Unit,
    onRefresh: () -> Unit,
    onDismiss: () -> Unit,
    onBack: () -> Unit,
) {
    // Cancelling a mission ends its work and cannot be undone, so it is asked.
    var cancelling by remember { mutableStateOf<NexMission?>(null) }
    val labels = state.sessions.associate { it.id to it.label }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("nex", style = MaterialTheme.typography.titleMedium)
                        Text(
                            machine + if (state.live) " · live" else "",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                actions = {
                    TextButton(onClick = onRefresh, enabled = !state.loading) {
                        Text(if (state.loading) "Loading…" else "Refresh")
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            state.failure?.let { Strip(it, alarming = true, onDismiss = onDismiss) }
            state.notice?.let { Strip(it, alarming = false, onDismiss = onDismiss) }

            LazyColumn(Modifier.weight(1f)) {
                state.away?.let { why -> item { AwayNotice(why, onRefresh) } }

                if (state.pending.isNotEmpty()) {
                    item {
                        NexHeading(
                            if (state.pending.size == 1) "1 action is waiting for you"
                            else "${state.pending.size} actions are waiting for you",
                        )
                    }
                    for (a in state.pending) {
                        item(key = "a${a.id}") {
                            ApprovalCard(
                                approval = a,
                                sessionLabel = a.sessionId?.let { labels[it] },
                                deciding = a.id in state.deciding,
                                onApprove = { onApprove(a) },
                                onDeny = { onDeny(a) },
                            )
                        }
                    }
                }

                item {
                    Row(
                        Modifier.fillMaxWidth().padding(end = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) { NexHeading("Sessions") }
                        if (state.roots.isNotEmpty()) {
                            TextButton(onClick = onNew, enabled = state.away == null) { Text("New session") }
                        }
                    }
                }
                for (s in state.ordered) {
                    item(key = "s${s.id}") { SessionRow(s, state.waitingOnYou(s.id)) { onOpen(s.id) } }
                }
                if (state.loaded && state.sessions.isEmpty() && state.away == null) {
                    item {
                        Text(
                            "No nex sessions on this computer yet. Start one there with `nex` in a " +
                                "project folder; after that the phone can start more in the same folder.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(16.dp),
                        )
                    }
                }

                if (state.shows(NexUiState.MISSIONS) && state.missions.isNotEmpty()) {
                    item { NexHeading("Missions") }
                    for (m in state.missions) {
                        item(key = "m${m.id}") {
                            MissionRow(
                                mission = m,
                                detail = state.missionDetails[m.id],
                                busy = state.sending,
                                onToggle = { onToggleMission(m.id) },
                                onAction = { verb ->
                                    if (verb == "missions.cancel") cancelling = m else onMission(verb, m)
                                },
                            )
                        }
                    }
                }

                val usage = state.usage.filter { it.worthShowing }
                if (state.shows(NexUiState.USAGE) && usage.isNotEmpty()) {
                    item { NexHeading("Usage") }
                    for (u in usage) item(key = "u${u.connection}") { UsageRow(u) }
                }
            }
        }
    }

    cancelling?.let { m ->
        AlertDialog(
            onDismissRequest = { cancelling = null },
            title = { Text("Cancel this mission?") },
            text = {
                Text(
                    "\"${m.objective}\" stops, and its running tasks are cancelled. Work already " +
                        "committed in its branches stays.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    onMission("missions.cancel", m)
                    cancelling = null
                }) { Text("Cancel mission") }
            },
            dismissButton = { TextButton(onClick = { cancelling = null }) { Text("Keep it") } },
        )
    }
}

@Composable
private fun SessionRow(s: NexSession, waiting: Boolean, onOpen: () -> Unit) {
    val state = NexStates.agentState(s.state, waitingOnYou = waiting)
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClickLabel = "Open ${s.label}", onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StateBadge(state)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(s.label, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                listOfNotNull(
                    if (waiting) "waiting for you" else NexStates.words(s.state),
                    s.provider,
                    folderName(s.root),
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.surfaceVariant)
}

@Composable
private fun MissionRow(
    mission: NexMission,
    detail: NexMissionDetail?,
    busy: Boolean,
    onToggle: () -> Unit,
    onAction: (String) -> Unit,
) {
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable(
                    role = Role.Button,
                    onClickLabel = if (detail == null) "Show tasks" else "Hide tasks",
                    onClick = onToggle,
                )
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            StateBadge(NexStates.agentState(mission.state))
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(mission.objective, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(
                    NexStates.words(mission.state),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        detail?.let { d ->
            Column(Modifier.padding(start = 52.dp, end = 16.dp, bottom = 8.dp)) {
                d.reason?.let {
                    Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
                for (t in d.tasks) {
                    Text(
                        "${t.id}: ${NexStates.words(t.state)}" + if (t.attempt > 1) ", attempt ${t.attempt}" else "",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    t.lastError?.let {
                        Text(
                            it,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 3,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                if (d.verify.isNotEmpty()) {
                    Spacer(Modifier.height(4.dp))
                    Text("Checked with", style = MaterialTheme.typography.labelSmall)
                    for (v in d.verify) Text(v, style = MachineText, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                }
            }
        }
        val actions = NexMission.actions(mission.state)
        if (actions.isNotEmpty()) {
            FlowRow(Modifier.padding(start = 44.dp, end = 8.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                for (verb in actions) {
                    if (verb == "missions.cancel") {
                        TextButton(onClick = { onAction(verb) }, enabled = !busy) { Text(missionVerbLabel(verb)) }
                    } else {
                        Button(onClick = { onAction(verb) }, enabled = !busy) { Text(missionVerbLabel(verb)) }
                    }
                }
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.surfaceVariant)
}

@Composable
private fun UsageRow(u: NexUsage) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp)) {
        Text(u.connection, style = MaterialTheme.typography.titleSmall)
        val tokens = u.tokens.filter { it.total > 0 }
        if (tokens.isNotEmpty()) {
            Text(
                tokens.joinToString(" · ") { "${it.metric.removeSuffix("_tokens").replace('_', ' ')} ${compactCount(it.total)}" } +
                    if (tokens.all { it.estimate }) " (estimated)" else "",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        for (q in u.quota) {
            val used = q.usedPercent?.let { "%.0f%% used".format(it) } ?: "use not reported"
            Text(
                listOfNotNull(q.label, q.window, used, q.resetAt?.let { "resets $it" }, if (q.stale) "may be out of date" else null)
                    .joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.surfaceVariant)
}
