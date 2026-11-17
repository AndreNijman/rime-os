#!/usr/bin/env python3
"""A notification server that records what it is sent, for tests on a
private session bus. Usage: fake-notify-server.py LOG.jsonl

Each Notify and CloseNotification is one JSON line. Never run it on a real
session: it takes org.freedesktop.Notifications."""
import sys, json
from gi.repository import Gio, GLib
XML = """<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/>
<arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method></interface></node>"""
log = open(sys.argv[1], "a"); n = [0]
def call(conn, sender, path, iface, method, params, inv):
    p = params.unpack()
    if method == "Notify":
        app, rep, icon, summ, body, actions, hints, exp = p
        n[0] += 1; nid = rep or n[0]
        log.write(json.dumps({"op":"notify","id":nid,"replaces":rep,"app":app,"icon":icon,"summary":summ,"body":body,
                              "actions":actions,"hints":{k:str(v) for k,v in hints.items()},"expire":exp})+"\n"); log.flush()
        inv.return_value(GLib.Variant("(u)", (nid,)))
    else:
        log.write(json.dumps({"op":"close","id":p[0]})+"\n"); log.flush(); inv.return_value(None)
node = Gio.DBusNodeInfo.new_for_xml(XML)
def acquired(conn, name):
    conn.register_object("/org/freedesktop/Notifications", node.interfaces[0], call, None, None)
Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.Notifications", 0, acquired, lambda *a: print("ready", flush=True), None)
GLib.MainLoop().run()
