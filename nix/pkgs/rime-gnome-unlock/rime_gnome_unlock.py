#!/usr/bin/env python3
# rime-gnome-unlock: the part of org.gnome.DisplayManager that GNOME Shell's lock screen uses, for a session that
# greetd started (docs/nixos/desktops.md §12.4). Root, system bus, GNOME systems only.
#
# GNOME Shell 50 builds its screen shield only when org.gnome.DisplayManager answers (js/misc/loginManager.js
# canLock) and unlocks through libgdm: Manager.OpenReauthenticationChannel returns the address of a private D-Bus
# server, and the unlock dialog runs one org.gnome.DisplayManager.UserVerifier conversation per attempt over it.
# GDM 50.2's own daemon serves exactly this for a session it did not start (gdm-manager.c:962
# open_temporary_reauthentication_channel; on success :930 on_reauthentication_verification_complete calls
# session_unlock = logind UnlockSession), but it cannot run without putting its greeter on every graphical seat
# (gdm_manager_start, :2357, starts the local display factory unconditionally at :2370).
# This daemon is that one path and nothing else:
#
#   Manager (system bus, /org/gnome/DisplayManager/Manager)
#     Version                       "50.2", the GDM this mirrors (gnome-shell needs >= 3.5.91)
#     RegisterSession/Display       accepted, nothing to do (GDM tracks its own sessions with them)
#     OpenSession                   refused: there is no greeter here
#     OpenReauthenticationChannel   only for the caller's own local graphical session (below)
#   UserVerifier (the private server, /org/gnome/DisplayManager/Session)
#     EnableExtensions              accepted, none served
#     BeginVerification[ForUser]    only "gdm-password", only the session's own user
#     AnswerQuery, Cancel
#
# Fails closed: the only path to VerificationComplete (and to logind UnlockSession) is pam_authenticate returning
# PAM_SUCCESS for the session's own user, with PAM_USER unchanged, in a conversation nobody cancelled. Anything
# else (a refused caller, an exception, a cancelled or timed-out prompt, a dropped client, a PAM worker that died or
# hung) ends the conversation or the channel without it. GNOME Shell keeps its shield up until it sees
# VerificationComplete.
#
# What it guards and what it does not: the private socket is the session user's (0600) and its server admits that
# uid only, so another local user cannot reach a conversation. The session's own uid is not a boundary: a process of
# that user can already call logind UnlockSession (`loginctl unlock-session`) itself, and one outside the session
# (`systemd-run --user`) is treated like GNOME Shell (a user unit) and given the user's graphical session. Such a
# process can also use a channel to test passwords against PAM (pam_faillock and the PAM delays apply, as at any
# prompt); that includes a sandboxed app given system-bus access.
#
# PAM: service "rime-gnome-unlock" (= NixOS's gdm-password: auth substack login). Only pam_authenticate runs: no
# pam_open_session (an unlock opens no session), no pam_setcred and no pam_acct_mgmt, whose result GDM ignores for a
# reauthentication anyway (gdm-session-worker.c:1389-1393: "the user is already logged in after all"). Each
# conversation runs in its own process (this file with --pam-worker), so a PAM module that crashes or hangs ends that
# one attempt and nobody else's channel; it is killed on cancel or when the stack stops answering. PAM_TTY is the
# session's terminal, as GDM sets it (gdm-session-worker.c:1261-1264); PAM_RHOST stays unset, as GDM leaves it for a
# local display (:1220-1241 sets it only when the display is not local), and remote sessions are refused here.
#
# Answers (passwords) are never logged.
import concurrent.futures
import ctypes
import json
import os
import pwd
import queue
import resource
import select
import subprocess
import sys
import threading
import time

from gi.repository import Gio, GLib

LIBPAM = "@libpam@"
PAM_SERVICE = "rime-gnome-unlock"
RUNTIME_DIR = "/run/rime-gnome-unlock"
GDM_VERSION = "50.2"

BUS_NAME = "org.gnome.DisplayManager"
MANAGER_PATH = "/org/gnome/DisplayManager/Manager"
MANAGER_IFACE = "org.gnome.DisplayManager.Manager"
SESSION_PATH = "/org/gnome/DisplayManager/Session"
VERIFIER_IFACE = "org.gnome.DisplayManager.UserVerifier"
PASSWORD_SERVICE = "gdm-password"

