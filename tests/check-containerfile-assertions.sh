#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-containerfile-assertions.sh — run the image build's own grep and
#  `python3 -c` assertions against this repository, before a 50-minute build
#  runs them.
#
#  Four separate assertions on roadmap/v2.2 could never pass or never run, and
#  each one was found the same way: by a `base` job failing at a later step than
#  the last, 50 minutes at a time.
#
#      706489ec  ! grep -q '_keep' … matched the policy file's OWN comment
#      e24cd244  nft -c could not reach netlink in a build container at all
#      43d98f10  ! grep 'mouse = true' matched the three comment lines that
#                explain why `{ mouse = true }` is wrong
#      2e33c1a6  grep 'RimeShellKeybinds.${_variant}' — a draft spelling of a
#                variable that appears nowhere in the file it greps
#
#  All four are the same defect: an assertion nobody has ever watched go red.
#  A Containerfile assertion is the one kind of test that cannot be run by
#  running the test suite, because it only exists inside a build — so this
#  resolves each one back through the Containerfile's own COPY lines to the
#  file in this repo that becomes it, and runs it here.
#
#  It is honest about what it cannot reach. Many stanzas grep a path that only
#  exists after a `sed`, or inside the vendored rime-shell clone, or through a
#  variable built at build time. Those are reported as UNRESOLVED and counted
#  separately — never as passes. "Could not check" is not "fine".
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

FILES=(Containerfile.base Containerfile.core)
[ "$#" -gt 0 ] && FILES=("$@")

python3 - "${FILES[@]}" <<'PY'
import re, subprocess, os, shlex, sys

failures = 0
checked = 0
unresolved = 0
unresolved_examples = []
inert = 0
inert_examples = []

