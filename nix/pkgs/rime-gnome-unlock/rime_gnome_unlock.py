#!/usr/bin/env python3
# rime-gnome-unlock: the part of org.gnome.DisplayManager that GNOME Shell's lock screen uses, for a session that
# greetd started (docs/nixos/desktops.md §12.4). Root, system bus, GNOME systems only.
#
# GNOME Shell 50 builds its screen shield only when org.gnome.DisplayManager answers (js/misc/loginManager.js
# canLock) and unlocks through libgdm: Manager.OpenReauthenticationChannel returns the address of a private D-Bus
# server, and the unlock dialog runs one org.gnome.DisplayManager.UserVerifier conversation per attempt over it.
# GDM's own daemon serves exactly this for a session it did not start (gdm-manager.c
# open_temporary_reauthentication_channel, then session_unlock = logind UnlockSession), but it cannot run without
# putting its greeter on every graphical seat (gdm_manager_start starts the local display factory unconditionally).
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
# else (a refused caller, an exception, a cancelled or timed-out prompt, a dropped client) ends the conversation or
# the channel without it. GNOME Shell keeps its shield up until it sees VerificationComplete.
#
# PAM: service "rime-gnome-unlock" (= NixOS's gdm-password: auth substack login). Only pam_authenticate runs: no
# pam_open_session (an unlock opens no session), no pam_setcred and no pam_acct_mgmt, whose results GDM ignores
# for a reauthentication anyway (gdm-session-worker.c: "the user is already logged in after all").
#
# Answers (passwords) are never logged.
import ctypes
import os
import pwd
import queue
import sys
import threading

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

# How long one prompt waits for an answer, and a channel for its client. GNOME Shell cancels an idle unlock dialog
# itself well before either; these only bound what a client that vanished without closing can hold.
ANSWER_TIMEOUT_S = 300
CONNECT_TIMEOUT_S = 60

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

    def authenticate(self, service, user, converse):
        """Returns (pam result code, PAM_USER after pam_authenticate or None)."""

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
            rc = self._authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK)
            item = ctypes.c_void_p()
            if self._get_item(handle, PAM_USER, ctypes.byref(item)) == PAM_SUCCESS and item.value:
                final_user = ctypes.string_at(item.value).decode("utf-8", "replace")
        finally:
            self._end(handle, rc)
        return rc, final_user


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
        }

    def unlock(self, session_id):
        self._call("/org/freedesktop/login1", "org.freedesktop.login1.Manager", "UnlockSession",
                   GLib.Variant("(s)", (session_id,)), None)


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


def peer_allowed(credentials, uid):
    return credentials is not None and credentials.get_unix_user() == uid


# ── one reauthentication channel ────────────────────────────────────────────────────────────────────────────────


class Conversation:
    """One PAM conversation, run on its own thread; the prompts go out as signals, the answers come back through
    answer()."""

    def __init__(self, channel, service):
        self.channel = channel
        self.service = service
        self._answers = queue.Queue()
        self._lock = threading.Lock()
        self._pending = False
        self.cancelled = threading.Event()

    def start(self):
        threading.Thread(target=self._run, name=f"pam-{self.service}", daemon=True).start()

    def _run(self):
        ok, code = False, None
        try:
            code, final_user = self.channel.pam.authenticate(PAM_SERVICE, self.channel.user, self._converse)
            ok = code == PAM_SUCCESS and final_user == self.channel.user and not self.cancelled.is_set()
            if code == PAM_SUCCESS and final_user != self.channel.user:
                log(4, f"session {self.channel.session_id}: PAM did not end with the session's user; refused")
        except Exception as e:  # noqa: BLE001 - any failure is a failed attempt, never a pass
            log(3, f"session {self.channel.session_id}: PAM conversation failed: {type(e).__name__}")
        GLib.idle_add(self.channel.conversation_done, self, ok, code)

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
        self.cancelled.set()
        self._answers.put(None)


