#!/usr/bin/env python3
"""fake-github: a TEST stand-in for the parts of GitHub the release publisher talks to (stand-in and VM tests only).

  fake-github --dir DIR --repo OWNER/NAME --token TOKEN --api HOST:PORT --download HOST:PORT --objects HOST:PORT
              [--objects-url http://objects.example:PORT]

  api       the REST calls rime-nix-release makes (api.github.com and uploads.github.com share it here):
              GET  /repos/O/N/releases/tags/TAG          POST /repos/O/N/releases
              GET  /repos/O/N/releases/ID/assets         POST /repos/O/N/releases/ID/assets?name=NAME
              DELETE /repos/O/N/releases/assets/ID
            every write needs `Authorization: Bearer TOKEN`; reads are anonymous, as on a public repository
  download  github.com: GET/HEAD /O/N/releases/download/TAG/NAME answers 302 to the objects origin, as GitHub
            redirects to objects.githubusercontent.com
  objects   serves the asset bytes (GET, HEAD, Range: bytes=a-b)

Assets are stored under DIR/files/<id>, metadata in DIR/state.json. A release asset's name is unique per release;
an upload whose name is taken answers 422, like GitHub.
"""

import argparse
import json
import os
import re
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class State:
    def __init__(self, d, repo, token):
        self.dir, self.repo, self.token = d, repo, token
        self.lock = threading.Lock()
        os.makedirs(os.path.join(d, "files"), exist_ok=True)
        p = os.path.join(d, "state.json")
        self.data = json.load(open(p)) if os.path.exists(p) else {"next": 1, "releases": []}

    def save(self):
        tmp = os.path.join(self.dir, "state.json.tmp")
        json.dump(self.data, open(tmp, "w"), indent=1)
        os.replace(tmp, os.path.join(self.dir, "state.json"))

    def new_id(self):
        n = self.data["next"]
        self.data["next"] += 1
        return n

    def release_by_tag(self, tag):
        return next((r for r in self.data["releases"] if r["tag_name"] == tag), None)

    def release_by_id(self, rid):
        return next((r for r in self.data["releases"] if r["id"] == rid), None)

    def asset_by_id(self, aid):
        for r in self.data["releases"]:
            for a in r["assets"]:
                if a["id"] == aid:
                    return r, a
        return None, None


def public(rel):
    return {k: v for k, v in rel.items() if k != "assets"}


