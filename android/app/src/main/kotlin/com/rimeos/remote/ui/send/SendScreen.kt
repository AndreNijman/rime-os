package com.rimeos.remote.ui.send

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.ui.agent.Strip

/**
 * "Send to Rime": what was shared, and which computer it would go to.
 *
 * Arriving here sends nothing. The person sees the text or the files, picks a
 * computer, and taps Send.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SendScreen(
    state: SendUiState,
    onChoose: (String) -> Unit,
    onSend: () -> Unit,
    onStop: () -> Unit,
    onDismiss: () -> Unit,
    onDone: () -> Unit,
) {
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Send to Rime", style = MaterialTheme.typography.titleMedium, maxLines = 1) },
                navigationIcon = {
                    TextButton(onClick = onDone, enabled = !state.sending) { Text(if (state.done) "Done" else "Cancel") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState())) {
            state.failure?.let { Strip(it, alarming = true, onDismiss = if (state.done) null else onDismiss) }
            state.notice?.let { Strip(it, alarming = false) }

            if (state.empty) {
                Line("Nothing that can be sent was shared.")
                if (state.refused > 0) Line(refusedLine(state.refused), quiet = true)
                return@Column
            }

            Heading(if (state.text != null) "Text for the clipboard" else "Files for Downloads")
            state.text?.let { text ->
                Text(
                    text,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 8,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
                state.textProblem?.let { Line(it) }
            }
            if (state.describing) Line("Reading what was shared…", quiet = true)
            state.files.forEachIndexed { i, f -> FileRow(f, state, i) }
            if (state.refused > 0) Line(refusedLine(state.refused), quiet = true)

            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Heading("Send to")
            if (state.machines.isEmpty()) {
                Line("This phone is not paired with a computer yet. Pair one from the Machines screen first.")
                return@Column
            }
            Column(Modifier.selectableGroup()) {
                for (m in state.machines) {
                    val picked = m.deviceId == state.chosen
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = picked,
                                enabled = !state.sending && !state.done,
                                role = Role.RadioButton,
                                onClick = { onChoose(m.deviceId) },
                            )
                            .padding(horizontal = 8.dp, vertical = 2.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = picked, onClick = null, enabled = !state.sending && !state.done)
                        Text(m.name, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.padding(start = 8.dp))
                    }
                }
            }

            if (state.sending && state.text == null) {
                OutlinedButton(
                    onClick = onStop,
                    shape = RoundedCornerShape(6.dp),
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                ) { Text("Stop") }
            } else if (!state.done) {
                Button(
                    onClick = onSend,
                    enabled = state.canSend,
                    shape = RoundedCornerShape(6.dp),
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                ) { Text(sendLabel(state)) }
                Line(
                    if (state.text != null) {
                        "It replaces what the computer has copied."
                    } else {
                        "Files go to the computer's Downloads folder. A file with the same name is never replaced."
                    },
                    quiet = true,
                )
            }
        }
    }
}

private fun sendLabel(state: SendUiState): String {
    val to = state.chosenName()?.let { " to $it" } ?: ""
    return when {
        state.sending -> "Sending…"
        state.text != null -> "Copy$to"
        state.sendable.size == 1 -> "Send 1 file$to"
        else -> "Send ${state.sendable.size} files$to"
    }
}

private fun refusedLine(n: Int): String =
    if (n == 1) "1 shared item is not a file this app may read, so it is left out."
    else "$n shared items are not files this app may read, so they are left out."

@Composable
private fun FileRow(f: SendFile, state: SendUiState, index: Int) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text(f.label, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
        val detail = when {
            f.problem != null -> f.problem
            state.saved.size > state.sendable.indexOf(index) && state.sendable.indexOf(index) >= 0 ->
                "Saved as ${state.saved[state.sendable.indexOf(index)].name}"
            f.size != null -> SendUiState.megabytes(f.size)
            else -> null
        }
        detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        val size = f.size
        if (state.current == index && size != null && size > 0) {
            LinearProgressIndicator(
                progress = { (state.sent.toFloat() / size.toFloat()).coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
            )
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun Line(text: String, quiet: Boolean = false) {
    Text(
        text,
        style = if (quiet) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
        color = if (quiet) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
    )
}