class Channel:
    def __init__(self, manager, session_id, uid, user, pid):
        self.manager = manager
        self.pam = manager.pam
        self.session_id = session_id
        self.uid = uid
        self.user = user
        self.pid = pid
        self.connection = None
        self.closed = False
        self.conversations = {}
        self._registration = None

        observer = Gio.DBusAuthObserver.new()
        observer.connect("allow-mechanism", lambda _o, mechanism: mechanism == "EXTERNAL")
        observer.connect("authorize-authenticated-peer", lambda _o, _s, creds: peer_allowed(creds, self.uid))
        self.server = Gio.DBusServer.new_sync(f"unix:dir={manager.runtime_dir}", Gio.DBusServerFlags.NONE,
                                              Gio.dbus_generate_guid(), observer, None)
        self.server.connect("new-connection", self._on_connection)
        self.server.start()
        self.address = self.server.get_client_address()
        if self.address.startswith("unix:path="):
            self.socket_path = self.address[len("unix:path="):].split(",")[0]
            # The socket's own mode lets any local user reach the handshake; the observer above admits the session's
            # uid only (GDM's private servers are 0666 behind the same check).
            os.chmod(self.socket_path, 0o666)
        else:
            self.socket_path = None
        self._timeout = GLib.timeout_add_seconds(CONNECT_TIMEOUT_S, self._connect_timeout)

    def _connect_timeout(self):
        self._timeout = 0
        if self.connection is None:
            log(5, f"session {self.session_id}: nobody connected to the reauthentication channel; closed")
            self.close()
        return GLib.SOURCE_REMOVE

    def _on_connection(self, _server, connection):
        if self.closed or self.connection is not None:
            return False  # one client per channel, as GDM
        self.connection = connection
        info = Gio.DBusNodeInfo.new_for_xml(VERIFIER_XML).interfaces[0]
        self._registration = connection.register_object_with_closures2(SESSION_PATH, info, self._on_call, None, None)
        connection.connect("closed", lambda *_: self.close())
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
            invocation.return_value(None)
            if service != PASSWORD_SERVICE:
                self.emit("ServiceUnavailable", service, "")
                self.emit("ConversationStopped", service)
                return
            conversation = Conversation(self, service)
            self.conversations[service] = conversation
            self.emit("ConversationStarted", service)
            conversation.start()
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
            try:
                self.manager.logind.unlock(self.session_id)
            except GLib.Error as e:
                # GNOME Shell lowers its shield on VerificationComplete; logind's LockedHint follows from it.
                log(4, f"session {self.session_id}: logind UnlockSession failed: {e.message}")
            self.close()
            return GLib.SOURCE_REMOVE
        log(5, f"session {self.session_id} (uid {self.uid}): {service} refused (PAM {code})")
        if code in (PAM_AUTHINFO_UNAVAIL, PAM_MODULE_UNKNOWN, PAM_MAXTRIES):
            self.emit("ServiceUnavailable", service, FAILED_MESSAGE if code == PAM_MAXTRIES else "")
        else:
            self.emit("Problem", service, FAILED_MESSAGE)
        self.emit("VerificationFailed", service)
        self.emit("ConversationStopped", service)
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
    def __init__(self, bus, pam, logind, runtime_dir=RUNTIME_DIR):
        self.bus = bus
        self.pam = pam
        self.logind = logind
        self.runtime_dir = runtime_dir
        self.channels = {}  # caller pid -> Channel: one each, a new request replaces the old (as GDM)
        info = Gio.DBusNodeInfo.new_for_xml(MANAGER_XML).interfaces[0]
        bus.register_object_with_closures2(MANAGER_PATH, info, self._on_call, self._on_get_property, None)

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
            try:
                address = self.open_channel(sender, params.unpack()[0])
            except Refused as e:
                invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", str(e))
                return
            except Exception as e:  # noqa: BLE001 - refuse, never half-open
                log(3, f"OpenReauthenticationChannel failed: {type(e).__name__}: {e}")
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "could not open a channel")
                return
            invocation.return_value(GLib.Variant("(s)", (address,)))
        else:
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)

    def open_channel(self, sender, username):
        pid, uid = self._caller(sender)
        try:
            owner = pwd.getpwuid(uid).pw_name
        except KeyError:
            raise Refused("No session available") from None
        if username != owner:
            raise Refused("only the caller's own user can be reauthenticated")
        session = self.logind.session_for(pid, uid)
        check_session(session, uid)
        old = self.channels.pop(pid, None)
        if old is not None:
            old.close()
        channel = Channel(self, session["id"], uid, owner, pid)
        self.channels[pid] = channel
        log(6, f"session {session['id']} (uid {uid}): reauthentication channel for pid {pid}")
        return channel.address

    def channel_closed(self, channel):
        if self.channels.get(channel.pid) is channel:
            del self.channels[channel.pid]


def main():
    if os.geteuid() != 0:
        sys.exit("rime-gnome-unlock runs as root (it runs PAM for other users)")
    loop = GLib.MainLoop()
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    Manager(bus, Pam(LIBPAM), Logind(bus))

    def lost(_conn, _name):
        log(3, f"{BUS_NAME} is owned by something else (is GDM running?); exiting")
        loop.quit()

    Gio.bus_own_name_on_connection(bus, BUS_NAME, Gio.BusNameOwnerFlags.DO_NOT_QUEUE, None, lost)
    loop.run()
    sys.exit(1)


if __name__ == "__main__":
    main()
