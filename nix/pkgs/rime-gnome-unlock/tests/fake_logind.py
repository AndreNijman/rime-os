"""A stand-in for systemd-logind on the test bus: the calls rime-gnome-unlock makes, answered from a JSON file that
the test rewrites between cases (FAKE_LOGIND_CONFIG), and every UnlockSession appended to FAKE_LOGIND_UNLOCKS.

  {"pids": {"<pid>": "<session id>"},          GetSessionByPID (absent: org.freedesktop.login1.NoSessionForPID)
   "display": {"<uid>": "<session id>"},       GetUser + User.Display (absent uid: NoSuchUser)
   "sessions": {"<id>": {"uid": 1000, "class": "user", "type": "wayland", "remote": false, "state": "active",
                         "tty": "tty7"}},                      (tty optional)
   "delay": 0}                                  seconds GetSessionByPID and GetUser take (a loaded logind)

TestRemoveSession(s) (test only) emits Manager.SessionRemoved for that id.
"""
import json
import os
import sys
import time

from gi.repository import Gio, GLib

ROOT = "/org/freedesktop/login1"

MANAGER_XML = """<node><interface name="org.freedesktop.login1.Manager">
  <method name="GetSessionByPID"><arg type="u" direction="in"/><arg type="o" direction="out"/></method>
  <method name="GetUser"><arg type="u" direction="in"/><arg type="o" direction="out"/></method>
  <method name="UnlockSession"><arg type="s" direction="in"/></method>
  <method name="TestRemoveSession"><arg type="s" direction="in"/></method>
  <signal name="SessionRemoved"><arg type="s"/><arg type="o"/></signal>
</interface></node>"""
SESSION_XML = """<node><interface name="org.freedesktop.login1.Session">
  <property name="Id" type="s" access="read"/>
  <property name="User" type="(uo)" access="read"/>
  <property name="Class" type="s" access="read"/>
  <property name="Type" type="s" access="read"/>
  <property name="Remote" type="b" access="read"/>
  <property name="State" type="s" access="read"/>
  <property name="TTY" type="s" access="read"/>
  <property name="VTNr" type="u" access="read"/>
  <property name="Display" type="s" access="read"/>
</interface></node>"""
USER_XML = """<node><interface name="org.freedesktop.login1.User">
  <property name="Display" type="(so)" access="read"/>
</interface></node>"""


def config():
    with open(os.environ["FAKE_LOGIND_CONFIG"]) as f:
        return json.load(f)


def session_path(sid):
    return f"{ROOT}/session/{sid}"


class FakeLogind:
    def __init__(self, bus):
        self.bus = bus
        self.registered = set()
        bus.register_object_with_closures2(ROOT, Gio.DBusNodeInfo.new_for_xml(MANAGER_XML).interfaces[0], self.on_manager, None, None)

    def ensure(self, path, xml, getter):
        if path not in self.registered:
            self.bus.register_object_with_closures2(path, Gio.DBusNodeInfo.new_for_xml(xml).interfaces[0], None, getter, None)
            self.registered.add(path)
        return path

    def session(self, sid):
        def get(_c, _s, _p, _i, name):
            s = config()["sessions"].get(sid)
            if s is None:
                return None
            return {
                "Id": GLib.Variant("s", sid),
                "User": GLib.Variant("(uo)", (s["uid"], f"{ROOT}/user/_{s['uid']}")),
                "Class": GLib.Variant("s", s["class"]),
                "Type": GLib.Variant("s", s["type"]),
                "Remote": GLib.Variant("b", s["remote"]),
                "State": GLib.Variant("s", s["state"]),
                "TTY": GLib.Variant("s", s.get("tty", "")),
                "VTNr": GLib.Variant("u", s.get("vtnr", 0)),
                "Display": GLib.Variant("s", s.get("display", "")),
            }[name]

        return self.ensure(session_path(sid), SESSION_XML, get)

    def user(self, uid):
        def get(_c, _s, _p, _i, _name):
            sid = config()["display"].get(str(uid))
            return GLib.Variant("(so)", (sid, self.session(sid)) if sid else ("", "/"))

        return self.ensure(f"{ROOT}/user/_{uid}", USER_XML, get)

    def on_manager(self, _c, _sender, _path, _iface, method, params, invocation):
        cfg = config()
        if method in ("GetSessionByPID", "GetUser"):
            time.sleep(cfg.get("delay", 0))
        if method == "GetSessionByPID":
            (pid,) = params.unpack()
            sid = cfg["pids"].get(str(pid))
            if sid is None:
                invocation.return_dbus_error("org.freedesktop.login1.NoSessionForPID", f"PID {pid} has no session")
            else:
                invocation.return_value(GLib.Variant("(o)", (self.session(sid),)))
        elif method == "GetUser":
            (uid,) = params.unpack()
            if str(uid) not in cfg["display"]:
                invocation.return_dbus_error("org.freedesktop.login1.NoSuchUser", f"user {uid} is not logged in")
            else:
                invocation.return_value(GLib.Variant("(o)", (self.user(uid),)))
        elif method == "UnlockSession":
            (sid,) = params.unpack()
            with open(os.environ["FAKE_LOGIND_UNLOCKS"], "a") as f:
                f.write(sid + "\n")
            invocation.return_value(None)
        elif method == "TestRemoveSession":
            (sid,) = params.unpack()
            self.bus.emit_signal(None, ROOT, "org.freedesktop.login1.Manager", "SessionRemoved",
                                 GLib.Variant("(so)", (sid, session_path(sid))))
            invocation.return_value(None)


def main():
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    FakeLogind(bus)
    loop = GLib.MainLoop()

    def acquired(*_):
        print("ready", flush=True)

    Gio.bus_own_name_on_connection(bus, "org.freedesktop.login1", Gio.BusNameOwnerFlags.NONE, acquired,
                                   lambda *_: sys.exit("lost org.freedesktop.login1"))
    loop.run()


if __name__ == "__main__":
    main()