# How long one prompt waits for an answer, a channel for its client, and a connected channel with no conversation
# running. GNOME Shell begins a conversation as soon as it connects, and again right after a failed one, and cancels
# an idle unlock dialog itself; these only bound what a client that vanished or stalled can hold.
ANSWER_TIMEOUT_S = 300
CONNECT_TIMEOUT_S = 60
IDLE_TIMEOUT_S = 60
# How long the PAM stack may work between two prompts (pam_unix's failure delay is ~2 s) before its worker is killed.
PAM_STEP_TIMEOUT_S = 60
# Open channels per uid: a new one closes the oldest. GNOME Shell holds one; a second covers its reopen after Cancel.
MAX_CHANNELS_PER_UID = 2
# OpenReauthenticationChannel lookups (bus credentials, passwd, logind) run off the main loop, so Version and
# Introspect never wait behind them; at most this many at once, the rest refused as busy.
LOOKUP_THREADS = 4
MAX_LOOKUPS = 16
# Every FD_CHECK_S the daemon counts its open files; past 3/4 of RLIMIT_NOFILE it exits for systemd to restart it
# (GLib's GDBusServer gives every incoming connection a thread and an fd until its handshake ends, with no timeout).
FD_CHECK_S = 10
WORKER_LINE_MAX = 65536

FAILED_MESSAGE = "Sorry, password authentication didn’t work. Please try again."

# Linux-PAM 1.7 (security/_pam_types.h)
PAM_SUCCESS = 0
PAM_BUF_ERR = 5
PAM_AUTHINFO_UNAVAIL = 9
PAM_MAXTRIES = 11
PAM_CONV_ERR = 19
PAM_MODULE_UNKNOWN = 28
PAM_DISALLOW_NULL_AUTHTOK = 0x1
PAM_USER = 2
PAM_TTY = 3
PAM_PROMPT_ECHO_OFF = 1
PAM_PROMPT_ECHO_ON = 2
PAM_ERROR_MSG = 3
PAM_TEXT_INFO = 4
PAM_MAX_NUM_MSG = 32
PAM_MAX_RESP_SIZE = 512

MANAGER_XML = """
<node>
  <interface name="org.gnome.DisplayManager.Manager">
    <method name="RegisterDisplay"/>
    <method name="RegisterSession"/>
    <method name="OpenSession"><arg name="address" direction="out" type="s"/></method>
    <method name="OpenReauthenticationChannel">
      <arg name="username" direction="in" type="s"/>
      <arg name="address" direction="out" type="s"/>
    </method>
    <property name="Version" type="s" access="read"/>
  </interface>
</node>"""

VERIFIER_XML = """
<node>
  <interface name="org.gnome.DisplayManager.UserVerifier">
    <method name="EnableExtensions"><arg name="extensions" direction="in" type="as"/></method>
    <method name="BeginVerification"><arg name="service_name" direction="in" type="s"/></method>
    <method name="BeginVerificationForUser">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="username" direction="in" type="s"/>
    </method>
    <method name="AnswerQuery">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="answer" direction="in" type="s"/>
    </method>
    <method name="Cancel"/>
    <signal name="ConversationStarted"><arg name="service_name" type="s"/></signal>
    <signal name="ConversationStopped"><arg name="service_name" type="s"/></signal>
    <signal name="ReauthenticationStarted"><arg name="pid_of_caller" type="i"/></signal>
    <signal name="Info"><arg name="service_name" type="s"/><arg name="info" type="s"/></signal>
    <signal name="Problem"><arg name="service_name" type="s"/><arg name="problem" type="s"/></signal>
    <signal name="InfoQuery"><arg name="service_name" type="s"/><arg name="query" type="s"/></signal>
    <signal name="SecretInfoQuery"><arg name="service_name" type="s"/><arg name="query" type="s"/></signal>
    <signal name="Reset"/>
    <signal name="ServiceUnavailable"><arg name="service_name" type="s"/><arg name="message" type="s"/></signal>
    <signal name="VerificationFailed"><arg name="service_name" type="s"/></signal>
    <signal name="VerificationComplete"><arg name="service_name" type="s"/></signal>
  </interface>
</node>"""


def log(priority, text):
    # journald reads the <N> prefix as the syslog priority.
    print(f"<{priority}>{text}", file=sys.stderr, flush=True)


class Refused(Exception):
    """The request is not served; the text goes back to the caller as AccessDenied."""


class Cancelled(Exception):
    """Raised inside the PAM conversation: the answer will not come."""


# ── PAM through libpam, by path ─────────────────────────────────────────────────────────────────────────────────


class PamMessage(ctypes.Structure):
    _fields_ = [("msg_style", ctypes.c_int), ("msg", ctypes.c_char_p)]


class PamResponse(ctypes.Structure):
    # resp is malloc'd here and freed by PAM: never a Python-owned buffer.
    _fields_ = [("resp", ctypes.c_void_p), ("resp_retcode", ctypes.c_int)]


CONV_FUNC = ctypes.CFUNCTYPE(
    ctypes.c_int,
    ctypes.c_int,
    ctypes.POINTER(ctypes.POINTER(PamMessage)),
    ctypes.POINTER(ctypes.POINTER(PamResponse)),
    ctypes.c_void_p,
)


class PamConv(ctypes.Structure):
    _fields_ = [("conv", CONV_FUNC), ("appdata_ptr", ctypes.c_void_p)]


