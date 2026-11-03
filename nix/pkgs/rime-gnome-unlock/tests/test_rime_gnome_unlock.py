"""rime-gnome-unlock against GNOME's own client library.

Three processes besides this one: a private dbus-daemon (the system bus), tests/fake_logind.py (systemd-logind) and
tests/harness.py (the installed daemon's Manager, with pam_wrapper's libpam). The client side is libgdm's Gdm.Client,
the code GNOME Shell's unlock dialog runs. PAM is pam_wrapper + pam_matrix (a password file), with pam_chatty in
front so the Info path is exercised too.

Environment (default.nix sets it): RIME_GNOME_UNLOCK (the installed module), PAM_WRAPPER_LIB, PAM_WRAPPER_MODULES,
GI_TYPELIB_PATH with Gdm-1.0, dbus-daemon on PATH. The file re-executes itself once with pam_wrapper preloaded.
"""
import importlib.util
import json
import os
import pwd
import stat
import subprocess
import sys
import tempfile
import time
import unittest

PASSWORD = "correct horse"
WRONG = "battery staple"

if os.environ.get("RGU_TEST_WRAPPED") != "1":
    tmp = tempfile.mkdtemp(prefix="rgu-")
    user = pwd.getpwuid(os.getuid()).pw_name
    mods = os.environ["PAM_WRAPPER_MODULES"]
    os.makedirs(f"{tmp}/pam.d")
    with open(f"{tmp}/passdb", "w") as f:
        f.write(f"{user}:{PASSWORD}:rime-gnome-unlock\n{user}:{PASSWORD}:rime-test-plain\n")
    with open(f"{tmp}/pam.d/rime-gnome-unlock", "w") as f:
        f.write(f"auth required {mods}/pam_chatty.so num_lines=1 info\n"
                f"auth required {mods}/pam_matrix.so passdb={tmp}/passdb\n")
    with open(f"{tmp}/pam.d/rime-test-plain", "w") as f:
        f.write(f"auth required {mods}/pam_matrix.so passdb={tmp}/passdb\n")
    env = dict(os.environ, RGU_TEST_WRAPPED="1", RGU_TMP=tmp, PAM_WRAPPER="1",
               PAM_WRAPPER_SERVICE_DIR=f"{tmp}/pam.d", LD_PRELOAD=os.environ["PAM_WRAPPER_LIB"])
    os.execve(sys.executable, [sys.executable, "-B", "-W", "error::ResourceWarning", "-W", "error::DeprecationWarning:rime_gnome_unlock", __file__] + sys.argv[1:], env)

import gi  # noqa: E402

gi.require_version("Gdm", "1.0")
from gi.repository import Gdm, Gio, GLib  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TMP = os.environ["RGU_TMP"]
RUNTIME = f"{TMP}/run"
LOGIND_CONFIG = f"{TMP}/logind.json"
UNLOCKS = f"{TMP}/unlocks"
UID = os.getuid()
USER = pwd.getpwuid(UID).pw_name
PID = os.getpid()
ACCESS_DENIED = "org.freedesktop.DBus.Error.AccessDenied"


def load_module():
    spec = importlib.util.spec_from_file_location("rime_gnome_unlock", os.environ["RIME_GNOME_UNLOCK"])
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


rgu = load_module()
FAILED = rgu.FAILED_MESSAGE


def session(**over):
    s = {"uid": UID, "class": "user", "type": "wayland", "remote": False, "state": "active"}
    s.update(over)
    return s


def set_logind(pids=None, display=None, sessions=None):
    with open(LOGIND_CONFIG + ".new", "w") as f:
        json.dump({"pids": pids or {}, "display": display or {}, "sessions": sessions or {}}, f)
    os.replace(LOGIND_CONFIG + ".new", LOGIND_CONFIG)


def own_graphical_session():
    set_logind(pids={str(PID): "s1"}, sessions={"s1": session()})


def unlocks():
    try:
        with open(UNLOCKS) as f:
            return f.read().split()
    except FileNotFoundError:
        return []


def iterate_until(predicate, timeout=10.0):
    ctx = GLib.MainContext.default()
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > deadline:
            return False
        ctx.iteration(False)
        time.sleep(0.005)
    return True


