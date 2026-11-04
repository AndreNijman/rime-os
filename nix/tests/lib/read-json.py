# read_json(machine, path): a guest JSON file, read whole or not at all. Included into VM test scripts.
#
# Not json.loads(machine.succeed(f"cat {path}")). succeed() brings output back over the guest console as ONE base64
# line ending in a newline, and checks only the command's exit status, not that the line arrived whole: a short line
# whose length is still a multiple of 4 decodes without an error. Seen twice on hosted runners: update-e2e run
# 41597182018 read an 80 kB split.json as its first 78 kB ("Unterminated string"); update-delta run 41732071922 read
# an 86 kB manifest.json as broken JSON at byte 86022 ("Expecting ',' delimiter"), while the same check passed
# locally. The file crosses the driver's shared directory instead and must match the sha256 the guest computed.
import hashlib as _rj_hashlib
import itertools as _rj_itertools
import json as _rj_json

_rj_reads = _rj_itertools.count()


def read_json(machine, path):
    name = f"read-json-{next(_rj_reads)}"
    want = machine.succeed(f"cp {path} /tmp/shared/{name} && sync /tmp/shared/{name} && sha256sum {path}").split()[0]
    host = machine.shared_dir / name
    data = host.read_bytes()
    host.unlink()
    got = _rj_hashlib.sha256(data).hexdigest()
    assert got == want, f"{machine.name}:{path} came back as {len(data)} bytes with sha256 {got}, not {want}"
    return _rj_json.loads(data)