class Pam:
    """One pam_start/pam_authenticate/pam_end per call. `converse(style, text)` answers a prompt (or raises
    Cancelled) and is told about messages; it runs on the calling thread, inside pam_authenticate."""

    def __init__(self, libpath):
        lib = ctypes.CDLL(libpath)
        self._start = lib.pam_start
        self._start.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.POINTER(PamConv),
                                ctypes.POINTER(ctypes.c_void_p)]
        self._start.restype = ctypes.c_int
        self._authenticate = lib.pam_authenticate
        self._authenticate.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self._authenticate.restype = ctypes.c_int
        self._set_item = lib.pam_set_item
        self._set_item.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_char_p]
        self._set_item.restype = ctypes.c_int
        self._get_item = lib.pam_get_item
        self._get_item.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_void_p)]
        self._get_item.restype = ctypes.c_int
        self._end = lib.pam_end
        self._end.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self._end.restype = ctypes.c_int
        libc = ctypes.CDLL("libc.so.6")
        self._calloc = libc.calloc
        self._calloc.argtypes = [ctypes.c_size_t, ctypes.c_size_t]
        self._calloc.restype = ctypes.c_void_p
        self._strdup = libc.strdup
        self._strdup.argtypes = [ctypes.c_char_p]
        self._strdup.restype = ctypes.c_void_p
        self._free = libc.free
        self._free.argtypes = [ctypes.c_void_p]
        self._strlen = libc.strlen
        self._strlen.argtypes = [ctypes.c_void_p]
        self._strlen.restype = ctypes.c_size_t

    def _wipe_free(self, ptr):
        if ptr:
            ctypes.memset(ptr, 0, self._strlen(ptr))
            self._free(ptr)

    def authenticate(self, service, user, converse, tty=None):
        """Returns (pam result code, PAM_USER after pam_authenticate or None). `tty` becomes PAM_TTY."""

        def conv(n, msgs, resp_out, _appdata):
            if n <= 0 or n > PAM_MAX_NUM_MSG:
                return PAM_CONV_ERR
            block = self._calloc(n, ctypes.sizeof(PamResponse))
            if not block:
                return PAM_BUF_ERR
            out = ctypes.cast(block, ctypes.POINTER(PamResponse))
            try:
                for i in range(n):
                    m = msgs[i].contents
                    text = (m.msg or b"").decode("utf-8", "replace")
                    if m.msg_style in (PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON):
                        answer = converse(m.msg_style, text).encode("utf-8")
                        if b"\0" in answer or len(answer) >= PAM_MAX_RESP_SIZE:
                            raise Cancelled("answer refused")
                        out[i].resp = self._strdup(answer)
                        if not out[i].resp:
                            raise Cancelled("out of memory")
                    elif m.msg_style in (PAM_ERROR_MSG, PAM_TEXT_INFO):
                        converse(m.msg_style, text)
                    else:
                        raise Cancelled(f"unsupported PAM message style {m.msg_style}")
            except BaseException:  # noqa: B036 - anything here must end the conversation, never escape into C
                for i in range(n):
                    self._wipe_free(out[i].resp)
                self._free(block)
                return PAM_CONV_ERR
            resp_out[0] = out
            return PAM_SUCCESS

        callback = CONV_FUNC(conv)  # kept alive until pam_end
        pam_conv = PamConv(callback, None)
        handle = ctypes.c_void_p()
        rc = self._start(service.encode(), user.encode(), ctypes.byref(pam_conv), ctypes.byref(handle))
        if rc != PAM_SUCCESS:
            return rc, None
        final_user = None
        try:
            if tty:
                rc = self._set_item(handle, PAM_TTY, tty.encode())
                if rc != PAM_SUCCESS:
                    return rc, None
            rc = self._authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK)
            item = ctypes.c_void_p()
            if self._get_item(handle, PAM_USER, ctypes.byref(item)) == PAM_SUCCESS and item.value:
                final_user = ctypes.string_at(item.value).decode("utf-8", "replace")
        finally:
            self._end(handle, rc)
        return rc, final_user


def pam_worker(libpam, service, user, tty):
    """The --pam-worker process: one pam_authenticate, its conversation as JSON lines. Out (the original stdout):
    {"prompt": style, "text": …}, {"message": style, "text": …}, then {"result": rc, "user": PAM_USER or null}.
    In (the original stdin): {"answer": …} per prompt; end of input cancels. fds 0 and 1 are pointed away first, so
    a PAM module that reads or prints cannot touch the conversation."""
    try:
        libc = ctypes.CDLL("libc.so.6", use_errno=True)
        libc.prctl(1, 9)  # PR_SET_PDEATHSIG, SIGKILL: die with the daemon (best effort; seccomp may refuse)
    except (OSError, AttributeError):
        pass
    if os.getppid() == 1:
        return 1
    answers = os.fdopen(os.dup(0), "rb")
    out = os.fdopen(os.dup(1), "wb")
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    os.dup2(2, 1)

    def send(obj):
        out.write(json.dumps(obj).encode() + b"\n")
        out.flush()

    def converse(style, text):
        if style in (PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON):
            send({"prompt": style, "text": text})
            line = answers.readline(WORKER_LINE_MAX)
            if not line.endswith(b"\n"):
                raise Cancelled()
            answer = json.loads(line)["answer"]
            if not isinstance(answer, str):
                raise Cancelled()
            return answer
        send({"message": style, "text": text})
        return ""

    rc, final_user = Pam(libpam).authenticate(service, user, converse, tty or None)
    send({"result": rc, "user": final_user})
    return 0


