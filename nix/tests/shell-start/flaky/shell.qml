// A shell that dies on its first start and comes up on a retried one (the launcher sets
// RIME_SHELL_START_ATTEMPT on a retry). The death is a SIGKILL from a child: a crash, as far as anyone can tell.
import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
    id: root
    readonly property bool retried: !!Quickshell.env("RIME_SHELL_START_ATTEMPT")
    Process {
        running: !root.retried
        command: ["sh", "-c", "kill -KILL $PPID"]
    }
    LazyLoader {
        active: root.retried
        IpcHandler {
            target: "probe"
            function ping(): string { return "pong"; }
        }
    }
}
