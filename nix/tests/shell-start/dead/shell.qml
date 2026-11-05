// A shell that dies on every start (SIGKILL from a child, as a crash would).
import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
    Process {
        running: true
        command: ["sh", "-c", "kill -KILL $PPID"]
    }
}
