#!/usr/bin/env python3
"""The cargo test total for the rimed workspace, from the checkPhase log.

    rimed-test-report.py LOG METADATA SKIPS OUT_TXT OUT_JSON

LOG       everything `cargo test --workspace --no-fail-fast -- --show-output` printed
METADATA  `cargo metadata --no-deps --format-version 1`, to name the crate each test target belongs to
SKIPS     JSON: the groups from rimed.nix, {"skip": {group: [name, ...]}, "selfSkipOk": {group: [name, ...]}}
OUT_*     the report, as text and as JSON. Neither holds a timing, so the output stays reproducible.

cargo's output is per test TARGET (a crate's unit tests, each tests/*.rs, its doc-tests); this sums the targets
into crates. A test that printed a SKIP line and returned is counted as "self-skip", NOT as passed: cargo calls
it ok, but it proved nothing. It refuses (exit 1) when the total cannot be trusted:

  - a target header without a `test result:` line, or the other way round (a lost or unattributed binary);
  - a failed test;
  - the `filtered out` total differs from the number of --skip names: a renamed or deleted test leaves its
    skip entry matching nothing (or a name that matches two tests skips one nobody chose), and either way the
    skip list no longer says what is skipped;
  - a test that PASSED while printing a SKIP line: it returned early and proved nothing, so it must be named
    in a selfSkipOk group (the group is the reason) or fixed. A passing test's output is only in the log because
    of --show-output;
  - a selfSkipOk name that did not skip itself (fixed, renamed or deleted): the list must stay true.
"""
import json
import re
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
HEADER = re.compile(r"^\s*Running (?:unittests )?(\S+) \((\S+)\)\s*$")
DOCTEST = re.compile(r"^\s*Doc-tests (\S+)\s*$")
RESULT = re.compile(
    r"^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out"
)
IGNORED = re.compile(r"^test (\S+) \.\.\. ignored(?:, (.*))?$")
STDOUT_START = re.compile(r"^---- (\S+) stdout ----$")
# A line that STARTS with SKIP, skipped or skipping (any case): "SKIP: ...", "skipped: loginctl is not on PATH".
# Anchored, so a daemon's own log line under test ("rimed: skip (absent) ...") is not taken for one.
SELF_SKIP = re.compile(r"^\s*(?:SKIP|skipp(?:ed|ing))\b", re.IGNORECASE)


def norm(name):
    return name.replace("-", "_")


def targets_from(metadata):
    out = []
    for pkg in metadata["packages"]:
        root = pkg["manifest_path"].rsplit("/", 1)[0] + "/"
        for t in pkg["targets"]:
            rel = t["src_path"][len(root):] if t["src_path"].startswith(root) else t["src_path"]
            out.append((pkg["name"], norm(t["name"]), rel, t["kind"]))
    return out


def allowed(name, names):
    return any(name == k or name.endswith("::" + k) for k in names)


def crate_of(targets, src, binary, last):
    stem = binary.rsplit("/", 1)[-1]
    name = norm(stem.rsplit("-", 1)[0]) if re.search(r"-[0-9a-f]{16}$", stem) else norm(stem)
    hits = [t for t in targets if t[1] == name and t[2] == src]
    if len(hits) > 1 and last:
        hits = [t for t in hits if t[0] == last] or hits
    return hits[0][0] if len(hits) == 1 else None