def settle(seconds=0.4):
    iterate_until(lambda: False, seconds)


def remote_error(err):
    return Gio.DBusError.get_remote_error(err)


SIGNALS = ["conversation-started", "conversation-stopped", "info", "problem", "info-query", "secret-info-query",
           "service-unavailable", "verification-failed", "verification-complete", "reset"]


class Client:
    """One Gdm.Client reauthentication channel, as GNOME Shell's ShellUserVerifier opens it."""

    def __init__(self, user=USER):
        self.client = Gdm.Client()
        self.verifier = self.client.open_reauthentication_channel_sync(user, None)
        self.events = []
        for name in SIGNALS:
            self.verifier.connect(name, self._record, name)

    def _record(self, _verifier, *args):
        self.events.append((args[-1],) + tuple(args[:-1]))

    def names(self):
        return [e[0] for e in self.events]

    def flow(self):
        """names(), with each run of Info messages (pam_chatty's lines) as one."""
        out = []
        for name in self.names():
            if not (name == "info" and out and out[-1] == "info"):
                out.append(name)
        return out

    def wait(self, name, count=1, timeout=10.0):
        ok = iterate_until(lambda: self.names().count(name) >= count, timeout)
        if not ok:
            raise AssertionError(f"no {name} (#{count}); got {self.events}")

    def begin(self, service="gdm-password", user=USER):
        self.verifier.call_begin_verification_for_user_sync(service, user, None)

    def answer(self, text, service="gdm-password"):
        self.verifier.call_answer_query_sync(service, text, None)

    def closed(self):
        return self.verifier.get_connection().is_closed()

    def wait_closed(self, timeout=10.0):
        if not iterate_until(self.closed, timeout):
            raise AssertionError("the channel stayed open")


def system_bus():
    return Gio.bus_get_sync(Gio.BusType.SYSTEM, None)


def call_manager(method, args=None, reply=None):
    return system_bus().call_sync(rgu.BUS_NAME, rgu.MANAGER_PATH, rgu.MANAGER_IFACE, method, args,
                                  GLib.VariantType(reply) if reply else None, Gio.DBusCallFlags.NONE, 5000, None)


def spawn(*args):
    p = subprocess.Popen([sys.executable, "-B", "-W", "error::DeprecationWarning:rime_gnome_unlock", *args], stdout=subprocess.PIPE, text=True)
    line = p.stdout.readline().strip()
    if not line:
        p.kill()
        p.wait()
        raise RuntimeError(f"{args[0]} did not start")
    return p, line


PROCS = []
TEST_DBUS = None


def setUpModule():
    global TEST_DBUS
    os.makedirs(RUNTIME, mode=0o711)
    own_graphical_session()
    TEST_DBUS = Gio.TestDBus.new(Gio.TestDBusFlags.NONE)
    TEST_DBUS.up()
    os.environ["DBUS_SYSTEM_BUS_ADDRESS"] = TEST_DBUS.get_bus_address()
    os.environ["FAKE_LOGIND_CONFIG"] = LOGIND_CONFIG
    os.environ["FAKE_LOGIND_UNLOCKS"] = UNLOCKS
    PROCS.append(spawn(f"{HERE}/fake_logind.py")[0])
    PROCS.append(spawn(f"{HERE}/harness.py", "manager", RUNTIME)[0])


def tearDownModule():
    for p in PROCS:
        p.terminate()
        p.wait()
        p.stdout.close()
    TEST_DBUS.down()


