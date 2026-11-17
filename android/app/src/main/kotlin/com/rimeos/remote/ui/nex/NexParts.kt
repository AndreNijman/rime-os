package com.rimeos.remote.ui.nex

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexStates
import com.rimeos.remote.ui.theme.MachineText
import com.rimeos.remote.ui.theme.RimeTones

@Composable
internal fun NexHeading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp)
            .semantics { heading() },
    )
}

/**
 * One approval nex is waiting on: what the tool would do, what it needs, and
 * the two answers.
 *
 * The summary is nexd's own sentence ("run `bash test.sh`"), and the scopes
 * are what the action reaches. Approve sends the fingerprint the card was
 * drawn from, so a yes is a yes to what is on the screen: if the action
 * changed since, nexd refuses it rather than running something else.
 */
@Composable
internal fun ApprovalCard(
    approval: NexApproval,
    sessionLabel: String?,
    deciding: Boolean,
    onApprove: () -> Unit,
    onDeny: () -> Unit,
) {
    val tone = RimeTones.forState(NexStates.agentState("approval_pending"))
    Row(Modifier.fillMaxWidth().padding(end = 16.dp, top = 10.dp, bottom = 10.dp)) {
        Box(Modifier.width(3.dp).heightIn(min = 72.dp).background(tone))
        Spacer(Modifier.width(13.dp))
        Column(Modifier.weight(1f)) {
            Text(approval.summary, style = MaterialTheme.typography.titleSmall)
            Text(
                listOfNotNull(
                    approval.tool,
                    sessionLabel,
                    approval.scopes.takeIf { it.isNotEmpty() }?.let { "needs " + it.joinToString(", ") },
                ).joinToString(" · "),
                style = MachineText,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            for (reason in approval.reasons.take(3)) {
                Text(
                    reason,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onApprove, enabled = !deciding) { Text(if (deciding) "Sending…" else "Approve") }
                OutlinedButton(onClick = onDeny, enabled = !deciding) { Text("Deny") }
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.surfaceVariant)
}

/** What stands where nex would be when the computer's nex cannot be reached. */
@Composable
internal fun AwayNotice(why: String, onRetry: () -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth().padding(12.dp),
        shape = MaterialTheme.shapes.small,
    ) {
        Column(Modifier.padding(12.dp)) {
            Text("nex is not reachable on this computer", style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(4.dp))
            // The bridge's words, which say which of the two it is: not set up,
            // or set up and not running.
            Text(why, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(4.dp))
            Text(
                "Start it on the computer with `nexd`, or check `rime-remoted` was started with `--nexd`.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = onRetry) { Text("Try again") }
        }
    }
}

/** A mission action in words a person would press. */
internal fun missionVerbLabel(verb: String): String = when (verb) {
    "missions.approve" -> "Approve plan"
    "missions.start" -> "Start"
    "missions.pause" -> "Pause"
    "missions.resume" -> "Resume"
    "missions.cancel" -> "Cancel mission"
    else -> verb.substringAfter('.')
}

/** A folder path's last part, for rows where the whole path does not fit. */
internal fun folderName(path: String?): String? =
    path?.trimEnd('/')?.substringAfterLast('/')?.takeIf { it.isNotEmpty() }

/** Token counts as people read them: 1.2k, 3.4M. */
internal fun compactCount(n: Double): String = when {
    n >= 1_000_000 -> "%.1fM".format(n / 1_000_000)
    n >= 1_000 -> "%.1fk".format(n / 1_000)
    else -> n.toLong().toString()
}
