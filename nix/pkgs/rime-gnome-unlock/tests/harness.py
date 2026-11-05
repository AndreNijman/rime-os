"""Runs the installed rime_gnome_unlock module on the test bus, with pam_wrapper's libpam in place of the real one.

  harness.py manager <runtime dir>          the Manager as main() would run it (name, logind, PAM workers), with a
                                            short idle timeout (RGU_IDLE_TIMEOUT_S, default 2 s)
  harness.py channel <runtime dir> <uid>    one bare reauthentication Channel admitting <uid> only; prints its address

The daemon itself takes the libpam path from its build; this is the only place another one is passed in. A channel
for another uid cannot be chowned to it without root: there the socket keeps this process's uid (still 0600), so the
test reaches the server's own uid check.
"""
import importlib.util
import os
import sys

from gi.repository import Gio, GLib


def load():
    spec = importlib.util.spec_from_file_location("rime_gnome_unlock", os.environ["RIME_GNOME_UNLOCK"])
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    mode, runtime_dir = sys.argv[1], sys.argv[2]
    rgu = load()
    loop = GLib.MainLoop()
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    if mode == "manager":
        rgu.IDLE_TIMEOUT_S = float(os.environ.get("RGU_IDLE_TIMEOUT_S", "2"))
        rgu.Manager(bus, os.environ["PAM_WRAPPER_LIB"], rgu.Logind(bus), runtime_dir)
        Gio.bus_own_name_on_connection(bus, rgu.BUS_NAME, Gio.BusNameOwnerFlags.DO_NOT_QUEUE,
                                       lambda *_: print("ready", flush=True), lambda *_: loop.quit())
    elif mode == "channel":
        class Stub:
            logind = None
            worker_argv = None

            def __init__(self):
                self.runtime_dir = runtime_dir

            def channel_closed(self, _channel):
                pass

        uid = int(sys.argv[3])
        if uid != os.getuid() and os.getuid() != 0:
            rgu.restrict_socket = lambda path, _uid: os.chmod(path, 0o600)
        channel = rgu.Channel(Stub(), "c1", uid, "nobody-in-particular")
        print(channel.address, flush=True)
    loop.run()


if __name__ == "__main__":
    main()
