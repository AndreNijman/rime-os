#!/usr/bin/python3 -I
"""Check that rime_liveupdate reached the binary policy and file contexts.

`semodule -l` only proves a module is installed. This reads the compiled
policy the kernel loads and the file_contexts udev labels from.
"""
import subprocess
import sys

import setools

TYPE = "liveupdate_device_t"
fails = []
p = setools.SELinuxPolicy()
try:
    t = p.lookup_type(TYPE)
except Exception as e:  # noqa: BLE001 - any lookup failure is the answer
    sys.exit(f"{TYPE} is not in the binary policy: {e}")
attrs = sorted(str(a) for a in t.attributes())
if attrs != ["device_node"]:
    fails.append(f"{TYPE} attributes are {attrs}, expected exactly ['device_node']")
direct = list(setools.TERuleQuery(p, ruletype=["allow"], target=TYPE, target_indirect=False).results())
if direct:
    fails.append(f"{TYPE} has rules of its own: {[str(r) for r in direct]}")
udev = setools.TERuleQuery(p, ruletype=["allow"], source="udev_t", target=TYPE,
                           tclass=["chr_file"], perms=["relabelto"])
if not list(udev.results()):
    fails.append("udev_t cannot relabel a node to " + TYPE)
# Root in an unconfined login must still reach it (LUO experiments are run
# that way); Fedora grants that through device_node, not a rule of ours.
root = setools.TERuleQuery(p, ruletype=["allow"], source="unconfined_t", target=TYPE,
                           tclass=["chr_file"], perms=["open", "read", "write", "ioctl"])
if not any({"open", "read", "write", "ioctl"} <= set(r.perms) for r in root.results()):
    fails.append("unconfined_t cannot open, read, write and ioctl " + TYPE)
ctx = subprocess.run(["matchpathcon", "-m", "chr_file", "/dev/liveupdate"],
                     capture_output=True, text=True).stdout.split()
if len(ctx) != 2 or ctx[1] != f"system_u:object_r:{TYPE}:s0":
    fails.append(f"/dev/liveupdate resolves to {ctx}")
for f in fails:
    print("FAIL", f)
if fails:
    sys.exit(1)
print(f"/dev/liveupdate -> {TYPE} (device_node only, no rules of its own)")