def main(log_path, meta_path, skips_path, out_txt, out_json):
    targets = targets_from(json.load(open(meta_path)))
    cfg = json.load(open(skips_path))
    skip_groups = cfg["skip"]
    self_skip_groups = cfg.get("selfSkipOk", {})
    self_skip_names = [n for names in self_skip_groups.values() for n in names]
    skip_names = [n for names in skip_groups.values() for n in names]

    errors = []
    runs = []  # one per target: crate, target, counts
    pending = None
    last_crate = None
    ignored = []
    self_skipped = []  # (crate, test, the SKIP line), each test once per target
    seen_self_skip = set()
    current_stdout = None

    for raw in open(log_path, errors="replace"):
        line = ANSI.sub("", raw.rstrip("\n"))
        m = HEADER.match(line)
        d = DOCTEST.match(line)
        if m or d:
            if pending is not None:
                errors.append(f"no `test result:` line for {pending['target']}")
            if m:
                crate = crate_of(targets, m.group(1), m.group(2), last_crate)
                target = m.group(1)
            else:
                crate = next((t[0] for t in targets if t[1] == norm(d.group(1)) and "lib" in t[3]), None)
                target = "doc-tests"
            if crate is None:
                errors.append(f"cannot name the crate of: {line.strip()}")
                crate = "?"
            last_crate = crate
            pending = {"crate": crate, "target": target}
            current_stdout = None
            continue
        m = STDOUT_START.match(line)
        if m:
            current_stdout = m.group(1)
            continue
        if line.startswith("successes:") or line.startswith("failures:") or line.startswith("running "):
            current_stdout = None
        if current_stdout and SELF_SKIP.search(line):
            crate = pending["crate"] if pending else last_crate
            key = (crate, pending["target"] if pending else None, current_stdout)
            if key not in seen_self_skip:
                seen_self_skip.add(key)
                self_skipped.append((crate, current_stdout, line.strip()))
        m = IGNORED.match(line)
        if m:
            ignored.append((last_crate, m.group(1), m.group(2) or "no reason given"))
            continue
        m = RESULT.match(line)
        if m:
            if pending is None:
                errors.append(f"a `test result:` line with no target header: {line}")
                continue
            passed, failed, ign, _measured, filtered = (int(x) for x in m.groups()[1:])
            pending.update(passed=passed, failed=failed, ignored=ign, filtered=filtered)
            runs.append(pending)
            pending = None
    if pending is not None:
        errors.append(f"no `test result:` line for {pending['target']} (the log ends inside it)")
    if not runs:
        errors.append("no test target ran at all")

    keys = ("targets", "passed", "selfskip", "failed", "ignored", "filtered")
    crates = {}
    for r in runs:
        c = crates.setdefault(r["crate"], dict.fromkeys(keys, 0))
        c["targets"] += 1
        for k in ("passed", "failed", "ignored", "filtered"):
            c[k] += r[k]
    for crate, _name, _line in self_skipped:
        c = crates.setdefault(crate, dict.fromkeys(keys, 0))
        c["passed"] -= 1  # cargo counted it as passed
        c["selfskip"] += 1
    total = {k: sum(c[k] for c in crates.values()) for k in keys}

    if total["failed"]:
        errors.append(f"{total['failed']} test(s) failed")
    if total["filtered"] != len(skip_names):
        errors.append(
            f"--skip names {len(skip_names)}, tests filtered out {total['filtered']}: a skip entry matches no test "
            "(renamed or deleted) or matches more than one; fix the list in nix/pkgs/rimed.nix"
        )
    for crate, name, line in self_skipped:
        if not allowed(name, self_skip_names):
            errors.append(f"{name} ({crate}) passed by skipping itself ({line!r}); fix the environment or name it "
                          "in selfSkipOk")
    for k in self_skip_names:
        if not any(allowed(name, [k]) for _c, name, _l in self_skipped):
            errors.append(f"selfSkipOk names {k}, which did not skip itself (fixed, renamed or deleted?): remove it")

    lines = ["rimed workspace: cargo test --workspace --no-fail-fast (Nix build sandbox)", ""]
    def row(name, c):
        return (f"{name:<20} {c['targets']:>7} {c['passed']:>7} {c['selfskip']:>8} {c['failed']:>7} "
                f"{c['ignored']:>8} {c['filtered']:>8}")

    lines.append(f"{'crate':<20} {'targets':>7} {'passed':>7} {'selfskip':>8} {'failed':>7} {'ignored':>8} {'skipped':>8}")
    for name in sorted(crates):
        lines.append(row(name, crates[name]))
    lines.append(row("TOTAL", total))
    lines += ["", "passed = ran and passed. selfskip = cargo says ok, but the test printed SKIP and returned (proved",
              "nothing). skipped = --skip by name. ignored = #[ignore] in the source."]
    lines += ["", "skipped (--skip, by name, with the reason group from nix/pkgs/rimed.nix):"]
    for group, names in skip_groups.items():
        for n in names:
            lines.append(f"  {group}: {n}")
    lines += ["", "ignored in the source (#[ignore]):"]
    for crate, name, why in ignored:
        lines.append(f"  {crate}: {name}: {why}")
    lines += ["", "self-skipped (cargo: ok; printed SKIP and returned), by selfSkipOk group from nix/pkgs/rimed.nix:"]
    for group, names in self_skip_groups.items():
        for crate, name, line in self_skipped:
            if allowed(name, names):
                lines.append(f"  {group}: {crate}: {name}: {line[:160]}")
    if errors:
        lines += ["", "REFUSED:"] + [f"  {e}" for e in errors]

    text = "\n".join(lines) + "\n"
    open(out_txt, "w").write(text)
    json.dump(
        {
            "crates": crates,
            "total": total,
            "skipped": skip_groups,
            "ignored": [{"crate": c, "test": n, "reason": w} for c, n, w in ignored],
            "selfSkipped": [{"crate": c, "test": n, "line": l} for c, n, l in self_skipped],
            "selfSkipOk": self_skip_groups,
            "errors": errors,
        },
        open(out_json, "w"),
        indent=2,
        sort_keys=True,
    )
    sys.stdout.write(text)
    return 1 if errors else 0


if __name__ == "__main__":
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    sys.exit(main(*sys.argv[1:]))