class ManagerTest(unittest.TestCase):
    def setUp(self):
        own_graphical_session()
        self.before = unlocks()

    def tearDown(self):
        # Every channel a test opened is gone by the end of it, socket included.
        self.assertTrue(iterate_until(lambda: os.listdir(RUNTIME) == [], 5), os.listdir(RUNTIME))

    def new_unlocks(self):
        return unlocks()[len(self.before):]

    def test_version_is_a_gdm_gnome_shell_accepts(self):
        (version,) = system_bus().call_sync(rgu.BUS_NAME, rgu.MANAGER_PATH, "org.freedesktop.DBus.Properties", "Get",
                                            GLib.Variant("(ss)", (rgu.MANAGER_IFACE, "Version")),
                                            GLib.VariantType("(v)"), Gio.DBusCallFlags.NONE, 5000, None).unpack()
        self.assertEqual(version, "50.2")
        parts = [int(p) for p in version.split(".")]
        self.assertGreaterEqual(parts, [3, 5, 91])  # gnome-shell loginManager.js canLock()

    def test_register_session_is_accepted_open_session_refused(self):
        call_manager("RegisterSession")
        call_manager("RegisterDisplay")
        with self.assertRaises(GLib.Error) as cm:
            call_manager("OpenSession", None, "(s)")
        self.assertEqual(remote_error(cm.exception), ACCESS_DENIED)

    def test_correct_password_unlocks_and_closes(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.answer(PASSWORD)
        c.wait("verification-complete")
        c.wait_closed()
        self.assertEqual(c.flow(), ["conversation-started", "info", "secret-info-query", "verification-complete"])
        self.assertEqual(c.events[-1], ("verification-complete", "gdm-password"))
        self.assertTrue(iterate_until(lambda: self.new_unlocks() == ["s1"]), self.new_unlocks())

    def test_wrong_password_is_refused_then_retry_on_same_channel(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.answer(WRONG)
        c.wait("conversation-stopped")
        settle()
        self.assertEqual(c.flow(), ["conversation-started", "info", "secret-info-query", "problem",
                                     "verification-failed", "conversation-stopped"])
        self.assertIn(("problem", "gdm-password", FAILED), c.events)
        self.assertFalse(c.closed())
        self.assertEqual(self.new_unlocks(), [])
        # GNOME Shell retries on the same channel after ConversationStopped.
        c.begin()
        c.wait("secret-info-query", 2)
        c.answer(PASSWORD)
        c.wait("verification-complete")
        c.wait_closed()
        self.assertTrue(iterate_until(lambda: self.new_unlocks() == ["s1"]), self.new_unlocks())

    def test_empty_password_is_refused(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.answer("")
        c.wait("verification-failed")
        self.assertNotIn("verification-complete", c.names())
        self.assertEqual(self.new_unlocks(), [])
        c.verifier.call_cancel_sync(None)
        c.wait_closed()

    def test_channel_for_another_user_is_refused(self):
        with self.assertRaises(GLib.Error) as cm:
            Client(user="root" if USER != "root" else "nobody")
        self.assertEqual(remote_error(cm.exception), ACCESS_DENIED)

    def test_verifying_another_user_is_refused(self):
        c = Client()
        with self.assertRaises(GLib.Error) as cm:
            c.begin(user="root")
        self.assertEqual(remote_error(cm.exception), ACCESS_DENIED)
        c.verifier.call_cancel_sync(None)
        c.wait_closed()
        self.assertEqual(self.new_unlocks(), [])

    def test_other_services_are_unavailable(self):
        c = Client()
        c.begin(service="gdm-fingerprint")
        c.wait("conversation-stopped")
        self.assertEqual(c.events, [("service-unavailable", "gdm-fingerprint", ""),
                                    ("conversation-stopped", "gdm-fingerprint")])
        c.verifier.call_cancel_sync(None)
        c.wait_closed()
        self.assertEqual(self.new_unlocks(), [])

    def test_second_begin_while_running_is_an_error(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        with self.assertRaises(GLib.Error):
            c.begin()
        c.verifier.call_cancel_sync(None)
        c.wait_closed()

    def test_answer_without_a_question_is_an_error(self):
        c = Client()
        with self.assertRaises(GLib.Error):
            c.answer(PASSWORD)
        c.begin()
        c.wait("secret-info-query")
        c.answer(WRONG)
        with self.assertRaises(GLib.Error):
            c.answer(PASSWORD)  # the one question was answered already
        c.wait("conversation-stopped")
        settle()
        self.assertNotIn("verification-complete", c.names())
        c.verifier.call_cancel_sync(None)
        c.wait_closed()
        self.assertEqual(self.new_unlocks(), [])

    def test_cancel_mid_prompt_closes_without_unlocking(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.verifier.call_cancel_sync(None)
        c.wait_closed()
        settle()
        self.assertNotIn("verification-complete", c.names())
        self.assertEqual(self.new_unlocks(), [])

    def test_client_vanishing_mid_prompt_does_not_unlock(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.verifier.get_connection().close_sync(None)
        settle(1.0)
        self.assertEqual(self.new_unlocks(), [])

    def test_new_channel_from_the_same_process_replaces_the_old(self):
        first = Client()
        second = Client()
        first.wait_closed()
        self.assertFalse(second.closed())
        second.begin()
        second.wait("secret-info-query")
        second.answer(PASSWORD)
        second.wait("verification-complete")
        second.wait_closed()

    def test_socket_is_world_connectable_and_removed(self):
        (address,) = call_manager("OpenReauthenticationChannel", GLib.Variant("(s)", (USER,)), "(s)").unpack()
        self.assertTrue(address.startswith(f"unix:path={RUNTIME}/"), address)
        path = address[len("unix:path="):].split(",")[0]
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o666)
        conn = Gio.DBusConnection.new_for_address_sync(
            address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
        conn.call_sync(None, rgu.SESSION_PATH, rgu.VERIFIER_IFACE, "Cancel", None, None,
                       Gio.DBusCallFlags.NONE, 5000, None)
        self.assertTrue(iterate_until(conn.is_closed))
        self.assertTrue(iterate_until(lambda: not os.path.exists(path)))

    def test_fallback_to_the_users_graphical_session(self):
        # GNOME Shell runs as a systemd user unit: its pid is in no session.
        set_logind(display={str(UID): "s7"}, sessions={"s7": session()})
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        c.answer(PASSWORD)
        c.wait("verification-complete")
        c.wait_closed()
        self.assertTrue(iterate_until(lambda: self.new_unlocks() == ["s7"]), self.new_unlocks())

    def test_refused_sessions(self):
        cases = {
            "no session at all": dict(),
            "greeter session": dict(pids={str(PID): "g"}, sessions={"g": session(**{"class": "greeter"})}),
            "remote session": dict(pids={str(PID): "r"}, sessions={"r": session(remote=True)}),
            "tty session": dict(pids={str(PID): "t"}, sessions={"t": session(type="tty")}),
            "another user's session": dict(pids={str(PID): "o"}, sessions={"o": session(uid=UID + 1)}),
            "closing session": dict(pids={str(PID): "c"}, sessions={"c": session(state="closing")}),
            "fallback to a remote session": dict(display={str(UID): "r"}, sessions={"r": session(remote=True)}),
            "user with no display": dict(display={str(UID): ""}),
            # A caller IN a session gets that session or nothing, never the graphical one beside it.
            "own tty session beside a graphical one": dict(pids={str(PID): "t"}, display={str(UID): "s1"},
                                                           sessions={"t": session(type="tty"), "s1": session()}),
        }
        for name, cfg in cases.items():
            with self.subTest(name):
                set_logind(**cfg)
                with self.assertRaises(GLib.Error) as cm:
                    Client()
                self.assertEqual(remote_error(cm.exception), ACCESS_DENIED)
        self.assertEqual(os.listdir(RUNTIME), [])
        self.assertEqual(self.new_unlocks(), [])


class ChannelPeerTest(unittest.TestCase):
    def connect(self, uid):
        p, address = spawn(f"{HERE}/harness.py", "channel", f"{TMP}/peer-{uid}", str(uid))
        try:
            return Gio.DBusConnection.new_for_address_sync(
                address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
        finally:
            self.addCleanup(lambda: (p.terminate(), p.wait(), p.stdout.close()))

    def setUp(self):
        for uid in (UID, UID + 1):
            os.makedirs(f"{TMP}/peer-{uid}", mode=0o711, exist_ok=True)

    def test_the_sessions_own_uid_gets_in(self):
        conn = self.connect(UID)
        self.assertFalse(conn.is_closed())
        conn.close_sync(None)

    def test_any_other_uid_is_turned_away(self):
        with self.assertRaises(GLib.Error):
            conn = self.connect(UID + 1)
            # Some GLib versions finish the handshake and close right after: either way, no calls get through.
            conn.call_sync(None, rgu.SESSION_PATH, rgu.VERIFIER_IFACE, "EnableExtensions",
                           GLib.Variant("(as)", ([],)), None, Gio.DBusCallFlags.NONE, 5000, None)

    def test_peer_allowed(self):
        mine = Gio.Credentials.new()  # this process: uid UID
        self.assertTrue(rgu.peer_allowed(mine, UID))
        self.assertFalse(rgu.peer_allowed(mine, UID + 1))
        self.assertFalse(rgu.peer_allowed(None, UID))


class PamTest(unittest.TestCase):
    def setUp(self):
        self.pam = rgu.Pam(os.environ["PAM_WRAPPER_LIB"])
        self.prompts = []

    def converse_with(self, answer):
        def converse(style, text):
            self.prompts.append((style, text))
            if callable(answer):
                return answer()
            return answer
        return converse

    def test_right_password(self):
        rc, user = self.pam.authenticate("rime-test-plain", USER, self.converse_with(PASSWORD))
        self.assertEqual((rc, user), (rgu.PAM_SUCCESS, USER))
        self.assertEqual([s for s, _ in self.prompts], [rgu.PAM_PROMPT_ECHO_OFF])

    def test_wrong_password(self):
        rc, _ = self.pam.authenticate("rime-test-plain", USER, self.converse_with(WRONG))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_nul_in_answer_fails(self):
        rc, _ = self.pam.authenticate("rime-test-plain", USER, self.converse_with(PASSWORD + "\0x"))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_overlong_answer_fails(self):
        rc, _ = self.pam.authenticate("rime-test-plain", USER, self.converse_with("x" * rgu.PAM_MAX_RESP_SIZE))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_cancelled_conversation_fails(self):
        def boom():
            raise rgu.Cancelled()
        rc, _ = self.pam.authenticate("rime-test-plain", USER, self.converse_with(boom))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_any_exception_in_the_conversation_fails(self):
        def boom():
            raise ValueError("unexpected")
        rc, _ = self.pam.authenticate("rime-test-plain", USER, self.converse_with(boom))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_unknown_service_fails(self):
        rc, _ = self.pam.authenticate("rime-no-such-service", USER, self.converse_with(PASSWORD))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)


class StubChannel:
    def __init__(self, result=None, error=None):
        self.user = USER
        self.session_id = "s1"
        self.done = []
        result_, error_ = result, error

        class StubPam:
            def authenticate(self, _service, _user, _converse):
                if error_:
                    raise error_
                return result_

        self.pam = StubPam()

    def conversation_done(self, conversation, ok, code):
        self.done.append((ok, code))
        return GLib.SOURCE_REMOVE

    def emit_later(self, *_):
        pass


class DecisionTest(unittest.TestCase):
    """Conversation._run is the one place a PAM outcome becomes "unlock"."""

    def outcome(self, channel, cancel=False):
        conversation = rgu.Conversation(channel, "gdm-password")
        if cancel:
            conversation.cancel()
        conversation._run()
        self.assertTrue(iterate_until(lambda: channel.done))
        return channel.done[0][0]

    def test_success_for_the_same_user_passes(self):
        self.assertTrue(self.outcome(StubChannel((rgu.PAM_SUCCESS, USER))))

    def test_pam_failure_fails(self):
        self.assertFalse(self.outcome(StubChannel((7, USER))))

    def test_pam_success_for_another_user_fails(self):
        self.assertFalse(self.outcome(StubChannel((rgu.PAM_SUCCESS, "root"))))

    def test_pam_success_without_a_user_fails(self):
        self.assertFalse(self.outcome(StubChannel((rgu.PAM_SUCCESS, None))))

    def test_success_after_cancel_fails(self):
        self.assertFalse(self.outcome(StubChannel((rgu.PAM_SUCCESS, USER)), cancel=True))

    def test_exception_fails(self):
        self.assertFalse(self.outcome(StubChannel(error=RuntimeError("x"))))


class SessionCheckTest(unittest.TestCase):
    def test_only_a_local_graphical_user_session_of_the_caller(self):
        good = dict(session(), id="s1")
        rgu.check_session(good, UID)
        for change in ({"uid": UID + 1}, {"class": "greeter"}, {"class": "lock-screen"}, {"type": "tty"},
                       {"type": "unspecified"}, {"remote": True}, {"remote": None}, {"state": "closing"},
                       {"id": ""}):
            with self.subTest(change):
                with self.assertRaises(rgu.Refused):
                    rgu.check_session(dict(good, **change), UID)


if __name__ == "__main__":
    unittest.main()
