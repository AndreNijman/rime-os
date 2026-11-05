"""rime-gnome-unlock against GNOME's own client library.

Three processes besides this one: a private dbus-daemon (the system bus), tests/fake_logind.py (systemd-logind) and
tests/harness.py (the installed daemon's Manager, with pam_wrapper's libpam; each conversation in its own
--pam-worker process, as installed). The client side is libgdm's Gdm.Client, the code GNOME Shell's unlock dialog
runs. PAM is pam_wrapper + pam_matrix (a password file), with pam_succeed_if in front requiring PAM_TTY /dev/tty7 (the
fake sessions' terminal) and pam_chatty so the Info path is exercised too.

Environment (default.nix sets it): RIME_GNOME_UNLOCK (the installed module), PAM_WRAPPER_LIB, PAM_WRAPPER_MODULES,
PAM_MODULES (Linux-PAM's own), GI_TYPELIB_PATH with Gdm-1.0, dbus-daemon on PATH. The file re-executes itself once
with pam_wrapper preloaded.
"""
import importlib.util
import json
import os
import pwd
import resource
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
    linux_pam = os.environ["PAM_MODULES"]
    os.makedirs(f"{tmp}/pam.d")
    with open(f"{tmp}/passdb", "w") as f:
        for service in ("rime-gnome-unlock", "rime-test-plain", "rime-test-tty"):
            f.write(f"{user}:{PASSWORD}:{service}\n")
    with open(f"{tmp}/pam.d/rime-gnome-unlock", "w") as f:
        f.write(f"auth requisite {linux_pam}/pam_succeed_if.so quiet tty = /dev/tty7\n"
                f"auth required {mods}/pam_chatty.so num_lines=1 info\n"
                f"auth required {mods}/pam_matrix.so passdb={tmp}/passdb\n")
    with open(f"{tmp}/pam.d/rime-test-plain", "w") as f:
        f.write(f"auth required {mods}/pam_matrix.so passdb={tmp}/passdb\n")
    with open(f"{tmp}/pam.d/rime-test-tty", "w") as f:
        f.write(f"auth requisite {linux_pam}/pam_succeed_if.so quiet tty = /dev/tty7\n"
                f"auth required {mods}/pam_matrix.so passdb={tmp}/passdb\n")
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
    s = {"uid": UID, "class": "user", "type": "wayland", "remote": False, "state": "active", "tty": "tty7"}
    s.update(over)
    return s


def set_logind(pids=None, display=None, sessions=None, delay=0):
    with open(LOGIND_CONFIG + ".new", "w") as f:
        json.dump({"pids": pids or {}, "display": display or {}, "sessions": sessions or {}, "delay": delay}, f)
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


def get_version():
    (version,) = system_bus().call_sync(rgu.BUS_NAME, rgu.MANAGER_PATH, "org.freedesktop.DBus.Properties", "Get",
                                        GLib.Variant("(ss)", (rgu.MANAGER_IFACE, "Version")),
                                        GLib.VariantType("(v)"), Gio.DBusCallFlags.NONE, 5000, None).unpack()
    return version