# ── logind ──────────────────────────────────────────────────────────────────────────────────────────────────────


class Logind:
    def __init__(self, bus):
        self.bus = bus

    def _call(self, path, iface, method, args, reply):
        return self.bus.call_sync("org.freedesktop.login1", path, iface, method, args,
                                  GLib.VariantType(reply) if reply else None,
                                  Gio.DBusCallFlags.NONE, 5000, None)

    def session_for(self, pid, uid):
        """The logind session a reauthentication from (pid, uid) is for, as a dict; Refused if none.

        As GDM (gdm_find_display_session): the caller's own session; a caller outside any session (GNOME Shell runs
        as a systemd user unit) gets its user's graphical session, here logind's own pick (User.Display)."""
        path = None
        try:
            (path,) = self._call("/org/freedesktop/login1", "org.freedesktop.login1.Manager", "GetSessionByPID",
                                 GLib.Variant("(u)", (pid,)), "(o)").unpack()
        except GLib.Error as e:
            # Only "this pid is in no session" falls back; any other failure refuses.
            if Gio.DBusError.get_remote_error(e) != "org.freedesktop.login1.NoSessionForPID":
                raise Refused("No session available") from e
        if path is None:
            try:
                (user_path,) = self._call("/org/freedesktop/login1", "org.freedesktop.login1.Manager", "GetUser",
                                          GLib.Variant("(u)", (uid,)), "(o)").unpack()
                (display,) = self._call(user_path, "org.freedesktop.DBus.Properties", "Get",
                                        GLib.Variant("(ss)", ("org.freedesktop.login1.User", "Display")),
                                        "(v)").unpack()
            except GLib.Error as e:
                raise Refused("No session available") from e
            sid, path = display
            if not sid:
                raise Refused("No session available")
        try:
            (props,) = self._call(path, "org.freedesktop.DBus.Properties", "GetAll",
                                  GLib.Variant("(s)", ("org.freedesktop.login1.Session",)), "(a{sv})").unpack()
        except GLib.Error as e:
            raise Refused("No session available") from e
        return {
            "id": props.get("Id", ""),
            "uid": props.get("User", (None, ""))[0],
            "class": props.get("Class", ""),
            "type": props.get("Type", ""),
            "remote": props.get("Remote", True),
            "state": props.get("State", ""),
            "tty": props.get("TTY", ""),
            "vtnr": props.get("VTNr", 0),
            "display": props.get("Display", ""),
        }

    def unlock(self, session_id):
        """Asynchronous: the main loop never waits on logind."""

        def done(bus, result):
            try:
                bus.call_finish(result)
            except GLib.Error as e:
                # GNOME Shell lowers its shield on VerificationComplete; logind's LockedHint follows from it.
                log(4, f"session {session_id}: logind UnlockSession failed: {e.message}")

        self.bus.call("org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager",
                      "UnlockSession", GLib.Variant("(s)", (session_id,)), None, Gio.DBusCallFlags.NONE, 5000, None,
                      done)

    def subscribe_removed(self, callback):
        """callback(session id) whenever logind drops a session."""
        self.bus.signal_subscribe("org.freedesktop.login1", "org.freedesktop.login1.Manager", "SessionRemoved",
                                  "/org/freedesktop/login1", None, Gio.DBusSignalFlags.NONE,
                                  lambda _c, _s, _p, _i, _n, params: callback(params.unpack()[0]))


def check_session(session, uid):
    """Refused unless `session` is a local graphical user session of `uid` that is not closing."""
    if session.get("uid") != uid:
        raise Refused("No session available")
    if session.get("class") != "user":
        raise Refused("Only a user session can be reauthenticated")
    if session.get("type") not in ("wayland", "x11"):
        raise Refused("Only a graphical session can be reauthenticated")
    if session.get("remote") is not False:
        raise Refused("Only a local session can be reauthenticated")
    if session.get("state") not in ("active", "online"):
        raise Refused("No session available")
    if not session.get("id"):
        raise Refused("No session available")


