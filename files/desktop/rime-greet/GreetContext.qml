import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.Greetd

// ─────────────────────────────────────────────────────────────
// GreetContext — shared, non-visual greeter state.
//
// One instance lives at the shell root; every per-output GreetSurface
// binds to it. Holds the input buffers, the live clock, and (in later
// commits) the Greetd auth backend and session list. Keeping the
// state here — rather than per-surface as the Lockscreen did — means a
// multi-output kiosk mirrors one coherent auth attempt.
// ─────────────────────────────────────────────────────────────

Item {
    id: ctx

    // ── Auth / input state ────────────────────────────────────────
    property string username:  ""
    property string password:  ""
    property bool   checking:   false   // auth conversation in flight
    property bool   hasError:   false   // last attempt failed
    property string errorText:  ""

    // Emitted on every failed attempt so each surface can shake its
    // card and clear the field (the Lockscreen did this inline; here it
    // fans out to N surfaces).
    signal failed()

    // ── Edition ("gaming" | "daily" | "mono") ─────────────────────
    property string edition: "mono"

    // ── Session model ─────────────────────────────────────────────
    // sessions: [{ id, name, exec, desktopNames }] parsed from wayland-sessions.
    property var    sessions:     []
    property int    sessionIndex: 0
    property string _wantSession: ""    // desired id from last-session

    // ── Recovery preselection (roadmap P2-018) ────────────────────
    // The ONLY session id this greeter will ever select on its own initiative.
    // /usr/libexec/rime-session-watchdog carries the same constant and
    // tests/test-rime-session-watchdog.sh fails if the two drift apart. Named
    // rather than discovered for the reason `defaultSession` is named: a
    // greeter that acted on whatever a helper printed would be one typo away
    // from selecting something nobody can log in to.
    readonly property string recoverySession: "rime-safe-graphics"
    property string _recoverWanted: ""   // set only by the watchdog's `check`
    // Why recovery was preselected, shown on the greeter's status line. Set
    // ONLY when the id actually matched an enumerated session — a notice for a
    // session that is not in the picker is a dead end with an explanation.
    property string recoveryNotice: ""
    // The user has worked the session picker. From that moment their choice is
    // final: `_selectWanted()` stops selecting anything, so neither a late line
    // from the enumeration nor the watchdog can move the picker out from under
    // them. This is what makes the recovery preselection a suggestion.
    property bool   _userPicked:  false

    readonly property var currentSession:
        (sessions.length > 0 && sessionIndex >= 0 && sessionIndex < sessions.length)
            ? sessions[sessionIndex] : null
    readonly property string sessionName: currentSession ? currentSession.name : "Default"
    // Command line of the selected session; a login shell is the safe
    // fallback when no wayland-sessions exist (e.g. the dev box).
    readonly property string sessionCommand: currentSession ? currentSession.exec : ""

    function cycleSession(dir) {
        var n = ctx.sessions.length
        if (n === 0) return
        // Touching the picker settles the question. Before this flag existed
        // every `_addSession` line re-ran `_selectWanted()`, so a choice made
        // while the enumeration was still arriving could be silently undone;
        // with the watchdog able to preselect as well, a user who cycles away
        // from recovery must not be put back into it.
        ctx._userPicked     = true
        ctx._recoverWanted  = ""
        ctx.recoveryNotice  = ""
        ctx.sessionIndex = ((ctx.sessionIndex + dir) % n + n) % n
    }

    function _addSession(line) {
        var t = line.trim()
        if (t === "") return
        var parts = t.split("\t")
        if (parts.length < 3) return
        var exec = parts[2].replace(/%[a-zA-Z]/g, "").trim()   // strip field codes
        if (exec === "") return
        var list = ctx.sessions.slice()
        list.push({ id: parts[0], name: parts[1], exec: exec,
                    desktopNames: parts.length > 3 ? parts[3].trim() : "" })
        ctx.sessions = list
        ctx._selectWanted()
    }

    // The session that wins when this machine has no last-session memory yet.
    //
    // WHY THIS IS EXPLICIT AND NOT "index 0": sessions are enumerated by a
    // sorted shell glob over /usr/share/wayland-sessions, and sessionIndex
    // defaults to 0 — so the DEFAULT SESSION was whichever .desktop file sorted
    // first alphabetically. Adding one is enough to silently change what every
    // fresh install boots into. That already cost this project a lockout once:
    // hyprland-uwsm.desktop sorted before hyprland.desktop, became the default,
    // and bounce-looped at login on real hardware (see Containerfile.base).
    //
    // Every image now ships rime-gaming.desktop, which sorts before
    // BOTH of them. So the default is named, not positional, and adding a
    // session can no longer change it by accident.
    readonly property string defaultSession: "hyprland"

    function _selectWanted() {
        // The user's own choice outranks every rule below it.
        if (ctx._userPicked) return

        // Recovery outranks the remembered session, because the remembered
        // session is the one that has just failed to start three times running.
        // The id is checked against `recoverySession` at the point it is read
        // (see the watchdog Process below), so this loop can only ever land on
        // Rime Safe Graphics — and only if it is actually in the picker.
        if (ctx._recoverWanted) {
            for (var r = 0; r < ctx.sessions.length; r++)
                if (ctx.sessions[r].id === ctx._recoverWanted) {
                    ctx.sessionIndex   = r
                    ctx.recoveryNotice = "Your desktop did not start — "
                                       + ctx.sessions[r].name + " selected"
                    return
                }
            // Not installed: say nothing and fall through. A notice about a
            // session the user cannot choose is worse than no notice.
        }

        // A remembered session wins — WHILE IT IS STILL INSTALLED.
        //
        // The `return` that used to sit where the comment below is turned this
        // into the same lockout the block above describes. `last-session` is
        // written on every login, so after one trip through Gaming Mode it
        // reads `rime-gaming` — and the TryExec gate removes that entry the
        // moment gamescope is absent, which is every boot where the sysext has
        // not merged yet. The remembered id then matched nothing, this function
        // returned having selected nothing, and sessionIndex stayed at its
        // default 0: the FIRST SORTED ENTRY, which is exactly the positional
        // default that `defaultSession` exists to abolish.
        //
        // So a user who once tried Gaming Mode was silently moved to whatever
        // sorts first — rime-labwc on a built image — and the named default
        // they would otherwise have landed on was never consulted, because the
        // protection below was only ever wired to the empty-memory path.
        //
        // A memory written before the rename names the session by its APEX id
        // (apex-labwc, apex-gaming): last-session lives in /var/lib and outlives
        // the image that wrote it. The entries are rime-* now, so an apex-* id
        // is also tried under its new name — otherwise everyone who used
        // Floating would land on the default at their first Rime login.
        if (ctx._wantSession !== "") {
            var wants = [ctx._wantSession]
            if (ctx._wantSession.indexOf("apex-") === 0)  // rime-rename: keep — ids remembered before the rename
                wants.push("rime-" + ctx._wantSession.slice(5))
            for (var w = 0; w < wants.length; w++)
                for (var i = 0; i < ctx.sessions.length; i++)
                    if (ctx.sessions[i].id === wants[w]) { ctx.sessionIndex = i; return }
            // Remembered but no longer installed: fall through to the default.
        }
        // Otherwise fall back to the named default rather than glob position.
        for (var j = 0; j < ctx.sessions.length; j++)
            if (ctx.sessions[j].id === ctx.defaultSession) { ctx.sessionIndex = j; return }
    }

    // ── Live clock (local ticker, mirrors the Lockscreen) ─────────
    property string timeText: Qt.formatDateTime(new Date(), "hh:mm")
    property string dateText: Qt.formatDateTime(new Date(), "dddd, d MMMM")
    Timer {
        interval: 1000
        running:  true
        repeat:   true
        onTriggered: {
            var now = new Date()
            ctx.timeText = Qt.formatDateTime(now, "hh:mm")
            ctx.dateText = Qt.formatDateTime(now, "dddd, d MMMM")
        }
    }

    // ── Helpers ───────────────────────────────────────────────────
    function fail(msg) {
        ctx.password  = ""
        ctx.checking  = false
        ctx.hasError  = true
        ctx.errorText = msg
        ctx.failed()
    }

    // ── Greetd auth conversation ──────────────────────────────────
    //
    // PamContext → Greetd mapping (the Lockscreen used PAM directly; a
    // greeter must talk to greetd, which owns the PAM conversation):
    //
    //   Lockscreen (PAM)                 rime-greet (Greetd)
    //   ────────────────                 ───────────────────
    //   pam.start()                      Greetd.createSession(username)
    //   onResponseRequired → respond()   onAuthMessage(responseRequired)
    //                                        → Greetd.respond(password)
    //   onCompleted(Success) → unlock    onReadyToLaunch → Greetd.launch(argv)
    //   onCompleted(fail)    → fail()    onAuthFailure   → fail()
    //   onError              → fail()    onError         → fail()
    //
    // Submit: begins (or continues) the conversation.
    function tryAuth() {
        if (ctx.checking) return
        if (ctx.username.length === 0) { ctx.fail("Enter a username"); return }
        if (ctx.password.length === 0) return
        if (!Greetd.available)         { ctx.fail("greetd is not available"); return }
        ctx.hasError  = false
        ctx.errorText = ""
        ctx.checking  = true
        if (Greetd.state === GreetdState.Inactive) {
            // Fresh attempt — greetd replies with an auth message that
            // onAuthMessage answers with the buffered password.
            Greetd.createSession(ctx.username)
        } else {
            // A prompt is already outstanding (rare) — answer it directly.
            Greetd.respond(ctx.password)
        }
    }

    // Hand the chosen session to greetd. greetd opens the PAM session
    // and execs the command; quickshell exits. A login shell wraps the
    // Exec line so PATH / profile resolve, mirroring common greeters.
    //
    // last-user / last-session are written FIRST (values passed via env
    // so a hostile username can't inject shell), and the actual launch
    // is deferred to persistProc.onExited so the write flushes before
    // quickshell exits. An unwritable state dir is tolerated.
    // ── The environment a session starts with ────────────────────
    // greetd hands it to PAM before the session opens, so logind records
    // it too. Every session here is a Wayland session: XDG_SESSION_TYPE=
    // wayland always (logind's Type, which rimed's session reading needs).
    // A desktop whose session file has DesktopNames (GNOME, Plasma, COSMIC)
    // also gets what GDM or SDDM would have given it: XDG_CURRENT_DESKTOP
    // (DesktopNames, ";" -> ":") and XDG_SESSION_DESKTOP (the session id);
    // gnome-session and the portals choose by them. Rime's own sessions set
    // their desktop variables themselves and keep exactly the environment
    // they had, so they get XDG_SESSION_TYPE only (docs/nixos/desktops.md
    // §4.1; tests/test-rime-greet-sessions.sh §7 runs this function).
    readonly property var ownSessions: ["hyprland", "niri", "rime-labwc", "rime-safe-graphics", "rime-gaming"]
    function _sessionEnv(s) {
        var env = ["XDG_SESSION_TYPE=wayland"]
        if (!s || ctx.ownSessions.indexOf(s.id) >= 0) return env
        var names = String(s.desktopNames || "").split(";").filter(function(n) { return n.trim() !== "" })
        if (names.length === 0) return env
        env.push("XDG_CURRENT_DESKTOP=" + names.join(":"))
        env.push("XDG_SESSION_DESKTOP=" + s.id)
        return env
    }

    // ── The command greetd runs ──────────────────────────────────
    // greetd 0.10 does not exec the argv it is handed: it runs
    // `/bin/sh -c "exec <argv joined by spaces>"` (greetd/src/session/
    // worker.rs). An unquoted ["sh", "-lc", cmd] therefore reached the shell
    // as `sh -lc <first word of cmd>` with the other words as $0, $1, so an
    // Exec of more than one word ran its first word alone: Plasma's
    // `plasma-dbus-run-session-if-needed …/startplasma-wayland` exited at
    // once, niri lost `--session`. The command is single-quoted for that
    // join, so `sh -lc` receives the session's Exec whole
    // (tests/test-rime-greet-sessions.sh §9 runs it through /bin/sh).
    function _shQuote(s) {
        return "'" + String(s).replace(/'/g, "'\\''") + "'"
    }
    function _sessionArgv(cmd) {
        return ["sh", "-lc", ctx._shQuote(cmd.length > 0 ? cmd : "exec ${SHELL:-/bin/sh} -l")]
    }

    property var _pendingArgv: null
    property var _pendingEnv: null
    function launch() {
        var cmd  = ctx.sessionCommand.trim()
        var argv = ctx._sessionArgv(cmd)
        var sid  = ctx.currentSession ? ctx.currentSession.id : ""
        ctx._pendingArgv = argv
        // No session at all (the login-shell fallback above): no environment, as before.
        ctx._pendingEnv  = ctx.currentSession ? ctx._sessionEnv(ctx.currentSession) : null
        persistProc.environment = {
            "AG_USER": ctx.username,
            "AG_SESS": sid,
            // RECOVERY IS NEVER REMEMBERED. last-session is written on every
            // login, so before this flag one trip through Rime Safe Graphics
            // made the recovery desktop the preselected default for ever —
            // a user who went there once to fix their machine came back to a
            // fixed machine and a picker still pointing at the rescue session.
            // Skipping the write leaves the PREVIOUS memory intact rather than
            // clearing it, so they land back on the desktop they were using.
            // Gaming Mode's equivalent problem is solved elsewhere and
            // differently (rime-session-select writes the file deliberately).
            "AG_REMEMBER": (sid !== "" && sid !== ctx.recoverySession) ? "1" : "0"
        }
        // The watchdog `record` goes LAST and is wrapped twice. It is on the
        // only path to Greetd.launch() — the launch is deferred to
        // persistProc.onExited — so a helper that hung here would be a machine
        // nobody can log in to. `timeout` caps it; `|| true` covers a `timeout`
        // that is not there to cap it with; and last-user/last-session are
        // already on disk before it is called, so even the capped case loses
        // nothing but the bounce count.
        persistProc.command = ["sh", "-c",
            "d=/var/lib/rime-greet; mkdir -p \"$d\" 2>/dev/null;" +
            " printf '%s' \"$AG_USER\" > \"$d/last-user\" 2>/dev/null || true;" +
            " [ \"${AG_REMEMBER:-1}\" = 1 ] &&" +
            "   printf '%s' \"$AG_SESS\" > \"$d/last-session\" 2>/dev/null;" +
            " timeout 5 /usr/libexec/rime-session-watchdog record \"$AG_SESS\"" +
            "   >/dev/null 2>&1 || true;" +
            " true"]
        persistProc.running = true
    }

    Process {
        id: persistProc
        onExited: function(exitCode, exitStatus) {
            if (ctx._pendingArgv) {
                var a = ctx._pendingArgv
                var e = ctx._pendingEnv
                ctx._pendingArgv = null
                ctx._pendingEnv = null
                if (e) Greetd.launch(a, e)
                else Greetd.launch(a)
            }
        }
    }

    Connections {
        target: Greetd

        // greetd relays a PAM prompt. Hidden prompts (echoResponse
        // false) are the password; visible prompts get the username.
        // error-type messages are surfaced without ending the session.
        function onAuthMessage(message, error, responseRequired, echoResponse) {
            if (responseRequired) {
                Greetd.respond(echoResponse ? ctx.username : ctx.password)
            } else if (error) {
                ctx.hasError  = true
                ctx.errorText = message
            }
        }

        // The one and only success path.
        function onReadyToLaunch() {
            ctx.launch()
        }

        // greetd has already torn the session down; reset to a clean
        // Inactive state so the next Enter starts fresh.
        function onAuthFailure(message) {
            ctx.fail(message && message.length > 0 ? message : "Wrong password")
        }

        function onError(message) {
            if (Greetd.state !== GreetdState.Inactive) Greetd.cancelSession()
            ctx.fail("Auth unavailable: " + message)
        }
    }

    // ── Edition detection ─────────────────────────────────────────
    // Precedence: /etc/rime-greet/edition override → /etc/os-release
    // VARIANT_ID → "mono" fallback (dev box / unset). Non-fatal.
    Process {
        id: editionProc
        running: true
        command: ["sh", "-c",
            "if [ -r /etc/rime-greet/edition ]; then head -n1 /etc/rime-greet/edition;" +
            " elif [ -r /etc/os-release ]; then . /etc/os-release; echo \"${VARIANT_ID:-}\"; fi"]
        stdout: SplitParser {
            onRead: function(line) {
                var v = line.trim().toLowerCase()
                // `rime` is the single published image (VARIANT_ID=rime).
                // `daily` and `gaming` are kept so a machine that has not yet
                // updated past the three-edition split still gets its accent
                // instead of falling through to mono.
                if (v === "rime" || v === "gaming" || v === "daily") ctx.edition = v
                else if (v !== "")                                   ctx.edition = "mono"
            }
        }
    }

    // ── Last user (prefill) ───────────────────────────────────────
    Process {
        running: true
        command: ["sh", "-c", "cat /var/lib/rime-greet/last-user 2>/dev/null; echo"]
        stdout: SplitParser {
            onRead: function(line) {
                var u = line.trim()
                if (u !== "" && ctx.username === "") ctx.username = u
            }
        }
    }

    // ── Wallpaper (the user's own, not the shipped default) ───────
    // The greeter used to hardcode /usr/share/backgrounds/rime/default.jpg, so
    // changing your wallpaper in Rime Shell moved the desktop and left the
    // login screen on the factory image forever. It could not have worked: /usr
    // is read-only and this process runs as the `greetd` user, which cannot
    // read mode-0700 home directories.
    //
    // Rime Shell now publishes its choice to /var/lib/rime-greet/wallpapers/
    // through a root helper (see /usr/libexec/rime-greet-wallpaper), and this
    // resolves <username>.{jpg,png,webp} there, falling back to the shipped
    // default. Resolution is by EXPLICIT extension rather than a glob so a
    // leftover temp file from a crashed publish can never be selected.
    readonly property string defaultWallpaper: "/usr/share/backgrounds/rime/default.jpg"
    property string wallpaperPath: ctx.defaultWallpaper
    readonly property url wallpaper: "file://" + ctx.wallpaperPath

    // Re-resolve as the username field changes, so typing a different account
    // greets you with THEIR wallpaper. Debounced: this spawns a process, and
    // doing that per keystroke would be silly.
    onUsernameChanged: wallpaperDebounce.restart()
    Timer {
        id: wallpaperDebounce
        interval: 400
        onTriggered: {
            if (!wallpaperProc.running) wallpaperProc.running = true
            if (!accentProc.running) accentProc.running = true
            if (!motionProc.running) motionProc.running = true
        }
    }

    Process {
        id: wallpaperProc
        running: true
        // The username goes in as an environment variable, never spliced into
        // the script — same rule as launch() below, and it applies here too
        // because this value can come straight from the text field.
        environment: ({ "AG_USER": ctx.username })
        command: ["sh", "-c",
            "d=/var/lib/rime-greet/wallpapers;" +
            " u=\"${AG_USER:-}\";" +
            " [ -n \"$u\" ] || u=\"$(cat /var/lib/rime-greet/last-user 2>/dev/null)\";" +
            // Anything outside this class cannot be a published filename, so
            // treat it as "no user" rather than letting it reach the glob.
            " case \"$u\" in ''|.*|*[!A-Za-z0-9._-]*) u='' ;; esac;" +
            " if [ -n \"$u\" ]; then for e in jpg png webp; do" +
            "   if [ -f \"$d/$u.$e\" ]; then printf '%s\\n' \"$d/$u.$e\"; exit 0; fi;" +
            " done; fi;" +
            " printf '%s\\n' /usr/share/backgrounds/rime/default.jpg"]
        stdout: SplitParser {
            onRead: function(line) {
                var p = line.trim()
                if (p !== "") ctx.wallpaperPath = p
            }
        }
    }

    // ── Password shapes: Rime Shell's own component, not a copy ───
    // The password field draws its feedback with the same PasswordShapes the
    // lock screen uses, loaded from the Rime Shell tree the image vendors to
    // /usr/share/rime-shell — one implementation, so the two screens cannot
    // drift. GreetSurface loads it through a Loader and keeps the plain masked
    // dots if it is missing (a shell older than the component, a dev box), so
    // a decoration can never be the reason a login screen fails to load.
    // RIME_GREET_SHELL_ROOT points at a checkout for the test suites.
    readonly property url shapesSource:
        "file://" + (Quickshell.env("RIME_GREET_SHELL_ROOT") || "/usr/share/rime-shell")
        + "/src/components/auth/PasswordShapes.qml"

    // ── Motion (the user's own Reduce Motion and speed) ───────────
    // Published beside the accent by /usr/libexec/rime-greet-wallpaper from
    // Rime Shell's settings.json, three lines from a closed vocabulary. The
    // password field's shapes (PasswordShapes, loaded from Rime Shell) move on
    // these, so a user who turned Reduce Motion on is not shown motion at the
    // login screen either. Validated again here, line by line; anything else
    // keeps the default, which is the shell's own default.
    property bool   motionReduced: false
    property string motionSpeed:   "balanced"
    property real   motionScale:   1.0
    Process {
        id: motionProc
        running: true
        environment: ({ "AG_USER": ctx.username })
        command: ["sh", "-c",
            "u=\"${AG_USER:-}\";" +
            " [ -n \"$u\" ] || u=\"$(cat /var/lib/rime-greet/last-user 2>/dev/null)\";" +
            " case \"$u\" in ''|.*|*[!A-Za-z0-9._-]*) exit 0 ;; esac;" +
            " head -c 256 \"/var/lib/rime-greet/motion/$u\" 2>/dev/null; echo"]
        // Collected and applied once the read has finished, for the reason the
        // accent reader below documents: a trailing empty line must not reset
        // what the lines before it set.
        property var _seen: ({})
        stdout: SplitParser {
            onRead: function(line) {
                var m = /^(reduce|speed|scale)=(.*)$/.exec(line.trim())
                if (!m) return
                var v = m[2]
                if (m[1] === "reduce" && (v === "0" || v === "1")) motionProc._seen.reduce = v === "1"
                else if (m[1] === "speed" && /^(snappy|balanced|relaxed)$/.test(v)) motionProc._seen.speed = v
                else if (m[1] === "scale" && /^[0-9]{1,2}(\.[0-9]{1,4})?$/.test(v)
                         && parseFloat(v) >= 0 && parseFloat(v) <= 2.5) motionProc._seen.scale = parseFloat(v)
            }
        }
        onExited: {
            var s = motionProc._seen
            ctx.motionReduced = s.reduce === true
            ctx.motionSpeed   = s.speed !== undefined ? s.speed : "balanced"
            ctx.motionScale   = s.scale !== undefined ? s.scale : 1.0
            motionProc._seen  = ({})
        }
    }

    // ── Accent (the user's own, from their wallpaper) ─────────────
    // Published beside the wallpaper by /usr/libexec/rime-greet-wallpaper,
    // which takes it from Rime Shell's matugen output. Empty means "none
    // published yet", and the edition colour in shell.qml is used instead.
    // Validated again here: the file is root-owned, but a colour that is not a
    // colour must never reach a binding.
    property string accent: ""
    Process {
        id: accentProc
        running: true
        environment: ({ "AG_USER": ctx.username })
        command: ["sh", "-c",
            "u=\"${AG_USER:-}\";" +
            " [ -n \"$u\" ] || u=\"$(cat /var/lib/rime-greet/last-user 2>/dev/null)\";" +
            " case \"$u\" in ''|.*|*[!A-Za-z0-9._-]*) exit 0 ;; esac;" +
            " head -c 16 \"/var/lib/rime-greet/accents/$u\" 2>/dev/null; echo"]
        // Collected, and applied once the read has finished. It used to set
        // `accent` per line, and the command prints the file (which ends in a
        // newline) and then `echo`s another — so the second, EMPTY line reset
        // the accent to "" immediately after the colour set it, and the login
        // screen stayed on the edition green with the accent file correct and
        // readable (L16, 2026-09-23). The last valid colour wins; none means
        // none, so switching to an account with no accent still clears it.
        property string _seen: ""
        stdout: SplitParser {
            onRead: function(line) {
                var c = line.trim()
                if (/^#[0-9a-fA-F]{6}$/.test(c)) accentProc._seen = c
            }
        }
        onExited: {
            ctx.accent = accentProc._seen
            accentProc._seen = ""
        }
    }

    // ── Last session (preselect once the list is parsed) ──────────
    Process {
        running: true
        command: ["sh", "-c", "cat /var/lib/rime-greet/last-session 2>/dev/null; echo"]
        stdout: SplitParser {
            onRead: function(line) {
                var s = line.trim()
                if (s !== "") { ctx._wantSession = s; ctx._selectWanted() }
            }
        }
    }

    // ── Bounce watchdog (roadmap P2-018) ──────────────────────────
    //
    // "A graphics/compositor/shell failure CAN ENTER a conservative recovery
    // desktop" is the criterion, and the verb was the part that did not exist:
    // Rime Safe Graphics shipped as a session a PERSON picks. A machine whose
    // driver stopped working after an update bounces — greetd starts the
    // session, it dies in a second, the login screen comes back — and nothing
    // in the system counted, because greetd is a stock unit with no Restart=,
    // no StartLimitBurst and no OnFailure=, and rime-boot-health's units are
    // inert on a GRUB machine, which every Rime machine is.
    //
    // /usr/libexec/rime-session-watchdog counts. The greeter's whole part in it
    // is this: `record` on the way out (above) and `check` on the way in.
    //
    // FAIL OPEN, three ways over:
    //   * `timeout` caps it and a missing helper is a silent no-op — either way
    //     the parser reads the bare `echo` and nothing is selected;
    //   * the id is compared against `recoverySession` before it is used, so
    //     garbage, a second line or an injected id selects NOTHING. The helper
    //     cannot name a session; it can only vote for the one named here;
    //   * it preselects. The user cycles away and that sticks (`_userPicked`).
    Process {
        running: true
        command: ["sh", "-c",
            "timeout 5 /usr/libexec/rime-session-watchdog check 2>/dev/null; echo"]
        stdout: SplitParser {
            onRead: function(line) {
                var s = line.trim()
                if (s !== "" && s === ctx.recoverySession) {
                    ctx._recoverWanted = s
                    ctx._selectWanted()
                }
            }
        }
    }

    // ── Keyboard layout ───────────────────────────────────────────
    //
    // Three files in this repository already carry the same note — the keymap
    // generator, sway-greet.conf and labwc-greet/environment: on a QWERTZ or
    // AZERTY machine a password typed on a US map does not match, and a user
    // who has just installed Rime cannot get in. All three fixed which layout
    // the greeter STARTS on. None of them tells the user what that layout is,
    // and none lets them change it, so a wrong one still presents as an
    // ordinary "wrong password" with no way to diagnose it from the login
    // screen. This is the readout and the switch.
    //
    // Read from sway, not from the environment. XKB_DEFAULT_LAYOUT is what the
    // host was ASKED for; sway's input report is what it actually resolved, and
    // that is what the keystrokes will follow.
    //
    // sway is the greeter's host (greetd-config.toml). Under the documented
    // labwc fallback there is no such IPC, swaymsg fails, and the fallback
    // Process below leaves an indicator with no switch — the honest state
    // there, rather than a control that does nothing.
    property var    layouts:    []
    property string layoutName: "us"

    readonly property bool canSwitchLayout: ctx.layouts.length > 1

    function cycleLayout(dir) {
        if (!ctx.canSwitchLayout) return
        layoutSwitchProc.command = ["sh", "-c",
            "swaymsg input type:keyboard xkb_switch_layout " +
            (dir < 0 ? "prev" : "next") + " >/dev/null 2>&1 || true"]
        layoutSwitchProc.running = true
    }

    // Parses the one `active<TAB>name,name,…` line LAYOUT_SCRIPT emits.
    // Separated from the Process so tests can drive it directly, the way
    // _addSession is driven by the session-list suite.
    function _setLayouts(line) {
        var parts = String(line).split("\t")
        if (parts.length < 2) return
        var active = parts[0].trim()
        var all    = parts[1].split(",")
        var clean  = []
        for (var i = 0; i < all.length; i++) {
            var v = all[i].trim()
            if (v !== "" && clean.indexOf(v) < 0) clean.push(v)
        }
        if (clean.length === 0) return
        ctx.layouts    = clean
        ctx.layoutName = (active !== "") ? active : clean[0]
    }

    // The extraction is deliberately grep/sed only — no jq, no python. The
    // greeter runs as the `greetd` system user before any session exists, and
    // an absent interpreter would leave the indicator silently stuck on its
    // default, which is the exact failure this feature exists to end.
    //
    // Newlines are squashed first so the array survives as one token: sway
    // renders xkb_layout_names as ["English (US)", "French"], and anything that
    // splits on commas keeps only the first entry — which reads as a
    // single-layout machine and hides the switch on precisely the machines that
    // need it.
    readonly property string layoutScript:
        "j=$(swaymsg -t get_inputs -r 2>/dev/null | tr -d '\\n');" +
        " [ -n \"$j\" ] || exit 0;" +
        " a=$(printf '%s' \"$j\" | grep -o '\"xkb_active_layout_name\"[[:space:]]*:[[:space:]]*\"[^\"]*\"'" +
        "      | head -n1 | sed 's/.*:[[:space:]]*\"//; s/\"$//');" +
        " n=$(printf '%s' \"$j\" | grep -o '\"xkb_layout_names\"[[:space:]]*:[[:space:]]*\\[[^]]*\\]'" +
        "      | head -n1 | sed 's/.*\\[//; s/\\]//; s/\"//g; s/[[:space:]]*,[[:space:]]*/,/g;" +
        "                    s/^[[:space:]]*//; s/[[:space:]]*$//');" +
        " [ -n \"$n\" ] || exit 0;" +
        " printf '%s\\t%s\\n' \"$a\" \"$n\""

    Process {
        id: layoutSwitchProc
        running: false
        // Re-read rather than assume the switch landed: sway refuses
        // xkb_switch_layout on a keyboard with one configured layout, and an
        // indicator that advanced its own index would then be lying.
        onExited: function(exitCode, exitStatus) { layoutReadProc.running = true }
    }

    Process {
        id: layoutReadProc
        running: true
        command: ["sh", "-c", ctx.layoutScript]
        stdout: SplitParser {
            onRead: function(line) { ctx._setLayouts(line) }
        }
    }

    // Fallback for the labwc host, and for any sway that answers nothing: the
    // layout the session was ASKED to come up on. Better a readout that is
    // right by default than a hardcoded "us" that lies on an AZERTY machine.
    // It defers to sway whenever sway has answered.
    Process {
        running: true
        command: ["sh", "-c", "printf '%s\\n' \"${XKB_DEFAULT_LAYOUT:-}\""]
        stdout: SplitParser {
            onRead: function(line) {
                var v = line.trim()
                if (v === "" || ctx.layouts.length > 0) return
                ctx.layouts    = v.split(",")
                ctx.layoutName = ctx.layouts[0]
            }
        }
    }

    // ── Session list from /usr/share/wayland-sessions/*.desktop ───
    //
    // TryExec is honoured, per the desktop-entry spec: an entry whose TryExec
    // binary is not on PATH is skipped entirely. Rime ships one image for every
    // laptop and the gaming userspace installs on demand, so rime-gaming.desktop
    // is present on every machine but must only be OFFERED where gamescope has
    // actually been installed. Without this filter the greeter would list a
    // session that exits straight back to the greeter on most machines.
    Process {
        running: true
        command: ["sh", "-c",
            "for f in /usr/share/wayland-sessions/*.desktop; do" +
            " [ -r \"$f\" ] || continue;" +
            " id=$(basename \"$f\" .desktop);" +
            " tryexec=$(sed -n 's/^TryExec=//p' \"$f\" | head -n1);" +
            " if [ -n \"$tryexec\" ] && ! command -v \"$tryexec\" >/dev/null 2>&1;" +
            " then continue; fi;" +
            " name=$(sed -n 's/^Name=//p' \"$f\" | head -n1);" +
            " exec=$(sed -n 's/^Exec=//p' \"$f\" | head -n1);" +
            " [ -n \"$exec\" ] || continue;" +
            " dn=$(sed -n 's/^DesktopNames=//p' \"$f\" | head -n1);" +
            " printf '%s\\t%s\\t%s\\t%s\\n' \"$id\" \"${name:-$id}\" \"$exec\" \"$dn\";" +
            " done"]
        stdout: SplitParser {
            onRead: function(line) { ctx._addSession(line) }
        }
    }
}