def connect_and_cancel(address):
    """Connects to a channel the way libgdm does and cancels it; returns once it is closed."""
    conn = Gio.DBusConnection.new_for_address_sync(address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
    conn.call_sync(None, rgu.SESSION_PATH, rgu.VERIFIER_IFACE, "Cancel", None, None, Gio.DBusCallFlags.NONE, 5000, None)
    assert iterate_until(conn.is_closed)


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
        version = get_version()
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

    def test_a_users_third_channel_closes_the_oldest(self):
        first, second = Client(), Client()
        settle()
        self.assertFalse(first.closed())
        third = Client()
        first.wait_closed()
        settle()
        self.assertFalse(second.closed())
        self.assertFalse(third.closed())
        second.verifier.call_cancel_sync(None)
        second.wait_closed()
        third.begin()
        third.wait("secret-info-query")
        third.answer(PASSWORD)
        third.wait("verification-complete")
        third.wait_closed()

    def test_connected_channel_with_no_conversation_closes_when_idle(self):
        c = Client()
        settle()
        self.assertFalse(c.closed())
        c.wait_closed(timeout=10)  # the harness's idle timeout is 2 s
        self.assertEqual(self.new_unlocks(), [])

    def test_idle_timeout_spares_a_running_conversation_and_returns_after_a_failed_one(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        settle(3.5)  # longer than the idle timeout, waiting at the prompt
        self.assertFalse(c.closed())
        c.answer(WRONG)
        c.wait("conversation-stopped")
        c.wait_closed(timeout=10)  # nothing begun again: idle
        self.assertEqual(self.new_unlocks(), [])

    def test_socket_is_the_session_users_alone_and_gone_once_connected(self):
        (address,) = call_manager("OpenReauthenticationChannel", GLib.Variant("(s)", (USER,)), "(s)").unpack()
        self.assertTrue(address.startswith(f"unix:path={RUNTIME}/"), address)
        path = address[len("unix:path="):].split(",")[0]
        st = os.stat(path)
        self.assertEqual(stat.S_IMODE(st.st_mode), 0o600)
        self.assertEqual(st.st_uid, UID)
        conn = Gio.DBusConnection.new_for_address_sync(
            address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
        # One client per channel: once it is in, the server stops listening and its socket is gone.
        self.assertTrue(iterate_until(lambda: not os.path.exists(path)))
        conn.call_sync(None, rgu.SESSION_PATH, rgu.VERIFIER_IFACE, "Cancel", None, None,
                       Gio.DBusCallFlags.NONE, 5000, None)
        self.assertTrue(iterate_until(conn.is_closed))

    def test_channels_of_a_removed_session_close(self):
        c = Client()
        c.begin()
        c.wait("secret-info-query")
        system_bus().call_sync("org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager",
                               "TestRemoveSession", GLib.Variant("(s)", ("s1",)), None, Gio.DBusCallFlags.NONE, 5000,
                               None)
        c.wait_closed()
        settle()
        self.assertNotIn("verification-complete", c.names())
        self.assertEqual(self.new_unlocks(), [])

    def test_version_answers_while_a_lookup_waits_on_logind(self):
        # gnome-shell asks for Version once, synchronously, at start-up; a slow logind must not hold that answer.
        set_logind(pids={str(PID): "s1"}, sessions={"s1": session()}, delay=3)
        replies = []

        def done(bus, result):
            try:
                replies.append(bus.call_finish(result))
            except GLib.Error as e:
                replies.append(e)

        system_bus().call(rgu.BUS_NAME, rgu.MANAGER_PATH, rgu.MANAGER_IFACE, "OpenReauthenticationChannel",
                          GLib.Variant("(s)", (USER,)), GLib.VariantType("(s)"), Gio.DBusCallFlags.NONE, 10000, None,
                          done)
        settle(0.3)
        t0 = time.monotonic()
        self.assertEqual(get_version(), "50.2")
        self.assertLess(time.monotonic() - t0, 1.5)
        self.assertTrue(iterate_until(lambda: replies, 10))
        self.assertIsInstance(replies[0], GLib.Variant, replies)
        connect_and_cancel(replies[0].unpack()[0])

    def test_pam_tty_is_the_sessions_terminal(self):
        # The test's PAM stack (pam_succeed_if) requires PAM_TTY /dev/tty7.
        for name, s, ok in (("TTY tty7", session(), True),
                            ("TTY /dev/tty7", session(tty="/dev/tty7"), True),
                            ("VTNr 7, no TTY", session(tty="", vtnr=7), True),
                            ("TTY tty8", session(tty="tty8"), False),
                            ("no terminal", session(tty=""), False)):
            with self.subTest(name):
                set_logind(pids={str(PID): "s1"}, sessions={"s1": s})
                c = Client()
                c.begin()
                if ok:
                    c.wait("secret-info-query")
                    c.answer(PASSWORD)
                    c.wait("verification-complete")
                    c.wait_closed()
                else:
                    c.wait("conversation-stopped")
                    self.assertNotIn("secret-info-query", c.names())
                    c.verifier.call_cancel_sync(None)
                    c.wait_closed()
        self.assertEqual(self.new_unlocks(), ["s1", "s1", "s1"])

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

    @unittest.skipIf(UID == 0, "root may chown to any uid")
    def test_a_socket_that_cannot_be_given_to_its_uid_is_not_served(self):
        runtime = f"{TMP}/peer-chown"
        os.makedirs(runtime, mode=0o711, exist_ok=True)

        class Stub:
            runtime_dir = runtime

        # EPERM, or EINVAL in a user namespace where that uid is not mapped (the build sandbox): refused either way.
        with self.assertRaises(OSError):
            rgu.Channel(Stub(), "c1", UID + 1, "someone-else")
        self.assertEqual(os.listdir(runtime), [])

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

    def test_tty_becomes_pam_tty(self):
        rc, _ = self.pam.authenticate("rime-test-tty", USER, self.converse_with(PASSWORD), tty="/dev/tty7")
        self.assertEqual(rc, rgu.PAM_SUCCESS)
        for tty in ("/dev/tty8", None):
            with self.subTest(tty):
                rc, _ = self.pam.authenticate("rime-test-tty", USER, self.converse_with(PASSWORD), tty=tty)
                self.assertNotEqual(rc, rgu.PAM_SUCCESS)

    def test_pam_tty_of_a_session(self):
        self.assertEqual(rgu.pam_tty({"tty": "tty2"}), "/dev/tty2")
        self.assertEqual(rgu.pam_tty({"tty": "/dev/tty3"}), "/dev/tty3")
        self.assertEqual(rgu.pam_tty({"tty": "", "vtnr": 4}), "/dev/tty4")
        self.assertEqual(rgu.pam_tty({"tty": "", "vtnr": 0, "display": ":1"}), ":1")
        self.assertEqual(rgu.pam_tty({}), "")

    def test_unknown_service_fails(self):
        rc, _ = self.pam.authenticate("rime-no-such-service", USER, self.converse_with(PASSWORD))
        self.assertNotEqual(rc, rgu.PAM_SUCCESS)


class StubChannel:
    """What a Conversation needs of its Channel, recording what it is told."""

    def __init__(self, worker_argv=None):
        self.user = USER
        self.session_id = "s1"
        self.tty = "/dev/tty7"
        self.manager = type("M", (), {"worker_argv": worker_argv})()
        self.closed = False
        self.conversations = {}
        self.done = []
        self.emitted = []

    def conversation_done(self, conversation, ok, code):
        self.done.append((ok, code))
        return GLib.SOURCE_REMOVE

    def emit(self, name, *args):
        self.emitted.append((name,) + args)

    def emit_later(self, name, *args):
        self.emitted.append((name,) + args)

    def _disarm_idle(self):
        pass


class DecisionTest(unittest.TestCase):
    """Conversation._run is the one place a PAM outcome becomes "unlock"."""

    def outcome(self, result=None, error=None, cancel=False):
        channel = StubChannel()
        conversation = rgu.Conversation(channel, "gdm-password")

        def authenticate():
            if error:
                raise error
            return result

        conversation._authenticate = authenticate
        if cancel:
            conversation.cancel()
        conversation._run()
        self.assertTrue(iterate_until(lambda: channel.done))
        return channel.done[0][0]

    def test_success_for_the_same_user_passes(self):
        self.assertTrue(self.outcome((rgu.PAM_SUCCESS, USER)))

    def test_pam_failure_fails(self):
        self.assertFalse(self.outcome((7, USER)))

    def test_pam_success_for_another_user_fails(self):
        self.assertFalse(self.outcome((rgu.PAM_SUCCESS, "root")))

    def test_pam_success_without_a_user_fails(self):
        self.assertFalse(self.outcome((rgu.PAM_SUCCESS, None)))

    def test_success_after_cancel_fails(self):
        self.assertFalse(self.outcome((rgu.PAM_SUCCESS, USER), cancel=True))

    def test_exception_fails(self):
        self.assertFalse(self.outcome(error=RuntimeError("x")))


def fake_worker(mode):
    return [sys.executable, "-I", "-B", f"{HERE}/fake_worker.py", mode]


class WorkerTest(unittest.TestCase):
    """Each conversation's PAM runs in a worker process: whatever it does, it ends that attempt only."""

    def setUp(self):
        self.step = rgu.PAM_STEP_TIMEOUT_S
        rgu.PAM_STEP_TIMEOUT_S = 1

    def tearDown(self):
        rgu.PAM_STEP_TIMEOUT_S = self.step

    def run_worker(self, mode, answer=None, cancel_after=None):
        channel = StubChannel(fake_worker(mode))
        conversation = rgu.Conversation(channel, "gdm-password")
        conversation.start()
        t0 = time.monotonic()
        if answer is not None:
            self.assertTrue(iterate_until(lambda: conversation.answer(answer), 10))
        if cancel_after is not None:
            settle(cancel_after)
            conversation.cancel()
        self.assertTrue(iterate_until(lambda: channel.done, 10), "the conversation never ended")
        self.assertIsNotNone(conversation.proc.returncode, "the worker is still running")
        return channel, conversation, time.monotonic() - t0

    def test_a_worker_that_dies_mid_conversation_fails_the_attempt(self):
        channel, _, _ = self.run_worker("crash", answer=PASSWORD)
        self.assertEqual(channel.done, [(False, None)])
        self.assertIn(("SecretInfoQuery", "gdm-password", "Password: "), channel.emitted)

    def test_a_worker_that_stops_answering_is_killed(self):
        channel, conversation, took = self.run_worker("hang")
        self.assertEqual(channel.done, [(False, None)])
        self.assertEqual(conversation.proc.returncode, -9)
        self.assertLess(took, 5)

    def test_cancel_kills_the_worker(self):
        rgu.PAM_STEP_TIMEOUT_S = 60
        channel, conversation, took = self.run_worker("hang", cancel_after=0.3)
        self.assertEqual(channel.done, [(False, None)])
        self.assertEqual(conversation.proc.returncode, -9)
        self.assertLess(took, 5)

    def test_the_real_worker_runs_pam(self):
        argv = [sys.executable, "-I", "-B", os.environ["RIME_GNOME_UNLOCK"], "--pam-worker",
                os.environ["PAM_WRAPPER_LIB"]]
        for password, ok in ((PASSWORD, True), (WRONG, False)):
            with self.subTest(ok=ok):
                channel = StubChannel(argv)
                conversation = rgu.Conversation(channel, "gdm-password")
                conversation.start()
                self.assertTrue(iterate_until(lambda: conversation.answer(password), 10))
                self.assertTrue(iterate_until(lambda: channel.done, 10))
                self.assertEqual(channel.done[0][0], ok)
                self.assertEqual(conversation.proc.returncode, 0)


class DispatchTest(unittest.TestCase):
    class Invocation:
        def __init__(self):
            self.replies = []

        def return_value(self, value):
            self.replies.append(("value", value))

        def return_dbus_error(self, name, _message):
            self.replies.append(("error", name))

    def begin(self, channel):
        invocation = self.Invocation()
        rgu.Channel._dispatch(channel, "BeginVerification", ("gdm-password",), invocation)
        return invocation.replies

    def test_a_conversation_that_cannot_start_is_an_error_and_not_kept(self):
        channel = StubChannel(["/nonexistent/pam-worker"])
        self.assertEqual(self.begin(channel), [("error", "org.freedesktop.DBus.Error.Failed")])
        self.assertEqual(channel.conversations, {})
        self.assertEqual(channel.emitted, [])
        # Nothing stale blocks the next attempt: it fails the same way, not as "already running".
        self.assertEqual(self.begin(channel), [("error", "org.freedesktop.DBus.Error.Failed")])

    def test_a_started_conversation_is_kept_then_announced(self):
        channel = StubChannel(fake_worker("hang"))
        self.assertEqual(self.begin(channel), [("value", None)])
        self.assertEqual(channel.emitted, [("ConversationStarted", "gdm-password")])
        conversation = channel.conversations["gdm-password"]
        conversation.cancel()
        self.assertTrue(iterate_until(lambda: channel.done, 10))


class FdPressureTest(unittest.TestCase):
    def test_past_three_quarters_of_the_limit(self):
        n = len(os.listdir("/proc/self/fd"))
        self.assertFalse(rgu.fd_pressure(n * 4))
        self.assertTrue(rgu.fd_pressure(n))
        self.assertFalse(rgu.fd_pressure())  # this test process, at its real limit
        self.assertGreater(resource.getrlimit(resource.RLIMIT_NOFILE)[0], n)


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