for path in sys.argv[1:]:
    if not os.path.exists(path):
        print(f"SKIP  {path} does not exist")
        continue
    lines = open(path).read().split('\n')

    # The file's own COPY map, image path -> repo path.
    #
    # Join continuation lines FIRST. This file writes any COPY whose two paths
    # are long across two physical lines:
    #
    #     COPY files/system/NetworkManager/20-rime-wifi-powersave.conf \\
    #          /etc/NetworkManager/conf.d/20-rime-wifi-powersave.conf
    #
    # A single-line regex sees neither half, so the destination looked like a
    # path nothing copies and EVERY assertion against it was written off as
    # "no COPY source here" -- 96 of them, the largest unresolved category,
    # reported as unchecked when they were perfectly checkable.
    joined_copy_lines = []
    buf = ''
    for l in lines:
        stripped = l.rstrip()
        if stripped.endswith('\\'):
            buf += stripped[:-1].rstrip() + ' '
            continue
        joined_copy_lines.append(buf + stripped)
        buf = ''
    if buf:
        joined_copy_lines.append(buf)

    copies = []
    for l in joined_copy_lines:
        m = re.match(r'^COPY\s+(?:--\S+\s+)*(\S+)\s+(\S+)\s*$', l)
        if m:
            copies.append((m.group(1), m.group(2)))

    def to_repo(img):
        """Every repo path that becomes `img` in the built image.

        A grep target can be a directory ABOVE the COPY destinations that fill
        it -- `/usr/share/rime/hypr/` is filled by two separate COPY lines --
        and that is exactly the shape 43d98f10 had, so resolving only the
        exact and inside-a-COPY cases would miss the defect this file exists
        to catch."""
        best = None
        for src, dst in copies:
            if img == dst:
                return [src]
            if dst.endswith('/') and img.startswith(dst):
                cand = src.rstrip('/') + '/' + img[len(dst):]
                if best is None or len(dst) > len(best[0]):
                    best = (dst, cand)
        if best:
            return [best[1]]
        # A COPY of a DIRECTORY whose destination carries no trailing slash:
        #
        #     COPY files/desktop/rime-greet /usr/share/rime-greet
        #
        # Docker fills the destination from the source's CONTENTS, so
        # /usr/share/rime-greet/GreetContext.qml is a real image path — but the
        # two branches above only match an exact destination or one spelled
        # with a trailing slash, so every assertion against a file inside such a
        # tree resolved to None and was written off as UNRESOLVED. The greeter
        # is copied exactly this way, which meant the login screen's assertions
        # — the boot-critical ones — were in the bucket this file reports as
        # "could not check" while reading as though the file had them covered.
        #
        # Only when the repo source really IS a directory: a destination that
        # happens to be a prefix of an unrelated path must not be resolved
        # through a file.
        best = None
        for src, dst in copies:
            if dst.endswith('/') or not img.startswith(dst.rstrip('/') + '/'):
                continue
            if not os.path.isdir(src.rstrip('/')):
                continue
            cand = src.rstrip('/') + img[len(dst.rstrip('/')):]
            if best is None or len(dst) > len(best[0]):
                best = (dst, cand)
        if best:
            return [best[1]]
        if img.endswith('/'):
            under = [src for src, dst in copies if dst.startswith(img)]
            if under:
                return under
        return None

    def check_python(lineno, cmd, piped):
        """Run a `python3 -c` build assertion here, against the repo source.

        Until this existed the resolver below matched `grep` and nothing else,
        so a `python3 -c` assertion fell through its `continue`: not checked,
        and not counted as unchecked either -- the outcome this file's own
        header calls the worst of the three. Containerfile.base carries seven
        of them, and two guard the Firefox enterprise policy that every browser
        capsule reads through the sandbox's read-only bind of `/`.

        The path rewrite is the load-bearing part, not a convenience, and both
        halves of that were measured rather than argued. These assertions open
        absolute IMAGE paths, and the two places this runs disagree about
        whether those paths exist.

        On a Rime machine `/etc/firefox/policies/policies.json` EXISTS. With
        the rewrite removed and the repository's own `policies.json` replaced
        by `{ "policies": }`, this file reports "193 checked, 0 failed" and
        exits 0: it parsed the LIVE file and learned nothing about the repo.

        On a machine without the image -- a runner -- the same unrewritten
        handler fails all six resolvable assertions with `FileNotFoundError`
        and exits 1, for a reason that has nothing to do with what any of them
        assert. Measured in a python:3.12-slim container over this tree, with
        the repository's files left valid.

        With the rewrite, that same container reports 194 checked and 0 failed,
        which is the point: what is being checked is the repository, and it
        should not matter what the machine underneath happens to have. So every
        image path in the script and in its argv is resolved through the same
        COPY map the greps use and rewritten before it runs, and a script with
        one path that will not resolve is UNRESOLVED rather than a pass.
        """
        global checked, failures, unresolved
        if piped:
            unresolved += 1
            unresolved_examples.append(
                f"{path}:{lineno} python3 -c reads a pipe -- only the build can check it")
            return
        try:
            toks = shlex.split(cmd)
        except ValueError:
            unresolved += 1
            unresolved_examples.append(f"{path}:{lineno} unparseable")
            return
        toks = [t for t in toks if t not in (';', '\\')]
        if len(toks) < 3 or toks[0] != 'python3' or toks[1] != '-c':
            unresolved += 1
            unresolved_examples.append(
                f"{path}:{lineno} a python3 form this resolver cannot follow")
            return
        script, argv = toks[2], toks[3:]
        if 'sys.stdin' in script:
            unresolved += 1
            unresolved_examples.append(
                f"{path}:{lineno} python3 -c reads a pipe -- only the build can check it")
            return

        # Every absolute image path the script opens, plus every one handed to
        # it as an argument. A `$` anywhere makes the path a build-time value --
        # `"/usr/libexec/$g"` inside a `for` is exactly the 43d98f10 shape --
        # and there is nothing here to follow it with.
        wanted = re.findall(r"""open\(\s*['"](/(?:usr|etc)/[^'"]+)['"]""", script)
        for a in argv:
            if a.startswith('/usr/') or a.startswith('/etc/'):
                wanted.append(a)
        if any('$' in t for t in [script] + argv):
            unresolved += 1
            unresolved_examples.append(
                f"{path}:{lineno} python3 -c path is built from a shell variable")
            return
        if not wanted:
            unresolved += 1
            unresolved_examples.append(
                f"{path}:{lineno} python3 -c names no image path to resolve")
            return

        rewritten, rargv = script, list(argv)
        for img in wanted:
            repos = [r for r in (to_repo(img) or []) if os.path.isfile(r)]
            if len(repos) != 1:
                unresolved += 1
                unresolved_examples.append(f"{path}:{lineno} {img} has no COPY source here")
                return
            real = os.path.abspath(repos[0])
            rewritten = rewritten.replace(img, real)
            rargv = [real if a == img else a for a in rargv]

        r = subprocess.run(['python3', '-c', rewritten] + rargv,
                           capture_output=True, text=True)
        checked += 1
        if os.environ.get('RIME_CF_VERBOSE'):
            print(f"  check {path}:{lineno} python3 -c over {' '.join(wanted)} -> "
                  f"{'ok' if r.returncode == 0 else 'FAILED'}")
        if r.returncode != 0:
            failures += 1
            print(f"FAIL  {path}:{lineno}")
            print(f"      python3 -c over {' '.join(wanted)} exits {r.returncode} "
                  f"against this repository's own copy of it")
            for x in (r.stderr or r.stdout).strip().split('\n')[-3:]:
                print(f"          {x[:150]}")
            print(f"      This assertion can only ever FAIL the build.")


    # Walk stanza by stanza so a `f=/usr/libexec/...` assignment is in scope for
    # the greps below it. That is not a nicety: the fourth defect was found in a
    # stanza that greps "$f", so a resolver that only understood literal paths
    # would have missed exactly the one this file exists to catch.
    i = 0
    while i < len(lines):
        if not lines[i].startswith('RUN '):
            i += 1
            continue
        # A comment line inside a continued RUN does NOT end with a backslash --
        # buildah strips comments before joining -- so a walker that ends the
        # stanza at the first line without one stops at the first comment and
        # silently drops every assertion after it. That is how the first draft
        # of this file passed while blind to two of the four defects it was
        # written for: both sit under an explanatory comment. Only a non-comment
        # line without a continuation ends a stanza.
        stanza = []
        j = i
        while True:
            stanza.append((j + 1, lines[j]))
            stripped = lines[j].strip()
            is_comment = stripped.startswith('#')
            if not is_comment and not lines[j].rstrip().endswith('\\'):
                break
            j += 1
            if j >= len(lines):
                break
            # A blank line, or the next instruction, really does end it.
            if not lines[j].strip() or re.match(r'^[A-Z]+\s', lines[j]):
                break
        i = j + 1

        env = {}
        # Values the stanza reads out of a KEY=value file the image COPYs; see
        # the VAR="$(sed -n 's/^KEY=//p' ...)" shape below.
        kv = {}
        # `for m in hyprland niri labwc; do ... grep "^rime(\"${m}\")" ...` --
        # a pattern built from a loop variable is not a literal and cannot be
        # checked from here. It is UNRESOLVED, not a failure: reporting it as a
        # defect would be this file making the same mistake it exists to catch.
        loopvars = set()
        for _, raw in stanza:
            for lm in re.finditer(r'\bfor\s+([A-Za-z_][A-Za-z0-9_]*)\s+in\s', raw):
                loopvars.add(lm.group(1))

        # ── a refusal that cannot fail ───────────────────────────────────────
        # bash does not apply errexit to a command whose status is inverted
        # with `!`. bash(1), under `set -e`, exempts "any command in a pipeline
        # but the last, or if the command's return value is being inverted with
        # !". So this prints REACHED:
        #
        #     set -e; ! grep -q x <<<x; echo REACHED
        #
        # A `! grep` written as a build-time refusal therefore fails NOTHING —
        # unless it is the last command of its RUN, where its status is what
        # buildah reads, or it carries its own `||` handler. Every other one is
        # a comment with a grep in it.
        #
        # This was the state of 43 refusals across these files on 2026-09-12,
        # three of them added THAT DAY to repair assertions that could only
        # fail. The class is invisible to the resolver below, which reports a
        # refusal matching nothing as a pass — correct about the pattern, silent
        # about the fact that nothing would have happened either way.
        joined = []
        for lineno, raw in stanza:
            s = raw.strip()
            if s.startswith('RUN '):
                s = s[4:].strip()
            s = s.rstrip('\\').rstrip()
            if s.strip().startswith('#') or not s.strip():
                continue
            joined.append((lineno, s))

        text, idx_line = '', []
        for lineno, s in joined:
            if text and not text.endswith(' '):
                text += ' '
                idx_line.append(lineno)
            for ch in s:
                text += ch
                idx_line.append(lineno)

        # Split into top-level commands on `;`, honouring quotes so a `;` inside
        # a grep pattern does not invent a command boundary.
        segs, cur, cur_line, q = [], '', None, None
        for k, ch in enumerate(text):
            if q:
                cur += ch
                if ch == q:
                    q = None
                continue
            if ch in ('"', "'"):
                q, cur = ch, cur + ch
                if cur_line is None:
                    cur_line = idx_line[k]
                continue
            if ch == ';':
                segs.append((cur_line, cur.strip()))
                cur, cur_line = '', None
                continue
            cur += ch
            if cur_line is None and not ch.isspace():
                cur_line = idx_line[k]
        if cur.strip():
            segs.append((cur_line, cur.strip()))
        segs = [(l, s) for l, s in segs if s]

        for n, (lineno, seg) in enumerate(segs):
            body = seg
            for kw in ('do ', 'then ', 'else '):
                if body.startswith(kw):
                    body = body[len(kw):].lstrip()
            # `test ! -u f` and `[ "$x" != y ]` are operators, not an inverted
            # exit status; only a leading `!` inverts a command.
            if not body.startswith('!') or body.startswith('!='):
                continue
            # An explicit handler runs regardless of errexit. This is a
            # substring test, so a `||` inside a grep PATTERN would read as a
            # handler and skip the line. No such line exists in these files
            # today; if one is ever written, this is where it hides.
            if '||' in body or '&&' in body:
                continue
            # Effective when nothing but block terminators follows it: the
            # status of the enclosing loop or `if` is the status of its last
            # command, and that becomes the RUN's.
            if all(s.strip() in ('done', 'fi', 'esac', '}') for _, s in segs[n + 1:]):
                continue
            inert += 1
            inert_examples.append(f"{path}:{lineno} {body[:96]}")
        # Resolve from the JOINED segments, not from physical lines. A grep
        # whose pattern and file sit on two lines -- which is how this file
        # writes any long one -- looked like a grep with no file at all, and
        # a resolver that reads one line can neither check it nor tell it
        # apart from the tail of a pipe.
        for lineno, seg in segs:
            s = seg.strip()
            if s.startswith('RUN '):
                s = s[4:].strip()
            if s.startswith('#') or not s:
                continue
            for kw in ('do ', 'then ', 'else '):
                if s.startswith(kw):
                    s = s[len(kw):].lstrip()
            # A `python3 -c` assertion. Validity -- does this JSON parse,
            # does this Python file compile, does this policy have the shape it
            # is supposed to have -- is written in python here rather than in
            # grep, because a grep cannot answer any of the three. See
            # check_python: these were invisible to this file until it existed.
            pym = re.search(r'(?:^|\|\s*)(python3\s+-c\b.*)$', s)
            if pym:
                check_python(lineno, pym.group(1),
                             piped=bool(re.search(r'\|\s*python3\s+-c\b', s)))
                continue

            # `cmd | grep PATTERN` really does read stdin.
            piped = False
            if '|' in s:
                tail = s.rsplit('|', 1)[1].strip()
                if re.match(r'^(!\s*)?grep\s', tail):
                    piped, s = True, tail

            # VAR=/usr/... or VAR="/usr/..."   (one per physical line, which is
            # how this file writes them)
            m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)=("?)(/(?:usr|etc)/[^"\s;]+)\2\s*;?\s*$', s)
            if m:
                env[m.group(1)] = m.group(3)
                continue

            # VAR="$(sed -n 's/^KEY=//p' /usr/...)": a value read from a
            # KEY=value file the image COPYs. /usr/share/rime/secrets-provider
            # names the Secret Service provider once for every assertion that
            # needs it, and a grep built from it is still a literal once the
            # repo file that becomes it is read. Exactly one KEY= line, or it
            # stays unresolved (sed -n would print every one).
            m = re.match(r'''^([A-Za-z_][A-Za-z0-9_]*)="\$\(sed -n 's/\^([A-Za-z_][A-Za-z0-9_]*)=//p' (/(?:usr|etc)/[^\s)"]+)\)"$''', s)
            if m:
                vals = []
                for r in (to_repo(m.group(3)) or []):
                    if os.path.isfile(r):
                        with open(r) as fh:
                            vals += [ln.rstrip('\n').split('=', 1)[1] for ln in fh
                                     if ln.startswith(m.group(2) + '=')]
                if len(vals) == 1:
                    kv[m.group(1)] = vals[0]
                continue

            # Three shapes carry a grep assertion, and the `if` one is the
            # house form for a REFUSAL: rewriting `! grep` to
            # `if grep ...; then echo FATAL; exit 1; fi` is what makes it fail a
            # build at all. A resolver that only understood `grep` and `! grep`
            # would report 13 fewer checks the moment those were repaired, and
            # call that an improvement.
            m = re.match(r'^(if\s+)?(!\s*)?grep\s+(.*)$', s)
            if not m:
                continue
            is_if = bool(m.group(1))
            negated = bool(m.group(2))
            rhs = re.split(r';\s*then\b', m.group(3))[0]
            try:
                toks = shlex.split('grep ' + rhs)
            except ValueError:
                unresolved += 1
                unresolved_examples.append(f"{path}:{lineno} unparseable")
                continue
            toks = toks[1:]
            # Drop a trailing `; \` fragment shlex kept as its own token.
            toks = [t for t in toks if t not in (';', '\\')]

            flags = [t for t in toks if t.startswith('-') and len(t) > 1]
            rest = [t for t in toks if t not in flags]
            if piped or len(rest) < 2:
                # A grep at the tail of a pipe reads the command's output, which
                # only the build can produce. This used to `continue` SILENTLY --
                # the worst of the three outcomes: not checked, and not counted
                # as unchecked either. `rime-vm --help | grep -q 'no viewer'`
                # could never pass, because that help went to stderr so the pipe
                # carried nothing, and it cost a 50-minute build to find out
                # while this file reported "0 failed" throughout.
                unresolved += 1
                what = rest[0] if rest else '?'
                unresolved_examples.append(
                    f"{path}:{lineno} grep {what!r} reads a pipe -- only the build can check it")
                continue
            pattern, target = rest[0], rest[-1].rstrip(';')
            for v, val in kv.items():
                pattern = re.sub(r'\$\{' + re.escape(v) + r'\}|\$' + re.escape(v) + r'\b', lambda _: val, pattern)
                target = re.sub(r'\$\{' + re.escape(v) + r'\}|\$' + re.escape(v) + r'\b', lambda _: val, target)

            # Resolve the target: a literal image path, or a variable the
            # stanza set to one.
            if any(re.search(r'\$\{?' + re.escape(v) + r'\}?', pattern) for v in loopvars):
                unresolved += 1
                unresolved_examples.append(f"{path}:{lineno} pattern is built from a loop variable")
                continue

            img = None
            vm = re.match(r'^\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?$', target)
            if vm and vm.group(1) in env:
                img = env[vm.group(1)]
            elif target.startswith('/usr/') or target.startswith('/etc/'):
                img = target

            if img is None:
                unresolved += 1
                unresolved_examples.append(f"{path}:{lineno} target {target}")
                continue
            repos = to_repo(img if img.endswith('/') else img.rstrip('/'))
            repos = [r for r in (repos or []) if os.path.exists(r)]
            if not repos:
                unresolved += 1
                unresolved_examples.append(f"{path}:{lineno} {img} has no COPY source here")
                continue

            gflags = ['-q']
            if any(os.path.isdir(r) for r in repos):
                gflags.append('-r')
            for f in flags:
                if f.startswith('-') and not f.startswith('--'):
                    for ch in f[1:]:
                        if ch in 'EFiwxr':
                            gflags.append('-' + ch)
            r = subprocess.run(['grep'] + gflags + [pattern] + repos, capture_output=True)
            matched = (r.returncode == 0)
            checked += 1
            if os.environ.get('RIME_CF_VERBOSE'):
                print(f"  check {path}:{lineno} {'if ' if is_if else ''}{'!' if negated else ''}grep {pattern!r} -> "
                      f"{'match' if matched else 'no match'} in {' '.join(repos)}")
            # `grep X` asserts a match; `! grep X` and `if grep X; then exit 1`
            # both assert its absence; `if ! grep X; then exit 1` asserts a
            # match again.
            want = (negated == is_if)
            if matched != want:
                failures += 1
                if not want:
                    print(f"FAIL  {path}:{lineno}")
                    print(f"      a refusal of {pattern!r} is supposed to find NOTHING, and it matches {' '.join(repos)}")
                    m2 = subprocess.run(['grep', '-n'] + [g for g in gflags if g != '-q'] + [pattern] + repos,
                                        capture_output=True, text=True)
                    for x in m2.stdout.strip().split('\n')[:3]:
                        print(f"          {x[:150]}")
                    print(f"      This assertion can only ever FAIL the build.")
                else:
                    print(f"FAIL  {path}:{lineno}")
                    print(f"      grep {pattern!r} finds nothing in {' '.join(repos)}")
                    print(f"      This assertion can only ever FAIL the build.")