def handler(state, role, objects_url):
    o, n = state.repo.split("/")

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):
            pass

        def reply(self, code, body=None, headers=None, raw=None):
            data = raw if raw is not None else (json.dumps(body).encode() if body is not None else b"")
            self.send_response(code)
            for k, v in (headers or {}).items():
                self.send_header(k, v)
            if raw is None:
                self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)

        def authed(self):
            return self.headers.get("Authorization", "") in (f"Bearer {state.token}", f"token {state.token}")

        def body(self):
            n = int(self.headers.get("Content-Length") or 0)
            return self.rfile.read(n) if n else b""

        # ── api ──
        def api(self):
            u = urllib.parse.urlparse(self.path)
            q = dict(urllib.parse.parse_qsl(u.query))
            base = f"/repos/{o}/{n}/releases"
            with state.lock:
                if self.command in ("POST", "DELETE") and not self.authed():
                    self.body()
                    return self.reply(401, {"message": "Bad credentials"})
                m = re.match(rf"^{base}/tags/(.+)$", u.path)
                if self.command == "GET" and m:
                    r = state.release_by_tag(urllib.parse.unquote(m.group(1)))
                    return self.reply(200, public(r)) if r else self.reply(404, {"message": "Not Found"})
                if self.command == "POST" and u.path == base:
                    req = json.loads(self.body() or b"{}")
                    if state.release_by_tag(req.get("tag_name", "")):
                        return self.reply(422, {"message": "Validation Failed", "errors": [{"code": "already_exists"}]})
                    r = {"id": state.new_id(), "tag_name": req["tag_name"], "name": req.get("name"),
                         "draft": bool(req.get("draft")), "assets": []}
                    state.data["releases"].append(r)
                    state.save()
                    return self.reply(201, public(r))
                m = re.match(rf"^{base}/([0-9]+)/assets$", u.path)
                if m and self.command == "GET":
                    r = state.release_by_id(int(m.group(1)))
                    if not r:
                        return self.reply(404, {"message": "Not Found"})
                    per, page = int(q.get("per_page", 30)), int(q.get("page", 1))
                    return self.reply(200, r["assets"][(page - 1) * per: page * per])
                if m and self.command == "POST":
                    r = state.release_by_id(int(m.group(1)))
                    data = self.body()
                    name = q.get("name", "")
                    if not r:
                        return self.reply(404, {"message": "Not Found"})
                    if any(a["name"] == name for a in r["assets"]):
                        return self.reply(422, {"message": "Validation Failed",
                                                "errors": [{"code": "already_exists", "field": "name"}]})
                    a = {"id": state.new_id(), "name": name, "size": len(data), "state": "uploaded",
                         "browser_download_url": f"/{o}/{n}/releases/download/{r['tag_name']}/{name}"}
                    open(os.path.join(state.dir, "files", str(a["id"])), "wb").write(data)
                    r["assets"].append(a)
                    state.save()
                    return self.reply(201, a)
                m = re.match(rf"^{base}/assets/([0-9]+)$", u.path)
                if m and self.command == "DELETE":
                    r, a = state.asset_by_id(int(m.group(1)))
                    if not a:
                        return self.reply(404, {"message": "Not Found"})
                    r["assets"].remove(a)
                    try:
                        os.remove(os.path.join(state.dir, "files", str(a["id"])))
                    except FileNotFoundError:
                        pass
                    state.save()
                    return self.reply(204)
            return self.reply(404, {"message": "Not Found"})

        # ── github.com/O/N/releases/download/TAG/NAME → 302 ──
        def download(self):
            m = re.match(rf"^/{o}/{n}/releases/download/([^/]+)/([^/?]+)$", urllib.parse.urlparse(self.path).path)
            with state.lock:
                r = state.release_by_tag(urllib.parse.unquote(m.group(1))) if m else None
                a = next((a for a in (r or {}).get("assets", []) if a["name"] == m.group(2)), None) if r else None
            if not a or a.get("state") != "uploaded":
                return self.reply(404, raw=b"Not Found", headers={"Content-Type": "text/plain"})
            # What a client fetched, for the tests (update-delta.nix): "ADDRESS METHOD TAG/NAME".
            with state.lock, open(os.path.join(state.dir, "downloads.log"), "a") as f:
                f.write(f"{self.client_address[0]} {self.command} {m.group(1)}/{m.group(2)}\n")
            return self.reply(302, raw=b"", headers={"Location": f"{objects_url}/{a['id']}/{a['name']}"})

        # ── objects origin ──
        def objects(self):
            m = re.match(r"^/([0-9]+)/[^/]+$", urllib.parse.urlparse(self.path).path)
            f = os.path.join(state.dir, "files", m.group(1)) if m else None
            if not f or not os.path.exists(f):
                return self.reply(404, raw=b"Not Found", headers={"Content-Type": "text/plain"})
            data = open(f, "rb").read()
            rng = re.match(r"^bytes=([0-9]+)-([0-9]*)$", self.headers.get("Range", ""))
            hdr = {"Content-Type": "application/octet-stream", "Accept-Ranges": "bytes"}
            if rng:
                a, b = int(rng.group(1)), int(rng.group(2) or len(data) - 1)
                hdr["Content-Range"] = f"bytes {a}-{b}/{len(data)}"
                return self.reply(206, raw=data[a:b + 1], headers=hdr)
            return self.reply(200, raw=data, headers=hdr)

        def route(self):
            return {"api": self.api, "download": self.download, "objects": self.objects}[role]()

        do_GET = do_HEAD = do_POST = do_DELETE = route

    return H


def serve(state, role, addr, objects_url):
    host, port = addr.rsplit(":", 1)
    srv = ThreadingHTTPServer((host, int(port)), handler(state, role, objects_url))
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv


def main():
    ap = argparse.ArgumentParser(prog="fake-github")
    ap.add_argument("--dir", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--token", required=True)
    ap.add_argument("--api", required=True)
    ap.add_argument("--download", required=True)
    ap.add_argument("--objects", required=True)
    ap.add_argument("--objects-url")
    a = ap.parse_args()
    state = State(a.dir, a.repo, a.token)
    objects_url = (a.objects_url or f"http://{a.objects}").rstrip("/")
    srvs = [serve(state, "api", a.api, objects_url), serve(state, "download", a.download, objects_url),
            serve(state, "objects", a.objects, objects_url)]
    print(f"fake-github: api {a.api}, download {a.download}, objects {a.objects}", flush=True)
    threading.Event().wait()
    for s in srvs:
        s.shutdown()


if __name__ == "__main__":
    main()
