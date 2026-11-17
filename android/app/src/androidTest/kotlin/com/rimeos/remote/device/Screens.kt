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
import com.rimeos.remote.core.nex.NexApproval
import com.rimeos.remote.core.nex.NexConnection
import com.rimeos.remote.core.nex.NexMission
import com.rimeos.remote.core.nex.NexMissionDetail
import com.rimeos.remote.core.nex.NexSession
import com.rimeos.remote.core.nex.NexTranscript
import com.rimeos.remote.core.nex.NexUsage
import com.rimeos.remote.ui.nex.NexNewSessionScreen
import com.rimeos.remote.ui.nex.NexScreen
import com.rimeos.remote.ui.nex.NexSessionScreen
import com.rimeos.remote.ui.nex.NexUiState
import com.rimeos.remote.core.machine.Battery
import com.rimeos.remote.core.machine.MachineStatus
import com.rimeos.remote.core.machine.MediaState
import com.rimeos.remote.core.machine.Player
import com.rimeos.remote.core.machine.Volume
import com.rimeos.remote.ui.machine.MachineScreen
import com.rimeos.remote.ui.send.SendChoice
import com.rimeos.remote.ui.send.SendFile
import com.rimeos.remote.ui.send.SendScreen
import com.rimeos.remote.ui.send.SendUiState
import com.rimeos.remote.ui.machine.MachineUiState

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

    // nex, with every section showing: an approval waiting, a running and an
    // idle session, an open mission, usage, and an open transcript with a
    // reply being written. Long names on purpose, for the large-text walk.
    private val nexRoot = "/home/andre/Projects/rime/a-folder-with-a-long-name"
    private val nexSessions = listOf(
        NexSession("s1", "Fix the failing operator test in lib.sh", "running", "build", "scripted", nexRoot, "2026-11-17T04:00:00Z"),
        NexSession("s2", null, "idle", "build", "claude-code", nexRoot, "2026-11-16T04:00:00Z"),
    )
    private val nexApproval = NexApproval(
        "a1", "s1", "run_command", "run `bash test.sh && git diff --stat -- lib.sh`", listOf("process.exec"),
        listOf("mode build default: Ask"), "fp", "2026-11-17T05:00:00Z", false,
    )
    private val nexMission = NexMission("m1", "Add a subtract() helper with tests", "active", "2026-11-17T04:10:00Z")
    private val nexTranscript = NexTranscript(
        nexSessions[0],
        listOf(
            NexTranscript.Entry.Said("user", "add() subtracts; fix it and run the tests", "t1"),
            NexTranscript.Entry.Said("assistant", "Fixing the operator.", "t1"),
            NexTranscript.Entry.ToolUse("edit_file", "{\"path\":\"lib.sh\"}", "succeeded", null, false, "edited lib.sh", "t1"),
            NexTranscript.Entry.TurnEnd("completed", "Fixed.", "verified", null, "t0"),
        ),
        listOf(nexApproval),
        12,
    )
    private val nexState = NexUiState(
        machine = machine.deviceId,
        loaded = true,
        live = true,
        sessions = nexSessions,
        pending = listOf(nexApproval),
        missions = listOf(nexMission),
        missionDetails = mapOf(
            "m1" to NexMissionDetail(nexMission, listOf(NexMissionDetail.Task("impl", "running", 1, null, "s1")), null, listOf("bash test.sh")),
        ),
        usage = listOf(NexUsage("claude-code", listOf(NexUsage.Tokens("input", 12_345.0, false)), listOf(NexUsage.Quota("5-hour window", 42.0, "5h", null, false)))),
        connections = listOf(NexConnection("scripted", "scripted", true, false), NexConnection("claude-code", "claude-code", true, true)),
        selected = "s1",
        transcript = nexTranscript,
        typing = "Running the tests now, then I will",
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
        "This computer" to {
            MachineScreen(
                state = MachineUiState(
                    machine = "l16",
                    offersStatus = true,
                    offersLock = true,
                    offersMedia = true,
                    offersClipboard = true,
                    loaded = true,
                    status = MachineStatus(Battery(81, "charging"), true, "balanced", false, "restart"),
                    media = MediaState(
                        Player("spotify", "playing", "A song whose title runs on far longer than any screen is wide", "An artist with a long name"),
                        Volume(40, false),
                    ),
                ),
                onLock = {}, onMedia = {}, onVolume = {}, onMute = {},
                onRefresh = {}, onDismiss = {}, onBack = {},
            )
        },
        "Send text" to {
            SendScreen(
                state = SendUiState(
                    text = "A copied paragraph long enough to wrap onto a second and a third line on a phone held upright",
                    machines = listOf(SendChoice("d1", "l16"), SendChoice("d2", "a desktop with a long name")),
                    chosen = "d1",
                ),
                onChoose = {}, onSend = {}, onStop = {}, onDismiss = {}, onDone = {},
            )
        },
        "Send files" to {
            SendScreen(
                state = SendUiState(
                    files = listOf(
                        SendFile("IMG_20261117_081530_holiday_photo_with_a_long_name.jpg", 3_400_000, null),
                        SendFile("notes.pdf", 220_000, null),
                        SendFile(null, null, "The app that shared it did not say how big it is, so it cannot be sent."),
                    ),
                    refused = 1,
                    machines = listOf(SendChoice("d1", "l16")),
                    chosen = "d1",
                    sending = true,
                    current = 0,
                    sent = 1_200_000,
                ),
                onChoose = {}, onSend = {}, onStop = {}, onDismiss = {}, onDone = {},
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
                onNex = {},
                onComputer = {},
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
        "nex" to {
            NexScreen(
                machine = "l16", state = nexState,
                onOpen = {}, onNew = {}, onApprove = {}, onDeny = {}, onMission = { _, _ -> },
                onToggleMission = {}, onRefresh = {}, onDismiss = {}, onBack = {},
            )
        },
        "nex session" to {
            NexSessionScreen(
                session = "s1", state = nexState,
                onSend = { _, _ -> }, onStop = {}, onApprove = {}, onDeny = {}, onRefresh = {}, onDismiss = {}, onBack = {},
            )
        },
        "New nex session" to {
            NexNewSessionScreen(state = nexState, onCreate = { _, _, _, _ -> }, onCreated = {}, onDismiss = {}, onBack = {})
        },
    )
}
