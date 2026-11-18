#!/usr/bin/env python3
"""A notification server that records what it is sent, for tests on a
private session bus. Usage: fake-notify-server.py LOG.jsonl

Each Notify, CloseNotification and emitted NotificationClosed is one JSON
line. Like a real server it emits NotificationClosed(id, 3) for a
CloseNotification. A Notify titled "close-at-once" is closed as dismissed (2)
BEFORE its reply goes out. org.rimeos.Test.Emit(id, reason) on /test makes it
emit NotificationClosed, as a person dismissing it (2) or a timeout (1) would.
org.rimeos.Test.Reply(id, text) emits NotificationReplied(id, text) and then
NotificationClosed(id, 2) back to back, as Quickshell does when a person
answers in the reply field. GetCapabilities lists inline-reply unless
FAKE_NOTIFY_INLINE_REPLY=0.
Never run it on a real session: it takes org.freedesktop.Notifications."""
import sys, json, os
from gi.repository import Gio, GLib
XML = """<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/>
<arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<method name="GetCapabilities"><arg type="as" direction="out"/></method>
<signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
<signal name="NotificationReplied"><arg type="u"/><arg type="s"/></signal></interface>
<interface name="org.rimeos.Test"><method name="Emit"><arg type="u" direction="in"/><arg type="u" direction="in"/></method>
<method name="Reply"><arg type="u" direction="in"/><arg type="s" direction="in"/></method>
</interface></node>"""
CAPS = ["body", "actions"] + ([] if os.environ.get("FAKE_NOTIFY_INLINE_REPLY") == "0" else ["inline-reply"])
log = open(sys.argv[1], "a"); n = [0]
def closed(conn, nid, reason):
    log.write(json.dumps({"op":"closed","id":nid,"reason":reason})+"\n"); log.flush()
    conn.emit_signal(None, "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "NotificationClosed",
                     GLib.Variant("(uu)", (nid, reason)))
def call(conn, sender, path, iface, method, params, inv):
    p = params.unpack()
    if method == "Emit":
        closed(conn, p[0], p[1]); inv.return_value(None)
    elif method == "Reply":
        log.write(json.dumps({"op":"replied","id":p[0],"text":p[1]})+"\n"); log.flush()
        conn.emit_signal(None, "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "NotificationReplied",
                         GLib.Variant("(us)", (p[0], p[1])))
        closed(conn, p[0], 2); inv.return_value(None)
    elif method == "GetCapabilities":
        inv.return_value(GLib.Variant("(as)", (CAPS,)))
    elif method == "Notify":
        app, rep, icon, summ, body, actions, hints, exp = p
        n[0] += 1; nid = rep or n[0]
        log.write(json.dumps({"op":"notify","id":nid,"replaces":rep,"app":app,"icon":icon,"summary":summ,"body":body,
                              "actions":actions,"hints":{k:str(v) for k,v in hints.items()},"expire":exp})+"\n"); log.flush()
        if summ == "close-at-once": closed(conn, nid, 2)
        inv.return_value(GLib.Variant("(u)", (nid,)))
    else:
        log.write(json.dumps({"op":"close","id":p[0]})+"\n"); log.flush(); inv.return_value(None)
        closed(conn, p[0], 3)
node = Gio.DBusNodeInfo.new_for_xml(XML)
def acquired(conn, name):
    conn.register_object("/org/freedesktop/Notifications", node.interfaces[0], call, None, None)
    conn.register_object("/test", node.interfaces[1], call, None, None)
Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.Notifications", 0, acquired, lambda *a: print("ready", flush=True), None)
GLib.MainLoop().run()