def pam_tty(session):
    """PAM_TTY for a session: its terminal (/dev/ttyN), else its X display, else none."""
    tty = session.get("tty") or ""
    if tty:
        return tty if tty.startswith("/dev/") else "/dev/" + tty
    if session.get("vtnr"):
        return f"/dev/tty{session['vtnr']}"
    return session.get("display") or ""


def peer_allowed(credentials, uid):
    return credentials is not None and credentials.get_unix_user() == uid


def restrict_socket(path, uid):
    """Only `uid` (and root) may connect: mode 0600 first, then the owner. Raises if either fails."""
    os.chmod(path, 0o600)
    os.chown(path, uid, -1)


def fd_pressure(limit=None):
    """True once this process holds more than 3/4 of its open-file limit."""
    if limit is None:
        limit = resource.getrlimit(resource.RLIMIT_NOFILE)[0]
    return len(os.listdir("/proc/self/fd")) > limit * 3 // 4


# ── one reauthentication channel ────────────────────────────────────────────────────────────────────────────────


class Conversation:
    """One PAM conversation: a --pam-worker process, driven from its own thread; the prompts go out as signals, the
    answers come back through answer()."""

    def __init__(self, channel, service):
        self.channel = channel
        self.service = service
        self._answers = queue.Queue()
        self._lock = threading.Lock()
        self._pending = False
        self.cancelled = threading.Event()
        self.proc = None
        self._buffer = b""
        self._finished = False

    def start(self):
        """Main thread. Raises if the worker cannot be started; nothing is left running then."""
        argv = self.channel.manager.worker_argv + [PAM_SERVICE, self.channel.user, self.channel.tty or ""]
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, close_fds=True)
        try:
            threading.Thread(target=self._run, name=f"pam-{self.service}", daemon=True).start()
        except BaseException:
            self._reap()
            raise

    def _run(self):
        ok, code = False, None
        try:
            code, final_user = self._authenticate()
            ok = code == PAM_SUCCESS and final_user == self.channel.user and not self.cancelled.is_set()
            if code == PAM_SUCCESS and final_user != self.channel.user:
                log(4, f"session {self.channel.session_id}: PAM did not end with the session's user; refused")
        except Cancelled as e:
            if not self.cancelled.is_set() and str(e):
                log(4, f"session {self.channel.session_id}: PAM conversation ended: {e}")
        except Exception as e:  # noqa: BLE001 - any failure is a failed attempt, never a pass
            if not self.cancelled.is_set():
                log(3, f"session {self.channel.session_id}: PAM conversation failed: {type(e).__name__}")
        finally:
            self._reap()
        GLib.idle_add(self.channel.conversation_done, self, ok, code)

    def _authenticate(self):
        """The worker's conversation, relayed. Returns (pam result code, PAM_USER or None)."""
        while True:
            msg = self._receive()
            if "result" in msg:
                code, user = msg["result"], msg.get("user")
                if not isinstance(code, int) or not (user is None or isinstance(user, str)):
                    raise Cancelled("the PAM worker sent a malformed result")
                self._finished = True
                return code, user
            style = msg.get("prompt", msg.get("message"))
            text = msg.get("text")
            if not isinstance(text, str) or style not in (PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON,
                                                           PAM_ERROR_MSG, PAM_TEXT_INFO):
                raise Cancelled("the PAM worker sent a malformed message")
            if "prompt" in msg and style in (PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON):
                answer = self._converse(style, text)
                self.proc.stdin.write(json.dumps({"answer": answer}).encode() + b"\n")
                self.proc.stdin.flush()
            elif "message" in msg and style in (PAM_ERROR_MSG, PAM_TEXT_INFO):
                self._converse(style, text)
            else:
                raise Cancelled("the PAM worker sent a malformed message")

    def _receive(self):
        """The worker's next line. Cancelled when it ends, stops answering for PAM_STEP_TIMEOUT_S, or we cancel."""
        fd = self.proc.stdout.fileno()
        poller = select.poll()
        poller.register(fd, select.POLLIN | select.POLLHUP)
        deadline = time.monotonic() + PAM_STEP_TIMEOUT_S
        while b"\n" not in self._buffer:
            if self.cancelled.is_set():
                raise Cancelled()
            left = deadline - time.monotonic()
            if left <= 0:
                raise Cancelled("the PAM stack did not answer in time")
            if not poller.poll(int(min(left, 0.25) * 1000) + 1):
                continue
            chunk = os.read(fd, 4096)
            if not chunk:
                raise Cancelled("the PAM worker ended without a result")
            self._buffer += chunk
            if len(self._buffer) > WORKER_LINE_MAX:
                raise Cancelled("the PAM worker sent too much")
        line, self._buffer = self._buffer.split(b"\n", 1)
        try:
            msg = json.loads(line)
        except ValueError:
            raise Cancelled("the PAM worker sent a malformed message") from None
        if not isinstance(msg, dict):
            raise Cancelled("the PAM worker sent a malformed message")
        return msg

    def _reap(self):
        """The worker is gone afterwards: a finished one exits by itself, any other is killed."""
        proc = self.proc
        if proc is None:
            return
        if not self._finished and proc.poll() is None:
            proc.kill()
        for f in (proc.stdin, proc.stdout):
            try:
                f.close()
            except OSError:
                pass
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()

    def _converse(self, style, text):
        if self.cancelled.is_set():
            raise Cancelled()
        if style == PAM_PROMPT_ECHO_OFF:
            return self._ask("SecretInfoQuery", text)
        if style == PAM_PROMPT_ECHO_ON:
            return self._ask("InfoQuery", text)
        self.channel.emit_later("Problem" if style == PAM_ERROR_MSG else "Info", self.service, text)
        return ""

    def _ask(self, signal, text):
        with self._lock:
            self._pending = True
        self.channel.emit_later(signal, self.service, text)
        try:
            answer = self._answers.get(timeout=ANSWER_TIMEOUT_S)
        except queue.Empty:
            raise Cancelled("no answer") from None
        if answer is None or self.cancelled.is_set():
            raise Cancelled()
        return answer

    def answer(self, text):
        """Main thread. False when no prompt is waiting."""
        with self._lock:
            if not self._pending:
                return False
            self._pending = False
        self._answers.put(text)
        return True

    def cancel(self):
        """Any thread: the conversation ends without a result, and its worker is killed."""
        self.cancelled.set()
        self._answers.put(None)
        proc = self.proc
        if proc is not None and proc.poll() is None:
            try:
                proc.kill()
            except ProcessLookupError:
                pass


