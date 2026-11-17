package com.rimeos.remote.device

import androidx.compose.runtime.Composable
import com.rimeos.remote.core.PairedMachine
import com.rimeos.remote.core.agent.AgentSession
import com.rimeos.remote.core.agent.Hello
import com.rimeos.remote.ui.ApprovalsUiState
import com.rimeos.remote.ui.PickerUiState
import com.rimeos.remote.ui.MachinesScreen
import com.rimeos.remote.ui.UiState
import com.rimeos.remote.ui.WorktreesUiState
import com.rimeos.remote.ui.agent.AgentCenterScreen
import com.rimeos.remote.ui.agent.ApprovalsScreen
import com.rimeos.remote.ui.agent.HelpScreen
import com.rimeos.remote.ui.agent.StartAgentScreen
import com.rimeos.remote.ui.agent.WorktreesScreen

/**
 * Every screen this app has that can be composed without a live socket, with
 * the state it would really hold, named once.
 *
 * Named once because the accessibility pass, the large-text pass and the
 * landscape pass must all walk the SAME list. Three lists would drift, and the
 * screen that fell off one of them would be the screen nobody checked.
 *
 * What is NOT here is [com.rimeos.remote.ui.term.TerminalScreen], and it is
 * left out rather than stubbed: it is driven by a `TerminalController` that
 * owns a PTY attachment, and half a controller would make the checks below
 * assert something no user ever sees. It has its own walks instead —
 * `TerminalHarnessOnDeviceTest` for the semantics tree and the accessibility
 * framework, `TerminalOnDeviceTest` for a real PTY over Wi-Fi — because both
 * need a controller that is whole.
 */
object Screens {

    private val machine = PairedMachine(
        deviceId = "abcdef0123456789",
        machine = "l16",
        desktopKey = "LBkUC9WKrgavtk9Tc-D8-A42upjIotTORE17jKQgM38",
        deviceKey = "0000000000000000000000000000000000000000000",
        sealed = "c2VhbGVk",
        lan = listOf("192.168.1.232:7717"),
        pairedMs = 1_750_000_000_000L,
    )

    private val sessions = listOf(
        AgentSession(
            id = 1,
            agent = "claude",
            program = "claude",
            cwd = "/home/andre/Projects/rime",
            project = "/home/andre/Projects/rime",
            projectName = "rime",
            state = "waiting",
            detail = "needs your answer",
        ),
        AgentSession(
            id = 2,
            agent = "codex",
            program = "codex",
            cwd = "/home/andre/Projects/rime-shell",
            state = "running",
        ),
    )

    /** Every screen, as (name, composable). */
    val all: List<Pair<String, @Composable () -> Unit>> = listOf(
        "Machines" to {
            MachinesScreen(
                state = UiState(loading = false, machines = listOf(machine), unlocked = true),
                onPair = {}, onConnect = {}, onPing = {}, onForget = {},
                onCrashReports = {}, onShareCrash = {}, onClearCrash = {},
                onDynamicColour = {}, onLock = {}, onDismiss = {},
            )
        },
        "Agent Center" to {
            AgentCenterScreen(
                machine = "l16",
                sessions = sessions,
                nowSeconds = 1_750_000_100L,
                busy = null,
                failure = null,
                onRefresh = {}, onOpen = {}, onStart = {}, onProjects = {},
                onApprovals = {}, onHelp = {}, onBack = {}, onDismiss = {},
            )
        },
        "Start an agent" to {
            StartAgentScreen(
                machine = "l16",
                hello = Hello(
                    version = 10,
                    agents = listOf("claude", "codex", "generic"),
                    defaultAgent = "claude",
                ),
                knownDirectories = listOf("/home/andre/Projects/rime"),
                picker = PickerUiState(),
                busy = null,
                failure = null,
                onLoad = {}, onProject = {},
                onStart = { _, _, _, _, _, _ -> }, onBack = {}, onDismiss = {},
            )
        },
        "Approvals" to {
            ApprovalsScreen(
                state = ApprovalsUiState(loading = false),
                nowSeconds = 1_750_000_100L,
                onLoad = {}, onRevokeGrant = { _, _ -> }, onRevokeSystemGrant = {}, onBack = {},
            )
        },
        "Projects" to {
            WorktreesScreen(
                machine = "l16",
                state = WorktreesUiState(loading = false, askedSeconds = 1_750_000_050L),
                liveSessions = setOf(1),
                onLoad = {}, onOpenSession = {}, onBack = {},
            )
        },
        "Guide" to { HelpScreen(onBack = {}) },
    )
}
