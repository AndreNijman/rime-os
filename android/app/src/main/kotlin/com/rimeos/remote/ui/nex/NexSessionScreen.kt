package com.rimeos.remote.ui.nex

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexStates
import com.rimeos.remote.core.nex.NexTranscript
import com.rimeos.remote.ui.agent.Strip
import com.rimeos.remote.ui.theme.MachineText
import com.rimeos.remote.ui.theme.RimeTones

/**
 * One nex session: what was said, what its tools did, what is waiting for
 * you, and a box to say the next thing.
 *
 * The reply being written arrives word by word from the live feed and is
 * drawn under the transcript until the message is complete; then the
 * transcript has it and the live copy goes.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NexSessionScreen(
    session: String,
    state: NexUiState,
    onSend: (String, () -> Unit) -> Unit,
    onStop: () -> Unit,
    onApprove: (NexApproval) -> Unit,
    onDeny: (NexApproval) -> Unit,
    onRefresh: () -> Unit,
    onDismiss: () -> Unit,
    onBack: () -> Unit,
) {
    val transcript = state.transcript?.takeIf { it.session.id == session }
    val listed = state.sessions.firstOrNull { it.id == session }
    val title = transcript?.session?.label ?: listed?.label ?: "Session"
    val running = transcript?.running ?: listed?.running ?: false
    val pending = state.pendingFor(session)
    var draft by rememberSaveable(session) { mutableStateOf("") }
    val list = rememberLazyListState()
    val rows = (transcript?.entries?.size ?: 0) + if (state.typing.isNotEmpty()) 1 else 0

    // Follow the bottom as the session writes, the way a terminal does.
    val total = rows + pending.size
    LaunchedEffect(total, state.typing.length / 200) {
        if (total > 0) list.animateScrollToItem(total - 1)
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            listOfNotNull(
                                when {
                                    pending.isNotEmpty() -> "waiting for you"
                                    else -> (transcript?.session?.state ?: listed?.state)?.let(NexStates::words)
                                },
                                (transcript?.session?.provider ?: listed?.provider),
                                folderName(transcript?.session?.root ?: listed?.root),
                            ).joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                actions = {
                    if (running) {
                        TextButton(onClick = onStop, enabled = !state.sending) { Text("Stop") }
                    } else {
                        TextButton(onClick = onRefresh) { Text("Refresh") }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).imePadding()) {
            state.failure?.let { Strip(it, alarming = true, onDismiss = onDismiss) }
            state.notice?.let { Strip(it, alarming = false, onDismiss = onDismiss) }
            state.away?.let { AwayNotice(it, onRefresh) }

            LazyColumn(Modifier.weight(1f), state = list) {
                if (transcript == null && state.away == null) {
                    item {
                        Text(
                            "Loading…",
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(16.dp),
                        )
                    }
                }
                transcript?.entries?.forEachIndexed { i, e -> item(key = "e$i") { Entry(e) } }
                if (state.typing.isNotEmpty()) {
                    item(key = "typing") { Typing(state.typing) }
                }
                for (a in pending) {
                    item(key = "a${a.id}") {
                        ApprovalCard(
                            approval = a,
                            sessionLabel = null,
                            deciding = a.id in state.deciding,
                            onApprove = { onApprove(a) },
                            onDeny = { onDeny(a) },
                        )
                    }
                }
            }

            Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.Bottom) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    label = { Text(if (running) "Say something when it is done" else "Message") },
                    modifier = Modifier.weight(1f),
                    maxLines = 6,
                    enabled = state.away == null,
                )
                Spacer(Modifier.width(8.dp))
                Button(
                    onClick = { onSend(draft) { draft = "" } },
                    enabled = draft.isNotBlank() && !state.sending && state.away == null,
                ) { Text(if (state.sending) "Sending…" else "Send") }
            }
        }
    }
}

@Composable
private fun Entry(e: NexTranscript.Entry) {
    when (e) {
        is NexTranscript.Entry.Said -> Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
            Text(
                if (e.role == "user") "You" else "nex",
                style = MaterialTheme.typography.labelSmall,
                color = if (e.role == "user") MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(e.text, style = MaterialTheme.typography.bodyMedium)
        }
        is NexTranscript.Entry.ToolUse -> Column(Modifier.fillMaxWidth().padding(start = 28.dp, end = 16.dp, top = 4.dp, bottom = 4.dp)) {
            val tone = when {
                e.error || e.state in setOf("failed", "denied", "timed_out") -> RimeTones.forState(NexStates.agentState("failed"))
                else -> MaterialTheme.colorScheme.onSurfaceVariant
            }
            Text(
                listOfNotNull(
                    e.name,
                    e.state?.let(NexStates::words),
                    e.exitCode?.let { "exit $it" },
                ).joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
                color = tone,
            )
            if (e.args.isNotBlank()) Text(e.args, style = MachineText, maxLines = 3, overflow = TextOverflow.Ellipsis)
            e.result?.takeIf { it.isNotBlank() }?.let {
                Text(
                    it,
                    style = MachineText,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 6,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        is NexTranscript.Entry.TurnEnd -> Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp)) {
            Text(
                listOfNotNull(
                    "Turn " + NexStates.words(e.state),
                    e.verification?.let { NexStates.words(it) },
                ).joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            e.verificationReason?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Spacer(Modifier.height(4.dp))
        }
    }
}

@Composable
private fun Typing(text: String) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            // Polite: a screen reader reads what arrived when it is free,
            // rather than interrupting itself for every word.
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Text("nex, writing", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(text, style = MaterialTheme.typography.bodyMedium)
    }
}