class Channel:
    def __init__(self, manager, session_id, uid, user, tty=""):
        self.manager = manager
        self.session_id = session_id
        self.uid = uid
        self.user = user
        self.tty = tty
        self.connection = None
        self.closed = False
        self.conversations = {}
        self._registration = None
        self._timeout = 0
        self._idle = 0

        observer = Gio.DBusAuthObserver.new()
        observer.connect("allow-mechanism", lambda _o, mechanism: mechanism == "EXTERNAL")
        observer.connect("authorize-authenticated-peer", lambda _o, _s, creds: peer_allowed(creds, self.uid))
        self.server = Gio.DBusServer.new_sync(f"unix:dir={manager.runtime_dir}", Gio.DBusServerFlags.NONE,
                                              Gio.dbus_generate_guid(), observer, None)
        self.address = self.server.get_client_address()
        self.socket_path = None
        try:
            if not self.address.startswith("unix:path="):
                raise RuntimeError(f"unexpected server address {self.address}")
            self.socket_path = self.address[len("unix:path="):].split(",")[0]
            # Bound but not yet serving (the server starts below). Until here the socket has the service's umask
            # (0755: root only); from here only the session's uid can connect. GDM's own servers are 0666 behind
            # the uid check alone, which leaves every local user able to hold a handshake open.
            restrict_socket(self.socket_path, uid)
        except BaseException:
            # Never started, so stop() would do nothing: the listener goes with this object, the socket by hand.
            if self.socket_path:
                try:
                    os.unlink(self.socket_path)
                except FileNotFoundError:
                    pass
            raise
        self.server.connect("new-connection", self._on_connection)
        self.server.start()
        self._timeout = GLib.timeout_add_seconds(CONNECT_TIMEOUT_S, self._connect_timeout)

    def _connect_timeout(self):
        self._timeout = 0
        if self.connection is None:
            log(5, f"session {self.session_id}: nobody connected to the reauthentication channel; closed")
            self.close()
        return GLib.SOURCE_REMOVE

    def _arm_idle(self):
        self._disarm_idle()
        if not self.closed:
            self._idle = GLib.timeout_add(int(IDLE_TIMEOUT_S * 1000), self._idle_timeout)

    def _disarm_idle(self):
        if self._idle:
            GLib.source_remove(self._idle)
            self._idle = 0

    def _idle_timeout(self):
        self._idle = 0
        if not self.conversations:
            log(5, f"session {self.session_id}: reauthentication channel idle; closed")
            self.close()
        return GLib.SOURCE_REMOVE

    def _on_connection(self, _server, connection):
        if self.closed or self.connection is not None:
            return False  # one client per channel, as GDM
        self.connection = connection
        # The one client is in: stop listening (this also removes the socket), so nobody else can start a handshake.
        self.server.stop()
        info = Gio.DBusNodeInfo.new_for_xml(VERIFIER_XML).interfaces[0]
        self._registration = connection.register_object_with_closures2(SESSION_PATH, info, self._on_call, None, None)
        connection.connect("closed", lambda *_: self.close())
        self._arm_idle()
        return True

    def emit(self, name, *args):
        if self.closed or self.connection is None or self.connection.is_closed():
            return
        signature = "(" + "s" * len(args) + ")" if args else None
        params = GLib.Variant(signature, args) if args else None
        try:
            self.connection.emit_signal(None, SESSION_PATH, VERIFIER_IFACE, name, params)
        except GLib.Error:
            self.close()

    def emit_later(self, name, *args):
        GLib.idle_add(lambda: (self.emit(name, *args), GLib.SOURCE_REMOVE)[1])

    def _on_call(self, _conn, _sender, _path, _iface, method, params, invocation):
        try:
            self._dispatch(method, params.unpack() if params is not None else (), invocation)
        except Exception as e:  # noqa: BLE001 - a bad call ends with an error, never with a verified user
            log(3, f"session {self.session_id}: {method} failed: {type(e).__name__}")
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "request failed")

    def _dispatch(self, method, args, invocation):
        if self.closed:
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "channel closed")
            return
        if method == "EnableExtensions":
            invocation.return_value(None)
        elif method in ("BeginVerification", "BeginVerificationForUser"):
            service = args[0]
            user = args[1] if method == "BeginVerificationForUser" else self.user
            if user != self.user:
                invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied",
                                             "only the session's own user can be verified")
                return
            if service in self.conversations:
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed",
                                             f"{service} is already running")
                return
            if service != PASSWORD_SERVICE:
                invocation.return_value(None)
                self.emit("ServiceUnavailable", service, "")
                self.emit("ConversationStopped", service)
                return
            # Started first: a conversation that could not start is an error to the caller and is not kept.
            conversation = Conversation(self, service)
            try:
                conversation.start()
            except Exception as e:  # noqa: BLE001
                log(3, f"session {self.session_id}: could not start a PAM conversation: {type(e).__name__}")
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "could not start the conversation")
                return
            self.conversations[service] = conversation
            self._disarm_idle()
            invocation.return_value(None)
            self.emit("ConversationStarted", service)
        elif method == "AnswerQuery":
            service, answer = args
            conversation = self.conversations.get(service)
            if conversation is None or not conversation.answer(answer):
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "no question is waiting")
                return
            invocation.return_value(None)
        elif method == "Cancel":
            invocation.return_value(None)
            GLib.idle_add(lambda: (self.close(), GLib.SOURCE_REMOVE)[1])
        else:
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)

    def conversation_done(self, conversation, ok, code):
        """Main thread, once per conversation."""
        service = conversation.service
        if self.conversations.get(service) is conversation:
            del self.conversations[service]
        if self.closed or conversation.cancelled.is_set():
            return GLib.SOURCE_REMOVE
        if ok:
            log(5, f"session {self.session_id} (uid {self.uid}): unlocked by {service}")
            self.emit("VerificationComplete", service)
            self.manager.logind.unlock(self.session_id)
            self.close()
            return GLib.SOURCE_REMOVE
        log(5, f"session {self.session_id} (uid {self.uid}): {service} refused (PAM {code})")
        if code in (PAM_AUTHINFO_UNAVAIL, PAM_MODULE_UNKNOWN, PAM_MAXTRIES):
            self.emit("ServiceUnavailable", service, FAILED_MESSAGE if code == PAM_MAXTRIES else "")
        else:
            self.emit("Problem", service, FAILED_MESSAGE)
        self.emit("VerificationFailed", service)
        self.emit("ConversationStopped", service)
        if not self.conversations:
            self._arm_idle()
        return GLib.SOURCE_REMOVE

    def close(self):
        if self.closed:
            return
        self.closed = True
        for conversation in list(self.conversations.values()):
            conversation.cancel()
        if self._timeout:
            GLib.source_remove(self._timeout)
            self._timeout = 0
        self._disarm_idle()
        if self.connection is not None:
            if self._registration:
                self.connection.unregister_object(self._registration)
            try:
                self.connection.flush_sync(None)
            except GLib.Error:
                pass
            self.connection.close(None, None, None)
        self.server.stop()
        if self.socket_path:
            try:
                os.unlink(self.socket_path)
            except FileNotFoundError:
                pass
        self.manager.channel_closed(self)