print()
print(f"containerfile assertions: {checked} checked, {failures} failed, {unresolved} could not be resolved, {inert} inert")
if unresolved:
    print("  UNRESOLVED (not checked, and not a pass — these run only inside the build):")
    # A bare total says how much is unchecked but not where the risk is. The
    # rime-vm `--help | grep` that failed base at step 120/165 was a pipe, and
    # a pipe is the category the build alone can settle -- so the breakdown is
    # the part worth reading.
    import collections as _c
    reason = _c.Counter()
    for e in unresolved_examples:
        if 'reads a pipe' in e:            reason['a pipe — only the build can run it'] += 1
        elif 'has no COPY source here' in e: reason['the build creates the file; no COPY to resolve'] += 1
        elif 'loop variable' in e:         reason['pattern built from a loop variable'] += 1
        elif 'shell variable' in e:        reason['a path built from a shell variable'] += 1
        elif 'unparseable' in e:           reason['shell-unparseable'] += 1
        else:                              reason['a target this resolver cannot follow'] += 1
    for why, n in reason.most_common():
        print(f"    {n:4}  {why}")
    print("    examples:")
    for e in unresolved_examples[:12]:
        print(f"      {e}")
    if len(unresolved_examples) > 12:
        print(f"      … and {len(unresolved_examples) - 12} more")
if inert:
    print("  INERT (a `!` refusal errexit does not apply to — it fails nothing):")
    for e in inert_examples[:60]:
        print(f"    {e}")
    if len(inert_examples) > 20:
        print(f"    \u2026 and {len(inert_examples) - 20} more")
    print("  Rewrite each as `if <check>; then echo \"FATAL: ...\"; exit 1; fi`.")
sys.exit(1 if (failures or inert) else 0)
PY
