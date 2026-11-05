"""A stand-in for `rime_gnome_unlock.py --pam-worker`, for the cases real PAM modules cannot be made to show:

  fake_worker.py crash    asks for the password, then dies (SIGKILL, so no core dump reaches the host's
                          handler) once it has the answer: a crashing PAM module
  fake_worker.py hang     says nothing, ever: a PAM module stuck in a network call

The trailing arguments the daemon appends (service, user, tty) are ignored.
"""
import json
import os
import signal
import sys
import time

mode = sys.argv[1]
if mode == "crash":
    sys.stdout.write(json.dumps({"prompt": 1, "text": "Password: "}) + "\n")
    sys.stdout.flush()
    sys.stdin.readline()
    os.kill(os.getpid(), signal.SIGKILL)
elif mode == "hang":
    while True:
        time.sleep(60)