# ── the manager on the system bus ───────────────────────────────────────────────────────────────────────────────


class Manager:
    def __init__(self, bus, libpam, logind, runtime_dir=RUNTIME_DIR):
        self.bus = bus
        self.logind = logind
        self.runtime_dir = runtime_dir
        self.worker_argv = [sys.executable, "-I", "-B", os.path.abspath(__file__), "--pam-worker", libpam]
        self.channels = {}  # uid -> [Channel], oldest first; at most MAX_CHANNELS_PER_UID
        self._lookups = concurrent.futures.ThreadPoolExecutor(max_workers=LOOKUP_THREADS,
                                                              thread_name_prefix="lookup")
        self._in_flight = 0
        info = Gio.DBusNodeInfo.new_for_xml(MANAGER_XML).interfaces[0]
        bus.register_object_with_closures2(MANAGER_PATH, info, self._on_call, self._on_get_property, None)
        logind.subscribe_removed(self.session_removed)

    def _on_get_property(self, _conn, _sender, _path, _iface, name):
        if name == "Version":
            return GLib.Variant("s", GDM_VERSION)
        return None

    def _caller(self, sender):
        (creds,) = self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                      "GetConnectionCredentials", GLib.Variant("(s)", (sender,)),
                                      GLib.VariantType("(a{sv})"), Gio.DBusCallFlags.NONE, 5000, None).unpack()
        if "UnixUserID" not in creds or "ProcessID" not in creds:
            raise Refused("No session available")
        return creds["ProcessID"], creds["UnixUserID"]

    def _on_call(self, _conn, sender, _path, _iface, method, params, invocation):
        if method in ("RegisterSession", "RegisterDisplay"):
            invocation.return_value(None)
        elif method == "OpenSession":
            invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "there is no greeter here")
        elif method == "OpenReauthenticationChannel":
            if self._in_flight >= MAX_LOOKUPS:
                invocation.return_dbus_error("org.freedesktop.DBus.Error.LimitsExceeded", "busy; try again")
                return
            self._in_flight += 1
            username = params.unpack()[0]
            self._lookups.submit(self._lookup, sender, username, invocation)
        else:
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)

    def _lookup(self, sender, username, invocation):
        """A lookup thread: who is asking, for which session. Everything else happens on the main loop."""
        try:
            result = self.resolve(sender, username)
        except Exception as e:  # noqa: BLE001 - handed to the main loop, which refuses
            result = e
        GLib.idle_add(self._open_finish, invocation, result)

    def resolve(self, sender, username):
        pid, uid = self._caller(sender)
        try:
            owner = pwd.getpwuid(uid).pw_name
        except KeyError:
            raise Refused("No session available") from None
        if username != owner:
            raise Refused("only the caller's own user can be reauthenticated")
        session = self.logind.session_for(pid, uid)
        check_session(session, uid)
        return pid, uid, owner, session

    def _open_finish(self, invocation, result):
        self._in_flight -= 1
        if isinstance(result, Refused):
            invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", str(result))
            return GLib.SOURCE_REMOVE
        try:
            if isinstance(result, Exception):
                raise result
            address = self.open_channel(*result)
        except Exception as e:  # noqa: BLE001 - refuse, never half-open
            log(3, f"OpenReauthenticationChannel failed: {type(e).__name__}: {e}")
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "could not open a channel")
            return GLib.SOURCE_REMOVE
        invocation.return_value(GLib.Variant("(s)", (address,)))
        return GLib.SOURCE_REMOVE

    def open_channel(self, pid, uid, owner, session):
        """Main thread."""
        while len(self.channels.get(uid, [])) >= MAX_CHANNELS_PER_UID:
            self.channels[uid].pop(0).close()
        channel = Channel(self, session["id"], uid, owner, pam_tty(session))
        self.channels.setdefault(uid, []).append(channel)
        log(6, f"session {session['id']} (uid {uid}): reauthentication channel for pid {pid}")
        return channel.address

    def channel_closed(self, channel):
        mine = self.channels.get(channel.uid, [])
        if channel in mine:
            mine.remove(channel)
        if not mine:
            self.channels.pop(channel.uid, None)

    def session_removed(self, session_id):
        for channel in [c for mine in self.channels.values() for c in mine if c.session_id == session_id]:
            log(5, f"session {session_id}: logind removed it; its reauthentication channel is closed")
            channel.close()  # its session is gone


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--pam-worker":
        sys.exit(pam_worker(*sys.argv[2:6]))
    if os.geteuid() != 0:
        sys.exit("rime-gnome-unlock runs as root (it runs PAM for other users)")
    # Sockets of an earlier run (one killed without cleaning up) serve nothing.
    for name in os.listdir(RUNTIME_DIR) if os.path.isdir(RUNTIME_DIR) else []:
        try:
            os.unlink(os.path.join(RUNTIME_DIR, name))
        except OSError:
            pass
    loop = GLib.MainLoop()
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    Manager(bus, LIBPAM, Logind(bus))

    def lost(_conn, _name):
        log(3, f"{BUS_NAME} is owned by something else (is GDM running?); exiting")
        loop.quit()

    def check_fds():
        if fd_pressure():
            log(2, "open files near the limit (a client holding connections open?); exiting for a restart")
            loop.quit()
            return GLib.SOURCE_REMOVE
        return GLib.SOURCE_CONTINUE

    GLib.timeout_add_seconds(FD_CHECK_S, check_fds)
    Gio.bus_own_name_on_connection(bus, BUS_NAME, Gio.BusNameOwnerFlags.DO_NOT_QUEUE, None, lost)
    loop.run()
    sys.exit(1)


if __name__ == "__main__":
    main()
