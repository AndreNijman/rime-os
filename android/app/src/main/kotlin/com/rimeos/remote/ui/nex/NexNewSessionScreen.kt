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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
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
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.nex.NexConnection
import com.rimeos.remote.ui.agent.Strip
import com.rimeos.remote.ui.theme.MachineText

/**
 * A new nex session from the phone: which folder, which provider, and what
 * to do first.
 *
 * Only folders that already have a session are offered. That is nexd's rule
 * for a phone, not this screen's: a device may not point an agent at a
 * folder nobody at the computer has chosen. The provider starts as the one
 * that folder used last, if it is still connected.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NexNewSessionScreen(
    state: NexUiState,
    onCreate: (root: String, provider: String?, title: String?, firstMessage: String) -> Unit,
    onCreated: (String) -> Unit,
    onDismiss: () -> Unit,
    onBack: () -> Unit,
) {
    val roots = state.roots
    var root by rememberSaveable { mutableStateOf(roots.firstOrNull()) }
    val usable = state.connections.filter { it.available }
    var provider by rememberSaveable(root) {
        mutableStateOf(root?.let { NexConnection.suggest(it, state.sessions, state.connections) })
    }
    var title by rememberSaveable { mutableStateOf("") }
    var message by rememberSaveable { mutableStateOf("") }

    LaunchedEffect(state.created) { state.created?.let(onCreated) }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("New nex session", style = MaterialTheme.typography.titleMedium) },
                navigationIcon = { TextButton(onClick = onBack) { Text("Back") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding()
                .verticalScroll(rememberScrollState()),
        ) {
            state.failure?.let { Strip(it, alarming = true, onDismiss = onDismiss) }

            NexHeading("Folder")
            Column(Modifier.selectableGroup()) {
                for (r in roots) {
                    Choice(selected = r == root, onSelect = { root = r }) {
                        Text(folderName(r) ?: r, style = MaterialTheme.typography.bodyMedium)
                        Text(r, style = MachineText, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                    }
                }
            }

            if (usable.isNotEmpty()) {
                NexHeading("Provider")
                Column(Modifier.selectableGroup()) {
                    for (c in usable) {
                        Choice(selected = c.id == provider, onSelect = { provider = c.id }) {
                            Text(c.id, style = MaterialTheme.typography.bodyMedium)
                            Text(
                                if (c.native) "its own agent, run by nex" else c.kind,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }

            Column(Modifier.padding(16.dp)) {
                OutlinedTextField(
                    value = title,
                    onValueChange = { title = it },
                    label = { Text("Name (optional)") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = message,
                    onValueChange = { message = it },
                    label = { Text("What should it do first? (optional)") },
                    minLines = 3,
                    maxLines = 8,
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
                Button(
                    onClick = { root?.let { onCreate(it, provider, title, message) } },
                    enabled = root != null && !state.sending && state.away == null,
                ) { Text(if (state.sending) "Starting…" else "Start session") }
            }
        }
    }
}

@Composable
private fun Choice(selected: Boolean, onSelect: () -> Unit, content: @Composable () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .selectable(selected = selected, onClick = onSelect, role = Role.RadioButton)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The row is the control, so the radio itself is not a second one.
        RadioButton(selected = selected, onClick = null)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) { content() }
    }
}
